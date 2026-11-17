//! `CoreSink`: librespot's audio sink feeding the core engine's [`SourceWriter`] (MD24, MD25).
//!
//! librespot calls its sink with no boundary information. The exact signal is the order of its
//! player thread: it sends a `PlayerEvent` (`Seeked`, `Stopped`, `TrackChanged`, `EndOfTrack`)
//! before its next sink call. So every sink call first drains the sink's own event channel with
//! `try_recv` and turns those events into in-band source actions through [`SinkLogic`]:
//!
//! - `EndOfTrack` marks the track as ended (a following `TrackChanged` is gapless);
//! - `TrackChanged` flushes unless the track ended, then starts a track;
//! - `Seeked` flushes; `Stopped` flushes and leaves no track open;
//! - `start` resumes the engine; `stop` lets the tail play out after an end, otherwise pauses.
//!
//! Runs on librespot's player thread. Nothing here calls tokio (`try_recv` needs no runtime) and
//! `start`/`stop` never fail (librespot exits the process on a sink stop error). librespot's
//! `Converter` is never used: the engine quantises once (D20).

use std::sync::{Arc, Mutex};

use librespot_playback::{
    audio_backend::{Sink, SinkError, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
    player::{PlayerEvent, PlayerEventChannel},
};
use whipspot_core::engine::{SourceInfo, SourceWriter, StreamFormat};

/// What librespot's decoder hands the sink: interleaved stereo at 44.1 kHz, lossy.
pub const SPOTIFY_FORMAT: StreamFormat = StreamFormat {
    rate: 44_100,
    channels: 2,
    source: SourceInfo {
        codec: "Ogg Vorbis or MP3",
        bits: None,
        bitrate_kbps: None,
        lossless: false,
        replay_gain: None,
    },
};

/// Where the player's event channel is handed to the sink: filled right after `Player::new`
/// returns (the sink is built on the player thread before that), taken by the sink's first call.
pub type EventSlot = Arc<Mutex<Option<PlayerEventChannel>>>;

/// The player events the sink acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkEvent {
    EndOfTrack,
    TrackChanged,
    Seeked,
    Stopped,
    Other,
}

impl From<&PlayerEvent> for SinkEvent {
    fn from(ev: &PlayerEvent) -> Self {
        match ev {
            PlayerEvent::EndOfTrack { .. } => SinkEvent::EndOfTrack,
            PlayerEvent::TrackChanged { .. } => SinkEvent::TrackChanged,
            PlayerEvent::Seeked { .. } => SinkEvent::Seeked,
            PlayerEvent::Stopped { .. } => SinkEvent::Stopped,
            _ => SinkEvent::Other,
        }
    }
}

/// An in-band source action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SinkAction {
    Flush,
    StartTrack,
}

/// What `stop` does to the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopAction {
    /// The track ended: let the tail play out.
    Finish,
    /// Paused mid-track: fade out and keep the ring.
    Pause,
}

/// The sink's pure state machine over librespot's event order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SinkLogic {
    ended: bool,
    track_open: bool,
}

impl SinkLogic {
    /// The source actions one event asks for, in order.
    pub fn on_event(&mut self, ev: SinkEvent) -> &'static [SinkAction] {
        match ev {
            SinkEvent::EndOfTrack => {
                self.ended = true;
                &[]
            }
            SinkEvent::TrackChanged => {
                let acts: &'static [SinkAction] = if self.ended {
                    &[SinkAction::StartTrack]
                } else {
                    &[SinkAction::Flush, SinkAction::StartTrack]
                };
                self.ended = false;
                self.track_open = true;
                acts
            }
            SinkEvent::Seeked => &[SinkAction::Flush],
            SinkEvent::Stopped => {
                self.ended = false;
                self.track_open = false;
                &[SinkAction::Flush]
            }
            SinkEvent::Other => &[],
        }
    }

    pub fn on_stop(&self) -> StopAction {
        if self.ended {
            StopAction::Finish
        } else {
            StopAction::Pause
        }
    }

    /// A write must open a track first.
    pub fn needs_track(&self) -> bool {
        !self.track_open
    }

    pub fn track_opened(&mut self) {
        self.track_open = true;
    }
}

/// librespot's sink over the engine's [`SourceWriter`].
pub struct CoreSink {
    source: SourceWriter,
    slot: EventSlot,
    events: Option<PlayerEventChannel>,
    logic: SinkLogic,
}

fn sink_error() -> SinkError {
    SinkError::OnWrite("audio engine rejected the samples".into())
}

impl CoreSink {
    pub fn new(source: SourceWriter, slot: EventSlot) -> Self {
        CoreSink {
            source,
            slot,
            events: None,
            logic: SinkLogic::default(),
        }
    }

    /// Apply every event the player thread sent before this call.
    fn drain(&mut self) {
        if self.events.is_none() {
            self.events = self.slot.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
        let Some(rx) = self.events.as_mut() else {
            return;
        };
        while let Ok(ev) = rx.try_recv() {
            for act in self.logic.on_event(SinkEvent::from(&ev)) {
                match act {
                    SinkAction::Flush => self.source.flush(),
                    SinkAction::StartTrack => {
                        // A failure here leaves the next write to report it.
                        if self.source.start_track(SPOTIFY_FORMAT).is_err() {
                            self.logic.track_open = false;
                        }
                    }
                }
            }
        }
    }
}

impl Sink for CoreSink {
    fn start(&mut self) -> SinkResult<()> {
        self.drain();
        self.source.resume();
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.drain();
        match self.logic.on_stop() {
            StopAction::Finish => self.source.finish(),
            StopAction::Pause => self.source.pause(),
        }
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, _converter: &mut Converter) -> SinkResult<()> {
        self.drain();
        let AudioPacket::Samples(samples) = packet else {
            return Ok(());
        };
        if self.logic.needs_track() {
            self.source
                .start_track(SPOTIFY_FORMAT)
                .map_err(|_| sink_error())?;
            self.logic.track_opened();
        }
        self.source.write(&samples).map_err(|_| sink_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_core::SpotifyUri;
    use whipspot_core::engine::{DeviceFormat, Engine, OutputMode, OutputRequest, SampleFormat};
    use whipspot_testkit::FakeOutput;

    const URI: &str = "spotify:track:4uLU6hMCjMI75M1A2tKUQC";

    fn id() -> SpotifyUri {
        SpotifyUri::from_uri(URI).unwrap()
    }

    fn run(events: &[SinkEvent]) -> (SinkLogic, Vec<SinkAction>) {
        let mut logic = SinkLogic::default();
        let acts = events
            .iter()
            .flat_map(|&e| logic.on_event(e).iter().copied())
            .collect();
        (logic, acts)
    }

    #[test]
    fn track_change_without_end_flushes_then_starts() {
        let (_, acts) = run(&[SinkEvent::TrackChanged]);
        assert_eq!(acts, [SinkAction::Flush, SinkAction::StartTrack]);
    }

    #[test]
    fn track_change_after_end_is_gapless() {
        let other = |e: PlayerEvent| SinkEvent::from(&e);
        let events = [
            other(PlayerEvent::EndOfTrack {
                play_request_id: 1,
                track_id: id(),
            }),
            other(PlayerEvent::PlayRequestIdChanged { play_request_id: 2 }),
            SinkEvent::TrackChanged,
            other(PlayerEvent::Playing {
                play_request_id: 2,
                track_id: id(),
                position_ms: 0,
            }),
        ];
        assert_eq!(events[1], SinkEvent::Other);
        assert_eq!(events[3], SinkEvent::Other);
        let (_, acts) = run(&events);
        assert_eq!(acts, [SinkAction::StartTrack]);
    }

    #[test]
    fn seek_flushes() {
        let (_, acts) = run(&[SinkEvent::from(&PlayerEvent::Seeked {
            play_request_id: 1,
            track_id: id(),
            position_ms: 5,
        })]);
        assert_eq!(acts, [SinkAction::Flush]);
    }

    #[test]
    fn stop_flushes_and_the_next_write_opens_a_track() {
        let (logic, acts) = run(&[
            SinkEvent::TrackChanged,
            SinkEvent::from(&PlayerEvent::Stopped {
                play_request_id: 1,
                track_id: id(),
            }),
        ]);
        assert_eq!(acts[2..], [SinkAction::Flush]);
        assert!(logic.needs_track());
        let (logic, _) = run(&[SinkEvent::TrackChanged]);
        assert!(!logic.needs_track());
    }

    #[test]
    fn stop_finishes_after_end_and_pauses_otherwise() {
        let (logic, _) = run(&[SinkEvent::TrackChanged, SinkEvent::EndOfTrack]);
        assert_eq!(logic.on_stop(), StopAction::Finish);
        let (logic, _) = run(&[SinkEvent::TrackChanged]);
        assert_eq!(logic.on_stop(), StopAction::Pause);
        let (logic, _) = run(&[SinkEvent::EndOfTrack, SinkEvent::TrackChanged]);
        assert_eq!(logic.on_stop(), StopAction::Pause);
    }

    /// 16-bit-exact stereo frames, distinct per frame.
    fn packet(base: i32, frames: usize) -> Vec<f64> {
        (0..frames as i32)
            .flat_map(|i| {
                let v = f64::from(base + i % 20_000) / 32_768.0;
                [v, -v]
            })
            .collect()
    }

    #[test]
    fn seek_flushes_in_band_before_the_next_packet() {
        let fake = FakeOutput::new(
            DeviceFormat {
                rate: 44_100,
                channels: 2,
                sample: SampleFormat::I16,
                valid_bits: 16,
            },
            OutputMode::Exclusive,
        );
        let parts = Engine::new(Default::default(), Some(Box::new(fake.clone())));
        let mut engine = parts.engine;
        engine.open(&OutputRequest::default()).unwrap();

        let slot: EventSlot = Arc::default();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        *slot.lock().unwrap() = Some(rx);
        let mut sink = CoreSink::new(parts.source, slot);
        let mut conv = Converter::new(None);

        let a = packet(1_000, 4_410);
        let b = packet(-25_000, 2_000);
        sink.write(AudioPacket::Samples(a.clone()), &mut conv)
            .unwrap();
        let first = fake.render_i16(441);
        let expect = |s: &[f64]| -> Vec<i16> { s.iter().map(|&x| (x * 32_768.0) as i16).collect() };
        assert_eq!(first, expect(&a[..882]));

        tx.send(PlayerEvent::Seeked {
            play_request_id: 1,
            track_id: id(),
            position_ms: 60_000,
        })
        .unwrap();
        sink.write(AudioPacket::Samples(b.clone()), &mut conv)
            .unwrap();
        // The flush is in band: what follows the engine's declick (H26, M10 step 3) is `b` and
        // nothing more of `a`. The declick is 5 ms (221 frames at 44.1 kHz) of `a` ramping to
        // zero, then `b` fading in over as long; from there `b` is exact. `a`'s left channel is
        // positive, `b`'s negative.
        const TAIL: usize = 221;
        let out = fake.render_i16(1_500);
        let left = |s: &[i16]| s.iter().step_by(2).copied().collect::<Vec<_>>();
        assert!(left(&out[..2 * TAIL]).iter().all(|&s| s >= 0), "tail");
        assert!(
            left(&out[2 * TAIL..]).iter().all(|&s| s <= 0),
            "after the tail"
        );
        assert_eq!(out[4 * TAIL..], expect(&b[2 * TAIL..3_000 - 2 * TAIL]));
        // Raw packets are ignored.
        sink.write(AudioPacket::Raw(vec![1, 2, 3]), &mut conv)
            .unwrap();
    }
}

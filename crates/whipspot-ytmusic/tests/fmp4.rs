//! Probe (step 1): can Symphonia 0.6.1, with the features this crate enables, decode and seek a
//! fragmented MP4 with AAC-LC audio and a sidx, which is what YouTube's DASH audio streams are?
//!
//! The fixture `data/tone-frag.m4a` is synthetic: 3 s of a 440 Hz stereo 44.1 kHz tone, AAC-LC at
//! 128 kbps, 1 s fragments with a global sidx. It was made with:
//!
//! ffmpeg -f lavfi -i "sine=frequency=440:duration=3" -ac 2 -ar 44100 -c:a aac -b:a 128k \
//!   -movflags +dash+global_sidx -frag_duration 1000000 tone-frag.m4a
//!
//! The source reads only by offset from an in-memory buffer, as a ranged HTTP body will (it has
//! a known length and can seek, and nothing else).

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;

use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

const FIXTURE: &[u8] = include_bytes!("data/tone-frag.m4a");

/// A seekable, length-known source over a shared buffer: every read is `buf[pos..pos + n]`.
struct OffsetSource {
    buf: Arc<[u8]>,
    pos: u64,
}

impl Read for OffsetSource {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let start = usize::try_from(self.pos)
            .unwrap_or(usize::MAX)
            .min(self.buf.len());
        let n = out.len().min(self.buf.len() - start);
        out[..n].copy_from_slice(&self.buf[start..start + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for OffsetSource {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let len = self.buf.len() as i128;
        let target = match to {
            SeekFrom::Start(p) => i128::from(p),
            SeekFrom::End(d) => len + i128::from(d),
            SeekFrom::Current(d) => i128::from(self.pos) + i128::from(d),
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start",
            ));
        }
        self.pos = u64::try_from(target).unwrap_or(u64::MAX);
        Ok(self.pos)
    }
}

impl MediaSource for OffsetSource {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.buf.len() as u64)
    }
}

fn open() -> Box<dyn symphonia::core::formats::FormatReader> {
    let src = OffsetSource {
        buf: Arc::from(FIXTURE),
        pos: 0,
    };
    let mss = MediaSourceStream::new(Box::new(src), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    hint.with_extension("m4a");
    symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .expect("probe the fragmented mp4")
}

#[test]
fn fixture_is_small_and_synthetic() {
    assert!(FIXTURE.len() <= 64 * 1024, "{} bytes", FIXTURE.len());
}

#[test]
fn decodes_the_whole_fragmented_file() {
    let mut format = open();
    let track = format
        .default_track(TrackType::Audio)
        .expect("an audio track");
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .expect("audio params")
        .clone();
    let track_id = track.id;
    assert_eq!(params.sample_rate, Some(44_100));
    assert_eq!(params.channels.as_ref().map(|c| c.count()), Some(2));
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .expect("an AAC decoder");

    let mut frames = 0usize;
    while let Some(packet) = format.next_packet().expect("next packet") {
        if packet.track_id != track_id {
            continue;
        }
        let decoded: GenericAudioBufferRef = decoder.decode(&packet).expect("decode");
        frames += decoded.frames();
    }
    let seconds = frames as f64 / 44_100.0;
    assert!(seconds >= 2.9, "decoded only {seconds:.3} s");
}

#[test]
fn seeks_to_two_seconds() {
    let mut format = open();
    let track = format
        .default_track(TrackType::Audio)
        .expect("an audio track");
    let (track_id, time_base) = (track.id, track.time_base.expect("a time base"));
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .expect("audio params")
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .expect("an AAC decoder");

    format
        .seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: Time::from_millis_u64(2_000),
                track_id: Some(track_id),
            },
        )
        .expect("seek to 2.0 s");
    decoder.reset();

    let packet = loop {
        let p = format
            .next_packet()
            .expect("next packet after the seek")
            .expect("a packet after the seek");
        if p.track_id == track_id {
            break p;
        }
    };
    let ts = u64::try_from(packet.pts.get()).expect("a non-negative pts");
    let ms = u128::from(ts) * 1000 * u128::from(time_base.numer.get())
        / u128::from(time_base.denom.get());
    let ms = ms as i64;
    assert!((ms - 2_000).abs() <= 50, "next packet at {ms} ms");
    decoder
        .decode(&packet)
        .expect("decode the first packet after the seek");
}

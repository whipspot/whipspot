//! `whipspot-app --self-test [--device <name>]` (H21, D16): plays a quiet test tone through the
//! real output for each mode the output offers, at 44.1 and 96 kHz, and prints the audio
//! diagnostics of each run so the user can report what their device does.
//!
//! No provider, no credential store and no settings: [`run`] takes an output and nothing else,
//! and `main` calls it before any of those is built. The tone goes through the core engine's
//! own [`SourceWriter`], so the run exercises the same ring, resampler, DSP and quantiser as
//! playback. Open failures print a fixed text (hard rule 1), never the adapter's error. Underruns
//! are information, not failure: the exit rule only asks that at least one run opened.
//!
//! Agents never run this (hard rule 2: it opens a real device); the unit tests drive [`run`]
//! with testkit's `ClockedOutput`, and the per-run counters with a scripted `FakeOutput` whose
//! callbacks run on the test's own thread (exact counts, whatever the host's load).

use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use whipspot_audio_out::OutputRouter;
use whipspot_core::diagnostics::AudioDiagnostics;
use whipspot_core::engine::{
    AudioOutput, BAND_COUNT, Engine, EngineConfig, EngineMonitor, EngineStatus, OutputError,
    OutputInfo, OutputMode, OutputRequest, SampleFormat, SourceError, SourceInfo, SourceWriter,
    StreamFormat,
};
use whipspot_core::player::{
    EqState, LyricsState, OutputStatus, PlayState, PlayerState, QueueState, RepeatMode,
};

/// The tone: 1 kHz at -30 dBFS (D16), quiet enough for headphones and speakers at any volume.
pub const TONE_HZ: f64 = 1000.0;
pub const TONE_DBFS: f64 = -30.0;
/// The rates each mode is tried at.
pub const RATES: [u32; 2] = [44_100, 96_000];
/// How long each run plays: long enough for the clock drift's fit to span its two seconds
/// after the half-second warm-up with the callbacks' jitter averaged down (MD138).
pub const RUN_DURATION: Duration = Duration::from_secs(5);
/// How often the tap reader empties the tap (its ring holds far more than this at 96 kHz).
const TAP_POLL: Duration = Duration::from_millis(20);
/// A tone block, in frames per second of tone: 1/20 s.
const BLOCKS_PER_SECOND: u32 = 20;

/// Fixed texts of the output (never an adapter's error).
const TEXT_START_FAILED: &str = "could not start the tone";
const TEXT_PLAY_FAILED: &str = "the tone stopped early";

/// One run: a mode at a rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub mode: OutputMode,
    pub rate: u32,
}

/// What one run came to.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub run: Run,
    /// `None` when the run opened and played; else the fixed failure text.
    pub failure: Option<&'static str>,
    pub underruns: Option<u64>,
    pub cpu_max_us: Option<u32>,
    pub drift_ppm: Option<f32>,
}

impl Outcome {
    pub fn opened(&self) -> bool {
        self.failure.is_none()
    }
}

/// Every offered mode at every rate, in mode order.
pub fn plan(modes: &[OutputMode]) -> Vec<Run> {
    modes
        .iter()
        .flat_map(|&mode| RATES.map(|rate| Run { mode, rate }))
        .collect()
}

/// The exit rule: success when at least one run opened.
pub fn succeeded(outcomes: &[Outcome]) -> bool {
    outcomes.iter().any(Outcome::opened)
}

fn mode_name(mode: OutputMode) -> &'static str {
    match mode {
        OutputMode::Shared => "Shared",
        OutputMode::Exclusive => "Exclusive",
    }
}

/// The fixed text for an open that failed.
fn open_failure(error: OutputError) -> &'static str {
    match error {
        OutputError::NoAdapter => "no audio output on this system",
        OutputError::NoDevice => "device not found",
        OutputError::Unsupported => "the device cannot run this format or mode",
        OutputError::Busy => "device in use",
        OutputError::Failed => "could not open",
    }
}

fn source_failure(error: SourceError) -> &'static str {
    match error {
        SourceError::Unsupported | SourceError::NoOutput => TEXT_START_FAILED,
        _ => TEXT_PLAY_FAILED,
    }
}

fn sample_text(info: &OutputInfo) -> &'static str {
    match (info.format.sample, info.format.valid_bits) {
        (SampleFormat::F32, _) => "32-bit float",
        (SampleFormat::I16, _) => "16-bit",
        (SampleFormat::I32, 24) => "24-bit in 32",
        (SampleFormat::I32, _) => "32-bit",
    }
}

/// "Shared mode, <device>, 48 kHz, 32-bit float" (the player's summary line).
fn summary(info: &OutputInfo) -> String {
    format!(
        "{} mode, {}, {} kHz, {}",
        mode_name(info.mode),
        info.device,
        f64::from(info.format.rate) / 1000.0,
        sample_text(info)
    )
}

/// Frame `n` of the tone at `rate` (both channels carry it).
fn tone_sample(n: u64, rate: u32) -> f64 {
    let amp = 10f64.powf(TONE_DBFS / 20.0);
    amp * (std::f64::consts::TAU * TONE_HZ * (n as f64) / f64::from(rate)).sin()
}

/// A player state that carries only what the diagnostics text reads: the output.
fn state_for(info: &OutputInfo, modes: &[OutputMode]) -> PlayerState {
    let mut output = OutputStatus::default();
    output.mode = info.mode;
    output.modes = modes.to_vec();
    PlayerState {
        revision: 0,
        play_state: PlayState::Stopped,
        current: None,
        position_ms: 0,
        position_at: Instant::now(),
        volume: 100,
        balance: 0,
        replay_gain: Default::default(),
        eq: EqState {
            enabled: false,
            preamp_db: 0.0,
            bands_db: [0.0; BAND_COUNT],
        },
        shuffle: false,
        repeat: RepeatMode::Off,
        queue: QueueState::default(),
        providers: Vec::new(),
        output_summary: summary(info),
        output,
        lyrics: LyricsState::Off,
        lyrics_enabled: false,
        remote: None,
        power: Default::default(),
    }
}

/// The device a run asks for: the named one if the output lists it (by name or id), else the
/// name as given (the open then fails with the fixed "device not found").
fn resolve_device(engine: &mut Engine, wanted: Option<&str>) -> Option<String> {
    let wanted = wanted?;
    let listed = engine.devices().unwrap_or_default();
    let found = listed
        .iter()
        .find(|d| d.id == wanted)
        .or_else(|| listed.iter().find(|d| d.name.eq_ignore_ascii_case(wanted)));
    Some(found.map_or_else(|| wanted.to_owned(), |d| d.id.clone()))
}

/// The tone's stream: stereo PCM at `rate`.
fn tone_format(rate: u32) -> StreamFormat {
    StreamFormat {
        rate,
        channels: 2,
        source: SourceInfo {
            codec: "pcm",
            bits: None,
            bitrate_kbps: None,
            lossless: true,
            replay_gain: None,
        },
    }
}

/// How a run's tone is played into the open output: `(source, rate, duration)`, the fixed
/// failure text if it cannot. The self-test plays [`play_tone`]; a test plays a script of its
/// own, so that what renders and when is the test's to decide rather than the host's timing.
type Play<'a> = dyn FnMut(&SourceWriter, u32, Duration) -> Result<(), &'static str> + 'a;

/// Plays the tone for `duration` into the open output; the fixed failure text if it cannot.
fn play_tone(source: &SourceWriter, rate: u32, duration: Duration) -> Result<(), &'static str> {
    let ring = Duration::from_millis(u64::from(EngineConfig::default().ring_ms));
    source
        .start_track(tone_format(rate))
        .map_err(source_failure)?;
    let total = (duration.as_secs_f64() * f64::from(rate)) as u64;
    let block = u64::from(rate / BLOCKS_PER_SECOND).max(1);
    let mut buf = Vec::with_capacity(block as usize * 2);
    let mut n = 0;
    while n < total {
        let end = (n + block).min(total);
        buf.clear();
        for i in n..end {
            let s = tone_sample(i, rate);
            buf.push(s);
            buf.push(s);
        }
        source.write(&buf).map_err(source_failure)?;
        n = end;
    }
    source.finish();
    // The ring still holds up to its depth: let the tail play out before the numbers are read.
    std::thread::sleep(ring + Duration::from_millis(60));
    Ok(())
}

/// `diag` with the engine's lifetime counters made this run's: every run shares one engine, so
/// its underruns, tap drops, clips, limiter frames, frames decoded and callback CPU average count
/// from the engine's start. `base` is the status read before this run's open (no callback runs
/// between the previous close and that open); `now` is read with `diag`. The engine's counters
/// themselves stay cumulative (the diagnostics window relies on that).
fn this_run(
    mut diag: AudioDiagnostics,
    base: &EngineStatus,
    now: &EngineStatus,
) -> AudioDiagnostics {
    let since = |v: Option<u64>, b: u64| v.map(|v| v.saturating_sub(b));
    diag.output.underruns = since(diag.output.underruns, base.underruns);
    diag.output.tap_drops = since(diag.output.tap_drops, base.tap_drops);
    diag.dsp.clip_count = since(diag.dsp.clip_count, base.clips);
    diag.dsp.limiter_frames = since(diag.dsp.limiter_frames, base.limiter_frames);
    diag.decode.frames_decoded = since(diag.decode.frames_decoded, base.source_frames);
    // The engine keeps the average over all callbacks; this run's is the difference of the
    // totals (each total off by under 1 ns per callback from the average's rounding).
    let callbacks = now.callbacks.saturating_sub(base.callbacks);
    diag.live.callback_cpu_avg_us = (callbacks > 0).then(|| {
        let total = |s: &EngineStatus| u128::from(s.cpu_avg_ns) * u128::from(s.callbacks);
        let avg_ns = total(now).saturating_sub(total(base)) / u128::from(callbacks);
        u32::try_from(avg_ns / 1000).unwrap_or(u32::MAX)
    });
    diag
}

fn live(diag: &AudioDiagnostics) -> (Option<u64>, Option<u32>, Option<f32>) {
    (
        diag.output.underruns,
        diag.live.callback_cpu_max_us,
        diag.live.clock_drift_ppm,
    )
}

#[allow(clippy::too_many_arguments)]
fn one_run(
    engine: &mut Engine,
    source: &SourceWriter,
    monitor: &EngineMonitor,
    modes: &[OutputMode],
    run: Run,
    device: Option<&str>,
    duration: Duration,
    play: &mut Play<'_>,
    out: &mut dyn Write,
) -> io::Result<Outcome> {
    writeln!(out, "\n== {}, {} Hz ==", mode_name(run.mode), run.rate)?;
    let request = OutputRequest {
        device: device.map(str::to_owned),
        mode: run.mode,
        // Exclusive asks for the track's rate; shared runs at the device's own and resamples.
        rate: (run.mode == OutputMode::Exclusive).then_some(run.rate),
        formats: Vec::new(),
    };
    // The counters as the previous run left them (its stream is closed, nothing renders).
    let base = engine.status();
    let info = match engine.open(&request) {
        Ok(info) => info,
        Err(error) => {
            let failure = open_failure(error);
            writeln!(out, "not opened: {failure}")?;
            return Ok(Outcome {
                run,
                failure: Some(failure),
                underruns: None,
                cpu_max_us: None,
                drift_ppm: None,
            });
        }
    };
    writeln!(out, "opened: {}", summary(&info))?;
    let played = play(source, run.rate, duration);
    let now = monitor.status();
    let diag = this_run(monitor.diagnostics(), &base, &now);
    let (underruns, cpu_max_us, drift_ppm) = live(&diag);
    match played {
        Ok(()) => {
            let text = whipspot_ui::diagnostics::text(&diag, &state_for(&info, modes));
            write!(out, "{text}")?;
        }
        Err(failure) => writeln!(out, "stopped: {failure}")?,
    }
    engine.close();
    Ok(Outcome {
        run,
        failure: played.err(),
        underruns,
        cpu_max_us,
        drift_ppm,
    })
}

fn summary_line(o: &Outcome) -> String {
    let head = format!("{} {} Hz:", mode_name(o.run.mode), o.run.rate);
    if let Some(failure) = o.failure {
        return format!("{head} failed ({failure})");
    }
    let cpu = o
        .cpu_max_us
        .map_or_else(|| "n/a".to_owned(), |us| format!("{us} us"));
    let drift = o
        .drift_ppm
        .map_or_else(|| "n/a".to_owned(), |ppm| format!("{ppm:+.1} ppm"));
    let underruns = o
        .underruns
        .map_or_else(|| "n/a".to_owned(), |n| n.to_string());
    format!("{head} opened, underruns {underruns}, callback CPU max {cpu}, clock drift {drift}")
}

/// Runs every run in `runs` in order on `output` and writes the report to `out`: a notice, then
/// per run a header and the diagnostics text, then one summary line per run. Returns each
/// run's outcome; see [`succeeded`] for the exit rule. `device` is a device name or id; `None`
/// is the system default. Writes only to `out`.
pub fn run(
    output: Box<dyn AudioOutput>,
    runs: &[Run],
    device: Option<&str>,
    duration: Duration,
    out: &mut dyn Write,
) -> io::Result<Vec<Outcome>> {
    run_with(output, runs, device, duration, out, &mut play_tone)
}

/// [`run`] with each run's tone played by `play`.
fn run_with(
    output: Box<dyn AudioOutput>,
    runs: &[Run],
    device: Option<&str>,
    duration: Duration,
    out: &mut dyn Write,
    play: &mut Play<'_>,
) -> io::Result<Vec<Outcome>> {
    let parts = Engine::new(EngineConfig::default(), Some(output));
    let mut engine = parts.engine;
    let (source, mut tap) = (parts.source, parts.tap);
    let monitor = engine.monitor();
    let modes = engine.modes();
    let device = resolve_device(&mut engine, device);
    writeln!(
        out,
        "Whipspot audio self-test: a {TONE_HZ} Hz tone at {TONE_DBFS} dBFS plays for {} s per run on {}.",
        duration.as_secs_f64(),
        device.as_deref().unwrap_or("the system default device")
    )?;
    writeln!(out, "It is quiet; turn your volume down first if unsure.")?;
    // In the app the spectrum reads the tap; here a reader thread empties it, so "Tap drops"
    // counts the tap's real drops rather than everything an unread tap could not hold.
    let reading = AtomicBool::new(true);
    let outcomes = std::thread::scope(|scope| {
        scope.spawn(|| {
            while reading.load(Ordering::Relaxed) {
                tap.drain(|_| {});
                std::thread::sleep(TAP_POLL);
            }
        });
        let mut outcomes = Vec::with_capacity(runs.len());
        let mut result = Ok(());
        for &run in runs {
            match one_run(
                &mut engine,
                &source,
                &monitor,
                &modes,
                run,
                device.as_deref(),
                duration,
                play,
                out,
            ) {
                Ok(outcome) => outcomes.push(outcome),
                Err(error) => {
                    result = Err(error);
                    break;
                }
            }
        }
        reading.store(false, Ordering::Relaxed);
        result.map(|()| outcomes)
    })?;
    writeln!(out, "\nSummary")?;
    for outcome in &outcomes {
        writeln!(out, "{}", summary_line(outcome))?;
    }
    Ok(outcomes)
}

/// What `--self-test` was given.
#[derive(Debug, PartialEq, Eq)]
pub struct SelfTestArgs {
    pub device: Option<String>,
}

const USAGE: &str = "usage: whipspot-app --self-test [--device <name>]";

/// The arguments after `--self-test`. `Err` is the fixed usage text.
pub fn parse_args(args: &[String]) -> Result<SelfTestArgs, &'static str> {
    let mut device = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--device" if device.is_none() => {
                device = Some(it.next().ok_or(USAGE)?.clone());
            }
            _ => return Err(USAGE),
        }
    }
    Ok(SelfTestArgs { device })
}

/// `main`'s entry: `args` are those after `--self-test`. Prints to stdout; exit 0 when at least
/// one run opened, 1 otherwise (or when the report could not be written), 2 for bad usage.
pub fn main_with(args: &[String]) -> ExitCode {
    let parsed = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
    let output = OutputRouter::new();
    let runs = plan(&output.modes());
    let stdout = io::stdout();
    let mut out = stdout.lock();
    match run(
        Box::new(output),
        &runs,
        parsed.device.as_deref(),
        RUN_DURATION,
        &mut out,
    ) {
        Ok(outcomes) if succeeded(&outcomes) => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whipspot_core::engine::{DeviceFormat, OpenFailed, OutputDevice, Renderer};
    use whipspot_testkit::{ClockedOutput, FakeOutput};

    /// A clocked output that offers both modes and refuses the ones in `refuse`.
    struct Gate {
        inner: ClockedOutput,
        refuse: Vec<OutputMode>,
    }

    impl Gate {
        fn new(refuse: &[OutputMode]) -> Self {
            Gate {
                inner: ClockedOutput::new(48_000),
                refuse: refuse.to_vec(),
            }
        }
    }

    impl AudioOutput for Gate {
        fn devices(&mut self) -> Result<Vec<OutputDevice>, OutputError> {
            Ok(vec![OutputDevice {
                id: "dev-1".into(),
                name: "Test Device".into(),
                is_default: true,
            }])
        }

        fn open(
            &mut self,
            request: &OutputRequest,
            renderer: Renderer,
        ) -> Result<OutputInfo, OpenFailed> {
            if self.refuse.contains(&request.mode) || request.device.as_deref() == Some("missing") {
                let error = if request.device.is_some() {
                    OutputError::NoDevice
                } else {
                    OutputError::Busy
                };
                return Err(OpenFailed { error, renderer });
            }
            self.inner.open(request, renderer)
        }

        fn close(&mut self) -> Option<Renderer> {
            self.inner.close()
        }

        fn modes(&self) -> Vec<OutputMode> {
            vec![OutputMode::Shared, OutputMode::Exclusive]
        }
    }

    const SHORT: Duration = Duration::from_millis(250);

    fn report(output: Gate, device: Option<&str>) -> (Vec<Outcome>, String) {
        let runs = plan(&output.modes());
        let mut buf = Vec::new();
        let outcomes = run(Box::new(output), &runs, device, SHORT, &mut buf).expect("report");
        (outcomes, String::from_utf8(buf).expect("utf8"))
    }

    #[test]
    fn plan_is_every_mode_at_both_rates() {
        let runs = plan(&[OutputMode::Shared, OutputMode::Exclusive]);
        assert_eq!(runs.len(), 4);
        assert_eq!(
            runs[0],
            Run {
                mode: OutputMode::Shared,
                rate: 44_100
            }
        );
        assert_eq!(
            runs[3],
            Run {
                mode: OutputMode::Exclusive,
                rate: 96_000
            }
        );
        assert!(plan(&[]).is_empty());
    }

    #[test]
    fn tone_is_a_1_khz_sine_at_minus_30_dbfs() {
        let rate = 48_000;
        let peak = (0..u64::from(rate))
            .map(|n| tone_sample(n, rate).abs())
            .fold(0.0, f64::max);
        let want = 10f64.powf(-30.0 / 20.0);
        assert!((peak - want).abs() < 1e-3, "peak {peak}");
        // 48 frames per period at 1 kHz: sample 12 is the crest, sample 24 a zero crossing.
        assert!((tone_sample(12, rate) - want).abs() < 1e-9);
        assert!(tone_sample(24, rate).abs() < 1e-9);
    }

    #[test]
    fn every_run_opens_and_prints_the_diagnostics_text() {
        let (outcomes, text) = report(Gate::new(&[]), None);
        assert_eq!(outcomes.len(), 4);
        assert!(outcomes.iter().all(Outcome::opened), "{outcomes:?}");
        assert!(succeeded(&outcomes));
        assert!(text.contains("Whipspot audio self-test"));
        assert!(text.contains("system default device"));
        assert_eq!(text.matches("Audio diagnostics").count(), 4, "{text}");
        assert!(text.contains("== Shared, 44100 Hz =="));
        assert!(text.contains("== Exclusive, 96000 Hz =="));
        assert!(text.contains("Verdict:"));
        // The reader thread empties the tap: a 96 kHz run renders more than the tap holds, so
        // without it these runs would count drops.
        assert_eq!(text.matches("Tap drops: 0\n").count(), 4, "{text}");
        let summary = text.split("\nSummary\n").nth(1).expect("summary section");
        assert_eq!(summary.lines().count(), 4);
        assert!(summary.contains("Shared 44100 Hz: opened, underruns"));
    }

    #[test]
    fn a_refused_mode_prints_a_fixed_failure_and_the_rest_still_run() {
        let (outcomes, text) = report(Gate::new(&[OutputMode::Exclusive]), None);
        assert_eq!(outcomes.iter().filter(|o| o.opened()).count(), 2);
        assert!(succeeded(&outcomes));
        assert!(text.contains("Exclusive 44100 Hz: failed (device in use)"));
        assert!(text.contains("not opened: device in use"));
        assert_eq!(text.matches("Audio diagnostics").count(), 2);
    }

    #[test]
    fn nothing_opening_fails_the_exit_rule() {
        let (outcomes, text) = report(
            Gate::new(&[OutputMode::Shared, OutputMode::Exclusive]),
            None,
        );
        assert!(outcomes.iter().all(|o| !o.opened()));
        assert!(!succeeded(&outcomes));
        assert!(!succeeded(&[]));
        assert!(!text.contains("Audio diagnostics"));
    }

    #[test]
    fn a_named_device_resolves_by_name_and_an_unknown_one_fails_fixed() {
        let (outcomes, text) = report(Gate::new(&[]), Some("test device"));
        assert!(succeeded(&outcomes));
        assert!(text.contains("on dev-1."), "{text}");
        let (outcomes, text) = report(Gate::new(&[]), Some("missing"));
        assert!(!succeeded(&outcomes));
        assert!(text.contains("Shared 44100 Hz: failed (device not found)"));
    }

    /// The underruns line of a printed diagnostics text.
    fn underruns_line(text: &str) -> &str {
        text.lines()
            .find(|l| l.contains("Underruns"))
            .expect("an underruns line")
            .trim_end()
    }

    // Every run shares one engine, whose counters live as long as it does: each run reports its
    // own. The output is a `FakeOutput` whose callbacks the scripted tone below runs itself, on
    // the test's thread between its writes, so each count is exact on any host, however loaded
    // (no second thread, no sleep). Each callback after the track starts and before any audio is
    // written comes up short (an underrun); the ones with audio queued for all of their frames do
    // not, nor the one after the end (silence by design). Run 1 starves three callbacks, run 2
    // one: the engine counts four in all, and run 2 reports its own one.
    #[test]
    fn each_run_reports_its_own_underruns() {
        const RATE: u32 = 48_000;
        const BLOCK: usize = 480;
        const FED: usize = 4;
        let output = FakeOutput::new(
            DeviceFormat {
                rate: RATE,
                channels: 2,
                sample: SampleFormat::F32,
                valid_bits: 32,
            },
            OutputMode::Shared,
        );
        let device = output.clone();
        let mut starved = [3, 1].into_iter();
        let mut play = |source: &SourceWriter, rate: u32, _: Duration| {
            let starve = starved.next().expect("a script per run");
            source
                .start_track(tone_format(rate))
                .map_err(source_failure)?;
            for _ in 0..starve {
                device.render_f32(BLOCK);
            }
            // Well inside the ring's 250 ms, so the write never waits for a callback.
            let tone: Vec<f64> = (0..(FED * BLOCK) as u64)
                .flat_map(|n| [tone_sample(n, rate); 2])
                .collect();
            source.write(&tone).map_err(source_failure)?;
            for _ in 0..FED {
                device.render_f32(BLOCK);
            }
            source.finish();
            device.render_f32(BLOCK);
            Ok(())
        };
        let run48 = Run {
            mode: OutputMode::Shared,
            rate: RATE,
        };
        let mut buf = Vec::new();
        let outcomes = run_with(
            Box::new(output),
            &[run48, run48],
            None,
            Duration::from_millis(40),
            &mut buf,
            &mut play,
        )
        .expect("report");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(outcomes.iter().all(Outcome::opened), "{text}");
        assert_eq!(outcomes[0].underruns, Some(3), "{text}");
        assert_eq!(outcomes[1].underruns, Some(1), "{text}");
        // The printed diagnostics and the summary lines say the same.
        let runs: Vec<&str> = text.split("== Shared, 48000 Hz ==").skip(1).collect();
        assert_eq!(runs.len(), 2, "{text}");
        assert!(underruns_line(runs[0]).ends_with(" 3"), "{text}");
        assert!(underruns_line(runs[1]).ends_with(" 1"), "{text}");
        let summary = text.split("\nSummary\n").nth(1).expect("summary section");
        let lines: Vec<&str> = summary.lines().collect();
        assert_eq!(lines.len(), 2, "{summary}");
        assert!(
            lines[0].starts_with("Shared 48000 Hz: opened, underruns 3,"),
            "{summary}"
        );
        assert!(
            lines[1].starts_with("Shared 48000 Hz: opened, underruns 1,"),
            "{summary}"
        );
    }

    #[test]
    fn arguments() {
        let args = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(parse_args(&[]), Ok(SelfTestArgs { device: None }));
        assert_eq!(
            parse_args(&args(&["--device", "Speakers"])),
            Ok(SelfTestArgs {
                device: Some("Speakers".into())
            })
        );
        assert!(parse_args(&args(&["--device"])).is_err());
        assert!(parse_args(&args(&["--device", "a", "--device", "b"])).is_err());
        assert!(parse_args(&args(&["--other"])).is_err());
    }
}

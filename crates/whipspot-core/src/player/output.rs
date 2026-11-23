//! The output choice's fallback chain and the labels of what opened (MD40).

use super::{NO_OUTPUT, output_summary};
use crate::engine::{OutputError, OutputMode, OutputRequest, Reopened};

/// The requests to try, in order, for the user's choice: Exclusive(d) → Shared(d) → Shared
/// (default) when a device was named; Shared(d) → Shared(default). Only the exclusive entry
/// carries `rate`.
pub(crate) fn chain(
    device: Option<&str>,
    mode: OutputMode,
    rate: Option<u32>,
) -> Vec<OutputRequest> {
    let request = |device: Option<&str>, mode, rate| OutputRequest {
        device: device.map(str::to_owned),
        mode,
        rate,
        formats: Vec::new(),
    };
    let mut requests = Vec::with_capacity(3);
    if mode == OutputMode::Exclusive {
        requests.push(request(device, OutputMode::Exclusive, rate));
    }
    requests.push(request(device, OutputMode::Shared, None));
    if device.is_some() {
        requests.push(request(None, OutputMode::Shared, None));
    }
    requests
}

/// The summary while the output is released: "<Shared|Exclusive> mode, <device>, idle", with
/// the mode chosen.
pub(crate) fn idle_summary(mode: OutputMode, device: Option<&str>) -> String {
    let mode = match mode {
        OutputMode::Shared => "Shared",
        OutputMode::Exclusive => "Exclusive",
    };
    format!("{mode} mode, {}, idle", device.unwrap_or("default device"))
}

/// What a [`chain`] reopen came to, for the state and the notices.
#[derive(Debug, PartialEq)]
pub(crate) struct Outcome {
    pub(crate) summary: String,
    /// Why what plays is not what was chosen.
    pub(crate) fallback: Option<&'static str>,
    /// The mode that opened; `None` when every attempt failed.
    pub(crate) opened: Option<OutputMode>,
    /// An exclusive choice plays shared.
    pub(crate) exclusive_lost: bool,
    /// A named device's attempts failed; the default device plays.
    pub(crate) device_lost: bool,
}

const DEVICE_MISSING: &str = "chosen device not found";

/// Why the exclusive attempt failed, for the summary and `fallback`.
fn exclusive_reason(e: OutputError) -> &'static str {
    match e {
        OutputError::Busy => "device in use",
        OutputError::Unsupported => "no matching exclusive format",
        OutputError::NoDevice => "device not found",
        OutputError::NoAdapter => "not available on this system",
        OutputError::Failed => "could not open",
    }
}

/// The exclusive reason plus the missing device (`NoDevice` is the device's own fault and
/// reads as the missing device alone).
fn exclusive_and_device(e: OutputError) -> &'static str {
    match e {
        OutputError::Busy => "device in use; chosen device not found",
        OutputError::Unsupported => "no matching exclusive format; chosen device not found",
        OutputError::NoDevice => DEVICE_MISSING,
        OutputError::NoAdapter => "not available on this system; chosen device not found",
        OutputError::Failed => "could not open; chosen device not found",
    }
}

/// Label the result of reopening with `requests` (a [`chain`]; its first entry is the choice).
pub(crate) fn outcome(requests: &[OutputRequest], reopened: &Reopened) -> Outcome {
    let Some((index, info)) = &reopened.opened else {
        return Outcome {
            summary: NO_OUTPUT.to_owned(),
            fallback: None,
            opened: None,
            exclusive_lost: false,
            device_lost: false,
        };
    };
    let summary = output_summary(info);
    let (Some(chosen), Some(used)) = (requests.first(), requests.get(*index)) else {
        return Outcome {
            summary,
            fallback: None,
            opened: Some(info.mode),
            exclusive_lost: false,
            device_lost: false,
        };
    };
    let mode_lost = chosen.mode == OutputMode::Exclusive && used.mode != OutputMode::Exclusive;
    let device_lost = chosen.device.is_some() && used.device.is_none();
    // The exclusive attempt is the first one.
    let first_error = reopened
        .errors
        .first()
        .copied()
        .unwrap_or(OutputError::Failed);
    let (fallback, exclusive_lost) = match (mode_lost, device_lost) {
        (false, false) => (None, false),
        (true, false) => (Some(exclusive_reason(first_error)), true),
        (false, true) => (Some(DEVICE_MISSING), false),
        (true, true) => (
            Some(exclusive_and_device(first_error)),
            first_error != OutputError::NoDevice,
        ),
    };
    let summary = match fallback {
        None => summary,
        Some(reason) if exclusive_lost => format!("{summary} (exclusive unavailable: {reason})"),
        Some(reason) => format!("{summary} ({reason})"),
    };
    Outcome {
        summary,
        fallback,
        opened: Some(info.mode),
        exclusive_lost,
        device_lost,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{DeviceFormat, OutputInfo, SampleFormat};

    fn req(device: Option<&str>, mode: OutputMode, rate: Option<u32>) -> OutputRequest {
        OutputRequest {
            device: device.map(str::to_owned),
            mode,
            rate,
            formats: Vec::new(),
        }
    }

    fn info(mode: OutputMode) -> OutputInfo {
        OutputInfo {
            host_api: "fake".into(),
            device: "dac".into(),
            mode,
            format: DeviceFormat {
                rate: 96_000,
                channels: 2,
                sample: SampleFormat::I32,
                valid_bits: 24,
            },
            buffer_frames: None,
            latency_ms: None,
        }
    }

    #[test]
    fn chain_shapes() {
        use OutputMode::{Exclusive, Shared};
        assert_eq!(
            chain(Some("d"), Exclusive, Some(96_000)),
            [
                req(Some("d"), Exclusive, Some(96_000)),
                req(Some("d"), Shared, None),
                req(None, Shared, None),
            ]
        );
        assert_eq!(
            chain(None, Exclusive, Some(44_100)),
            [req(None, Exclusive, Some(44_100)), req(None, Shared, None)]
        );
        assert_eq!(
            chain(Some("d"), Shared, Some(48_000)),
            [req(Some("d"), Shared, None), req(None, Shared, None)]
        );
        assert_eq!(chain(None, Shared, None), [req(None, Shared, None)]);
    }

    #[test]
    fn labels() {
        use OutputMode::{Exclusive, Shared};
        let base = "Shared mode, dac, 96 kHz, 24-bit in 32";
        let reopened = |index: usize, mode, errors: Vec<OutputError>| Reopened {
            opened: Some((index, info(mode))),
            errors,
        };
        // The choice opened.
        let ex = chain(Some("d"), Exclusive, Some(96_000));
        let o = outcome(&ex, &reopened(0, Exclusive, vec![]));
        assert_eq!(o.summary, "Exclusive mode, dac, 96 kHz, 24-bit in 32");
        assert_eq!(
            (o.fallback, o.exclusive_lost, o.device_lost),
            (None, false, false)
        );
        assert_eq!(o.opened, Some(Exclusive));
        // Exclusive unavailable, every reason.
        for (e, text) in [
            (OutputError::Busy, "device in use"),
            (OutputError::Unsupported, "no matching exclusive format"),
            (OutputError::NoDevice, "device not found"),
            (OutputError::NoAdapter, "not available on this system"),
            (OutputError::Failed, "could not open"),
        ] {
            let o = outcome(&ex, &reopened(1, Shared, vec![e]));
            assert_eq!(o.summary, format!("{base} (exclusive unavailable: {text})"));
            assert_eq!(o.fallback, Some(text));
            assert!(o.exclusive_lost && !o.device_lost);
        }
        // Exclusive and the device lost.
        let o = outcome(
            &ex,
            &reopened(2, Shared, vec![OutputError::Busy, OutputError::NoDevice]),
        );
        assert_eq!(
            o.summary,
            format!("{base} (exclusive unavailable: device in use; chosen device not found)")
        );
        assert_eq!(o.fallback, Some("device in use; chosen device not found"));
        assert!(o.exclusive_lost && o.device_lost);
        // The device is missing in both modes: the device alone.
        let o = outcome(
            &ex,
            &reopened(
                2,
                Shared,
                vec![OutputError::NoDevice, OutputError::NoDevice],
            ),
        );
        assert_eq!(o.summary, format!("{base} (chosen device not found)"));
        assert_eq!(o.fallback, Some("chosen device not found"));
        assert!(!o.exclusive_lost && o.device_lost);
        // Shared on a missing device.
        let sh = chain(Some("d"), Shared, None);
        let o = outcome(&sh, &reopened(1, Shared, vec![OutputError::NoDevice]));
        assert_eq!(o.summary, format!("{base} (chosen device not found)"));
        assert_eq!(o.fallback, Some("chosen device not found"));
        assert!(!o.exclusive_lost && o.device_lost);
        // Nothing opened.
        let o = outcome(
            &sh,
            &Reopened {
                opened: None,
                errors: vec![OutputError::Failed; 2],
            },
        );
        assert_eq!(o.summary, "No audio output");
        assert_eq!((o.opened, o.fallback), (None, None));
        // Idle.
        assert_eq!(
            idle_summary(OutputMode::Exclusive, None),
            "Exclusive mode, default device, idle"
        );
        assert_eq!(
            idle_summary(OutputMode::Exclusive, Some("dac")),
            "Exclusive mode, dac, idle"
        );
        assert_eq!(
            idle_summary(OutputMode::Shared, Some("dac")),
            "Shared mode, dac, idle"
        );
    }
}

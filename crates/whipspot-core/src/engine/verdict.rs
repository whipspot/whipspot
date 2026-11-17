//! The path verdict (D24, MD27): computed from what the renderer actually applied, never from
//! the control settings.

use crate::diagnostics::Verdict;

use super::format::{OutputInfo, OutputMode};
use super::quantise::holds_source;
use super::status::EngineStatus;

/// The verdict on the path and whether the source is lossy; `(None, None)` with no playing
/// track. BIT-PERFECT needs exclusive mode and the exact path; a resampled track is RESAMPLED;
/// anything else is LOSSLESS-PROCESSED. Reasons are fixed texts.
pub fn verdict(info: &OutputInfo, status: &EngineStatus) -> (Option<Verdict>, Option<bool>) {
    let Some(track) = status.track else {
        return (None, None);
    };
    let lossy = Some(!track.format.source.lossless);
    let mut reasons = Vec::new();
    if track.resampled {
        reasons.push(format!(
            "resampled {} Hz to {} Hz (sinc)",
            track.format.rate, info.format.rate
        ));
    }
    let a = &status.applied;
    for (on, text) in [
        (a.replay_gain, "ReplayGain"),
        (a.eq, "EQ"),
        (a.pre_gain, "preamp and headroom"),
        (a.volume, "volume"),
        (a.balance, "balance"),
        (a.limiter_engaged, "limiter engaged"),
        (a.dither, "TPDF dither"),
        (a.fade, "pause fade"),
    ] {
        if on {
            reasons.push(text.to_owned());
        }
    }
    if !holds_source(&track.format, &info.format) {
        reasons.push("device format does not hold the source".to_owned());
    }
    if info.mode == OutputMode::Shared {
        reasons.push("shared mode: the system mixer processes the output".to_owned());
    }
    let v = if track.resampled {
        Verdict::Resampled { reasons }
    } else if info.mode == OutputMode::Exclusive && a.exact {
        Verdict::BitPerfect
    } else {
        Verdict::LosslessProcessed { reasons }
    };
    (Some(v), lossy)
}

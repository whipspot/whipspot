//! The power seam (efficiency mode): the power mode the player wants, decided from what is shown
//! and what plays, and the adapter that asks the OS for it. The player's control task decides
//! the mode with [`wanted`] and calls [`PowerControl::set`] only when it changes; the adapter owns
//! every OS call. Nothing here names an OS or touches a thread.

/// The power mode the player asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PowerMode {
    /// Shown, or the efficiency setting is off.
    #[default]
    Normal,
    /// Hidden, audio open or loading: the front-end and network threads save power, the audio path
    /// never does.
    Eco,
    /// Hidden and silent: the whole process saves power.
    EcoIdle,
}

/// Why [`PowerControl::set`] failed. The texts are fixed and name no OS call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerError {
    /// This OS or build has no power control.
    Unsupported,
    /// The OS refused the change.
    Refused,
}

/// What the player reports about power to the front-end (the Diagnostics window).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PowerStatus {
    /// The adapter's name, or `None` when no adapter is wired.
    pub name: Option<&'static str>,
    /// The efficiency setting.
    pub enabled: bool,
    /// Whether any of our windows is shown.
    pub shown: bool,
    /// The mode last applied.
    pub mode: PowerMode,
    /// The last refusal, if the last `set` failed.
    pub error: Option<PowerError>,
}

impl Default for PowerStatus {
    fn default() -> Self {
        PowerStatus {
            name: None,
            enabled: true,
            shown: true,
            mode: PowerMode::Normal,
            error: None,
        }
    }
}

/// Sets the process's power mode. Held as `Arc<dyn PowerControl>` by the player and shared with
/// the adapter that applies it.
///
/// `set` is idempotent, never panics, returns only the fixed [`PowerError`]s, and is called only
/// when the wanted mode changes.
pub trait PowerControl: Send + Sync {
    /// A short fixed name for the Diagnostics window.
    fn name(&self) -> &'static str;
    /// Apply `mode`.
    fn set(&self, mode: PowerMode) -> Result<(), PowerError>;
}

/// The mode the player wants: Normal while shown or when the setting is off; otherwise Eco while
/// audio is open, opening or loading, and EcoIdle when silent.
pub fn wanted(enabled: bool, shown: bool, audio_active: bool) -> PowerMode {
    if !enabled || shown {
        PowerMode::Normal
    } else if audio_active {
        PowerMode::Eco
    } else {
        PowerMode::EcoIdle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wanted_covers_all_eight_inputs() {
        // (enabled, shown, audio_active, expected)
        let table = [
            (false, false, false, PowerMode::Normal),
            (false, false, true, PowerMode::Normal),
            (false, true, false, PowerMode::Normal),
            (false, true, true, PowerMode::Normal),
            (true, true, false, PowerMode::Normal),
            (true, true, true, PowerMode::Normal),
            (true, false, true, PowerMode::Eco),
            (true, false, false, PowerMode::EcoIdle),
        ];
        for (enabled, shown, audio, expected) in table {
            assert_eq!(
                wanted(enabled, shown, audio),
                expected,
                "wanted({enabled}, {shown}, {audio})"
            );
        }
    }

    #[test]
    fn power_status_default_is_enabled_shown_and_normal() {
        assert_eq!(
            PowerStatus::default(),
            PowerStatus {
                name: None,
                enabled: true,
                shown: true,
                mode: PowerMode::Normal,
                error: None,
            }
        );
    }
}

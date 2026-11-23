//! Preferences in `settings.json` (MD14). Holds no credential, token or client id, and never
//! will: those stay in the credential store.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use whipspot_core::engine::{BAND_COUNT, ReplayGainMode};
use whipspot_core::skin::{MAX_PANEL_SIDE, PanelId};
use whipspot_core::text::display_text;
use whipspot_ui::StreamQuality;
use whipspot_ui::theme::ThemeId;

use crate::paths::AppPaths;

/// The only format version this build reads or writes.
const VERSION: u32 = 1;
const MAX_VOLUME: u8 = 100;
/// The balance range, -100 (left) to 100 (right); 0 is centre.
const MAX_BALANCE: i8 = 100;
const MIN_WIDTH: f64 = 640.0;
const MAX_WIDTH: f64 = 10_000.0;
const MIN_HEIGHT: f64 = 400.0;
const MAX_HEIGHT: f64 = 10_000.0;
/// The equalizer's gain range, dB (both the bands and the preamp).
const EQ_MAX_DB: f32 = 12.0;
/// The most saved skin panels (D11).
const MAX_SKIN_PANELS: usize = 16;
/// The most saved music folders, and the longest provider id kept.
const MAX_MUSIC_FOLDERS: usize = 64;
const MAX_PROVIDER_ID: usize = 64;

/// The output mode: the system's shared mixer (the default; never takes the device, MD44).
pub const OUTPUT_MODE_SHARED: &str = "shared";
/// The output mode: exclusive access to the device, where the OS allows it (D18).
pub const OUTPUT_MODE_EXCLUSIVE: &str = "exclusive";
/// The front-end: the native look (the default).
pub const FRONT_END_NATIVE: &str = "native";
/// The front-end: the skin named by `skin` (D11).
pub const FRONT_END_SKIN: &str = "skin";
/// ReplayGain: not applied (the default; the path stays bit-perfect, D8).
pub const REPLAY_GAIN_OFF: &str = "off";
/// ReplayGain: the track's gain.
pub const REPLAY_GAIN_TRACK: &str = "track";
/// ReplayGain: the album's gain, the track's when the album has none.
pub const REPLAY_GAIN_ALBUM: &str = "album";

/// The user's preferences. Missing fields take their defaults; unknown fields are ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    /// Playback volume, 0..=100.
    pub volume: u8,
    /// Left/right balance, -100..=100; 0 is centre (the gain stage stays bypassed).
    #[serde(deserialize_with = "clamped_balance")]
    pub balance: i8,
    /// The view shown at launch (for example `"playlists"`).
    pub last_view: String,
    /// The output device by name; `None` is the system default.
    pub output_device: Option<String>,
    /// `"shared"` or `"exclusive"`; any other value loads as `"shared"`.
    pub output_mode: String,
    /// Look lyrics up on LRCLIB for the current song (MD78); on by default.
    pub lyrics: bool,
    /// Let the OS save power while every window is hidden and nothing plays (D7); on by default.
    pub efficiency_mode: bool,
    /// The theme's key, `"a"`, `"b"` or `"c"`; any other value loads as `"c"` (MD89).
    pub theme: String,
    /// `"high"`, `"normal"` or `"low"` (MD88); any other value loads as `"high"`.
    pub stream_quality: String,
    /// Hide the browse view and show now playing when the pointer rests (MD50); on by default.
    pub auto_hide: bool,
    pub eq: EqSettings,
    pub window: WindowSettings,
    /// `"native"` or `"skin"`; any other value loads as `"native"` (D11).
    pub front_end: String,
    /// The skin's file name in the skins folder; `None` when none was chosen.
    pub skin: Option<String>,
    /// The skin's panels stay above other windows.
    pub skin_on_top: bool,
    /// Where the panels of `skin` were, at most 16; used only for that skin.
    pub skin_panels: Vec<SkinPanelSettings>,
    /// The Local provider's music folders (D10, D12), at most 64. `None`: never set, so the first
    /// start adds the OS Music folder; `Some(vec![])` is the user's empty list and stays empty.
    pub music_folders: Option<Vec<String>>,
    /// The provider whose library the views browse (D11); `None` is the first one with a library.
    pub library_provider: Option<String>,
    /// `"off"`, `"track"` or `"album"` (D8); any other value loads as `"off"`.
    pub replay_gain: String,
    /// The first-run tour was finished or skipped; missing loads as `false`, so every install sees it once.
    pub tour_seen: bool,
    /// The Spotify Connect speaker is on (D7, D8): on by default, and it runs only while the
    /// Playback login is stored. Saved from the Accounts step's state at exit.
    pub spotify_connect: bool,
    /// The Connect speaker's stable device id (D10): 40 lowercase hex characters, made once and
    /// kept; not a secret. Anything else loads as `None` and a new one is made.
    pub connect_device_id: Option<String>,
}

/// One skin panel as the user left it: its id, its outer top-left corner in desktop units,
/// whether it was shown and, for a panel the skin lets the user resize, its size in skin pixels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkinPanelSettings {
    pub id: String,
    pub x: i32,
    pub y: i32,
    pub visible: bool,
    /// The size in skin pixels; a value outside `1..=MAX_PANEL_SIDE` loads as `None`. A file
    /// from before sizes were saved has neither.
    #[serde(default)]
    pub w: Option<u32>,
    #[serde(default)]
    pub h: Option<u32>,
}

/// The equalizer as the user left it (MD89). Gains are dB, within ±12.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqSettings {
    pub enabled: bool,
    pub preamp_db: f32,
    /// One gain per band, [`BAND_COUNT`] long after load.
    pub bands_db: Vec<f32>,
}

/// The main window's size and state, in logical pixels, and where it was (MD90).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowSettings {
    pub width: f64,
    pub height: f64,
    pub maximized: bool,
    /// The top-left corner in physical pixels; `None` lets the OS place the window.
    pub x: Option<i32>,
    pub y: Option<i32>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: VERSION,
            volume: 70,
            balance: 0,
            last_view: "playlists".to_owned(),
            output_device: None,
            output_mode: OUTPUT_MODE_SHARED.to_owned(),
            lyrics: true,
            efficiency_mode: true,
            theme: ThemeId::default().key().to_owned(),
            stream_quality: StreamQuality::default().key().to_owned(),
            auto_hide: true,
            eq: EqSettings::default(),
            window: WindowSettings::default(),
            front_end: FRONT_END_NATIVE.to_owned(),
            skin: None,
            skin_on_top: false,
            skin_panels: Vec::new(),
            music_folders: None,
            library_provider: None,
            replay_gain: REPLAY_GAIN_OFF.to_owned(),
            tour_seen: false,
            spotify_connect: true,
            connect_device_id: None,
        }
    }
}

impl Default for EqSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            preamp_db: 0.0,
            bands_db: vec![0.0; BAND_COUNT],
        }
    }
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            width: 1180.0,
            height: 740.0,
            maximized: false,
            x: None,
            y: None,
        }
    }
}

/// Shown once when the stored settings were unusable and defaults are in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadNotice {
    /// The file was unreadable, invalid, or from a newer version; it was copied to `.bak`.
    Reset,
}

impl LoadNotice {
    /// The fixed text shown to the user.
    pub fn text(self) -> &'static str {
        match self {
            Self::Reset => "Settings could not be read; defaults are used.",
        }
    }
}

impl fmt::Display for LoadNotice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text())
    }
}

/// A save failed. The text is fixed: no OS error detail reaches the user or the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsError;

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("could not save settings")
    }
}

impl std::error::Error for SettingsError {}

impl Settings {
    /// Reads `settings.json`. A missing file gives the defaults with no notice. An unreadable
    /// file, invalid JSON or a newer `version` is copied to `settings.json.bak`, and the defaults
    /// are returned with [`LoadNotice::Reset`]. Values are normalised ([`Settings::normalise`]).
    /// Never creates a directory.
    pub fn load(paths: &AppPaths) -> (Self, Option<LoadNotice>) {
        let file = paths.settings_file();
        let bytes = match fs::read(&file) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return (Self::default(), None),
            Err(_) => return reset(&file),
        };
        match serde_json::from_slice::<Self>(&bytes) {
            Ok(mut settings) if settings.version <= VERSION => {
                settings.normalise();
                (settings, None)
            }
            _ => reset(&file),
        }
    }

    /// Writes `settings.json` atomically: `settings.json.tmp` in the same directory is written
    /// and synced, then renamed over the target. Creates `config_dir` if needed.
    pub fn save(&self, paths: &AppPaths) -> Result<(), SettingsError> {
        fs::create_dir_all(&paths.config_dir).map_err(|_| SettingsError)?;
        let text = serde_json::to_vec_pretty(self).map_err(|_| SettingsError)?;
        let tmp = paths.config_dir.join("settings.json.tmp");
        let result =
            write_synced(&tmp, &text).and_then(|()| fs::rename(&tmp, paths.settings_file()));
        if result.is_err() {
            // Our own temporary name; best effort, the previous settings file stands.
            let _ = fs::remove_file(&tmp);
        }
        result.map_err(|_| SettingsError)
    }

    /// The ReplayGain mode `replay_gain` names (an unknown value is off).
    pub fn replay_gain_mode(&self) -> ReplayGainMode {
        match self.replay_gain.as_str() {
            REPLAY_GAIN_TRACK => ReplayGainMode::Track,
            REPLAY_GAIN_ALBUM => ReplayGainMode::Album,
            _ => ReplayGainMode::Off,
        }
    }

    /// Stores `mode` as the `replay_gain` text.
    pub fn set_replay_gain_mode(&mut self, mode: ReplayGainMode) {
        self.replay_gain = match mode {
            ReplayGainMode::Off => REPLAY_GAIN_OFF,
            ReplayGainMode::Track => REPLAY_GAIN_TRACK,
            ReplayGainMode::Album => REPLAY_GAIN_ALBUM,
        }
        .to_owned();
    }

    /// The music folders to start with (D12): the saved list, or, when none was ever saved
    /// (`music_folders` is `None`), the OS Music folder `os_music` if there is one. A saved empty
    /// list stays empty.
    pub fn start_music_folders(&self, os_music: Option<&Path>) -> Vec<String> {
        match &self.music_folders {
            Some(folders) => folders.clone(),
            None => os_music
                .filter(|p| p.is_dir())
                .and_then(|p| p.to_str())
                .map(|p| vec![p.to_owned()])
                .unwrap_or_default(),
        }
    }

    /// Brings loaded values into their ranges. An unknown choice takes its default; every gain is
    /// finite and within ±[`EQ_MAX_DB`] (a non-finite one becomes 0 dB); the band list is exactly
    /// [`BAND_COUNT`] long, padded with 0 dB or cut, so a short list keeps the rest of the file.
    fn normalise(&mut self) {
        if self.output_mode != OUTPUT_MODE_EXCLUSIVE {
            self.output_mode = OUTPUT_MODE_SHARED.to_owned();
        }
        self.volume = self.volume.min(MAX_VOLUME);
        self.balance = self.balance.clamp(-MAX_BALANCE, MAX_BALANCE);
        self.window.width = self.window.width.clamp(MIN_WIDTH, MAX_WIDTH);
        self.window.height = self.window.height.clamp(MIN_HEIGHT, MAX_HEIGHT);
        if ThemeId::from_key(&self.theme).is_none() {
            self.theme = ThemeId::default().key().to_owned();
        }
        self.stream_quality = StreamQuality::from_key(&self.stream_quality)
            .key()
            .to_owned();
        self.eq.preamp_db = eq_gain(self.eq.preamp_db);
        self.eq.bands_db.resize(BAND_COUNT, 0.0);
        for gain in &mut self.eq.bands_db {
            *gain = eq_gain(*gain);
        }
        if self.front_end != FRONT_END_SKIN {
            self.front_end = FRONT_END_NATIVE.to_owned();
        }
        self.skin_panels.retain(|p| PanelId::new(&p.id).is_some());
        self.skin_panels.truncate(MAX_SKIN_PANELS);
        let side = |v: &mut Option<u32>| {
            if !v.is_some_and(|s| (1..=MAX_PANEL_SIDE).contains(&s)) {
                *v = None;
            }
        };
        for p in &mut self.skin_panels {
            side(&mut p.w);
            side(&mut p.h);
        }
        if let Some(folders) = &mut self.music_folders {
            folders.retain(|f| !f.trim().is_empty() && !f.chars().any(char::is_control));
            folders.truncate(MAX_MUSIC_FOLDERS);
        }
        self.library_provider.take_if(|id| {
            id.is_empty() || id.len() > MAX_PROVIDER_ID || id.chars().any(char::is_control)
        });
        self.connect_device_id.take_if(|id| !is_device_id(id));
        if !matches!(
            self.replay_gain.as_str(),
            REPLAY_GAIN_TRACK | REPLAY_GAIN_ALBUM
        ) {
            self.replay_gain = REPLAY_GAIN_OFF.to_owned();
        }
    }
}

/// Length of the Connect device id, in lowercase hex characters (D10).
const DEVICE_ID_LEN: usize = 40;
/// The longest Connect device name, in characters (D9).
const MAX_DEVICE_NAME: usize = 64;
/// The Connect device name when the computer's name is empty (D9).
const DEVICE_NAME: &str = "Whipspot";

/// A Connect device id as [`new_device_id`] makes it: 40 lowercase hex characters.
fn is_device_id(id: &str) -> bool {
    id.len() == DEVICE_ID_LEN && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The id of `bytes` in lowercase hex.
fn device_id_from(bytes: &[u8; DEVICE_ID_LEN / 2]) -> String {
    use fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// A new random Connect device id (D10), or `None` when the OS gives no randomness (Connect is
/// then left off for this run).
pub fn new_device_id() -> Option<String> {
    let mut bytes = [0u8; DEVICE_ID_LEN / 2];
    getrandom::fill(&mut bytes).ok()?;
    Some(device_id_from(&bytes))
}

/// The name the user's Spotify apps show for this device (D9): `Whipspot on <computer name>`,
/// the computer name made safe to draw, the whole at most 64 characters; just `Whipspot` when the
/// computer has no usable name.
pub fn connect_device_name(host: &str) -> String {
    let host = display_text(host, MAX_DEVICE_NAME);
    if host.is_empty() {
        return DEVICE_NAME.to_owned();
    }
    display_text(&format!("{DEVICE_NAME} on {host}"), MAX_DEVICE_NAME)
}

impl Settings {
    /// The saved Connect device id, or a new one made now and kept (D10). `None` when there is
    /// none and none could be made.
    pub fn ensure_connect_device_id(&mut self) -> Option<&str> {
        if self.connect_device_id.is_none() {
            self.connect_device_id = new_device_id();
        }
        self.connect_device_id.as_deref()
    }
}

/// Reads the balance as a wide integer so a value outside `i8` (such as 250) clamps to
/// `-100..=100` instead of failing the whole file.
fn clamped_balance<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i8, D::Error> {
    let v = i64::deserialize(d)?;
    Ok(v.clamp(-i64::from(MAX_BALANCE), i64::from(MAX_BALANCE)) as i8)
}

/// A gain the equalizer takes: finite and within ±[`EQ_MAX_DB`].
fn eq_gain(db: f32) -> f32 {
    if db.is_finite() {
        db.clamp(-EQ_MAX_DB, EQ_MAX_DB)
    } else {
        0.0
    }
}

/// Copies the unusable `file` to `settings.json.bak` (replacing an older backup) and returns
/// the defaults with the notice. A failed copy is ignored: the notice is still shown.
fn reset(file: &Path) -> (Settings, Option<LoadNotice>) {
    let backup = file.with_extension("json.bak");
    let _ = fs::copy(file, backup);
    (Settings::default(), Some(LoadNotice::Reset))
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique scratch directory under the system temp dir, removed on drop. Only this
    /// directory is ever removed.
    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("whipspot-settings-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn save_then_load_round_trips() {
        let scratch = Scratch::new("round-trip");
        let paths = AppPaths::under(&scratch.0);
        let saved = Settings {
            version: 1,
            volume: 42,
            balance: -37,
            last_view: "liked".to_owned(),
            output_device: Some("Speakers".to_owned()),
            output_mode: OUTPUT_MODE_EXCLUSIVE.to_owned(),
            lyrics: false,
            efficiency_mode: false,
            theme: "b".to_owned(),
            stream_quality: "low".to_owned(),
            auto_hide: false,
            eq: EqSettings {
                enabled: false,
                preamp_db: -3.5,
                bands_db: vec![6.0, 5.0, 3.0, 1.0, 0.0, -1.5, -2.0, 0.5, 4.0, 12.0],
            },
            window: WindowSettings {
                width: 900.0,
                height: 600.0,
                maximized: true,
                x: Some(-1280),
                y: Some(40),
            },
            front_end: FRONT_END_SKIN.to_owned(),
            skin: Some("blue.skin".to_owned()),
            skin_on_top: true,
            skin_panels: vec![
                SkinPanelSettings {
                    id: "main".to_owned(),
                    x: -20,
                    y: 300,
                    visible: true,
                    w: Some(325),
                    h: Some(174),
                },
                SkinPanelSettings {
                    id: "library".to_owned(),
                    x: 255,
                    y: 300,
                    visible: false,
                    w: None,
                    h: None,
                },
            ],
            music_folders: Some(vec!["D:\\Music".to_owned(), "/home/u/flac".to_owned()]),
            library_provider: Some("local".to_owned()),
            replay_gain: REPLAY_GAIN_ALBUM.to_owned(),
            tour_seen: true,
            spotify_connect: false,
            connect_device_id: Some("0123456789abcdef0123456789abcdef01234567".to_owned()),
        };
        saved.save(&paths).unwrap();
        let (loaded, notice) = Settings::load(&paths);
        assert_eq!(loaded, saved);
        assert_eq!(notice, None);
    }

    #[test]
    fn lyrics_default_on_and_an_m5_file_without_the_field_loads_on() {
        assert!(Settings::default().lyrics);
        let scratch = Scratch::new("lyrics-missing");
        let paths = AppPaths::under(&scratch.0);
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::write(
            paths.settings_file(),
            br#"{"version":1,"volume":30,"output_mode":"shared"}"#,
        )
        .unwrap();
        let (loaded, notice) = Settings::load(&paths);
        assert!(loaded.lyrics);
        assert_eq!(loaded.volume, 30);
        assert_eq!(notice, None);
    }

    #[test]
    fn efficiency_mode_default_on_and_a_file_without_the_field_loads_on() {
        assert!(Settings::default().efficiency_mode);
        let scratch = Scratch::new("efficiency-missing");
        let paths = AppPaths::under(&scratch.0);
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::write(
            paths.settings_file(),
            br#"{"version":1,"volume":30,"output_mode":"shared"}"#,
        )
        .unwrap();
        let (loaded, notice) = Settings::load(&paths);
        assert!(loaded.efficiency_mode);
        assert_eq!(loaded.volume, 30);
        assert_eq!(notice, None);
        let off = load_text(
            "efficiency-off",
            br#"{"version":1,"efficiency_mode":false}"#,
        );
        assert!(!off.efficiency_mode);
    }

    /// MD116: the `audio_output` key of the deleted rodio fallback is ignored on load and absent
    /// from the next save; every other saved value is kept.
    #[test]
    fn old_audio_output_key_is_ignored_and_not_saved_again() {
        for value in ["rodio", "core", "bogus"] {
            let scratch = Scratch::new("old-audio-output");
            let paths = AppPaths::under(&scratch.0);
            fs::create_dir_all(&paths.config_dir).unwrap();
            let text = format!(
                r#"{{"version":1,"volume":55,"audio_output":"{value}","output_mode":"exclusive","output_device":"Speakers","theme":"b"}}"#
            );
            fs::write(paths.settings_file(), text).unwrap();
            let (loaded, notice) = Settings::load(&paths);
            assert_eq!(notice, None);
            assert_eq!(loaded.volume, 55);
            assert_eq!(loaded.output_mode, OUTPUT_MODE_EXCLUSIVE);
            assert_eq!(loaded.output_device.as_deref(), Some("Speakers"));
            assert_eq!(loaded.theme, "b");
            loaded.save(&paths).unwrap();
            let saved = fs::read_to_string(paths.settings_file()).unwrap();
            assert!(!saved.contains("audio_output"), "{saved}");
            let (again, notice) = Settings::load(&paths);
            assert_eq!(notice, None);
            assert_eq!(again, loaded);
        }
    }

    #[test]
    fn output_mode_defaults_to_shared_and_device_to_system_default() {
        let settings = Settings::default();
        assert_eq!(settings.output_mode, "shared");
        assert_eq!(settings.output_device, None);
    }

    #[test]
    fn output_mode_exclusive_and_device_round_trip() {
        let scratch = Scratch::new("output-exclusive");
        let paths = AppPaths::under(&scratch.0);
        let saved = Settings {
            output_device: Some("USB DAC".to_owned()),
            output_mode: OUTPUT_MODE_EXCLUSIVE.to_owned(),
            ..Settings::default()
        };
        saved.save(&paths).unwrap();
        let (loaded, notice) = Settings::load(&paths);
        assert_eq!(loaded.output_mode, "exclusive");
        assert_eq!(loaded.output_device.as_deref(), Some("USB DAC"));
        assert_eq!(loaded, saved);
        assert_eq!(notice, None);
    }

    #[test]
    fn output_mode_bogus_value_loads_as_shared() {
        let scratch = Scratch::new("output-mode-bogus");
        let paths = AppPaths::under(&scratch.0);
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::write(
            paths.settings_file(),
            br#"{"version":1,"output_mode":"bogus"}"#,
        )
        .unwrap();
        let (loaded, notice) = Settings::load(&paths);
        assert_eq!(loaded.output_mode, "shared");
        assert_eq!(notice, None);
    }

    #[test]
    fn output_fields_missing_load_defaults_without_notice() {
        let scratch = Scratch::new("output-fields-missing");
        let paths = AppPaths::under(&scratch.0);
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::write(paths.settings_file(), br#"{"version":1,"volume":55}"#).unwrap();
        let (loaded, notice) = Settings::load(&paths);
        assert_eq!(loaded.output_mode, "shared");
        assert_eq!(loaded.output_device, None);
        assert_eq!(loaded.version, 1);
        assert_eq!(notice, None);
        assert!(!paths.config_dir.join("settings.json.bak").exists());
    }

    #[test]
    fn unknown_and_missing_fields_keep_known_values_and_default_the_rest() {
        let scratch = Scratch::new("unknown-missing");
        let paths = AppPaths::under(&scratch.0);
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::write(
            paths.settings_file(),
            br#"{"version":1,"volume":30,"future_field":{"a":1},"window":{"width":800.0}}"#,
        )
        .unwrap();
        let (loaded, notice) = Settings::load(&paths);
        assert_eq!(notice, None);
        assert_eq!(loaded.volume, 30);
        assert_eq!(loaded.window.width, 800.0);
        assert_eq!(loaded.last_view, "playlists");
        assert_eq!(loaded.window.height, 740.0);
        assert!(!loaded.window.maximized);
    }

    #[test]
    fn invalid_json_gives_defaults_and_keeps_the_original_bytes_in_bak() {
        let scratch = Scratch::new("invalid");
        let paths = AppPaths::under(&scratch.0);
        fs::create_dir_all(&paths.config_dir).unwrap();
        let original: &[u8] = b"{not json at all";
        fs::write(paths.settings_file(), original).unwrap();
        let (loaded, notice) = Settings::load(&paths);
        assert_eq!(loaded, Settings::default());
        assert_eq!(notice, Some(LoadNotice::Reset));
        let backup = paths.config_dir.join("settings.json.bak");
        assert_eq!(fs::read(backup).unwrap(), original);
    }

    #[test]
    fn save_into_a_missing_directory_creates_it_and_leaves_no_tmp() {
        let scratch = Scratch::new("missing-dir");
        let paths = AppPaths::under(&scratch.0);
        assert!(!paths.config_dir.exists());
        Settings::default().save(&paths).unwrap();
        assert!(paths.settings_file().is_file());
        assert!(!paths.config_dir.join("settings.json.tmp").exists());
    }

    #[test]
    fn balance_is_clamped_on_load() {
        let scratch = Scratch::new("balance-clamp");
        let paths = AppPaths::under(&scratch.0);
        fs::create_dir_all(&paths.config_dir).unwrap();
        for (text, want) in [
            (r#"{"version":1,"balance":250}"#, 100),
            (r#"{"version":1,"balance":-128}"#, -100),
            (r#"{"version":1,"balance":-9000}"#, -100),
            (r#"{"version":1,"balance":-40}"#, -40),
            (r#"{"version":1}"#, 0),
        ] {
            fs::write(paths.settings_file(), text).unwrap();
            let (loaded, notice) = Settings::load(&paths);
            assert_eq!(notice, None, "{text}");
            assert_eq!(loaded.balance, want, "{text}");
        }
    }

    #[test]
    fn out_of_range_values_are_clamped_and_a_newer_version_is_reset() {
        let scratch = Scratch::new("clamp-version");
        let paths = AppPaths::under(&scratch.0);
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::write(
            paths.settings_file(),
            br#"{"version":1,"volume":250,"window":{"width":10,"height":99999}}"#,
        )
        .unwrap();
        let (loaded, notice) = Settings::load(&paths);
        assert_eq!(notice, None);
        assert_eq!(loaded.volume, 100);
        assert_eq!(loaded.balance, 0, "a file without balance loads centre");
        assert_eq!(loaded.window.width, 640.0);
        assert_eq!(loaded.window.height, 10_000.0);

        let newer = br#"{"version":2,"volume":10}"#;
        fs::write(paths.settings_file(), newer).unwrap();
        let (loaded, notice) = Settings::load(&paths);
        assert_eq!(loaded, Settings::default());
        assert_eq!(notice, Some(LoadNotice::Reset));
        assert_eq!(
            fs::read(paths.config_dir.join("settings.json.bak")).unwrap(),
            newer
        );
    }

    /// Writes `text` as `settings.json` and loads it.
    fn load_text(name: &str, text: &[u8]) -> Settings {
        let scratch = Scratch::new(name);
        let paths = AppPaths::under(&scratch.0);
        fs::create_dir_all(&paths.config_dir).unwrap();
        fs::write(paths.settings_file(), text).unwrap();
        let (loaded, notice) = Settings::load(&paths);
        assert_eq!(notice, None);
        loaded
    }

    #[test]
    fn an_m6_file_without_the_new_fields_loads_with_the_defaults_and_keeps_its_values() {
        let loaded = load_text(
            "m6-file",
            br#"{"version":1,"volume":30,"last_view":"liked","lyrics":false,"window":{"width":900.0,"height":600.0,"maximized":true}}"#,
        );
        assert_eq!(loaded.volume, 30);
        assert_eq!(loaded.last_view, "liked");
        assert!(!loaded.lyrics);
        assert_eq!(loaded.window.width, 900.0);
        assert!(loaded.window.maximized);
        assert_eq!(loaded.theme, "c");
        assert_eq!(loaded.stream_quality, "high");
        assert!(loaded.auto_hide, "an older file loads with auto-hide on");
        assert_eq!(loaded.eq, EqSettings::default());
        assert!(loaded.eq.enabled);
        assert_eq!(loaded.eq.preamp_db, 0.0);
        assert_eq!(loaded.eq.bands_db, vec![0.0; 10]);
        assert_eq!((loaded.window.x, loaded.window.y), (None, None));
    }

    #[test]
    fn auto_hide_defaults_on_and_a_saved_off_stays_off() {
        assert!(Settings::default().auto_hide);
        assert!(!load_text("auto-hide-off", br#"{"version":1,"auto_hide":false}"#).auto_hide);
        assert!(load_text("auto-hide-on", br#"{"version":1,"auto_hide":true}"#).auto_hide);
    }

    #[test]
    fn a_short_or_long_band_list_is_padded_or_cut_to_ten() {
        let short = load_text(
            "bands-short",
            br#"{"version":1,"eq":{"bands_db":[1.0,2.0,3.0]}}"#,
        );
        assert_eq!(
            short.eq.bands_db,
            vec![1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
        );
        let long = load_text(
            "bands-long",
            br#"{"version":1,"eq":{"bands_db":[1,2,3,4,5,6,7,8,9,10,11,12]}}"#,
        );
        assert_eq!(
            long.eq.bands_db,
            vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]
        );
    }

    #[test]
    fn gains_are_clamped_to_twelve_db_and_non_finite_ones_become_zero() {
        let loaded = load_text(
            "gains-clamped",
            br#"{"version":1,"eq":{"preamp_db":-99.0,"bands_db":[99.0,-99.0,0,0,0,0,0,0,0,12.0]}}"#,
        );
        assert_eq!(loaded.eq.preamp_db, -12.0);
        assert_eq!(loaded.eq.bands_db[0], 12.0);
        assert_eq!(loaded.eq.bands_db[1], -12.0);
        assert_eq!(loaded.eq.bands_db[9], 12.0);

        let mut settings = Settings {
            eq: EqSettings {
                enabled: true,
                preamp_db: f32::NAN,
                bands_db: vec![f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 5.0],
            },
            ..Settings::default()
        };
        settings.normalise();
        assert_eq!(settings.eq.preamp_db, 0.0);
        // Infinities are not finite either: they take 0 dB, not the clamp.
        assert_eq!(
            settings.eq.bands_db,
            vec![0.0, 0.0, 0.0, 5.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]
        );
    }

    #[test]
    fn an_unknown_theme_or_quality_loads_as_the_default() {
        let loaded = load_text(
            "theme-quality-unknown",
            br#"{"version":1,"theme":"z","stream_quality":"ultra"}"#,
        );
        assert_eq!(loaded.theme, "c");
        assert_eq!(loaded.stream_quality, "high");
        let known = load_text(
            "theme-quality-known",
            br#"{"version":1,"theme":"a","stream_quality":"normal"}"#,
        );
        assert_eq!(known.theme, "a");
        assert_eq!(known.stream_quality, "normal");
    }

    #[test]
    fn the_skin_fields_default_when_missing() {
        let loaded = load_text("skin-missing", br#"{"version":1,"volume":30}"#);
        assert_eq!(loaded.front_end, "native");
        assert_eq!(loaded.skin, None);
        assert!(!loaded.skin_on_top);
        assert!(loaded.skin_panels.is_empty());
        assert_eq!(loaded.volume, 30);
    }

    #[test]
    fn an_unknown_front_end_loads_as_native() {
        let loaded = load_text(
            "front-end-unknown",
            br#"{"version":1,"front_end":"x","skin":"a.skin"}"#,
        );
        assert_eq!(loaded.front_end, "native");
        assert_eq!(loaded.skin.as_deref(), Some("a.skin"));
        let skin = load_text("front-end-skin", br#"{"version":1,"front_end":"skin"}"#);
        assert_eq!(skin.front_end, "skin");
    }

    #[test]
    fn saved_panels_keep_sixteen_and_drop_invalid_ids() {
        let mut panels: Vec<String> = (0..17)
            .map(|i| format!(r#"{{"id":"p{i}","x":{i},"y":0,"visible":true}}"#))
            .collect();
        panels.insert(
            3,
            r#"{"id":"Bad Id","x":0,"y":0,"visible":true}"#.to_owned(),
        );
        let text = format!(r#"{{"version":1,"skin_panels":[{}]}}"#, panels.join(","));
        let loaded = load_text("skin-panels", text.as_bytes());
        assert_eq!(loaded.skin_panels.len(), 16);
        assert!(loaded.skin_panels.iter().all(|p| p.id != "Bad Id"));
        assert_eq!(loaded.skin_panels[0].id, "p0");
        assert_eq!(loaded.skin_panels[15].id, "p15");
        let one = load_text(
            "skin-panel-bad",
            br#"{"version":1,"skin_panels":[{"id":"","x":0,"y":0,"visible":true},{"id":"main","x":1,"y":2,"visible":false}]}"#,
        );
        assert_eq!(
            one.skin_panels,
            vec![SkinPanelSettings {
                id: "main".to_owned(),
                x: 1,
                y: 2,
                visible: false,
                w: None,
                h: None,
            }]
        );
    }
    #[test]
    fn a_panel_size_outside_its_range_loads_as_none() {
        let s = load_text(
            "skin-panel-size",
            br#"{"version":1,"skin_panels":[{"id":"a","x":0,"y":0,"visible":true,"w":0,"h":5000},{"id":"b","x":0,"y":0,"visible":true,"w":325,"h":174},{"id":"c","x":0,"y":0,"visible":true,"w":4096}]}"#,
        );
        let sizes: Vec<(Option<u32>, Option<u32>)> =
            s.skin_panels.iter().map(|p| (p.w, p.h)).collect();
        assert_eq!(
            sizes,
            [(None, None), (Some(325), Some(174)), (Some(4096), None)]
        );
    }

    #[test]
    fn a_file_from_before_the_local_provider_loads_unchanged_and_fills_the_new_fields() {
        let old = br#"{"version":1,"volume":30,"theme":"b","front_end":"skin","skin":"a.skin","eq":{"enabled":false}}"#;
        let loaded = load_text("pre-local", old);
        assert_eq!(loaded.volume, 30);
        assert_eq!(loaded.theme, "b");
        assert_eq!(loaded.front_end, "skin");
        assert_eq!(loaded.skin.as_deref(), Some("a.skin"));
        assert!(!loaded.eq.enabled);
        assert_eq!(loaded.music_folders, None);
        assert_eq!(loaded.library_provider, None);
        assert_eq!(loaded.replay_gain, REPLAY_GAIN_OFF);
        assert_eq!(loaded.replay_gain_mode(), ReplayGainMode::Off);
        assert!(!loaded.tour_seen, "an older file has not seen the tour");
    }

    #[test]
    fn tour_seen_defaults_off_and_a_saved_true_stays_true() {
        assert!(!Settings::default().tour_seen);
        assert!(!load_text("tour-off", br#"{"version":1}"#).tour_seen);
        assert!(load_text("tour-on", br#"{"version":1,"tour_seen":true}"#).tour_seen);
    }

    #[test]
    fn connect_settings_default_on_and_an_older_file_loads_with_the_defaults() {
        let defaults = Settings::default();
        assert!(defaults.spotify_connect);
        assert_eq!(defaults.connect_device_id, None);
        for old in [
            &br#"{"version":1}"#[..],
            br#"{"version":1,"volume":30,"tour_seen":true}"#,
        ] {
            let loaded = load_text("pre-connect", old);
            assert!(loaded.spotify_connect);
            assert_eq!(loaded.connect_device_id, None);
        }
        let off = load_text("connect-off", br#"{"version":1,"spotify_connect":false}"#);
        assert!(!off.spotify_connect);
    }

    #[test]
    fn a_device_id_is_forty_lowercase_hex_and_survives_save_and_load() {
        let mut settings = Settings::default();
        let id = settings.ensure_connect_device_id().unwrap().to_owned();
        assert!(is_device_id(&id), "{id}");
        assert_eq!(settings.ensure_connect_device_id(), Some(id.as_str()));
        let scratch = Scratch::new("device-id");
        let paths = AppPaths::under(&scratch.0);
        settings.save(&paths).unwrap();
        let (mut loaded, _) = Settings::load(&paths);
        assert_eq!(loaded.connect_device_id.as_deref(), Some(id.as_str()));
        assert_eq!(loaded.ensure_connect_device_id(), Some(id.as_str()));
        // Each new id differs.
        assert_ne!(new_device_id().unwrap(), new_device_id().unwrap());
        assert_eq!(device_id_from(&[0xab; 20]), "ab".repeat(20));
        assert_eq!(device_id_from(&[0x0f; 20]), "0f".repeat(20));
    }

    #[test]
    fn a_malformed_device_id_loads_as_none() {
        for bad in [
            "",
            "abc",
            &"A".repeat(40),
            &"g".repeat(40),
            &"a".repeat(41),
            &"a".repeat(39),
        ] {
            let text = format!(r#"{{"version":1,"connect_device_id":"{bad}"}}"#);
            let loaded = load_text("bad-device-id", text.as_bytes());
            assert_eq!(loaded.connect_device_id, None, "{bad}");
        }
        let good = "0123456789abcdef0123456789abcdef01234567";
        let text = format!(r#"{{"version":1,"connect_device_id":"{good}"}}"#);
        let loaded = load_text("good-device-id", text.as_bytes());
        assert_eq!(loaded.connect_device_id.as_deref(), Some(good));
    }

    #[test]
    fn the_connect_device_name_is_the_host_made_safe_and_capped() {
        assert_eq!(connect_device_name("DESKTOP-1"), "Whipspot on DESKTOP-1");
        assert_eq!(connect_device_name(""), "Whipspot");
        assert_eq!(connect_device_name("  \t\n "), "Whipspot");
        let name = connect_device_name(&"h".repeat(200));
        assert_eq!(name.chars().count(), MAX_DEVICE_NAME);
        assert!(name.starts_with("Whipspot on hhh"));
        let name = connect_device_name("my\u{0}\tpc\u{202E}x\n");
        assert!(!name.chars().any(char::is_control), "{name:?}");
        assert!(!name.contains('\u{202E}'));
        assert!(name.starts_with("Whipspot on my"));
        assert_eq!(
            connect_device_name(&"\u{e9}".repeat(100)).chars().count(),
            MAX_DEVICE_NAME
        );
    }

    #[test]
    fn replay_gain_round_trips_and_an_unknown_value_loads_off() {
        for (text, want) in [
            ("off", ReplayGainMode::Off),
            ("track", ReplayGainMode::Track),
            ("album", ReplayGainMode::Album),
            ("loud", ReplayGainMode::Off),
            ("", ReplayGainMode::Off),
        ] {
            let json = format!(r#"{{"version":1,"replay_gain":"{text}"}}"#);
            assert_eq!(load_text("rg", json.as_bytes()).replay_gain_mode(), want);
        }
        for mode in [
            ReplayGainMode::Off,
            ReplayGainMode::Track,
            ReplayGainMode::Album,
        ] {
            let mut s = Settings::default();
            s.set_replay_gain_mode(mode);
            assert_eq!(s.replay_gain_mode(), mode);
        }
    }

    #[test]
    fn music_folders_are_cleaned_and_capped_on_load() {
        let many: Vec<String> = (0..70).map(|i| format!("\"/m{i}\"")).collect();
        let text = format!(
            r#"{{"version":1,"music_folders":["", "  ", "/a\u0000b", {}]}}"#,
            many.join(",")
        );
        let loaded = load_text("folders-clean", text.as_bytes());
        let folders = loaded.music_folders.unwrap();
        assert_eq!(folders.len(), MAX_MUSIC_FOLDERS);
        assert_eq!(folders[0], "/m0");
        let provider = load_text("provider-empty", br#"{"version":1,"library_provider":""}"#);
        assert_eq!(provider.library_provider, None);
    }

    #[test]
    fn the_first_start_adds_the_music_folder_once_and_an_empty_saved_list_stays_empty() {
        let scratch = Scratch::new("music-first-start");
        let music = scratch.0.join("Music");
        fs::create_dir_all(&music).unwrap();
        let music_text = music.to_str().unwrap().to_owned();
        let paths = AppPaths::under(&scratch.0);

        // Never set: the OS Music folder, when it exists.
        let first = Settings::default();
        assert_eq!(
            first.start_music_folders(Some(&music)),
            vec![music_text.clone()]
        );
        assert!(
            first
                .start_music_folders(Some(&scratch.0.join("missing")))
                .is_empty()
        );
        assert!(first.start_music_folders(None).is_empty());

        // The session ends with the folder kept: it stays, nothing is added twice.
        let mut kept = Settings {
            music_folders: Some(first.start_music_folders(Some(&music))),
            ..Settings::default()
        };
        kept.save(&paths).unwrap();
        let (loaded, _) = Settings::load(&paths);
        assert_eq!(loaded.start_music_folders(Some(&music)), vec![music_text]);

        // The user removed it: the empty list is saved and stays empty on the next start.
        kept.music_folders = Some(Vec::new());
        kept.save(&paths).unwrap();
        let (loaded, _) = Settings::load(&paths);
        assert_eq!(loaded.music_folders, Some(Vec::new()));
        assert!(loaded.start_music_folders(Some(&music)).is_empty());
    }

    // ---- property tests (H13, D3): seeded random settings; failures print the seed ----

    use whipspot_testkit::rng::{SplitMix64, case_rng, mutate};

    /// Cases per property: 500 unless `WHIPSPOT_FUZZ_ITERS` says otherwise.
    fn cases() -> u64 {
        std::env::var("WHIPSPOT_FUZZ_ITERS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(500)
    }

    fn base_seed(salt: u64) -> u64 {
        let extra: u64 = std::env::var("WHIPSPOT_FUZZ_SEED")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);
        salt ^ extra
    }

    fn random_text(rng: &mut SplitMix64) -> String {
        const PIECES: [&str; 14] = [
            "a", "Z", "x", "0", " ", "-", "_", ".", "/", "\"", "\\", "\n", "\u{e9}", "\u{65e5}",
        ];
        (0..rng.below(12)).map(|_| *rng.pick(&PIECES)).collect()
    }

    /// A finite value, mostly within `±span` and sometimes far outside it.
    fn random_f32(rng: &mut SplitMix64, span: f32) -> f32 {
        let unit = rng.below(2_001) as f32 / 1_000.0 - 1.0;
        let wide = if rng.one_in(5) { 40.0 } else { 1.0 };
        unit * span * wide
    }

    fn random_panel_id(rng: &mut SplitMix64) -> String {
        const OK: &[u8] = b"abcxyz0189._-";
        const ANY: [&str; 6] = ["Main", "bad id", "", "-lead", "p/q", "\u{e9}"];
        if rng.one_in(4) {
            return (*rng.pick(&ANY)).to_owned();
        }
        let len = 1 + rng.below(33);
        let mut id = String::new();
        for i in 0..len {
            let c = *rng.pick(OK) as char;
            // The first character may not be punctuation.
            id.push(if i == 0 && !c.is_ascii_alphanumeric() {
                'a'
            } else {
                c
            });
        }
        id
    }

    /// Settings whose values may lie outside their ranges or use unknown keys, but are all
    /// representable in JSON (finite numbers).
    fn random_settings(rng: &mut SplitMix64) -> Settings {
        let maybe = |rng: &mut SplitMix64| rng.one_in(2);
        let band_count = rng.below(15);
        Settings {
            version: 1,
            volume: rng.below(256) as u8,
            balance: rng.next_u64() as i8,
            last_view: random_text(rng),
            output_device: maybe(rng).then(|| random_text(rng)),
            output_mode: ["shared", "exclusive", "", "bogus"][rng.below(4)].to_owned(),
            lyrics: maybe(rng),
            efficiency_mode: maybe(rng),
            theme: ["a", "b", "c", "z", ""][rng.below(5)].to_owned(),
            stream_quality: ["high", "normal", "low", "ultra"][rng.below(4)].to_owned(),
            auto_hide: maybe(rng),
            eq: EqSettings {
                enabled: maybe(rng),
                preamp_db: random_f32(rng, 12.0),
                bands_db: (0..band_count).map(|_| random_f32(rng, 12.0)).collect(),
            },
            window: WindowSettings {
                width: rng.below(40_000) as f64 / 3.0,
                height: rng.below(40_000) as f64 / 3.0,
                maximized: maybe(rng),
                x: maybe(rng).then(|| rng.next_u64() as i32),
                y: maybe(rng).then(|| rng.next_u64() as i32),
            },
            front_end: ["native", "skin", "x", ""][rng.below(4)].to_owned(),
            skin: maybe(rng).then(|| random_text(rng)),
            skin_on_top: maybe(rng),
            skin_panels: (0..rng.below(20))
                .map(|_| SkinPanelSettings {
                    id: random_panel_id(rng),
                    x: rng.next_u64() as i32,
                    y: rng.next_u64() as i32,
                    visible: maybe(rng),
                    w: maybe(rng).then(|| rng.next_u64() as u32),
                    h: maybe(rng).then(|| rng.below(9000) as u32),
                })
                .collect(),
            music_folders: maybe(rng).then(|| {
                (0..rng.below(70))
                    .map(|_| random_text(rng))
                    .collect::<Vec<_>>()
            }),
            library_provider: maybe(rng).then(|| random_text(rng)),
            replay_gain: ["off", "track", "album", "", "loud"][rng.below(5)].to_owned(),
            tour_seen: maybe(rng),
            spotify_connect: maybe(rng),
            connect_device_id: maybe(rng).then(|| {
                if maybe(rng) {
                    "0123456789abcdef0123456789abcdef01234567".to_owned()
                } else {
                    random_text(rng)
                }
            }),
        }
    }

    fn normalised(mut s: Settings) -> Settings {
        s.normalise();
        s
    }

    /// What a loaded value must satisfy: inside every range the normaliser enforces.
    fn assert_in_range(s: &Settings) -> Result<(), String> {
        let bad = |what: &str| Err(format!("{what} out of range in {s:?}"));
        if s.volume > MAX_VOLUME {
            return bad("volume");
        }
        if s.balance.unsigned_abs() > MAX_BALANCE as u8 {
            return bad("balance");
        }
        if !(MIN_WIDTH..=MAX_WIDTH).contains(&s.window.width)
            || !(MIN_HEIGHT..=MAX_HEIGHT).contains(&s.window.height)
        {
            return bad("window size");
        }
        if s.eq.bands_db.len() != BAND_COUNT
            || !s
                .eq
                .bands_db
                .iter()
                .chain([&s.eq.preamp_db])
                .all(|g| g.is_finite() && g.abs() <= EQ_MAX_DB)
        {
            return bad("eq gain");
        }
        if s.skin_panels.len() > MAX_SKIN_PANELS
            || !s.skin_panels.iter().all(|p| PanelId::new(&p.id).is_some())
        {
            return bad("skin panels");
        }
        if !s
            .skin_panels
            .iter()
            .flat_map(|p| [p.w, p.h])
            .flatten()
            .all(|v| (1..=MAX_PANEL_SIDE).contains(&v))
        {
            return bad("skin panel size");
        }
        if s.output_mode != OUTPUT_MODE_SHARED && s.output_mode != OUTPUT_MODE_EXCLUSIVE {
            return bad("output_mode");
        }
        if s.front_end != FRONT_END_NATIVE && s.front_end != FRONT_END_SKIN {
            return bad("front_end");
        }
        if ThemeId::from_key(&s.theme).is_none() {
            return bad("theme");
        }
        if s.music_folders.as_ref().is_some_and(|f| {
            f.len() > MAX_MUSIC_FOLDERS
                || f.iter()
                    .any(|p| p.trim().is_empty() || p.chars().any(char::is_control))
        }) {
            return bad("music folders");
        }
        if s.library_provider
            .as_ref()
            .is_some_and(|id| id.is_empty() || id.len() > MAX_PROVIDER_ID)
        {
            return bad("library provider");
        }
        if ![REPLAY_GAIN_OFF, REPLAY_GAIN_TRACK, REPLAY_GAIN_ALBUM]
            .contains(&s.replay_gain.as_str())
        {
            return bad("replay gain");
        }
        Ok(())
    }

    #[test]
    fn random_settings_round_trip_through_save_and_load() {
        let seed = base_seed(0x5E77_1465);
        let scratch = Scratch::new("prop-round-trip");
        let paths = AppPaths::under(&scratch.0);
        for case in 0..cases() {
            let mut rng = case_rng(seed, case);
            let input = random_settings(&mut rng);
            input.save(&paths).unwrap();
            let (loaded, notice) = Settings::load(&paths);
            let want = normalised(input.clone());
            assert!(
                notice.is_none() && loaded == want,
                "round trip failed: seed={seed} case={case}\ninput:  {input:?}\nwanted: {want:?}\n\
                 loaded: {loaded:?} ({notice:?})"
            );
        }
    }

    #[test]
    fn normalise_is_idempotent_and_lands_in_range_even_for_non_finite_gains() {
        let seed = base_seed(0x001D_E490);
        for case in 0..cases() {
            let mut rng = case_rng(seed, case);
            let mut input = random_settings(&mut rng);
            // Non-finite gains cannot be saved (JSON has none) but can reach `normalise`.
            for gain in input
                .eq
                .bands_db
                .iter_mut()
                .chain([&mut input.eq.preamp_db])
            {
                if rng.one_in(6) {
                    *gain = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY][rng.below(3)];
                }
            }
            let once = normalised(input.clone());
            let twice = normalised(once.clone());
            assert!(
                once == twice,
                "normalise is not idempotent: seed={seed} case={case}\ninput: {input:?}\n\
                 once:  {once:?}\ntwice: {twice:?}"
            );
            if let Err(e) = assert_in_range(&once) {
                panic!("normalise left a value out of range: seed={seed} case={case}: {e}");
            }
        }
    }

    #[test]
    fn mutated_settings_files_load_without_a_panic_and_keep_the_bak_rule() {
        let seed = base_seed(0x0B17_F11E);
        let scratch = Scratch::new("prop-mutate");
        let paths = AppPaths::under(&scratch.0);
        fs::create_dir_all(&paths.config_dir).unwrap();
        let backup = paths.config_dir.join("settings.json.bak");
        let dict: [&[u8]; 6] = [
            b"null",
            b"\"audio_output\"",
            b"1e999",
            b"-1",
            b"[",
            b"\"version\":2",
        ];
        for case in 0..cases() {
            let mut rng = case_rng(seed, case);
            let original = random_settings(&mut rng);
            let mut bytes = serde_json::to_vec_pretty(&original).unwrap();
            mutate(&mut bytes, &mut rng, &dict);
            fs::write(paths.settings_file(), &bytes).unwrap();
            let _ = fs::remove_file(&backup);
            let (loaded, notice) = Settings::load(&paths);
            let context = |what: &str| {
                format!(
                    "{what}: seed={seed} case={case}\nfile: {}\nloaded: {loaded:?}",
                    String::from_utf8_lossy(&bytes)
                )
            };
            match notice {
                // Unusable: the defaults, and the unusable bytes kept in `.bak`.
                Some(LoadNotice::Reset) => {
                    assert!(loaded == Settings::default(), "{}", context("not defaults"));
                    assert!(
                        fs::read(&backup).is_ok_and(|b| b == bytes),
                        "{}",
                        context(".bak is not the original bytes")
                    );
                }
                // Usable: every value in range, the file untouched, no backup written.
                None => {
                    if let Err(e) = assert_in_range(&loaded) {
                        panic!("{}", context(&e));
                    }
                    assert!(!backup.exists(), "{}", context("a .bak for a usable file"));
                    assert!(
                        fs::read(paths.settings_file()).unwrap() == bytes,
                        "{}",
                        context("load changed the file")
                    );
                }
            }
        }
    }
}

//! The composition root (D31): the one place that builds the credential store, the Spotify
//! provider, the provider registry and the player, and hands the player to the window host.
//!
//! Hard rule 2: nothing but this binary touches the real credential store; the demo example
//! runs on fakes.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use whipspot_app::host::{self, HostOptions, WindowSpec};
use whipspot_app::logging;
use whipspot_app::paths::{AppPaths, os_music_dir};
use whipspot_app::power::{self, OsPower};
use whipspot_app::selftest;
use whipspot_app::settings::{
    EqSettings, FRONT_END_NATIVE, FRONT_END_SKIN, OUTPUT_MODE_EXCLUSIVE, OUTPUT_MODE_SHARED,
    Settings, SkinPanelSettings, connect_device_name,
};
use whipspot_audio_out::OutputRouter;
use whipspot_core::art::ArtRouter;
use whipspot_core::engine::{OutputMode, OutputRequest};
use whipspot_core::folders::MusicFolders;
use whipspot_core::player::{self, EngineSetup, Player, PlayerAction, PlayerConfig, ProviderEntry};
use whipspot_core::provider::ProviderRegistry;
use whipspot_core::provider::model::LoginState;
use whipspot_core::skin::{SkinRegistry, SkinSource};
use whipspot_credentials::OsCredentialStore;
use whipspot_local::{ART_SCHEME, LocalConfig, LocalProvider};
use whipspot_net::{Lrclib, NetArt};
use whipspot_skin_fs::DirSkinSource;
use whipspot_skin_winamp::WinampEngine;
use whipspot_skin_wmp::WmpEngine;
use whipspot_spotify::connect::{self, ConnectOptions};
use whipspot_spotify::player_host::EVENT_CAPACITY;
use whipspot_spotify::provider::{ID as SPOTIFY_ID, STEP_CONNECT};
use whipspot_spotify::{Quality, SpotifyOutput, SpotifyProvider, trace};
use whipspot_ui::skin::{SavedLayout, SavedPanel, SkinLibrary, SkinPrefs, SkinReport, SkinUi};
use whipspot_ui::theme::ThemeId;
use whipspot_ui::tray::TrayMenu;
use whipspot_ui::{DiagnosticsUi, NativeUi, ProviderChoice, StreamQuality, Tab, UiPrefs};
use whipspot_update::{LunepackUpdater, Start};
use whipspot_ytmusic::http::ReqwestHttp;
use whipspot_ytmusic::run::ProcessRunner;
use whipspot_ytmusic::tools::ToolEnv;
use whipspot_ytmusic::{YtDeps, YtMusicProvider};

/// The smallest window the layout is made for, logical px.
const MIN_SIZE: (f64, f64) = (760.0, 480.0);
/// The diagnostics window's size and smallest size, logical px (D40).
const DIAGNOSTICS_SIZE: (f64, f64) = (560.0, 720.0);
const DIAGNOSTICS_MIN_SIZE: (f64, f64) = (440.0, 480.0);
/// How long the runtime may take to wind down after the window closed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
/// How long the stream helper's daily update waits after the window starts opening.
const STARTUP_UPDATE_DELAY: Duration = Duration::from_secs(5);
/// The user-run flag that prints the dealer URIs a Connect session sees, redacted (D16).
const CONNECT_TRACE_FLAG: &str = "--connect-trace";
/// The user-run flag that prints each skin panel drag event's numbers to stderr (debug builds).
const SKIN_TRACE_FLAG: &str = "--skin-trace";
fn main() -> ExitCode {
    // First, before any window, GPU or audio library loads a DLL (M2-P1, D5).
    #[cfg(windows)]
    let dll_search_restricted = whipspot_app::platform_windows::restrict_dll_search();
    // The update lifecycle's start (D10): an installed copy that was rolled back hands over to the
    // previous version and exits; a copy that is not installed goes on. Before the arguments, so
    // every start of an installed copy is counted. Logged below, once the log is up.
    let (start, start_note) = whipspot_update::lifecycle_startup();
    if start == Start::Exit {
        return ExitCode::SUCCESS;
    }
    // `--self-test` (H21) returns before the settings, the credential store or any provider
    // exists: it only plays a tone through the output and prints (hard rule 2).
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--self-test") {
        return selftest::main_with(&args[1..]);
    }
    // `--connect-trace` (D16): the Connect session prints its redacted dealer URIs to stderr.
    // Without it nothing is subscribed or printed.
    if args.iter().any(|a| a == CONNECT_TRACE_FLAG) {
        trace::enable();
    }
    // `--skin-trace` (D3 of ux-fixes-1118): a skin drag's pointer, frames, scale and moves to
    // stderr, numbers only. A release build ignores it.
    if args.iter().any(|a| a == SKIN_TRACE_FLAG) {
        whipspot_ui::skin::trace::enable();
    }
    // No directory root known: no settings file and no log, the app still runs.
    let paths = AppPaths::from_os();
    logging::init(paths.as_ref());
    log::info!("{}", start_note.text());
    #[cfg(windows)]
    if !dll_search_restricted {
        log::warn!("could not restrict the DLL search path");
    }
    match run(paths.as_ref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => {
            // Fixed texts only (M2-F2): never a library or OS error text.
            log::error!("the app stopped");
            eprintln!("Whipspot: the app stopped");
            ExitCode::FAILURE
        }
    }
}

/// The first frame is up and the player is built (D9): tells the update lifecycle this version
/// works, off the UI thread (the call may wait for the install lock). Without it an installed
/// version that was just updated is rolled back after three starts.
fn confirm_update_health() {
    let spawned = std::thread::Builder::new()
        .name("whipspot-update-confirm".into())
        .spawn(|| log::info!("{}", whipspot_update::confirm_healthy().text()));
    if spawned.is_err() {
        log::warn!("could not start the update confirmation");
    }
}

/// The settings string for the mode the user chose.
fn mode_setting(mode: OutputMode) -> &'static str {
    match mode {
        OutputMode::Shared => OUTPUT_MODE_SHARED,
        OutputMode::Exclusive => OUTPUT_MODE_EXCLUSIVE,
    }
}

/// The Spotify bitrate cap for the user's streaming quality (MD88). The only place the two
/// enums meet.
fn spotify_quality(quality: StreamQuality) -> Quality {
    match quality {
        StreamQuality::High => Quality::Kbps320,
        StreamQuality::Normal => Quality::Kbps160,
        StreamQuality::Low => Quality::Kbps96,
    }
}

/// The registry of skin engines and, when a config directory is known, the skins folder source.
struct SkinSetup {
    registry: Arc<SkinRegistry>,
    source: Option<Arc<dyn SkinSource>>,
}

fn skin_setup(paths: Option<&AppPaths>) -> anyhow::Result<SkinSetup> {
    let mut registry = SkinRegistry::new();
    registry
        .register(Arc::new(WinampEngine))
        .map_err(|_| anyhow!("could not register the skin engine"))?;
    registry
        .register(Arc::new(WmpEngine))
        .map_err(|_| anyhow!("could not register the skin engine"))?;
    let source = paths.map(|paths| {
        let source = DirSkinSource::new(paths.skins_dir(), registry.extensions());
        if source.ensure().is_err() {
            log::warn!("could not create the skins folder");
        }
        log::info!("skins folder: {} skin file(s) listed", source.list().len());
        Arc::new(source) as Arc<dyn SkinSource>
    });
    Ok(SkinSetup {
        registry: Arc::new(registry),
        source,
    })
}

/// The skin front-end's start: always on top, and the saved places of the saved skin (D11).
fn skin_prefs(settings: &Settings) -> SkinPrefs {
    SkinPrefs {
        on_top: settings.skin_on_top,
        saved: settings.skin.clone().map(|skin| SavedLayout {
            skin,
            panels: settings
                .skin_panels
                .iter()
                .map(|p| SavedPanel {
                    id: p.id.clone(),
                    x: p.x,
                    y: p.y,
                    visible: p.visible,
                    size: p.w.zip(p.h),
                })
                .collect(),
        }),
    }
}

/// Writes what the skin front-end reported into the settings: the front-end, the skin, always
/// on top and the panels' places (kept from the file when none were read for the same skin).
fn save_skin(settings: &mut Settings, report: SkinReport) {
    settings.skin_on_top = report.on_top;
    let Some(name) = report.skin else {
        settings.front_end = FRONT_END_NATIVE.to_owned();
        return;
    };
    settings.front_end = FRONT_END_SKIN.to_owned();
    if !report.panels.is_empty() || settings.skin.as_deref() != Some(name.as_str()) {
        settings.skin_panels = report
            .panels
            .into_iter()
            .map(|p| SkinPanelSettings {
                id: p.id,
                x: p.x,
                y: p.y,
                visible: p.visible,
                w: p.size.map(|s| s.0),
                h: p.size.map(|s| s.1),
            })
            .collect();
    }
    settings.skin = Some(name);
}

fn run(paths: Option<&AppPaths>) -> anyhow::Result<()> {
    let (mut settings, notice) = match paths {
        Some(paths) => Settings::load(paths),
        None => (Settings::default(), None),
    };

    // Each worker applies the efficiency mode's thread mark to itself when it unparks (I5); no
    // audio work runs on these workers.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .on_thread_unpark(power::sync_current_thread)
        .build()
        .context("tokio runtime")?;
    let _enter = rt.enter();

    // The core engine plays on the output the saved mode names (shared by default; exclusive per
    // MD46). This is the only place the output adapter is built.
    let request = OutputRequest {
        device: settings.output_device.clone(),
        mode: if settings.output_mode == OUTPUT_MODE_EXCLUSIVE {
            OutputMode::Exclusive
        } else {
            OutputMode::Shared
        },
        ..Default::default()
    };
    let setup = EngineSetup::Output {
        output: Box::new(OutputRouter::new()),
        request,
    };
    let (ctx, wiring) = player::wiring(setup, EVENT_CAPACITY);
    let quality = StreamQuality::from_key(&settings.stream_quality);
    let mut registry = ProviderRegistry::new();
    // The Local provider (D1), registered first so it leads the library switch: the user's music
    // folders, the index under the cache root and the favourites under the config root (user
    // data). With no directory root there is nowhere to keep either, so it is not registered.
    // Nothing is logged about the folders.
    let mut local_folders: Option<Arc<dyn MusicFolders>> = None;
    let mut local_art = None;
    let start_folders = settings.start_music_folders(os_music_dir().as_deref());
    if let Some(paths) = paths {
        let local = LocalProvider::new(
            LocalConfig {
                folders: start_folders.clone(),
                cache_dir: paths.cache_dir.clone(),
                data_dir: paths.config_dir.clone(),
            },
            ctx.clone(),
        );
        local_folders = Some(local.folders());
        local_art = Some(local.art());
        registry
            .register(Arc::new(local))
            .map_err(|_| anyhow!("could not register the Local provider"))?;
    }
    // Each provider writes through its own source handle, so a late stop from one never
    // touches the next one's audio (D13). The Spotify Connect speaker (D1, D3) is a second
    // stack of the same provider writing as its own owner; it needs the stable device id, so
    // with no randomness for one it is left out for this run. Both spawn inside the runtime.
    let mut spotify = SpotifyProvider::new_with_output(
        Arc::new(OsCredentialStore),
        ctx.for_owner(SPOTIFY_ID),
        SpotifyOutput::Core,
    )
    .with_quality(spotify_quality(quality))
    .with_library_watch();
    if let Some(device_id) = settings.ensure_connect_device_id().map(str::to_owned) {
        let host = gethostname::gethostname();
        spotify = spotify.with_connect(ConnectOptions {
            enabled: settings.spotify_connect,
            name: connect_device_name(&host.to_string_lossy()),
            device_id,
            volume: settings.volume,
            context: ctx.for_owner(connect::OWNER),
        });
    }
    let spotify = Arc::new(spotify);
    registry
        .register(spotify.clone())
        .map_err(|_| anyhow!("could not register the Spotify provider"))?;
    // The YouTube Music provider (D1 of the youtube-music task): the stream helper's files live
    // in `~/.whipspot/tools`, so with no directory root it is not registered. The daily tool
    // update runs on its own thread once the window is opening (never before the first frame).
    let mut ytmusic = None;
    if let (Some(paths), Ok(http)) = (paths, ReqwestHttp::new()) {
        let context = ctx.for_owner(whipspot_ytmusic::PROVIDER_ID);
        let provider = Arc::new(YtMusicProvider::new(YtDeps {
            http: Arc::new(http),
            runner: Arc::new(ProcessRunner::new()),
            env: ToolEnv::from_process(paths.tools_dir()),
            store: Arc::new(OsCredentialStore),
            source: context.source,
            events: context.events,
        }));
        registry
            .register(provider.clone())
            .map_err(|_| anyhow!("could not register the YouTube Music provider"))?;
        ytmusic = Some(provider);
    }
    // The network adapter (D47, MD81) is built here only: art from the hosts the providers
    // declare, cached under the cache root (MD80), and LRCLIB lyrics. With no directory root
    // there is no cache folder, so no art is fetched; lyrics need no folder.
    let hosts = registry.image_hosts();
    let mut config = PlayerConfig::default()
        .with_lyrics(Arc::new(Lrclib::new()), settings.lyrics)
        .with_power(Arc::new(OsPower::new()), settings.efficiency_mode);
    if let Some(paths) = paths {
        // Art follows the scheme of the ref: `local-art:` to the Local provider, the rest to the
        // network adapter (D9).
        let mut art =
            ArtRouter::new().with_default(Arc::new(NetArt::new(paths.cache_dir.clone(), &hosts)));
        if let Some(local_art) = local_art {
            art.register(ART_SCHEME, local_art)
                .map_err(|_| anyhow!("could not register the Local art source"))?;
        }
        config = config.with_art(Arc::new(art));
    }
    // The updater (D4): only for a copy the installer put on the machine; any other copy leaves
    // the status at Unavailable and nothing runs. The app crate's own version is the running one.
    let updater = LunepackUpdater::new(whipspot_app::APP_VERSION);
    if updater.available() {
        config = config.with_updater(Arc::new(updater), settings.update_check);
    }
    // The skin engines and the skins folder (D4, MD100). Built here only: the folder is created
    // if missing and listed once for the count; skins load on the library's worker (D5). No skin
    // name or path is logged.
    let skins = skin_setup(paths)?;
    let library = SkinLibrary::new(skins.source, skins.registry);
    // The library provider the switch chose, shared by the native view and the skinned front-end
    // and saved on exit (D11). With no saved choice the Local provider is shown first (MD174): it
    // needs no login, so a first start has a library to show.
    let first_library = settings.library_provider.clone().or_else(|| {
        local_folders
            .is_some()
            .then(|| whipspot_local::ID.to_owned())
    });
    let provider_choice = ProviderChoice::new(first_library);
    let mut skin_ui = SkinUi::new(library.clone(), skin_prefs(&settings))
        .with_provider_choice(provider_choice.clone());
    if settings.front_end == FRONT_END_SKIN
        && let Some(name) = &settings.skin
    {
        library.choose(name);
    }
    let handle = Player::start(registry, wiring, config, rt.handle());
    handle.dispatch(PlayerAction::SetVolume(settings.volume));
    handle.dispatch(PlayerAction::SetBalance(settings.balance));
    handle.dispatch(PlayerAction::SetReplayGain(settings.replay_gain_mode()));
    handle.dispatch(PlayerAction::SetEqEnabled(settings.eq.enabled));
    handle.dispatch(PlayerAction::SetEqPreamp(settings.eq.preamp_db));
    for (index, &gain_db) in settings.eq.bands_db.iter().enumerate() {
        handle.dispatch(PlayerAction::SetEqBand { index, gain_db });
    }

    let prefs = UiPrefs {
        theme: ThemeId::from_key(&settings.theme).unwrap_or_default(),
        quality,
        auto_hide: settings.auto_hide,
        tour_seen: settings.tour_seen,
    };
    let folder = paths.map(|p| p.skins_dir().display().to_string());
    // The tray icon's menu chooses from the same library; its Skin row needs the skins folder.
    let tray_menu = TrayMenu::new(library.clone(), folder.is_some());
    let mut view = NativeUi::new(host::chrome(), Tab::from_key(&settings.last_view))
        .with_app_version(whipspot_app::APP_VERSION)
        .with_prefs(prefs)
        .with_skins(library, folder)
        .with_provider_choice(provider_choice.clone());
    if let Some(folders) = &local_folders {
        // The folder dialog opens on this (the UI) thread, from the Settings page's Add button.
        view = view.with_music_folders(folders.clone(), host::pick_folder);
    }
    if let Some(notice) = notice {
        view.push_notice(notice.text());
    }
    let spec = WindowSpec {
        key: "main",
        open_at_start: true,
        title: "Whipspot".into(),
        size: (settings.window.width, settings.window.height),
        min_size: MIN_SIZE,
        maximized: settings.window.maximized,
        position: settings.window.x.zip(settings.window.y),
    };
    let opts = HostOptions {
        smoke_frames: None,
        system_fonts: true,
        first_frame: Some(confirm_update_health),
        ..HostOptions::default()
    };
    log::info!("window opening");
    if let Some(provider) = ytmusic {
        // Off the UI thread, a little after the window starts opening; its result is not logged
        // beyond the fixed word.
        let spawned = std::thread::Builder::new()
            .name("whipspot-ytmusic-update".into())
            .spawn(move || {
                std::thread::sleep(STARTUP_UPDATE_DELAY);
                log::info!("stream helper update: {:?}", provider.startup_update());
            });
        if spawned.is_err() {
            log::warn!("could not start the stream helper update");
        }
    }
    // The diagnostics window is parked until Ctrl+Shift+D or the Audio button opens it (MD45).
    let mut diagnostics = DiagnosticsUi::new(host::chrome());
    let diagnostics_spec = WindowSpec {
        key: "diagnostics",
        open_at_start: false,
        title: "Audio — Whipspot".into(),
        size: DIAGNOSTICS_SIZE,
        min_size: DIAGNOSTICS_MIN_SIZE,
        maximized: false,
        position: None,
    };
    let report = host::run(
        &handle,
        vec![(spec, &mut view), (diagnostics_spec, &mut diagnostics)],
        Some(&mut skin_ui),
        Some(tray_menu),
        opts,
    )?;
    log::info!("window closed");

    settings.volume = handle.state().volume;
    settings.balance = handle.state().balance;
    settings.lyrics = handle.state().lyrics_enabled;
    settings.efficiency_mode = handle.state().power.enabled;
    settings.update_check = handle.state().update_check;
    // The Connect step's state, when the Accounts page ever read it (D7); else what was loaded.
    if let Some(on) = step_logged_in(&handle.state().providers, SPOTIFY_ID, STEP_CONNECT) {
        settings.spotify_connect = on;
    }
    settings.set_replay_gain_mode(handle.state().replay_gain);
    // The switch's choice, when one was made (a saved one that was never changed stays).
    if let Some(id) = provider_choice.get() {
        settings.library_provider = Some(id);
    }
    // The folders as the user left them. A list that was never set, started empty (no Music
    // folder to add) and is still empty stays unset, so the Music folder is offered once it
    // exists; a list the user emptied is saved as empty and stays empty (D12).
    if let Some(folders) = &local_folders {
        let list = folders.folders();
        if settings.music_folders.is_some() || !start_folders.is_empty() || !list.is_empty() {
            settings.music_folders = Some(list);
        }
    }
    let eq = handle.state().eq;
    settings.eq = EqSettings {
        enabled: eq.enabled,
        preamp_db: eq.preamp_db,
        bands_db: eq.bands_db.to_vec(),
    };
    settings.theme = view.theme_id().key().to_owned();
    settings.stream_quality = view.stream_quality().key().to_owned();
    settings.auto_hide = view.auto_hide();
    settings.tour_seen = view.tour_seen();
    settings.last_view = view.tab().key().to_owned();
    save_skin(&mut settings, skin_ui.kept_report());
    settings.window.maximized = report.maximized;
    if let Some((x, y)) = report.position {
        settings.window.x = Some(x);
        settings.window.y = Some(y);
    }
    // The user's choice, never the fallback the engine ended up on (MD46).
    settings.output_device = handle.state().output.device.clone();
    settings.output_mode = mode_setting(handle.state().output.mode).to_owned();
    // The host reports the last restored size, so a maximised session keeps it.
    if let Some((width, height)) = report.size {
        settings.window.width = width;
        settings.window.height = height;
    }
    if let Some(paths) = paths
        && settings.save(paths).is_err()
    {
        log::warn!("could not save settings");
    }

    // The Connect device goes inactive in the user's Spotify apps instead of timing out; bounded,
    // so a stuck session never holds the exit.
    rt.block_on(async {
        let _ = tokio::time::timeout(SHUTDOWN_GRACE, spotify.shutdown_connect()).await;
    });
    drop(handle);
    rt.shutdown_timeout(SHUTDOWN_GRACE);
    Ok(())
}

/// Whether `step` of `provider` reads logged in in the providers' last status answer; `None`
/// when the provider, its answer or the step is not there.
fn step_logged_in(providers: &[ProviderEntry], provider: &str, step: &str) -> Option<bool> {
    let auth = providers
        .iter()
        .find(|p| p.info.id == provider)?
        .auth
        .as_ref()?;
    let step = auth.steps.iter().find(|s| s.id == step)?;
    Some(step.state == LoginState::LoggedIn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use whipspot_core::provider::Provider;
    use whipspot_core::provider::model::{AuthStepStatus, ProviderAuthStatus};
    use whipspot_testkit::FakeProvider;

    fn entry(id: &'static str, steps: Option<Vec<(&str, LoginState)>>) -> ProviderEntry {
        ProviderEntry {
            info: FakeProvider::new(id, false).info(),
            auth: steps.map(|steps| ProviderAuthStatus {
                steps: steps
                    .into_iter()
                    .map(|(id, state)| AuthStepStatus {
                        id: id.to_owned(),
                        state,
                        action: None,
                        detail: None,
                    })
                    .collect(),
            }),
            likes_writable: false,
        }
    }

    #[test]
    fn the_connect_step_state_is_read_from_the_provider_statuses() {
        let on = LoginState::LoggedIn;
        let off = LoginState::LoggedOut;
        let providers = [
            entry("local", Some(vec![("connect", on.clone())])),
            entry(
                "spotify",
                Some(vec![("playback", on.clone()), ("connect", off.clone())]),
            ),
        ];
        assert_eq!(
            step_logged_in(&providers, "spotify", "connect"),
            Some(false)
        );
        assert_eq!(
            step_logged_in(&providers, "spotify", "playback"),
            Some(true)
        );
        let providers = [entry("spotify", Some(vec![("connect", on)]))];
        assert_eq!(step_logged_in(&providers, "spotify", "connect"), Some(true));
        // Not asked yet, no such step, no such provider: nothing to save.
        let providers = [
            entry("spotify", None),
            entry("other", Some(vec![("connect", off)])),
        ];
        assert_eq!(step_logged_in(&providers, "spotify", "connect"), None);
        assert_eq!(step_logged_in(&providers, "other", "web"), None);
        assert_eq!(step_logged_in(&providers, "missing", "connect"), None);
        assert_eq!(step_logged_in(&[], "spotify", "connect"), None);
    }
}

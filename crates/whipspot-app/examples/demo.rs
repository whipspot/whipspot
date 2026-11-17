//! The app's window on testkit fakes: no adapter, no network, no audio device and no app data.
//! For agents and the director to look at the UI (D40).
//!
//!   demo                         runs the window until it is closed
//!   demo --smoke                 opens it, draws at least 60 frames, closes it: `smoke ok: <n> frames`
//!   demo --dump <dir>            writes the 84 screens below to `<dir>` (1440x900 logical px unless
//!                                stated, the diagnostics window 560x720; the five skin panels
//!                                at their own size)
//!   demo --dump-skin <dir>       writes only the skin screens of the lists below (15 files)
//!   demo --dump-desktop <dir>    writes the two desktop composites and, beside them, the three
//!                                panels they are made of in the same state (for pixel checks)
//!   demo --diagnostics           prints the audio diagnostics text of the playing demo player (the
//!                                window's "copy as text") and exits 0; no window (H17)
//!   demo --check-golden [--out <dir>]   renders the golden screens offscreen, halves them (2x2 box)
//!                                and compares them with `examples/golden/` (H14), leaving out
//!                                the regions that vary between runs (`masks_of`); a failure
//!                                writes `<name>-actual.png` and `<name>-diff.png` under `--out`
//!                                (default the OS temp dir), never into the repo
//!   demo --bless-golden          rewrites `examples/golden/` after an intended visual change
//!   demo --smoke-skin            opens the demo skin's panel windows on the real desktop (logged
//!                                out, so the host's own timer opens them), runs the panel-windows
//!                                contract and a layout check on them, switches back to the
//!                                native look (its window must draw again), chooses the skin again
//!                                with its windows' opening forced to fail (the native window must
//!                                stay up with the fixed failure status), closes: `smoke-skin ok`
//!   demo --smoke-skin --check-fails   the same with a check that fails: must exit non-zero
//!   demo --smoke-tray            puts the tray icon in the notification area / menu bar (the demo's
//!                                fakes behind its menu), opens the menu by a programmatic click at
//!                                the icon's rect, draws frames, switches to the skins page and back
//!                                by injected picks, closes it and removes the icon: `smoke-tray ok`
//!   demo --smoke-tray --check-fails   the same ending in a forced failure: must exit non-zero
//!   demo --dump-tray <dir>       writes only the three tray screens (below)
//!   demo --dump-tray-scratch <dir>   writes the tray screens only the visual review looks at
//!                                (`menu-long-main`, `menu-long-skins`, `menu-light`, `menu-paused`)
//!   demo --logged-out            (with either of the first two) starts logged out
//!
//! `--smoke` opens the diagnostics window beside the main one (two windows); a plain run opens it
//! with Ctrl+Shift+D or the media control's device icon.
//!
//! Dumped screens (MD60), for each theme t in a, b, c: `t-browse-grid@1x` (Albums),
//! `t-browse-tracks@1x` (an album open), `t-idle@1x`, `t-tooltip@1x` (the quality pill's signal
//! path) and `t-progress-hover@1x` (the ring's tooltip over the idle view); then in theme c:
//! `c-browse-grid@2x`, `c-home@1x`, `c-search@1x` (results for "a"), `c-account@1x` (logged out),
//! `c-empty@1x` (logged out, Home), `c-refused@1x` (the unreadable playlist after its refusal),
//! `c-min@1x` (760x480, Albums); and `diagnostics@1x`. Eight screens show covers, lyrics and likes
//! (M6): `a-idle-art@1x`, `b-idle-art@1x`, `c-idle-art@1x` (the idle view with its cover),
//! `a-idle-lyrics@1x`, `b-idle-lyrics@1x`, `c-idle-lyrics@1x` (cover and synced lyrics, 31.5 s
//! in), `c-idle-art-2@1x` (another cover) and `c-like-readonly@1x` (the Liked route with a login
//! that cannot change likes, the heart hovered). The three `t-idle@1x` screens have no cover and
//! no lyrics; every other screen has covers and no lyrics. A track plays and the spectrum moves
//! in every screen but the logged-out ones. Hover and idle states are forced through
//! `NativeUi::force`; one more unsaved check drives the real idle path (the pointer leaves, time
//! passes) and fails the dump if the view does not go idle.
//!
//! Thirteen more show the panes of M7 (MD92): `a-equalizer@1x`, `b-equalizer@1x`,
//! `c-equalizer@1x` (the Bass preset, balance L 40, the limiter and clip indicators forced lit),
//! `a-queue@1x`, `b-queue@1x`, `c-queue@1x` (12 more tracks queued, shuffle on, a row hovered),
//! `c-queue-drag@1x` (a forced drag preview), `a-settings@1x`, `b-settings@1x`, `c-settings@1x`
//! (the top of the page), `c-settings-full@1x` (1440x2200, the whole page), `c-settings-scrolled@1x`
//! (the page scrolled 200 px: the caption band must stay clear of the cards above the viewport,
//! M9-S7-1) and `a-account@1x` and `b-account@1x` (logged out, as `c-account@1x`).
//!
//! Two more show Home in a tall window (round 3): `c-home-tall@1x` (2000x1076) and `c-home-rows@1x`
//! (1440x1440, two rows of cards in each section).
//!
//! Four more show an idle view on a near-white cover: `a-`/`b-`/`c-idle-art-light@1x` and
//! `c-idle-lyrics-light@1x` (round 2). Eight more show the v1 feedback fixes: `a-`/`b-`/`c-idle-lyrics-1080p@1x` and
//! `c-idle-art-1080p@1x` (the idle group centred at 1920x1080), `c-autohide-on@1x`,
//! `c-autohide-off@1x`, `a-autohide-off@1x` and `b-autohide-off@1x` (the top bar's auto-hide icon
//! lit and struck through). Two more unsaved checks drive the real idle path: a pointer resting
//! inside the window idles the view after a few seconds, and with auto-hide off it never does.
//!
//! Five more show the skin front-end (M9), drawn from a synthetic skin built in code and loaded
//! through the real engine: `skin-main@1x`, `skin-main@2x` (a long title scrolling, the spectrum
//! running), `skin-main-pressed@2x` (play held, shuffle and repeat on, volume 30, balance left),
//! `skin-main-stopped@2x` and `skin-equalizer@2x` (a set curve). Five more show the skin's lists:
//! `skin-playlist@2x` (twelve queued, the third playing, the fifth selected), `skin-library@2x`
//! (the top level), `skin-library-tracks@2x` (an opened playlist), and the empty states
//! `skin-playlist-empty@2x` and `skin-library-logged-out@2x`. Each is the panel's own size times
//! its pixel scale, with the corner its window shape cuts off made transparent. Three more finish
//! the batch of step 7: `skin-desktop@1x` and `skin-desktop@2x` (the visible panels in one surface
//! at their attach-layout places, shapes cut) and `c-settings-skins@1x` (the native Settings page
//! with its Skins card, the demo skin active). Two more show the playlist's own scroll bar and
//! size: `skin-playlist-scrolled@2x` (twelve queued, scrolled four rows with the wheel, the thumb
//! held; the dump measures that the thumb's pixels are the demo sheet's pressed sprite) and
//! `skin-playlist-resized@2x` (opened at 325x174 from a saved layout, two steps each way: more
//! rows in view, the tiles continuous).
//!
//! Seven more show the Local provider's controls (step 7 of the local-provider task):
//! `a-`/`b-`/`c-browse-switch@1x` (Albums, a second provider with a library registered: the
//! provider button between the auto-hide and settings icons, its menu held open) and `a-`/`b-`/`c-settings-folders@1x` (Settings
//! scrolled to the Music folders and Sound cards over a demo folder list; a and b are idle, c is
//! mid-scan with ReplayGain on Track), and `c-browse-switch-min@1x` (760x480: the narrowest bar
//! with the provider button, the tab gap tightened). The folders are a fixed fake, never a real folder.
//!
//! `c-account-helper@1x` shows the Accounts pane with the demo provider's first step carrying an
//! action button ("Update") and a status line, and a second provider registered.
//!
//! Six more show the first-run tour (`Force.tour`, a second provider registered so every card has its
//! control): `a-`/`b-`/`c-tour@1x` (card 1, "Your library", below the provider button), `c-tour-pill@1x`
//! (card 4 above the quality pill), `c-tour-queue@1x` (card 5 above the queue icon) and
//! `c-tour-min@1x` (760x480, card 1). Every other shot runs with the tour off.
//!
//! Three more show the tray icon (step 7 of the skin-playlist-tray task): `tray-menu@1x` (the main
//! page of the menu with a track playing, as `Ui::menu_window` draws it, over a flat dark desktop),
//! `tray-menu-skins@1x` (the skins page, the demo skin current, two skins listed) and
//! `tray-icon@1x` (the icon at 16 px, at 32 px halved to 16 and at 32 px, 1:1 and enlarged 4x, on a
//! light and a dark strip).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use lune_ui::input::{Event, Modifiers, ScrollDelta};
use lune_ui::paint::{Painter, Rect};
use lune_ui::ui::Ui;
use whipspot_app::host::{self, Dumper, HostOptions, PanelCheck, PanelView, WindowSpec};
use whipspot_core::engine::{OutputRequest, ReplayGainMode};
use whipspot_core::folders::{FolderError, MusicFolders, ScanStatus};
use whipspot_core::player::{
    self, EngineSetup, LyricsState, PlayState, Player, PlayerAction, PlayerConfig, PlayerHandle,
    PlayerState, SpectrumFrame,
};
use whipspot_core::provider::{Library, ProviderRegistry};
use whipspot_core::skin::{PanelWindows, SkinEngine, SkinRegistry, SkinSource, WidgetKind};
use whipspot_skin_winamp::WinampEngine;
use whipspot_skin_winamp::demo::demo_skin;
use whipspot_testkit::{
    ClockedOutput, DemoProvider, FakeArt, FakeLyrics, FakeProvider, FakeSkinSource, demo_images,
    demo_lyrics, run_panel_windows_contract,
};
use whipspot_ui::diagnostics;
use whipspot_ui::skin::{
    FrontChange, PanelPlan, SavedLayout, SavedPanel, SkinForce, SkinLibrary, SkinPrefs, SkinStatus,
    SkinUi, pixel_scale,
};
use whipspot_ui::theme::ThemeId;
use whipspot_ui::tray::{TrayMenu, icon_rgba};
use whipspot_ui::{
    DIAGNOSTICS_KEY, DiagnosticsUi, EqMeters, Force, Hover, Mode, NativeUi, QueueDrag, Route, Tab,
    UiPrefs,
};

const SMOKE_FRAMES: u32 = 60;
const DUMP_SIZE: (u32, u32) = (1440, 900);
const MIN_DUMP_SIZE: (u32, u32) = (760, 480);
const DIAGNOSTICS_SIZE: (u32, u32) = (560, 720);
/// The tall dump of the whole settings page.
const SETTINGS_FULL_SIZE: (u32, u32) = (1440, 2200);
/// How far the scrolled Settings shot scrolls the page, logical px.
const SETTINGS_SCROLL: f32 = 200.0;
/// A wide, tall window (Home at the size of the v1 round 3 report).
const TALL_SIZE: (u32, u32) = (2000, 1076);
/// A window tall enough for two rows of cards in each Home section (the demo has 8 of each).
const ROWS_SIZE: (u32, u32) = (1440, 1440);
/// A full-HD window (the idle group's centring).
const FULL_HD_SIZE: (u32, u32) = (1920, 1080);
/// How many more tracks the queue shots enqueue, and the playlist (an index into the first page;
/// the second is the unreadable one) they come from.
const QUEUE_EXTRA: usize = 12;
const QUEUE_PLAYLIST: usize = 2;
/// The Bass preset of the equalizer (MD84), dB per band.
const BASS: [f32; 10] = [6.0, 5.0, 3.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
/// How long a dump waits for each state it needs before it captures anyway.
const STATE_WAIT: Duration = Duration::from_secs(3);
/// A dump frame's real-time pause (answers from the player arrive meanwhile) and its app-time step.
const FRAME_PAUSE: Duration = Duration::from_millis(30);
const FRAME_STEP: Duration = Duration::from_millis(50);
/// Where the lyrics shots seek to: well into the invented lines, between two of them.
const LYRICS_SEEK_MS: u32 = 31_500;
/// How many frames a capture waits for covers and lyrics to arrive before it fails.
const SETTLE_FRAMES: u32 = 200;

struct Args {
    mode: RunMode,
    logged_out: bool,
}

enum RunMode {
    Run,
    Smoke,
    Dump(PathBuf),
    /// Only the skin screens.
    DumpSkin(PathBuf),
    /// The desktop composites and their parts.
    DumpDesktop(PathBuf),
    /// The diagnostics text, headless.
    Diagnostics,
    /// The golden screenshots: compare against `examples/golden/` or, with `bless`, rewrite them.
    /// Failures write their `-actual` and `-diff` PNGs under `out` (default: the OS temp dir).
    Golden {
        bless: bool,
        out: Option<PathBuf>,
    },
    /// The panel windows of the demo skin on the real desktop; `check_fails` swaps the check for
    /// one that must fail the run.
    SmokeSkin {
        check_fails: bool,
    },
    /// The tray icon and its menu on the real desktop; `check_fails` as for `SmokeSkin`.
    SmokeTray {
        check_fails: bool,
    },
    /// The tray menu's pages and the icon.
    DumpTray(PathBuf),
    /// The tray screens only the visual review looks at.
    DumpTrayScratch(PathBuf),
}

fn parse(args: &[String]) -> anyhow::Result<Args> {
    let (mut mode, mut logged_out, mut check_fails) = (RunMode::Run, false, false);
    let mut out: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--smoke" => mode = RunMode::Smoke,
            "--dump" => {
                let dir = it.next().context("--dump needs a directory")?;
                mode = RunMode::Dump(PathBuf::from(dir));
            }
            "--dump-skin" => {
                let dir = it.next().context("--dump-skin needs a directory")?;
                mode = RunMode::DumpSkin(PathBuf::from(dir));
            }
            "--dump-desktop" => {
                let dir = it.next().context("--dump-desktop needs a directory")?;
                mode = RunMode::DumpDesktop(PathBuf::from(dir));
            }
            "--diagnostics" => mode = RunMode::Diagnostics,
            "--check-golden" => {
                mode = RunMode::Golden {
                    bless: false,
                    out: None,
                }
            }
            "--bless-golden" => {
                mode = RunMode::Golden {
                    bless: true,
                    out: None,
                }
            }
            "--out" => {
                let dir = it.next().context("--out needs a directory")?;
                out = Some(PathBuf::from(dir));
            }
            "--smoke-skin" => mode = RunMode::SmokeSkin { check_fails: false },
            "--smoke-tray" => mode = RunMode::SmokeTray { check_fails: false },
            "--dump-tray" => {
                let dir = it.next().context("--dump-tray needs a directory")?;
                mode = RunMode::DumpTray(PathBuf::from(dir));
            }
            "--dump-tray-scratch" => {
                let dir = it.next().context("--dump-tray-scratch needs a directory")?;
                mode = RunMode::DumpTrayScratch(PathBuf::from(dir));
            }
            "--check-fails" => check_fails = true,
            "--logged-out" => logged_out = true,
            _ => bail!(
                "usage: demo [--smoke | --diagnostics | --check-golden [--out <dir>] | --bless-golden | --smoke-skin [--check-fails] | --smoke-tray [--check-fails] | --dump <dir> | --dump-skin <dir> | --dump-desktop <dir> | --dump-tray <dir>] [--logged-out]"
            ),
        }
    }
    if check_fails {
        match &mut mode {
            RunMode::SmokeSkin { check_fails } | RunMode::SmokeTray { check_fails } => {
                *check_fails = true;
            }
            _ => bail!("--check-fails goes with --smoke-skin or --smoke-tray"),
        }
    }
    if let Some(dir) = out {
        match &mut mode {
            RunMode::Golden { out, .. } => *out = Some(dir),
            _ => bail!("--out goes with --check-golden or --bless-golden"),
        }
    }
    Ok(Args { mode, logged_out })
}

/// The demo clock's rate: not the tone's 44.1 kHz, so the dump shows a resampled path.
const DEMO_RATE: u32 = 48_000;

/// What a demo player is set up with.
#[derive(Debug, Clone, Copy)]
struct Setup {
    logged_in: bool,
    /// Covers come from `FakeArt`.
    art: bool,
    /// Every cover is near-white.
    light_art: bool,
    /// Lyrics are on (the source answers for every title of the playing list either way).
    lyrics: bool,
    /// The login cannot change likes.
    read_only_likes: bool,
    /// The playlist (an index into the first page) whose first track plays.
    playlist: usize,
    /// Twelve more tracks are queued and shuffle is on.
    queue: bool,
    /// With `queue`: shuffle stays off (a shuffle order is random, so a golden cannot use it).
    unshuffled: bool,
    /// A second provider with a library is registered after the demo one, so the sidebar's
    /// provider switch shows (D11). Off in every golden and dump shot but the switch's.
    second_provider: bool,
    /// The demo provider's first login step carries an action button and a status line.
    helper_status: bool,
}

impl Setup {
    /// Covers on, lyrics off, likes writable, the first playlist.
    fn new(logged_in: bool) -> Setup {
        Setup {
            logged_in,
            art: true,
            light_art: false,
            lyrics: false,
            read_only_likes: false,
            playlist: 0,
            queue: false,
            unshuffled: false,
            second_provider: false,
            helper_status: false,
        }
    }
}

/// A player on the demo provider, logged in or out. Logged in, the chosen playlist's first track
/// plays (on the synthetic tone, through the engine and a clock instead of a device). Its
/// runtime must outlive it.
fn player(rt: &tokio::runtime::Runtime, setup: Setup) -> anyhow::Result<PlayerHandle> {
    let Setup {
        logged_in,
        art,
        light_art,
        lyrics,
        read_only_likes,
        playlist,
        queue,
        unshuffled,
        second_provider,
        helper_status,
    } = setup;
    let setup = EngineSetup::Output {
        output: Box::new(ClockedOutput::new(DEMO_RATE)),
        request: OutputRequest::default(),
    };
    let (ctx, wiring) = player::wiring(setup, 64);
    let mut demo = DemoProvider::new(ctx, logged_in);
    if read_only_likes {
        demo = demo.read_only_likes();
    }
    if helper_status {
        demo = demo.with_helper_status("Update", "Helper 2026.10.26, runtime 0.18.0");
    }
    let demo = Arc::new(demo);
    let mut registry = ProviderRegistry::new();
    registry
        .register(demo.clone())
        .map_err(|_| anyhow::anyhow!("could not register the demo provider"))?;
    if second_provider {
        // A fake with a library and a name of its own; fakes only, no app data (hard rule 2).
        registry
            .register(Arc::new(
                FakeProvider::new("demofiles", true)
                    .with_name("Local files")
                    .with_glyph(whipspot_core::provider::ProviderGlyph::Files),
            ))
            .map_err(|_| anyhow::anyhow!("could not register the second demo provider"))?;
    }
    // The tracks that will play, read before the player starts so the lyrics source knows them.
    let mut extra = Vec::new();
    let context = if logged_in {
        let first = rt
            .block_on(demo.playlists(0))
            .map_err(|e| anyhow::anyhow!("demo library: {}", e.message))?;
        let playlist = first
            .items
            .get(playlist)
            .context("the demo has no such playlist")?;
        let page = rt
            .block_on(demo.playlist_items(&playlist.id, 0))
            .map_err(|e| anyhow::anyhow!("demo playlist: {}", e.message))?;
        if queue {
            let other = first
                .items
                .get(QUEUE_PLAYLIST)
                .context("the demo has no third playlist")?;
            let more = rt
                .block_on(demo.playlist_items(&other.id, 0))
                .map_err(|e| anyhow::anyhow!("demo playlist: {}", e.message))?;
            extra = more.items.into_iter().take(QUEUE_EXTRA).collect();
        }
        page.items
    } else {
        Vec::new()
    };
    let fake_lyrics = FakeLyrics::new();
    for track in &context {
        fake_lyrics.with(&track.name, demo_lyrics());
    }
    let mut config = PlayerConfig::default().with_lyrics(Arc::new(fake_lyrics), lyrics);
    if art {
        config = config.with_art(Arc::new(if light_art {
            FakeArt::paper()
        } else {
            FakeArt::new()
        }));
    }
    let handle = Player::start(registry, wiring, config, rt.handle());
    handle.dispatch(PlayerAction::RefreshAuth);
    if logged_in {
        handle.dispatch(PlayerAction::PlayItem { context, index: 0 });
        if queue {
            handle.dispatch(PlayerAction::Enqueue(extra));
            handle.dispatch(PlayerAction::SetShuffle(!unshuffled));
        }
    }
    Ok(handle)
}

/// Waits (at most `STATE_WAIT`) until `done` holds; whether it did.
fn wait_for(mut done: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + STATE_WAIT;
    while !done() {
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    true
}

fn is_playing(handle: &PlayerHandle) -> bool {
    handle.state().play_state == PlayState::Playing
}

/// One dumped screen of the main window.
struct Shot {
    name: String,
    theme: ThemeId,
    route: Route,
    /// The view the shot forces (the machine never decides in a dump).
    mode: Mode,
    hover: Option<Hover>,
    /// Logged in: a track plays and the bars move.
    logged_in: bool,
    /// Covers are on (they are, but in the three `t-idle@1x` shots).
    art: bool,
    /// Every cover is near-white (the legibility shots on a light cover).
    light_art: bool,
    /// Lyrics are on.
    lyrics: bool,
    /// The login cannot change likes.
    read_only_likes: bool,
    /// The playlist (index) whose first track plays.
    playlist: usize,
    /// Twelve more tracks are queued and shuffle is on.
    queue: bool,
    /// With `queue`: shuffle stays off (the golden's queue screen).
    unshuffled: bool,
    /// The Bass preset is applied to the equalizer before the capture.
    eq_bass: bool,
    /// The balance set before the capture (0 is the centre).
    balance: i8,
    /// The equalizer's forced limiter and clip indicators.
    eq_meters: Option<EqMeters>,
    /// A forced queue drag.
    queue_drag: Option<QueueDrag>,
    /// The auto-hide switch is on.
    auto_hide: bool,
    /// Seek here once the track plays, before the capture.
    seek_ms: Option<u32>,
    /// Scroll the page this many logical px down before the capture.
    scroll: f32,
    /// A second provider with a library is registered (the provider switch shows).
    second_provider: bool,
    /// The demo provider's first login step has an action button and a status line.
    helper_status: bool,
    /// The first-run tour's card at this list index is forced on.
    tour: Option<usize>,
    /// The Settings page's Music folders card over a fixed demo list: `Some(scanning)`.
    folders: Option<bool>,
    /// ReplayGain is set to this mode before the capture.
    replay: Option<ReplayGainMode>,
    size: (u32, u32),
    scale: f32,
}

impl Shot {
    fn new(name: impl Into<String>, theme: ThemeId, route: Route) -> Shot {
        Shot {
            name: name.into(),
            theme,
            route,
            mode: Mode::Browse,
            hover: None,
            logged_in: true,
            art: true,
            light_art: false,
            lyrics: false,
            read_only_likes: false,
            playlist: 0,
            queue: false,
            unshuffled: false,
            eq_bass: false,
            balance: 0,
            eq_meters: None,
            queue_drag: None,
            auto_hide: true,
            seek_ms: None,
            scroll: 0.0,
            second_provider: false,
            helper_status: false,
            tour: None,
            folders: None,
            replay: None,
            size: DUMP_SIZE,
            scale: 1.0,
        }
    }
}

/// The album the demo's grid shows first, and the playlist its provider refuses.
fn album() -> Route {
    Route::Album {
        id: "demo-album-1".into(),
        name: "Low Tide Sessions".into(),
        artist: "Velmora".into(),
        images: demo_images("demo-album-1"),
        year: Some(1993),
    }
}

fn refused() -> Route {
    Route::Playlist {
        id: "demo-playlist-2".into(),
        name: "Old Demos".into(),
        images: demo_images("demo-playlist-2"),
    }
}

/// The fixed music folders of the settings-folders shots: a list and a scan status that never
/// change (fakes only, no real folder; hard rule 2).
struct DemoFolders {
    list: Vec<String>,
    status: ScanStatus,
}

impl MusicFolders for DemoFolders {
    fn folders(&self) -> Vec<String> {
        self.list.clone()
    }
    fn add(&self, _: String) -> Result<(), FolderError> {
        Err(FolderError::NotAFolder)
    }
    fn remove(&self, _: &str) {}
    fn rescan(&self) {}
    fn status(&self) -> ScanStatus {
        self.status
    }
}

/// The demo folder list: two short paths and one long enough to be cut in the middle.
fn demo_folders(scanning: bool) -> Arc<dyn MusicFolders> {
    Arc::new(DemoFolders {
        list: vec![
            "D:\\Music".to_owned(),
            "E:\\Lossless\\Jazz and Classical\\Remastered Editions".to_owned(),
            "C:\\Users\\listener\\Documents\\Backups\\Old Hard Drive\\Music\\Ripped CDs\\Collection\\Disc Images\\Lossless Archive\\Volume Two"
                .to_owned(),
        ],
        status: ScanStatus {
            scanning,
            files_seen: if scanning { 1342 } else { 3120 },
            tracks: 3120,
        },
    })
}

/// How far the settings-folders shots scroll the page, logical px: Playback's bottom, the Music
/// folders card and the Sound card in view.
const FOLDERS_SCROLL: f32 = 440.0;

/// The 58 screens of the main window, in dump order (`diagnostics@1x` is the 59th).
fn shots() -> Vec<Shot> {
    let mut shots = Vec::new();
    for (t, theme) in [("a", ThemeId::A), ("b", ThemeId::B), ("c", ThemeId::C)] {
        let shot = |what: &str, route| Shot::new(format!("{t}-{what}@1x"), theme, route);
        shots.extend([
            shot("browse-grid", Route::Albums),
            shot("browse-tracks", album()),
            Shot {
                mode: Mode::Idle,
                art: false,
                ..shot("idle", Route::Albums)
            },
            Shot {
                hover: Some(Hover::Pill),
                ..shot("tooltip", Route::Albums)
            },
            Shot {
                mode: Mode::Idle,
                hover: Some(Hover::Ring(0.82)),
                ..shot("progress-hover", Route::Albums)
            },
        ]);
    }
    for (t, theme) in [("a", ThemeId::A), ("b", ThemeId::B), ("c", ThemeId::C)] {
        let idle = |what: &str| Shot {
            mode: Mode::Idle,
            ..Shot::new(format!("{t}-{what}@1x"), theme, Route::Albums)
        };
        shots.extend([
            idle("idle-art"),
            Shot {
                lyrics: true,
                seek_ms: Some(LYRICS_SEEK_MS),
                ..idle("idle-lyrics")
            },
        ]);
    }
    let c = ThemeId::C;
    shots.extend([
        Shot {
            scale: 2.0,
            ..Shot::new("c-browse-grid@2x", c, Route::Albums)
        },
        Shot::new("c-home@1x", c, Route::Home),
        Shot {
            size: TALL_SIZE,
            ..Shot::new("c-home-tall@1x", c, Route::Home)
        },
        Shot {
            size: ROWS_SIZE,
            ..Shot::new("c-home-rows@1x", c, Route::Home)
        },
        Shot::new("c-search@1x", c, Route::Search),
        Shot {
            logged_in: false,
            ..Shot::new("c-account@1x", c, Route::Account)
        },
        Shot {
            logged_in: false,
            ..Shot::new("c-empty@1x", c, Route::Home)
        },
        Shot::new("c-refused@1x", c, refused()),
        Shot {
            size: MIN_DUMP_SIZE,
            ..Shot::new("c-min@1x", c, Route::Albums)
        },
        Shot {
            mode: Mode::Idle,
            playlist: 2,
            ..Shot::new("c-idle-art-2@1x", c, Route::Albums)
        },
        Shot {
            read_only_likes: true,
            hover: Some(Hover::Like),
            ..Shot::new("c-like-readonly@1x", c, Route::Liked)
        },
    ]);
    // The panes of M7 (MD92).
    for (t, theme) in [("a", ThemeId::A), ("b", ThemeId::B), ("c", ThemeId::C)] {
        let shot = |what: &str, route| Shot::new(format!("{t}-{what}@1x"), theme, route);
        shots.extend([
            Shot {
                eq_bass: true,
                balance: -40,
                eq_meters: Some(EqMeters {
                    limiter: true,
                    clip: true,
                }),
                ..shot("equalizer", Route::Equalizer)
            },
            Shot {
                queue: true,
                hover: Some(Hover::QueueRow(2)),
                ..shot("queue", Route::Queue)
            },
            shot("settings", Route::Settings),
        ]);
    }
    shots.extend([
        Shot {
            queue: true,
            queue_drag: Some(QueueDrag { from: 1, to: 5 }),
            ..Shot::new("c-queue-drag@1x", c, Route::Queue)
        },
        Shot {
            size: SETTINGS_FULL_SIZE,
            ..Shot::new("c-settings-full@1x", c, Route::Settings)
        },
        // M9-S7-1: the page scrolled, so the glass cards above the viewport are out of sight; the
        // caption band must match the unscrolled page's.
        Shot {
            scroll: SETTINGS_SCROLL,
            ..Shot::new("c-settings-scrolled@1x", c, Route::Settings)
        },
    ]);
    for (t, theme) in [("a", ThemeId::A), ("b", ThemeId::B)] {
        shots.push(Shot {
            logged_in: false,
            ..Shot::new(format!("{t}-account@1x"), theme, Route::Account)
        });
    }
    // The v1 feedback fixes: the idle group centred at 1920x1080, the auto-hide icon's two states.
    for (t, theme) in [("a", ThemeId::A), ("b", ThemeId::B), ("c", ThemeId::C)] {
        shots.push(Shot {
            mode: Mode::Idle,
            lyrics: true,
            seek_ms: Some(LYRICS_SEEK_MS),
            size: FULL_HD_SIZE,
            ..Shot::new(format!("{t}-idle-lyrics-1080p@1x"), theme, Route::Albums)
        });
    }
    shots.push(Shot {
        mode: Mode::Idle,
        size: FULL_HD_SIZE,
        ..Shot::new("c-idle-art-1080p@1x", c, Route::Albums)
    });
    // Idle legibility on a near-white cover (v1 round 2): every theme with the cover, theme C
    // with synced lyrics.
    for (t, theme) in [("a", ThemeId::A), ("b", ThemeId::B), ("c", ThemeId::C)] {
        shots.push(Shot {
            mode: Mode::Idle,
            light_art: true,
            ..Shot::new(format!("{t}-idle-art-light@1x"), theme, Route::Albums)
        });
    }
    shots.push(Shot {
        mode: Mode::Idle,
        light_art: true,
        lyrics: true,
        seek_ms: Some(LYRICS_SEEK_MS),
        ..Shot::new("c-idle-lyrics-light@1x", c, Route::Albums)
    });
    for (t, theme, auto_hide) in [
        ("c", ThemeId::C, true),
        ("c", ThemeId::C, false),
        ("a", ThemeId::A, false),
        ("b", ThemeId::B, false),
    ] {
        let state = if auto_hide { "on" } else { "off" };
        shots.push(Shot {
            auto_hide,
            ..Shot::new(format!("{t}-autohide-{state}@1x"), theme, Route::Albums)
        });
    }
    // Step 2 of the YouTube Music task: a login step with an action button and a status line.
    shots.push(Shot {
        second_provider: true,
        helper_status: true,
        ..Shot::new("c-account-helper@1x", ThemeId::C, Route::Account)
    });
    // The Local provider's controls: the switch, and the Music folders and ReplayGain cards.
    for (t, theme) in [("a", ThemeId::A), ("b", ThemeId::B), ("c", ThemeId::C)] {
        shots.push(Shot {
            second_provider: true,
            hover: Some(Hover::ProviderMenu),
            ..Shot::new(format!("{t}-browse-switch@1x"), theme, Route::Albums)
        });
    }
    shots.push(Shot {
        second_provider: true,
        size: MIN_DUMP_SIZE,
        ..Shot::new("c-browse-switch-min@1x", ThemeId::C, Route::Albums)
    });
    // The first-run tour: card 1 in each theme (the library button needs a second provider), the
    // sound-quality and queue cards in C, and card 1 in the narrowest window.
    for (t, theme) in [("a", ThemeId::A), ("b", ThemeId::B), ("c", ThemeId::C)] {
        shots.push(Shot {
            second_provider: true,
            tour: Some(0),
            ..Shot::new(format!("{t}-tour@1x"), theme, Route::Albums)
        });
    }
    for (name, card) in [("c-tour-pill@1x", 3), ("c-tour-queue@1x", 4)] {
        shots.push(Shot {
            second_provider: true,
            tour: Some(card),
            ..Shot::new(name, ThemeId::C, Route::Albums)
        });
    }
    shots.push(Shot {
        second_provider: true,
        tour: Some(0),
        size: MIN_DUMP_SIZE,
        ..Shot::new("c-tour-min@1x", ThemeId::C, Route::Albums)
    });
    for (t, theme) in [("a", ThemeId::A), ("b", ThemeId::B), ("c", ThemeId::C)] {
        let c = theme == ThemeId::C;
        shots.push(Shot {
            folders: Some(c),
            replay: c.then_some(ReplayGainMode::Track),
            scroll: FOLDERS_SCROLL,
            ..Shot::new(format!("{t}-settings-folders@1x"), theme, Route::Settings)
        });
    }
    shots
}

/// One headless window driven by a script.
struct Script<'a> {
    dumper: Dumper,
    view: NativeUi,
    handle: &'a PlayerHandle,
}

impl Script<'_> {
    fn step(&mut self, events: Vec<Event>) -> anyhow::Result<()> {
        self.dumper
            .step(&mut self.view, self.handle, events, FRAME_STEP)?;
        std::thread::sleep(FRAME_PAUSE);
        Ok(())
    }

    /// `n` frames with no input; answers from the player arrive between them.
    fn frames(&mut self, n: u32) -> anyhow::Result<()> {
        (0..n).try_for_each(|_| self.step(Vec::new()))
    }

    /// Pauses the track, waits for the pause to show in the state and draws one last frame at
    /// the same app time: the position and the bars stay as they were.
    fn freeze(&mut self) -> anyhow::Result<()> {
        self.handle.dispatch(PlayerAction::Pause);
        wait_for(|| self.handle.state().play_state == PlayState::Paused);
        self.dumper
            .step(&mut self.view, self.handle, Vec::new(), Duration::ZERO)
    }

    /// Draws frames until the covers it drew are in and, when `lyrics`, the current track's
    /// lyrics are found; fails after [`SETTLE_FRAMES`].
    fn settle(&mut self, lyrics: bool) -> anyhow::Result<()> {
        for _ in 0..SETTLE_FRAMES {
            if self.view.art_settled() && (!lyrics || self.lyrics_found()) {
                // One more frame draws what the last registration brought.
                return self.step(Vec::new());
            }
            self.step(Vec::new())?;
        }
        bail!(
            "covers or lyrics did not arrive within {SETTLE_FRAMES} frames (covers settled: {}, lyrics wanted: {lyrics}, found: {})",
            self.view.art_settled(),
            self.lyrics_found()
        )
    }

    /// The lyrics of the current track were looked up (found or not).
    fn lyrics_found(&self) -> bool {
        let state = self.handle.state();
        match (&state.lyrics, &state.current) {
            (LyricsState::Found { uri, .. }, Some(now)) => *uri == now.track.uri,
            _ => false,
        }
    }

    /// Waits for the track to play and the bars to move.
    fn wait_playing(&self) {
        wait_for(|| is_playing(self.handle));
        let mut spectrum = SpectrumFrame::default();
        let active = wait_for(|| {
            self.handle.spectrum(&mut spectrum);
            spectrum.active
        });
        if !active {
            eprintln!("demo: no spectrum within {} s", STATE_WAIT.as_secs());
        }
    }
}

/// A script over `handle` and a fresh view in `shot`'s theme and size, with `force` set.
fn script_for<'a>(
    shot: &Shot,
    handle: &'a PlayerHandle,
    force: Force,
) -> anyhow::Result<Script<'a>> {
    let mut view = NativeUi::new(host::chrome(), Tab::Home).with_prefs(UiPrefs {
        theme: shot.theme,
        auto_hide: shot.auto_hide,
        // Every shot is without the tour but the ones that force a card (Force.tour).
        tour_seen: true,
        ..UiPrefs::default()
    });
    if let Some(scanning) = shot.folders {
        view = view.with_music_folders(demo_folders(scanning), || None);
    }
    view.force(force);
    let dumper = Dumper::new(&mut view, shot.size, shot.scale)?;
    Ok(Script {
        dumper,
        view,
        handle,
    })
}

/// Draws `shot` into `path`. Each shot has its own player and view.
fn capture(rt: &tokio::runtime::Runtime, shot: &Shot, path: &Path) -> anyhow::Result<()> {
    let handle = player(
        rt,
        Setup {
            logged_in: shot.logged_in,
            art: shot.art,
            light_art: shot.light_art,
            lyrics: shot.lyrics,
            read_only_likes: shot.read_only_likes,
            playlist: shot.playlist,
            queue: shot.queue,
            unshuffled: shot.unshuffled,
            second_provider: shot.second_provider,
            helper_status: shot.helper_status,
        },
    )?;
    if let Some(mode) = shot.replay {
        handle.dispatch(PlayerAction::SetReplayGain(mode));
    }
    if shot.balance != 0 {
        handle.dispatch(PlayerAction::SetBalance(shot.balance));
    }
    if shot.eq_bass {
        for (index, gain_db) in BASS.into_iter().enumerate() {
            handle.dispatch(PlayerAction::SetEqBand { index, gain_db });
        }
    }
    let force = Force {
        mode: Some(shot.mode),
        hover: shot.hover,
        eq_meters: shot.eq_meters,
        queue_drag: shot.queue_drag,
        tour: shot.tour,
    };
    let mut script = script_for(shot, &handle, force)?;
    script.view.navigate(shot.route.clone());
    if shot.route == Route::Search {
        script.view.search("a");
    }
    script.frames(12)?;
    if shot.logged_in {
        script.wait_playing();
    }
    script.frames(8)?;
    if shot.scroll > 0.0 {
        let pos = (shot.size.0 as f32 / 2.0, shot.size.1 as f32 / 2.0);
        script.step(vec![Event::Scroll {
            pos,
            delta: ScrollDelta::Pixels {
                x: 0.0,
                y: -shot.scroll,
            },
            modifiers: Modifiers::default(),
        }])?;
        script.frames(12)?;
    }
    if let Some(ms) = shot.seek_ms {
        handle.dispatch(PlayerAction::Seek { position_ms: ms });
    }
    script.settle(shot.lyrics)?;
    if shot.logged_in {
        script.freeze()?;
    }
    script.dumper.save(path)?;
    println!("dumped {}", path.display());
    Ok(())
}

/// The real idle path, unforced: once a track plays, a pointer move keeps the browse view and
/// the pointer leaving, then time passing, must bring the idle view. Nothing is saved.
fn check_idle_path(rt: &tokio::runtime::Runtime) -> anyhow::Result<()> {
    let handle = player(rt, Setup::new(true))?;
    let mut shot = Shot::new("idle-path", ThemeId::C, Route::Albums);
    shot.size = MIN_DUMP_SIZE;
    let mut script = script_for(&shot, &handle, Force::default())?;
    script.frames(4)?;
    script.wait_playing();
    script.step(vec![Event::PointerMoved {
        pos: (400.0, 200.0),
    }])?;
    anyhow::ensure!(
        script.view.mode() == Mode::Browse,
        "the view is not in browse right after a pointer move"
    );
    script.step(vec![Event::PointerLeft])?;
    // Synthetic time well past the leave delay, in steps so the machine sees each one.
    for _ in 0..8 {
        script.dumper.step(
            &mut script.view,
            &handle,
            Vec::new(),
            Duration::from_millis(500),
        )?;
    }
    anyhow::ensure!(
        script.view.mode() == Mode::Idle,
        "the view did not go idle after the pointer left"
    );
    Ok(())
}

/// The real idle path with the pointer resting inside the window: after one move and a few
/// seconds of synthetic time the view must go idle, and with auto-hide off it must not. Nothing is
/// saved.
fn check_idle_rest(rt: &tokio::runtime::Runtime) -> anyhow::Result<()> {
    for auto_hide in [true, false] {
        let handle = player(rt, Setup::new(true))?;
        let mut shot = Shot::new("idle-rest", ThemeId::C, Route::Albums);
        shot.size = MIN_DUMP_SIZE;
        shot.auto_hide = auto_hide;
        let mut script = script_for(&shot, &handle, Force::default())?;
        script.frames(4)?;
        script.wait_playing();
        script.step(vec![Event::PointerMoved {
            pos: (400.0, 200.0),
        }])?;
        anyhow::ensure!(
            script.view.mode() == Mode::Browse,
            "the view is not in browse right after a pointer move"
        );
        // Twelve half-second steps (6 s) with the pointer still and inside.
        for _ in 0..12 {
            script.dumper.step(
                &mut script.view,
                &handle,
                Vec::new(),
                Duration::from_millis(500),
            )?;
        }
        let want = if auto_hide { Mode::Idle } else { Mode::Browse };
        anyhow::ensure!(
            script.view.mode() == want,
            "a resting pointer with auto-hide {auto_hide} left the view in {:?}",
            script.view.mode()
        );
    }
    Ok(())
}

/// Prints the diagnostics text of the playing demo player, headless (H17): the text the
/// diagnostics window copies, from one snapshot. Fails when the text is not the report's shape.
fn print_diagnostics(rt: &tokio::runtime::Runtime) -> anyhow::Result<()> {
    let handle = player(rt, Setup::new(true))?;
    anyhow::ensure!(
        wait_for(|| is_playing(&handle)),
        "the demo track did not start"
    );
    let mut spectrum = SpectrumFrame::default();
    anyhow::ensure!(
        wait_for(|| {
            handle.spectrum(&mut spectrum);
            spectrum.active
        }),
        "the demo tone did not reach the tap"
    );
    let (diag, state) = (handle.diagnostics(), handle.state());
    let text = diagnostics::text(&diag, &state);
    anyhow::ensure!(
        text.starts_with(
            "Audio diagnostics
"
        ),
        "the text does not start with its title"
    );
    let summary = format!(
        "Output summary: {}
",
        state.output_summary
    );
    anyhow::ensure!(
        !state.output_summary.is_empty() && text.contains(&summary),
        "the text lacks the output summary"
    );
    for part in [
        "
Verdict: ",
        "
Source
",
        "
DSP
",
        "
Output
",
        "
Live
",
    ] {
        anyhow::ensure!(text.contains(part), "the text lacks {:?}", part.trim());
    }
    print!("{text}");
    Ok(())
}

/// Draws the diagnostics window at 1x while the tone plays into `path`.
fn capture_diagnostics(rt: &tokio::runtime::Runtime, path: &Path) -> anyhow::Result<()> {
    let handle = player(rt, Setup::new(true))?;
    let mut view = DiagnosticsUi::new(host::chrome());
    let mut dumper = Dumper::new(&mut view, DIAGNOSTICS_SIZE, 1.0)?;
    let mut frames = |n: u32| -> anyhow::Result<()> {
        for _ in 0..n {
            dumper.step(&mut view, &handle, Vec::new(), FRAME_STEP)?;
            std::thread::sleep(FRAME_PAUSE);
        }
        Ok(())
    };
    frames(6)?;
    wait_for(|| is_playing(&handle));
    let mut spectrum = SpectrumFrame::default();
    wait_for(|| {
        handle.spectrum(&mut spectrum);
        spectrum.active
    });
    frames(10)?;
    dumper.save(path)?;
    println!("dumped {}", path.display());
    Ok(())
}

// ---- the skin front-end (M9) ----------------------------------------------------------------

/// How long a skin dump waits for the loader.
const SKIN_LOAD_WAIT: Duration = Duration::from_secs(5);
/// The shortest "artist - title" a skin shot needs for its scroll check, in glyphs (the title
/// rect is 154 px of 5 px glyphs: 30.8).
const LONG_TITLE_GLYPHS: usize = 32;
/// The point (skin px from the title rect's left) where the scrolling shot pins the text's end.
const SCROLL_END_AT: u64 = 100;
/// The equalizer curve of the skin shot: dB per band, and the preamp.
const SKIN_BANDS: [f32; 10] = [-12.0, -6.0, 0.0, 6.0, 12.0, 12.0, 6.0, 0.0, -6.0, -12.0];
const SKIN_PREAMP: f32 = 3.0;
/// The hint a skin shows when it opens from the settings (D9), pinned for the 1x shot.
const SKIN_HINT: &str = "Ctrl+Shift+N: native look";

/// What a skin shot shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SkinState {
    /// Playing; the hint replaces the title.
    Hint,
    /// Playing, the long title scrolling at a pinned point.
    Scrolling,
    /// Playing with the play button held, shuffle and repeat on, volume 30, balance left.
    Pressed,
    /// Stopped, the spectrum at rest.
    Stopped,
    /// The equalizer panel with a set curve.
    Equalizer,
    /// The playlist panel: twelve queued, the third playing, the fifth selected.
    Playlist,
    /// The playlist panel with an empty queue.
    PlaylistEmpty,
    /// The playlist panel scrolled four rows, the scroll thumb held (pressed sprite).
    PlaylistScrolled,
    /// The playlist panel resized two steps each way (325x174), as a saved layout opens it.
    PlaylistResized,
    /// The library panel at its top level.
    Library,
    /// The library panel with a playlist opened.
    LibraryTracks,
    /// The library panel of a logged-out provider.
    LibraryLoggedOut,
}

impl SkinState {
    /// The panel the shot draws: main 0, equalizer 1, playlist 2, library 3 (the demo skin order).
    fn panel(self) -> usize {
        match self {
            SkinState::Equalizer => 1,
            SkinState::Playlist
            | SkinState::PlaylistEmpty
            | SkinState::PlaylistScrolled
            | SkinState::PlaylistResized => 2,
            SkinState::Library | SkinState::LibraryTracks | SkinState::LibraryLoggedOut => 3,
            _ => 0,
        }
    }
}

/// A queued track name long enough to reach the duration column of the playlist list.
const LONG_NAME: &str = "An Unreasonably Long Track Name That Has To Be Cut Before The Time";
/// The list rects' tops, the skin pixel column the shots click in, and the row pitch.
const LIST_TOP: f32 = 20.0;
const LIST_ROW: f32 = 11.0;
const LIST_CLICK_X: f32 = 60.0;
/// The scrolled playlist shot: rows the wheel scrolls (a line each), the scroll bar's left edge
/// and the top of the thumb in skin px once it sits at row 4 of 7 (12 rows, 5 in view): 20 +
/// round(4 / 7 * (58 - 18)).
const SCROLLED_ROWS: f32 = 4.0;
const SCROLL_BAR_X: f32 = 260.0;
const SCROLLED_THUMB_TOP: f32 = 43.0;
/// Frames drawn while a page of a list loads.
const LOAD_STEPS: usize = 24;

struct SkinShot {
    name: &'static str,
    state: SkinState,
    scale: f32,
}

fn skin_shots() -> [SkinShot; 12] {
    let shot = |name, state, scale| SkinShot { name, state, scale };
    [
        shot("skin-main@1x", SkinState::Hint, 1.0),
        shot("skin-main@2x", SkinState::Scrolling, 2.0),
        shot("skin-main-pressed@2x", SkinState::Pressed, 2.0),
        shot("skin-main-stopped@2x", SkinState::Stopped, 2.0),
        shot("skin-equalizer@2x", SkinState::Equalizer, 2.0),
        shot("skin-playlist@2x", SkinState::Playlist, 2.0),
        shot("skin-library@2x", SkinState::Library, 2.0),
        shot("skin-library-tracks@2x", SkinState::LibraryTracks, 2.0),
        shot("skin-playlist-empty@2x", SkinState::PlaylistEmpty, 2.0),
        shot(
            "skin-playlist-scrolled@2x",
            SkinState::PlaylistScrolled,
            2.0,
        ),
        shot("skin-playlist-resized@2x", SkinState::PlaylistResized, 2.0),
        shot(
            "skin-library-logged-out@2x",
            SkinState::LibraryLoggedOut,
            2.0,
        ),
    ]
}

/// The synthetic skin's file name in the demo's skin source.
const DEMO_SKIN: &str = "demo.wsz";

/// A skin library over the synthetic skin, built through the real engine; nothing is chosen yet.
fn demo_library() -> anyhow::Result<SkinLibrary> {
    let mut registry = SkinRegistry::new();
    registry
        .register(Arc::new(WinampEngine))
        .map_err(|_| anyhow::anyhow!("could not register the skin engine"))?;
    let source: Arc<dyn SkinSource> = Arc::new(FakeSkinSource::new(vec![(
        DEMO_SKIN.to_owned(),
        demo_skin(),
    )]));
    Ok(SkinLibrary::new(Some(source), Arc::new(registry)))
}

/// A skin front-end over the synthetic skin, loaded through the real engine on the loader thread
/// (waits for it), and its panel plan.
fn skin_ui(scale: f32) -> anyhow::Result<(SkinUi, PanelPlan)> {
    skin_ui_with(scale, SkinPrefs::default())
}

/// The playlist size of the resized shot: two lattice steps each way (25x29) from 275x116.
const RESIZED_PLAYLIST: (u32, u32) = (325, 174);

/// [`skin_ui`] with the prefs the skin starts from.
fn skin_ui_with(scale: f32, prefs: SkinPrefs) -> anyhow::Result<(SkinUi, PanelPlan)> {
    let library = demo_library()?;
    let mut ui = SkinUi::new(library.clone(), prefs);
    ui.set_pixel_scale(pixel_scale(f64::from(scale)));
    library.choose(DEMO_SKIN);
    let until = Instant::now() + SKIN_LOAD_WAIT;
    loop {
        match ui.take_change() {
            Some(FrontChange::Panels(plan)) => return Ok((ui, plan)),
            Some(FrontChange::Native) => bail!("the skin dump got the native look"),
            None => {}
        }
        if Instant::now() >= until || !ui.pending() {
            bail!("the demo skin did not load: {:?}", library.status());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The playlist (of the first few) whose first track has a long enough "artist - title".
fn long_title_playlist(rt: &tokio::runtime::Runtime) -> anyhow::Result<usize> {
    // Not the second playlist: the demo provider refuses it.
    for playlist in [0, 2, 3, 4] {
        let handle = player(
            rt,
            Setup {
                art: false,
                playlist,
                ..Setup::new(true)
            },
        )?;
        if !wait_for(|| handle.state().current.is_some()) {
            continue;
        }
        let long = handle.state().current.as_ref().is_some_and(|c| {
            format!("{} - {}", c.track.artist, c.track.name)
                .chars()
                .count()
                >= LONG_TITLE_GLYPHS
        });
        if long {
            return Ok(playlist);
        }
    }
    bail!("no demo track has a title of {LONG_TITLE_GLYPHS} glyphs or more")
}

/// Draws a skin shot into `path`: one panel, its own player, loader and front-end.
fn capture_skin(
    rt: &tokio::runtime::Runtime,
    shot: &SkinShot,
    playlist: usize,
    path: &Path,
) -> anyhow::Result<()> {
    let playback = shot.state != SkinState::LibraryLoggedOut;
    let handle = player(
        rt,
        Setup {
            art: false,
            playlist,
            ..Setup::new(playback)
        },
    )?;
    // The resized shot opens the playlist from a saved layout, as the app does.
    let prefs = if shot.state == SkinState::PlaylistResized {
        SkinPrefs {
            on_top: false,
            saved: Some(SavedLayout {
                skin: DEMO_SKIN.to_owned(),
                panels: vec![SavedPanel {
                    id: "playlist".to_owned(),
                    x: 0,
                    y: 0,
                    visible: true,
                    size: Some(RESIZED_PLAYLIST),
                }],
            }),
        }
    } else {
        SkinPrefs::default()
    };
    let (mut skin, plan) = skin_ui_with(shot.scale, prefs)?;
    let k = skin.pixel_scale();
    let panel = shot.state.panel();
    // The equalizer and the playlist are open and the library is closed (the skin's start), and
    // the shown panel has the focus.
    skin.force(SkinForce {
        visible: vec![(1, true), (2, true), (3, panel == 3)],
    });
    skin.focused(panel, true);
    let spec = &plan.panels[panel];
    let logical = (
        ((spec.size.0 * k) as f32 / shot.scale).round() as u32,
        ((spec.size.1 * k) as f32 / shot.scale).round() as u32,
    );
    let mut dumper = Dumper::new(&mut PanelView::new(&mut skin, panel), logical, shot.scale)?;
    match shot.state {
        SkinState::Pressed => {
            handle.dispatch(PlayerAction::SetShuffle(true));
            handle.dispatch(PlayerAction::SetRepeat(player::RepeatMode::All));
            handle.dispatch(PlayerAction::SetVolume(30));
            handle.dispatch(PlayerAction::SetBalance(-100));
        }
        SkinState::Equalizer => {
            handle.dispatch(PlayerAction::SetEqEnabled(true));
            handle.dispatch(PlayerAction::SetEqPreamp(SKIN_PREAMP));
            for (index, gain_db) in SKIN_BANDS.into_iter().enumerate() {
                handle.dispatch(PlayerAction::SetEqBand { index, gain_db });
            }
        }
        _ => {}
    }
    if shot.state == SkinState::Hint {
        skin.hint(SKIN_HINT);
    }
    let mut step = |skin: &mut SkinUi, events: Vec<Event>, advance: Duration| {
        let r = dumper.step(&mut PanelView::new(skin, panel), &handle, events, advance);
        std::thread::sleep(FRAME_PAUSE);
        r
    };
    // Until a track is current and has been drawn once.
    let until = Instant::now() + STATE_WAIT;
    while playback && handle.state().current.is_none() && Instant::now() < until {
        step(&mut skin, Vec::new(), FRAME_STEP)?;
    }
    step(&mut skin, Vec::new(), Duration::ZERO)?;
    let mut spectrum = SpectrumFrame::default();
    if playback {
        wait_for(|| is_playing(&handle));
        wait_for(|| {
            handle.spectrum(&mut spectrum);
            spectrum.active
        });
    } else {
        // The provider answer about its login.
        wait_for(|| handle.state().providers.iter().any(|p| p.auth.is_some()));
    }
    // A click on row `row` of a list at the list skin column.
    let to_logical = |v: f32| v * k as f32 / shot.scale;
    let click = |row: usize| {
        let pos = (
            to_logical(LIST_CLICK_X),
            to_logical(LIST_TOP + LIST_ROW * row as f32 + LIST_ROW / 2.0),
        );
        let button = |pressed| Event::PointerButton {
            pos,
            button: lune_ui::input::PointerButton::Primary,
            pressed,
            modifiers: lune_ui::input::Modifiers::default(),
        };
        vec![Event::PointerMoved { pos }, button(true), button(false)]
    };
    match shot.state {
        SkinState::Scrolling => {
            // About 1:23 into the track, the title text's end SCROLL_END_AT px inside its rect.
            handle.dispatch(PlayerAction::Seek {
                position_ms: 83_000,
            });
            // The frames below start once the seek shows, not however soon the player gets to it.
            wait_for(|| handle.state().position_now() >= 83_000);
            let glyphs = handle.state().current.as_ref().map_or(0, |c| {
                format!("{} - {}", c.track.artist, c.track.name)
                    .chars()
                    .count()
            }) as u64;
            let travel_ms = (glyphs * 5).saturating_sub(SCROLL_END_AT) * 1000 / 20;
            step(&mut skin, Vec::new(), Duration::from_millis(travel_ms))?;
            for _ in 0..6 {
                step(&mut skin, Vec::new(), Duration::from_millis(10))?;
            }
        }
        SkinState::Pressed => {
            // The pointer over the play button (39,88, 23x18 skin px) and held.
            let pos = (to_logical(50.0), to_logical(97.0));
            for _ in 0..6 {
                step(&mut skin, Vec::new(), FRAME_STEP)?;
            }
            step(&mut skin, vec![Event::PointerMoved { pos }], FRAME_STEP)?;
            let press = Event::PointerButton {
                pos,
                button: lune_ui::input::PointerButton::Primary,
                pressed: true,
                modifiers: lune_ui::input::Modifiers::default(),
            };
            step(&mut skin, vec![press], FRAME_STEP)?;
        }
        SkinState::Stopped => {
            handle.dispatch(PlayerAction::Stop);
            wait_for(|| handle.state().play_state == PlayState::Stopped);
            // The bars fall to rest: 250 ms steps (the fall is capped per step).
            for _ in 0..16 {
                step(&mut skin, Vec::new(), Duration::from_millis(250))?;
            }
        }
        SkinState::Hint | SkinState::Equalizer => {
            for _ in 0..8 {
                step(&mut skin, Vec::new(), FRAME_STEP)?;
            }
        }
        SkinState::Playlist | SkinState::PlaylistScrolled | SkinState::PlaylistResized => {
            // Twelve queued, the third playing (a long name on the second row), then the fifth
            // row clicked and the pointer gone. The scrolled shot instead scrolls four rows with
            // the wheel and holds the thumb; the resized one is the same list in a taller panel.
            let mut context: Vec<_> = handle
                .state()
                .queue
                .items
                .iter()
                .take(12)
                .cloned()
                .collect();
            if context.len() != 12 {
                bail!("the demo queue has {} tracks, not 12", context.len());
            }
            context[1].name = LONG_NAME.to_owned();
            handle.dispatch(PlayerAction::PlayItem { context, index: 2 });
            if !wait_for(|| {
                let s = handle.state();
                s.queue.items.len() == 12 && s.queue.current == Some(2)
            }) {
                bail!("the demo queue did not settle");
            }
            for _ in 0..8 {
                step(&mut skin, Vec::new(), FRAME_STEP)?;
            }
            if shot.state == SkinState::PlaylistScrolled {
                let at = (to_logical(LIST_CLICK_X), to_logical(LIST_TOP + 20.0));
                let wheel = Event::Scroll {
                    pos: at,
                    delta: ScrollDelta::Lines {
                        x: 0.0,
                        y: -SCROLLED_ROWS,
                    },
                    modifiers: Modifiers::default(),
                };
                step(
                    &mut skin,
                    vec![Event::PointerMoved { pos: at }, wheel],
                    FRAME_STEP,
                )?;
                // The thumb now sits at SCROLLED_THUMB_TOP; the pointer on it, held.
                let on = (
                    to_logical(SCROLL_BAR_X + 4.0),
                    to_logical(SCROLLED_THUMB_TOP + 9.0),
                );
                step(&mut skin, vec![Event::PointerMoved { pos: on }], FRAME_STEP)?;
                let press = Event::PointerButton {
                    pos: on,
                    button: lune_ui::input::PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::default(),
                };
                step(&mut skin, vec![press], FRAME_STEP)?;
            } else {
                step(&mut skin, click(4), FRAME_STEP)?;
                step(&mut skin, vec![Event::PointerLeft], FRAME_STEP)?;
            }
            for _ in 0..4 {
                step(&mut skin, Vec::new(), FRAME_STEP)?;
            }
        }
        SkinState::PlaylistEmpty => {
            handle.dispatch(PlayerAction::ClearQueue);
            if !wait_for(|| handle.state().queue.items.is_empty()) {
                bail!("the demo queue did not clear");
            }
            for _ in 0..8 {
                step(&mut skin, Vec::new(), FRAME_STEP)?;
            }
        }
        SkinState::Library | SkinState::LibraryLoggedOut => {
            for _ in 0..8 {
                step(&mut skin, Vec::new(), FRAME_STEP)?;
            }
        }
        SkinState::LibraryTracks => {
            // Top level: Liked, Playlists, Albums. Playlists, then its first playlist (the page
            // loads between the clicks).
            for _ in 0..4 {
                step(&mut skin, Vec::new(), FRAME_STEP)?;
            }
            step(&mut skin, click(1), FRAME_STEP)?;
            for _ in 0..LOAD_STEPS {
                step(&mut skin, Vec::new(), FRAME_STEP)?;
            }
            step(&mut skin, click(1), FRAME_STEP)?;
            for _ in 0..LOAD_STEPS {
                step(&mut skin, Vec::new(), FRAME_STEP)?;
            }
            step(&mut skin, vec![Event::PointerLeft], FRAME_STEP)?;
            for _ in 0..4 {
                step(&mut skin, Vec::new(), FRAME_STEP)?;
            }
        }
    }
    let shape = spec.shape.clone();
    let scale = k as f32;
    dumper.save_masked(path, |x, y| {
        shape
            .as_ref()
            .is_none_or(|s| s.contains(((x as f32 + 0.5) / scale, (y as f32 + 0.5) / scale)))
    })?;
    println!("dumped {}", path.display());
    Ok(())
}

// ---- the desktop composite and the Skins card (M9 step 7) ------------------------------------

/// The composite of the visible panels as one view (the demo skin's attach layout).
struct Desktop<'s> {
    skin: &'s mut SkinUi,
}

impl host::WindowView for Desktop<'_> {
    fn frame(&mut self, ui: &mut lune_ui::ui::Ui<'_>, player: &PlayerHandle) {
        self.skin.compose(ui, player);
    }

    fn images(&mut self, store: &mut dyn lune_ui::image::ImageStore) {
        self.skin.images(store);
    }

    fn shortcuts(&self) -> lune_ui::shortcuts::Shortcuts {
        self.skin.shortcuts()
    }
}

/// How many frames the desktop shots settle for before the pinned scroll.
const DESKTOP_WARMUP: usize = 8;

/// Draws the desktop composite at `scale` into `path`: the equalizer and the playlist open, the
/// library closed, main focused, a track playing with its long title scrolled to the pinned point,
/// the equalizer on with the shots' curve, twelve queued with the third playing. With `parts`,
/// each of the three panels is also drawn alone, in the same state and at the same app times,
/// into `<parts>/skin-desktop-<panel>@<n>x.png` (the pixel checks compare them with the bands).
fn capture_desktop(
    rt: &tokio::runtime::Runtime,
    playlist: usize,
    scale: f32,
    path: &Path,
    parts: Option<&Path>,
) -> anyhow::Result<()> {
    let handle = player(
        rt,
        Setup {
            art: false,
            playlist,
            ..Setup::new(true)
        },
    )?;
    let (mut skin, plan) = skin_ui(scale)?;
    let k = skin.pixel_scale();
    skin.force(SkinForce {
        visible: vec![(1, true), (2, true), (3, false)],
    });
    for panel in 0..plan.panels.len() {
        skin.focused(panel, panel == 0);
    }
    let (placed, size) = skin.compose_layout();
    let shown: Vec<usize> = placed.iter().map(|p| p.panel).collect();
    anyhow::ensure!(
        shown == [0, 1, 2],
        "the desktop shows panels {shown:?}, not main, equalizer and playlist"
    );
    let logical = |physical: u32| (physical as f32 / scale).round() as u32;
    let mut desktop = Dumper::new(
        &mut Desktop { skin: &mut skin },
        (logical(size.0), logical(size.1)),
        scale,
    )?;
    // Each part has its own front-end: a front-end registers its images with one image store
    // only, and every dumper has its own.
    let mut solo: Vec<(Dumper, SkinUi)> = Vec::new();
    if parts.is_some() {
        for p in &placed {
            let (mut part, _) = skin_ui(scale)?;
            part.force(SkinForce {
                visible: vec![(1, true), (2, true), (3, false)],
            });
            for panel in 0..plan.panels.len() {
                part.focused(panel, panel == 0);
            }
            let size = (logical(p.w), logical(p.h));
            let dumper = Dumper::new(&mut PanelView::new(&mut part, p.panel), size, scale)?;
            solo.push((dumper, part));
        }
    }
    // One step of every dumper at the same app time.
    let mut step = |skin: &mut SkinUi, advance: Duration| -> anyhow::Result<()> {
        desktop.step(&mut Desktop { skin }, &handle, Vec::new(), advance)?;
        for ((dumper, part), p) in solo.iter_mut().zip(&placed) {
            dumper.step(
                &mut PanelView::new(part, p.panel),
                &handle,
                Vec::new(),
                advance,
            )?;
        }
        std::thread::sleep(FRAME_PAUSE);
        Ok(())
    };
    handle.dispatch(PlayerAction::SetEqEnabled(true));
    handle.dispatch(PlayerAction::SetEqPreamp(SKIN_PREAMP));
    for (index, gain_db) in SKIN_BANDS.into_iter().enumerate() {
        handle.dispatch(PlayerAction::SetEqBand { index, gain_db });
    }
    let mut context: Vec<_> = handle
        .state()
        .queue
        .items
        .iter()
        .take(12)
        .cloned()
        .collect();
    if context.len() != 12 {
        bail!("the demo queue has {} tracks, not 12", context.len());
    }
    context[1].name = LONG_NAME.to_owned();
    handle.dispatch(PlayerAction::PlayItem { context, index: 2 });
    if !wait_for(|| {
        let s = handle.state();
        s.queue.items.len() == 12 && s.queue.current == Some(2)
    }) {
        bail!("the demo queue did not settle");
    }
    wait_for(|| is_playing(&handle));
    let mut spectrum = SpectrumFrame::default();
    wait_for(|| {
        handle.spectrum(&mut spectrum);
        spectrum.active
    });
    for _ in 0..DESKTOP_WARMUP {
        step(&mut skin, FRAME_STEP)?;
    }
    handle.dispatch(PlayerAction::Seek {
        position_ms: 83_000,
    });
    let glyphs = handle.state().current.as_ref().map_or(0, |c| {
        format!("{} - {}", c.track.artist, c.track.name)
            .chars()
            .count()
    }) as u64;
    let travel_ms = (glyphs * 5).saturating_sub(SCROLL_END_AT) * 1000 / 20;
    step(&mut skin, Duration::from_millis(travel_ms))?;
    for _ in 0..6 {
        step(&mut skin, Duration::from_millis(10))?;
    }
    // The shapes cut the composite: each pixel is kept only when the panel that covers it does.
    let shapes: Vec<_> = placed
        .iter()
        .map(|p| (*p, plan.panels[p.panel].shape.clone()))
        .collect();
    let scale_k = k as f32;
    desktop.save_masked(path, |x, y| {
        let (x, y) = (x as i32, y as i32);
        shapes.iter().any(|(p, shape)| {
            let inside = x >= p.x && y >= p.y && x < p.x + p.w as i32 && y < p.y + p.h as i32;
            inside
                && shape.as_ref().is_none_or(|s| {
                    s.contains((
                        ((x - p.x) as f32 + 0.5) / scale_k,
                        ((y - p.y) as f32 + 0.5) / scale_k,
                    ))
                })
        })
    })?;
    println!("dumped {}", path.display());
    if let Some(dir) = parts {
        let names = ["main", "equalizer", "playlist"];
        for (((dumper, _), (_, shape)), name) in solo.iter().zip(&shapes).zip(names) {
            let part = dir.join(format!("skin-desktop-{name}@{}x.png", scale as u32));
            dumper.save_masked(&part, |x, y| {
                shape.as_ref().is_none_or(|s| {
                    s.contains(((x as f32 + 0.5) / scale_k, (y as f32 + 0.5) / scale_k))
                })
            })?;
            println!("dumped {}", part.display());
        }
    }
    Ok(())
}

/// What the Skins card shows as its folder in the demo: a fixed text, never a real path.
const SKINS_FOLDER: &str = "demo skins folder";

/// The native Settings page (theme C, 1440x900) with its Skins card, the demo skin listed and in
/// use, into `path`. The page is not scrolled: at this size the whole card sits under the look
/// card.
fn capture_settings_skins(rt: &tokio::runtime::Runtime, path: &Path) -> anyhow::Result<()> {
    let handle = player(rt, Setup::new(true))?;
    let library = demo_library()?;
    library.choose(DEMO_SKIN);
    let until = Instant::now() + SKIN_LOAD_WAIT;
    while library.pending() || !matches!(library.status(), SkinStatus::Active { .. }) {
        if Instant::now() >= until {
            bail!("the demo skin did not load: {:?}", library.status());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let shot = Shot::new("c-settings-skins@1x", ThemeId::C, Route::Settings);
    let mut view = NativeUi::new(host::chrome(), Tab::Home)
        .with_prefs(UiPrefs {
            theme: shot.theme,
            auto_hide: shot.auto_hide,
            tour_seen: true,
            ..UiPrefs::default()
        })
        .with_skins(library, Some(SKINS_FOLDER.to_owned()));
    view.force(Force {
        mode: Some(shot.mode),
        ..Force::default()
    });
    let dumper = Dumper::new(&mut view, shot.size, shot.scale)?;
    let mut script = Script {
        dumper,
        view,
        handle: &handle,
    };
    script.view.navigate(shot.route.clone());
    script.frames(12)?;
    // As the other native shots: the track plays, then freezes so the bars and the title hold.
    script.wait_playing();
    script.frames(8)?;
    script.settle(false)?;
    script.freeze()?;
    script.dumper.save(path)?;
    println!("dumped {}", path.display());
    Ok(())
}

// ---- the panel windows on the real desktop (M9 step 7) ---------------------------------------

/// The frames `--smoke-skin` waits for from the panel windows (several panels draw at once).
const SMOKE_SKIN_FRAMES: u32 = 240;

/// The panel check of `--smoke-skin`: the panel-windows contract on the real windows (a panic
/// becomes an error), then the attach layout the skin declares, on the real frames.
fn check_panels(windows: &mut dyn PanelWindows) -> Result<(), String> {
    let contract = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_panel_windows_contract(&mut *windows);
    }));
    if let Err(payload) = contract {
        let text = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("a panic without text");
        return Err(text.to_owned());
    }
    if windows.count() != 4 {
        return Err(format!("{} panel windows, not 4", windows.count()));
    }
    let frame = |i: usize| {
        windows
            .frame(i)
            .ok_or_else(|| format!("panel {i} has no frame"))
    };
    let (main, equalizer, playlist) = (frame(0)?, frame(1)?, frame(2)?);
    if (equalizer.x, equalizer.y) != (main.x, main.y + main.h) {
        return Err(format!(
            "the equalizer is at {},{}, not under main at {},{}",
            equalizer.x,
            equalizer.y,
            main.x,
            main.y + main.h
        ));
    }
    if (playlist.x, playlist.y) != (equalizer.x, equalizer.y + equalizer.h) {
        return Err(format!(
            "the playlist is at {},{}, not under the equalizer at {},{}",
            playlist.x,
            playlist.y,
            equalizer.x,
            equalizer.y + equalizer.h
        ));
    }
    if windows.is_visible(3) {
        return Err("the library panel is shown at start".to_owned());
    }
    if !windows.is_visible(0) {
        return Err("the first panel is not shown".to_owned());
    }
    Ok(())
}

/// The check `--check-fails` runs instead: the run must end non-zero.
fn forced_failure(_windows: &mut dyn PanelWindows) -> Result<(), String> {
    Err("forced failure".to_owned())
}

/// Opens the demo skin's panel windows on the desktop, runs the panel check once, switches back to
/// the native look, then fails a forced open of the skin's windows (risk A3: the hidden native
/// window must come back both ways) and closes. The player is logged out (nothing plays, so no player wake can open the panels: the host's own
/// poll timer must). Nothing is read from or written to the app's data.
fn smoke_skin(rt: &tokio::runtime::Runtime, check_fails: bool) -> anyhow::Result<()> {
    let handle = player(rt, Setup::new(false))?;
    let library = demo_library()?;
    library.choose(DEMO_SKIN);
    let mut skin = SkinUi::new(library, SkinPrefs::default());
    let mut view = NativeUi::new(host::chrome(), Tab::default());
    let spec = WindowSpec {
        key: "main",
        open_at_start: true,
        title: "Whipspot (demo)".into(),
        size: (f64::from(DUMP_SIZE.0), f64::from(DUMP_SIZE.1)),
        min_size: (760.0, 480.0),
        maximized: false,
        position: None,
    };
    let check: PanelCheck = if check_fails {
        forced_failure
    } else {
        check_panels
    };
    let opts = HostOptions {
        smoke_frames: Some(SMOKE_SKIN_FRAMES),
        system_fonts: false,
        panel_check: Some(check),
        switch_back: true,
    };
    let report = host::run(
        &handle,
        vec![(spec, &mut view)],
        Some(&mut skin),
        None,
        opts,
    )?;
    println!("smoke-skin ok: {} frames", report.frames);
    Ok(())
}

/// Both desktop composites into `dir`, with their parts when `parts`.
fn dump_desktops(
    rt: &tokio::runtime::Runtime,
    playlist: usize,
    dir: &Path,
    parts: bool,
) -> anyhow::Result<()> {
    for (name, scale) in [("skin-desktop@1x", 1.0), ("skin-desktop@2x", 2.0)] {
        let path = dir.join(format!("{name}.png"));
        capture_desktop(rt, playlist, scale, &path, parts.then_some(dir))?;
    }
    Ok(())
}

/// Measures the scrolled playlist shot: the pixels where the thumb sits (centred in the bar, 4/7
/// of the way down) are, at 2x, the demo sheet's pressed thumb sprite, and that sprite is not the
/// normal one, so the skin's own art is drawn and not a generic bar.
fn check_scroll_thumb(path: &Path) -> anyhow::Result<()> {
    let skin = WinampEngine
        .load(&demo_skin())
        .map_err(|_| anyhow::anyhow!("the demo skin did not load"))?;
    let panel = skin
        .panels
        .iter()
        .find(|p| p.id.as_str() == "playlist")
        .context("the demo skin has no playlist panel")?;
    let (bar, thumb) = panel
        .widgets
        .iter()
        .find_map(|w| match &w.kind {
            WidgetKind::ListScroll { thumb } => Some((w.rect, *thumb)),
            _ => None,
        })
        .context("the playlist has no scroll bar")?;
    let shot = read_png(path)?;
    let sheet = |s: whipspot_core::skin::SpriteRect| -> anyhow::Result<Vec<[u8; 3]>> {
        let image = skin
            .images
            .get(usize::from(s.image.0))
            .context("the thumb's sheet is missing")?;
        let mut px = Vec::new();
        for y in 0..s.rect.h {
            for x in 0..s.rect.w {
                let at = ((s.rect.y + y) as usize * image.width() as usize
                    + (s.rect.x + x) as usize)
                    * 4;
                let p = &image.rgba()[at..at + 3];
                px.push([p[0], p[1], p[2]]);
            }
        }
        Ok(px)
    };
    let (normal, pressed) = (sheet(thumb[0])?, sheet(thumb[1])?);
    anyhow::ensure!(
        normal != pressed,
        "the demo's two thumb sprites are the same"
    );
    let (tw, th) = (thumb[1].rect.w, thumb[1].rect.h);
    let travel = bar.h - th;
    // First row 4 of 7: round(4 / 7 * travel).
    let x0 = bar.x + (bar.w - tw) / 2;
    let y0 = bar.y + (4 * travel + 3) / 7;
    let k = 2usize;
    let mut wrong = 0;
    for sy in 0..th as usize {
        for sx in 0..tw as usize {
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let (px, py) = (k * (x0 as usize + sx) + dx, k * (y0 as usize + sy) + dy);
                let at = (py * shot.size.0 as usize + px) * 4;
                if shot.rgba[at..at + 3] != pressed[sy * tw as usize + sx] {
                    wrong += 1;
                }
            }
        }
    }
    anyhow::ensure!(
        wrong == 0,
        "{wrong} thumb pixels of {} differ from the pressed sprite",
        path.display()
    );
    Ok(())
}

/// The skin screens: the twelve panels, the two desktop composites and the Skins card.
fn dump_skin(rt: &tokio::runtime::Runtime, dir: &Path) -> anyhow::Result<()> {
    let playlist = long_title_playlist(rt)?;
    for shot in skin_shots() {
        let path = dir.join(format!("{}.png", shot.name));
        capture_skin(rt, &shot, playlist, &path)?;
        if shot.state == SkinState::PlaylistScrolled {
            check_scroll_thumb(&path)?;
        }
    }
    dump_desktops(rt, playlist, dir, false)?;
    capture_settings_skins(rt, &dir.join("c-settings-skins@1x.png"))
}

// ---- the tray icon (step 7 of the skin-playlist-tray task) --------------------------------------

/// The flat desktops behind the menu dumps: a dark and a light Windows 11 taskbar colour.
const TRAY_DARK: [u8; 4] = [0x20, 0x20, 0x20, 0xFF];
const TRAY_LIGHT: [u8; 4] = [0xF3, 0xF3, 0xF3, 0xFF];
/// The margin of desktop around the menu window in the dumps, logical px.
const TRAY_MARGIN: f32 = 8.0;
/// A second synthetic skin's name, so the skins page lists more than one.
const DEMO_SKIN_B: &str = "second demo skin.wsz";

/// The skin library of the tray shots: the synthetic skin under `names` (so the skins page lists
/// them all).
fn tray_library(names: &[&str]) -> anyhow::Result<SkinLibrary> {
    let mut registry = SkinRegistry::new();
    registry
        .register(Arc::new(WinampEngine))
        .map_err(|_| anyhow::anyhow!("could not register the skin engine"))?;
    let entries = names
        .iter()
        .map(|name| ((*name).to_owned(), demo_skin()))
        .collect();
    let source: Arc<dyn SkinSource> = Arc::new(FakeSkinSource::new(entries));
    Ok(SkinLibrary::new(Some(source), Arc::new(registry)))
}

/// `--smoke-tray`: the tray icon and its menu on the real desktop, with the demo's fakes. Nothing
/// is read from or written to the app's data.
fn smoke_tray(rt: &tokio::runtime::Runtime, check_fails: bool) -> anyhow::Result<()> {
    let handle = player(rt, Setup::new(true))?;
    let menu = TrayMenu::new(tray_library(&[DEMO_SKIN, DEMO_SKIN_B])?, true);
    host::tray_smoke(menu, &handle, check_fails)?;
    println!("smoke-tray ok");
    Ok(())
}

/// A menu window drawn the way `Ui::menu_window` draws it, over a flat desktop.
struct MenuShot {
    menu: TrayMenu,
    state: PlayerState,
    desktop: [u8; 4],
    panel: (f32, f32),
}

impl host::WindowView for MenuShot {
    fn frame(&mut self, ui: &mut Ui<'_>, _player: &PlayerHandle) {
        let win = ui.painter().window;
        ui.painter().fill(win, 0.0, self.desktop);
        let area = Rect::new(TRAY_MARGIN, TRAY_MARGIN, self.panel.0, self.panel.1);
        let _ = self.menu.frame_of(ui, area, &self.state);
    }
}

/// Draws `menu` for `state` over `desktop` into `path`; returns the menu panel's size, logical px.
fn capture_tray_menu(
    handle: &PlayerHandle,
    menu: TrayMenu,
    state: PlayerState,
    desktop: [u8; 4],
    path: &Path,
) -> anyhow::Result<(f32, f32)> {
    let mut text = host::bundled_text_cache();
    let panel = {
        let mut p: Painter<'_, ()> = Painter::new(
            1.0,
            Rect::new(0.0, 0.0, 4096.0, 4096.0),
            &mut text,
            None,
            lune_ui::lunegit_dark::THEME,
        );
        menu.size(&mut p, &state)
    };
    let size = (
        (panel.0 + 2.0 * TRAY_MARGIN).ceil() as u32,
        (panel.1 + 2.0 * TRAY_MARGIN).ceil() as u32,
    );
    let mut view = MenuShot {
        menu,
        state,
        desktop,
        panel,
    };
    let mut dumper = Dumper::new(&mut view, size, 1.0)?;
    dumper.step(&mut view, handle, Vec::new(), Duration::ZERO)?;
    dumper.step(&mut view, handle, Vec::new(), Duration::from_secs(1))?;
    dumper.step(&mut view, handle, Vec::new(), Duration::from_secs(1))?;
    dumper.save(path)?;
    println!("dumped {}", path.display());
    Ok(panel)
}

/// Waits until the library has listed `count` skins.
fn wait_for_skins(library: &SkinLibrary, count: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        wait_for(|| library.names().len() >= count),
        "the demo skins were not listed"
    );
    Ok(())
}

/// The main page and the skins page (the demo skin current) of the tray menu, and the icon.
fn dump_tray(rt: &tokio::runtime::Runtime, dir: &Path) -> anyhow::Result<()> {
    let handle = player(rt, Setup::new(true))?;
    anyhow::ensure!(
        wait_for(|| is_playing(&handle)),
        "the demo track did not play"
    );
    let library = tray_library(&[DEMO_SKIN, DEMO_SKIN_B])?;
    let main = TrayMenu::new(library.clone(), true);
    capture_tray_menu(
        &handle,
        main,
        (*handle.state()).clone(),
        TRAY_DARK,
        &dir.join("tray-menu@1x.png"),
    )?;
    library.refresh();
    wait_for_skins(&library, 2)?;
    library.choose(DEMO_SKIN);
    anyhow::ensure!(
        wait_for(|| matches!(library.status(), SkinStatus::Active { .. })),
        "the demo skin did not load"
    );
    let mut skins = TrayMenu::new(library, true);
    skins.apply(whipspot_ui::tray::Pick::OpenSkins);
    capture_tray_menu(
        &handle,
        skins,
        (*handle.state()).clone(),
        TRAY_DARK,
        &dir.join("tray-menu-skins@1x.png"),
    )?;
    write_image(&dir.join("tray-icon@1x.png"), &tray_icon_sheet())?;
    println!("dumped {}", dir.join("tray-icon@1x.png").display());
    Ok(())
}

/// The screens only the visual review looks at: long texts, the paused state and a light desktop.
fn dump_tray_scratch(rt: &tokio::runtime::Runtime, dir: &Path) -> anyhow::Result<()> {
    let handle = player(rt, Setup::new(true))?;
    anyhow::ensure!(
        wait_for(|| is_playing(&handle)),
        "the demo track did not play"
    );
    let long_skin = format!("{}.wsz", "a-skin-with-a-very-long-file-name-".repeat(2));
    let library = tray_library(&[DEMO_SKIN, &long_skin])?;
    let menu = |library: &SkinLibrary| TrayMenu::new(library.clone(), true);
    let mut long = (*handle.state()).clone();
    if let Some(now) = long.current.as_mut() {
        now.track.name = "The Extraordinarily Long Title Of A Symphonic Movement In Several Parts "
            .repeat(2)
            .chars()
            .take(80)
            .collect();
        now.track.artist = "An Ensemble With A Great Many Players ".repeat(2)[..60].to_owned();
    }
    let (w, _) = capture_tray_menu(
        &handle,
        menu(&library),
        long.clone(),
        TRAY_DARK,
        &dir.join("menu-long-main.png"),
    )?;
    anyhow::ensure!(w <= 280.0, "the long main page is {w} px wide");
    library.refresh();
    wait_for_skins(&library, 2)?;
    library.choose(DEMO_SKIN);
    anyhow::ensure!(
        wait_for(|| matches!(library.status(), SkinStatus::Active { .. })),
        "the demo skin did not load"
    );
    let mut skins = menu(&library);
    skins.apply(whipspot_ui::tray::Pick::OpenSkins);
    let (w, _) = capture_tray_menu(
        &handle,
        skins,
        long.clone(),
        TRAY_DARK,
        &dir.join("menu-long-skins.png"),
    )?;
    anyhow::ensure!(w <= 280.0, "the long skins page is {w} px wide");
    capture_tray_menu(
        &handle,
        menu(&library),
        (*handle.state()).clone(),
        TRAY_LIGHT,
        &dir.join("menu-light.png"),
    )?;
    let mut paused = (*handle.state()).clone();
    paused.play_state = PlayState::Paused;
    capture_tray_menu(
        &handle,
        menu(&library),
        paused,
        TRAY_DARK,
        &dir.join("menu-paused.png"),
    )?;
    Ok(())
}

/// The icon halved by the mean of premultiplied 2x2 blocks (what an OS does to scale a straight
/// alpha picture down).
fn halve_icon(rgba: &[u8], side: usize) -> Vec<u8> {
    let half = side / 2;
    let mut out = Vec::with_capacity(half * half * 4);
    for y in 0..half {
        for x in 0..half {
            let (mut c, mut a) = ([0u32; 3], 0u32);
            for (dy, dx) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let at = ((2 * y + dy) * side + 2 * x + dx) * 4;
                let alpha = u32::from(rgba[at + 3]);
                for (i, ch) in c.iter_mut().enumerate() {
                    *ch += u32::from(rgba[at + i]) * alpha;
                }
                a += alpha;
            }
            if a == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                out.extend(c.iter().map(|ch| ((ch + a / 2) / a) as u8));
                out.push(((a + 2) / 4) as u8);
            }
        }
    }
    out
}

/// Draws `src` (straight alpha, `side` square) at (`x`, `y`) of the opaque `dst` (`width` wide),
/// each source pixel `k` x `k` (nearest).
fn blit_icon(dst: &mut [u8], width: usize, at: (usize, usize), src: &[u8], side: usize, k: usize) {
    for sy in 0..side {
        for sx in 0..side {
            let p = &src[(sy * side + sx) * 4..][..4];
            let alpha = u32::from(p[3]);
            for dy in 0..k {
                for dx in 0..k {
                    let d = ((at.1 + sy * k + dy) * width + at.0 + sx * k + dx) * 4;
                    for c in 0..3 {
                        let mixed = u32::from(p[c]) * alpha + u32::from(dst[d + c]) * (255 - alpha);
                        dst[d + c] = ((mixed + 127) / 255) as u8;
                    }
                }
            }
        }
    }
}

/// The icon on a light and a dark strip (the taskbar stand-ins): `icon_rgba(16)`, the 32 px icon
/// halved to 16 (what the OS does at 100 %) and `icon_rgba(32)`, each 1:1 and enlarged 4x.
fn tray_icon_sheet() -> Image {
    const PAD: usize = 8;
    const K: usize = 4;
    let (small, big) = (icon_rgba(16), icon_rgba(32));
    let halved = halve_icon(&big, 32);
    let items: [(&[u8], usize); 3] = [(&small, 16), (&halved, 16), (&big, 32)];
    let strip_w = PAD + items.iter().map(|(_, s)| s * K + PAD).sum::<usize>();
    let strip_h = PAD + 32 + PAD + 32 * K + PAD;
    let (width, height) = (2 * strip_w, strip_h);
    let mut rgba = vec![0u8; width * height * 4];
    for (i, bg) in [TRAY_LIGHT, TRAY_DARK].into_iter().enumerate() {
        for y in 0..height {
            for x in 0..strip_w {
                let d = (y * width + i * strip_w + x) * 4;
                rgba[d..d + 4].copy_from_slice(&bg);
            }
        }
        let mut x = i * strip_w + PAD;
        for (src, side) in items {
            blit_icon(&mut rgba, width, (x, PAD), src, side, 1);
            blit_icon(&mut rgba, width, (x, PAD + 32 + PAD), src, side, K);
            x += side * K + PAD;
        }
    }
    Image {
        size: (width as u32, height as u32),
        rgba,
    }
}

// ---- golden screenshots (H14) ----------------------------------------------------------------

/// The screens kept as goldens, halved (a mix of the pages, the diagnostics window and a skin).
const GOLDEN_SET: [&str; 8] = [
    "c-browse-grid@1x",
    "a-browse-tracks@1x",
    "b-idle@1x",
    "c-settings@1x",
    "c-equalizer@1x",
    "c-queue@1x",
    "diagnostics@1x",
    "skin-main@2x",
];
/// A pixel passes when every channel is within this of the golden.
const GOLDEN_CHANNEL_TOLERANCE: u8 = 8;
/// At least this share of the pixels outside the masks must pass, and the mean absolute channel
/// difference stay under `GOLDEN_MEAN_LIMIT`: room for a GPU's rounding; a moved or changed
/// element fails.
const GOLDEN_PIXEL_SHARE: f64 = 0.999;
const GOLDEN_MEAN_LIMIT: f64 = 1.0;

/// A region of a golden screen whose content varies between runs by design, left out of the
/// comparison (MD131): `[x, y, w, h]` in the halved image's pixels. Measured on this host (M10
/// fix round 2, nine checks, four of them run at once, and checks with a skin shot delayed): the
/// screens differ between runs in these regions and nowhere else; each is no wider than what it
/// covers.
type Region = [usize; 4];

/// The main window with the media control (1440x900, halved; whipspot-ui's layout puts the media
/// control at y 812 to 888 and ends the sheet 16 px above it): the spectrum bars seen in that gap
/// and in the strip under the media control, and the progress knob near the top of the play
/// button's ring (the position, a few seconds into the track).
const MEDIA_LIVE: [Region; 3] = [[0, 398, 720, 8], [0, 444, 720, 6], [354, 404, 14, 12]];
/// The idle view's spectrum bars: `Layout::visualiser`, full width and the bottom 30 %
/// (`spectrum::HEIGHT_SHARE`) of the window; nothing else is drawn there in that view.
const IDLE_LIVE: [Region; 1] = [[0, 315, 720, 135]];
/// The diagnostics window's "Frames decoded" value (how much tone was written by the time of the
/// shot), room for eight digits; the live figures are below the window's fold.
const DIAGNOSTICS_LIVE: [Region; 1] = [[70, 300, 70, 18]];
/// The demo skin's main panel (the halved pixels are the skin's own), playing about 1:23 into the
/// track: its visualiser (76x16 at 24,43), and what the wall clock moves between the seek and
/// the shot (a shot 1 s later failed before these were left out; fix round 2 of M10): the units
/// of the elapsed seconds (the last time digit, 9x13 at 90,26) and the position slider's thumb
/// (29x10 at 84,72 in the golden, a pixel to the right per about 1.2 s on this track), with the
/// room it moves in 6 s more and one pixel before it.
const SKIN_MAIN_LIVE: [Region; 3] = [[24, 43, 76, 16], [90, 26, 9, 13], [83, 72, 35, 10]];

/// The live regions of the golden screen `screen`.
fn masks_of(screen: &str) -> &'static [Region] {
    match screen {
        "c-browse-grid@1x" | "a-browse-tracks@1x" | "c-settings@1x" | "c-equalizer@1x"
        | "c-queue@1x" => &MEDIA_LIVE,
        "b-idle@1x" => &IDLE_LIVE,
        "diagnostics@1x" => &DIAGNOSTICS_LIVE,
        "skin-main@2x" => &SKIN_MAIN_LIVE,
        _ => &[],
    }
}

/// The caption band of the scrolled Settings shot (M9-S7-1): logical px from the top.
const CAPTION_BAND_ROWS: usize = 34;

/// A decoded 8-bit RGBA image.
struct Image {
    size: (u32, u32),
    rgba: Vec<u8>,
}

fn read_png(path: &Path) -> anyhow::Result<Image> {
    let file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder
        .read_info()
        .with_context(|| format!("read {}", path.display()))?;
    let mut buf = vec![0; reader.output_buffer_size().context("png size")?];
    let info = reader.next_frame(&mut buf)?;
    anyhow::ensure!(
        info.bit_depth == png::BitDepth::Eight,
        "{} is not 8 bits per channel",
        path.display()
    );
    let data = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        other => bail!("{} has the color type {other:?}", path.display()),
    };
    Ok(Image {
        size: (info.width, info.height),
        rgba,
    })
}

fn write_image(path: &Path, image: &Image) -> anyhow::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    lune_ui::dump::write_png(path, image.size, &image.rgba).map_err(|e| anyhow::anyhow!("{e}"))
}

/// The image at half the width and height: each pixel the mean of its 2x2 block (an odd last
/// row or column is dropped).
fn halve(image: &Image) -> Image {
    let (w, h) = (image.size.0 as usize / 2, image.size.1 as usize / 2);
    let row = image.size.0 as usize * 4;
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            for c in 0..4 {
                let at = |dy: usize, dx: usize| {
                    u32::from(image.rgba[(2 * y + dy) * row + (2 * x + dx) * 4 + c])
                };
                rgba.push(((at(0, 0) + at(0, 1) + at(1, 0) + at(1, 1) + 2) / 4) as u8);
            }
        }
    }
    Image {
        size: (w as u32, h as u32),
        rgba,
    }
}

/// How far an image is from its golden.
#[derive(Debug)]
struct Difference {
    max: u8,
    /// Share of the compared pixels with every channel within the tolerance.
    within: f64,
    mean: f64,
    /// Pixels left out by a mask.
    masked: usize,
}

impl Difference {
    fn passes(&self) -> bool {
        self.within >= GOLDEN_PIXEL_SHARE && self.mean <= GOLDEN_MEAN_LIMIT
    }
}

impl std::fmt::Display for Difference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "max channel diff {}, {:.3} % of pixels over {}, mean {:.3}, {} px masked",
            self.max,
            (1.0 - self.within) * 100.0,
            GOLDEN_CHANNEL_TOLERANCE,
            self.mean,
            self.masked
        )
    }
}

/// Compares `a` with `b` (the same size), looking at the first `rows` rows only and leaving out
/// the pixels inside `masks` (`[x, y, w, h]` each).
fn compare(a: &Image, b: &Image, rows: usize, masks: &[Region]) -> anyhow::Result<Difference> {
    anyhow::ensure!(
        a.size == b.size,
        "size {}x{} against {}x{}",
        a.size.0,
        a.size.1,
        b.size.0,
        b.size.1
    );
    let width = a.size.0 as usize;
    let len = rows.saturating_mul(width * 4).min(a.rgba.len());
    let masked = |i: usize| {
        let (x, y) = (i % width, i / width);
        masks
            .iter()
            .any(|&[mx, my, mw, mh]| (mx..mx + mw).contains(&x) && (my..my + mh).contains(&y))
    };
    let (mut max, mut sum, mut bad, mut skipped) = (0u8, 0u64, 0usize, 0usize);
    for (i, (pa, pb)) in a.rgba[..len]
        .chunks_exact(4)
        .zip(b.rgba[..len].chunks_exact(4))
        .enumerate()
    {
        if masked(i) {
            skipped += 1;
            continue;
        }
        let mut worst = 0;
        for (x, y) in pa.iter().zip(pb) {
            let d = x.abs_diff(*y);
            worst = worst.max(d);
            sum += u64::from(d);
        }
        max = max.max(worst);
        bad += usize::from(worst > GOLDEN_CHANNEL_TOLERANCE);
    }
    let pixels = (len / 4 - skipped).max(1);
    Ok(Difference {
        max,
        within: 1.0 - bad as f64 / pixels as f64,
        mean: sum as f64 / (pixels * 4) as f64,
        masked: skipped,
    })
}

/// The difference image: each channel's |diff| times 8, opaque.
fn diff_image(a: &Image, b: &Image) -> Image {
    let rgba = a
        .rgba
        .chunks_exact(4)
        .zip(b.rgba.chunks_exact(4))
        .flat_map(|(pa, pb)| {
            let d = |i: usize| pa[i].abs_diff(pb[i]).saturating_mul(8);
            [d(0), d(1), d(2), 255]
        })
        .collect();
    Image { size: a.size, rgba }
}

/// Draws the golden screen `name` full size into `path`.
fn render_screen(
    rt: &tokio::runtime::Runtime,
    name: &str,
    skin_playlist: &mut Option<usize>,
    path: &Path,
) -> anyhow::Result<()> {
    if name == "diagnostics@1x" {
        return capture_diagnostics(rt, path);
    }
    if let Some(shot) = skin_shots().iter().find(|s| s.name == name) {
        let playlist = match *skin_playlist {
            Some(p) => p,
            None => *skin_playlist.insert(long_title_playlist(rt)?),
        };
        return capture_skin(rt, shot, playlist, path);
    }
    let mut shot = shots()
        .into_iter()
        .find(|s| s.name == name)
        .with_context(|| format!("no screen named {name}"))?;
    // A shuffle order is random per run: the golden queue screen shows the list order.
    shot.unshuffled = shot.queue;
    capture(rt, &shot, path)
}

/// A directory a run makes for itself, removed when this is dropped (an early error included).
struct RunScratch(PathBuf);

impl Drop for RunScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// `--check-golden` / `--bless-golden`.
fn golden(rt: &tokio::runtime::Runtime, bless: bool, out: Option<PathBuf>) -> anyhow::Result<()> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("golden");
    let out = out.unwrap_or_else(|| std::env::temp_dir().join("whipspot-golden"));
    // The full-size renders, read back as soon as written, are this run's own: checks run at
    // once (the same default `out`) never read one another's half-written files.
    let renders = out.join(format!("render-{}", std::process::id()));
    let _scratch = RunScratch(renders.clone());
    let mut skin_playlist = None;
    let mut failed: Vec<&str> = Vec::new();
    for name in GOLDEN_SET {
        let full = renders.join(format!("{name}.png"));
        render_screen(rt, name, &mut skin_playlist, &full)?;
        let actual = halve(&read_png(&full)?);
        let golden_path = dir.join(format!("{name}.png"));
        if bless {
            write_image(&golden_path, &actual)?;
            println!("blessed {}", golden_path.display());
            continue;
        }
        let Ok(wanted) = read_png(&golden_path) else {
            println!(
                "golden {name}: missing ({}); run --bless-golden",
                golden_path.display()
            );
            failed.push(name);
            continue;
        };
        let result = compare(&actual, &wanted, usize::MAX, masks_of(name));
        match &result {
            Ok(d) if d.passes() => {
                println!("golden {name}: ok ({d})");
                continue;
            }
            Ok(d) => println!("golden {name}: FAILED ({d})"),
            Err(e) => println!("golden {name}: FAILED ({e})"),
        }
        write_image(&out.join(format!("{name}-actual.png")), &actual)?;
        if result.is_ok() {
            write_image(
                &out.join(format!("{name}-diff.png")),
                &diff_image(&actual, &wanted),
            )?;
        }
        println!(
            "  wrote {name}-actual.png and {name}-diff.png under {}",
            out.display()
        );
        failed.push(name);
    }
    if !bless {
        check_settings_caption(rt, &renders, &mut failed)?;
    }
    anyhow::ensure!(
        failed.is_empty(),
        "golden check failed: {}",
        failed.join(", ")
    );
    if !bless {
        println!("golden ok: {} screens", GOLDEN_SET.len());
    }
    Ok(())
}

/// M9-S7-1: the caption band of the Settings page scrolled by `SETTINGS_SCROLL` equals the
/// unscrolled page's (a glass card scrolled out of the viewport must not blur over it).
fn check_settings_caption(
    rt: &tokio::runtime::Runtime,
    renders: &Path,
    failed: &mut Vec<&str>,
) -> anyhow::Result<()> {
    let plain = read_png(&renders.join("c-settings@1x.png"))?;
    let scrolled_path = renders.join("c-settings-scrolled@1x.png");
    let shot = shots()
        .into_iter()
        .find(|s| s.name == "c-settings-scrolled@1x")
        .context("no scrolled settings shot")?;
    capture(rt, &shot, &scrolled_path)?;
    let scrolled = read_png(&scrolled_path)?;
    let d = compare(&plain, &scrolled, CAPTION_BAND_ROWS, &[])?;
    if d.passes() {
        println!("golden settings caption band, scrolled: ok ({d})");
    } else {
        println!("golden settings caption band, scrolled: FAILED ({d})");
        failed.push("settings caption band");
    }
    Ok(())
}
fn run() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Args { mode, logged_out } = parse(&args)?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .context("tokio runtime")?;
    match mode {
        RunMode::Diagnostics => print_diagnostics(&rt)?,
        RunMode::Golden { bless, out } => golden(&rt, bless, out)?,
        RunMode::SmokeSkin { check_fails } => smoke_skin(&rt, check_fails)?,
        RunMode::SmokeTray { check_fails } => smoke_tray(&rt, check_fails)?,
        RunMode::DumpTray(dir) => dump_tray(&rt, &dir)?,
        RunMode::DumpTrayScratch(dir) => dump_tray_scratch(&rt, &dir)?,
        RunMode::DumpDesktop(dir) => {
            let playlist = long_title_playlist(&rt)?;
            dump_desktops(&rt, playlist, &dir, true)?;
        }
        RunMode::DumpSkin(dir) => dump_skin(&rt, &dir)?,
        RunMode::Dump(dir) => {
            check_idle_path(&rt)?;
            check_idle_rest(&rt)?;
            for shot in shots() {
                capture(&rt, &shot, &dir.join(format!("{}.png", shot.name)))?;
            }
            capture_diagnostics(&rt, &dir.join("diagnostics@1x.png"))?;
            dump_skin(&rt, &dir)?;
            dump_tray(&rt, &dir)?;
        }
        RunMode::Run | RunMode::Smoke => {
            let smoke = matches!(mode, RunMode::Smoke);
            let handle = player(
                &rt,
                Setup {
                    lyrics: true,
                    ..Setup::new(!logged_out)
                },
            )?;
            let mut view = NativeUi::new(host::chrome(), Tab::default());
            let spec = WindowSpec {
                key: "main",
                open_at_start: true,
                title: "Whipspot (demo)".into(),
                size: (f64::from(DUMP_SIZE.0), f64::from(DUMP_SIZE.1)),
                min_size: (760.0, 480.0),
                maximized: false,
                position: None,
            };
            let opts = HostOptions {
                smoke_frames: smoke.then_some(SMOKE_FRAMES),
                system_fonts: !smoke,
                panel_check: None,
                switch_back: false,
            };
            let mut diagnostics = DiagnosticsUi::new(host::chrome());
            let diagnostics_spec = WindowSpec {
                key: DIAGNOSTICS_KEY,
                open_at_start: smoke,
                title: "Audio \u{2014} Whipspot".into(),
                size: (f64::from(DIAGNOSTICS_SIZE.0), f64::from(DIAGNOSTICS_SIZE.1)),
                min_size: (440.0, 480.0),
                maximized: false,
                position: None,
            };
            let report = host::run(
                &handle,
                vec![(spec, &mut view), (diagnostics_spec, &mut diagnostics)],
                None,
                None,
                opts,
            )?;
            if smoke {
                println!("smoke ok: {} frames", report.frames);
            }
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    // As the app does at start-up (D5), so every demo run (the smokes included) proves the
    // window and GPU stack still load with the restricted DLL search path.
    #[cfg(windows)]
    let _ = whipspot_app::platform_windows::restrict_dll_search();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("demo: {e:#}");
            ExitCode::FAILURE
        }
    }
}

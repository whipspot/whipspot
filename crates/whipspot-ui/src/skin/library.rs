//! The skins the user can choose and the one being loaded (D5). Listing and loading run on one
//! worker thread, never on the UI thread: a skin file can be large and its parse slow. The UI
//! polls [`SkinLibrary::status`] and [`SkinLibrary::take_loaded`]; the latest choice wins.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use whipspot_core::skin::{Skin, SkinError, SkinRegistry, SkinSource};

/// The failure text when the loader thread is gone.
pub const LOADER_STOPPED: &str = "the skin loader stopped";
/// The failure text when there is no skins folder.
pub const NO_SKINS_FOLDER: &str = "no skins folder";
/// The failure text when the skin loaded but its windows could not be opened.
pub const OPEN_FAILED: &str = "the skin's windows could not be opened";

/// The core's fixed text of a skin error, as a `&'static str` (the same words as its `Display`).
fn error_text(e: SkinError) -> &'static str {
    match e {
        SkinError::InvalidName => "not a valid skin file name",
        SkinError::NotFound => "skin not found",
        SkinError::NotAllowed => "skin file is not allowed",
        SkinError::TooLarge => "skin file is too large",
        SkinError::Io => "could not read the skin file",
        SkinError::UnknownFormat => "not a skin this app can read",
        SkinError::Malformed => "the skin file is damaged",
        SkinError::Unsupported => "the skin uses a feature this app does not support",
    }
}

/// What the front-end shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkinStatus {
    /// The native look.
    Native,
    /// `name` is being loaded.
    Loading { name: String },
    /// `name` loaded (its model waits in [`SkinLibrary::take_loaded`] until taken).
    Active { name: String },
    /// `name` could not be loaded; `text` is fixed.
    Failed { name: String, text: &'static str },
}

/// A loaded skin. `generation` grows with every successful load.
#[derive(Debug, Clone)]
pub struct LoadedSkin {
    pub generation: u64,
    pub name: String,
    pub skin: Arc<Skin>,
}

enum Job {
    List,
    Load { serial: u64, name: String },
}

struct State {
    status: SkinStatus,
    names: Vec<String>,
    /// Bumped by every `choose` and `choose_native`; a load whose serial is older is dropped.
    serial: u64,
    generation: u64,
    loaded: Option<LoadedSkin>,
    /// List jobs sent and not yet answered.
    listing: usize,
    /// The worker thread is gone (it panicked, or never started).
    dead: bool,
}

impl State {
    /// The worker is gone: whatever waits on it fails.
    fn mark_dead(&mut self) {
        self.dead = true;
        self.listing = 0;
        if let SkinStatus::Loading { name } = &self.status {
            self.status = SkinStatus::Failed {
                name: name.clone(),
                text: LOADER_STOPPED,
            };
        }
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

struct Shared {
    state: Arc<Mutex<State>>,
    source: Option<Arc<dyn SkinSource>>,
    registry: Arc<SkinRegistry>,
    /// The worker's queue, made on first use; `None` inside when the thread could not start.
    jobs: OnceLock<Option<Sender<Job>>>,
}

/// The skins of one source and the loading of one of them. Cheap to clone; clones share it.
#[derive(Clone)]
pub struct SkinLibrary {
    shared: Arc<Shared>,
}

/// Marks the state dead when the worker ends, also by a panic.
struct DeathGuard(Arc<Mutex<State>>);

impl Drop for DeathGuard {
    fn drop(&mut self) {
        lock(&self.0).mark_dead();
    }
}

fn work(
    jobs: Receiver<Job>,
    state: Arc<Mutex<State>>,
    source: Arc<dyn SkinSource>,
    registry: Arc<SkinRegistry>,
) {
    let _guard = DeathGuard(state.clone());
    for job in jobs {
        match job {
            Job::List => {
                let names = source.list();
                let mut s = lock(&state);
                s.names = names;
                s.listing = s.listing.saturating_sub(1);
            }
            Job::Load { serial, name } => {
                if lock(&state).serial != serial {
                    continue;
                }
                let result = registry.load(source.as_ref(), &name);
                let mut s = lock(&state);
                if s.serial != serial {
                    continue;
                }
                match result {
                    Ok(skin) => {
                        s.generation += 1;
                        s.loaded = Some(LoadedSkin {
                            generation: s.generation,
                            name: name.clone(),
                            skin: Arc::new(skin),
                        });
                        s.status = SkinStatus::Active { name };
                    }
                    Err(e) => {
                        s.status = SkinStatus::Failed {
                            name,
                            text: error_text(e),
                        };
                    }
                }
            }
        }
    }
}

impl SkinLibrary {
    /// A library over `source` (`None` when there is no skins folder). No thread starts until
    /// the first `refresh` or `choose`.
    pub fn new(source: Option<Arc<dyn SkinSource>>, registry: Arc<SkinRegistry>) -> Self {
        SkinLibrary {
            shared: Arc::new(Shared {
                state: Arc::new(Mutex::new(State {
                    status: SkinStatus::Native,
                    names: Vec::new(),
                    serial: 0,
                    generation: 0,
                    loaded: None,
                    listing: 0,
                    dead: false,
                })),
                source,
                registry,
                jobs: OnceLock::new(),
            }),
        }
    }

    /// Queues `job` on the worker, starting it on first use. `false` when the worker is gone
    /// (the state is then marked dead).
    fn send(&self, job: Job) -> bool {
        let sh = &self.shared;
        let Some(source) = sh.source.clone() else {
            return false;
        };
        let jobs = sh.jobs.get_or_init(|| {
            let (tx, rx) = channel();
            let (state, registry) = (sh.state.clone(), sh.registry.clone());
            std::thread::Builder::new()
                .name("skin-loader".to_owned())
                .spawn(move || work(rx, state, source, registry))
                .ok()
                .map(|_| tx)
        });
        let sent = jobs.as_ref().is_some_and(|tx| tx.send(job).is_ok());
        if !sent {
            lock(&sh.state).mark_dead();
        }
        sent
    }

    /// Lists the source again (on the worker); `names` changes when it answers.
    pub fn refresh(&self) {
        if self.shared.source.is_none() {
            return;
        }
        lock(&self.shared.state).listing += 1;
        self.send(Job::List);
    }

    /// The skin names last listed; empty before the first answer or without a source.
    pub fn names(&self) -> Vec<String> {
        lock(&self.shared.state).names.clone()
    }

    /// Loads `name` (on the worker); it supersedes any choice still loading.
    pub fn choose(&self, name: &str) {
        let serial = {
            let mut s = lock(&self.shared.state);
            s.serial += 1;
            s.loaded = None;
            if s.dead {
                s.status = SkinStatus::Failed {
                    name: name.to_owned(),
                    text: LOADER_STOPPED,
                };
                return;
            }
            if self.shared.source.is_none() {
                s.status = SkinStatus::Failed {
                    name: name.to_owned(),
                    text: NO_SKINS_FOLDER,
                };
                return;
            }
            s.status = SkinStatus::Loading {
                name: name.to_owned(),
            };
            s.serial
        };
        self.send(Job::Load {
            serial,
            name: name.to_owned(),
        });
    }

    /// Back to the native look; a load still running is dropped when it ends.
    pub fn choose_native(&self) {
        let mut s = lock(&self.shared.state);
        s.serial += 1;
        s.loaded = None;
        s.status = SkinStatus::Native;
    }

    /// The skin that loaded could not be shown: the status becomes `Failed` with `text` (fixed),
    /// and a result still waiting is dropped.
    pub(crate) fn fail_active(&self, text: &'static str) {
        let mut s = lock(&self.shared.state);
        s.serial += 1;
        s.loaded = None;
        let name = match &s.status {
            SkinStatus::Active { name } | SkinStatus::Loading { name } => name.clone(),
            SkinStatus::Failed { name, .. } => name.clone(),
            SkinStatus::Native => return,
        };
        s.status = SkinStatus::Failed { name, text };
    }

    pub fn status(&self) -> SkinStatus {
        lock(&self.shared.state).status.clone()
    }

    /// The skin that finished loading, once.
    pub fn take_loaded(&self) -> Option<LoadedSkin> {
        lock(&self.shared.state).loaded.take()
    }

    /// A finished load waits in [`SkinLibrary::take_loaded`].
    pub fn has_loaded(&self) -> bool {
        lock(&self.shared.state).loaded.is_some()
    }

    /// A load or a listing is still running.
    pub fn pending(&self) -> bool {
        let s = lock(&self.shared.state);
        !s.dead && (matches!(s.status, SkinStatus::Loading { .. }) || s.listing > 0)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver, Sender};
    use std::time::{Duration, Instant};

    use whipspot_core::skin::{
        Align, Anchor, Dock, Fill, ImageId, Panel, PanelId, Rect, Shape, Size, SkinEngine,
        SkinImage, SkinInfo, SkinTheme, SpriteRect, Widget, WidgetKind,
    };

    use super::*;

    /// A skin that passes `validate`.
    fn valid_skin() -> Skin {
        let rect = Rect {
            x: 0,
            y: 0,
            w: 8,
            h: 8,
        };
        Skin {
            info: SkinInfo {
                engine: String::new(),
                name: String::new(),
            },
            panels: vec![Panel {
                id: PanelId::new("main").unwrap(),
                title: "Main".to_owned(),
                size: Size { w: 8, h: 8 },
                resize: None,
                shape: Shape::Rect,
                dock: Dock::default(),
                visible_at_start: true,
                widgets: vec![Widget {
                    rect,
                    anchor: Anchor {
                        h: Align::Start,
                        v: Align::Start,
                    },
                    kind: WidgetKind::Image {
                        sprite: SpriteRect {
                            image: ImageId(0),
                            rect,
                        },
                        fill: Fill::Once,
                    },
                }],
            }],
            images: vec![SkinImage::new(8, 8, vec![0; 8 * 8 * 4]).unwrap()],
            fonts: Vec::new(),
            visualiser: None,
            theme: SkinTheme::default(),
        }
    }

    /// Bytes `b"ok"` load, `b"bad"` are malformed, `b"boom"` panic.
    struct TestEngine;

    impl SkinEngine for TestEngine {
        fn id(&self) -> &'static str {
            "test"
        }
        fn extensions(&self) -> &'static [&'static str] {
            &["tsk"]
        }
        fn detect(&self, _bytes: &[u8]) -> bool {
            true
        }
        fn load(&self, bytes: &[u8]) -> Result<Skin, SkinError> {
            match bytes {
                b"ok" => Ok(valid_skin()),
                b"boom" => panic!("test engine panic"),
                _ => Err(SkinError::Malformed),
            }
        }
    }

    /// Names: "a.tsk" (ok), "bad.tsk", "boom.tsk", "slow.tsk" (ok after the gate opens; says
    /// when it has started).
    struct TestSource {
        started: Mutex<Sender<()>>,
        gate: Mutex<Receiver<()>>,
    }

    impl SkinSource for TestSource {
        fn list(&self) -> Vec<String> {
            ["a.tsk", "bad.tsk", "boom.tsk", "slow.tsk"]
                .map(str::to_owned)
                .to_vec()
        }
        fn read(&self, name: &str) -> Result<Vec<u8>, SkinError> {
            match name {
                "a.tsk" => Ok(b"ok".to_vec()),
                "bad.tsk" => Ok(b"bad".to_vec()),
                "boom.tsk" => Ok(b"boom".to_vec()),
                "slow.tsk" => {
                    let _ = self.started.lock().unwrap().send(());
                    let _ = self
                        .gate
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(10));
                    Ok(b"ok".to_vec())
                }
                _ => Err(SkinError::NotFound),
            }
        }
    }

    /// A library, the receiver of "slow started" and the sender that opens the slow gate.
    fn library() -> (SkinLibrary, Receiver<()>, Sender<()>) {
        let (started_tx, started_rx) = channel();
        let (gate_tx, gate_rx) = channel();
        let source = TestSource {
            started: Mutex::new(started_tx),
            gate: Mutex::new(gate_rx),
        };
        let mut registry = SkinRegistry::new();
        registry.register(Arc::new(TestEngine)).unwrap();
        let lib = SkinLibrary::new(Some(Arc::new(source)), Arc::new(registry));
        (lib, started_rx, gate_tx)
    }

    fn wait_until(lib: &SkinLibrary, done: impl Fn(&SkinLibrary) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done(lib) {
            assert!(Instant::now() < deadline, "timed out: {:?}", lib.status());
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn lists_and_loads() {
        let (lib, _, _) = library();
        assert!(lib.names().is_empty());
        assert_eq!(lib.status(), SkinStatus::Native);
        lib.refresh();
        wait_until(&lib, |l| !l.pending());
        assert_eq!(lib.names().len(), 4);
        lib.choose("a.tsk");
        wait_until(&lib, |l| !l.pending());
        assert_eq!(
            lib.status(),
            SkinStatus::Active {
                name: "a.tsk".to_owned()
            }
        );
        let loaded = lib.take_loaded().expect("loaded");
        assert_eq!(loaded.generation, 1);
        assert_eq!(loaded.name, "a.tsk");
        assert_eq!(loaded.skin.panels.len(), 1);
        assert!(lib.take_loaded().is_none());
        // The clone shares the state; another load bumps the generation.
        let clone = lib.clone();
        clone.choose("a.tsk");
        wait_until(&lib, |l| !l.pending());
        assert_eq!(lib.take_loaded().map(|l| l.generation), Some(2));
    }

    /// The front-end stays pending (the host keeps polling) from the choice until the host has
    /// taken each change: a finished load waiting, and the native look chosen while a skin runs.
    #[test]
    fn the_front_end_stays_pending_until_the_host_takes_the_change() {
        use crate::skin::{FrontChange, SkinPrefs, SkinUi};
        let (lib, _, _) = library();
        let mut ui = SkinUi::new(lib.clone(), SkinPrefs::default());
        assert!(!ui.pending());
        lib.choose("a.tsk");
        assert!(ui.pending());
        wait_until(&lib, |l| !l.pending());
        assert!(lib.has_loaded());
        assert!(ui.pending(), "a finished load waits for take_change");
        assert!(matches!(ui.take_change(), Some(FrontChange::Panels(_))));
        assert!(!ui.pending());
        lib.choose_native();
        assert!(ui.pending(), "the native look waits for take_change");
        assert!(matches!(ui.take_change(), Some(FrontChange::Native)));
        assert!(!ui.pending());
    }

    #[test]
    fn latest_choice_wins_over_a_slow_earlier_one() {
        let (lib, started, gate) = library();
        lib.choose("slow.tsk");
        started.recv_timeout(Duration::from_secs(10)).unwrap();
        lib.choose("a.tsk");
        assert!(lib.pending());
        gate.send(()).unwrap();
        wait_until(&lib, |l| !l.pending());
        assert_eq!(
            lib.status(),
            SkinStatus::Active {
                name: "a.tsk".to_owned()
            }
        );
        let loaded = lib.take_loaded().expect("loaded");
        assert_eq!(loaded.name, "a.tsk");
        assert_eq!(loaded.generation, 1);
    }

    #[test]
    fn failure_gives_the_fixed_text() {
        let (lib, _, _) = library();
        lib.choose("bad.tsk");
        wait_until(&lib, |l| !l.pending());
        assert_eq!(
            lib.status(),
            SkinStatus::Failed {
                name: "bad.tsk".to_owned(),
                text: "the skin file is damaged"
            }
        );
        lib.choose("gone.tsk");
        wait_until(&lib, |l| !l.pending());
        assert!(matches!(
            lib.status(),
            SkinStatus::Failed {
                text: "skin not found",
                ..
            }
        ));
        assert!(lib.take_loaded().is_none());
    }

    #[test]
    fn choose_native_while_loading_stays_native() {
        let (lib, started, gate) = library();
        lib.choose("slow.tsk");
        started.recv_timeout(Duration::from_secs(10)).unwrap();
        lib.choose_native();
        assert_eq!(lib.status(), SkinStatus::Native);
        gate.send(()).unwrap();
        // The slow load finishes and is dropped; a later list answer shows the worker moved on.
        lib.refresh();
        wait_until(&lib, |l| !l.pending());
        assert_eq!(lib.status(), SkinStatus::Native);
        assert!(lib.take_loaded().is_none());
    }

    #[test]
    fn a_panicked_worker_fails_the_load() {
        let (lib, _, _) = library();
        lib.choose("boom.tsk");
        wait_until(&lib, |l| !l.pending());
        assert_eq!(
            lib.status(),
            SkinStatus::Failed {
                name: "boom.tsk".to_owned(),
                text: LOADER_STOPPED
            }
        );
        // Later choices fail at once too.
        lib.choose("a.tsk");
        wait_until(&lib, |l| !l.pending());
        assert!(matches!(
            lib.status(),
            SkinStatus::Failed {
                text: LOADER_STOPPED,
                ..
            }
        ));
    }

    #[test]
    fn without_a_source_nothing_lists_and_choose_fails() {
        let lib = SkinLibrary::new(None, Arc::new(SkinRegistry::new()));
        lib.refresh();
        assert!(lib.names().is_empty());
        assert!(!lib.pending());
        lib.choose("a.tsk");
        assert_eq!(
            lib.status(),
            SkinStatus::Failed {
                name: "a.tsk".to_owned(),
                text: NO_SKINS_FOLDER
            }
        );
    }

    #[test]
    fn error_texts_match_the_core() {
        for e in [
            SkinError::InvalidName,
            SkinError::NotFound,
            SkinError::NotAllowed,
            SkinError::TooLarge,
            SkinError::Io,
            SkinError::UnknownFormat,
            SkinError::Malformed,
            SkinError::Unsupported,
        ] {
            assert_eq!(error_text(e), e.to_string());
        }
    }
}

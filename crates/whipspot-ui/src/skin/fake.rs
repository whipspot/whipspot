//! Test doubles for the skin front-end (whipspot-ui cannot use the testkit): panel windows on a
//! fake desktop and panel builders.

use std::sync::Arc;
use std::time::Instant;

use whipspot_core::engine::BAND_COUNT;
use whipspot_core::player::{
    EqState, LyricsState, NowPlaying, OutputStatus, PlayState, PlayerState, QueueState, RepeatMode,
};
use whipspot_core::provider::Track;
use whipspot_core::skin::{
    Attach, Dock, Edge, Panel, PanelId, PanelWindows, PanelWindowsError, ScreenPoint, ScreenRect,
    Shape, Size,
};

/// A panel with no widgets, optionally attached to `to`'s `edge`.
pub(crate) fn panel(id: &str, w: u32, h: u32, attach: Option<(&str, Edge)>) -> Panel {
    Panel {
        id: PanelId::new(id).unwrap(),
        title: id.to_owned(),
        size: Size { w, h },
        resize: None,
        shape: Shape::Rect,
        dock: Dock {
            group: Some("main".to_owned()),
            attach: attach.map(|(to, edge)| Attach {
                to: PanelId::new(to).unwrap(),
                edge,
            }),
            snap: true,
        },
        visible_at_start: true,
        widgets: Vec::new(),
    }
}

/// Panel windows on a fake desktop: frames move exactly, every call is recorded.
pub(crate) struct FakeWindows {
    pub frames: Vec<ScreenRect>,
    pub visible: Vec<bool>,
    pub docked: Vec<bool>,
    pub areas: Vec<ScreenRect>,
    pub pointer: Option<ScreenPoint>,
    pub units_per_pixel: f32,
    /// Every batch passed to `move_panels`.
    pub moves: Vec<Vec<(usize, ScreenPoint)>>,
    /// Every accepted `resize_panel` call.
    pub resizes: Vec<(usize, i32, i32)>,
    pub on_top: bool,
}

impl FakeWindows {
    pub fn new(frames: Vec<ScreenRect>) -> Self {
        let n = frames.len();
        FakeWindows {
            frames,
            visible: vec![true; n],
            docked: vec![false; n],
            areas: vec![ScreenRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1040,
            }],
            pointer: None,
            units_per_pixel: 1.0,
            moves: Vec::new(),
            resizes: Vec::new(),
            on_top: false,
        }
    }

    pub fn origin(&self, panel: usize) -> (i32, i32) {
        (self.frames[panel].x, self.frames[panel].y)
    }
}

impl PanelWindows for FakeWindows {
    fn count(&self) -> usize {
        self.frames.len()
    }
    fn frame(&self, panel: usize) -> Option<ScreenRect> {
        self.frames.get(panel).copied()
    }
    fn is_visible(&self, panel: usize) -> bool {
        self.visible.get(panel).copied().unwrap_or(false)
    }
    fn work_areas(&self) -> Vec<ScreenRect> {
        self.areas.clone()
    }
    fn pointer(&self) -> Option<ScreenPoint> {
        self.pointer
    }
    fn units_per_pixel(&self) -> f32 {
        self.units_per_pixel
    }
    fn move_panels(&mut self, moves: &[(usize, ScreenPoint)]) -> Result<(), PanelWindowsError> {
        if moves.iter().any(|&(i, _)| i >= self.frames.len()) {
            return Err(PanelWindowsError::Failed);
        }
        for &(i, p) in moves {
            self.frames[i].x = p.x;
            self.frames[i].y = p.y;
        }
        self.moves.push(moves.to_vec());
        Ok(())
    }
    fn resize_panel(&mut self, panel: usize, w: i32, h: i32) -> Result<(), PanelWindowsError> {
        match self.frames.get_mut(panel) {
            Some(frame) if w > 0 && h > 0 => {
                frame.w = w;
                frame.h = h;
                self.resizes.push((panel, w, h));
                Ok(())
            }
            _ => Err(PanelWindowsError::Failed),
        }
    }
    fn set_visible(&mut self, panel: usize, visible: bool) {
        if let Some(v) = self.visible.get_mut(panel) {
            *v = visible;
        }
    }
    fn set_docked(&mut self, panel: usize, docked: bool) {
        if let Some(d) = self.docked.get_mut(panel) {
            *d = docked;
        }
    }
    fn set_always_on_top(&mut self, on: bool) {
        self.on_top = on;
    }
    fn minimise(&mut self) {}
}

/// A stopped player at volume 50 with nothing current.
pub(crate) fn player_state() -> PlayerState {
    PlayerState {
        revision: 0,
        play_state: PlayState::Stopped,
        current: None,
        position_ms: 0,
        position_at: Instant::now(),
        volume: 50,
        balance: 0,
        replay_gain: Default::default(),
        eq: EqState {
            enabled: false,
            preamp_db: 0.0,
            bands_db: [0.0; BAND_COUNT],
        },
        shuffle: false,
        repeat: RepeatMode::Off,
        queue: QueueState {
            items: Arc::new(Vec::new()),
            current: None,
            next: None,
        },
        providers: Vec::new(),
        output_summary: String::new(),
        output: OutputStatus::default(),
        lyrics: LyricsState::Off,
        lyrics_enabled: false,
        remote: None,
        power: Default::default(),
        update: Default::default(),
        app_version: String::new(),
        update_check: true,
    }
}

/// `state` paused with a current track of `duration_ms`.
pub(crate) fn with_current(mut state: PlayerState, duration_ms: u64) -> PlayerState {
    state.play_state = PlayState::Paused;
    state.current = Some(NowPlaying {
        track: Track {
            uri: "test:1".to_owned(),
            name: "Title".to_owned(),
            artist: "Artist".to_owned(),
            album: String::new(),
            duration_ms,
            images: Vec::new(),
            year: None,
            album_ref: None,
            artists: vec![],
        },
        provider: "test",
        source_format: None,
    });
    state
}

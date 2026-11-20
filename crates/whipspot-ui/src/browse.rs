//! The browse sheet's content: the top bar, the tabs and the routed pages, with the explorer
//! history (MD54). Home and the collection grids are `grid.rs`; a playlist, album, liked, search
//! or queue route draws a track list. Interface fixed by M5 step 5.

use std::time::Duration;

use lune_ui::paint::Rect;
use lune_ui::scene::LayerId;
use lune_ui::ui::Ui;
use lune_ui::widgets::button::ButtonKind;
use whipspot_core::player::{PlayerAction, PlayerHandle, PlayerNotice, PlayerState, ProviderEntry};
use whipspot_core::provider::{Album, EntityRef, Playlist, Readable, Track};

use crate::Anchors;
use crate::account::{self, AccountView};
use crate::anim;
use crate::art::ArtCache;
use crate::artist::{self, AlbumArgs, ArtistView};
use crate::equalizer::EqualizerPane;
use crate::grid::{self, Art, CardSpec, Covers, HomeOut, Page, PageOut, Section, Status};
use crate::history::{History, Route, Tab, tab_of};
use crate::layout;
use crate::library::{
    Access, LibraryAnswer, LibraryModel, ListKind, PrefillAnswer, ProviderChoice, Slot,
    library_entry,
};
use crate::likes::LikeBook;
use crate::pages::{self, FilterCache, Hero, HeroOut, Save};
use crate::queue::QueuePane;
use crate::scroll_memory::{self, Restore, Scrolled, Step};
use crate::search;
use crate::settings::{SettingsOut, SettingsPane, SettingsView};
use crate::theme::Look;
use crate::topbar::{Switch, TopBar, TopState};
use crate::tracks::{Form, TrackTable, note, play_from, play_visible};
use crate::{Force, Hover};

/// Most uris of a loaded page the like book is asked about per frame (the book itself sends at
/// most this many per action).
const WANT_CHUNK: usize = 50;

/// The fixed text of a playlist route the provider refuses (D48); it names no provider.
pub const REFUSED_TEXT: &str =
    "This playlist can't be opened: its music service does not let this app read it.";
/// The refusal text wraps within `min(REFUSED_W, area width - 2 * REFUSED_MARGIN)`: one line at
/// every window size the shell allows (the text is under 500 px at 14 px).
const REFUSED_W: f32 = 560.0;
const REFUSED_MARGIN: f32 = 24.0;
/// The Retry button: the size of the account view's Log in.
const RETRY_SIZE: (f32, f32) = (104.0, 32.0);
/// The track pages' header: its height below the content top, and the table's inset.
const TRACKS_TOP: f32 = 64.0;
const TABLE_INSET: f32 = 12.0;

/// The sheet's retained state: the history, the library lists and the account view.
#[derive(Debug)]
pub struct Browse {
    history: History,
    library: LibraryModel,
    /// The library provider the user chose (shared with the skinned front-end).
    choice: ProviderChoice,
    account: AccountView,
    settings: SettingsPane,
    equalizer: EqualizerPane,
    queue: QueuePane,
    top: TopBar,
    /// Where the top bar's controls were drawn in the last frame.
    anchors: Anchors,
    /// The last query `search` was given; the library runs it once a provider is usable.
    pending_query: Option<String>,
    /// A play disc was pressed: this route plays its first track once its first page is in.
    play_when_loaded: Option<Route>,
    pane: TrackTable,
    /// The artist page's state (the selected chip, its Popular table).
    artist: ArtistView,
    /// A Save to library was pressed: the albums list loads again once the write is done.
    album_saved: bool,
    /// The filter bar's text, the route it belongs to (it resets when the route changes) and
    /// whether it had keyboard focus in the last frame.
    filter: String,
    filter_for: Option<Route>,
    filter_focused: bool,
    /// How much of the list on show the like book has been told about.
    feed: Feed,
    /// The track page's visible rows, kept while nothing they depend on changes (M5-F6).
    visible: FilterCache,
    /// The route drawn last frame and when it changed to the one on show (a page change fades
    /// the page in; the first route does not).
    shown: Option<Route>,
    changed: Option<Duration>,
    /// A restore of the page's scroll in progress after a back or forward (MD202).
    restore: Option<Restore>,
    /// The offset the views are asked to reveal this frame (the restore's), and what the page on
    /// show reported of its scroll in this frame.
    want: Option<f64>,
    scrolled: Option<Scrolled>,
}

/// What a page draws against in one frame.
#[derive(Clone, Copy)]
pub(crate) struct Show<'a> {
    pub(crate) look: &'a Look,
    pub(crate) state: &'a PlayerState,
    pub(crate) player: &'a PlayerHandle,
    /// `Some(writable)` when the library provider has likes (the hearts are drawn), else `None`.
    pub(crate) hearts: Option<bool>,
}

/// How far the like book has been fed from one list (named by its serial).
#[derive(Debug, Default)]
pub(crate) struct Feed {
    serial: u64,
    /// Rows already given to `LikeBook::want` or, for the Liked list, noted as liked.
    done: usize,
}

impl Browse {
    /// A sheet that opens on `start`'s root route.
    pub fn new(start: Tab) -> Self {
        Browse {
            history: History::new(start.root()),
            library: LibraryModel::default(),
            choice: ProviderChoice::default(),
            account: AccountView::default(),
            settings: SettingsPane::default(),
            // A unit struct for now: step 3 gives the pane its state.
            equalizer: Default::default(),
            queue: QueuePane::default(),
            top: TopBar::default(),
            anchors: Anchors::default(),
            pending_query: None,
            play_when_loaded: None,
            pane: TrackTable::default(),
            artist: ArtistView::default(),
            album_saved: false,
            filter: String::new(),
            filter_for: None,
            filter_focused: false,
            feed: Feed::default(),
            visible: FilterCache::default(),
            shown: None,
            changed: None,
            restore: None,
            want: None,
            scrolled: None,
        }
    }

    /// Shares the library provider choice with the rest of the app (the host saves it).
    pub fn with_choice(mut self, choice: ProviderChoice) -> Self {
        self.choice = choice;
        self
    }

    /// The tab the route on show belongs to (the Home tab for the routes that belong to none).
    pub fn tab(&self) -> Tab {
        tab_of(self.history.current()).unwrap_or_default()
    }

    /// Opens `route` (a push: back returns to the one before). The Search route lands with its
    /// field focused.
    pub fn navigate(&mut self, route: Route) {
        if route == Route::Search {
            self.top.focus();
        }
        self.history.push(route);
    }

    /// Shows the library of provider `id`: the choice is kept, and a page that belongs to the old
    /// provider's library (an opened playlist or album) gives way to its tab's root, since the
    /// new provider does not know its id. The lists restart in the next frame (`sync`).
    fn switch_to(&mut self, id: &str) {
        self.choice.set(id);
        self.play_when_loaded = None;
        if matches!(
            self.history.current(),
            Route::Playlist { .. } | Route::Album { .. } | Route::Artist { .. }
        ) {
            let root = self.tab().root();
            self.history.push(root);
        }
    }

    /// Opens what a clicked artist or album link names: the library switches to the link's
    /// provider first when another one shows (D6), then the page is pushed. A link whose
    /// provider cannot explore, or that names neither an artist nor an album, does nothing.
    pub(crate) fn open_ref(&mut self, r: &EntityRef, providers: &[ProviderEntry]) {
        let Some(route) = link_route(r) else {
            return;
        };
        let scheme = r.uri.split_once(':').map_or("", |(s, _)| s);
        let Some(entry) = providers
            .iter()
            .find(|p| p.info.id == scheme && p.info.caps.library && p.info.caps.explore)
        else {
            return;
        };
        // The page already on show: nothing to open.
        let on_show = match (self.history.current(), &route) {
            (Route::Artist { uri: a, .. }, Route::Artist { uri: b, .. }) => a == b,
            (cur @ Route::Album { .. }, Route::Album { uri, .. }) => cur.album_key() == Some(uri),
            _ => false,
        };
        if on_show {
            return;
        }
        let selected = self.choice.get();
        let shown = library_entry(providers, selected.as_deref()).map(|e| e.info.id);
        if shown != Some(entry.info.id) {
            self.switch_to(entry.info.id);
        }
        self.navigate(route);
    }

    /// Goes back one route; whether there was one.
    pub fn back(&mut self) -> bool {
        self.history.back()
    }

    /// Goes forward one route; whether there was one.
    pub fn forward(&mut self) -> bool {
        self.history.forward()
    }

    /// Shows the Search route and runs `query` there.
    pub fn search(&mut self, query: &str) {
        self.top.set_text(query);
        self.pending_query = Some(query.to_owned());
        self.history.push(Route::Search);
    }

    /// Takes the notices this sheet consumes: library pages and prefills go to the library
    /// model, a login result goes to the account view (and comes back, so the shell toasts it);
    /// everything else comes back untouched.
    pub fn route_notice(&mut self, notice: PlayerNotice) -> Option<PlayerNotice> {
        match notice {
            PlayerNotice::Prefill { request, value } => {
                self.library.push_prefill(PrefillAnswer { request, value });
                None
            }
            PlayerNotice::LibraryPage { request, result } => {
                self.library.push_page(LibraryAnswer { request, result });
                None
            }
            // Every provider's lists refresh the same way: a written like (the core sends it for
            // any provider) reloads Liked, a change the provider reports reloads them all.
            PlayerNotice::LikesChanged { provider } => {
                self.library.changed(&provider, true);
                if std::mem::take(&mut self.album_saved) {
                    self.library.refresh_albums(&provider);
                }
                None
            }
            PlayerNotice::LibraryChanged { provider } => {
                self.library.changed(&provider, false);
                None
            }
            PlayerNotice::LoginResult {
                ref provider,
                ref step,
                ok,
            } => {
                self.account.login_result(provider, step, ok);
                Some(notice)
            }
            other => Some(other),
        }
    }

    /// Whether an Escape press belongs to nobody on the sheet: no text field has focus and no
    /// menu, dropdown or drag of the sheet could take it (as of the last frame). The toolkit
    /// leaves a handled Escape in the frame's events and has no signal for an open overlay, so
    /// the pages that open dropdowns, menus or drags (Settings, Equalizer, Queue) never count.
    pub(crate) fn escape_free(&self) -> bool {
        !self.text_focused()
            && !self.top.menu_open()
            && !matches!(
                self.history.current(),
                Route::Settings | Route::Equalizer | Route::Queue
            )
    }

    /// Whether the top bar's provider menu is open now (as of this frame's top bar).
    pub(crate) fn provider_menu_open(&self) -> bool {
        self.top.menu_open()
    }

    /// Whether a text field of the sheet had keyboard focus in the last frame.
    pub fn text_focused(&self) -> bool {
        self.top.focused() || self.account.focused() || self.filter_focused
    }

    /// Where the top bar's controls were drawn in the last frame (the provider button is `None`
    /// when there is no switch).
    pub(crate) fn anchors(&self) -> Anchors {
        self.anchors
    }

    /// Draws the sheet's content in `rect` (the sheet's glass is the shell's). `settings` is what
    /// the settings page shows of the shell and `force` the demo's forced states; the page's
    /// requests come back for the shell to apply.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn frame(
        &mut self,
        ui: &mut Ui<'_>,
        rect: Rect,
        look: &Look,
        state: &PlayerState,
        player: &PlayerHandle,
        art: &mut ArtCache,
        likes: &mut LikeBook,
        (settings, force): (&SettingsView, &Force),
    ) -> SettingsOut {
        let selected = self.choice.get();
        let access = Access::of(&state.providers, selected.as_deref());
        self.library.sync(access.provider());
        self.library.pump(player);
        if self.library.provider().is_some()
            && let Some(query) = self.pending_query.take()
        {
            self.library.start_search(&query);
        }
        ui.painter().on(LayerId::Popup);

        let out = self.top.frame(
            ui,
            rect,
            look,
            TopState {
                tab: tab_of(self.history.current()),
                can_back: self.history.can_back(),
                can_forward: self.history.can_forward(),
                auto_hide: settings.auto_hide,
                switch: library_entry(&state.providers, selected.as_deref())
                    .and_then(|e| Switch::of(&state.providers, e.info.id)),
                menu_forced: force.hover == Some(crate::Hover::ProviderMenu),
            },
        );
        self.anchors = Anchors {
            provider: out.provider_rect,
            gear: Some(out.gear_rect),
            eye: Some(out.hide_rect),
            ..Anchors::default()
        };
        if let Some(id) = out.provider {
            self.switch_to(id);
            ui.request_repaint_at(ui.time() + crate::FADE_FRAME);
        }
        if out.back {
            self.history.back();
        } else if out.forward {
            self.history.forward();
        } else if let Some(tab) = out.tab {
            self.navigate(tab.root());
        } else if out.settings {
            self.navigate(Route::Settings);
        }
        if let Some(query) = out.submit {
            self.library.start_search(&query);
            self.pending_query = None;
            self.history.push(Route::Search);
        }

        let now = ui.time();
        let (arrived, drawn_at) = self.begin_scroll(now);

        let route = self.history.current().clone();
        if route != Route::Account {
            self.account.leave();
        }
        let body = Rect::new(rect.x, rect.y + 72.0, rect.w, (rect.h - 72.0).max(0.0));
        let c = grid::content_rect(rect);
        // The page area of the panes that need no library: the content card with its heading slot.
        let area = Rect::new(
            c.x - layout::CONTENT_PAD,
            c.y - layout::HEADING_TOP,
            c.w + 2.0 * layout::CONTENT_PAD,
            c.h + layout::HEADING_TOP,
        );
        // A page change fades the new page in (and slides it up a few px).
        match &self.shown {
            Some(was) if *was == route => {}
            Some(_) => {
                self.shown = Some(route.clone());
                self.changed = Some(now);
            }
            None => self.shown = Some(route.clone()),
        }
        let enter = anim::enter(self.changed, now, anim::PAGE);
        if anim::running(self.changed, now, anim::PAGE) {
            ui.request_repaint_at(now + crate::FADE_FRAME);
        }
        let settings_out = anim::page(ui, enter, |ui| {
            let mut settings_out = SettingsOut {
                toggle_auto_hide: out.auto_hide,
                ..SettingsOut::default()
            };
            match (&route, access) {
                (Route::Account, _) => {
                    self.account
                        .frame(ui, area, look, state, &mut self.library, player);
                }
                (Route::Settings, _) => {
                    settings_out = SettingsOut {
                        toggle_auto_hide: settings_out.toggle_auto_hide,
                        ..self.settings.frame(ui, area, look, settings, state, player)
                    };
                }
                (Route::Equalizer, _) => {
                    self.equalizer
                        .frame(ui, area, look, state, player, force.eq_meters);
                }
                (_, Access::NoProvider) => note(ui, body, "No library provider"),
                (_, Access::Checking) => note(ui, body, "Checking your login\u{2026}"),
                (_, Access::LoggedOut) => {
                    if account::login_prompt(ui, body, look) {
                        self.navigate(Route::Account);
                    }
                }
                (_, Access::Ready(id)) => {
                    let mut covers = Covers {
                        cache: art,
                        pull: player,
                    };
                    let hearts = hearts_of(&state.providers, id);
                    let on = Show {
                        look,
                        state,
                        player,
                        hearts,
                    };
                    self.library_route(ui, rect, &route, &on, &mut covers, likes, force);
                }
            }
            settings_out
        });
        self.settle_play(player);
        self.play_release(ui.time(), player);
        self.settle_scroll(ui, rect, arrived, drawn_at);
        settings_out
    }

    /// Starts the frame's scroll memory: a back or forward that arrived at a scrolled entry begins
    /// a restore, and one pending from before a push, back or forward is dropped (it belonged to
    /// another page). Sets what the views are asked for and clears what they reported. Returns
    /// whether a restore began now and the history generation the frame draws.
    fn begin_scroll(&mut self, now: Duration) -> (bool, u64) {
        if self
            .restore
            .is_some_and(|r| r.generation != self.history.generation())
        {
            self.restore = None;
        }
        let arrived = match self.history.take_arrival() {
            Some(y) => {
                self.restore = Some(Restore {
                    y,
                    since: now,
                    generation: self.history.generation(),
                });
                true
            }
            None => false,
        };
        self.want = self.restore.map(|r| r.y);
        self.scrolled = None;
        (arrived, self.history.generation())
    }

    /// Ends the frame's scroll memory: a restore in progress steps (and ends when the page is
    /// there, cannot go further, the user took over or the time is up); otherwise the page's
    /// offset goes into its history entry. Nothing is written while a restore is pending, and
    /// nothing when the frame navigated (the next frame draws the new entry).
    fn settle_scroll(&mut self, ui: &mut Ui<'_>, sheet: Rect, arrived: bool, drawn_at: u64) {
        let scrolled = self.scrolled.take();
        if self.history.generation() != drawn_at {
            return;
        }
        let now = ui.time();
        match self.restore {
            Some(r) => {
                // The press that went back is not the user taking over.
                let moved = !arrived && scroll_memory::user_moved(ui, sheet);
                if scroll_memory::step(&r, scrolled, now, moved) == Step::Done {
                    self.restore = None;
                } else {
                    ui.request_repaint_at(now + crate::FADE_FRAME);
                }
            }
            None => {
                if let Some(s) = scrolled {
                    self.history.set_scroll(s.offset);
                }
            }
        }
    }

    /// Plays the release a Play on the artist page asked for, once its tracks are in.
    fn play_release(&mut self, now: Duration, player: &PlayerHandle) {
        let Some((tracks, shuffle)) = self.library.take_play() else {
            return;
        };
        let index = if shuffle { pick(now, tracks.len()) } else { 0 };
        if shuffle {
            player.dispatch(PlayerAction::SetShuffle(true));
        }
        player.dispatch(PlayerAction::PlayItem {
            context: tracks,
            index,
        });
    }

    /// The page of a library route while the library is usable.
    #[allow(clippy::too_many_arguments)]
    fn library_route(
        &mut self,
        ui: &mut Ui<'_>,
        sheet: Rect,
        route: &Route,
        on: &Show<'_>,
        covers: &mut Covers<'_>,
        likes: &mut LikeBook,
        force: &Force,
    ) {
        let (vp, c) = (grid::viewport(sheet), grid::content_rect(sheet));
        match route {
            Route::Home => self.home(ui, vp, c, on, covers),
            Route::Playlists => self.playlists(ui, vp, c, on, covers),
            Route::Albums => self.albums(ui, vp, c, on, covers),
            Route::Artist { uri, name, images } => {
                let at = (uri.as_str(), name.as_str(), images.as_slice());
                self.artist_page(ui, sheet, at, force.group, on, covers, likes);
            }
            Route::Queue => {
                if self.filter_for.as_ref() != Some(route) {
                    self.filter.clear();
                    self.filter_for = Some(route.clone());
                }
                let hover = match force.hover {
                    Some(Hover::QueueRow(i)) => Some(i),
                    _ => None,
                };
                let out = self.queue.frame(
                    ui,
                    (sheet, c),
                    on,
                    covers,
                    likes,
                    &mut self.filter,
                    hover,
                    force.queue_drag,
                );
                self.filter_focused = out.filter_focused;
            }
            _ => self.tracks(ui, (sheet, c), route, on, covers, likes),
        }
    }

    /// Opens `route`, and with `play` plays its first track as soon as the first page is in.
    fn open(&mut self, route: Route, play: bool) {
        if play {
            self.play_when_loaded = Some(route.clone());
        }
        self.navigate(route);
    }

    fn playlist_route(p: &Playlist) -> Route {
        Route::Playlist {
            id: p.id.clone(),
            name: p.name.clone(),
            images: p.images.clone(),
        }
    }

    fn playlist_card(&self, p: &Playlist) -> CardSpec {
        let (subtitle, dim) = if self.library.is_refused(&p.id) {
            (grid::NOT_READABLE.to_owned(), true)
        } else if p.readable == Readable::Unreadable {
            (grid::MAY_NOT_READ.to_owned(), true)
        } else {
            (
                match p.track_count {
                    Some(1) => "1 song".to_owned(),
                    Some(n) => format!("{n} songs"),
                    None => "Playlist".to_owned(),
                },
                false,
            )
        };
        CardSpec {
            title: p.name.clone(),
            subtitle,
            dim,
            art: Art::Note,
            images: p.images.clone(),
            playing: false,
        }
    }

    fn album_cards(&self, state: &PlayerState) -> Vec<CardSpec> {
        let now = state.current.as_ref().map(|n| &n.track);
        self.library
            .albums
            .items()
            .iter()
            .map(|a| CardSpec {
                title: a.name.clone(),
                subtitle: a.artist.clone(),
                dim: false,
                art: Art::Note,
                images: a.images.clone(),
                playing: now.is_some_and(|t| t.album == a.name && t.artist == a.artist),
            })
            .collect()
    }

    fn playlist_cards(&self) -> Vec<CardSpec> {
        self.library
            .playlists
            .items()
            .iter()
            .map(|p| self.playlist_card(p))
            .collect()
    }

    fn liked_card(&self) -> CardSpec {
        let liked = &self.library.liked;
        let subtitle = match (liked.has_more(), liked.total()) {
            (_, Some(n)) => songs(n),
            (false, None) => songs(liked.items().len() as u32),
            (true, None) => "Liked songs".to_owned(),
        };
        CardSpec {
            title: "Liked songs".to_owned(),
            subtitle,
            dim: false,
            art: Art::Liked,
            images: Vec::new(),
            playing: false,
        }
    }

    /// Asks for the first page of `kind` when nothing has been asked for yet.
    fn first_page(&mut self, kind: ListKind, player: &PlayerHandle) {
        let empty = match kind {
            ListKind::Playlists => self.library.playlists.items().is_empty(),
            ListKind::Albums => self.library.albums.items().is_empty(),
            ListKind::Liked => self.library.liked.items().is_empty(),
            _ => false,
        };
        if empty {
            self.library.request(kind, player);
        }
    }

    fn home(&mut self, ui: &mut Ui<'_>, vp: Rect, c: Rect, on: &Show<'_>, covers: &mut Covers<'_>) {
        let Show {
            look,
            state,
            player,
            ..
        } = *on;
        for kind in [ListKind::Playlists, ListKind::Albums, ListKind::Liked] {
            self.first_page(kind, player);
        }
        let mut playlists = vec![self.liked_card()];
        playlists.extend(self.playlist_cards());
        let albums = self.album_cards(state);
        let sections = [
            Section {
                title: "Playlists",
                tab: Tab::Playlists,
                cards: &playlists,
                status: Status::Ready,
            },
            Section {
                title: "Albums",
                tab: Tab::Albums,
                cards: &albums,
                status: status_of(&self.library.albums, "No saved albums."),
            },
        ];
        let HomeOut {
            header,
            open,
            play,
            retry,
            scrolled,
        } = grid::home(ui, vp, c, look, &sections, covers, self.want);
        let (pl, al, li) = (
            &self.library.playlists,
            &self.library.albums,
            &self.library.liked,
        );
        let loading = (pl.items().is_empty() && can_grow(pl))
            || (al.items().is_empty() && can_grow(al))
            || (li.items().is_empty() && can_grow(li));
        self.scrolled = scrolled.map(|s| scrolled_of(s, loading));
        if let Some(tab) = header {
            self.navigate(tab.root());
        }
        if let Some(s) = retry {
            let kind = if s == 0 {
                ListKind::Playlists
            } else {
                ListKind::Albums
            };
            self.library.retry(kind, player);
        }
        for (hit, play) in [(open, false), (play, true)] {
            let Some((s, i)) = hit else { continue };
            let route = match (s, i) {
                (0, 0) => Some(Route::Liked),
                (0, i) => self
                    .library
                    .playlists
                    .items()
                    .get(i - 1)
                    .map(Self::playlist_route),
                (_, i) => self.album_route(i),
            };
            if let Some(route) = route {
                self.open(route, play);
            }
        }
    }

    fn album_route(&self, i: usize) -> Option<Route> {
        self.library.albums.items().get(i).map(release_route)
    }

    fn playlists(
        &mut self,
        ui: &mut Ui<'_>,
        vp: Rect,
        c: Rect,
        on: &Show<'_>,
        covers: &mut Covers<'_>,
    ) {
        let (look, player) = (on.look, on.player);
        let cards = self.playlist_cards();
        let slot = &self.library.playlists;
        let page = Page {
            scope: "playlists",
            title: "Playlists",
            count: count_of(slot),
            cards: &cards,
            status: status_of(slot, "No playlists."),
        };
        let out = grid::collection(ui, vp, c, look, &page, covers, self.want);
        let growing = can_grow(slot);
        self.scrolled = out.scrolled.map(|s| scrolled_of(s, growing));
        self.page_out(out, ListKind::Playlists, player, |s, i| {
            s.library.playlists.items().get(i).map(Self::playlist_route)
        });
    }

    fn albums(
        &mut self,
        ui: &mut Ui<'_>,
        vp: Rect,
        c: Rect,
        on: &Show<'_>,
        covers: &mut Covers<'_>,
    ) {
        let Show {
            look,
            state,
            player,
            ..
        } = *on;
        let cards = self.album_cards(state);
        let slot = &self.library.albums;
        let page = Page {
            scope: "albums",
            title: "Albums",
            count: count_of(slot),
            cards: &cards,
            status: status_of(slot, "No saved albums."),
        };
        let out = grid::collection(ui, vp, c, look, &page, covers, self.want);
        let growing = can_grow(slot);
        self.scrolled = out.scrolled.map(|s| scrolled_of(s, growing));
        self.page_out(out, ListKind::Albums, player, |s, i| s.album_route(i));
    }

    /// Acts on what a collection page reported: the card pressed, a Retry, the next page.
    fn page_out(
        &mut self,
        out: PageOut,
        kind: ListKind,
        player: &PlayerHandle,
        route_of: impl Fn(&Self, usize) -> Option<Route>,
    ) {
        if let Some(i) = out.play.or(out.open)
            && let Some(route) = route_of(self, i)
        {
            self.open(route, out.play.is_some());
        }
        if out.retry {
            self.library.retry(kind, player);
        } else {
            let slot_empty = match kind {
                ListKind::Playlists => self.library.playlists.items().is_empty(),
                _ => self.library.albums.items().is_empty(),
            };
            if out.near_end || slot_empty {
                self.library.request(kind, player);
            }
        }
    }

    /// The track page of Liked, Search, a playlist, an album or the queue (MD59): the hero column
    /// (playlists and albums) or a page header, the filter bar and the table.
    fn tracks(
        &mut self,
        ui: &mut Ui<'_>,
        (sheet, c): (Rect, Rect),
        route: &Route,
        on: &Show<'_>,
        covers: &mut Covers<'_>,
        likes: &mut LikeBook,
    ) {
        if let Route::Album { uri, .. } = route
            && !uri.is_empty()
        {
            self.explored_album(ui, (sheet, c), route, on, covers, likes);
            return;
        }
        let Show {
            look,
            state,
            player,
            hearts,
        } = *on;
        let kind = match route {
            Route::Liked => Some(ListKind::Liked),
            Route::Search => Some(ListKind::Search),
            Route::Playlist { id, name, .. } => {
                self.library.open_playlist(id, name);
                Some(ListKind::OpenPlaylist)
            }
            Route::Album {
                id,
                name,
                images,
                year,
                ..
            } => {
                self.library.open_album(id, name, images, *year);
                Some(ListKind::OpenAlbum)
            }
            _ => None,
        };
        ui.painter().on(LayerId::Popup);

        // A playlist the provider refuses (D48): its name and the fixed text with Retry.
        if let Route::Playlist { id, name, .. } = route
            && self.library.is_refused(id)
            && !slot_of(&self.library, ListKind::OpenPlaylist).is_some_and(Slot::loading)
        {
            grid::header(ui.painter(), look, c, name, None);
            let top = c.y + TRACKS_TOP;
            let rect = Rect::new(
                c.x - TABLE_INSET,
                top,
                c.w + 2.0 * TABLE_INSET,
                (c.bottom() - top).max(0.0),
            );
            self.refused(ui, rect, player);
            self.filter_focused = false;
            return;
        }

        if self.filter_for.as_ref() != Some(route) {
            self.filter.clear();
            self.filter_for = Some(route.clone());
        }
        let current = state.current.as_ref().map(|n| n.track.uri.as_str());
        let album_page = matches!(route, Route::Album { .. });
        let has_hero = matches!(route, Route::Playlist { .. } | Route::Album { .. });
        let geo = pages::geometry(sheet.w + 2.0 * layout::SHEET_X, c, has_hero);
        let slot = kind.and_then(|k| slot_of(&self.library, k));
        let count = slot.and_then(count_of);
        if hearts.is_some()
            && let Some(slot) = slot
        {
            self.feed.step(slot, route == &Route::Liked, likes, player);
        }

        // Search without anything to list: its header and a note.
        if route == &Route::Search {
            let supported = state
                .providers
                .iter()
                .find(|p| p.info.caps.library && Some(p.info.id) == self.library.provider())
                .is_some_and(|p| p.info.caps.search);
            if let Some(msg) = search::message(supported, &self.library) {
                grid::header(ui.painter(), look, c, "Search", None);
                let rect = Rect::new(c.x, c.y + 64.0, c.w, (c.h - 64.0).max(0.0));
                note(ui, rect, msg);
                self.filter_focused = false;
                return;
            }
        }

        let mut hero = HeroOut::default();
        if let Some(side) = geo.hero_side {
            let (crumb, name, by, images, year) = match route {
                Route::Album {
                    name,
                    artist,
                    images,
                    year,
                    ..
                } => (
                    "Albums",
                    name.as_str(),
                    Some(artist.as_str()),
                    &images[..],
                    *year,
                ),
                Route::Playlist { name, images, .. } => {
                    ("Playlists", name.as_str(), None, &images[..], None)
                }
                _ => ("", "", None, &[][..], None),
            };
            let scale = ui.painter().s;
            let art = covers.get(images, side, scale);
            let (loaded, complete) = slot.map_or((&[][..], false), |s| (s.items(), !s.has_more()));
            let meta = pages::meta_text(album_page, year, count, loaded, complete);
            // The album's artists as links, from the card that opened it (none when it was not
            // opened from the albums grid).
            let by_refs = match route {
                Route::Album { id, .. } => self
                    .library
                    .albums
                    .items()
                    .iter()
                    .find(|a| &a.id == id)
                    .map_or(&[][..], |a| a.artists.as_slice()),
                _ => &[][..],
            };
            let spec = Hero {
                crumb,
                name,
                by,
                by_refs,
                meta: &meta,
                side,
                art,
                save: None,
            };
            hero = pages::hero(ui, c, look, &spec);
        } else {
            let (title, sub) = page_header(route, count);
            grid::header(ui.painter(), look, c, &title, sub.as_deref());
        }

        let Some(slot) = slot else {
            self.filter_focused = false;
            return;
        };
        let shown = !slot.items().is_empty();
        self.filter_focused = shown && pages::filter_bar(ui, geo.filter, &mut self.filter);
        let visible = self
            .visible
            .get(route, &self.filter, slot.serial(), slot.items());
        let empty = match route {
            Route::Search => "No results.",
            _ => "No songs.",
        };
        let form = Form {
            album_page,
            ..Form::default()
        };
        self.pane.set_restore(self.want);
        let result = self.pane.frame(
            ui, geo.table, look, form, slot, visible, empty, current, covers, likes, player,
            hearts, None,
        );
        self.scrolled = self.pane.scrolled().map(|s| scrolled_of(s, can_grow(slot)));
        if let Some(pos) = result.play {
            play_visible(player, slot, visible, pos);
        }
        let n = slot.items().len();
        if hero.play && n > 0 {
            play_from(player, slot, 0);
        }
        if hero.shuffle && n > 0 {
            // A track chosen from the clock: no random source in the view, and no one to
            // reproduce it for.
            let at = ui.time().as_millis() as usize % n;
            player.dispatch(PlayerAction::SetShuffle(true));
            player.dispatch(PlayerAction::PlayItem {
                context: slot.items().to_vec(),
                index: at,
            });
        }

        if hero.crumb {
            self.navigate(if album_page {
                Route::Albums
            } else {
                Route::Playlists
            });
        }
        if let Some(kind) = kind {
            if result.retry {
                self.library.retry(kind, player);
            } else if result.more {
                self.library.request(kind, player);
            }
        }
    }

    /// The artist page of `open` (its uri, the name and portraits the link knew).
    #[allow(clippy::too_many_arguments)]
    fn artist_page(
        &mut self,
        ui: &mut Ui<'_>,
        sheet: Rect,
        (uri, name, images): (&str, &str, &[whipspot_core::provider::ImageRef]),
        forced: Option<whipspot_core::provider::ReleaseGroup>,
        on: &Show<'_>,
        covers: &mut Covers<'_>,
        likes: &mut LikeBook,
    ) {
        let player = on.player;
        self.filter_focused = false;
        self.library.open_artist(uri, name, images);
        let prev = self
            .history
            .previous()
            .map(Route::label)
            .unwrap_or_default();
        ui.painter().on(LayerId::Popup);
        let Some(open) = self.library.open_artist.as_ref() else {
            return;
        };
        self.artist.set_restore(self.want);
        let out = self
            .artist
            .frame(ui, sheet, open, (&prev, forced), on, covers, likes);
        let group = self.artist.group(uri);
        let head = open.head();
        let growing = (head.data().is_none() && !head.failed()) || can_grow(open.releases(group));
        self.scrolled = out.scrolled.map(|s| scrolled_of(s, growing));
        // What the page asked for, read before the library is borrowed again.
        let releases = open.releases(group).items();
        let opened = out.open.and_then(|i| releases.get(i)).map(release_route);
        let played = out
            .play_release
            .and_then(|i| releases.get(i))
            .map(|a| a.uri.clone());
        let first = first_release(open).map(|a| a.uri.clone());
        let popular = open.popular.clone();
        let head_failed = open.head().failed();
        if out.crumb {
            self.history.back();
        }
        if out.play || out.shuffle {
            let plan = artist_play(popular.len(), first.as_deref(), out.shuffle, ui.time());
            match plan {
                ArtistPlay::Tracks { index, shuffle } => {
                    if shuffle {
                        player.dispatch(PlayerAction::SetShuffle(true));
                    }
                    player.dispatch(PlayerAction::PlayItem {
                        context: popular,
                        index,
                    });
                }
                ArtistPlay::Release { uri, shuffle } => {
                    self.library.play_release(&uri, shuffle, player);
                }
                ArtistPlay::Nothing => {}
            }
        }
        if let Some(uri) = played {
            self.library.play_release(&uri, false, player);
        }
        if let Some(route) = opened {
            self.navigate(route);
        }
        if out.retry {
            if head_failed {
                self.library.retry_artist();
            } else {
                self.library.retry(ListKind::Releases(group), player);
            }
        } else if out.more {
            self.library.request(ListKind::Releases(group), player);
        }
    }

    /// The explored album page (an album opened by its uri): board 3's hero column, the table
    /// with an Artist column, Save to library and "More by".
    fn explored_album(
        &mut self,
        ui: &mut Ui<'_>,
        (sheet, _): (Rect, Rect),
        route: &Route,
        on: &Show<'_>,
        covers: &mut Covers<'_>,
        likes: &mut LikeBook,
    ) {
        let Route::Album {
            uri,
            name,
            images,
            year,
            artist,
            ..
        } = route
        else {
            return;
        };
        let Show {
            state,
            player,
            hearts,
            ..
        } = *on;
        self.library.open_explored_album(uri, name, images, *year);
        ui.painter().on(LayerId::Popup);
        let save_albums = state
            .providers
            .iter()
            .find(|p| p.info.caps.library && Some(p.info.id) == self.library.provider())
            .is_some_and(|p| p.info.caps.save_albums);
        let prev = self
            .history
            .previous()
            .map(Route::label)
            .unwrap_or_default();
        let Some(open) = self.library.open_album.as_ref() else {
            return;
        };
        let named = open.explore.as_ref().is_some_and(|e| e.album.is_some());
        if hearts.is_some() {
            self.feed.step(&open.tracks, false, likes, player);
        }
        let save = (save_albums && named).then(|| {
            likes.want([uri.as_str()], player);
            Save {
                saved: likes.state(uri),
                writable: hearts == Some(true),
            }
        });
        if self.filter_for.as_ref() != Some(route) {
            self.filter.clear();
            self.filter_for = Some(route.clone());
        }
        let visible = self.visible.get(
            route,
            &self.filter,
            open.tracks.serial(),
            open.tracks.items(),
        );
        let typed = self.filter.clone();
        let args = AlbumArgs {
            prev: &prev,
            window_w: sheet.w + 2.0 * layout::SHEET_X,
            save,
            route_artist: artist,
            visible,
            restore: self.want,
        };
        let out = artist::album_page(
            ui,
            sheet,
            open,
            &args,
            on,
            covers,
            likes,
            (&mut self.pane, &mut self.filter),
        );
        self.filter_focused = out.filter_focused;
        self.scrolled = out.scrolled.map(|s| scrolled_of(s, can_grow(&open.tracks)));
        // The rows follow the text a frame late (the field is drawn inside the page): draw again.
        if self.filter != typed {
            ui.request_repaint_at(ui.time());
        }
        let slot = &open.tracks;
        let visible = self
            .visible
            .get(route, &self.filter, slot.serial(), slot.items());
        if let Some(pos) = out.pane.play {
            play_visible(player, slot, visible, pos);
        }
        let n = slot.items().len();
        if out.hero.play && n > 0 {
            play_from(player, slot, 0);
        }
        if out.hero.shuffle && n > 0 {
            player.dispatch(PlayerAction::SetShuffle(true));
            player.dispatch(PlayerAction::PlayItem {
                context: slot.items().to_vec(),
                index: pick(ui.time(), n),
            });
        }
        let more = artist::more_by(open);
        let opened = out.open.and_then(|i| more.get(i)).map(|a| release_route(a));
        let played = out
            .play_release
            .and_then(|i| more.get(i))
            .map(|a| a.uri.clone());
        let (retry, want_more) = (out.pane.retry, out.pane.more);
        if out.hero.crumb {
            if prev.is_empty() {
                self.navigate(Route::Albums);
            } else {
                self.history.back();
            }
        }
        if out.hero.save {
            let writable = hearts == Some(true);
            likes.click(uri, writable, player);
            self.album_saved = writable;
        }
        if let Some(uri) = played {
            self.library.play_release(&uri, false, player);
        }
        if let Some(route) = opened {
            self.navigate(route);
        }
        if retry {
            self.library.retry(ListKind::OpenAlbum, player);
        } else if want_more {
            self.library.request(ListKind::OpenAlbum, player);
        }
    }

    /// The refusal of a playlist the provider does not let this app read (D48): a fixed text and
    /// a Retry button.
    fn refused(&mut self, ui: &mut Ui<'_>, rect: Rect, player: &PlayerHandle) {
        let theme = *ui.theme();
        let spec = lune_ui::text::Spec::plain(
            REFUSED_TEXT,
            lune_ui::text::Style::ui(14.0, theme.fg_secondary),
        )
        .wrapped(REFUSED_W.min(rect.w - 2.0 * REFUSED_MARGIN));
        let (w, h) = ui.painter().measure(&spec);
        let y = rect.y + (rect.h - h - 14.0 - RETRY_SIZE.1) / 2.0;
        let x = rect.x + (rect.w - w) / 2.0;
        ui.painter().text(&spec, x, y);
        let button = Rect::new(
            rect.x + (rect.w - RETRY_SIZE.0) / 2.0,
            y + h + 14.0,
            RETRY_SIZE.0,
            RETRY_SIZE.1,
        );
        if ui
            .button("refused-retry", button, "Retry", ButtonKind::Primary)
            .clicked
        {
            self.library.retry(ListKind::OpenPlaylist, player);
        }
    }

    /// Plays the first track of the route a play disc opened, once its first page is in; drops
    /// the wish when the route is no longer on show or its list ended up empty or failed.
    fn settle_play(&mut self, player: &PlayerHandle) {
        let Some(route) = &self.play_when_loaded else {
            return;
        };
        if self.history.current() != route {
            self.play_when_loaded = None;
            return;
        }
        let kind = match route {
            Route::Liked => Some(ListKind::Liked),
            Route::Playlist { id, .. } => self
                .library
                .open_playlist
                .as_ref()
                .filter(|o| &o.id == id)
                .map(|_| ListKind::OpenPlaylist),
            Route::Album { .. } => self
                .library
                .open_album
                .as_ref()
                .filter(|o| route.album_key() == Some(o.id.as_str()))
                .map(|_| ListKind::OpenAlbum),
            _ => None,
        };
        let Some(slot) = kind.and_then(|k| slot_of(&self.library, k)) else {
            // The page has not been drawn yet (the slot opens with it), or the route has no list.
            if !matches!(
                route,
                Route::Liked | Route::Playlist { .. } | Route::Album { .. }
            ) {
                self.play_when_loaded = None;
            }
            return;
        };
        if !slot.items().is_empty() {
            play_from(player, slot, 0);
            self.play_when_loaded = None;
        } else if slot.failed() || (!slot.loading() && !slot.has_more()) {
            self.play_when_loaded = None;
        }
    }
}

impl Feed {
    /// Tells the like book about the rows of `slot` it has not heard of yet: the Liked list's rows
    /// are noted as saved; any other list's go to `want` (a chunk per frame, the book sending at
    /// most one action per call). A different list starts over.
    pub(crate) fn step(
        &mut self,
        slot: &Slot<Track>,
        liked_list: bool,
        likes: &mut LikeBook,
        player: &PlayerHandle,
    ) {
        if self.serial != slot.serial() {
            *self = Feed {
                serial: slot.serial(),
                done: 0,
            };
        }
        let items = slot.items();
        let end = if liked_list {
            items.len()
        } else {
            items.len().min(self.done + WANT_CHUNK)
        };
        if end <= self.done {
            return;
        }
        let fresh = &items[self.done..end];
        if liked_list {
            for t in fresh {
                likes.note(&t.uri, true);
            }
        } else {
            likes.want(fresh.iter().map(|t| t.uri.as_str()), player);
        }
        self.done = end;
    }
}

/// `Some(writable)` when the library provider `id` (the one on show) can like songs (`writable`:
/// it can change them now), `None` when it cannot at all and no heart is drawn.
fn hearts_of(providers: &[ProviderEntry], id: &str) -> Option<bool> {
    let entry = providers
        .iter()
        .find(|p| p.info.caps.library && p.info.id == id)?;
    entry.info.caps.likes.then_some(entry.likes_writable)
}

/// The title of a track page without a hero column and the muted text beside it.
fn page_header(route: &Route, count: Option<u32>) -> (String, Option<String>) {
    match route {
        Route::Liked => ("Liked songs".into(), count.map(songs)),
        Route::Search => (
            "Search".into(),
            count.map(|n| format!("{n} {}", if n == 1 { "result" } else { "results" })),
        ),
        Route::Playlist { name, .. } => (name.clone(), count.map(songs)),
        Route::Album { name, artist, .. } => (name.clone(), Some(artist.clone())),
        _ => (String::new(), None),
    }
}

/// The route a clicked link opens: an artist page or an explored album, addressed by the link's
/// uri; `None` for a uri that names neither.
fn link_route(r: &EntityRef) -> Option<Route> {
    let (_, rest) = r.uri.split_once(':')?;
    match rest.split(':').next()? {
        "artist" => Some(Route::Artist {
            uri: r.uri.clone(),
            name: r.name.clone(),
            images: Vec::new(),
        }),
        "album" => Some(Route::Album {
            id: String::new(),
            uri: r.uri.clone(),
            name: r.name.clone(),
            artist: String::new(),
            images: Vec::new(),
            year: None,
        }),
        _ => None,
    }
}

/// What a Play or Shuffle on the artist page does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ArtistPlay {
    /// Play the popular list from `index` (shuffle: turn shuffle on first).
    Tracks {
        index: usize,
        shuffle: bool,
    },
    /// The artist has no popular list: play its first release.
    Release {
        uri: String,
        shuffle: bool,
    },
    Nothing,
}

/// Play and Shuffle play the popular list when there is one (shuffle starts at a position taken
/// from the clock), else the first release.
fn artist_play(popular: usize, first: Option<&str>, shuffle: bool, now: Duration) -> ArtistPlay {
    if popular > 0 {
        let index = if shuffle { pick(now, popular) } else { 0 };
        ArtistPlay::Tracks { index, shuffle }
    } else if let Some(uri) = first {
        ArtistPlay::Release {
            uri: uri.to_owned(),
            shuffle,
        }
    } else {
        ArtistPlay::Nothing
    }
}

/// The release Play falls back to: the first of the artist's own, else any with a uri.
fn first_release(open: &crate::library::ArtistOpen) -> Option<&Album> {
    let all = open
        .releases(whipspot_core::provider::ReleaseGroup::All)
        .items();
    all.iter()
        .find(|a| !a.uri.is_empty() && a.kind != whipspot_core::provider::AlbumKind::AppearsOn)
        .or_else(|| all.iter().find(|a| !a.uri.is_empty()))
}

/// The route that opens `a`: by its uri when it has one (the explored page), else by its id.
fn release_route(a: &Album) -> Route {
    Route::Album {
        id: a.id.clone(),
        uri: a.uri.clone(),
        name: a.name.clone(),
        artist: a.artist.clone(),
        images: a.images.clone(),
        year: a.year,
    }
}

/// A position in `n` items chosen from the clock: no random source in the view, and no one to
/// reproduce it for.
fn pick(now: Duration, n: usize) -> usize {
    if n == 0 {
        0
    } else {
        now.as_millis() as usize % n
    }
}

/// "N songs" ("1 song").
fn songs(n: u32) -> String {
    if n == 1 {
        "1 song".to_owned()
    } else {
        format!("{n} songs")
    }
}

/// The size a collection header states: the answered total, else what loaded once complete.
fn count_of<T>(slot: &Slot<T>) -> Option<u32> {
    slot.total().or_else(|| {
        (!slot.has_more() && !slot.items().is_empty()).then_some(slot.items().len() as u32)
    })
}

/// Whether more of `slot`'s list may still arrive (so a page reaching for the offset it was left
/// at may find it a page later).
fn can_grow<T>(slot: &Slot<T>) -> bool {
    !slot.failed() && (slot.loading() || slot.has_more())
}

/// What a view reported of its scroll area (offset, extent, viewport heights) as a [`Scrolled`].
fn scrolled_of((offset, extent, viewport): (f64, f64, f64), can_grow: bool) -> Scrolled {
    Scrolled {
        offset,
        extent,
        viewport,
        can_grow,
    }
}

/// What a collection list is doing, for its page.
fn status_of<T>(slot: &Slot<T>, empty: &'static str) -> Status {
    if slot.failed() {
        Status::Failed
    } else if slot.loading() || slot.has_more() {
        Status::Loading
    } else if slot.items().is_empty() {
        Status::Empty(empty)
    } else {
        Status::Ready
    }
}

/// The loaded tracks of a track list kind.
fn slot_of(library: &LibraryModel, kind: ListKind) -> Option<&Slot<Track>> {
    match kind {
        ListKind::Liked => Some(&library.liked),
        ListKind::OpenPlaylist => library.open_playlist.as_ref().map(|o| &o.tracks),
        ListKind::OpenAlbum => library.open_album.as_ref().map(|o| &o.tracks),
        ListKind::Search => library.search.as_ref().map(|o| &o.tracks),
        ListKind::Playlists | ListKind::Albums | ListKind::Releases(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_is_the_tours_only_on_pages_without_menus_dropdowns_or_drags() {
        let mut sheet = Browse::new(Tab::Home);
        assert!(sheet.escape_free());
        for route in [Route::Settings, Route::Equalizer, Route::Queue] {
            sheet.navigate(route.clone());
            assert!(!sheet.escape_free(), "{route:?}");
        }
        sheet.navigate(Route::Albums);
        assert!(sheet.escape_free());
    }

    use whipspot_core::provider::model::{Capabilities, ProviderInfo};

    /// One frame of the scroll memory alone: the views are replaced by `report`, the scroll the
    /// page on show would have reported; `ms` is the frame's time.
    fn memory_frame(
        h: &mut lune_ui::testing::UiHarness,
        sheet: &mut Browse,
        ms: u64,
        report: Option<(f64, bool)>,
    ) -> Option<f64> {
        h.at_ms(ms);
        let mut want = None;
        h.frame(|ui| {
            let (arrived, drawn_at) = sheet.begin_scroll(ui.time());
            want = sheet.want;
            sheet.scrolled = report.map(|(offset, can_grow)| Scrolled {
                offset,
                extent: 5000.0,
                viewport: 400.0,
                can_grow,
            });
            sheet.settle_scroll(ui, Rect::new(0.0, 0.0, 800.0, 600.0), arrived, drawn_at);
        });
        want
    }

    #[test]
    fn back_restores_the_offset_the_page_was_left_at_and_only_then_records_again() {
        let mut h = lune_ui::testing::UiHarness::new((800.0, 600.0));
        let mut sheet = Browse::new(Tab::Home);
        sheet.navigate(Route::Albums);
        for i in 0..3 {
            assert_eq!(
                memory_frame(&mut h, &mut sheet, i * 16, Some((600.0, false))),
                None
            );
        }
        // An album opened from the grid starts at the top and records its own offset.
        sheet.navigate(Route::Liked);
        memory_frame(&mut h, &mut sheet, 100, Some((0.0, false)));
        memory_frame(&mut h, &mut sheet, 116, Some((80.0, false)));
        // Back: the restore asks for 600; the fresh page reports 0 and must not overwrite it.
        assert!(sheet.back());
        let want = memory_frame(&mut h, &mut sheet, 200, Some((0.0, true)));
        assert_eq!(want, Some(600.0));
        let want = memory_frame(&mut h, &mut sheet, 216, Some((0.0, true)));
        assert_eq!(want, Some(600.0), "still asked while the page is not there");
        // The page gets there: the restore ends and recording resumes with the real offset.
        memory_frame(&mut h, &mut sheet, 300, Some((600.0, true)));
        assert!(sheet.restore.is_none());
        let want = memory_frame(&mut h, &mut sheet, 316, Some((640.0, false)));
        assert_eq!(want, None);
        // Forward and back again: the Liked entry kept its 80, the Albums one now 640.
        assert!(sheet.forward());
        assert_eq!(
            memory_frame(&mut h, &mut sheet, 400, Some((0.0, true))),
            Some(80.0)
        );
        assert!(sheet.back());
        assert_eq!(
            memory_frame(&mut h, &mut sheet, 500, Some((0.0, true))),
            Some(640.0)
        );
    }

    #[test]
    fn a_push_or_the_user_or_the_clock_ends_a_restore() {
        let new = || {
            let mut sheet = Browse::new(Tab::Home);
            sheet.navigate(Route::Albums);
            sheet
        };
        let go_back = |h: &mut lune_ui::testing::UiHarness, sheet: &mut Browse| {
            memory_frame(h, sheet, 0, Some((300.0, false)));
            sheet.navigate(Route::Liked);
            memory_frame(h, sheet, 16, Some((0.0, false)));
            sheet.back();
            assert_eq!(memory_frame(h, sheet, 100, Some((0.0, true))), Some(300.0));
        };
        // A push (a tab click, a link) drops it: the next frame asks for nothing.
        let mut h = lune_ui::testing::UiHarness::new((800.0, 600.0));
        let mut sheet = new();
        go_back(&mut h, &mut sheet);
        sheet.navigate(Route::Playlists);
        assert_eq!(
            memory_frame(&mut h, &mut sheet, 116, Some((0.0, true))),
            None
        );
        assert!(sheet.restore.is_none());

        // The wheel in a later frame hands the page back to the user.
        let mut h = lune_ui::testing::UiHarness::new((800.0, 600.0));
        let mut sheet = new();
        go_back(&mut h, &mut sheet);
        h.input.scroll_pixels(0.0, -40.0);
        memory_frame(&mut h, &mut sheet, 116, Some((0.0, true)));
        assert!(sheet.restore.is_none());
        assert_eq!(
            memory_frame(&mut h, &mut sheet, 132, Some((0.0, true))),
            None
        );

        // The press that went back (the same frame) is not the user taking over.
        let mut h = lune_ui::testing::UiHarness::new((800.0, 600.0));
        let mut sheet = new();
        memory_frame(&mut h, &mut sheet, 0, Some((300.0, false)));
        sheet.navigate(Route::Liked);
        memory_frame(&mut h, &mut sheet, 16, Some((0.0, false)));
        sheet.back();
        h.input.pointer_to(20.0, 20.0);
        h.input.click(lune_ui::input::PointerButton::Primary);
        memory_frame(&mut h, &mut sheet, 100, Some((0.0, true)));
        assert!(sheet.restore.is_some());

        // Three seconds on, a restore that never got there gives up.
        let mut h = lune_ui::testing::UiHarness::new((800.0, 600.0));
        let mut sheet = new();
        go_back(&mut h, &mut sheet);
        memory_frame(&mut h, &mut sheet, 3000, Some((0.0, true)));
        assert!(sheet.restore.is_some());
        memory_frame(&mut h, &mut sheet, 3101, Some((0.0, true)));
        assert!(sheet.restore.is_none());
    }

    fn entry(id: &'static str, explore: bool) -> ProviderEntry {
        ProviderEntry {
            info: ProviderInfo {
                id,
                name: id,
                glyph: Default::default(),
                caps: Capabilities {
                    library: true,
                    pcm_tap: true,
                    eq: true,
                    search: true,
                    likes: true,
                    explore,
                    save_albums: false,
                },
                auth_steps: Vec::new(),
            },
            auth: None,
            likes_writable: true,
        }
    }

    fn link(uri: &str, name: &str) -> EntityRef {
        EntityRef {
            uri: uri.into(),
            name: name.into(),
        }
    }

    #[test]
    fn a_link_opens_its_page_and_switches_the_library_to_its_provider_first() {
        let providers = [entry("one", true), entry("two", true)];
        let mut sheet = Browse::new(Tab::Home);
        // The library shows "one" (the first with a library); a link of "two" switches it.
        sheet.open_ref(&link("two:artist:9", "Ann"), &providers);
        assert_eq!(sheet.choice.get().as_deref(), Some("two"));
        assert!(
            matches!(sheet.history.current(), Route::Artist { uri, name, .. } if uri == "two:artist:9" && name == "Ann")
        );
        // A link of the provider on show does not touch the choice and opens an album by uri.
        sheet.open_ref(&link("two:album:3", "Low Tide"), &providers);
        assert_eq!(sheet.history.current().album_key(), Some("two:album:3"));
        assert_eq!(
            sheet.history.previous().map(Route::label),
            Some("Ann".into())
        );
        // The page already on show is not pushed again.
        sheet.open_ref(&link("two:album:3", "Low Tide"), &providers);
        assert!(sheet.back() && matches!(sheet.history.current(), Route::Artist { .. }));
        sheet.open_ref(&link("two:artist:9", "Ann"), &providers);
        assert!(
            sheet.forward(),
            "the artist page on show is not pushed again"
        );
    }

    #[test]
    fn a_link_that_cannot_be_followed_does_nothing() {
        let providers = [entry("one", true), entry("plain", false)];
        let mut sheet = Browse::new(Tab::Home);
        for bad in [
            link("plain:artist:1", "x"), // a provider that cannot explore
            link("gone:artist:1", "x"),  // no such provider
            link("one:track:1", "x"),    // neither an artist nor an album
            link("nocolon", "x"),
            link("", "x"),
        ] {
            sheet.open_ref(&bad, &providers);
            assert_eq!(*sheet.history.current(), Route::Home, "{bad:?}");
            assert_eq!(sheet.choice.get(), None, "{bad:?}");
        }
    }

    #[test]
    fn play_and_shuffle_on_an_artist_use_the_popular_list_else_the_first_release() {
        let now = Duration::from_millis(7_003);
        assert_eq!(
            artist_play(5, Some("p:album:1"), false, now),
            ArtistPlay::Tracks {
                index: 0,
                shuffle: false
            }
        );
        // Shuffle starts somewhere in the list, chosen from the clock.
        assert_eq!(
            artist_play(5, None, true, now),
            ArtistPlay::Tracks {
                index: 7_003 % 5,
                shuffle: true
            }
        );
        assert_eq!(
            artist_play(0, Some("p:album:1"), true, now),
            ArtistPlay::Release {
                uri: "p:album:1".into(),
                shuffle: true
            }
        );
        assert_eq!(artist_play(0, None, false, now), ArtistPlay::Nothing);
    }

    #[test]
    fn a_release_opens_by_uri_when_it_has_one_and_by_id_when_not() {
        let mut a = Album {
            id: "id".into(),
            uri: "p:album:1".into(),
            name: "N".into(),
            artist: "A".into(),
            track_count: None,
            images: vec![],
            year: Some(1999),
            artists: vec![],
            kind: Default::default(),
        };
        assert_eq!(release_route(&a).album_key(), Some("p:album:1"));
        a.uri.clear();
        assert_eq!(release_route(&a).album_key(), Some("id"));
    }
}

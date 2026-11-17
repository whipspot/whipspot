//! The browse sheet's content: the top bar, the tabs and the routed pages, with the explorer
//! history (MD54). Home and the collection grids are `grid.rs`; a playlist, album, liked, search
//! or queue route draws a track list. Interface fixed by M5 step 5.

use std::time::Duration;

use lune_ui::paint::Rect;
use lune_ui::scene::LayerId;
use lune_ui::ui::Ui;
use lune_ui::widgets::button::ButtonKind;
use whipspot_core::player::{PlayerAction, PlayerHandle, PlayerNotice, PlayerState, ProviderEntry};
use whipspot_core::provider::{Playlist, Readable, Track};

use crate::Anchors;
use crate::account::{self, AccountView};
use crate::anim;
use crate::art::ArtCache;
use crate::equalizer::EqualizerPane;
use crate::grid::{self, Art, CardSpec, Covers, HomeOut, Page, PageOut, Section, Status};
use crate::history::{History, Route, Tab, tab_of};
use crate::layout;
use crate::library::{
    Access, LibraryAnswer, LibraryModel, ListKind, PrefillAnswer, ProviderChoice, Slot,
    library_entry,
};
use crate::likes::LikeBook;
use crate::pages::{self, FilterCache, Hero, HeroOut};
use crate::queue::QueuePane;
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
            filter: String::new(),
            filter_for: None,
            filter_focused: false,
            feed: Feed::default(),
            visible: FilterCache::default(),
            shown: None,
            changed: None,
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
            Route::Playlist { .. } | Route::Album { .. }
        ) {
            let root = self.tab().root();
            self.history.push(root);
        }
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
        let now = ui.time();
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
        settings_out
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
        } = grid::home(ui, vp, c, look, &sections, covers);
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
        self.library.albums.items().get(i).map(|a| Route::Album {
            id: a.id.clone(),
            name: a.name.clone(),
            artist: a.artist.clone(),
            images: a.images.clone(),
            year: a.year,
        })
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
        let out = grid::collection(ui, vp, c, look, &page, covers);
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
        let out = grid::collection(ui, vp, c, look, &page, covers);
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
            let spec = Hero {
                crumb,
                name,
                by,
                meta: &meta,
                side,
                art,
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
        let form = Form { album_page };
        let result = self.pane.frame(
            ui, geo.table, look, form, slot, visible, empty, current, covers, likes, player, hearts,
        );
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
            Route::Album { id, .. } => self
                .library
                .open_album
                .as_ref()
                .filter(|o| &o.id == id)
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
        ListKind::Playlists | ListKind::Albums => None,
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
}

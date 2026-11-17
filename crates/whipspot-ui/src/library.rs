//! The library model: what the player answered to library and prefill requests, kept as lists.
//!
//! Every list (playlists, albums, liked songs, one open playlist, one open album, the search
//! results) holds its loaded items, the offset to ask for next and at most one pending request.
//! One counter numbers every request; an answer whose id is pending for no list is stale and is
//! dropped. The model builds requests; the views send them (see [`LibraryModel::request`]).

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use whipspot_core::player::{
    LibraryPage, LibraryQuery, PlayerAction, PlayerHandle, ProviderEntry, RequestId,
};
use whipspot_core::provider::model::LoginState;
use whipspot_core::provider::{Album, ImageRef, LibErrorKind, LibraryError, Playlist, Track};

/// The text a failed list shows (never the error's own text).
pub const LOAD_FAILED: &str = "Could not load this list.";
/// Prefill answers kept while nothing takes them; older ones are dropped.
const MAX_PREFILLS: usize = 16;

/// One answer to a library request.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryAnswer {
    pub request: RequestId,
    pub result: Result<LibraryPage, LibraryError>,
}

/// One answer to a prefill request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefillAnswer {
    pub request: RequestId,
    pub value: Option<String>,
}

/// The library provider the user chose, shared by the native view and the skinned front-end so
/// both show the same one (D11). `None` is "the first provider with a library". The host reads it
/// back on exit to save it.
#[derive(Debug, Clone, Default)]
pub struct ProviderChoice(Arc<Mutex<Option<String>>>);

impl ProviderChoice {
    /// A choice that starts at `id` (a saved one; it may name a provider that is gone).
    pub fn new(id: Option<String>) -> Self {
        ProviderChoice(Arc::new(Mutex::new(id)))
    }

    /// The chosen provider id, if one was chosen.
    pub fn get(&self) -> Option<String> {
        self.0.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// Chooses provider `id`.
    pub fn set(&self, id: &str) {
        if let Ok(mut g) = self.0.lock() {
            *g = Some(id.to_owned());
        }
    }
}

/// The entry whose library the views show: `selected` when it names a provider with a library,
/// else (nothing chosen, or the choice is gone) the first provider with a library.
pub fn library_entry<'a>(
    providers: &'a [ProviderEntry],
    selected: Option<&str>,
) -> Option<&'a ProviderEntry> {
    selected
        .and_then(|id| {
            providers
                .iter()
                .find(|p| p.info.caps.library && p.info.id == id)
        })
        .or_else(|| providers.iter().find(|p| p.info.caps.library))
}

/// What the library views can show of the library provider (D11: the chosen provider with a
/// library, else the first one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access<'a> {
    /// No registered provider has a library.
    NoProvider,
    /// The provider's login state has not been answered yet.
    Checking,
    /// At least one of the provider's login steps is not logged in.
    LoggedOut,
    /// Every login step is logged in (a switch step, on or off, does not count): the provider id.
    Ready(&'a str),
}

impl<'a> Access<'a> {
    /// The access the provider entries give for the `selected` provider (see [`library_entry`]).
    pub fn of(providers: &'a [ProviderEntry], selected: Option<&str>) -> Access<'a> {
        let Some(entry) = library_entry(providers, selected) else {
            return Access::NoProvider;
        };
        // A switch step (an optional feature turned on or off) never gates the library.
        let mut logins = entry.info.auth_steps.iter().filter(|s| s.is_login());
        match &entry.auth {
            None if logins.clone().next().is_some() => Access::Checking,
            auth => {
                let logged_in = logins.all(|step| {
                    auth.as_ref().is_some_and(|a| {
                        a.steps
                            .iter()
                            .any(|s| s.id == step.id && s.state == LoginState::LoggedIn)
                    })
                });
                if logged_in {
                    Access::Ready(entry.info.id)
                } else {
                    Access::LoggedOut
                }
            }
        }
    }

    /// The provider id while ready.
    pub fn provider(self) -> Option<&'a str> {
        match self {
            Access::Ready(id) => Some(id),
            _ => None,
        }
    }
}

/// What a list loads from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    Playlists,
    Albums,
    Liked,
    PlaylistItems(String),
    AlbumTracks(String),
    Search(String),
}

impl Source {
    fn query(&self, offset: u32) -> LibraryQuery {
        match self {
            Source::Playlists => LibraryQuery::Playlists { offset },
            Source::Albums => LibraryQuery::Albums { offset },
            Source::Liked => LibraryQuery::Liked { offset },
            Source::PlaylistItems(id) => LibraryQuery::PlaylistItems {
                id: id.clone(),
                offset,
            },
            Source::AlbumTracks(id) => LibraryQuery::AlbumTracks {
                id: id.clone(),
                offset,
            },
            Source::Search(query) => LibraryQuery::Search {
                query: query.clone(),
                offset,
            },
        }
    }
}

/// A request the model built: the caller sends it with [`LibraryModel::send`].
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: RequestId,
    pub query: LibraryQuery,
}

/// One list: its loaded items and where it stands.
#[derive(Debug)]
pub struct Slot<T> {
    source: Source,
    /// Names this list (a view keys its widget state by it).
    serial: u64,
    items: Vec<T>,
    /// The offset to ask for next; `None` once the list is complete.
    next_offset: Option<u32>,
    /// The request in flight and the offset it asked for.
    pending: Option<(RequestId, u32)>,
    failed: bool,
    /// The list's size as the last answer told it, when it did.
    total: Option<u32>,
    /// The list loads again from the start (the provider said it changed): the items on show
    /// stay until the first page arrives and replaces them.
    refreshing: bool,
}

impl<T> Slot<T> {
    fn new(source: Source, serial: u64) -> Self {
        Slot {
            source,
            serial,
            items: Vec::new(),
            next_offset: Some(0),
            pending: None,
            failed: false,
            total: None,
            refreshing: false,
        }
    }

    /// A complete list that no request fills (the queue's copy): nothing more to ask for.
    pub fn fixed(items: Vec<T>, serial: u64) -> Self {
        let total = u32::try_from(items.len()).ok();
        Slot {
            next_offset: None,
            items,
            total,
            ..Slot::new(Source::Liked, serial)
        }
    }

    /// The list's size when the provider told it (a page's total), else `None`.
    pub fn total(&self) -> Option<u32> {
        self.total
    }

    /// The items loaded so far.
    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// The list's identity.
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// A request is in flight.
    pub fn loading(&self) -> bool {
        self.pending.is_some()
    }

    /// The last request failed (the list waits for a retry).
    pub fn failed(&self) -> bool {
        self.failed
    }

    /// More items can be asked for.
    pub fn has_more(&self) -> bool {
        self.next_offset.is_some()
    }

    /// Loads the list again from the start, keeping its serial and the items on show until the
    /// first page replaces them; an answer to what was asked before goes stale. A list that never
    /// asked for anything stays as it is (its first request loads it fresh anyway).
    fn refresh(&mut self) {
        if self.items.is_empty() && self.pending.is_none() && self.next_offset == Some(0) {
            return;
        }
        self.refreshing = true;
        self.next_offset = Some(0);
        self.pending = None;
        self.failed = false;
    }

    /// A refresh waits for its first request.
    fn refresh_due(&self) -> bool {
        self.refreshing && self.pending.is_none() && !self.failed
    }

    /// The next request, if one is due: more to load, none in flight, the last one not failed.
    fn request(&mut self, ids: &mut Ids) -> Option<Request> {
        let offset = self.next_offset?;
        if self.pending.is_some() || self.failed {
            return None;
        }
        let id = ids.next();
        self.pending = Some((id, offset));
        Some(Request {
            id,
            query: self.source.query(offset),
        })
    }

    /// Takes the answer to the pending request; `take` splits a page into its items and the next
    /// offset (`None` when the page is of another kind). A page that does not advance the offset
    /// ends the list rather than loop.
    fn finish(
        &mut self,
        result: Result<LibraryPage, LibraryError>,
        take: impl FnOnce(LibraryPage) -> Option<Taken<T>>,
    ) {
        let Some((_, offset)) = self.pending.take() else {
            return;
        };
        match result.ok().and_then(take) {
            Some((items, next, total)) => {
                if std::mem::take(&mut self.refreshing) {
                    self.items.clear();
                }
                self.items.extend(items);
                self.next_offset = next.filter(|n| *n > offset);
                self.total = total;
                self.failed = false;
            }
            None => self.failed = true,
        }
    }

    fn is_pending(&self, request: RequestId) -> bool {
        self.pending.is_some_and(|(id, _)| id == request)
    }
}

/// The request counter, one for every list and prefill.
#[derive(Debug, Default)]
struct Ids(u64);

impl Ids {
    fn next(&mut self) -> RequestId {
        self.0 += 1;
        RequestId(self.0)
    }
}

/// A playlist or album opened from the side list: its tracks.
#[derive(Debug)]
pub struct Open {
    pub id: String,
    pub name: String,
    /// The album's covers and year, which its tracks take (MD72); empty for the rest.
    pub images: Vec<ImageRef>,
    pub year: Option<u16>,
    pub tracks: Slot<Track>,
}

/// Fills each track without an album name with the album's name, covers and year (MD72):
/// `/albums/{id}/tracks` answers tracks without an album object.
fn fill_from_album(tracks: &mut [Track], name: &str, images: &[ImageRef], year: Option<u16>) {
    for t in tracks.iter_mut().filter(|t| t.album.is_empty()) {
        name.clone_into(&mut t.album);
        t.images = images.to_vec();
        t.year = year;
    }
}

/// The lists the views can ask to load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKind {
    Playlists,
    Albums,
    Liked,
    OpenPlaylist,
    OpenAlbum,
    Search,
}

/// A page taken apart: its items, the offset to ask for next and the list's total.
type Taken<T> = (Vec<T>, Option<u32>, Option<u32>);

fn playlists_of(page: LibraryPage) -> Option<Taken<Playlist>> {
    match page {
        LibraryPage::Playlists(p) => Some((p.items, p.next_offset, p.total)),
        _ => None,
    }
}

fn albums_of(page: LibraryPage) -> Option<Taken<Album>> {
    match page {
        LibraryPage::Albums(p) => Some((p.items, p.next_offset, p.total)),
        _ => None,
    }
}

fn tracks_of(page: LibraryPage) -> Option<Taken<Track>> {
    match page {
        LibraryPage::Tracks(p) => Some((p.items, p.next_offset, p.total)),
        _ => None,
    }
}

/// The library side of the player surface.
#[derive(Debug)]
pub struct LibraryModel {
    ids: Ids,
    /// The provider the lists belong to while it is logged in.
    active: Option<String>,
    pub playlists: Slot<Playlist>,
    pub albums: Slot<Album>,
    pub liked: Slot<Track>,
    pub open_playlist: Option<Open>,
    pub open_album: Option<Open>,
    /// The search results, with the query that produced them.
    pub search: Option<Open>,
    /// Playlists the provider refused to read this session (D48): a `NotReadable` answer marks
    /// one, a page that arrives later clears it.
    refused: HashSet<String>,
    prefills: VecDeque<PrefillAnswer>,
}

impl Default for LibraryModel {
    fn default() -> Self {
        LibraryModel::fresh(Ids::default(), None, VecDeque::new())
    }
}

impl LibraryModel {
    /// Lists with nothing loaded, each with a serial taken from the counter.
    fn fresh(mut ids: Ids, active: Option<String>, prefills: VecDeque<PrefillAnswer>) -> Self {
        let playlists = Slot::new(Source::Playlists, ids.next().0);
        let albums = Slot::new(Source::Albums, ids.next().0);
        let liked = Slot::new(Source::Liked, ids.next().0);
        LibraryModel {
            ids,
            active,
            playlists,
            albums,
            liked,
            open_playlist: None,
            open_album: None,
            search: None,
            refused: HashSet::new(),
            prefills,
        }
    }

    /// The next request id (a prefill uses it too).
    pub fn next_request(&mut self) -> RequestId {
        self.ids.next()
    }

    /// Tells which provider's library is usable now (`None` while logged out). The lists start
    /// afresh whenever that changes, so a login made in the Account view loads them and a logout
    /// drops them; answers to what was asked before become stale.
    pub fn sync(&mut self, provider: Option<&str>) {
        if self.active.as_deref() == provider {
            return;
        }
        *self = LibraryModel::fresh(
            std::mem::take(&mut self.ids),
            provider.map(str::to_owned),
            std::mem::take(&mut self.prefills),
        );
    }

    /// Loads every list again (the provider's library changed): each keeps what it shows until
    /// its first page arrives; [`Self::pump`] sends the requests.
    pub fn refresh_all(&mut self) {
        self.playlists.refresh();
        self.albums.refresh();
        self.liked.refresh();
        for o in [
            &mut self.open_playlist,
            &mut self.open_album,
            &mut self.search,
        ]
        .into_iter()
        .flatten()
        {
            o.tracks.refresh();
        }
    }

    /// Loads the liked list again (a like was written), as [`Self::refresh_all`] does.
    pub fn refresh_liked(&mut self) {
        self.liked.refresh();
    }

    /// Takes a notice that the library of `provider` changed: the lists refresh when they are
    /// that provider's (`likes_only` for a written like: only the liked list).
    pub fn changed(&mut self, provider: &str, likes_only: bool) {
        if self.active.as_deref() != Some(provider) {
            return;
        }
        if likes_only {
            self.refresh_liked();
        } else {
            self.refresh_all();
        }
    }

    /// The first request of each list that refreshes, built now (see [`Self::pump`]).
    fn refresh_requests(&mut self) -> Vec<Request> {
        fn due<T>(slot: &mut Slot<T>, ids: &mut Ids, out: &mut Vec<Request>) {
            if slot.refresh_due()
                && let Some(r) = slot.request(ids)
            {
                out.push(r);
            }
        }
        let ids = &mut self.ids;
        let mut out = Vec::new();
        due(&mut self.playlists, ids, &mut out);
        due(&mut self.albums, ids, &mut out);
        due(&mut self.liked, ids, &mut out);
        for o in [
            &mut self.open_playlist,
            &mut self.open_album,
            &mut self.search,
        ]
        .into_iter()
        .flatten()
        {
            due(&mut o.tracks, ids, &mut out);
        }
        out
    }

    /// Sends the first request of each list that refreshes, whether or not a view shows it.
    pub fn pump(&mut self, player: &PlayerHandle) {
        if self.active.is_none() {
            return;
        }
        for request in self.refresh_requests() {
            self.send(player, request);
        }
    }

    /// The provider whose library the lists show.
    pub fn provider(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// Opens a playlist: its tracks load from the start and the previous one's request (if any)
    /// goes stale. Opening the one already open does nothing.
    pub fn open_playlist(&mut self, id: &str, name: &str) {
        if self.open_playlist.as_ref().is_some_and(|o| o.id == id) {
            return;
        }
        let serial = self.ids.next().0;
        self.open_playlist = Some(Open {
            id: id.to_owned(),
            name: name.to_owned(),
            images: Vec::new(),
            year: None,
            tracks: Slot::new(Source::PlaylistItems(id.to_owned()), serial),
        });
    }

    /// Opens an album, as [`Self::open_playlist`] does a playlist; its tracks take `images` and
    /// `year` (and the name) when they arrive without an album of their own (MD72).
    pub fn open_album(&mut self, id: &str, name: &str, images: &[ImageRef], year: Option<u16>) {
        if self.open_album.as_ref().is_some_and(|o| o.id == id) {
            return;
        }
        let serial = self.ids.next().0;
        self.open_album = Some(Open {
            id: id.to_owned(),
            name: name.to_owned(),
            images: images.to_vec(),
            year,
            tracks: Slot::new(Source::AlbumTracks(id.to_owned()), serial),
        });
    }

    /// Starts a search: the results load from the start and an older search's request goes stale.
    pub fn start_search(&mut self, query: &str) {
        let serial = self.ids.next().0;
        self.search = Some(Open {
            id: query.to_owned(),
            name: query.to_owned(),
            images: Vec::new(),
            year: None,
            tracks: Slot::new(Source::Search(query.to_owned()), serial),
        });
    }

    /// The request that loads the next page of `kind`, if one is due.
    pub fn next_page(&mut self, kind: ListKind) -> Option<Request> {
        let ids = &mut self.ids;
        match kind {
            ListKind::Playlists => self.playlists.request(ids),
            ListKind::Albums => self.albums.request(ids),
            ListKind::Liked => self.liked.request(ids),
            ListKind::OpenPlaylist => self.open_playlist.as_mut()?.tracks.request(ids),
            ListKind::OpenAlbum => self.open_album.as_mut()?.tracks.request(ids),
            ListKind::Search => self.search.as_mut()?.tracks.request(ids),
        }
    }

    /// Asks for the next page of `kind` (when due) from the active provider.
    pub fn request(&mut self, kind: ListKind, player: &PlayerHandle) {
        if self.active.is_none() {
            return;
        }
        if let Some(request) = self.next_page(kind) {
            self.send(player, request);
        }
    }

    /// Clears the failure of `kind` and asks again from where it stopped.
    pub fn retry(&mut self, kind: ListKind, player: &PlayerHandle) {
        match kind {
            ListKind::Playlists => self.playlists.failed = false,
            ListKind::Albums => self.albums.failed = false,
            ListKind::Liked => self.liked.failed = false,
            ListKind::OpenPlaylist => {
                if let Some(o) = &mut self.open_playlist {
                    o.tracks.failed = false;
                }
            }
            ListKind::OpenAlbum => {
                if let Some(o) = &mut self.open_album {
                    o.tracks.failed = false;
                }
            }
            ListKind::Search => {
                if let Some(o) = &mut self.search {
                    o.tracks.failed = false;
                }
            }
        }
        self.request(kind, player);
    }

    /// Sends `request` to the active provider.
    pub fn send(&self, player: &PlayerHandle, request: Request) {
        let Some(provider) = self.active.clone() else {
            return;
        };
        player.dispatch(PlayerAction::Library {
            request: request.id,
            provider,
            query: request.query,
        });
    }

    /// Takes a library answer into the list that waits for it; an answer no list waits for is
    /// stale and is dropped.
    pub fn push_page(&mut self, answer: LibraryAnswer) {
        let LibraryAnswer { request, result } = answer;
        if self.playlists.is_pending(request) {
            self.playlists.finish(result, playlists_of);
        } else if self.albums.is_pending(request) {
            self.albums.finish(result, albums_of);
        } else if self.liked.is_pending(request) {
            self.liked.finish(result, tracks_of);
        } else if let Some(o) = self
            .open_playlist
            .as_mut()
            .filter(|o| o.tracks.is_pending(request))
        {
            let refusal = matches!(&result, Err(e) if e.kind == LibErrorKind::NotReadable);
            if refusal {
                self.refused.insert(o.id.clone());
            } else if result.is_ok() {
                self.refused.remove(&o.id);
            }
            o.tracks.finish(result, tracks_of);
        } else if let Some(o) = self
            .open_album
            .as_mut()
            .filter(|o| o.tracks.is_pending(request))
        {
            let Open {
                name,
                images,
                year,
                tracks,
                ..
            } = o;
            tracks.finish(result, |page| {
                let (mut items, next, total) = tracks_of(page)?;
                fill_from_album(&mut items, name, images, *year);
                Some((items, next, total))
            });
        } else if let Some(o) = self
            .search
            .as_mut()
            .filter(|o| o.tracks.is_pending(request))
        {
            o.tracks.finish(result, tracks_of);
        }
    }

    /// Whether the provider refused to read playlist `id` this session.
    pub fn is_refused(&self, id: &str) -> bool {
        self.refused.contains(id)
    }

    /// Keeps a prefill answer until the account view takes it.
    pub fn push_prefill(&mut self, answer: PrefillAnswer) {
        if self.prefills.len() >= MAX_PREFILLS {
            self.prefills.pop_front();
        }
        self.prefills.push_back(answer);
    }

    /// Takes the answer to prefill `request`, if it has come: `Some(value)` (the value may be
    /// `None`: nothing stored).
    pub fn take_prefill(&mut self, request: RequestId) -> Option<Option<String>> {
        let at = self.prefills.iter().position(|a| a.request == request)?;
        self.prefills.remove(at).map(|a| a.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use whipspot_core::provider::{PlaylistPage, Readable, TrackPage};

    fn playlist(id: &str) -> Playlist {
        Playlist {
            id: id.into(),
            name: id.into(),
            readable: Readable::Readable,
            track_count: None,
            images: vec![],
        }
    }

    fn track(n: u32) -> Track {
        Track {
            uri: format!("t:{n}"),
            name: format!("t{n}"),
            artist: "a".into(),
            album: "b".into(),
            duration_ms: 1000,
            images: vec![],
            year: None,
        }
    }

    fn tracks(items: Vec<Track>, next_offset: Option<u32>) -> Result<LibraryPage, LibraryError> {
        Ok(LibraryPage::Tracks(TrackPage {
            items,
            skipped: 0,
            next_offset,
            total: None,
            truncated: false,
        }))
    }

    fn ready() -> LibraryModel {
        let mut m = LibraryModel::default();
        m.sync(Some("p"));
        m
    }

    fn entry(id: &'static str, library: bool) -> ProviderEntry {
        use whipspot_core::provider::model::{Capabilities, ProviderInfo};
        ProviderEntry {
            info: ProviderInfo {
                id,
                name: id,
                glyph: Default::default(),
                caps: Capabilities {
                    library,
                    pcm_tap: true,
                    eq: true,
                    search: true,
                    likes: false,
                },
                auth_steps: Vec::new(),
            },
            auth: None,
            likes_writable: false,
        }
    }

    #[test]
    fn the_selected_provider_is_used_and_a_missing_one_falls_back_to_the_first() {
        let providers = [
            entry("one", true),
            entry("nolib", false),
            entry("two", true),
        ];
        assert_eq!(Access::of(&providers, None), Access::Ready("one"));
        assert_eq!(Access::of(&providers, Some("two")), Access::Ready("two"));
        // A provider without a library cannot be selected, nor can one that is gone.
        assert_eq!(Access::of(&providers, Some("nolib")), Access::Ready("one"));
        assert_eq!(Access::of(&providers, Some("gone")), Access::Ready("one"));
        assert_eq!(Access::of(&[], Some("two")), Access::NoProvider);
    }

    /// A provider with two logins and a switch step, with `states` as its last status answer.
    fn with_switch(states: Option<[LoginState; 3]>) -> ProviderEntry {
        use whipspot_core::provider::model::{AuthStepInfo, AuthStepStatus, ProviderAuthStatus};
        let step = |id, switch| AuthStepInfo {
            id,
            label: id,
            inputs: vec![],
            hint: None,
            switch,
        };
        let mut e = entry("stream", true);
        e.info.auth_steps = vec![
            step("playback", false),
            step("web", false),
            step("connect", true),
        ];
        e.auth = states.map(|states| ProviderAuthStatus {
            steps: ["playback", "web", "connect"]
                .into_iter()
                .zip(states)
                .map(|(id, state)| AuthStepStatus {
                    id: id.to_owned(),
                    state,
                    action: None,
                    detail: None,
                })
                .collect(),
        });
        e
    }

    #[test]
    fn a_switch_step_never_gates_the_library() {
        use LoginState::{LoggedIn as In, LoggedOut as Out};
        let check = |states: [LoginState; 3], want: Access<'_>| {
            assert_eq!(
                Access::of(&[with_switch(Some(states.clone()))], None),
                want,
                "{states:?}"
            );
        };
        // The switch off (an optional feature turned off) with both logins in: the library stays open.
        check([In, In, Out], Access::Ready("stream"));
        check([In, In, In], Access::Ready("stream"));
        // A login out still closes it, whatever the switch says.
        check([In, Out, In], Access::LoggedOut);
        check([Out, In, In], Access::LoggedOut);
        assert_eq!(Access::of(&[with_switch(None)], None), Access::Checking);
        // A provider whose only step is a switch is ready before its first answer.
        let mut only = with_switch(None);
        only.info.auth_steps.drain(..2);
        assert_eq!(Access::of(&[only], None), Access::Ready("stream"));
    }

    /// The liked list with `n` tracks loaded and complete.
    fn liked_loaded(m: &mut LibraryModel, n: u32) {
        let req = m.next_page(ListKind::Liked).expect("request");
        m.push_page(LibraryAnswer {
            request: req.id,
            result: tracks((1..=n).map(track).collect(), None),
        });
        assert_eq!(m.liked.items().len(), n as usize);
        assert!(!m.liked.has_more());
    }

    #[test]
    fn a_change_refreshes_in_place_and_the_first_page_replaces_the_rows() {
        let mut m = ready();
        liked_loaded(&mut m, 3);
        let serial = m.liked.serial();
        m.changed("p", true);
        // The rows stay on show and the list keeps its identity (its scroll) while it reloads.
        assert_eq!(m.liked.items().len(), 3);
        assert_eq!(m.liked.serial(), serial);
        let reqs = m.refresh_requests();
        assert_eq!(reqs.len(), 1, "only the liked list was loaded: {reqs:?}");
        assert_eq!(reqs[0].query, LibraryQuery::Liked { offset: 0 });
        assert!(m.refresh_requests().is_empty(), "asked once");
        m.push_page(LibraryAnswer {
            request: reqs[0].id,
            result: tracks(vec![track(1), track(3)], None),
        });
        let uris: Vec<_> = m.liked.items().iter().map(|t| t.uri.as_str()).collect();
        assert_eq!(uris, ["t:1", "t:3"], "replaced, not appended");
        // The pages after the first append again.
        m.changed("p", true);
        let r = m.refresh_requests().remove(0);
        m.push_page(LibraryAnswer {
            request: r.id,
            result: tracks(vec![track(7)], Some(1)),
        });
        let r = m.next_page(ListKind::Liked).expect("the second page");
        m.push_page(LibraryAnswer {
            request: r.id,
            result: tracks(vec![track(8)], None),
        });
        assert_eq!(m.liked.items().len(), 2);
    }

    #[test]
    fn a_change_of_another_provider_or_an_unloaded_list_asks_nothing() {
        let mut m = ready();
        liked_loaded(&mut m, 2);
        m.changed("q", false);
        assert!(m.refresh_requests().is_empty(), "not the provider on show");
        // A library change refreshes every list that loaded, not the ones never asked for.
        m.changed("p", false);
        let reqs = m.refresh_requests();
        assert_eq!(reqs.len(), 1, "{reqs:?}");
        assert_eq!(reqs[0].query, LibraryQuery::Liked { offset: 0 });
    }

    #[test]
    fn a_refresh_drops_the_answer_in_flight_and_a_failed_one_keeps_the_rows_until_retry() {
        let mut m = ready();
        liked_loaded(&mut m, 2);
        m.changed("p", false);
        let first = m.refresh_requests().remove(0);
        // A second change before the answer: the first answer is stale.
        m.changed("p", false);
        let second = m.refresh_requests().remove(0);
        m.push_page(LibraryAnswer {
            request: first.id,
            result: tracks(vec![track(9)], None),
        });
        assert_eq!(m.liked.items().len(), 2, "stale answer dropped");
        m.push_page(LibraryAnswer {
            request: second.id,
            result: Err(LibraryError::new(LibErrorKind::Http, "x")),
        });
        assert!(m.liked.failed());
        assert_eq!(m.liked.items().len(), 2, "the rows stay after a failure");
        assert!(m.refresh_requests().is_empty(), "waits for a retry");
        m.liked.failed = false;
        let retry = m.next_page(ListKind::Liked).expect("retry");
        m.push_page(LibraryAnswer {
            request: retry.id,
            result: tracks(vec![track(5)], None),
        });
        assert_eq!(m.liked.items().len(), 1, "the retry still replaces");
    }

    #[test]
    fn the_choice_is_shared_between_clones() {
        let choice = ProviderChoice::new(Some("one".into()));
        let other = choice.clone();
        assert_eq!(other.get().as_deref(), Some("one"));
        other.set("two");
        assert_eq!(choice.get().as_deref(), Some("two"));
        assert_eq!(ProviderChoice::default().get(), None);
    }

    #[test]
    fn switching_provider_restarts_the_lists_and_drops_the_old_answers() {
        let mut m = ready();
        let old = m.next_page(ListKind::Liked).expect("request");
        let serial = m.liked.serial();
        m.sync(Some("q"));
        assert_eq!(m.provider(), Some("q"));
        assert_ne!(m.liked.serial(), serial, "the lists are new");
        assert!(m.liked.items().is_empty());
        m.push_page(LibraryAnswer {
            request: old.id,
            result: tracks(vec![track(1)], None),
        });
        assert!(
            m.liked.items().is_empty(),
            "the old provider's answer is stale"
        );
        // The new provider's own first request is asked for afresh.
        let fresh = m.next_page(ListKind::Liked).expect("request");
        assert_ne!(fresh.id, old.id);
    }

    #[test]
    fn pages_append_and_the_next_one_is_asked_for() {
        let mut m = ready();
        let first = m.next_page(ListKind::Liked).expect("first page");
        assert_eq!(first.query, LibraryQuery::Liked { offset: 0 });
        assert!(m.next_page(ListKind::Liked).is_none(), "one at a time");
        m.push_page(LibraryAnswer {
            request: first.id,
            result: tracks(vec![track(1), track(2)], Some(2)),
        });
        assert_eq!(m.liked.items().len(), 2);
        let second = m.next_page(ListKind::Liked).expect("second page");
        assert_eq!(second.query, LibraryQuery::Liked { offset: 2 });
        m.push_page(LibraryAnswer {
            request: second.id,
            result: tracks(vec![track(3)], None),
        });
        assert_eq!(m.liked.items().len(), 3);
        assert!(m.next_page(ListKind::Liked).is_none(), "complete");
    }

    #[test]
    fn a_stale_answer_is_dropped() {
        let mut m = ready();
        m.open_playlist("a", "A");
        let old = m.next_page(ListKind::OpenPlaylist).expect("request");
        m.open_playlist("b", "B");
        m.push_page(LibraryAnswer {
            request: old.id,
            result: tracks(vec![track(1)], None),
        });
        let open = m.open_playlist.as_ref().expect("open");
        assert_eq!(open.id, "b");
        assert!(open.tracks.items().is_empty());
        // Nothing waits for an id nobody asked for, either.
        m.push_page(LibraryAnswer {
            request: RequestId(9999),
            result: tracks(vec![track(2)], None),
        });
        assert!(
            m.open_playlist
                .as_ref()
                .expect("open")
                .tracks
                .items()
                .is_empty()
        );
    }

    #[test]
    fn r1_album_tracks_without_an_album_take_the_routes_name_images_and_year() {
        let cover = ImageRef {
            url: "https://c/x/300".into(),
            width: Some(300),
            height: Some(300),
        };
        let mut m = ready();
        m.open_album(
            "al",
            "Low Tide Sessions",
            std::slice::from_ref(&cover),
            Some(2019),
        );
        let req = m.next_page(ListKind::OpenAlbum).expect("request");
        let own = Track {
            album: "Elsewhere".into(),
            year: Some(1999),
            ..track(2)
        };
        m.push_page(LibraryAnswer {
            request: req.id,
            result: tracks(
                vec![
                    Track {
                        album: String::new(),
                        ..track(1)
                    },
                    own.clone(),
                ],
                None,
            ),
        });
        let items = m.open_album.as_ref().expect("open").tracks.items();
        assert_eq!(items[0].album, "Low Tide Sessions");
        assert_eq!(items[0].images, vec![cover]);
        assert_eq!(items[0].year, Some(2019));
        assert_eq!(items[1], own, "a track with its own album is left as it is");
        // Playlist and search tracks are never filled.
        m.open_playlist("pl", "PL");
        let req = m.next_page(ListKind::OpenPlaylist).expect("request");
        m.push_page(LibraryAnswer {
            request: req.id,
            result: tracks(
                vec![Track {
                    album: String::new(),
                    ..track(3)
                }],
                None,
            ),
        });
        let items = m.open_playlist.as_ref().expect("open").tracks.items();
        assert_eq!(items[0].album, "");
    }

    #[test]
    fn a_failed_page_waits_for_a_retry() {
        let mut m = ready();
        let req = m.next_page(ListKind::Playlists).expect("request");
        m.push_page(LibraryAnswer {
            request: req.id,
            result: Err(LibraryError::new(
                whipspot_core::provider::LibErrorKind::Network,
                "secret detail",
            )),
        });
        assert!(m.playlists.failed());
        assert!(m.next_page(ListKind::Playlists).is_none());
        m.playlists.failed = false;
        let again = m.next_page(ListKind::Playlists).expect("retried");
        assert_eq!(again.query, LibraryQuery::Playlists { offset: 0 });
        m.push_page(LibraryAnswer {
            request: again.id,
            result: Ok(LibraryPage::Playlists(PlaylistPage {
                items: vec![playlist("x")],
                skipped: 0,
                next_offset: None,
                total: None,
            })),
        });
        assert_eq!(m.playlists.items().len(), 1);
    }

    #[test]
    fn a_login_change_starts_the_lists_afresh() {
        let mut m = ready();
        let req = m.next_page(ListKind::Liked).expect("request");
        m.sync(None);
        m.sync(Some("p"));
        m.push_page(LibraryAnswer {
            request: req.id,
            result: tracks(vec![track(1)], None),
        });
        assert!(m.liked.items().is_empty());
        let fresh = m.next_page(ListKind::Liked).expect("asks again");
        assert_ne!(fresh.id, req.id);
    }

    #[test]
    fn a_page_that_does_not_advance_ends_the_list() {
        let mut m = ready();
        let req = m.next_page(ListKind::Liked).expect("request");
        m.push_page(LibraryAnswer {
            request: req.id,
            result: tracks(Vec::new(), Some(0)),
        });
        assert!(m.next_page(ListKind::Liked).is_none());
    }

    fn refusal() -> Result<LibraryPage, LibraryError> {
        Err(LibraryError::new(
            whipspot_core::provider::LibErrorKind::NotReadable,
            "provider detail",
        ))
    }

    #[test]
    fn d1_a_not_readable_page_marks_only_that_playlist_and_a_later_page_clears_it() {
        let mut m = ready();
        m.open_playlist("demo-playlist-2", "Old Demos");
        let req = m.next_page(ListKind::OpenPlaylist).expect("request");
        m.push_page(LibraryAnswer {
            request: req.id,
            result: refusal(),
        });
        assert!(m.is_refused("demo-playlist-2"));
        assert!(!m.is_refused("demo-playlist-1"));
        assert!(m.open_playlist.as_ref().expect("open").tracks.failed());

        // Retry: the failure clears and the model builds the request again.
        m.open_playlist.as_mut().expect("open").tracks.failed = false;
        let again = m.next_page(ListKind::OpenPlaylist).expect("retry request");
        assert!(
            m.is_refused("demo-playlist-2"),
            "still refused until answered"
        );
        let pending = (again.id,);
        m.push_page(LibraryAnswer {
            request: pending.0,
            result: tracks(vec![track(1)], None),
        });
        assert!(!m.is_refused("demo-playlist-2"));
    }

    #[test]
    fn d1_other_errors_do_not_mark_a_playlist_refused() {
        let mut m = ready();
        m.open_playlist("p1", "P1");
        let req = m.next_page(ListKind::OpenPlaylist).expect("request");
        m.push_page(LibraryAnswer {
            request: req.id,
            result: Err(LibraryError::new(LibErrorKind::Network, "x")),
        });
        assert!(!m.is_refused("p1"));
        assert!(m.open_playlist.as_ref().expect("open").tracks.failed());
    }

    #[test]
    fn d2_a_predicted_unreadable_playlist_is_opened_and_asked_for_like_any_other() {
        let mut m = ready();
        let unreadable = Playlist {
            readable: Readable::Unreadable,
            ..playlist("demo-playlist-2")
        };
        m.open_playlist(&unreadable.id, &unreadable.name);
        let req = m
            .next_page(ListKind::OpenPlaylist)
            .expect("the open request");
        assert_eq!(
            req.query,
            LibraryQuery::PlaylistItems {
                id: "demo-playlist-2".into(),
                offset: 0
            }
        );
    }

    #[test]
    fn a_page_total_is_kept_with_the_list() {
        let mut m = ready();
        let req = m.next_page(ListKind::Playlists).expect("request");
        m.push_page(LibraryAnswer {
            request: req.id,
            result: Ok(LibraryPage::Playlists(PlaylistPage {
                items: vec![playlist("x")],
                skipped: 0,
                next_offset: Some(1),
                total: Some(8),
            })),
        });
        assert_eq!(m.playlists.total(), Some(8));
    }
}

//! Explorer navigation (MD54): the routes the browse sheet shows and a browser-style history over
//! them. Pure data: no `Ui`, no player.

use whipspot_core::provider::ImageRef;

/// The most entries the history keeps; the oldest is dropped past it.
pub const HISTORY_CAP: usize = 64;

/// The top-bar tabs. The settings key of each is stable (`last_view`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Tab {
    #[default]
    Home,
    Playlists,
    Albums,
    Liked,
    Search,
}

impl Tab {
    /// The tabs in top-bar order.
    pub const ALL: [Tab; 5] = [
        Tab::Home,
        Tab::Playlists,
        Tab::Albums,
        Tab::Liked,
        Tab::Search,
    ];

    /// The stable key the settings file stores.
    pub fn key(self) -> &'static str {
        match self {
            Tab::Home => "home",
            Tab::Playlists => "playlists",
            Tab::Albums => "albums",
            Tab::Liked => "liked",
            Tab::Search => "search",
        }
    }

    /// The tab stored under `key`; an unknown key (and the old "account") gives [`Tab::Home`].
    pub fn from_key(key: &str) -> Tab {
        Tab::ALL
            .into_iter()
            .find(|t| t.key() == key)
            .unwrap_or_default()
    }

    /// The tab's label.
    pub fn label(self) -> &'static str {
        match self {
            Tab::Home => "Home",
            Tab::Playlists => "Playlists",
            Tab::Albums => "Albums",
            Tab::Liked => "Liked",
            Tab::Search => "Search",
        }
    }

    /// The route a click on the tab pushes.
    pub fn root(self) -> Route {
        match self {
            Tab::Home => Route::Home,
            Tab::Playlists => Route::Playlists,
            Tab::Albums => Route::Albums,
            Tab::Liked => Route::Liked,
            Tab::Search => Route::Search,
        }
    }
}

/// What the browse sheet shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Route {
    #[default]
    Home,
    Playlists,
    Albums,
    Liked,
    Search,
    /// A playlist; `images` are its covers, for the hero (MD72, C6).
    Playlist {
        id: String,
        name: String,
        images: Vec<ImageRef>,
    },
    /// An album; `images` and `year` come from the card that opened it and fill its tracks (MD72).
    /// With a `uri` (the provider can explore) it is addressed by that uri and shows the explored
    /// album page; without one it is the plain album page, addressed by `id`.
    Album {
        id: String,
        uri: String,
        name: String,
        artist: String,
        images: Vec<ImageRef>,
        year: Option<u16>,
    },
    /// An artist's page, opened by a link; `images` are the portraits the link knew (often none).
    Artist {
        uri: String,
        name: String,
        images: Vec<ImageRef>,
    },
    Queue,
    Account,
    /// The settings page (MD82, MD87).
    Settings,
    /// The equalizer pane (MD82, MD83).
    Equalizer,
}

impl Route {
    /// The route's title, for the routes that are panes rather than lists.
    pub fn title(&self) -> &'static str {
        match self {
            Route::Home => "Home",
            Route::Playlists => "Playlists",
            Route::Albums => "Albums",
            Route::Liked => "Liked songs",
            Route::Search => "Search",
            Route::Playlist { .. } => "Playlist",
            Route::Album { .. } => "Album",
            Route::Artist { .. } => "Artist",
            Route::Queue => "Queue",
            Route::Account => "Accounts",
            Route::Settings => "Settings",
            Route::Equalizer => "Equalizer",
        }
    }

    /// The route's name for a breadcrumb: the playlist, album or artist's own name when it has
    /// one, else [`Self::title`].
    pub fn label(&self) -> String {
        match self {
            Route::Playlist { name, .. }
            | Route::Album { name, .. }
            | Route::Artist { name, .. }
                if !name.is_empty() =>
            {
                name.clone()
            }
            other => other.title().to_owned(),
        }
    }

    /// What identifies an album route: its uri when it has one, else its id.
    pub fn album_key(&self) -> Option<&str> {
        match self {
            Route::Album { id, uri, .. } => Some(if uri.is_empty() { id } else { uri }),
            _ => None,
        }
    }
}

/// The tab a route belongs to (highlighted in the top bar); none for the panes.
pub fn tab_of(route: &Route) -> Option<Tab> {
    match route {
        Route::Home => Some(Tab::Home),
        Route::Playlists | Route::Playlist { .. } => Some(Tab::Playlists),
        Route::Albums | Route::Album { .. } => Some(Tab::Albums),
        Route::Liked => Some(Tab::Liked),
        Route::Search => Some(Tab::Search),
        Route::Artist { .. }
        | Route::Queue
        | Route::Account
        | Route::Settings
        | Route::Equalizer => None,
    }
}

/// A browser-style history: a push truncates what lies forward of the current entry.
#[derive(Debug, Clone)]
pub struct History {
    entries: Vec<Route>,
    at: usize,
}

impl History {
    /// A history holding only `start`.
    pub fn new(start: Route) -> History {
        History {
            entries: vec![start],
            at: 0,
        }
    }

    /// The route on show.
    pub fn current(&self) -> &Route {
        &self.entries[self.at]
    }

    /// Opens `route`: forward entries are dropped; the current route again is a no-op; past
    /// [`HISTORY_CAP`] entries the oldest goes.
    pub fn push(&mut self, route: Route) {
        if *self.current() == route {
            return;
        }
        self.entries.truncate(self.at + 1);
        self.entries.push(route);
        if self.entries.len() > HISTORY_CAP {
            self.entries.remove(0);
        }
        self.at = self.entries.len() - 1;
    }

    /// The entry before the current one, if any (a breadcrumb names it).
    pub fn previous(&self) -> Option<&Route> {
        self.at.checked_sub(1).map(|i| &self.entries[i])
    }

    /// Goes one entry back; whether there was one.
    pub fn back(&mut self) -> bool {
        let ok = self.can_back();
        if ok {
            self.at -= 1;
        }
        ok
    }

    /// Goes one entry forward; whether there was one.
    pub fn forward(&mut self) -> bool {
        let ok = self.can_forward();
        if ok {
            self.at += 1;
        }
        ok
    }

    pub fn can_back(&self) -> bool {
        self.at > 0
    }

    pub fn can_forward(&self) -> bool {
        self.at + 1 < self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playlist(n: u32) -> Route {
        Route::Playlist {
            id: format!("p{n}"),
            name: format!("List {n}"),
            images: vec![],
        }
    }

    fn artist(name: &str) -> Route {
        Route::Artist {
            uri: format!("demo:artist:{name}"),
            name: name.into(),
            images: vec![],
        }
    }

    fn cover(url: &str) -> ImageRef {
        ImageRef {
            url: url.into(),
            width: Some(300),
            height: Some(300),
        }
    }

    #[test]
    fn h1_push_back_forward() {
        let mut h = History::new(Route::Home);
        assert!(!h.can_back() && !h.can_forward());
        h.push(Route::Albums);
        h.push(Route::Liked);
        assert_eq!(*h.current(), Route::Liked);
        assert!(h.back());
        assert_eq!(*h.current(), Route::Albums);
        assert!(h.can_forward());
        assert!(h.forward());
        assert_eq!(*h.current(), Route::Liked);
        assert!(!h.forward());
        assert!(h.back() && h.back());
        assert!(!h.back());
        assert_eq!(*h.current(), Route::Home);
    }

    #[test]
    fn h1_a_push_truncates_forward() {
        let mut h = History::new(Route::Home);
        h.push(Route::Albums);
        h.push(Route::Liked);
        h.back();
        h.push(Route::Search);
        assert!(!h.can_forward());
        assert_eq!(*h.current(), Route::Search);
        h.back();
        assert_eq!(*h.current(), Route::Albums);
    }

    #[test]
    fn h1_no_duplicate_push() {
        let mut h = History::new(Route::Home);
        h.push(Route::Home);
        assert!(!h.can_back());
        h.push(Route::Albums);
        h.push(Route::Albums);
        assert!(h.back());
        assert_eq!(*h.current(), Route::Home);
        assert!(!h.back());
    }

    #[test]
    fn h1_cap_drops_the_oldest() {
        let mut h = History::new(Route::Home);
        for n in 0..(HISTORY_CAP as u32 + 10) {
            h.push(playlist(n));
        }
        let mut steps = 0;
        while h.back() {
            steps += 1;
        }
        assert_eq!(steps, HISTORY_CAP - 1);
        assert_ne!(*h.current(), Route::Home);
    }

    #[test]
    fn h2_tab_of_every_route() {
        let album = Route::Album {
            id: "a".into(),
            uri: String::new(),
            name: "A".into(),
            artist: "B".into(),
            images: vec![cover("a")],
            year: Some(2021),
        };
        let cases = [
            (Route::Home, Some(Tab::Home)),
            (Route::Playlists, Some(Tab::Playlists)),
            (playlist(1), Some(Tab::Playlists)),
            (Route::Albums, Some(Tab::Albums)),
            (album, Some(Tab::Albums)),
            (Route::Liked, Some(Tab::Liked)),
            (Route::Search, Some(Tab::Search)),
            (artist("Velmora"), None),
            (Route::Queue, None),
            (Route::Account, None),
            (Route::Settings, None),
            (Route::Equalizer, None),
        ];
        for (route, tab) in cases {
            assert_eq!(tab_of(&route), tab, "{route:?}");
        }
        assert_eq!(Route::Settings.title(), "Settings");
        assert_eq!(Route::Equalizer.title(), "Equalizer");
    }

    #[test]
    fn r3_routes_carry_their_covers_and_compare_by_all_fields() {
        let album = |images: Vec<ImageRef>, year| Route::Album {
            id: "a".into(),
            uri: String::new(),
            name: "A".into(),
            artist: "B".into(),
            images,
            year,
        };
        let mut h = History::new(Route::Albums);
        h.push(album(vec![cover("x")], Some(2020)));
        // The same album again (same fields) is a no-op; other covers are another entry.
        h.push(album(vec![cover("x")], Some(2020)));
        assert!(h.back() && !h.back(), "one entry was pushed");
        h.forward();
        h.push(album(vec![cover("y")], Some(2020)));
        h.push(album(vec![cover("y")], Some(2021)));
        let mut steps = 0;
        while h.back() {
            steps += 1;
        }
        assert_eq!(steps, 3);
        h.forward();
        let Route::Album { images, year, .. } = h.current() else {
            panic!("an album route");
        };
        assert_eq!((images.len(), *year), (1, Some(2020)));
        let list = Route::Playlist {
            id: "p".into(),
            name: "P".into(),
            images: vec![cover("z")],
        };
        h.push(list.clone());
        assert_eq!(h.current(), &list);
        assert_eq!(tab_of(&list), Some(Tab::Playlists));
        assert!(h.back());
        assert_eq!(tab_of(h.current()), Some(Tab::Albums));
        h.forward();
        assert_eq!(h.current(), &list);
    }

    #[test]
    fn k1_tab_keys() {
        for tab in Tab::ALL {
            assert_eq!(Tab::from_key(tab.key()), tab);
            assert_eq!(tab_of(&tab.root()), Some(tab));
        }
        assert_eq!(Tab::from_key("account"), Tab::Home);
        assert_eq!(Tab::from_key("nonsense"), Tab::Home);
        assert_eq!(Tab::from_key(""), Tab::Home);
    }

    #[test]
    fn e1_breadcrumbs_name_the_previous_entry_and_albums_are_keyed_by_uri() {
        let mut h = History::new(Route::Home);
        assert!(h.previous().is_none());
        h.push(artist("Velmora"));
        assert_eq!(h.previous(), Some(&Route::Home));
        let linked = Route::Album {
            id: String::new(),
            uri: "demo:album:1".into(),
            name: "Low Tide".into(),
            artist: String::new(),
            images: vec![],
            year: None,
        };
        h.push(linked.clone());
        assert_eq!(h.previous().map(Route::label), Some("Velmora".to_owned()));
        assert_eq!(linked.album_key(), Some("demo:album:1"));
        assert_eq!(linked.label(), "Low Tide");
        assert_eq!(Route::Liked.album_key(), None);
        assert_eq!(Route::Home.label(), "Home");
        assert_eq!(artist("").label(), "Artist");
        assert_eq!(tab_of(&artist("x")), None);
        // Going back leaves a previous entry only while there is one.
        assert!(h.back() && h.back() && h.previous().is_none());
    }
}

//! Models that cross to the webview. The login and library models keep the field names of the
//! original Spotify-only wire format.

use std::collections::BTreeMap;

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum LoginState {
    LoggedOut,
    LoggedIn,
    /// A whitelisted message (the provider's fixed auth error text), never a library error.
    Error {
        message: String,
    },
}

/// Most image refs an adapter keeps per item.
pub const MAX_IMAGES: usize = 4;

/// One image of an item (a cover) as a provider names it. Provider data: only an `ArtSource`
/// fetches it, after its own checks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRef {
    pub url: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl ImageRef {
    /// The smallest ref whose known width is at least `px`, else the widest known, else the
    /// first; `None` when `images` is empty.
    pub fn pick(images: &[ImageRef], px: u32) -> Option<&ImageRef> {
        images
            .iter()
            .filter(|i| i.width.is_some_and(|w| w >= px))
            .min_by_key(|i| i.width)
            .or_else(|| {
                images
                    .iter()
                    .filter(|i| i.width.is_some())
                    .max_by_key(|i| i.width)
            })
            .or_else(|| images.first())
    }
}

/// Most artists a track or an album names as links; the rest stay in the display string.
pub const MAX_ARTISTS: usize = 8;

/// Most popular tracks an artist page carries.
pub const MAX_POPULAR: usize = 10;

/// A named thing a link opens: an artist or an album. `uri` has the provider's own scheme
/// (`spotify:artist:<id>`, `local:album:<hex>`), so `ProviderRegistry::resolve_uri` finds the
/// provider that made it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntityRef {
    pub uri: String,
    /// Display-clean (see [`Track::name`]).
    pub name: String,
}

/// A track that passed validation. `uri` is the provider's own track URI (Spotify: exactly `spotify:track:<22 base62>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub uri: String,
    /// `name`, `artist` and `album` are display-clean: providers pass them through
    /// [`crate::text::display_text`].
    pub name: String,
    /// Artist names joined with ", ".
    pub artist: String,
    pub album: String,
    pub duration_ms: u64,
    /// Its album's covers, at most [`MAX_IMAGES`].
    pub images: Vec<ImageRef>,
    /// Its album's release year.
    pub year: Option<u16>,
    /// The artists as links, at most [`MAX_ARTISTS`], in the order of `artist`. Empty when the
    /// provider cannot name them (no `explore`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub artists: Vec<EntityRef>,
    /// The album as a link; `None` when the provider cannot name it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_ref: Option<EntityRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackPage {
    pub items: Vec<Track>,
    /// Entries dropped (null, episode, local file, bad URI, no object).
    pub skipped: u32,
    /// Offset to request next, or `None` when there is nothing more (or the row cap is reached).
    pub next_offset: Option<u32>,
    pub total: Option<u32>,
    /// True when more rows exist but the `MAX_ROWS` cap stops the listing.
    pub truncated: bool,
}

/// Whether the current user can read a playlist's items. A prediction only: selecting the
/// playlist always asks, and the response is authoritative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Readable {
    Readable,
    Unreadable,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: String,
    /// Display-clean (see [`Track::name`]).
    pub name: String,
    pub readable: Readable,
    pub track_count: Option<u32>,
    /// Covers, at most [`MAX_IMAGES`].
    pub images: Vec<ImageRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistPage {
    pub items: Vec<Playlist>,
    pub skipped: u32,
    pub next_offset: Option<u32>,
    pub total: Option<u32>,
}

/// What kind of release an album is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AlbumKind {
    Album,
    Single,
    Compilation,
    /// A release that lists the artist as a guest.
    AppearsOn,
    #[default]
    Unknown,
}

impl AlbumKind {
    fn is_unknown(&self) -> bool {
        *self == AlbumKind::Unknown
    }
}

/// A saved album, as a tree entry. `id` is 22 base62 characters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Album {
    pub id: String,
    /// The album's own URI (`<scheme>:album:<id>`); empty when the provider cannot name it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub uri: String,
    /// Display-clean (see [`Track::name`]).
    pub name: String,
    /// Artist names joined with ", ", display-clean.
    pub artist: String,
    pub track_count: Option<u32>,
    /// Covers, at most [`MAX_IMAGES`].
    pub images: Vec<ImageRef>,
    pub year: Option<u16>,
    /// The artists as links, at most [`MAX_ARTISTS`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub artists: Vec<EntityRef>,
    #[serde(skip_serializing_if = "AlbumKind::is_unknown")]
    pub kind: AlbumKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumPage {
    pub items: Vec<Album>,
    pub skipped: u32,
    pub next_offset: Option<u32>,
    pub total: Option<u32>,
}

/// An artist, as a page header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Artist {
    pub uri: String,
    /// Display-clean (see [`Track::name`]).
    pub name: String,
    /// Portraits, at most [`MAX_IMAGES`].
    pub images: Vec<ImageRef>,
}

/// An artist page: the header, the popular tracks and the first page of releases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistPage {
    pub artist: Artist,
    /// At most [`MAX_POPULAR`]; empty when the provider has no such list.
    pub popular: Vec<Track>,
    /// The first page of releases, all groups together.
    pub releases: AlbumPage,
}

/// An album opened by its URI: the album and its first page of tracks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumDetail {
    pub album: Album,
    pub tracks: TrackPage,
}

/// Which releases of an artist a page lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReleaseGroup {
    All,
    Albums,
    Singles,
    AppearsOn,
}

/// What a provider can do; the UI hides what is off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// Has a [`super::Library`].
    pub library: bool,
    /// Decoded audio flows through the shared PCM tap (visualiser).
    pub pcm_tap: bool,
    /// Decoded audio flows through the shared EQ/balance pipeline.
    pub eq: bool,
    /// The library supports track search.
    pub search: bool,
    /// The library can say which tracks are saved and change that (needs `library`).
    pub likes: bool,
    /// The library opens artists and albums by URI (`Library::artist`, `album`,
    /// `artist_releases`) and names them on its tracks and albums (needs `library`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub explore: bool,
    /// `Library::liked` and `set_liked` also take album URIs (needs `likes`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub save_albums: bool,
}

/// One text input of a login step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputSpec {
    pub name: &'static str,
    pub label: &'static str,
    /// Documentation of the accepted shape; the provider validates authoritatively.
    pub pattern: &'static str,
    /// Short human description of the accepted shape ("32 hex characters"), for the placeholder
    /// and the "`<label>` must be `<format>`" message.
    pub format: &'static str,
    /// Longest accepted value in characters.
    pub max_len: u32,
}

/// One login a provider offers (Spotify has two: `playback` and `web`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStepInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub inputs: Vec<InputSpec>,
    /// Setup help shown above the inputs; `None` when the step needs none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<StepHint>,
    /// A switch for an optional feature (`LoggedIn` = on), not a login: it never gates the
    /// provider's library or playback, and it may start on. A front-end shows it like any step.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub switch: bool,
}

impl AuthStepInfo {
    /// Whether the step is a login, so that it must be logged in before the library can be used.
    pub fn is_login(&self) -> bool {
        !self.switch
    }
}

/// Help for a login step that needs setup outside the app (Spotify: register a redirect URI).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StepHint {
    /// Prose shown above the value.
    pub text: &'static str,
    /// A public value to copy (Spotify: the redirect URI); never a secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy: Option<&'static str>,
}

/// The neutral symbol a front-end shows for a provider (the library switch). Never a brand
/// mark: a provider picks the kind of source it is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderGlyph {
    /// A streaming service (music from the network).
    Stream,
    /// Files on this computer.
    Files,
    /// Anything else.
    #[default]
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    /// `[a-z][a-z0-9]{0,31}`; also the scheme of the provider's track URIs.
    pub id: &'static str,
    pub name: &'static str,
    /// The symbol the library switch shows for it (its name is the tooltip).
    pub glyph: ProviderGlyph,
    pub caps: Capabilities,
    pub auth_steps: Vec<AuthStepInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStepStatus {
    pub id: String,
    #[serde(flatten)]
    pub state: LoginState,
    /// Text of a primary button that calls `login(step, inputs)` in any state (for example
    /// "Install" or "Update"). Dynamic, so it lives here and not on the constant `AuthStepInfo`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    /// A one-line status under the state text. A fixed string, never a library error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Contains no secret; safe to send to the webview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthStatus {
    pub steps: Vec<AuthStepStatus>,
}

/// Login input values by input name. A provider rejects keys its step does not declare.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoginInput(pub BTreeMap<String, String>);

impl LoginInput {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: Option<u32>) -> ImageRef {
        ImageRef {
            url: format!("https://x.invalid/{w:?}"),
            width: w,
            height: w,
        }
    }

    #[test]
    fn pick_chooses_the_smallest_that_fits() {
        let imgs = [img(Some(640)), img(Some(300)), img(Some(64))];
        let w = |px| ImageRef::pick(&imgs, px).and_then(|i| i.width);
        assert_eq!(w(50), Some(64));
        assert_eq!(w(64), Some(64));
        assert_eq!(w(65), Some(300));
        assert_eq!(w(1000), Some(640));
        let unknown = [img(None), img(None)];
        assert_eq!(ImageRef::pick(&unknown, 100), Some(&unknown[0]));
        assert_eq!(ImageRef::pick(&[], 100), None);
        let mixed = [img(None), img(Some(200))];
        assert_eq!(ImageRef::pick(&mixed, 100), Some(&mixed[1]));
    }

    #[test]
    fn auth_status_serialises_step_state_flat() {
        let s = ProviderAuthStatus {
            steps: vec![
                AuthStepStatus {
                    id: "playback".into(),
                    state: LoginState::LoggedIn,
                    action: None,
                    detail: None,
                },
                AuthStepStatus {
                    id: "web".into(),
                    state: LoginState::Error {
                        message: "x".into(),
                    },
                    action: None,
                    detail: None,
                },
            ],
        };
        assert_eq!(
            serde_json::to_string(&s).unwrap(),
            r#"{"steps":[{"id":"playback","state":"loggedIn"},{"id":"web","state":"error","message":"x"}]}"#
        );
    }

    #[test]
    fn auth_status_serialises_action_and_detail_when_set() {
        let s = AuthStepStatus {
            id: "helper".into(),
            state: LoginState::LoggedIn,
            action: Some("Update".into()),
            detail: Some("v1".into()),
        };
        assert_eq!(
            serde_json::to_string(&s).unwrap(),
            r#"{"id":"helper","state":"loggedIn","action":"Update","detail":"v1"}"#
        );
    }

    #[test]
    fn info_serialises_camel_case() {
        let i = ProviderInfo {
            id: "p",
            name: "P",
            glyph: ProviderGlyph::Other,
            caps: Capabilities {
                library: true,
                pcm_tap: false,
                eq: false,
                search: true,
                likes: false,
                explore: false,
                save_albums: false,
            },
            auth_steps: vec![],
        };
        assert_eq!(
            serde_json::to_string(&i).unwrap(),
            r#"{"id":"p","name":"P","glyph":"other","caps":{"library":true,"pcmTap":false,"eq":false,"search":true,"likes":false},"authSteps":[]}"#
        );
        let mut on = i.clone();
        on.caps.explore = true;
        on.caps.save_albums = true;
        assert!(
            serde_json::to_string(&on)
                .unwrap()
                .contains(r#""likes":false,"explore":true,"saveAlbums":true}"#)
        );
    }
}

#[cfg(test)]
mod step_tests {
    use super::*;

    #[test]
    fn step_serialises_hint_and_input_format() {
        let step = AuthStepInfo {
            id: "web",
            label: "Library",
            inputs: vec![InputSpec {
                name: "k",
                label: "K",
                pattern: "^a$",
                format: "the letter a",
                max_len: 1,
            }],
            hint: Some(StepHint {
                text: "t",
                copy: Some("c"),
            }),
            switch: false,
        };
        assert_eq!(
            serde_json::to_string(&step).unwrap(),
            r#"{"id":"web","label":"Library","inputs":[{"name":"k","label":"K","pattern":"^a$","format":"the letter a","maxLen":1}],"hint":{"text":"t","copy":"c"}}"#
        );
        let none = AuthStepInfo {
            hint: None,
            inputs: vec![],
            ..step
        };
        assert_eq!(
            serde_json::to_string(&none).unwrap(),
            r#"{"id":"web","label":"Library","inputs":[]}"#
        );
        let switch = AuthStepInfo {
            switch: true,
            ..none
        };
        assert_eq!(
            serde_json::to_string(&switch).unwrap(),
            r#"{"id":"web","label":"Library","inputs":[],"switch":true}"#
        );
        assert!(!switch.is_login());
    }
}

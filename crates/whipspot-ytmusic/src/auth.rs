//! The provider's login: two steps in the Accounts pane (D3, D10).
//!
//! - `helper`: the stream helper (yt-dlp). It has no input. `login` enables it and installs or
//!   updates the managed copy when the user has no yt-dlp of their own; `logout` writes a
//!   `helper-off` flag file and deletes nothing.
//! - `cookies`: the path of a Netscape `cookies.txt`. Only the YouTube cookies are kept and
//!   stored as one secret in the credential store. The user's file is only read (D7).
//!
//! Every text a step shows or an error carries is a fixed string: no path, no cookie, no tool
//! output (hard rule 1).

use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use whipspot_core::credential_store::{CredentialStore, StoreError};
use whipspot_core::provider::model::LoginState;
use whipspot_core::provider::{
    AuthFailure, AuthFlow, AuthStepInfo, AuthStepStatus, BoxFuture, InputSpec, LoginInput,
    ProviderAuthStatus, StepHint,
};
use whipspot_core::secret::Secret;

use crate::cookies::{CookieError, Jar, MAX_FILE_BYTES};
use crate::resolve::Resolver;
use crate::tools::{QJS_VERSION, ToolError, ToolSet, Tools};

/// The credential-store account the first build of this provider kept the cookies in, as one
/// value. Nothing reads it any more; logout deletes it if it is there.
pub const COOKIES_ACCOUNT: &str = "ytmusic-cookies";
/// The account of the number of parts the cookies are stored in.
pub const COOKIES_COUNT_ACCOUNT: &str = "ytmusic-cookies-n";
/// The prefix every account of the cookies' parts has; the part's index follows.
pub const COOKIES_PART_PREFIX: &str = "ytmusic-cookies-";
/// The prefix every account of this provider has (the id of the provider), as Spotify's start
/// with `spotify-`.
pub const ACCOUNT_PREFIX: &str = "ytmusic-";
/// The most characters of one part: 1000 UTF-16 units are 2000 bytes, under the 2560 bytes of a
/// Windows Credential Manager blob.
pub const COOKIES_PART_CHARS: usize = 1000;
/// The most parts: 64,000 characters of cookies.
pub const COOKIES_MAX_PARTS: usize = 64;

/// The account of part `index` of the cookies.
pub fn cookies_part_account(index: usize) -> String {
    format!("{COOKIES_PART_PREFIX}{index}")
}

const fn has_prefix(s: &str, prefix: &str) -> bool {
    let (s, p) = (s.as_bytes(), prefix.as_bytes());
    if s.len() < p.len() {
        return false;
    }
    let mut i = 0;
    while i < p.len() {
        if s[i] != p[i] {
            return false;
        }
        i += 1;
    }
    true
}
const _: () = assert!(has_prefix(COOKIES_ACCOUNT, ACCOUNT_PREFIX));
const _: () = assert!(has_prefix(COOKIES_COUNT_ACCOUNT, ACCOUNT_PREFIX));
const _: () = assert!(has_prefix(COOKIES_PART_PREFIX, ACCOUNT_PREFIX));

pub const STEP_HELPER: &str = "helper";
pub const STEP_COOKIES: &str = "cookies";
pub const INPUT_PATH: &str = "path";
/// The flag file in the tools folder that switches the helper off.
pub const HELPER_OFF_FILE: &str = "helper-off";

/// The step state after InnerTube answered 401 or 403.
pub const MSG_COOKIES_EXPIRED: &str = "Cookies expired: import them again";
/// Shown where there is no managed install and no yt-dlp.
pub const MSG_INSTALL_YOURSELF: &str = "Install yt-dlp yourself";
pub const MSG_BAD_YTDLP_ENV: &str = "WHIPSPOT_YTDLP does not name a yt-dlp file";
const MSG_UNKNOWN_STEP: &str = "Unknown login step";
const MSG_UNKNOWN_INPUT: &str = "Unknown login input";
const MSG_NO_PATH: &str = "Enter the path to your cookies.txt";
const MSG_NOT_FOUND: &str = "Cookie file not found";
const MSG_UNREADABLE: &str = "Could not read the cookie file";
const MSG_NOT_A_FILE: &str = "That path is not a file";
const MSG_STORE: &str = "Could not use the credential store";
const MSG_FLAG: &str = "Could not change the helper setting";
const MSG_COOKIES_TOO_LARGE: &str = "That cookies file is too large";
const MSG_STORED_BAD: &str = "Stored cookies are not usable: import them again";
const MSG_INTERNAL: &str = "Internal error";

/// The two steps, constant for the life of the provider.
pub fn auth_steps() -> Vec<AuthStepInfo> {
    vec![
        AuthStepInfo {
            id: STEP_HELPER,
            label: "Stream helper",
            inputs: vec![],
            hint: Some(StepHint {
                text: "Finds the audio of each track. It needs yt-dlp: Install downloads a checked \
                       copy, or use your own.",
                copy: None,
            }),
            switch: false,
        },
        AuthStepInfo {
            id: STEP_COOKIES,
            label: "Browser cookies",
            inputs: vec![InputSpec {
                name: INPUT_PATH,
                label: "cookies.txt path",
                pattern: "^.{1,1024}$",
                format: "a file path",
                max_len: 1024,
            }],
            hint: Some(StepHint {
                text: "Export your YouTube Music cookies as a cookies.txt file with a browser \
                       extension and enter its path. Delete the exported file afterwards.",
                copy: None,
            }),
            switch: false,
        },
    ]
}

fn fail(text: &str) -> AuthFailure {
    AuthFailure(text.to_owned())
}

fn unpoison<T>(r: Result<T, PoisonError<T>>) -> T {
    r.unwrap_or_else(PoisonError::into_inner)
}

/// The logins of the provider. Cheap to clone; every clone is the same state.
#[derive(Clone)]
pub struct YtAuth(Arc<Core>);

struct Core {
    store: Arc<dyn CredentialStore>,
    tools: Arc<Tools>,
    resolver: Arc<Resolver>,
    /// InnerTube answered 401 or 403 and no import has happened since.
    cookies_expired: AtomicBool,
    /// Counts logins and logouts of the cookies: what was cached for the old ones is stale.
    cookies_generation: AtomicU64,
    /// One reader or writer of the cookies' parts at a time: a reader never sees a mix of two
    /// sets.
    cookies_io: Mutex<()>,
    /// The fixed text of the last failed install or update, until one succeeds.
    tool_error: Mutex<Option<String>>,
    /// The yt-dlp version read last, for the file it was read from.
    version: Mutex<Option<(PathBuf, Option<SystemTime>, String)>>,
}

impl YtAuth {
    pub fn new(
        store: Arc<dyn CredentialStore>,
        tools: Arc<Tools>,
        resolver: Arc<Resolver>,
    ) -> YtAuth {
        YtAuth(Arc::new(Core {
            store,
            tools,
            resolver,
            cookies_expired: AtomicBool::new(false),
            cookies_generation: AtomicU64::new(0),
            cookies_io: Mutex::new(()),
            tool_error: Mutex::new(None),
            version: Mutex::new(None),
        }))
    }

    /// The stream helper is not switched off.
    pub fn helper_enabled(&self) -> bool {
        !self.0.helper_off()
    }

    /// The stored YouTube cookies, if any are stored and usable.
    pub fn cookies(&self) -> Result<Option<Jar>, StoreError> {
        self.0.cookies()
    }

    /// The stored cookies as the Netscape text yt-dlp reads.
    pub fn cookies_secret(&self) -> Result<Option<Secret>, StoreError> {
        self.0.cookies_text()
    }

    /// Changes whenever cookies are imported or removed.
    pub fn cookies_generation(&self) -> u64 {
        self.0.cookies_generation.load(Ordering::SeqCst)
    }

    /// Whether usable cookies are stored and the service has not refused them.
    pub fn cookies_usable(&self) -> bool {
        !self.0.cookies_expired.load(Ordering::SeqCst)
            && self.0.cookies().is_ok_and(|jar| jar.is_some())
    }

    /// InnerTube refused the cookies: the `cookies` step reads as an error until they are
    /// imported again (or a request goes through).
    pub fn mark_cookies_expired(&self) {
        self.0.cookies_expired.store(true, Ordering::SeqCst);
    }

    pub fn clear_cookies_expired(&self) {
        self.0.cookies_expired.store(false, Ordering::SeqCst);
    }
}

fn step_status(
    id: &str,
    state: LoginState,
    action: Option<&str>,
    detail: Option<String>,
) -> AuthStepStatus {
    AuthStepStatus {
        id: id.to_owned(),
        state,
        action: action.map(str::to_owned),
        detail,
    }
}

impl Core {
    fn helper_flag(&self) -> PathBuf {
        self.tools.tools_dir().join(HELPER_OFF_FILE)
    }

    fn helper_off(&self) -> bool {
        self.helper_flag().exists()
    }

    fn cookies(&self) -> Result<Option<Jar>, StoreError> {
        Ok(self.cookies_text()?.and_then(|s| Jar::from_secret(&s).ok()))
    }

    /// The stored cookies put together from their parts: all of one set, or `None` when there
    /// is no count, the count is bad, or a part is missing or unreadable. Only the store
    /// failing on the count is an error.
    fn cookies_text(&self) -> Result<Option<Secret>, StoreError> {
        let _one_at_a_time = unpoison(self.cookies_io.lock());
        let Some(count) = self.store.get(COOKIES_COUNT_ACCOUNT)? else {
            return Ok(None);
        };
        let Some(count) = parse_part_count(count.expose()) else {
            return Ok(None);
        };
        let mut text = String::new();
        for index in 0..count {
            match self.store.get(&cookies_part_account(index)) {
                Ok(Some(part)) => text.push_str(part.expose()),
                _ => return Ok(None),
            }
        }
        Ok(Some(Secret::new(text)))
    }

    /// Deletes the count, every part a set can have, and the account of the first build. Every
    /// delete is tried; `Err` if any failed.
    fn delete_cookies(&self) -> Result<(), StoreError> {
        let _one_at_a_time = unpoison(self.cookies_io.lock());
        let mut result = self.store.delete(COOKIES_COUNT_ACCOUNT).map(|_| ());
        result = result.and(self.delete_parts(0..COOKIES_MAX_PARTS));
        result.and(self.store.delete(COOKIES_ACCOUNT).map(|_| ()))
    }

    /// Tries every delete in `indexes`; `Err` if any failed.
    fn delete_parts(&self, indexes: std::ops::Range<usize>) -> Result<(), StoreError> {
        let mut result = Ok(());
        for index in indexes {
            if self.store.delete(&cookies_part_account(index)).is_err() {
                result = Err(StoreError);
            }
        }
        result
    }

    /// Stores `text` in parts. The count is deleted before anything else changes and written
    /// after every part, so a reader sees the old set gone or the new one whole. On any failure
    /// nothing of either set is left.
    fn store_cookies(&self, text: &str) -> Result<(), AuthFailure> {
        let parts = split_parts(text);
        if parts.len() > COOKIES_MAX_PARTS {
            return Err(fail(MSG_COOKIES_TOO_LARGE));
        }
        let _one_at_a_time = unpoison(self.cookies_io.lock());
        let old_count = match self.store.get(COOKIES_COUNT_ACCOUNT) {
            Ok(Some(count)) => parse_part_count(count.expose()),
            _ => None,
        };
        let written = (|| -> Result<(), StoreError> {
            self.store.delete(COOKIES_COUNT_ACCOUNT)?;
            for (index, part) in parts.iter().enumerate() {
                self.store
                    .set(&cookies_part_account(index), &Secret::new(*part))?;
            }
            // Parts of a larger earlier set; its count is unknown when it was unreadable.
            self.delete_parts(parts.len()..old_count.unwrap_or(COOKIES_MAX_PARTS))?;
            self.store
                .set(COOKIES_COUNT_ACCOUNT, &Secret::new(parts.len().to_string()))
        })();
        if written.is_err() {
            let _ = self.store.delete(COOKIES_COUNT_ACCOUNT);
            let _ = self.delete_parts(0..COOKIES_MAX_PARTS);
            return Err(fail(MSG_STORE));
        }
        Ok(())
    }

    fn set_tool_error(&self, text: Option<String>) {
        *unpoison(self.tool_error.lock()) = text;
    }

    fn status(&self) -> ProviderAuthStatus {
        ProviderAuthStatus {
            steps: vec![self.helper_status(), self.cookies_status()],
        }
    }

    /// `yt-dlp --version` of this file, run once per file version.
    fn version_of(&self, set: &ToolSet) -> Option<String> {
        let modified = fs::metadata(&set.ytdlp).and_then(|m| m.modified()).ok();
        if let Some((path, mtime, v)) = unpoison(self.version.lock()).as_ref() {
            if *path == set.ytdlp && *mtime == modified {
                return Some(v.clone());
            }
        }
        let v = self.resolver.ytdlp_version().ok()?;
        *unpoison(self.version.lock()) = Some((set.ytdlp.clone(), modified, v.clone()));
        Some(v)
    }

    fn helper_status(&self) -> AuthStepStatus {
        if self.helper_off() {
            return step_status(STEP_HELPER, LoginState::LoggedOut, None, None);
        }
        let error = unpoison(self.tool_error.lock()).clone();
        let managed = self.tools.managed_supported();
        let (logged_in, action, detail) = match self.tools.resolve() {
            Some(set) => {
                let version = self.version_of(&set);
                if set.own {
                    let detail = match (version, set.qjs.is_some()) {
                        (Some(v), true) => format!("yt-dlp {v}, QuickJS-ng {QJS_VERSION}"),
                        (Some(v), false) => format!("yt-dlp {v}"),
                        (None, _) => "yt-dlp".to_owned(),
                    };
                    (true, Some("Update"), Some(detail))
                } else {
                    let detail = match version {
                        Some(v) => format!("Your yt-dlp {v}"),
                        None => "Your yt-dlp".to_owned(),
                    };
                    (true, None, Some(detail))
                }
            }
            // The set path wins over PATH and the managed copy, and an install cannot change it.
            None if self.tools.env_path_set() => (false, None, Some(MSG_BAD_YTDLP_ENV.to_owned())),
            None if managed => (false, Some("Install"), None),
            None => (false, None, Some(MSG_INSTALL_YOURSELF.to_owned())),
        };
        let state = match error {
            Some(message) => LoginState::Error { message },
            None if logged_in => LoginState::LoggedIn,
            None => LoginState::LoggedOut,
        };
        step_status(STEP_HELPER, state, action, detail)
    }

    fn cookies_status(&self) -> AuthStepStatus {
        let state = match self.cookies_text() {
            Err(_) => LoginState::Error {
                message: MSG_STORE.to_owned(),
            },
            Ok(None) => LoginState::LoggedOut,
            Ok(Some(secret)) => match Jar::from_secret(&secret) {
                Err(_) => LoginState::Error {
                    message: MSG_STORED_BAD.to_owned(),
                },
                Ok(_) if self.cookies_expired.load(Ordering::SeqCst) => LoginState::Error {
                    message: MSG_COOKIES_EXPIRED.to_owned(),
                },
                Ok(_) => LoginState::LoggedIn,
            },
        };
        step_status(STEP_COOKIES, state, None, None)
    }

    fn login(&self, step: &str, input: &LoginInput) -> Result<ProviderAuthStatus, AuthFailure> {
        match step {
            STEP_HELPER => {
                reject_other_keys(input, &[])?;
                self.enable_helper()?;
                self.install_if_needed()?;
            }
            STEP_COOKIES => {
                reject_other_keys(input, &[INPUT_PATH])?;
                self.import_cookies(input.get(INPUT_PATH).ok_or_else(|| fail(MSG_NO_PATH))?)?;
            }
            _ => return Err(fail(MSG_UNKNOWN_STEP)),
        }
        Ok(self.status())
    }

    fn logout(&self, step: &str) -> Result<ProviderAuthStatus, AuthFailure> {
        match step {
            STEP_HELPER => {
                // Nothing is deleted: only the flag is written.
                fs::create_dir_all(self.tools.tools_dir()).map_err(|_| fail(MSG_FLAG))?;
                fs::write(self.helper_flag(), b"off\n").map_err(|_| fail(MSG_FLAG))?;
                self.set_tool_error(None);
            }
            STEP_COOKIES => {
                self.delete_cookies().map_err(|_| fail(MSG_STORE))?;
                self.cookies_expired.store(false, Ordering::SeqCst);
                self.cookies_generation.fetch_add(1, Ordering::SeqCst);
            }
            _ => return Err(fail(MSG_UNKNOWN_STEP)),
        }
        Ok(self.status())
    }

    fn enable_helper(&self) -> Result<(), AuthFailure> {
        match fs::remove_file(self.helper_flag()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(fail(MSG_FLAG)),
        }
    }

    /// Installs or updates the managed copy unless the user's own yt-dlp is the one in use.
    fn install_if_needed(&self) -> Result<(), AuthFailure> {
        let needed = self.tools.resolve().is_none_or(|set| set.own);
        if !needed {
            self.set_tool_error(None);
            return Ok(());
        }
        match self.tools.install_or_update() {
            Ok(_) => {
                self.set_tool_error(None);
                Ok(())
            }
            Err(e) => {
                let text = match e {
                    ToolError::Unsupported => MSG_INSTALL_YOURSELF.to_owned(),
                    other => other.to_string(),
                };
                self.set_tool_error(Some(text.clone()));
                Err(AuthFailure(text))
            }
        }
    }

    fn import_cookies(&self, raw: &str) -> Result<(), AuthFailure> {
        let bytes = read_cookie_file(raw)?;
        let jar = Jar::parse(&bytes).map_err(|e: CookieError| fail(&e.to_string()))?;
        self.store_cookies(jar.to_secret().expose())?;
        self.cookies_expired.store(false, Ordering::SeqCst);
        self.cookies_generation.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// The part count of a stored value: a decimal number from 1 to [`COOKIES_MAX_PARTS`].
fn parse_part_count(text: &str) -> Option<usize> {
    if text.is_empty() || text.len() > 3 || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse()
        .ok()
        .filter(|n| (1..=COOKIES_MAX_PARTS).contains(n))
}

/// `text` cut at character boundaries into parts of at most [`COOKIES_PART_CHARS`] UTF-16
/// units. Empty text has no parts.
fn split_parts(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut units) = (0, 0);
    for (at, c) in text.char_indices() {
        if units + c.len_utf16() > COOKIES_PART_CHARS {
            parts.push(&text[start..at]);
            (start, units) = (at, 0);
        }
        units += c.len_utf16();
    }
    if start < text.len() {
        parts.push(&text[start..]);
    }
    parts
}

fn reject_other_keys(input: &LoginInput, allowed: &[&str]) -> Result<(), AuthFailure> {
    if input.0.keys().all(|k| allowed.contains(&k.as_str())) {
        Ok(())
    } else {
        Err(fail(MSG_UNKNOWN_INPUT))
    }
}

/// Reads the user's file, and only reads it: opened for reading, at most [`MAX_FILE_BYTES`].
fn read_cookie_file(raw: &str) -> Result<Vec<u8>, AuthFailure> {
    let mut path = raw.trim();
    // "Copy as path" in a file manager puts the path in quotes.
    for quote in ['"', '\''] {
        if let Some(inner) = path.strip_prefix(quote).and_then(|p| p.strip_suffix(quote)) {
            path = inner.trim();
        }
    }
    if path.is_empty() {
        return Err(fail(MSG_NO_PATH));
    }
    if path.chars().count() > 1024 || path.chars().any(char::is_control) {
        return Err(fail(MSG_UNREADABLE));
    }
    let read_error = |e: std::io::Error| {
        fail(if e.kind() == std::io::ErrorKind::NotFound {
            MSG_NOT_FOUND
        } else {
            MSG_UNREADABLE
        })
    };
    let meta = fs::metadata(path).map_err(read_error)?;
    if !meta.is_file() {
        return Err(fail(MSG_NOT_A_FILE));
    }
    if meta.len() > MAX_FILE_BYTES as u64 {
        return Err(fail(&CookieError::TooLarge.to_string()));
    }
    let file = fs::File::open(path).map_err(read_error)?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| fail(MSG_UNREADABLE))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(fail(&CookieError::TooLarge.to_string()));
    }
    Ok(bytes)
}

/// Runs `work` on a blocking thread: it reads files, runs `yt-dlp --version` and downloads.
fn blocking<T: Send + 'static>(
    core: Arc<Core>,
    work: impl FnOnce(&Core) -> Result<T, AuthFailure> + Send + 'static,
) -> BoxFuture<'static, Result<T, AuthFailure>> {
    Box::pin(async move {
        tokio::task::spawn_blocking(move || work(&core))
            .await
            .unwrap_or_else(|_| Err(fail(MSG_INTERNAL)))
    })
}

impl AuthFlow for YtAuth {
    fn status(&self) -> BoxFuture<'_, Result<ProviderAuthStatus, AuthFailure>> {
        blocking(self.0.clone(), |core| Ok(core.status()))
    }

    fn login<'a>(
        &'a self,
        step: &'a str,
        input: &'a LoginInput,
    ) -> BoxFuture<'a, Result<ProviderAuthStatus, AuthFailure>> {
        let (step, input) = (step.to_owned(), input.clone());
        blocking(self.0.clone(), move |core| core.login(&step, &input))
    }

    fn logout<'a>(
        &'a self,
        step: &'a str,
    ) -> BoxFuture<'a, Result<ProviderAuthStatus, AuthFailure>> {
        let step = step.to_owned();
        blocking(self.0.clone(), move |core| core.logout(&step))
    }
}

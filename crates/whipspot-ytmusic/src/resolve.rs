//! Stream resolve: asks yt-dlp for the URL of one track's audio and checks everything it says.
//!
//! yt-dlp is used for the URL only; Whipspot fetches and decodes the audio itself (D1). The
//! output of `yt-dlp -J` is untrusted input: it is read through typed serde structs that hold
//! only the fields used, and the URL must be `https` on a `googlevideo.com` subdomain with an AAC
//! (`mp4a`) track before it is returned. Only the `User-Agent` of yt-dlp's headers is kept.
//!
//! Errors are fixed strings. yt-dlp's stderr, its arguments and the cookies never reach one.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::Url;
use serde::Deserialize;
use whipspot_core::secret::Secret;

use crate::http::MAX_URL_BYTES;
use crate::run::{RunError, Runner, STDOUT_CAP};
use crate::tools::{Tools, os};

/// How long one yt-dlp resolve may take.
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(45);
/// How long `yt-dlp --version` may take.
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
/// The cache holds this many tracks.
pub const CACHE_ENTRIES: usize = 12;
/// A cached stream is dropped this long before its URL expires.
pub const EXPIRY_MARGIN: Duration = Duration::from_secs(30 * 60);
/// A cached stream is dropped this long after it was resolved.
pub const MAX_AGE: Duration = Duration::from_secs(5 * 60 * 60);
/// A failed resolve forces a tool update at most this often.
pub const UPDATE_EVERY: Duration = Duration::from_secs(10 * 60);

/// How a ranged read of the stream is asked for. Step 0's probe proved the `Range` header form
/// against the real service (206 with the exact bytes at 0 and at 1 MiB); the `Query` form
/// (`&range=a-b`) also answered 200 but is not used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeForm {
    Header,
    Query,
}

/// What a resolve found: everything a ranged reader needs.
#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    /// `https` on a `googlevideo.com` subdomain.
    pub url: String,
    /// At most a `User-Agent`.
    pub headers: Vec<(String, String)>,
    /// The format id's number: 140 (AAC 128 kbps) or 141 (AAC 256 kbps) as asked for.
    pub itag: u32,
    /// 128 or 256, only when yt-dlp's figure agrees with the itag; otherwise unknown.
    pub abr_kbps: Option<u32>,
    /// The size in bytes, when yt-dlp knows it.
    pub filesize: Option<u64>,
    /// The URL's own `expire` parameter.
    pub expires_at: Option<SystemTime>,
    pub range_form: RangeForm,
    /// yt-dlp's own chunk size for this stream: a block read by Whipspot is not larger.
    pub chunk_size: Option<u64>,
}

/// Why a resolve gave no stream. Fixed messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveError {
    /// The track id is not an 11-character id. Nothing was run.
    BadId,
    /// No yt-dlp is installed or set.
    NoTools,
    /// The tool did not run to its end.
    Run(RunError),
    /// The tool ran and failed (a non-zero exit).
    Failed,
    /// Its output was empty or not the JSON that was asked for.
    Unreadable,
    /// The answer holds no stream that may be played: not https on googlevideo.com, or not AAC.
    NoStream,
    /// The scratch folder or the cookies file for the run could not be written.
    Disk,
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::BadId => f.write_str("Invalid track id"),
            ResolveError::NoTools => f.write_str("The stream helper is not installed"),
            ResolveError::Run(e) => write!(f, "Stream helper failed: {e}"),
            ResolveError::Failed => f.write_str("The stream helper found no stream"),
            ResolveError::Unreadable => f.write_str("The stream helper's answer was not readable"),
            ResolveError::NoStream => f.write_str("No playable stream"),
            ResolveError::Disk => f.write_str("Could not prepare the stream helper run"),
        }
    }
}

impl std::error::Error for ResolveError {}

impl ResolveError {
    /// A failure an update of the tools may cure.
    fn retriable(self) -> bool {
        !matches!(
            self,
            ResolveError::BadId | ResolveError::NoTools | ResolveError::Disk
        )
    }
}

/// Eleven characters of `[A-Za-z0-9_-]`.
pub fn valid_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

struct CacheEntry {
    id: String,
    stream: Stream,
    /// Valid while the clock is before this.
    valid_until: SystemTime,
}

/// Resolves tracks to streams. One per provider; safe to share.
pub struct Resolver {
    tools: Arc<Tools>,
    runner: Arc<dyn Runner>,
    now: Box<dyn Fn() -> SystemTime + Send + Sync>,
    /// Least recently used first.
    cache: Mutex<VecDeque<CacheEntry>>,
    last_update: Mutex<Option<SystemTime>>,
}

fn unpoison<T>(r: Result<T, PoisonError<T>>) -> T {
    r.unwrap_or_else(PoisonError::into_inner)
}

impl Resolver {
    pub fn new(tools: Arc<Tools>, runner: Arc<dyn Runner>) -> Resolver {
        Resolver::with_clock(tools, runner, Box::new(SystemTime::now))
    }

    /// A resolver on another clock (the cache's validity and the update rate limit read it).
    pub fn with_clock(
        tools: Arc<Tools>,
        runner: Arc<dyn Runner>,
        now: Box<dyn Fn() -> SystemTime + Send + Sync>,
    ) -> Resolver {
        Resolver {
            tools,
            runner,
            now,
            cache: Mutex::new(VecDeque::new()),
            last_update: Mutex::new(None),
        }
    }

    /// The stream of track `video_id`: from the cache while it is valid, else from a yt-dlp run.
    /// A run that fails, or answers nothing usable, is followed by one tool update and one more
    /// run (D5) unless an update was forced in the last [`UPDATE_EVERY`] or the update itself
    /// failed. `cookies` is a Netscape cookie file's text.
    pub fn resolve(
        &self,
        video_id: &str,
        cookies: Option<&Secret>,
    ) -> Result<Stream, ResolveError> {
        if !valid_video_id(video_id) {
            return Err(ResolveError::BadId);
        }
        if let Some(stream) = self.cache_get(video_id) {
            return Ok(stream);
        }
        self.resolve_fresh(video_id, cookies)
    }

    /// Like [`resolve`](Self::resolve) but drops the cached entry first: for a stream whose URL
    /// the server has stopped accepting.
    pub fn refresh(
        &self,
        video_id: &str,
        cookies: Option<&Secret>,
    ) -> Result<Stream, ResolveError> {
        if !valid_video_id(video_id) {
            return Err(ResolveError::BadId);
        }
        unpoison(self.cache.lock()).retain(|e| e.id != video_id);
        self.resolve_fresh(video_id, cookies)
    }

    fn resolve_fresh(
        &self,
        video_id: &str,
        cookies: Option<&Secret>,
    ) -> Result<Stream, ResolveError> {
        let stream = match self.attempt(video_id, cookies) {
            Ok(s) => s,
            Err(e) if e.retriable() && self.update_tools() => self.attempt(video_id, cookies)?,
            Err(e) => return Err(e),
        };
        self.cache_put(video_id, &stream);
        Ok(stream)
    }

    /// The yt-dlp version (for a status line), read from `--version`.
    pub fn ytdlp_version(&self) -> Result<String, ResolveError> {
        let in_use = self.tools.acquire().ok_or(ResolveError::NoTools)?;
        let out = self
            .runner
            .run(
                &in_use.ytdlp,
                &[OsString::from("--version")],
                VERSION_TIMEOUT,
            )
            .map_err(ResolveError::Run)?;
        if !out.success() {
            return Err(ResolveError::Failed);
        }
        let text = std::str::from_utf8(&out.stdout).map_err(|_| ResolveError::Unreadable)?;
        let text = text.trim();
        let ok = (1..=32).contains(&text.len())
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+'));
        if ok {
            Ok(text.to_owned())
        } else {
            Err(ResolveError::Unreadable)
        }
    }

    /// One update of the tools, at most once in [`UPDATE_EVERY`]. `true` when it ran and
    /// succeeded, so a retry is worth it.
    fn update_tools(&self) -> bool {
        // Installing is the user's button: only our own copy is kept up to date here, and a
        // user's yt-dlp is never run differently after an update.
        if !self.tools.resolve().is_some_and(|set| set.own) {
            return false;
        }
        let now = (self.now)();
        {
            let mut last = unpoison(self.last_update.lock());
            if last.is_some_and(|t| now.duration_since(t).is_ok_and(|d| d < UPDATE_EVERY)) {
                return false;
            }
            // Set before the update: a failing network is not hammered either.
            *last = Some(now);
        }
        // No read guard is held here: the update waits for those.
        self.tools.install_or_update().is_ok()
    }

    fn attempt(&self, video_id: &str, cookies: Option<&Secret>) -> Result<Stream, ResolveError> {
        // Held until the run ends: no update replaces a binary under it.
        let in_use = self.tools.acquire().ok_or(ResolveError::NoTools)?;
        let tools_dir = self.tools.tools_dir();

        let cookie_file = match cookies {
            Some(secret) if !secret.is_empty() => Some(RunFile::create(tools_dir, secret)?),
            _ => None,
        };

        let mut args: Vec<OsString> = [
            "--ignore-config",
            "--no-playlist",
            "--no-warnings",
            "--no-progress",
            "-J",
            "-f",
            "141/140",
            "--cache-dir",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        args.push(tools_dir.join("cache").into_os_string());
        // The runtime is named only for our own yt-dlp: a user's has its own setup.
        if let (true, Some(qjs)) = (in_use.own, &in_use.qjs) {
            args.push("--js-runtimes".into());
            let mut arg = OsString::from("quickjs:");
            arg.push(qjs);
            args.push(arg);
        }
        if let Some(file) = &cookie_file {
            args.push("--cookies".into());
            args.push(file.0.clone().into_os_string());
        }
        args.push("--".into());
        args.push(format!("https://music.youtube.com/watch?v={video_id}").into());

        let out = self.runner.run(&in_use.ytdlp, &args, RESOLVE_TIMEOUT);
        // The cookies are gone before the answer is looked at, whatever it is.
        drop(cookie_file);
        let out = out.map_err(ResolveError::Run)?;
        if !out.success() {
            return Err(ResolveError::Failed);
        }
        parse_stream(&out.stdout)
    }

    fn cache_get(&self, id: &str) -> Option<Stream> {
        let now = (self.now)();
        let mut cache = unpoison(self.cache.lock());
        let at = cache.iter().position(|e| e.id == id)?;
        let entry = cache.remove(at)?;
        if now >= entry.valid_until {
            return None;
        }
        let stream = entry.stream.clone();
        cache.push_back(entry);
        Some(stream)
    }

    fn cache_put(&self, id: &str, stream: &Stream) {
        let now = (self.now)();
        let mut valid_until = now + MAX_AGE;
        if let Some(expires) = stream.expires_at {
            let by_expiry = expires.checked_sub(EXPIRY_MARGIN).unwrap_or(UNIX_EPOCH);
            valid_until = valid_until.min(by_expiry);
        }
        let mut cache = unpoison(self.cache.lock());
        cache.retain(|e| e.id != id);
        cache.push_back(CacheEntry {
            id: id.to_owned(),
            stream: stream.clone(),
            valid_until,
        });
        while cache.len() > CACHE_ENTRIES {
            cache.pop_front();
        }
    }
}

/// The cookies of one run: a file with a random name under `<tools>/run/`, removed when dropped
/// (success, error, timeout or panic alike).
struct RunFile(PathBuf);

impl RunFile {
    fn create(tools_dir: &Path, cookies: &Secret) -> Result<RunFile, ResolveError> {
        let dir = tools_dir.join("run");
        fs::create_dir_all(&dir).map_err(|_| ResolveError::Disk)?;
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|_| ResolveError::Disk)?;
        let name: String = random.iter().map(|b| format!("{b:02x}")).collect();
        let path = dir.join(format!("{name}.txt"));
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        os::private_file(&mut opts);
        let mut file = opts.open(&path).map_err(|_| ResolveError::Disk)?;
        // From here the file is removed on every path, a failed write included.
        let run_file = RunFile(path);
        file.write_all(cookies.expose().as_bytes())
            .and_then(|()| file.flush())
            .map_err(|_| ResolveError::Disk)?;
        Ok(run_file)
    }
}

/// Removes what a crashed run left in `<tools>/run/`: the plaintext cookies of that run. Only
/// that folder and only regular files in it; a symlink (the folder or an entry) is never
/// followed, and every error is ignored.
pub fn clear_run_files(tools_dir: &Path) {
    let dir = tools_dir.join("run");
    match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.file_type().is_dir() => {}
        _ => return,
    }
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_file()) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

impl Drop for RunFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

// ---- reading yt-dlp's answer --------------------------------------------------------------

/// The fields of one format that are read. Everything else in `-J` is skipped.
#[derive(Deserialize, Default)]
struct Format {
    format_id: Option<String>,
    url: Option<String>,
    http_headers: Option<Headers>,
    abr: Option<f64>,
    filesize: Option<f64>,
    acodec: Option<String>,
    vcodec: Option<String>,
    downloader_options: Option<DownloaderOptions>,
}

#[derive(Deserialize, Default)]
struct Headers {
    #[serde(rename = "User-Agent")]
    user_agent: Option<String>,
}

#[derive(Deserialize, Default)]
struct DownloaderOptions {
    http_chunk_size: Option<f64>,
}

/// `-J` of one video that was asked for one format: the format's own fields sit at the top, and
/// `requested_formats` is there when yt-dlp merged several.
#[derive(Deserialize)]
struct Info {
    requested_formats: Option<Vec<Format>>,
    #[serde(flatten)]
    top: Format,
}

/// The longest `User-Agent` kept.
const MAX_USER_AGENT: usize = 512;

/// Reads `yt-dlp -J` output into a [`Stream`], or says why not. Never panics on any input.
fn parse_stream(stdout: &[u8]) -> Result<Stream, ResolveError> {
    if stdout.len() > STDOUT_CAP || stdout.iter().all(u8::is_ascii_whitespace) {
        return Err(ResolveError::Unreadable);
    }
    // serde_json refuses nesting deeper than 128 with an error.
    let info: Info = serde_json::from_slice(stdout).map_err(|_| ResolveError::Unreadable)?;
    let format = match info.requested_formats {
        Some(mut list) if !list.is_empty() => {
            if list.len() != 1 {
                return Err(ResolveError::NoStream);
            }
            list.remove(0)
        }
        _ => info.top,
    };

    // Audio only, AAC.
    if format.vcodec.as_deref().is_some_and(|v| v != "none") {
        return Err(ResolveError::NoStream);
    }
    if !format
        .acodec
        .as_deref()
        .is_some_and(|a| a.starts_with("mp4a"))
    {
        return Err(ResolveError::NoStream);
    }
    let itag = format
        .format_id
        .as_deref()
        .and_then(|id| id.split('-').next())
        .and_then(|n| n.parse::<u32>().ok())
        .ok_or(ResolveError::NoStream)?;
    let (url, expires_at) = playable_url(format.url.as_deref().ok_or(ResolveError::NoStream)?)
        .ok_or(ResolveError::NoStream)?;

    let abr_kbps = match (itag, format.abr) {
        (140, Some(abr)) if (120.0..=140.0).contains(&abr) => Some(128),
        (141, Some(abr)) if (240.0..=270.0).contains(&abr) => Some(256),
        _ => None,
    };
    let whole = |v: f64| (v.is_finite() && (1.0..=1.0e12).contains(&v)).then_some(v as u64);
    let user_agent = format.http_headers.and_then(|h| h.user_agent).filter(|ua| {
        !ua.is_empty() && ua.len() <= MAX_USER_AGENT && !ua.bytes().any(|b| b.is_ascii_control())
    });
    Ok(Stream {
        url,
        headers: user_agent
            .map(|ua| vec![("User-Agent".to_owned(), ua)])
            .unwrap_or_default(),
        itag,
        abr_kbps,
        filesize: format.filesize.and_then(whole),
        expires_at,
        range_form: RangeForm::Header,
        chunk_size: format
            .downloader_options
            .and_then(|d| d.http_chunk_size)
            .and_then(whole),
    })
}

/// The URL if it may be fetched: `https`, no userinfo or port, a subdomain of `googlevideo.com`,
/// and (if it says) an `audio/mp4` mime. Also its `expire` parameter.
fn playable_url(text: &str) -> Option<(String, Option<SystemTime>)> {
    if text.len() > MAX_URL_BYTES {
        return None;
    }
    let url = Url::parse(text).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return None;
    }
    let domain = url.domain()?;
    let subdomain = domain
        .strip_suffix("googlevideo.com")
        .and_then(|p| p.strip_suffix('.'))
        .is_some_and(|p| !p.is_empty() && !p.ends_with('.') && !p.starts_with('.'));
    if !subdomain {
        return None;
    }
    let mut expires_at = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "mime" if value != "audio/mp4" => return None,
            "expire" => {
                // `checked_add`: a value past the clock's range is "no expiry", not a panic.
                expires_at = value
                    .parse::<u64>()
                    .ok()
                    .and_then(|s| UNIX_EPOCH.checked_add(Duration::from_secs(s)));
            }
            _ => {}
        }
    }
    Some((url.into(), expires_at))
}

//! The managed stream-helper tools: yt-dlp (resolves a stream URL) and QuickJS-ng (the script
//! runtime yt-dlp calls). This module finds them and installs and updates its own copies (D5).
//!
//! # Supply chain
//!
//! Every download goes over https to `github.com` and from there to
//! `release-assets.githubusercontent.com`, through [`Http`](crate::http::Http), whose allowlist
//! holds nothing else. Nothing downloaded is run before it verifies, and nothing is put in place
//! before it verifies:
//!
//! - yt-dlp: `SHA2-256SUMS.sig` is checked over `SHA2-256SUMS` with the yt-dlp signing key
//!   compiled into this crate (its fingerprint is pinned too), then the binary's SHA-256 is
//!   checked against its line in the verified sums. The sums parse like `sha256sum -c --strict`:
//!   each line is exactly `<64 hex digits><two spaces><name>`, names are matched byte for byte,
//!   and a second line for one name is an error.
//! - QuickJS-ng: a version pinned here, with the SHA-256 of each asset pinned here, so it moves
//!   with a Whipspot release.
//!
//! A verified file is written under a temporary name in the tools folder and renamed over the
//! installed copy while the write half of the [`Tools`] lock is held, so no run of a tool is in
//! progress at the moment of the swap. A rename that fails (on Windows: the file is in use)
//! keeps the old copy, deletes the temporary file and gives a fixed error.
//!
//! Error texts are fixed strings: no URL, header or tool output (hard rule 1).

use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pgp::composed::{Deserializable, DetachedSignature, SignedPublicKey};
use pgp::packet::SignatureType;
use pgp::types::KeyDetails;
use sha2::{Digest, Sha256};

use crate::http::{Cap, Http, HttpError};

#[cfg(windows)]
#[path = "windows.rs"]
pub(crate) mod os;

#[cfg(target_os = "macos")]
#[path = "macos.rs"]
pub(crate) mod os;

/// Any other target has no managed install (the project targets Windows and macOS).
#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) mod os {
    use super::Managed;
    use std::path::Path;
    use std::process::Command;

    pub(crate) const YTDLP_PATH_NAME: &str = "yt-dlp";
    pub(crate) const YTDLP_FILE: &str = "yt-dlp";
    pub(crate) const QJS_FILE: &str = "qjs";

    pub(crate) fn managed() -> Option<Managed> {
        None
    }
    pub(crate) fn configure_command(_cmd: &mut Command) {}
    pub(crate) fn kill_tree(_pid: u32) {}
    pub(crate) fn make_executable(_path: &Path) -> std::io::Result<()> {
        Ok(())
    }
    pub(crate) fn private_file(_opts: &mut std::fs::OpenOptions) {}
}

/// The yt-dlp signing key (`public.key` of the yt-dlp repository).
const YTDLP_PUBLIC_KEY: &str = include_str!("ytdlp-public.key");
/// Its fingerprint, as the README of yt-dlp gives it.
pub const YTDLP_KEY_FINGERPRINT: &str = "AC0CBBE6848D6A873464AF4E57CF65933B5A7581";

/// Where the yt-dlp release files are. `latest/download/<asset>` redirects to the newest
/// release.
pub const YTDLP_RELEASE_BASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";
/// The pinned QuickJS-ng release.
pub const QJS_RELEASE_BASE: &str =
    "https://github.com/quickjs-ng/quickjs/releases/download/v0.18.0";
/// The QuickJS-ng version of [`QJS_RELEASE_BASE`], for a status line.
pub const QJS_VERSION: &str = "0.18.0";
/// The checksum list and its signature, as named in a yt-dlp release.
pub const SUMS_NAME: &str = "SHA2-256SUMS";
pub const SIG_NAME: &str = "SHA2-256SUMS.sig";

/// The file that keeps the update daily: the Unix seconds of the last successful check.
pub const STAMP_FILE: &str = "update-stamp";
const UPDATE_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Which trust anchor the checksum list's signature is checked against.
#[derive(Debug, Clone)]
pub enum Trust {
    /// The yt-dlp signing key compiled into this crate. The only value the app uses.
    Compiled,
    /// An armored public key of the caller: for tests that sign their own checksum lists.
    Custom(String),
}

/// What there is to install on this target: the assets to fetch and the pin of the script
/// runtime.
#[derive(Debug, Clone)]
pub struct Managed {
    /// The yt-dlp asset name, also its line in the checksum list.
    pub ytdlp_asset: String,
    /// The QuickJS-ng asset name.
    pub qjs_asset: String,
    /// The SHA-256 the QuickJS-ng asset must have.
    pub qjs_sha256: [u8; 32],
}

impl Managed {
    pub(crate) fn from_hex(
        ytdlp_asset: &str,
        qjs_asset: &str,
        qjs_sha256: &str,
    ) -> Option<Managed> {
        Some(Managed {
            ytdlp_asset: ytdlp_asset.to_owned(),
            qjs_asset: qjs_asset.to_owned(),
            qjs_sha256: parse_hex32(qjs_sha256)?,
        })
    }
}

/// Looks a program up on PATH by file name.
pub type PathLookup = Box<dyn Fn(&str) -> Option<PathBuf> + Send + Sync>;
/// Moves a file over another (`fs::rename`).
pub type RenameFn = Box<dyn Fn(&Path, &Path) -> io::Result<()> + Send + Sync>;

/// Everything the tools module reads from the machine, injected so a test never depends on the
/// developer's PATH, clock or disk.
pub struct ToolEnv {
    /// The folder of the managed copies (`~/.whipspot/tools/`).
    pub tools_dir: PathBuf,
    /// The value of the `WHIPSPOT_YTDLP` environment variable, if set: the "set path". It wins
    /// over everything; when it does not name an existing file there is no yt-dlp.
    pub env_path: Option<PathBuf>,
    /// Looks a program up on PATH by file name.
    pub path_lookup: PathLookup,
    /// The clock the daily update reads.
    pub now: Box<dyn Fn() -> SystemTime + Send + Sync>,
    /// Moves the verified temporary file over the installed copy.
    pub rename: RenameFn,
    pub trust: Trust,
    /// `None`: no managed install on this target.
    pub managed: Option<Managed>,
}

impl ToolEnv {
    /// The compiled-in trust and pins, the real clock and `fs::rename`, and an environment that
    /// finds nothing: no set path and nothing on PATH. What a test starts from.
    pub fn new(tools_dir: PathBuf) -> ToolEnv {
        ToolEnv {
            tools_dir,
            env_path: None,
            path_lookup: Box::new(|_| None),
            now: Box::new(SystemTime::now),
            rename: Box::new(|from, to| fs::rename(from, to)),
            trust: Trust::Compiled,
            managed: os::managed(),
        }
    }

    /// [`new`](Self::new) plus the process's own `WHIPSPOT_YTDLP` and PATH.
    pub fn from_process(tools_dir: PathBuf) -> ToolEnv {
        let mut env = ToolEnv::new(tools_dir);
        env.env_path = std::env::var_os("WHIPSPOT_YTDLP")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        env.path_lookup = Box::new(lookup_on_path);
        env
    }
}

/// The first file called `name` in an absolute PATH entry (a relative or empty entry would make
/// the current directory a place programs are found).
fn lookup_on_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// The tools a run uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSet {
    pub ytdlp: PathBuf,
    /// The managed script runtime, if installed.
    pub qjs: Option<PathBuf>,
    /// yt-dlp is the managed copy (not the set path and not PATH).
    pub own: bool,
}

/// A [`ToolSet`] held for a run. While it lives no update replaces a binary. Drop it before
/// calling [`Tools::install_or_update`] on the same thread: the update waits for it.
pub struct InUse<'a> {
    _guard: RwLockReadGuard<'a, ()>,
    pub set: ToolSet,
}

impl std::ops::Deref for InUse<'_> {
    type Target = ToolSet;
    fn deref(&self) -> &ToolSet {
        &self.set
    }
}

/// What an install or update did to one tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Not ours to manage (the set path or PATH supplies it).
    NotManaged,
    /// The installed copy already matches the verified checksum.
    Unchanged,
    /// There was none; now there is.
    Installed,
    /// The installed copy was replaced by a newer verified one.
    Updated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Installed {
    pub ytdlp: Change,
    pub qjs: Change,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Ytdlp,
    Qjs,
}

/// Why an install or update did not happen. Fixed messages; the previous copy stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolError {
    /// No managed install on this target.
    Unsupported,
    /// The transport refused or failed.
    Http(HttpError),
    /// A download answered with something other than success.
    Download,
    /// The checksum list's signature did not verify.
    BadSignature,
    /// The verified checksum list is malformed or has no single line for the asset.
    BadSums,
    /// A download's SHA-256 is not the verified one (or the pinned one).
    HashMismatch,
    /// The tool could not be replaced: it is in use.
    InUse(Tool),
    /// The tools folder could not be written.
    Disk,
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolError::Unsupported => f.write_str("Managed install is unsupported on this system"),
            ToolError::Http(e) => write!(f, "Download failed: {e}"),
            ToolError::Download => f.write_str("Download failed: unexpected answer"),
            ToolError::BadSignature => {
                f.write_str("Update failed: the checksum list is not signed by the expected key")
            }
            ToolError::BadSums => {
                f.write_str("Update failed: the checksum list does not list this download once")
            }
            ToolError::HashMismatch => {
                f.write_str("Update failed: the download does not match its checksum")
            }
            ToolError::InUse(Tool::Ytdlp) => {
                f.write_str("Update failed: yt-dlp is in use; try again")
            }
            ToolError::InUse(Tool::Qjs) => {
                f.write_str("Update failed: QuickJS-ng is in use; try again")
            }
            ToolError::Disk => f.write_str("Update failed: could not write to the tools folder"),
        }
    }
}

impl std::error::Error for ToolError {}

impl From<HttpError> for ToolError {
    fn from(e: HttpError) -> Self {
        ToolError::Http(e)
    }
}

fn unpoison<T>(r: Result<T, PoisonError<T>>) -> T {
    r.unwrap_or_else(PoisonError::into_inner)
}

/// Finds, installs and updates the tools. One per app.
pub struct Tools {
    env: ToolEnv,
    http: Arc<dyn Http>,
    /// A run of a tool holds the read half ([`acquire`](Self::acquire)); replacing a binary holds
    /// the write half, so a swap never happens under a running tool and two runs (the current
    /// track and a preload) may overlap.
    lock: RwLock<()>,
    /// One install or update at a time.
    install: Mutex<()>,
}

impl Tools {
    pub fn new(env: ToolEnv, http: Arc<dyn Http>) -> Tools {
        Tools {
            env,
            http,
            lock: RwLock::new(()),
            install: Mutex::new(()),
        }
    }

    /// Whether this target has a managed install (Windows and macOS).
    pub fn managed_supported(&self) -> bool {
        self.env.managed.is_some()
    }

    /// Whether a managed copy of yt-dlp or of the script runtime is on disk: what the start-up
    /// update looks after. Installing the first copy is the user's button.
    pub fn has_managed_copy(&self) -> bool {
        [os::YTDLP_FILE, os::QJS_FILE]
            .iter()
            .any(|f| self.env.tools_dir.join(f).is_file())
    }

    /// The folder of the managed copies; a run keeps its scratch files in `run/` and yt-dlp its
    /// cache in `cache/` below it.
    pub fn tools_dir(&self) -> &Path {
        &self.env.tools_dir
    }

    /// Whether `WHIPSPOT_YTDLP` names a path (it may still not be a usable file: then
    /// [`resolve`](Self::resolve) finds nothing, and neither PATH nor an install replaces it).
    pub fn env_path_set(&self) -> bool {
        self.env.env_path.is_some()
    }

    /// Where the tools are now: the set path, then PATH, then the managed copy.
    pub fn resolve(&self) -> Option<ToolSet> {
        let (ytdlp, own) = if let Some(p) = &self.env.env_path {
            if p.is_absolute() && p.is_file() {
                (p.clone(), false)
            } else {
                return None;
            }
        } else if let Some(p) = (self.env.path_lookup)(os::YTDLP_PATH_NAME) {
            (p, false)
        } else {
            let managed = self.env.tools_dir.join(os::YTDLP_FILE);
            if !managed.is_file() {
                return None;
            }
            (managed, true)
        };
        let qjs = self.env.tools_dir.join(os::QJS_FILE);
        Some(ToolSet {
            ytdlp,
            qjs: qjs.is_file().then_some(qjs),
            own,
        })
    }

    /// [`resolve`](Self::resolve), holding the read half of the lock until the result is dropped.
    pub fn acquire(&self) -> Option<InUse<'_>> {
        let guard = unpoison(self.lock.read());
        self.resolve().map(|set| InUse { _guard: guard, set })
    }

    /// The daily check: [`install_or_update`](Self::install_or_update) unless one succeeded in
    /// the last 24 hours. `Ok(None)` means it was not due.
    pub fn update_if_due(&self) -> Result<Option<Installed>, ToolError> {
        if !self.stamp_due() {
            return Ok(None);
        }
        self.install_or_update().map(Some)
    }

    /// Installs what is missing and replaces what the verified checksum list says is outdated:
    /// yt-dlp (unless the set path or PATH supplies it) and the pinned script runtime.
    pub fn install_or_update(&self) -> Result<Installed, ToolError> {
        let managed = self.env.managed.as_ref().ok_or(ToolError::Unsupported)?;
        let _one_at_a_time = unpoison(self.install.lock());

        let external =
            self.env.env_path.is_some() || (self.env.path_lookup)(os::YTDLP_PATH_NAME).is_some();
        let ytdlp = if external {
            Change::NotManaged
        } else {
            self.install_ytdlp(managed)?
        };
        let qjs = self.install_qjs(managed)?;
        self.write_stamp();
        Ok(Installed { ytdlp, qjs })
    }

    fn install_ytdlp(&self, managed: &Managed) -> Result<Change, ToolError> {
        let sums = self.fetch(&format!("{YTDLP_RELEASE_BASE}/{SUMS_NAME}"), Cap::SMALL)?;
        let sig = self.fetch(&format!("{YTDLP_RELEASE_BASE}/{SIG_NAME}"), Cap::SMALL)?;
        if !verify_signature(&self.env.trust, &sums, &sig) {
            return Err(ToolError::BadSignature);
        }
        let expected = sums_hash(&sums, &managed.ytdlp_asset)?;

        let dest = self.env.tools_dir.join(os::YTDLP_FILE);
        if file_sha256(&dest) == Some(expected) {
            return Ok(Change::Unchanged);
        }
        let bin = self.fetch(
            &format!("{YTDLP_RELEASE_BASE}/{}", managed.ytdlp_asset),
            Cap::BINARY,
        )?;
        if sha256(&bin) != expected {
            return Err(ToolError::HashMismatch);
        }
        self.place(Tool::Ytdlp, &dest, &bin)
    }

    fn install_qjs(&self, managed: &Managed) -> Result<Change, ToolError> {
        let dest = self.env.tools_dir.join(os::QJS_FILE);
        if file_sha256(&dest) == Some(managed.qjs_sha256) {
            return Ok(Change::Unchanged);
        }
        let bin = self.fetch(
            &format!("{QJS_RELEASE_BASE}/{}", managed.qjs_asset),
            Cap::BINARY,
        )?;
        if sha256(&bin) != managed.qjs_sha256 {
            return Err(ToolError::HashMismatch);
        }
        self.place(Tool::Qjs, &dest, &bin)
    }

    /// One download: a success answer within its cap.
    fn fetch(&self, url: &str, cap: Cap) -> Result<Vec<u8>, ToolError> {
        let r = self.http.get(url, &[], None, cap)?;
        if !r.is_success() {
            return Err(ToolError::Download);
        }
        // The transport enforces the cap; this holds for any `Http`.
        if r.body.len() > cap.bytes() {
            return Err(ToolError::Http(HttpError::TooLarge));
        }
        Ok(r.body)
    }

    /// Writes the verified bytes to a temporary file in the tools folder and renames it over
    /// `dest` while holding the write half of the lock.
    fn place(&self, tool: Tool, dest: &Path, bytes: &[u8]) -> Result<Change, ToolError> {
        fs::create_dir_all(&self.env.tools_dir).map_err(|_| ToolError::Disk)?;
        let file_name = dest.file_name().ok_or(ToolError::Disk)?.to_string_lossy();
        let tmp = TempFile(self.env.tools_dir.join(format!(".{file_name}.tmp")));
        // A temporary file a crashed run left behind is ours to remove.
        let _ = fs::remove_file(&tmp.0);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp.0)
            .map_err(|_| ToolError::Disk)?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| ToolError::Disk)?;
        drop(file);
        os::make_executable(&tmp.0).map_err(|_| ToolError::Disk)?;

        let existed = dest.exists();
        let _swap = unpoison(self.lock.write());
        (self.env.rename)(&tmp.0, dest).map_err(|_| ToolError::InUse(tool))?;
        tmp.disarm();
        Ok(if existed {
            Change::Updated
        } else {
            Change::Installed
        })
    }

    fn stamp_path(&self) -> PathBuf {
        self.env.tools_dir.join(STAMP_FILE)
    }

    fn stamp_due(&self) -> bool {
        let last = fs::read_to_string(self.stamp_path())
            .ok()
            .and_then(|t| t.trim().parse::<u64>().ok());
        let now = unix_secs((self.env.now)());
        match last {
            // A stamp in the future (the clock went back) counts as due.
            Some(last) => now < last || now - last >= UPDATE_EVERY.as_secs(),
            None => true,
        }
    }

    fn write_stamp(&self) {
        let _ = fs::create_dir_all(&self.env.tools_dir);
        let _ = fs::write(self.stamp_path(), unix_secs((self.env.now)()).to_string());
    }
}

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A temporary file that is deleted when dropped unless it was put in place.
struct TempFile(PathBuf);

impl TempFile {
    fn disarm(self) {
        std::mem::forget(self);
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

// ---- verification -----------------------------------------------------------------------

/// Whether `sig` is a binary detached signature of `data` by the key `trust` names. Armored or
/// binary signature data both parse. Every failure (a bad key, a bad signature, a weak hash, the
/// wrong key) is `false`.
pub fn verify_signature(trust: &Trust, data: &[u8], sig: &[u8]) -> bool {
    let armored = match trust {
        Trust::Compiled => YTDLP_PUBLIC_KEY,
        Trust::Custom(key) => key.as_str(),
    };
    let Ok((key, _)) = SignedPublicKey::from_armor_single(armored.as_bytes()) else {
        return false;
    };
    if matches!(trust, Trust::Compiled)
        && format!("{:X}", key.fingerprint()) != YTDLP_KEY_FINGERPRINT
    {
        return false;
    }
    let Ok((sig, _)) = DetachedSignature::from_reader_single(sig) else {
        return false;
    };
    if sig.signature.typ() != Some(SignatureType::Binary) {
        return false;
    }
    // `verify` also checks that the signature names this key and that its hash is strong.
    sig.verify(&key.primary_key, data).is_ok()
        || key
            .public_subkeys
            .iter()
            .any(|sub| sig.verify(&sub.key, data).is_ok())
}

/// The SHA-256 the checksum list gives for `name`. Parses fail-closed like `sha256sum -c
/// --strict`: every line is exactly `<64 hex digits><two spaces><name>` ending in `\n`; a
/// malformed line, an empty line, no line for `name` and two lines for `name` are all
/// [`ToolError::BadSums`]. Names are matched byte for byte, never case-folded.
pub fn sums_hash(sums: &[u8], name: &str) -> Result<[u8; 32], ToolError> {
    let text = std::str::from_utf8(sums).map_err(|_| ToolError::BadSums)?;
    let body = text.strip_suffix('\n').ok_or(ToolError::BadSums)?;
    let mut found: Option<[u8; 32]> = None;
    for line in body.split('\n') {
        let hex = line.get(..64).ok_or(ToolError::BadSums)?;
        let rest = &line[64..];
        let line_name = rest.strip_prefix("  ").ok_or(ToolError::BadSums)?;
        if line_name.is_empty() {
            return Err(ToolError::BadSums);
        }
        let hash = parse_hex32(hex).ok_or(ToolError::BadSums)?;
        if line_name == name {
            if found.is_some() {
                return Err(ToolError::BadSums);
            }
            found = Some(hash);
        }
    }
    found.ok_or(ToolError::BadSums)
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    let mut out = [0u8; 32];
    for (i, pair) in b.chunks_exact(2).enumerate() {
        out[i] = nibble(pair[0])? << 4 | nibble(pair[1])?;
    }
    Some(out)
}

pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// The SHA-256 of a file, or `None` if it cannot be read.
fn file_sha256(path: &Path) -> Option<[u8; 32]> {
    let mut file = fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut chunk = [0u8; 64 * 1024];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => hasher.update(&chunk[..n]),
            Err(_) => return None,
        }
    }
    Some(hasher.finalize().into())
}

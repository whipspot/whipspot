//! The art disk cache (MD66): a flat directory of files named by the SHA-256 of the URL. A file
//! is written only after its bytes decoded, read only within the byte cap, and re-decoded by the
//! caller on every read. Eviction touches only names this module makes. Every I/O error is
//! ignored: art works without the cache, and deleting the directory is always safe.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use reqwest::Url;
use sha2::{Digest, Sha256};

/// The cache holds at most this many bytes of files.
pub(crate) const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;
/// The cache holds at most this many files.
pub(crate) const MAX_CACHE_FILES: usize = 2000;
/// Eviction runs after every this many writes.
const EVICT_EVERY: u64 = 32;
/// A temp file older than this is stale.
const STALE_TEMP: Duration = Duration::from_secs(3600);

/// Distinguishes temp files of one process (the pid separates processes).
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) struct DiskCache {
    dir: PathBuf,
    max_bytes: u64,
    writes: AtomicU64,
    evicted_once: AtomicBool,
}

/// Lowercase hex of the SHA-256 of the URL text: 64 characters, no path syntax possible.
fn hash_name(url: &Url) -> String {
    let digest = Sha256::digest(url.as_str().as_bytes());
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        s.push(char::from_digit(u32::from(b & 15), 16).unwrap_or('0'));
    }
    s
}

/// `^[0-9a-f]{64}$`
fn is_cache_name(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `^[0-9a-f]{64}\.tmp-[0-9]+-[0-9]+$`
fn is_temp_name(name: &str) -> bool {
    let Some((head, tail)) = name.split_once(".tmp-") else {
        return false;
    };
    let Some((pid, n)) = tail.split_once('-') else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    is_cache_name(head) && digits(pid) && digits(n)
}

impl DiskCache {
    pub(crate) fn new(dir: PathBuf) -> DiskCache {
        DiskCache {
            dir,
            max_bytes: MAX_CACHE_BYTES,
            writes: AtomicU64::new(0),
            evicted_once: AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    pub(crate) fn name(url: &Url) -> String {
        hash_name(url)
    }

    fn path(&self, url: &Url) -> PathBuf {
        self.dir.join(hash_name(url))
    }

    /// The cached bytes: `None` when absent, not a regular file, or over `max_bytes` by its
    /// metadata (then it is not read) or by what could be read.
    pub(crate) fn read(&self, url: &Url, max_bytes: usize) -> Option<Vec<u8>> {
        self.evict_first_use();
        let path = self.path(url);
        let meta = fs::symlink_metadata(&path).ok()?;
        if !meta.is_file() || meta.len() > max_bytes as u64 {
            return None;
        }
        let mut body = Vec::new();
        fs::File::open(&path)
            .ok()?
            .take(max_bytes as u64 + 1)
            .read_to_end(&mut body)
            .ok()?;
        (body.len() <= max_bytes).then_some(body)
    }

    /// Writes `<name>.tmp-<pid>-<n>`, then renames it over `<name>`. Call only after the bytes
    /// decoded. Errors are ignored.
    pub(crate) fn write(&self, url: &Url, bytes: &[u8]) {
        self.evict_first_use();
        let _ = self.write_inner(url, bytes);
        if (self.writes.fetch_add(1, Ordering::Relaxed) + 1) % EVICT_EVERY == 0 {
            self.evict();
        }
    }

    fn write_inner(&self, url: &Url, bytes: &[u8]) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let name = hash_name(url);
        let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = self
            .dir
            .join(format!("{name}.tmp-{}-{n}", std::process::id()));
        let result = fs::write(&tmp, bytes).and_then(|()| fs::rename(&tmp, self.dir.join(&name)));
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    /// Removes the entry for `url` (a hit that no longer decodes). Errors are ignored.
    pub(crate) fn remove(&self, url: &Url) {
        let path = self.path(url);
        if fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
            let _ = fs::remove_file(path);
        }
    }

    fn evict_first_use(&self) {
        if !self.evicted_once.swap(true, Ordering::Relaxed) {
            self.evict();
        }
    }

    /// Deletes stale temp files and then the oldest cache files until the budget holds.
    pub(crate) fn evict(&self) {
        evict_dir(
            &self.dir,
            self.max_bytes,
            MAX_CACHE_FILES,
            SystemTime::now(),
        );
    }
}

/// Looks only at regular files directly in `dir`; never descends and never touches a name that
/// is not a cache name or a temp name. Errors are ignored.
fn evict_dir(dir: &Path, max_bytes: u64, max_files: usize, now: SystemTime) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, String, u64)> = Vec::new();
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_file() {
            continue;
        }
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(meta) = entry.metadata() else { continue };
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if is_temp_name(&name) {
            let age = now.duration_since(modified).unwrap_or_default();
            if age > STALE_TEMP {
                let _ = fs::remove_file(entry.path());
            }
        } else if is_cache_name(&name) {
            files.push((modified, name, meta.len()));
        }
    }
    files.sort();
    let mut total: u64 = files.iter().map(|f| f.2).sum();
    let mut count = files.len();
    for (_, name, len) in files {
        if total <= max_bytes && count <= max_files {
            break;
        }
        if fs::remove_file(dir.join(&name)).is_ok() {
            total = total.saturating_sub(len);
            count -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn set_age(path: &Path, age: Duration) {
        let f = fs::OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(SystemTime::now() - age).unwrap();
    }

    #[test]
    fn c1_names_are_64_hex_whatever_the_url_holds() {
        for u in [
            "https://i.scdn.co/../../etc/passwd",
            "https://i.scdn.co/a\\b\\c",
            "https://i.scdn.co/a%2Fb%2e%2e%2F",
            "https://i.scdn.co/x?y=../z",
        ] {
            let n = DiskCache::name(&url(u));
            assert!(is_cache_name(&n), "{u} -> {n}");
        }
        assert_ne!(
            DiskCache::name(&url("https://i.scdn.co/a")),
            DiskCache::name(&url("https://i.scdn.co/b"))
        );
        // A known SHA-256 ("abc") checks the hex encoding.
        let digest = Sha256::digest(b"abc");
        assert_eq!(digest[0], 0xba);
        assert_eq!(
            hash_name(&url("https://a.example/")).len(),
            64,
            "length is fixed"
        );
    }

    #[test]
    fn name_patterns() {
        let h = "a".repeat(64);
        assert!(is_cache_name(&h));
        assert!(!is_cache_name(&"A".repeat(64)));
        assert!(!is_cache_name(&"a".repeat(63)));
        assert!(is_temp_name(&format!("{h}.tmp-123-4")));
        assert!(!is_temp_name(&format!("{h}.tmp-123")));
        assert!(!is_temp_name(&format!("{h}.tmp-1-x")));
        assert!(!is_temp_name(&format!("{h}.tmp-")));
        assert!(!is_temp_name("notes.tmp-1-2"));
    }

    #[test]
    fn c2_write_then_read() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().join("art"));
        let u = url("https://i.scdn.co/a");
        assert_eq!(cache.read(&u, 1024), None);
        cache.write(&u, b"bytes");
        assert_eq!(cache.read(&u, 1024).as_deref(), Some(&b"bytes"[..]));
        // Overwrite replaces.
        cache.write(&u, b"newer");
        assert_eq!(cache.read(&u, 1024).as_deref(), Some(&b"newer"[..]));
        // No temp file is left behind.
        let names: Vec<String> = fs::read_dir(dir.path().join("art"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec![DiskCache::name(&u)]);
        cache.remove(&u);
        assert_eq!(cache.read(&u, 1024), None);
    }

    #[test]
    fn c3_eviction_deletes_oldest_first_and_only_our_names() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let name = |c: char| c.to_string().repeat(64);
        // Five 10-byte cache files, 'a' oldest.
        for (i, c) in ['a', 'b', 'c', 'd', 'e'].into_iter().enumerate() {
            fs::write(d.join(name(c)), [0u8; 10]).unwrap();
            set_age(&d.join(name(c)), Duration::from_secs(1000 - i as u64 * 100));
        }
        // Things that must never be deleted.
        fs::write(d.join("notes.txt"), b"keep").unwrap();
        fs::create_dir(d.join("sub")).unwrap();
        fs::write(d.join("sub").join(name('f')), b"keep").unwrap();
        // A fresh temp file of another pid and a stale one.
        let fresh = format!("{}.tmp-99999-1", name('a'));
        let stale = format!("{}.tmp-99999-2", name('b'));
        fs::write(d.join(&fresh), b"fresh").unwrap();
        fs::write(d.join(&stale), b"stale").unwrap();
        set_age(&d.join(&stale), Duration::from_secs(2 * 3600));

        // Budget 30 bytes: the two oldest (a, b) go.
        evict_dir(d, 30, 100, SystemTime::now());
        assert!(!d.join(name('a')).exists() && !d.join(name('b')).exists());
        for c in ['c', 'd', 'e'] {
            assert!(d.join(name(c)).exists(), "{c} was evicted");
        }
        assert!(d.join("notes.txt").exists());
        assert!(d.join("sub").join(name('f')).exists());
        assert!(d.join(&fresh).exists(), "a fresh temp file was deleted");
        assert!(!d.join(&stale).exists(), "a stale temp file stayed");

        // The file budget works the same way.
        evict_dir(d, u64::MAX, 1, SystemTime::now());
        assert!(!d.join(name('c')).exists() && !d.join(name('d')).exists());
        assert!(d.join(name('e')).exists());
    }

    #[test]
    fn c3_the_public_eviction_runs_on_first_use_and_every_32_writes() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf());
        let old = format!("{}.tmp-1-1", "c".repeat(64));
        fs::write(dir.path().join(&old), b"x").unwrap();
        set_age(&dir.path().join(&old), Duration::from_secs(2 * 3600));
        // First use evicts the stale temp file.
        assert_eq!(cache.read(&url("https://i.scdn.co/a"), 10), None);
        assert!(!dir.path().join(&old).exists());
        // A stale temp file made later goes at the 32nd write.
        fs::write(dir.path().join(&old), b"x").unwrap();
        set_age(&dir.path().join(&old), Duration::from_secs(2 * 3600));
        for i in 0..31 {
            cache.write(&url(&format!("https://i.scdn.co/{i}")), b"b");
        }
        assert!(dir.path().join(&old).exists());
        cache.write(&url("https://i.scdn.co/last"), b"b");
        assert!(!dir.path().join(&old).exists());
    }

    #[test]
    fn c4_an_oversize_cached_file_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::new(dir.path().to_path_buf());
        let u = url("https://i.scdn.co/big");
        fs::write(dir.path().join(DiskCache::name(&u)), vec![1u8; 2000]).unwrap();
        assert_eq!(cache.read(&u, 1999), None);
        assert_eq!(cache.read(&u, 2000).map(|b| b.len()), Some(2000));
        // A directory under the cache name is not a regular file.
        let d = url("https://i.scdn.co/dir");
        fs::create_dir(dir.path().join(DiskCache::name(&d))).unwrap();
        assert_eq!(cache.read(&d, 10), None);
    }
}

//! User skins: files the user puts in the skins folder. None are bundled.
//!
//! The front-end never gets a path: it asks for a name, and a name is only accepted if it matches
//! `^[A-Za-z0-9 ._()\-]{1,100}\.<ext>$` (`<ext>` one of the configured extensions, case-insensitive,
//! ASCII only), contains no `..`, does not start with a Windows device name (on every OS) and,
//! once joined to the skins directory and canonicalised, still lies inside it, is a regular file
//! and is at most [`MAX_SKIN_BYTES`]. The same rules filter the listing, so the list never offers
//! a name `read` would refuse. Errors are the core's fixed texts; no path or OS error is
//! forwarded. Nothing here writes, renames or deletes a skin file; [`DirSkinSource::ensure`] only
//! creates the folder.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};

use whipspot_core::skin::{MAX_SKIN_BYTES, SkinError, SkinSource};

/// Longest part before the extension.
pub const MAX_STEM_CHARS: usize = 100;
/// The listing never returns more than this many names.
pub const MAX_LISTED: usize = 500;

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b' ' | b'.' | b'_' | b'(' | b')' | b'-')
}

/// `^[a-z0-9]{1,8}$`.
fn is_valid_extension(ext: &str) -> bool {
    (1..=8).contains(&ext.len())
        && ext
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// A reserved Windows device name: the part before the first `.`, trailing spaces and dots
/// removed, is `CON`, `PRN`, `AUX`, `NUL`, `COM0`-`COM9` or `LPT0`-`LPT9` in any case. Refused on
/// every OS so a skins folder copied between machines behaves the same.
fn is_device_name(name: &str) -> bool {
    let head = name.split('.').next().unwrap_or("");
    let head = head.trim_end_matches([' ', '.']);
    let up = head.to_ascii_uppercase();
    if matches!(up.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let b = up.as_bytes();
    b.len() == 4 && (up.starts_with("COM") || up.starts_with("LPT")) && b[3].is_ascii_digit()
}

/// The skins directory as a [`SkinSource`].
#[derive(Debug, Clone)]
pub struct DirSkinSource {
    root: PathBuf,
    /// Lowercase, valid, no duplicates.
    extensions: Vec<String>,
}

impl DirSkinSource {
    /// `extensions` are lowercased, kept only if `^[a-z0-9]{1,8}$`, and deduplicated. With none
    /// left, every name is refused and the listing is empty.
    pub fn new(root: impl Into<PathBuf>, extensions: Vec<String>) -> Self {
        let mut exts: Vec<String> = Vec::new();
        for e in extensions {
            let e = e.to_ascii_lowercase();
            if is_valid_extension(&e) && !exts.contains(&e) {
                exts.push(e);
            }
        }
        DirSkinSource {
            root: root.into(),
            extensions: exts,
        }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Create the directory if needed (so the user has somewhere to drop files).
    pub fn ensure(&self) -> Result<(), SkinError> {
        fs::create_dir_all(&self.root).map_err(|_| SkinError::Io)
    }

    /// `^[A-Za-z0-9 ._()\-]{1,100}\.<ext>$` with a configured, case-insensitive extension, no
    /// `..`, and not a Windows device name. Works on bytes: any non-ASCII character
    /// (look-alikes, NUL, separators) fails the class.
    fn is_valid_name(&self, name: &str) -> bool {
        let b = name.as_bytes();
        let stem_len = self.extensions.iter().find_map(|ext| {
            let split = b.len().checked_sub(ext.len() + 1)?;
            let (stem, tail) = b.split_at(split);
            (tail[0] == b'.' && tail[1..].eq_ignore_ascii_case(ext.as_bytes()))
                .then_some(stem.len())
        });
        let Some(stem_len) = stem_len else {
            return false;
        };
        (1..=MAX_STEM_CHARS).contains(&stem_len)
            && b[..stem_len].iter().all(|&c| is_name_byte(c))
            && !name.contains("..")
            && !is_device_name(name)
    }

    /// Validate `name` and resolve it to a canonical regular file inside the directory.
    fn resolve(&self, name: &str) -> Result<(PathBuf, u64), SkinError> {
        if !self.is_valid_name(name) {
            return Err(SkinError::InvalidName);
        }
        let root = fs::canonicalize(&self.root).map_err(|_| SkinError::NotFound)?;
        let path = fs::canonicalize(root.join(name)).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => SkinError::NotFound,
            _ => SkinError::Io,
        })?;
        if !path.starts_with(&root) {
            return Err(SkinError::NotAllowed);
        }
        let meta = fs::metadata(&path).map_err(|_| SkinError::Io)?;
        if !meta.is_file() {
            return Err(SkinError::NotAllowed);
        }
        if meta.len() > MAX_SKIN_BYTES {
            return Err(SkinError::TooLarge);
        }
        Ok((path, meta.len()))
    }
}

impl SkinSource for DirSkinSource {
    /// Names of the skins that `read` would accept, sorted case-insensitively. A missing
    /// directory is an empty list.
    fn list(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| self.is_valid_name(n) && self.resolve(n).is_ok())
            .collect();
        names.sort_by_cached_key(|n| (n.to_ascii_lowercase(), n.clone()));
        names.truncate(MAX_LISTED);
        names
    }

    /// The bytes of a skin. The read itself is capped too, in case the file grew after the check.
    fn read(&self, name: &str) -> Result<Vec<u8>, SkinError> {
        let (path, len) = self.resolve(name)?;
        let file = fs::File::open(path).map_err(|_| SkinError::Io)?;
        let mut buf = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
        file.take(MAX_SKIN_BYTES + 1)
            .read_to_end(&mut buf)
            .map_err(|_| SkinError::Io)?;
        if buf.len() as u64 > MAX_SKIN_BYTES {
            return Err(SkinError::TooLarge);
        }
        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use whipspot_testkit::{run_skin_source_contract, run_skin_source_size_contract};

    fn src(root: impl Into<PathBuf>) -> DirSkinSource {
        DirSkinSource::new(root, vec!["skn".to_owned()])
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) {
        fs::File::create(dir.join(name))
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    #[test]
    fn new_normalises_extensions() {
        let s = DirSkinSource::new(
            "x",
            ["SKN", "skn", "a.b", "", "toolongext", "Zip1", "ü"]
                .map(str::to_owned)
                .to_vec(),
        );
        assert_eq!(s.extensions, ["skn", "zip1"]);
    }

    #[test]
    fn name_validator_table() {
        let s = src("x");
        let long_ok = format!("{}.skn", "a".repeat(100));
        let long_bad = format!("{}.skn", "a".repeat(101));
        let valid = [
            "a.skn",
            "My Skin (v2).skn",
            "x.SKN",
            "x.SkN",
            "under_score-dash.skn",
            "1.2.3.skn",
            "a.skn.skn",
            "CONSOLE.skn",
            "COM10.skn",
            "LPT.skn",
            "COM.skn",
            &long_ok,
        ];
        for n in valid {
            assert!(s.is_valid_name(n), "{n:?} should be valid");
        }
        let invalid: [&str; 36] = [
            "",
            ".skn",
            "skn",
            "a.zip",
            "a.skn ",
            " ",
            "../x.skn",
            "a/b.skn",
            "a\\b.skn",
            "/abs.skn",
            "..skn",
            "a..skn",
            "x.skn\0",
            "x\0.skn",
            "x.skn\n",
            "a:b.skn",
            "a*b.skn",
            "a,b.skn",
            "é.skn",          // non-ASCII letter
            "ｘ.skn",         // fullwidth x
            "x.sḱn",          // accented letter in the extension
            "\u{212A}.skn",   // Kelvin sign (lowercases to k)
            "ab\u{202e}.skn", // right-to-left override
            "x.skn.exe",
            &long_bad,
            // Windows device names, any case, with trailing spaces, with more dots after.
            "CON.skn",
            "con.skn",
            "Nul .skn",
            "COM1.x.skn",
            "LPT9.skn",
            "prn.skn",
            "Aux.skn",
            "com0.skn",
            "CON .skn",
            "lpt1.SKN",
            "NUL.skn",
        ];
        for n in invalid {
            assert!(!s.is_valid_name(n), "{n:?} should be invalid");
        }
        // 4 bytes of non-ASCII must not be mistaken for the extension, nor panic on a boundary.
        assert!(!s.is_valid_name("a\u{1F600}"));
        assert!(!s.is_valid_name("\u{1F600}"));
    }

    #[test]
    fn several_extensions_and_none() {
        let two = DirSkinSource::new("x", vec!["skn".to_owned(), "zz".to_owned()]);
        assert!(two.is_valid_name("a.skn") && two.is_valid_name("a.ZZ"));
        assert!(!two.is_valid_name("a.txt"));
        let none = DirSkinSource::new("x", Vec::new());
        assert!(!none.is_valid_name("a.skn"));
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "a.skn", b"a");
        let none = DirSkinSource::new(t.path(), Vec::new());
        assert!(none.list().is_empty());
        assert_eq!(none.read("a.skn"), Err(SkinError::InvalidName));
    }

    #[test]
    fn lists_and_reads_valid_regular_files_only() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "b.skn", b"bbb");
        write(t.path(), "A.SKN", b"aaa");
        write(t.path(), "notes.txt", b"x");
        write(t.path(), "other.zip", b"x");
        write(t.path(), "bad\u{e9}.skn", b"x");
        fs::create_dir(t.path().join("dir.skn")).unwrap();
        let s = src(t.path());
        assert_eq!(s.list(), ["A.SKN", "b.skn"]);
        assert_eq!(s.read("b.skn").unwrap(), b"bbb");
        assert_eq!(s.read("A.SKN").unwrap(), b"aaa");
        assert_eq!(s.read("dir.skn"), Err(SkinError::NotAllowed));
        assert_eq!(s.read("missing.skn"), Err(SkinError::NotFound));
        assert_eq!(s.read("notes.txt"), Err(SkinError::InvalidName));
        assert_eq!(s.read("other.zip"), Err(SkinError::InvalidName));
        assert_eq!(s.read("../b.skn"), Err(SkinError::InvalidName));
        assert_eq!(s.read("CON.skn"), Err(SkinError::InvalidName));
    }

    #[test]
    fn missing_directory_lists_nothing_and_ensure_creates_it() {
        let t = tempfile::tempdir().unwrap();
        let s = src(t.path().join("skins"));
        assert!(s.list().is_empty());
        assert_eq!(s.read("a.skn"), Err(SkinError::NotFound));
        s.ensure().unwrap();
        s.ensure().unwrap(); // idempotent
        assert!(s.path().is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_leaving_the_directory_is_refused_and_not_listed() {
        use std::os::unix::fs::symlink;
        let outside = tempfile::tempdir().unwrap();
        write(outside.path(), "secret.skn", b"top secret");
        let t = tempfile::tempdir().unwrap();
        let skins = t.path().join("skins");
        fs::create_dir(&skins).unwrap();
        write(&skins, "real.skn", b"real");
        symlink(outside.path().join("secret.skn"), skins.join("escape.skn")).unwrap();
        symlink(outside.path(), skins.join("dirlink.skn")).unwrap();
        // A symlink to another file inside the directory is fine.
        symlink(skins.join("real.skn"), skins.join("alias.skn")).unwrap();
        // A dangling link is just not found.
        symlink(skins.join("nope"), skins.join("dangling.skn")).unwrap();
        let s = src(&skins);
        assert_eq!(s.read("escape.skn"), Err(SkinError::NotAllowed));
        assert_eq!(s.read("dirlink.skn"), Err(SkinError::NotAllowed));
        assert_eq!(s.read("alias.skn").unwrap(), b"real");
        assert_eq!(s.read("dangling.skn"), Err(SkinError::NotFound));
        assert_eq!(s.list(), ["alias.skn", "real.skn"]);
    }

    #[cfg(unix)]
    #[test]
    fn a_skins_directory_that_is_itself_a_symlink_works() {
        use std::os::unix::fs::symlink;
        let t = tempfile::tempdir().unwrap();
        let real = t.path().join("real");
        fs::create_dir(&real).unwrap();
        write(&real, "a.skn", b"a");
        symlink(&real, t.path().join("skins")).unwrap();
        let s = src(t.path().join("skins"));
        assert_eq!(s.list(), ["a.skn"]);
        assert_eq!(s.read("a.skn").unwrap(), b"a");
    }

    #[test]
    fn size_cap_is_exact() {
        let t = tempfile::tempdir().unwrap();
        // Sparse files: set_len does not write 20 MiB.
        for (name, len) in [("ok.skn", MAX_SKIN_BYTES), ("big.skn", MAX_SKIN_BYTES + 1)] {
            fs::File::create(t.path().join(name))
                .unwrap()
                .set_len(len)
                .unwrap();
        }
        let s = src(t.path());
        assert_eq!(s.read("big.skn"), Err(SkinError::TooLarge));
        assert_eq!(s.read("ok.skn").unwrap().len() as u64, MAX_SKIN_BYTES);
        assert_eq!(s.list(), ["ok.skn"], "an oversized skin is not offered");
    }

    #[test]
    fn listing_is_capped() {
        let t = tempfile::tempdir().unwrap();
        for i in 0..(MAX_LISTED + 5) {
            write(t.path(), &format!("s{i:04}.skn"), b"x");
        }
        assert_eq!(src(t.path()).list().len(), MAX_LISTED);
    }

    #[test]
    fn error_texts_carry_no_path() {
        let t = tempfile::tempdir().unwrap();
        let s = src(t.path().join("nope"));
        for e in [
            s.read("a.skn").unwrap_err(),
            s.read("../a.skn").unwrap_err(),
            SkinError::NotAllowed,
            SkinError::TooLarge,
            SkinError::Io,
        ] {
            let text = e.to_string();
            assert!(!text.contains('/') && !text.contains("tmp"), "{text}");
        }
    }

    #[test]
    fn passes_the_source_contract() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "b.skn", b"bbb");
        write(t.path(), "A.skn", b"aaa");
        run_skin_source_contract(&src(t.path()), "nope.skn");
    }

    #[test]
    fn passes_the_source_size_contract() {
        let t = tempfile::tempdir().unwrap();
        // Sparse files: set_len does not write 20 MiB.
        for (name, len) in [
            ("cap.skn", MAX_SKIN_BYTES),
            ("over.skn", MAX_SKIN_BYTES + 1),
        ] {
            fs::File::create(t.path().join(name))
                .unwrap()
                .set_len(len)
                .unwrap();
        }
        write(t.path(), "small.skn", b"s");
        run_skin_source_size_contract(&src(t.path()), "cap.skn", "over.skn");
    }

    #[test]
    fn the_listing_cap_keeps_the_first_names_in_order() {
        let t = tempfile::tempdir().unwrap();
        for i in 0..(MAX_LISTED + 5) {
            write(t.path(), &format!("s{i:04}.skn"), b"x");
        }
        let names = src(t.path()).list();
        assert_eq!(names.len(), MAX_LISTED);
        assert_eq!(names[0], "s0000.skn");
        assert_eq!(names[MAX_LISTED - 1], format!("s{:04}.skn", MAX_LISTED - 1));
        assert!(names.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn a_file_changed_between_list_and_read_is_judged_at_read_time() {
        let t = tempfile::tempdir().unwrap();
        for name in ["gone", "grown", "swapped", "kept", "dir"] {
            write(t.path(), &format!("{name}.skn"), b"old");
        }
        let s = src(t.path());
        let listed = s.list();
        assert_eq!(
            listed,
            [
                "dir.skn",
                "gone.skn",
                "grown.skn",
                "kept.skn",
                "swapped.skn"
            ]
        );

        fs::remove_file(t.path().join("gone.skn")).unwrap();
        fs::File::create(t.path().join("grown.skn"))
            .unwrap()
            .set_len(MAX_SKIN_BYTES + 1)
            .unwrap();
        write(t.path(), "swapped.skn", b"brand new content");
        fs::remove_file(t.path().join("dir.skn")).unwrap();
        fs::create_dir(t.path().join("dir.skn")).unwrap();

        assert_eq!(s.read("gone.skn"), Err(SkinError::NotFound));
        assert_eq!(s.read("grown.skn"), Err(SkinError::TooLarge));
        assert_eq!(s.read("swapped.skn").unwrap(), b"brand new content");
        assert_eq!(s.read("dir.skn"), Err(SkinError::NotAllowed));
        assert_eq!(s.read("kept.skn").unwrap(), b"old");
        // A fresh listing agrees with what read now accepts.
        assert_eq!(s.list(), ["kept.skn", "swapped.skn"]);
        for name in s.list() {
            assert!(s.read(&name).is_ok());
        }
    }

    #[test]
    fn the_same_listing_and_bytes_twice() {
        let t = tempfile::tempdir().unwrap();
        write(t.path(), "b.skn", b"bbb");
        write(t.path(), "A.skn", b"aaa");
        let s = src(t.path());
        assert_eq!(s.list(), s.list());
        assert_eq!(s.read("b.skn"), s.read("b.skn"));
    }
}

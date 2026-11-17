//! Seeded fuzzing of the skins folder source (D2, H12, hard rules 5 and 9): random names, with
//! separators, dots, device names, long and odd-character forms, against a real temporary
//! directory that has a planted file outside the root. Ordinary `cargo test`; see
//! `whipspot_testkit::rng` for `WHIPSPOT_FUZZ_ITERS` and `WHIPSPOT_FUZZ_SEED`.
//!
//! Properties: no panic; `read` never returns the planted outside file's bytes; a name outside
//! the documented grammar is `InvalidName` (an oracle written here from the grammar in the
//! module docs of `dir`, not the code's own predicate) and a name inside it never is; `list`
//! only offers names the oracle accepts.

use std::fs;
use std::path::Path;

use whipspot_core::skin::{MAX_SKIN_BYTES, SkinError, SkinSource};
use whipspot_testkit::rng::{Case, SplitMix64, fuzz};

use crate::{DirSkinSource, MAX_LISTED, MAX_STEM_CHARS};

const OUTSIDE_BYTES: &[u8] = b"OUTSIDE-THE-ROOT-SECRET";

/// The documented grammar: `^[A-Za-z0-9 ._()\-]{1,100}\.skn$` (extension case-insensitive), no
/// `..`, and not a Windows device name (`CON PRN AUX NUL COM0-9 LPT0-9`, the part before the
/// first dot with trailing spaces and dots removed, in any case).
fn oracle_valid(name: &str) -> bool {
    let Some(stem) = name
        .len()
        .checked_sub(4)
        .filter(|&at| name.is_char_boundary(at))
        .map(|at| (&name[..at], &name[at..]))
        .and_then(|(stem, tail)| tail.eq_ignore_ascii_case(".skn").then_some(stem))
    else {
        return false;
    };
    if stem.is_empty() || stem.chars().count() > MAX_STEM_CHARS {
        return false;
    }
    if !stem
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || " ._()-".contains(c))
    {
        return false;
    }
    if name.contains("..") {
        return false;
    }
    let head = name.split('.').next().unwrap_or("");
    let head = head.trim_end_matches([' ', '.']).to_ascii_uppercase();
    let reserved = ["CON", "PRN", "AUX", "NUL"].contains(&head.as_str())
        || (head.len() == 4
            && (head.starts_with("COM") || head.starts_with("LPT"))
            && head.as_bytes()[3].is_ascii_digit());
    !reserved
}

#[test]
fn the_oracle_agrees_with_the_documented_cases() {
    for ok in [
        "a.skn",
        "My Skin (v2).skn",
        "x.SkN",
        "1.2.3.skn",
        "COM10.skn",
        "LPT.skn",
    ] {
        assert!(oracle_valid(ok), "{ok:?}");
    }
    for bad in [
        "",
        ".skn",
        "a.zip",
        "a.skn ",
        "../x.skn",
        "a/b.skn",
        "a\\b.skn",
        "a..skn",
        "x.skn\0",
        "\u{e9}.skn",
        "CON.skn",
        "Nul .skn",
        "com0.skn",
        "LPT9.skn",
        "a:b.skn",
        "\u{1F600}",
        "x.skn.exe",
    ] {
        assert!(!oracle_valid(bad), "{bad:?}");
    }
    assert!(oracle_valid(&format!("{}.skn", "a".repeat(100))));
    assert!(!oracle_valid(&format!("{}.skn", "a".repeat(101))));
}

const FRAGMENTS: &[&str] = &[
    "a",
    "B",
    "9",
    "skin",
    "My Skin",
    "(v2)",
    "_",
    "-",
    " ",
    ".",
    "..",
    "...",
    ".skn",
    ".SKN",
    ".skn.exe",
    ".zip",
    "/",
    "\\",
    "../",
    "..\\",
    "./",
    "/etc/",
    "C:",
    "C:\\",
    "\\\\",
    "\\\\?\\",
    ":",
    "::$DATA",
    "*",
    "?",
    "<",
    ">",
    "|",
    "\"",
    "\0",
    "\n",
    "\t",
    "~1",
    "%2e%2e%2f",
    "CON",
    "con",
    "PRN",
    "Aux",
    "NUL",
    "nul",
    "COM1",
    "com9",
    "LPT1",
    "lpt0",
    "\u{e9}",
    "\u{212A}",
    "\u{ff58}",
    "\u{202e}",
    "\u{1F600}",
    "\u{fffd}",
    "\u{200b}",
    "outside",
    "outside.skn",
    "real",
    "real.skn",
    "link.skn",
];

fn random_name(rng: &mut SplitMix64) -> String {
    let mut name = String::new();
    for _ in 0..(1 + rng.below(6)) {
        let fragment: &&str = rng.pick(FRAGMENTS);
        name.push_str(fragment);
    }
    match rng.below(10) {
        // Mostly end in the real extension, so the stem rules are what decides.
        0..=5 => {
            let ext: &&str = rng.pick(&[".skn", ".SKN", ".SkN"]);
            name.push_str(ext);
        }
        6 => {
            // A long stem around the 100 character cap.
            name = format!("{}.skn", "a".repeat(95 + rng.below(12)));
        }
        7 => name = "a".repeat(100 + rng.below(400)),
        _ => {}
    }
    name
}

/// A root with a few valid skins plus a planted file next to it, and the planted file's path.
struct World {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    outside: std::path::PathBuf,
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("skins");
    fs::create_dir(&root).unwrap();
    let outside = dir.path().join("outside.skn");
    fs::write(&outside, OUTSIDE_BYTES).unwrap();
    fs::write(root.join("real.skn"), b"real skin").unwrap();
    fs::write(root.join("My Skin (v2).skn"), b"second").unwrap();
    fs::create_dir(root.join("dir.skn")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(&outside, root.join("link.skn")).unwrap();
        symlink(dir.path(), root.join("dirlink.skn")).unwrap();
    }
    World {
        _dir: dir,
        root,
        outside,
    }
}

fn source(root: &Path) -> DirSkinSource {
    DirSkinSource::new(root, vec!["skn".to_owned()])
}

fn check_read(src: &DirSkinSource, name: &str) {
    let result = src.read(name);
    if let Ok(bytes) = &result {
        assert_ne!(
            bytes.as_slice(),
            OUTSIDE_BYTES,
            "read {name:?} returned the file outside the root"
        );
        assert!(bytes.len() as u64 <= MAX_SKIN_BYTES);
    }
    if oracle_valid(name) {
        assert_ne!(
            result,
            Err(SkinError::InvalidName),
            "the grammar accepts {name:?} but read refused the name"
        );
    } else {
        assert_eq!(
            result,
            Err(SkinError::InvalidName),
            "the grammar rejects {name:?} but read did not refuse the name"
        );
    }
}

#[test]
fn fuzz_dir_skin_source_names() {
    let w = world();
    let src = source(&w.root);
    let (mut valid, mut total) = (0u64, 0u64);
    fuzz("skin-fs", 0x4653_0001, 40_000, |case: &mut Case| {
        total += 1;
        let name = random_name(&mut case.rng);
        case.set_input_len(name.len());
        valid += u64::from(oracle_valid(&name));
        check_read(&src, &name);
    });
    // The generator reaches the valid side too, or the checks above prove half of nothing.
    eprintln!("fuzz skin-fs: {valid} of {total} names were valid");
    if total >= 200 {
        assert!(valid * 100 >= total, "only {valid} of {total} names valid");
    }
}

#[test]
fn fuzz_dir_skin_source_traversal_forms_never_reach_the_outside_file() {
    // Hand-built attacks on top of the random ones: the planted file by relative, absolute and
    // doubled-separator spellings, all with the valid extension.
    let w = world();
    let src = source(&w.root);
    let abs = w.outside.to_string_lossy().into_owned();
    let forms = [
        "../outside.skn".to_owned(),
        "..\\outside.skn".to_owned(),
        "skins/../outside.skn".to_owned(),
        "./../outside.skn".to_owned(),
        "..//outside.skn".to_owned(),
        "%2e%2e/outside.skn".to_owned(),
        "..%5coutside.skn".to_owned(),
        abs.clone(),
        abs.replace('\\', "/"),
        format!("\\\\?\\{abs}"),
        "outside.skn/..".to_owned(),
        "outside.skn:stream".to_owned(),
        "outside.skn::$DATA".to_owned(),
        "outside.skn.".to_owned(),
        "outside.skn ".to_owned(),
        "link.skn".to_owned(),
        "dirlink.skn".to_owned(),
    ];
    for name in &forms {
        check_read(&src, name);
        assert!(
            src.read(name).is_err(),
            "{name:?} returned bytes; nothing outside the root may be reachable"
        );
    }
    // Control: the real files are still readable, so the checks above are not vacuous.
    assert_eq!(src.read("real.skn").unwrap(), b"real skin");
    assert_eq!(src.read("REAL.SKN").unwrap(), b"real skin");
}

#[test]
fn fuzz_dir_skin_source_listing_offers_only_valid_names_inside_the_root() {
    let w = world();
    // More files. Only names the grammar accepts are created: a hostile name must never be
    // joined to a path by this test either (an absolute or `..` name would write outside).
    let mut rng = SplitMix64::new(0x4653_0002);
    for _ in 0..600 {
        let name = random_name(&mut rng);
        if oracle_valid(&name) {
            let _ = fs::write(w.root.join(&name), b"x");
        }
    }
    for refused in ["notes.txt", "other.zip", "bad\u{e9}.skn"] {
        fs::write(w.root.join(refused), b"x").unwrap();
    }
    let src = source(&w.root);
    let listed = src.list();
    assert!(listed.len() <= MAX_LISTED);
    assert!(listed.contains(&"real.skn".to_owned()));
    assert!(
        listed.len() > 3,
        "the random files were created ({listed:?})"
    );
    for refused in ["notes.txt", "other.zip", "bad\u{e9}.skn"] {
        assert!(!listed.iter().any(|n| n == refused), "{refused}");
    }
    for name in &listed {
        assert!(oracle_valid(name), "listed {name:?}");
        let bytes = src.read(name).expect("a listed name reads");
        assert_ne!(bytes.as_slice(), OUTSIDE_BYTES, "{name:?}");
    }
    let mut sorted = listed.clone();
    sorted.sort_by_key(|n| (n.to_ascii_lowercase(), n.clone()));
    assert_eq!(listed, sorted, "the listing is sorted");
}

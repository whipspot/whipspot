//! H2: every seam the core declares has a contract suite. The core's `pub trait`s are read from
//! its sources and checked against the `SEAMS` table below; a trait that is not listed fails the
//! test, so a new seam cannot ship without a suite (or a stated reason). A listed suite must also
//! be called from `tests/contracts.rs`, so it runs on its fake.

use std::path::{Path, PathBuf};

/// How a core trait is covered.
enum Cover {
    /// Its own suite, a function of the testkit.
    Suite(&'static str),
    /// A part of a seam whose suite exercises it (named by the suite function).
    CoveredBy(&'static str),
}

/// Every `pub trait` of `whipspot-core` and what covers it. Add a row with a new trait.
const SEAMS: &[(&str, Cover)] = &[
    ("ArtSource", Cover::Suite("run_art_source_contract")),
    (
        "CredentialStore",
        Cover::Suite("run_credential_store_contract"),
    ),
    ("AudioOutput", Cover::Suite("run_audio_output_contract")),
    ("LyricsSource", Cover::Suite("run_lyrics_source_contract")),
    ("MusicFolders", Cover::Suite("run_music_folders_contract")),
    ("Provider", Cover::Suite("run_provider_contract")),
    ("AuthFlow", Cover::CoveredBy("run_provider_contract")),
    (
        "Playback",
        Cover::CoveredBy("run_provider_playback_contract"),
    ),
    ("Library", Cover::CoveredBy("run_provider_session_contract")),
    ("Remote", Cover::Suite("run_remote_contract")),
    ("SkinEngine", Cover::Suite("run_skin_engine_contract")),
    ("SkinSource", Cover::Suite("run_skin_source_contract")),
    ("PanelWindows", Cover::Suite("run_panel_windows_contract")),
];

fn suite_of(cover: &Cover) -> &'static str {
    match cover {
        Cover::Suite(s) | Cover::CoveredBy(s) => s,
    }
}

/// The names of the `pub trait`s declared in `source` (a line that starts with `pub trait`).
fn pub_traits(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|line| line.trim_start().strip_prefix("pub trait "))
        .map(|rest| {
            rest.chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>()
        })
        .filter(|name| !name.is_empty())
        .collect()
}

/// The traits in `source` that `SEAMS` does not list.
fn unlisted(source: &str) -> Vec<String> {
    pub_traits(source)
        .into_iter()
        .filter(|t| !SEAMS.iter().any(|(name, _)| name == t))
        .collect()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read the core's source directory") {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn core_sources() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../whipspot-core/src");
    let mut files = Vec::new();
    rust_files(&dir, &mut files);
    assert!(
        !files.is_empty(),
        "no source found under {dir:?}: the scan would pass on nothing"
    );
    files
}

#[test]
fn every_core_trait_is_listed_with_a_suite() {
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for file in core_sources() {
        let text = std::fs::read_to_string(&file).expect("read a core source");
        found.extend(pub_traits(&text));
        for t in unlisted(&text) {
            missing.push(format!("{t} ({})", file.display()));
        }
    }
    assert!(
        missing.is_empty(),
        "core traits with no row in SEAMS (add a contract suite or say what covers it): {missing:?}"
    );
    // The instrument finds the traits we know exist, and every row names a real trait.
    for (name, _) in SEAMS {
        assert!(
            found.iter().any(|f| f == name),
            "SEAMS lists {name}, which the core does not declare"
        );
    }
}

#[test]
fn every_listed_suite_is_defined_and_run_on_its_fake() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let defined = std::fs::read_to_string(root.join("src/contract.rs")).expect("read contract.rs");
    let run = std::fs::read_to_string(root.join("tests/contracts.rs")).expect("read contracts.rs");
    for (trait_name, cover) in SEAMS {
        let suite = suite_of(cover);
        assert!(
            defined.contains(&format!("pub fn {suite}("))
                || defined.contains(&format!("pub async fn {suite}(")),
            "{trait_name}: suite {suite} is not defined in src/contract.rs"
        );
        assert!(
            run.contains(&format!("{suite}(")),
            "{trait_name}: suite {suite} is never called from tests/contracts.rs"
        );
    }
}

#[test]
fn the_scanner_flags_a_seeded_new_trait() {
    let seeded = "pub trait Fresh: Send {\n    fn go(&self);\n}\n    pub trait Provider {}\n";
    assert_eq!(pub_traits(seeded), vec!["Fresh", "Provider"]);
    assert_eq!(unlisted(seeded), vec!["Fresh"]);
    // Trait text that is not a declaration is not a trait.
    assert!(pub_traits("// pub trait Nope\nlet x = \"pub trait Nope\";\n").is_empty());
    assert!(unlisted("pub trait Provider: Send + Sync {}").is_empty());
}

//! Seeded mutation fuzzing of the skin parsers (D2, H12, hard rule 5): the zip reader, the BMP
//! decoder and the whole engine on mutated synthetic archives. Ordinary `cargo test` cases on
//! stable Rust; `WHIPSPOT_FUZZ_ITERS` and `WHIPSPOT_FUZZ_SEED` scale and vary them (see
//! `whipspot_testkit::rng`). Not coverage-guided.
//!
//! Properties: nothing panics (the harness prints the seed and iteration, then resumes the
//! panic); every `Ok` respects the caps (entry count, per-entry and total inflated bytes, the
//! RGBA budget, `Skin::validate`).

use whipspot_core::art::MAX_ART_SIDE;
use whipspot_core::skin::SkinEngine;
use whipspot_testkit::rng::{Case, SplitMix64, edge_u32, fuzz, mutate};

use crate::engine::WinampEngine;
use crate::synth::{self, Parts};
use whipspot_skin_kit::bmp;
use whipspot_skin_kit::synth::{BmpSpec, ZEntry, ZipSpec};
use whipspot_skin_kit::zip::{Archive, MAX_ENTRIES, MAX_ENTRY_BYTES, MAX_INFLATED_TOTAL};

/// Tokens that steer text mutations toward the skin text formats' syntax and number edges.
const TEXT_DICT: &[&[u8]] = &[
    b"[Normal]",
    b"[Equalizer]",
    b"[Text]",
    b"NumPoints=",
    b"PointList=",
    b"Normal=",
    b"Current=",
    b"NormalBG=",
    b"SelectedBG=",
    b"Font=",
    b"#FFFFFF",
    b"#",
    b"=",
    b",",
    b"\r\n",
    b"\n",
    b"//",
    b"-1",
    b"0",
    b"4294967296",
    b"99999999999999999999",
    b"275,116",
    b"\xff\xfe",
];

const NAME_POOL: &[&[u8]] = &[
    b"main.bmp",
    b"MAIN.BMP",
    b"skin/main.bmp",
    b"skin\\Main.Bmp",
    b"../main.bmp",
    b"a/",
    b"",
    b"cbuttons.bmp",
    b"numbers.bmp",
    b"region.txt",
    b"pledit.txt",
    b"viscolor.txt",
    b"\xc3\xa9.bmp",
    b"x.bin",
    b"nul\0.bmp",
];

fn pick_name(rng: &mut SplitMix64) -> Vec<u8> {
    if rng.one_in(8) {
        // A name over the key cap, or one made of separators.
        return if rng.one_in(2) {
            vec![b'a'; 60 + rng.below(80)]
        } else {
            vec![b'/'; 1 + rng.below(5)]
        };
    }
    rng.pick(NAME_POOL).to_vec()
}

/// Reports how many inputs got past the parser and asserts a floor, so a harness that only ever
/// hits the early rejections cannot pass. Skipped for short runs.
fn assert_reach(name: &str, reached: u64, total: u64, min_percent: u64) {
    eprintln!("fuzz {name}: {reached} of {total} inputs parsed");
    if total >= 200 {
        assert!(
            reached * 100 >= total * min_percent,
            "{name}: only {reached} of {total} inputs got past the parser (floor {min_percent}%)"
        );
    }
}

fn edge_u16(rng: &mut SplitMix64) -> u16 {
    edge_u32(rng) as u16
}

// ---------------------------------------------------------------------------------------------
// Zip
// ---------------------------------------------------------------------------------------------

/// The corpus entries, compressed once: (stored, deflated) per file.
fn entry_corpus(parts: &Parts) -> Vec<(ZEntry, ZEntry)> {
    parts
        .files
        .iter()
        .map(|(n, d)| (ZEntry::stored(n, d), ZEntry::deflated(n, d)))
        .collect()
}

/// An archive spec from the corpus with random header fields and entries bent out of shape.
fn mutated_zip_spec(rng: &mut SplitMix64, corpus: &[(ZEntry, ZEntry)]) -> ZipSpec {
    let mut spec = ZipSpec {
        entries: corpus
            .iter()
            .map(|(stored, deflated)| {
                if rng.one_in(3) {
                    stored.clone()
                } else {
                    deflated.clone()
                }
            })
            .collect(),
        ..ZipSpec::default()
    };
    if rng.one_in(30) {
        // Over the entry cap: many tiny stored entries.
        let n = MAX_ENTRIES + 1 + rng.below(40);
        spec.entries = (0..n)
            .map(|i| ZEntry::stored(&format!("f{i}.bin"), b"x"))
            .collect();
    }
    for e in &mut spec.entries {
        if !rng.one_in(4) {
            continue;
        }
        match rng.below(9) {
            0 => e.size = edge_u32(rng),
            1 => e.csize = Some(edge_u32(rng)),
            2 => e.local_offset = Some(edge_u32(rng)),
            3 => e.method = *rng.pick(&[0u16, 1, 8, 9, 12, 14, 93, 99]),
            4 => e.flags = *rng.pick(&[0u16, 1, 8, 0x40, 0x41, 0x800]),
            5 => e.local_sig = edge_u32(rng),
            6 => e.disk_start = edge_u16(rng),
            7 => e.name = pick_name(rng),
            _ => {
                let at = rng.below(e.payload.len() + 1);
                e.payload.truncate(at);
            }
        }
    }
    if !spec.entries.is_empty() && rng.one_in(6) {
        // A duplicate key: the first entry in directory order must win.
        let i = rng.below(spec.entries.len());
        let copy = spec.entries[i].clone();
        spec.entries.push(copy);
    }
    if rng.one_in(5) {
        spec.count = Some(edge_u16(rng));
    }
    if rng.one_in(6) {
        spec.cd_offset = Some(edge_u32(rng));
    }
    if rng.one_in(6) {
        spec.cd_size = Some(edge_u32(rng));
    }
    if rng.one_in(10) {
        spec.disk = edge_u16(rng);
    }
    if rng.one_in(6) {
        let mut comment = vec![0u8; rng.below(200)];
        rng.fill(&mut comment);
        spec.comment = comment;
    }
    spec
}

/// Reads every key `keys` several times and checks the inflate caps over the whole archive.
fn check_reads(archive: &Archive, keys: &[String]) {
    let mut total = 0usize;
    for _ in 0..3 {
        for key in keys {
            let _ = archive.has(key);
            if let Some(Ok(bytes)) = archive.read(key) {
                assert!(bytes.len() <= MAX_ENTRY_BYTES, "entry over its cap");
                total += bytes.len();
                assert!(total <= MAX_INFLATED_TOTAL, "inflated total over its cap");
            }
        }
    }
}

fn corpus_keys(parts: &Parts) -> Vec<String> {
    let mut keys: Vec<String> = parts
        .files
        .iter()
        .map(|(n, _)| n.to_ascii_lowercase())
        .collect();
    keys.extend(["x.bin".to_owned(), "f0.bin".to_owned(), "a".repeat(70)]);
    keys
}

#[test]
fn fuzz_zip_archive_parse_and_read() {
    let parts = synth::parts();
    let keys = corpus_keys(&parts);
    let corpus = entry_corpus(&parts);
    let plain = synth::skin();
    let (mut reached, mut total) = (0u64, 0u64);
    fuzz("zip", 0x5A49_5001, 8000, |case: &mut Case| {
        total += 1;
        let rng = &mut case.rng;
        // `declared` is the entry count the end record says, known only while the bytes come
        // straight from a spec with no random comment bytes that could hold a second end record.
        let (mut bytes, declared) = if rng.one_in(5) {
            (plain.clone(), None)
        } else {
            let spec = mutated_zip_spec(rng, &corpus);
            let declared = spec
                .comment
                .is_empty()
                .then(|| usize::from(spec.count.unwrap_or(spec.entries.len() as u16)));
            (spec.bytes(), declared)
        };
        let byte_mutated = rng.below(3) != 0;
        if byte_mutated {
            mutate(
                &mut bytes,
                rng,
                &[b"PK\x03\x04", b"PK\x01\x02", b"PK\x05\x06"],
            );
        }
        case.set_input_len(bytes.len());
        if let Ok(archive) = Archive::parse(&bytes) {
            reached += 1;
            if let Some(count) = declared.filter(|_| !byte_mutated) {
                assert!(count <= MAX_ENTRIES, "parsed an archive over the entry cap");
            }
            check_reads(&archive, &keys);
        }
    });
    assert_reach("zip", reached, total, 10);
}

#[test]
fn fuzz_zip_truncated_at_every_length() {
    // Every prefix of a valid archive: no panic, and a prefix is never a larger archive.
    let bytes = whipspot_skin_kit::synth::zip_deflated(&[
        ("main.bmp", &[7u8; 300]),
        ("b.txt", b"hello world"),
    ]);
    for len in 0..=bytes.len() {
        if let Ok(a) = Archive::parse(&bytes[..len]) {
            let _ = a.read("main.bmp");
        }
    }
}

#[test]
fn inflated_total_cap_holds_over_repeated_reads() {
    // Three entries that each inflate to the per-entry cap: the shared total stops the reads.
    let big = vec![0u8; MAX_ENTRY_BYTES];
    let spec = ZipSpec {
        entries: ["a.bin", "b.bin", "c.bin"]
            .iter()
            .map(|n| ZEntry::deflated(n, &big))
            .collect(),
        ..ZipSpec::default()
    };
    let bytes = spec.bytes();
    let a = Archive::parse(&bytes).expect("parses");
    let mut total = 0usize;
    let mut refused = false;
    for _ in 0..4 {
        for key in ["a.bin", "b.bin", "c.bin"] {
            match a.read(key).expect("entry exists") {
                Ok(v) => {
                    total += v.len();
                    assert!(total <= MAX_INFLATED_TOTAL);
                }
                Err(_) => refused = true,
            }
        }
    }
    assert!(
        refused,
        "12 reads of 4 MiB pass 32 MiB, so some are refused"
    );
}

// ---------------------------------------------------------------------------------------------
// BMP
// ---------------------------------------------------------------------------------------------

/// A valid-looking bitmap with random size, depth and content (RLE8 included).
fn valid_bmp(rng: &mut SplitMix64) -> Vec<u8> {
    let w = 1 + rng.below(40);
    let h = 1 + rng.below(20);
    let top_down = rng.one_in(3);
    let tag = rng.below(256) as u8;
    match *rng.pick(&[1u16, 4, 8, 8, 24, 32, 0]) {
        0 => {
            // RLE8: encoded runs, absolute runs, escapes and a few random pairs.
            let mut rle = Vec::new();
            for _ in 0..(1 + rng.below(h * 2)) {
                match rng.below(5) {
                    0 => rle.extend_from_slice(&[0, 0]),
                    1 => rle.extend_from_slice(&[0, 2, rng.below(8) as u8, rng.below(4) as u8]),
                    2 => {
                        let n = 3 + rng.below(6);
                        rle.extend_from_slice(&[0, n as u8]);
                        rle.extend((0..n).map(|_| rng.below(256) as u8));
                        if n % 2 == 1 {
                            rle.push(0);
                        }
                    }
                    _ => rle.extend_from_slice(&[(1 + rng.below(12)) as u8, rng.below(256) as u8]),
                }
            }
            rle.extend_from_slice(&[0, 1]);
            let palette: Vec<[u8; 3]> = (0..1 + rng.below(256))
                .map(|i| synth::palette_at(i, tag))
                .collect();
            whipspot_skin_kit::synth::bmp_rle8(w as i32, h as i32, &palette, &rle)
        }
        bpp @ (24 | 32) => {
            let rows: Vec<Vec<[u8; 3]>> = (0..h)
                .map(|y| (0..w).map(|x| synth::px(x, y, tag)).collect())
                .collect();
            whipspot_skin_kit::synth::bmp_rgb(bpp, &rows, top_down)
        }
        bpp => {
            let max = 1usize << bpp;
            let palette: Vec<[u8; 3]> = (0..1 + rng.below(max))
                .map(|i| synth::palette_at(i, tag))
                .collect();
            let rows: Vec<Vec<u8>> = (0..h)
                .map(|y| {
                    (0..w)
                        .map(|x| synth::index_at(x, y, tag) & (max - 1) as u8)
                        .collect()
                })
                .collect();
            whipspot_skin_kit::synth::bmp_indexed(bpp, &palette, &rows, top_down)
        }
    }
}

/// A bitmap whose header fields are random edge values.
fn random_bmp_spec(rng: &mut SplitMix64) -> Vec<u8> {
    let dim = |rng: &mut SplitMix64| match rng.below(4) {
        0 => edge_u32(rng) as i32,
        1 => -(1 + rng.below(30) as i32),
        _ => 1 + rng.below(40) as i32,
    };
    let (w, h) = (dim(rng), dim(rng));
    let mut s = BmpSpec::new(w, h, *rng.pick(&[1u16, 4, 8, 16, 24, 32, 0, 64, 255]));
    s.planes = *rng.pick(&[1u16, 1, 1, 0, 2, 0xffff]);
    s.compression = *rng.pick(&[0u32, 0, 0, 1, 2, 3, 4, 5, 0xffff_ffff]);
    s.header_size = *rng.pick(&[40u32, 40, 52, 56, 108, 124, 12, 16, 64, 0, 200]);
    s.colours_used = rng.one_in(2).then(|| edge_u32(rng));
    s.palette = (0..rng.below(300)).map(|i| [i as u8, 1, 2]).collect();
    let mut data = vec![0u8; rng.below(700)];
    rng.fill(&mut data);
    if s.compression == 1 {
        // Bias an RLE stream toward escapes.
        for b in data.iter_mut().step_by(3) {
            *b = 0;
        }
    }
    s.data = data;
    s.data_offset = rng.one_in(3).then(|| edge_u32(rng));
    s.bytes()
}

/// Checks the caps; `true` when the bitmap decoded.
fn check_bmp(bytes: &[u8], budget: usize) -> bool {
    let dims = bmp::dimensions(bytes);
    if let Ok((w, h)) = dims {
        assert!(
            w <= MAX_ART_SIDE && h <= MAX_ART_SIDE,
            "header side over cap"
        );
    }
    if let Ok(img) = bmp::decode(bytes, budget) {
        let (w, h) = (img.width(), img.height());
        assert!(
            (1..=MAX_ART_SIDE).contains(&w) && (1..=MAX_ART_SIDE).contains(&h),
            "decoded side out of range"
        );
        assert_eq!(img.rgba().len() as u64, u64::from(w) * u64::from(h) * 4);
        assert!(img.rgba().len() <= budget, "decoded past the RGBA budget");
        assert_eq!(dims, Ok((w, h)), "dimensions and decode disagree");
        return true;
    }
    false
}

#[test]
fn fuzz_bmp_decode() {
    const BUDGETS: [usize; 7] = [0, 4, 1 << 10, 1 << 14, 1 << 16, 1 << 20, 1 << 22];
    let (mut reached, mut total) = (0u64, 0u64);
    fuzz("bmp", 0x424D_0001, 100_000, |case: &mut Case| {
        total += 1;
        let rng = &mut case.rng;
        let mut bytes = if rng.one_in(3) {
            random_bmp_spec(rng)
        } else {
            valid_bmp(rng)
        };
        if rng.below(4) != 0 {
            mutate(&mut bytes, rng, &[b"BM", &[0xff; 4], &[0, 0, 0, 0x80]]);
        }
        case.set_input_len(bytes.len());
        let budget = *case.rng.pick(&BUDGETS);
        reached += u64::from(check_bmp(&bytes, budget));
    });
    assert_reach("bmp", reached, total, 8);
}

#[test]
fn fuzz_bmp_valid_corpus_still_decodes_before_mutation() {
    // The generator really produces decodable files (otherwise the fuzz above would only test
    // the rejection paths).
    let mut rng = SplitMix64::new(11);
    let mut ok = 0;
    for _ in 0..200 {
        if bmp::decode(&valid_bmp(&mut rng), 1 << 22).is_ok() {
            ok += 1;
        }
    }
    assert!(ok > 120, "only {ok} of 200 generated bitmaps decode");
}

// ---------------------------------------------------------------------------------------------
// The engine
// ---------------------------------------------------------------------------------------------

#[test]
fn fuzz_engine_load_mutated_entries() {
    let base = synth::parts();
    let (mut reached, mut total) = (0u64, 0u64);
    fuzz("engine", 0x574E_0001, 400, |case: &mut Case| {
        total += 1;
        let rng = &mut case.rng;
        let mut parts: Parts = base.clone();
        for _ in 0..(1 + rng.below(2)) {
            let i = rng.below(parts.files.len());
            let (name, bytes) = &mut parts.files[i];
            let dict: &[&[u8]] = if name.ends_with(".txt") {
                TEXT_DICT
            } else {
                &[]
            };
            mutate(bytes, rng, dict);
        }
        if rng.one_in(5) {
            let victim = parts.files[rng.below(parts.files.len())].0.clone();
            parts = parts.remove(&victim);
        }
        let mut bytes = match rng.below(3) {
            0 => parts.zip(),
            1 => parts.zip_named(|n| format!("skin/{}", n.to_ascii_uppercase())),
            _ => parts.zip_named(|n| format!("a\\b\\{n}")),
        };
        if rng.one_in(3) {
            mutate(&mut bytes, rng, &[b"PK\x03\x04", b"PK\x01\x02"]);
        }
        case.set_input_len(bytes.len());
        let engine = WinampEngine;
        let _ = engine.detect(&bytes);
        if let Ok(skin) = engine.load(&bytes) {
            reached += 1;
            skin.validate().expect("a loaded skin validates");
        }
    });
    assert_reach("engine", reached, total, 20);
}

#[test]
fn fuzz_engine_text_files_with_dictionary_tokens() {
    // Only the three text files change, so every iteration reaches the text parsers.
    let base = synth::parts();
    let (mut reached, mut total) = (0u64, 0u64);
    fuzz("engine-text", 0x574E_0002, 400, |case: &mut Case| {
        total += 1;
        let rng = &mut case.rng;
        let mut parts = base.clone();
        for name in ["pledit.txt", "viscolor.txt", "region.txt"] {
            if rng.one_in(2) {
                let mut text = parts.get(name).to_vec();
                mutate(&mut text, rng, TEXT_DICT);
                parts = parts.set(name, text);
            }
        }
        let bytes = parts.zip();
        case.set_input_len(bytes.len());
        if let Ok(skin) = WinampEngine.load(&bytes) {
            reached += 1;
            skin.validate().expect("a loaded skin validates");
        }
    });
    assert_reach("engine-text", reached, total, 50);
}

//! Winamp-shaped synthetic skins built in code (hard rule 4: no real skin or asset is committed;
//! everything a test parses is generated here). The generic bitmap and archive builders live in
//! `whipspot_skin_kit::synth`.

use whipspot_skin_kit::synth::{BmpSpec, ZEntry, ZipSpec, bmp_indexed, bmp_rgb, bmp_rle8};

// ---------------------------------------------------------------------------------------------
// A whole synthetic skin
// ---------------------------------------------------------------------------------------------

/// The colour of pixel (x, y) of a generated sheet: distinct per sheet (`tag`) and per position,
/// so a test can tell which sheet and which part an image came from.
pub(crate) fn px(x: usize, y: usize, tag: u8) -> [u8; 3] {
    [
        tag.wrapping_add(x as u8),
        (y as u8).wrapping_mul(3),
        tag ^ ((x + y) as u8),
    ]
}

/// The palette index of pixel (x, y) in an indexed sheet. Runs of four equal pixels, so the same
/// sheet encodes as RLE.
pub(crate) fn index_at(x: usize, y: usize, tag: u8) -> u8 {
    ((x / 4 + 3 * y) as u8).wrapping_add(tag)
}

/// Palette entry `i` of an indexed sheet.
pub(crate) fn palette_at(i: usize, tag: u8) -> [u8; 3] {
    [i as u8, tag, 255 - i as u8]
}

fn palette256(tag: u8) -> Vec<[u8; 3]> {
    (0..256).map(|i| palette_at(i, tag)).collect()
}

fn index_rows(w: usize, h: usize, tag: u8) -> Vec<Vec<u8>> {
    (0..h)
        .map(|y| (0..w).map(|x| index_at(x, y, tag)).collect())
        .collect()
}

/// A 24-bit sheet of generated colours.
pub(crate) fn sheet24(w: usize, h: usize, tag: u8) -> Vec<u8> {
    let rows: Vec<Vec<[u8; 3]>> = (0..h)
        .map(|y| (0..w).map(|x| px(x, y, tag)).collect())
        .collect();
    bmp_rgb(24, &rows, false)
}

/// An 8-bit palettised sheet of generated indices.
pub(crate) fn sheet8(w: usize, h: usize, tag: u8) -> Vec<u8> {
    bmp_indexed(8, &palette256(tag), &index_rows(w, h, tag), false)
}

/// The same indices as an RLE8 sheet (runs of four, rows bottom-up).
pub(crate) fn sheet_rle8(w: usize, h: usize, tag: u8) -> Vec<u8> {
    let mut rle = Vec::new();
    for y in (0..h).rev() {
        let mut x = 0;
        while x < w {
            let n = (w - x).min(4);
            rle.extend_from_slice(&[n as u8, index_at(x, y, tag)]);
            x += n;
        }
        rle.extend_from_slice(&[0, 0]);
    }
    rle.extend_from_slice(&[0, 1]);
    bmp_rle8(w as i32, h as i32, &palette256(tag), &rle)
}

/// A one-bit sheet of zeros (the biggest pixel count for the fewest file bytes).
pub(crate) fn sheet1(w: usize, h: usize) -> Vec<u8> {
    let mut s = BmpSpec::new(w as i32, h as i32, 1);
    s.palette = vec![[0, 0, 0], [255, 255, 255]];
    s.data = vec![0; w.div_ceil(32) * 4 * h];
    s.bytes()
}

/// The files of a skin: entry name and bytes. [`parts`] has every sheet at its reference size and
/// the three text files; a test changes what it needs and writes the archive with [`Parts::zip`].
#[derive(Clone)]
pub(crate) struct Parts {
    pub files: Vec<(String, Vec<u8>)>,
}

impl Parts {
    /// Replaces the file `name`, or adds it.
    pub(crate) fn set(mut self, name: &str, bytes: Vec<u8>) -> Parts {
        match self.files.iter_mut().find(|(n, _)| n == name) {
            Some(f) => f.1 = bytes,
            None => self.files.push((name.to_owned(), bytes)),
        }
        self
    }

    pub(crate) fn remove(mut self, name: &str) -> Parts {
        self.files.retain(|(n, _)| n != name);
        self
    }

    pub(crate) fn get(&self, name: &str) -> &[u8] {
        &self.files.iter().find(|(n, _)| n == name).unwrap().1
    }

    /// A deflated archive with the entry names as they are.
    pub(crate) fn zip(&self) -> Vec<u8> {
        self.zip_named(|n| n.to_owned())
    }

    /// A deflated archive whose entry names are `rename(name)`.
    pub(crate) fn zip_named(&self, rename: impl Fn(&str) -> String) -> Vec<u8> {
        ZipSpec {
            entries: self
                .files
                .iter()
                .map(|(n, d)| ZEntry::deflated(&rename(n), d))
                .collect(),
            ..ZipSpec::default()
        }
        .bytes()
    }
}

/// Every bitmap at its reference size, in mixed formats, and the three text files.
pub(crate) fn parts() -> Parts {
    let visc: String = (0..24)
        .map(|i| format!("{},{},{}, // line {i}\r\n", i * 10, 255 - i * 10, i))
        .collect();
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("main.bmp", sheet24(275, 116, 1)),
        ("titlebar.bmp", sheet8(344, 87, 2)),
        ("cbuttons.bmp", sheet24(136, 36, 3)),
        ("shufrep.bmp", sheet8(92, 85, 4)),
        ("posbar.bmp", sheet_rle8(307, 10, 5)),
        ("volume.bmp", sheet8(68, 433, 6)),
        ("balance.bmp", sheet24(68, 433, 7)),
        ("monoster.bmp", sheet_rle8(56, 24, 8)),
        ("playpaus.bmp", sheet24(42, 9, 9)),
        ("numbers.bmp", sheet8(99, 13, 10)),
        ("nums_ex.bmp", sheet24(108, 13, 11)),
        ("text.bmp", sheet8(155, 18, 12)),
        ("eqmain.bmp", sheet24(275, 315, 13)),
        ("pledit.bmp", sheet8(280, 186, 14)),
        ("gen.bmp", sheet_rle8(194, 109, 15)),
        (
            "pledit.txt",
            b"[Text]\r\nNormal=#00FF00\r\nCurrent=#FFFFFF\r\nNormalBG=#000000\r\n\
              SelectedBG=#0000FF\r\nFont=Arial\r\n"
                .to_vec(),
        ),
        ("viscolor.txt", visc.into_bytes()),
        (
            "region.txt",
            b"[Normal]\r\nNumPoints=4,3\r\n\
              PointList=0,0,275,0,275,116,0,116,10,10,50,10,30,50\r\n\
              [Equalizer]\r\nNumPoints=4\r\nPointList=0,0,275,0,275,116,0,116\r\n"
                .to_vec(),
        ),
    ];
    Parts {
        files: files.into_iter().map(|(n, d)| (n.to_owned(), d)).collect(),
    }
}

/// The default synthetic skin as an archive.
pub(crate) fn skin() -> Vec<u8> {
    parts().zip()
}

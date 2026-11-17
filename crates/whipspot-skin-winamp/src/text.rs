//! Parsers for the small text files in a skin archive: an ini reader, hex colours, the 24-line
//! visualiser colour list and the window-shape region. All input is untrusted: sizes are capped
//! before anything is read, and a bad value gives `None`, never a panic.

use whipspot_core::skin::{MAX_POINTS, MAX_POLYGONS, Point, Rgb};

/// Larger text files are ignored.
pub(crate) const MAX_TEXT_BYTES: usize = 64 * 1024;

/// One ini section: its lowercase name and its `(key, value)` pairs (keys lowercase).
pub(crate) type Section = (String, Vec<(String, String)>);

/// Parses an ini file. Inputs over [`MAX_TEXT_BYTES`] give nothing. Lines starting with `;` or
/// `//` are comments; section and key names are case-insensitive (stored lowercase); a duplicate
/// key keeps its last value; lines before the first section header are ignored.
pub(crate) fn ini(bytes: &[u8]) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    if bytes.len() > MAX_TEXT_BYTES {
        return out;
    }
    let text = String::from_utf8_lossy(bytes);
    let mut current: Option<usize> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with("//") {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let Some((name, _)) = rest.split_once(']') else {
                continue;
            };
            let name = name.trim().to_lowercase();
            let idx = match out.iter().position(|(n, _)| *n == name) {
                Some(i) => i,
                None => {
                    out.push((name, Vec::new()));
                    out.len() - 1
                }
            };
            current = Some(idx);
            continue;
        }
        let (Some(idx), Some((key, value))) = (current, line.split_once('=')) else {
            continue;
        };
        let key = key.trim().to_lowercase();
        let value = value.trim().to_string();
        let pairs = &mut out[idx].1;
        match pairs.iter_mut().find(|(k, _)| *k == key) {
            Some(pair) => pair.1 = value,
            None => pairs.push((key, value)),
        }
    }
    out
}

/// `#RRGGBB` or `RRGGBB` (any case, surrounding whitespace ignored).
pub(crate) fn colour(s: &str) -> Option<Rgb> {
    let s = s.trim();
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let part = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    Some(Rgb {
        r: part(0)?,
        g: part(2)?,
        b: part(4)?,
    })
}

/// One line's leading three integers, each 0..=255, separated by spaces, tabs or commas.
fn colour_line(line: &str) -> Option<Rgb> {
    let mut rest = line.trim_start();
    let mut v = [0u8; 3];
    for (i, slot) in v.iter_mut().enumerate() {
        if i > 0 {
            rest = rest.trim_start_matches([' ', '\t', ',']);
        }
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || digits > 3 {
            return None;
        }
        *slot = u8::try_from(rest[..digits].parse::<u32>().ok()?).ok()?;
        rest = &rest[digits..];
    }
    Some(Rgb {
        r: v[0],
        g: v[1],
        b: v[2],
    })
}

/// The visualiser's 24 colours: the first 24 lines that start with three integers in 0..=255
/// (anything after them on the line is a comment). Fewer than 24 such lines give `None`.
pub(crate) fn viscolor(bytes: &[u8]) -> Option<[Rgb; 24]> {
    if bytes.len() > MAX_TEXT_BYTES {
        return None;
    }
    let text = String::from_utf8_lossy(bytes);
    let found: Vec<Rgb> = text.lines().filter_map(colour_line).take(24).collect();
    found.try_into().ok()
}

fn ints(s: &str) -> Option<Vec<i64>> {
    s.split([',', ' ', '\t'])
        .filter(|t| !t.is_empty())
        .map(|t| t.parse::<i64>().ok())
        .collect()
}

/// A window shape from a `NumPoints` / `PointList` pair in `values` (an ini section's pairs).
/// Every polygon has at least 3 points, the counts add up to the points listed, there are at most
/// `MAX_POLYGONS` polygons and `MAX_POINTS` points, and every point lies within `0..=w` by `0..=h`.
pub(crate) fn region(values: &[(String, String)], w: u32, h: u32) -> Option<Vec<Vec<Point>>> {
    let get = |k: &str| values.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
    let counts = ints(get("numpoints")?)?;
    let coords = ints(get("pointlist")?)?;
    if counts.is_empty() || counts.len() > MAX_POLYGONS {
        return None;
    }
    let mut total = 0usize;
    for c in &counts {
        let c = usize::try_from(*c).ok().filter(|c| *c >= 3)?;
        total = total.checked_add(c)?;
        if total > MAX_POINTS {
            return None;
        }
    }
    if coords.len() != total * 2 {
        return None;
    }
    let mut points = coords.chunks_exact(2).map(|p| {
        let x = u32::try_from(p[0]).ok().filter(|x| *x <= w)?;
        let y = u32::try_from(p[1]).ok().filter(|y| *y <= h)?;
        Some(Point { x, y })
    });
    let mut out = Vec::with_capacity(counts.len());
    for c in counts {
        let poly: Option<Vec<Point>> = points.by_ref().take(c as usize).collect();
        out.push(poly?);
    }
    Some(out)
}
#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn ini_table() {
        let src = b"; comment\r\n// another\r\n[Text]\r\nNormal = #00FF00\r\nBG=#000000\r\nnormal=#112233\r\n\r\n\
                    [ Other ]\r\nKey = a = b\r\n[TEXT]\r\nSelectedBG=#0000FF\r\nnot a pair\r\n[broken\r\nx=1\r\n";
        let got = ini(src);
        assert_eq!(
            got,
            vec![
                (
                    "text".to_string(),
                    pairs(&[
                        ("normal", "#112233"),
                        ("bg", "#000000"),
                        ("selectedbg", "#0000FF"),
                        // `[broken` is no header, so `x=1` stays in the section before it.
                        ("x", "1"),
                    ])
                ),
                ("other".to_string(), pairs(&[("key", "a = b")])),
            ]
        );
        // Keys before any section are dropped.
        assert_eq!(
            ini(b"a=1\n[s]\nb=2\n"),
            vec![("s".to_string(), pairs(&[("b", "2")]))]
        );
        assert!(ini(b"").is_empty());
        // Non-UTF-8 bytes do not panic.
        assert_eq!(ini(b"[s]\nk=\xff\xfe\n").len(), 1);
    }

    #[test]
    fn ini_over_the_cap_gives_nothing() {
        let mut big = b"[s]\nk=v\n".to_vec();
        big.resize(MAX_TEXT_BYTES + 1, b'\n');
        assert!(ini(&big).is_empty());
        big.truncate(MAX_TEXT_BYTES);
        assert_eq!(ini(&big).len(), 1);
    }

    #[test]
    fn colour_table() {
        assert_eq!(colour("#00FF7f"), Some(rgb(0, 255, 127)));
        assert_eq!(colour("0a0B0c"), Some(rgb(10, 11, 12)));
        assert_eq!(colour("  #FFFFFF  "), Some(rgb(255, 255, 255)));
        for bad in [
            "",
            "#",
            "#FFF",
            "#FFFFFFF",
            "GG0000",
            "#+1+1+1",
            "##000000",
            "00 000",
            "\u{e9}00000",
        ] {
            assert_eq!(colour(bad), None, "{bad:?}");
        }
    }

    fn vis_lines(n: usize) -> String {
        (0..n)
            .map(|i| format!("{},{},{}, // colour {i}\r\n", i, i * 2, 255 - i))
            .collect()
    }

    #[test]
    fn viscolor_table() {
        let got = viscolor(vis_lines(24).as_bytes()).expect("24 lines");
        assert_eq!(got[0], rgb(0, 0, 255));
        assert_eq!(got[23], rgb(23, 46, 232));
        // More than 24: the first 24 are used.
        assert_eq!(viscolor(vis_lines(30).as_bytes()), Some(got));
        assert_eq!(viscolor(vis_lines(23).as_bytes()), None);
        assert_eq!(viscolor(b""), None);
        // A value of 256 invalidates its line, so only 23 remain.
        let mut text = vis_lines(23);
        text.push_str("256,0,0\r\n");
        assert_eq!(viscolor(text.as_bytes()), None);
        // Comments and blank lines between colours are skipped; spaces separate too.
        let mut text = String::from("// header\n\n");
        for i in 0..24 {
            text.push_str(&format!("  {i} 1 2\t;x\n// between\n"));
        }
        assert_eq!(viscolor(text.as_bytes()).map(|v| v[5]), Some(rgb(5, 1, 2)));
        // Two numbers only, signs, over-long digits and over the size cap are not colours.
        for bad in ["1,2\n", "-1,2,3\n", "1,2,x\n", "0001,2,3\n", "1.5,2,3\n"] {
            assert_eq!(colour_line(bad), None, "{bad:?}");
        }
        let mut big = vis_lines(24).into_bytes();
        big.resize(MAX_TEXT_BYTES + 1, b'\n');
        assert_eq!(viscolor(&big), None);
    }

    fn region_values(counts: &str, points: &str) -> Vec<(String, String)> {
        pairs(&[("numpoints", counts), ("pointlist", points)])
    }

    #[test]
    fn region_table() {
        let two = region_values("4, 3", "0,0, 10,0, 10,5, 0,5, 1,1, 8,1, 8,4");
        let got = region(&two, 10, 5).expect("valid");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].len(), 4);
        assert_eq!(got[0][2], Point { x: 10, y: 5 });
        assert_eq!(got[1][0], Point { x: 1, y: 1 });
        // Trailing commas are fine.
        assert!(region(&region_values("3,", "0,0,5,0,5,5,"), 10, 10).is_some());

        let bad: &[(&str, &str, u32, u32)] = &[
            ("4,3", "0,0,10,0,10,5,0,5", 10, 5), // fewer points than counts
            ("3", "0,0,10,0,10,5,0,5", 10, 5),   // more points than counts
            ("2", "0,0,10,0", 10, 5),            // count 2
            ("3", "0,0,10,0,11,5", 10, 5),       // x out of bounds
            ("3", "0,0,10,0,10,6", 10, 5),       // y out of bounds
            ("3", "0,0,10,0,-1,5", 10, 5),       // negative
            ("-3", "0,0,10,0,10,5", 10, 5),      // negative count
            ("3", "0,0,10,0,10", 10, 5),         // odd number of values
            ("3", "0,0,10,0,10,x", 10, 5),       // not a number
            ("", "", 10, 5),                     // nothing
            ("3,3", "0,0,10,0,10,5", 10, 5),     // counts exceed the list
            ("99999999999999999999", "0,0,0,0,0,0", 10, 5), // overflowing count
        ];
        for (counts, points, w, h) in bad {
            assert_eq!(
                region(&region_values(counts, points), *w, *h),
                None,
                "{counts:?} {points:?}"
            );
        }
        // Missing keys.
        assert_eq!(region(&pairs(&[("numpoints", "3")]), 10, 10), None);
        assert_eq!(
            region(&pairs(&[("pointlist", "0,0,1,1,2,2")]), 10, 10),
            None
        );
    }

    #[test]
    fn region_caps() {
        // 129 triangles: over the polygon cap.
        let counts = vec!["3"; MAX_POLYGONS + 1].join(",");
        let points = vec!["0,0,1,1,2,2"; MAX_POLYGONS + 1].join(",");
        assert_eq!(region(&region_values(&counts, &points), 10, 10), None);
        let counts = vec!["3"; MAX_POLYGONS].join(",");
        let points = vec!["0,0,1,1,2,2"; MAX_POLYGONS].join(",");
        assert_eq!(
            region(&region_values(&counts, &points), 10, 10).map(|r| r.len()),
            Some(MAX_POLYGONS)
        );
        // One polygon over the point cap.
        let n = MAX_POINTS + 1;
        let points = vec!["1,1"; n].join(",");
        assert_eq!(
            region(&region_values(&n.to_string(), &points), 10, 10),
            None
        );
        let n = MAX_POINTS;
        let points = vec!["1,1"; n].join(",");
        assert_eq!(
            region(&region_values(&n.to_string(), &points), 10, 10).map(|r| r[0].len()),
            Some(MAX_POINTS)
        );
    }
}

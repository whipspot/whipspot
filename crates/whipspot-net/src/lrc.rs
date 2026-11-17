//! LRC and plain lyrics parsing (MD67, MD68). The input is untrusted text of at most the lyrics
//! body cap; the work is bounded by line counts as well as bytes, and the result goes through
//! [`Lyrics::sanitized`], the core's rules.

use whipspot_core::lyrics::{LyricLine, Lyrics, MAX_LYRIC_CHARS, MAX_LYRIC_LINES};

/// The most input lines looked at.
const MAX_INPUT_LINES: usize = 5_000;
/// The most lines collected before the core's sanitising keeps [`MAX_LYRIC_LINES`].
const MAX_COLLECTED: usize = 4 * MAX_LYRIC_LINES;
/// A line's text is cut to this many bytes while parsing (the core then cuts to characters).
const MAX_TEXT_BYTES: usize = 4 * MAX_LYRIC_CHARS;

/// `s` cut to at most `max` bytes on a char boundary.
fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Up to `max` ASCII digits at the start of `s`: (value, digit count, rest).
fn digits(s: &str, max: usize) -> Option<(u64, usize, &str)> {
    let n = s.bytes().take(max).take_while(u8::is_ascii_digit).count();
    if n == 0 {
        return None;
    }
    let value = s[..n]
        .bytes()
        .fold(0u64, |a, b| a * 10 + u64::from(b - b'0'));
    Some((value, n, &s[n..]))
}

/// A time `mm:ss` or `mm:ss.f..fff` at the start of `s`: (milliseconds, rest). `mm` is 1 to 3
/// digits, `ss` exactly 2 digits in 0..=59, the fraction 1 to 3 digits.
fn time(s: &str) -> Option<(u64, &str)> {
    let (mm, _, rest) = digits(s, 3)?;
    let rest = rest.strip_prefix(':')?;
    let (ss, n, rest) = digits(rest, 3)?;
    if n != 2 || ss > 59 {
        return None;
    }
    let (frac_ms, rest) = match rest.strip_prefix('.') {
        Some(r) => {
            let (f, n, rest) = digits(r, 4)?;
            if n > 3 {
                return None;
            }
            (f * 10u64.pow(3 - n as u32), rest)
        }
        None => (0, rest),
    };
    Some(((mm * 60 + ss) * 1000 + frac_ms, rest))
}

/// `[time]` at the start of `s`: (milliseconds, rest).
fn tag(s: &str) -> Option<(u64, &str)> {
    let inner = s.strip_prefix('[')?;
    let (ms, rest) = time(inner)?;
    Some((ms, rest.strip_prefix(']')?))
}

/// `s` with inline word tags `<mm:ss.ff>` removed.
fn strip_word_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match time(after).and_then(|(_, r)| r.strip_prefix('>')) {
            Some(r) => rest = r,
            None => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Timed lines of an LRC text: each leading `[mm:ss.xx]` tag of a line gives one line; a line
/// whose first bracket is not a time (metadata such as `[ar:x]`) is skipped. At most 5,000 input
/// lines are looked at; the result is sanitised (sorted by time, at most 1,000 lines).
pub(crate) fn parse_synced(text: &str) -> Vec<LyricLine> {
    let mut out: Vec<LyricLine> = Vec::new();
    'lines: for line in text.lines().take(MAX_INPUT_LINES) {
        let mut rest = line.trim_start();
        let mut times: Vec<u64> = Vec::new();
        while let Some((ms, r)) = tag(rest) {
            times.push(ms);
            rest = r;
            if times.len() >= MAX_COLLECTED {
                break;
            }
        }
        if times.is_empty() {
            continue;
        }
        let body = strip_word_tags(rest);
        let body = cut(&body, MAX_TEXT_BYTES);
        for at_ms in times {
            if out.len() >= MAX_COLLECTED {
                break 'lines;
            }
            out.push(LyricLine {
                at_ms,
                text: body.to_owned(),
            });
        }
    }
    match Lyrics::Synced(out).sanitized() {
        Lyrics::Synced(lines) => lines,
        _ => Vec::new(),
    }
}

/// Plain lyric lines: at most 5,000 input lines looked at, sanitised (at most 1,000 lines), with
/// leading and trailing blank lines dropped.
pub(crate) fn parse_plain(text: &str) -> Vec<String> {
    let lines: Vec<String> = text
        .lines()
        .take(MAX_INPUT_LINES)
        .take(MAX_COLLECTED)
        .map(|l| cut(l, MAX_TEXT_BYTES).to_owned())
        .collect();
    let Lyrics::Plain(mut lines) = Lyrics::Plain(lines).sanitized() else {
        return Vec::new();
    };
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    let lead = lines.iter().take_while(|l| l.is_empty()).count();
    lines.drain(..lead);
    lines
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn one(text: &str) -> Vec<(u64, String)> {
        parse_synced(text)
            .into_iter()
            .map(|l| (l.at_ms, l.text))
            .collect()
    }

    #[test]
    fn times_in_every_form() {
        assert_eq!(one("[00:12.34]hi"), vec![(12_340, "hi".into())]);
        assert_eq!(one("[01:02]x"), vec![(62_000, "x".into())]);
        assert_eq!(one("[00:01.5]a"), vec![(1_500, "a".into())]);
        assert_eq!(one("[00:01.123]a"), vec![(1_123, "a".into())]);
        assert_eq!(
            one("[999:59.999]a"),
            vec![(999 * 60_000 + 59_999, "a".into())]
        );
        assert_eq!(one("[00:12.34] spaced"), vec![(12_340, "spaced".into())]);
    }

    #[test]
    fn several_tags_give_several_lines() {
        assert_eq!(
            one("[00:05.00][00:01.00]c"),
            vec![(1_000, "c".into()), (5_000, "c".into())]
        );
    }

    #[test]
    fn metadata_and_malformed_times_are_skipped() {
        for bad in [
            "[ar:x]",
            "[offset:+100]",
            "[aa:bb]x",
            "[00:61.00]x",
            "[00:1]x",
            "[-1:00]x",
            "[1000:00]x",
            "[00:00.0000]x",
            "[x",
            "]",
            "[",
            "no tag",
            "",
        ] {
            assert!(one(bad).is_empty(), "{bad:?}");
        }
    }

    #[test]
    fn word_tags_are_removed_and_other_angles_stay() {
        assert_eq!(
            one("[00:01.00]<00:01.00>he<00:01.50>llo <b>x"),
            vec![(1_000, "hello <b>x".into())]
        );
    }

    #[test]
    fn unsorted_input_is_sorted_and_crlf_is_fine() {
        assert_eq!(
            one("[00:03.00]c\r\n[00:01.00]a\r\n[00:02.00]b\r\n"),
            vec![
                (1_000, "a".into()),
                (2_000, "b".into()),
                (3_000, "c".into())
            ]
        );
    }

    #[test]
    fn a_tag_only_line_keeps_an_empty_text() {
        assert_eq!(one("[00:09.00]"), vec![(9_000, String::new())]);
    }

    #[test]
    fn huge_input_is_bounded_in_work_and_output() {
        let text: String = (0..100_000)
            .map(|i| format!("[{:02}:{:02}.00]line {i}\n", (i / 60) % 100, i % 60))
            .collect();
        let started = Instant::now();
        let lines = parse_synced(&text);
        assert!(started.elapsed().as_secs() < 1);
        assert!(lines.len() <= MAX_LYRIC_LINES);
        assert!(!lines.is_empty());
        let started = Instant::now();
        assert!(parse_plain(&text).len() <= MAX_LYRIC_LINES);
        assert!(started.elapsed().as_secs() < 1);
        // Tags without end on one line.
        let tags = "[00:01.00]".repeat(50_000);
        assert!(parse_synced(&tags).len() <= MAX_LYRIC_LINES);
    }

    #[test]
    fn long_lines_are_cut_on_a_char_boundary() {
        let big = format!("[00:01.00]{}", "x".repeat(1_000_000));
        assert_eq!(parse_synced(&big)[0].text.chars().count(), MAX_LYRIC_CHARS);
        let multi = format!("[00:01.00]{}", "é".repeat(5_000));
        let l = &parse_synced(&multi)[0].text;
        assert_eq!(l.chars().count(), MAX_LYRIC_CHARS);
        assert!(l.chars().all(|c| c == 'é'));
        let plain = parse_plain(&"日".repeat(5_000));
        assert_eq!(plain[0].chars().count(), MAX_LYRIC_CHARS);
    }

    #[test]
    fn control_and_bidi_characters_do_not_survive() {
        let l = parse_synced("[00:01.00]a\u{202E}b\tc\u{200B}d");
        assert_eq!(l[0].text, "ab cd");
    }

    #[test]
    fn plain_keeps_inner_blanks_and_drops_the_edges() {
        assert_eq!(
            parse_plain("\n\nfirst\n\nsecond\r\n\n"),
            vec!["first".to_owned(), String::new(), "second".to_owned()]
        );
        assert!(parse_plain("  \n\t\n").is_empty());
        assert!(parse_plain("").is_empty());
    }

    #[test]
    fn no_input_panics() {
        for s in [
            "[", "[0", "[00:", "[00:0", "[00:00.", "<", "<00:", "[00:00]<", "\u{0}",
        ] {
            let _ = parse_synced(s);
            let _ = parse_plain(s);
            let _ = strip_word_tags(s);
        }
        let _ = parse_synced(&"[".repeat(100_000));
        let _ = parse_synced(&"<".repeat(100_000));
    }
}

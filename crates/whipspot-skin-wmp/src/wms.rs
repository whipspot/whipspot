//! The `.wms` reader (D11): our own lenient, bounded tokenizer. It builds an element tree and
//! nothing else: text content is ignored and the bodies of script elements are skipped, never
//! read as code. The input is untrusted (hard rule 5): every cap fails closed, the work is linear
//! in the input, and nothing here can panic.

/// Most input bytes [`decode_text`] accepts.
pub const MAX_WMS_BYTES: usize = 2 * 1024 * 1024;
/// Most elements a description may hold.
pub const MAX_ELEMENTS: usize = 4096;
/// Deepest element nesting.
pub const MAX_DEPTH: usize = 64;
/// Most attributes kept per element; later ones are dropped.
pub const MAX_ATTRS: usize = 64;
/// Longest attribute name kept, in bytes; a longer one drops the attribute.
pub const MAX_ATTR_NAME: usize = 64;
/// Longest attribute value kept, in characters; a longer one drops the attribute.
pub const MAX_ATTR_VALUE: usize = 8192;

/// Why a description was not read. No variant carries input text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WmsError {
    /// There is no root element.
    Malformed,
    /// A cap was exceeded.
    TooLarge,
}

/// One element. Names are stored in lowercase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
}

impl Element {
    /// The first attribute with this (lowercase) name.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// The elements of one description in document order, and the index of the root (the first
/// element whose tag is `theme`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    pub elements: Vec<Element>,
    pub root: usize,
}

impl Tree {
    pub fn get(&self, index: usize) -> Option<&Element> {
        self.elements.get(index)
    }
}

/// The text of a description: UTF-16LE with a `FF FE` mark, UTF-16BE with `FE FF`, or UTF-8 with
/// or without its mark. Unpaired surrogates, invalid UTF-8 and a dangling byte become U+FFFD.
/// `None` above [`MAX_WMS_BYTES`].
pub fn decode_text(bytes: &[u8]) -> Option<String> {
    if bytes.len() > MAX_WMS_BYTES {
        return None;
    }
    let utf16 = |body: &[u8], unit: fn([u8; 2]) -> u16| -> String {
        let units = body.chunks_exact(2).map(|c| unit([c[0], c[1]]));
        let mut s: String = char::decode_utf16(units)
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
        if body.len() % 2 == 1 {
            s.push(char::REPLACEMENT_CHARACTER);
        }
        s
    };
    Some(match bytes {
        [0xff, 0xfe, body @ ..] => utf16(body, u16::from_le_bytes),
        [0xfe, 0xff, body @ ..] => utf16(body, u16::from_be_bytes),
        [0xef, 0xbb, 0xbf, body @ ..] => String::from_utf8_lossy(body).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    })
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Like [`find`], with `needle` in lowercase and `hay` compared ASCII-case-insensitively.
fn find_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0c)
}

/// Decodes the five XML entities; anything else (a bare `&` too) stays as written.
fn unescape(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_owned();
    }
    const ENTITIES: [(&str, char); 5] = [
        ("&amp;", '&'),
        ("&lt;", '<'),
        ("&gt;", '>'),
        ("&quot;", '"'),
        ("&apos;", '\''),
    ];
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        match ENTITIES.iter().find(|(e, _)| rest.starts_with(e)) {
            Some((e, c)) => {
                out.push(*c);
                rest = &rest[e.len()..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Reads the description. See the module comment for what is kept.
pub fn parse(text: &str) -> Result<Tree, WmsError> {
    let b = text.as_bytes();
    let n = b.len();
    let mut elements: Vec<Element> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut i = 0;

    // Slicing `text` at `from..to` is safe where both ends sit on ASCII bytes or the end; every
    // scan below stops only at ASCII delimiters.
    let slice = |from: usize, to: usize| text.get(from..to).unwrap_or("");

    while i < n {
        let Some(off) = b[i..].iter().position(|c| *c == b'<') else {
            break;
        };
        i += off;
        let rest = &b[i..];
        if rest.starts_with(b"<!--") {
            i = find(&rest[4..], b"-->").map_or(n, |p| i + 4 + p + 3);
        } else if rest.starts_with(b"<![CDATA[") {
            i = find(&rest[9..], b"]]>").map_or(n, |p| i + 9 + p + 3);
        } else if rest.starts_with(b"<?") {
            i = find(&rest[2..], b"?>").map_or(n, |p| i + 2 + p + 2);
        } else if rest.starts_with(b"<!") {
            i = rest
                .iter()
                .position(|c| *c == b'>')
                .map_or(n, |p| i + p + 1);
        } else if rest.starts_with(b"</") {
            let end = rest.iter().position(|c| *c == b'>').map_or(n, |p| i + p);
            let name = slice(i + 2, end).trim().to_ascii_lowercase();
            if let Some(at) = stack.iter().rposition(|e| elements[*e].tag == name) {
                stack.truncate(at);
            }
            i = (end + 1).min(n);
        } else if rest.get(1).is_some_and(|c| !is_space(*c) && *c != b'>') {
            // An opening tag.
            let mut p = i + 1;
            let name_start = p;
            while p < n && !is_space(b[p]) && b[p] != b'/' && b[p] != b'>' {
                p += 1;
            }
            let tag = slice(name_start, p).to_ascii_lowercase();
            let mut attrs: Vec<(String, String)> = Vec::new();
            let mut self_closed = false;
            loop {
                while p < n && is_space(b[p]) {
                    p += 1;
                }
                if p >= n {
                    break;
                }
                match b[p] {
                    b'>' => {
                        p += 1;
                        break;
                    }
                    b'/' => {
                        if b.get(p + 1) == Some(&b'>') {
                            self_closed = true;
                            p += 2;
                            break;
                        }
                        p += 1;
                        continue;
                    }
                    _ => {}
                }
                let an = p;
                while p < n && !is_space(b[p]) && !matches!(b[p], b'=' | b'>' | b'/') {
                    p += 1;
                }
                let name = slice(an, p);
                if p == an {
                    // A stray `=`: step over it so the loop always advances.
                    p += 1;
                    continue;
                }
                let mut value = None;
                let mut q = p;
                while q < n && is_space(b[q]) {
                    q += 1;
                }
                if q < n && b[q] == b'=' {
                    q += 1;
                    while q < n && is_space(b[q]) {
                        q += 1;
                    }
                    let (vs, ve);
                    if q < n && (b[q] == b'"' || b[q] == b'\'') {
                        let quote = b[q];
                        vs = q + 1;
                        ve = b[vs..]
                            .iter()
                            .position(|c| *c == quote)
                            .map_or(n, |x| vs + x);
                        q = (ve + 1).min(n);
                    } else {
                        vs = q;
                        while q < n && !is_space(b[q]) && b[q] != b'>' {
                            q += 1;
                        }
                        ve = q;
                    }
                    value = Some(slice(vs, ve));
                    p = q;
                }
                if attrs.len() < MAX_ATTRS && name.len() <= MAX_ATTR_NAME {
                    let v = unescape(value.unwrap_or(""));
                    if v.chars().count() <= MAX_ATTR_VALUE {
                        attrs.push((name.to_ascii_lowercase(), v));
                    }
                }
            }
            i = p;
            if elements.len() >= MAX_ELEMENTS || stack.len() >= MAX_DEPTH {
                return Err(WmsError::TooLarge);
            }
            let index = elements.len();
            let parent = stack.last().copied();
            if let Some(par) = parent {
                elements[par].children.push(index);
            }
            let is_script = tag == "script";
            elements.push(Element {
                tag,
                attrs,
                parent,
                children: Vec::new(),
            });
            if is_script && !self_closed {
                // The body is code: skipped up to the closing tag, never read.
                i = find_ci(&b[i..], b"</script")
                    .and_then(|x| {
                        b[i + x..]
                            .iter()
                            .position(|c| *c == b'>')
                            .map(|y| i + x + y + 1)
                    })
                    .unwrap_or(n);
            } else if !self_closed {
                stack.push(index);
            }
        } else {
            i += 1;
        }
    }
    let root = elements
        .iter()
        .position(|e| e.tag == "theme")
        .ok_or(WmsError::Malformed)?;
    Ok(Tree { elements, root })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le(s: &str) -> Vec<u8> {
        let mut v = vec![0xff, 0xfe];
        for u in s.encode_utf16() {
            v.extend_from_slice(&u.to_le_bytes());
        }
        v
    }

    fn utf16be(s: &str) -> Vec<u8> {
        let mut v = vec![0xfe, 0xff];
        for u in s.encode_utf16() {
            v.extend_from_slice(&u.to_be_bytes());
        }
        v
    }

    #[test]
    fn decode_text_handles_every_encoding() {
        let s = "<theme a='é😀'/>";
        assert_eq!(decode_text(&utf16le(s)).as_deref(), Some(s));
        assert_eq!(decode_text(&utf16be(s)).as_deref(), Some(s));
        let mut u8bom = vec![0xef, 0xbb, 0xbf];
        u8bom.extend_from_slice(s.as_bytes());
        assert_eq!(decode_text(&u8bom).as_deref(), Some(s));
        assert_eq!(decode_text(s.as_bytes()).as_deref(), Some(s));
        assert_eq!(decode_text(b"plain ascii").as_deref(), Some("plain ascii"));
        assert_eq!(decode_text(&[]).as_deref(), Some(""));
    }

    #[test]
    fn decode_text_repairs_what_it_cannot_read() {
        // An odd byte count: the dangling byte is U+FFFD.
        assert_eq!(
            decode_text(&[0xff, 0xfe, 0x41, 0x00, 0x42]).as_deref(),
            Some("A\u{fffd}")
        );
        // An unpaired surrogate.
        assert_eq!(
            decode_text(&[0xff, 0xfe, 0x00, 0xd8, 0x41, 0x00]).as_deref(),
            Some("\u{fffd}A")
        );
        // Invalid UTF-8.
        assert_eq!(
            decode_text(&[b'a', 0xff, b'b']).as_deref(),
            Some("a\u{fffd}b")
        );
    }

    #[test]
    fn decode_text_refuses_over_the_cap() {
        assert!(decode_text(&vec![b'a'; MAX_WMS_BYTES]).is_some());
        assert!(decode_text(&vec![b'a'; MAX_WMS_BYTES + 1]).is_none());
    }

    fn tags(t: &Tree) -> Vec<&str> {
        t.elements.iter().map(|e| e.tag.as_str()).collect()
    }

    #[test]
    fn nested_elements_keep_document_order_and_parents() {
        let t = parse(
            "<THEME name=\"x\"><View id=v><SUBVIEW id=s><BUTTON id=b/></SUBVIEW></View></THEME>",
        )
        .unwrap();
        assert_eq!(tags(&t), ["theme", "view", "subview", "button"]);
        assert_eq!(t.root, 0);
        assert_eq!(t.elements[3].parent, Some(2));
        assert_eq!(t.elements[1].children, [2]);
        assert_eq!(t.elements[1].attr("id"), Some("v"));
        assert_eq!(t.elements[0].attr("name"), Some("x"));
    }

    #[test]
    fn quoting_entities_and_case() {
        let t = parse(
            "<theme><button ID='a b' onClick=\"x &amp; y &lt; &quot;z&quot; &apos;\" bare=val other=1 /></theme>",
        )
        .unwrap();
        let b = &t.elements[1];
        assert_eq!(b.attr("id"), Some("a b"));
        assert_eq!(b.attr("onclick"), Some("x & y < \"z\" '"));
        assert_eq!(b.attr("bare"), Some("val"));
        assert_eq!(b.attr("other"), Some("1"));
    }

    #[test]
    fn a_bare_ampersand_and_unknown_entities_stay() {
        let t = parse("<theme><button onclick=\"a && b; x=1&2 &nbsp; &amp\"/></theme>").unwrap();
        assert_eq!(
            t.elements[1].attr("onclick"),
            Some("a && b; x=1&2 &nbsp; &amp")
        );
    }

    #[test]
    fn comments_directives_and_script_bodies_are_skipped() {
        let t = parse(
            "<?xml version='1.0'?><!DOCTYPE x><!-- <view> --><theme><![CDATA[ <view> ]]>\
             <script language=\"JScript\">if (a<b && c>d) { x = '</view>'; }</SCRIPT>\
             <button id=after/></theme>",
        )
        .unwrap();
        assert_eq!(tags(&t), ["theme", "script", "button"]);
        // The script element is closed: the button is the theme's child, not the script's.
        assert_eq!(t.elements[2].parent, Some(0));
    }

    #[test]
    fn close_tags_pop_to_the_nearest_match_and_stray_ones_are_ignored() {
        let t = parse("<theme><view><subview></nope></view><button/></stray></theme>").unwrap();
        assert_eq!(tags(&t), ["theme", "view", "subview", "button"]);
        // `</view>` closed the subview too, so the button is a child of the theme.
        assert_eq!(t.elements[3].parent, Some(0));
    }

    #[test]
    fn no_theme_is_malformed() {
        assert_eq!(parse("<view/>"), Err(WmsError::Malformed));
        assert_eq!(parse(""), Err(WmsError::Malformed));
        assert_eq!(parse("not markup at all < > &"), Err(WmsError::Malformed));
    }

    #[test]
    fn the_first_theme_is_the_root() {
        let t = parse("<view/><theme id=a><theme id=b/></theme>").unwrap();
        assert_eq!(t.root, 1);
    }

    #[test]
    fn element_cap_fails_closed() {
        let ok = format!("<theme>{}</theme>", "<a/>".repeat(MAX_ELEMENTS - 1));
        assert_eq!(parse(&ok).unwrap().elements.len(), MAX_ELEMENTS);
        let over = format!("<theme>{}</theme>", "<a/>".repeat(MAX_ELEMENTS));
        assert_eq!(parse(&over), Err(WmsError::TooLarge));
    }

    #[test]
    fn depth_cap_fails_closed() {
        let ok = format!("<theme>{}", "<a>".repeat(MAX_DEPTH - 1));
        assert!(parse(&ok).is_ok());
        let over = format!("<theme>{}", "<a>".repeat(MAX_DEPTH));
        assert_eq!(parse(&over), Err(WmsError::TooLarge));
    }

    #[test]
    fn attribute_caps_drop_the_attribute() {
        let long = "x".repeat(MAX_ATTR_VALUE + 1);
        let edge = "y".repeat(MAX_ATTR_VALUE);
        let long_name = "n".repeat(MAX_ATTR_NAME + 1);
        let many: String = (0..MAX_ATTRS + 5).map(|i| format!(" a{i}=1")).collect();
        let t = parse(&format!(
            "<theme onclick=\"{long}\" geometry=\"{edge}\" {long_name}=1 keep=1{many}/>"
        ))
        .unwrap();
        let e = &t.elements[0];
        assert_eq!(e.attr("onclick"), None);
        assert_eq!(e.attr("geometry").map(str::len), Some(MAX_ATTR_VALUE));
        assert_eq!(e.attr(&long_name.to_ascii_lowercase()), None);
        assert_eq!(e.attr("keep"), Some("1"));
        assert_eq!(e.attrs.len(), MAX_ATTRS);
    }

    #[test]
    fn hostile_text_terminates() {
        for s in [
            "<theme",
            "<theme a=",
            "<theme a='unterminated",
            "<theme><!--",
            "<theme><![CDATA[",
            "<theme><script>",
            "<theme></",
            "<theme>< <<< =>",
            "<theme = = = />",
            "<<<<<<<<<",
        ] {
            let _ = parse(s);
        }
    }
}

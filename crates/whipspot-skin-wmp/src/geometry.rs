//! The geometry resolver (D5): an element's `left`, `top`, `width`, `height` and `zindex` as
//! integers, from a literal or from a small bounded arithmetic over other elements' values. No
//! script is run: a value that needs anything but integer literals, `+ - * /`, unary minus,
//! parentheses and the references below is simply unresolved (`None`).
//!
//! References: `view.<p>` (the enclosing VIEW), `<id>.<p>` (an element by id; ids compare
//! case-insensitively and the first element with an id wins) and the element's own bare `left`,
//! `top`, `width` and `height`, where `<p>` is one of those four. A `jscript:` or `wmpprop:`
//! prefix and a trailing `;` are stripped first.
//!
//! The expression is untrusted text: it is bounded in length, tokens, parenthesis depth and
//! reference depth, every operation is checked, and a cycle is unresolved.

use std::collections::HashMap;

use crate::wms::Tree;

/// Longest expression read, in characters.
pub const MAX_EXPR_CHARS: usize = 256;
/// Most tokens in one expression.
pub const MAX_TOKENS: usize = 128;
/// Deepest parenthesis nesting.
pub const MAX_PAREN_DEPTH: u32 = 16;
/// Deepest chain of references followed.
pub const MAX_REF_DEPTH: u32 = 32;
/// The range a resolved value must lie in.
pub const VALUE_RANGE: std::ops::RangeInclusive<i64> = -65536..=65536;

/// A property the resolver reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Prop {
    Left,
    Top,
    Width,
    Height,
    /// Not read by the composer, which sorts by the literal `zIndex`; kept for the resolver's API.
    #[allow(dead_code)]
    ZIndex,
}

impl Prop {
    fn attr(self) -> &'static str {
        match self {
            Prop::Left => "left",
            Prop::Top => "top",
            Prop::Width => "width",
            Prop::Height => "height",
            Prop::ZIndex => "zindex",
        }
    }

    /// The four geometry properties an expression may name.
    fn from_name(name: &str) -> Option<Prop> {
        match name.to_ascii_lowercase().as_str() {
            "left" => Some(Prop::Left),
            "top" => Some(Prop::Top),
            "width" => Some(Prop::Width),
            "height" => Some(Prop::Height),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fail {
    /// The value cannot be resolved (and never will).
    Unresolved,
    /// The reference chain was too deep for this entry point; not remembered.
    TooDeep,
}

type Res = Result<i64, Fail>;

enum Memo {
    Pending,
    Done(Option<i64>),
}

/// Resolves properties of one [`Tree`], remembering each (element, property) it has resolved.
pub struct Resolver<'a> {
    tree: &'a Tree,
    ids: HashMap<String, usize>,
    memo: HashMap<(usize, Prop), Memo>,
}

impl<'a> Resolver<'a> {
    pub fn new(tree: &'a Tree) -> Resolver<'a> {
        let mut ids = HashMap::new();
        for (i, e) in tree.elements.iter().enumerate() {
            if let Some(id) = e.attr("id") {
                ids.entry(id.to_ascii_lowercase()).or_insert(i);
            }
        }
        Resolver {
            tree,
            ids,
            memo: HashMap::new(),
        }
    }

    /// The value of `prop` on `element`. A missing `left`, `top` or `zindex` is 0; a missing
    /// `width` or `height`, an unreadable value and a result outside [`VALUE_RANGE`] are `None`.
    pub fn value(&mut self, element: usize, prop: Prop) -> Option<i64> {
        self.prop(element, prop, 0).ok()
    }

    /// The nearest element at or above `element` whose tag is `view`.
    fn enclosing_view(&self, element: usize) -> Option<usize> {
        let mut at = Some(element);
        // The tree is a forest of at most MAX_ELEMENTS elements, so this ends.
        while let Some(i) = at {
            let e = self.tree.get(i)?;
            if e.tag == "view" {
                return Some(i);
            }
            at = e.parent;
        }
        None
    }

    fn prop(&mut self, element: usize, prop: Prop, depth: u32) -> Res {
        if depth > MAX_REF_DEPTH {
            return Err(Fail::TooDeep);
        }
        match self.memo.get(&(element, prop)) {
            Some(Memo::Done(Some(v))) => return Ok(*v),
            Some(Memo::Done(None)) | Some(Memo::Pending) => return Err(Fail::Unresolved),
            None => {}
        }
        let raw = self
            .tree
            .get(element)
            .ok_or(Fail::Unresolved)?
            .attr(prop.attr())
            .map(str::to_owned);
        self.memo.insert((element, prop), Memo::Pending);
        let result = match raw {
            None => match prop {
                Prop::Width | Prop::Height => Err(Fail::Unresolved),
                _ => Ok(0),
            },
            Some(text) => self.expression(element, &text, depth).and_then(|v| {
                VALUE_RANGE
                    .contains(&v)
                    .then_some(v)
                    .ok_or(Fail::Unresolved)
            }),
        };
        match result {
            Ok(v) => {
                self.memo.insert((element, prop), Memo::Done(Some(v)));
            }
            Err(Fail::Unresolved) => {
                self.memo.insert((element, prop), Memo::Done(None));
            }
            Err(Fail::TooDeep) => {
                self.memo.remove(&(element, prop));
            }
        }
        result
    }

    fn expression(&mut self, element: usize, text: &str, depth: u32) -> Res {
        if text.chars().count() > MAX_EXPR_CHARS {
            return Err(Fail::Unresolved);
        }
        let body = strip(text);
        let tokens = tokenize(body).ok_or(Fail::Unresolved)?;
        let mut p = Parser {
            resolver: self,
            element,
            tokens,
            at: 0,
            parens: 0,
            depth,
        };
        let v = p.expr()?;
        if p.at != p.tokens.len() {
            return Err(Fail::Unresolved);
        }
        Ok(v)
    }
}

/// Removes surrounding blanks, an optional `jscript:` or `wmpprop:` prefix and a trailing `;`.
fn strip(text: &str) -> &str {
    let mut s = text.trim();
    for prefix in ["jscript:", "wmpprop:"] {
        if s.len() >= prefix.len()
            && s.is_char_boundary(prefix.len())
            && s[..prefix.len()].eq_ignore_ascii_case(prefix)
        {
            s = s[prefix.len()..].trim_start();
            break;
        }
    }
    s.strip_suffix(';').unwrap_or(s).trim_end()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Num(i64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Open,
    Close,
    Dot,
}

/// `None` for a character outside the grammar, a literal that overflows or too many tokens.
fn tokenize(s: &str) -> Option<Vec<Tok>> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let tok = match c {
            b' ' | b'\t' | b'\r' | b'\n' => {
                i += 1;
                continue;
            }
            b'+' => Tok::Plus,
            b'-' => Tok::Minus,
            b'*' => Tok::Star,
            b'/' => Tok::Slash,
            b'(' => Tok::Open,
            b')' => Tok::Close,
            b'.' => Tok::Dot,
            b'0'..=b'9' => {
                let mut v: i64 = 0;
                while i < b.len() && b[i].is_ascii_digit() {
                    v = v.checked_mul(10)?.checked_add(i64::from(b[i] - b'0'))?;
                    i += 1;
                }
                out.push(Tok::Num(v));
                if out.len() > MAX_TOKENS {
                    return None;
                }
                continue;
            }
            b'A'..=b'Z' | b'a'..=b'z' | b'_' => {
                let start = i;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
                out.push(Tok::Ident(s[start..i].to_owned()));
                if out.len() > MAX_TOKENS {
                    return None;
                }
                continue;
            }
            _ => return None,
        };
        i += 1;
        out.push(tok);
        if out.len() > MAX_TOKENS {
            return None;
        }
    }
    Some(out)
}

struct Parser<'r, 'a> {
    resolver: &'r mut Resolver<'a>,
    element: usize,
    tokens: Vec<Tok>,
    at: usize,
    parens: u32,
    depth: u32,
}

impl Parser<'_, '_> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.at)
    }

    fn expr(&mut self) -> Res {
        let mut v = self.term()?;
        loop {
            match self.peek() {
                Some(Tok::Plus) => {
                    self.at += 1;
                    v = v.checked_add(self.term()?).ok_or(Fail::Unresolved)?;
                }
                Some(Tok::Minus) => {
                    self.at += 1;
                    v = v.checked_sub(self.term()?).ok_or(Fail::Unresolved)?;
                }
                _ => return Ok(v),
            }
        }
    }

    fn term(&mut self) -> Res {
        let mut v = self.unary()?;
        loop {
            match self.peek() {
                Some(Tok::Star) => {
                    self.at += 1;
                    v = v.checked_mul(self.unary()?).ok_or(Fail::Unresolved)?;
                }
                Some(Tok::Slash) => {
                    self.at += 1;
                    let d = self.unary()?;
                    // Truncates toward zero; zero and MIN / -1 are unresolved.
                    v = v.checked_div(d).ok_or(Fail::Unresolved)?;
                }
                _ => return Ok(v),
            }
        }
    }

    fn unary(&mut self) -> Res {
        if self.peek() == Some(&Tok::Minus) {
            self.at += 1;
            return self.unary()?.checked_neg().ok_or(Fail::Unresolved);
        }
        self.primary()
    }

    fn primary(&mut self) -> Res {
        let tok = self.peek().cloned().ok_or(Fail::Unresolved)?;
        self.at += 1;
        match tok {
            Tok::Num(v) => Ok(v),
            Tok::Open => {
                self.parens += 1;
                if self.parens > MAX_PAREN_DEPTH {
                    return Err(Fail::Unresolved);
                }
                let v = self.expr()?;
                if self.peek() != Some(&Tok::Close) {
                    return Err(Fail::Unresolved);
                }
                self.at += 1;
                self.parens -= 1;
                Ok(v)
            }
            Tok::Ident(name) => self.reference(&name),
            _ => Err(Fail::Unresolved),
        }
    }

    /// An identifier: `<name>.<prop>` or a bare own property. A call or a longer path is
    /// unresolved.
    fn reference(&mut self, name: &str) -> Res {
        let (target, prop) = match self.peek() {
            Some(Tok::Open) => return Err(Fail::Unresolved),
            Some(Tok::Dot) => {
                self.at += 1;
                let Some(Tok::Ident(p)) = self.peek().cloned() else {
                    return Err(Fail::Unresolved);
                };
                self.at += 1;
                if matches!(self.peek(), Some(Tok::Dot | Tok::Open)) {
                    return Err(Fail::Unresolved);
                }
                let prop = Prop::from_name(&p).ok_or(Fail::Unresolved)?;
                let lower = name.to_ascii_lowercase();
                let target = if lower == "view" {
                    self.resolver.enclosing_view(self.element)
                } else {
                    self.resolver.ids.get(&lower).copied()
                };
                (target.ok_or(Fail::Unresolved)?, prop)
            }
            _ => (self.element, Prop::from_name(name).ok_or(Fail::Unresolved)?),
        };
        self.resolver.prop(target, prop, self.depth + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wms::parse;

    /// A description with `body` inside the root.
    fn setup(body: &str) -> (Tree, fn(&Tree, &str) -> usize) {
        let tree = parse(&format!("<theme>{body}</theme>")).unwrap();
        (tree, find)
    }

    /// The index of the element with this id.
    fn find(t: &Tree, id: &str) -> usize {
        t.elements
            .iter()
            .position(|e| e.attr("id") == Some(id))
            .unwrap()
    }

    #[test]
    fn literals_and_defaults() {
        let (t, find) = setup(
            "<view id=v width=\"859\" height=\"468\"><button id=b left=\"12\" top=\"-3\"/></view>",
        );
        let mut r = Resolver::new(&t);
        let b = find(&t, "b");
        assert_eq!(r.value(b, Prop::Left), Some(12));
        assert_eq!(r.value(b, Prop::Top), Some(-3));
        assert_eq!(r.value(b, Prop::ZIndex), Some(0));
        assert_eq!(r.value(b, Prop::Width), None);
        assert_eq!(r.value(find(&t, "v"), Prop::Width), Some(859));
        // Left of the view itself is missing: 0.
        assert_eq!(r.value(find(&t, "v"), Prop::Left), Some(0));
    }

    #[test]
    fn references_and_prefixes() {
        let (t, find) = setup(
            "<view id=v width=\"300\"><subview id=svA left=\"10\" width=\"50\"/>\
             <subview id=c left=\"JScript:svA.left + svA.width - 4;\" \
             width=\"wmpprop:view.width-10\" top=\"7\" height=\"left - top\"/></view>",
        );
        let mut r = Resolver::new(&t);
        let c = find(&t, "c");
        assert_eq!(r.value(c, Prop::Left), Some(56));
        assert_eq!(r.value(c, Prop::Width), Some(290));
        // Own bare properties: left (56) - top (7).
        assert_eq!(r.value(c, Prop::Height), Some(49));
    }

    #[test]
    fn ids_are_case_insensitive_and_the_first_wins() {
        let (t, find) = setup(
            "<view id=v><a id=X left=\"5\"/><a id=x left=\"9\"/><a id=u left=\"x.left*2\"/></view>",
        );
        let mut r = Resolver::new(&t);
        assert_eq!(r.value(find(&t, "u"), Prop::Left), Some(10));
    }

    #[test]
    fn arithmetic_rules() {
        let cases: [(&str, Option<i64>); 14] = [
            ("1+2*3", Some(7)),
            ("(1+2)*3", Some(9)),
            ("-7/2", Some(-3)),
            ("7/-2", Some(-3)),
            ("- -4", Some(4)),
            ("10/0", None),
            ("9223372036854775807+1", None),
            ("9223372036854775808", None),
            ("65536", Some(65536)),
            ("65537", None),
            ("-65537", None),
            ("GetEqSliderLeft(2)", None),
            ("100%", None),
            ("player.controls.x", None),
        ];
        for (expr, want) in cases {
            let (t, find) = setup(&format!("<view id=v><a id=a left=\"{expr}\"/></view>"));
            let mut r = Resolver::new(&t);
            assert_eq!(r.value(find(&t, "a"), Prop::Left), want, "{expr}");
        }
    }

    #[test]
    fn unknown_names_are_unresolved() {
        for expr in [
            "nobody.left",
            "view.depth",
            "a.zindex",
            "foo",
            "view.",
            "1 2",
            "(1",
            "1)",
        ] {
            let (t, find) = setup(&format!("<view id=v><a id=a left=\"{expr}\"/></view>"));
            let mut r = Resolver::new(&t);
            assert_eq!(r.value(find(&t, "a"), Prop::Left), None, "{expr}");
        }
    }

    #[test]
    fn cycles_are_unresolved() {
        let (t, find) = setup(
            "<view id=v><a id=a left=\"b.left\"/><a id=b left=\"a.left\"/>\
             <a id=s left=\"left\"/><a id=ok left=\"3\" top=\"a.left\"/></view>",
        );
        let mut r = Resolver::new(&t);
        assert_eq!(r.value(find(&t, "a"), Prop::Left), None);
        assert_eq!(r.value(find(&t, "b"), Prop::Left), None);
        assert_eq!(r.value(find(&t, "s"), Prop::Left), None);
        // An element that merely refers to a cycle is unresolved too; its other values are not.
        assert_eq!(r.value(find(&t, "ok"), Prop::Top), None);
        assert_eq!(r.value(find(&t, "ok"), Prop::Left), Some(3));
    }

    #[test]
    fn parenthesis_depth_is_capped() {
        let nest = |n: usize| format!("{}1{}", "(".repeat(n), ")".repeat(n));
        for (n, want) in [(16, Some(1)), (17, None)] {
            let (t, find) = setup(&format!("<view id=v><a id=a left=\"{}\"/></view>", nest(n)));
            let mut r = Resolver::new(&t);
            assert_eq!(r.value(find(&t, "a"), Prop::Left), want, "{n}");
        }
    }

    #[test]
    fn lengths_and_token_counts_are_capped() {
        let long = format!("1{}", "+0".repeat(MAX_EXPR_CHARS / 2));
        assert!(long.chars().count() > MAX_EXPR_CHARS);
        let many = format!("1{}", "+1".repeat(MAX_TOKENS));
        let ok = format!("1{}", "+1".repeat(30));
        for (expr, want) in [(long, None), (many, None), (ok, Some(31))] {
            let (t, find) = setup(&format!("<view id=v><a id=a left=\"{expr}\"/></view>"));
            let mut r = Resolver::new(&t);
            assert_eq!(r.value(find(&t, "a"), Prop::Left), want);
        }
    }

    #[test]
    fn a_reference_chain_over_the_depth_cap_is_unresolved_from_its_head() {
        let mut body = String::from("<view id=v>");
        let n = (MAX_REF_DEPTH + 5) as usize;
        for i in 0..n {
            if i + 1 < n {
                body.push_str(&format!("<a id=e{i} left=\"e{}.left\"/>", i + 1));
            } else {
                body.push_str(&format!("<a id=e{i} left=\"1\"/>"));
            }
        }
        body.push_str("</view>");
        let (t, find) = setup(&body);
        let mut r = Resolver::new(&t);
        assert_eq!(r.value(find(&t, "e0"), Prop::Left), None);
        // Resolving the tail first remembers the short chains, so the head then succeeds.
        let mut r = Resolver::new(&t);
        for i in (0..n).rev() {
            assert_eq!(r.value(find(&t, &format!("e{i}")), Prop::Left), Some(1));
        }
    }

    #[test]
    fn results_are_stable_across_calls() {
        let (t, find) = setup("<view id=v width=\"100\"><a id=a width=\"view.width/4\"/></view>");
        let mut r = Resolver::new(&t);
        let a = find(&t, "a");
        assert_eq!(r.value(a, Prop::Width), Some(25));
        assert_eq!(r.value(a, Prop::Width), Some(25));
    }
}

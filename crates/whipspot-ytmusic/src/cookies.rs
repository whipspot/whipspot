//! A Netscape `cookies.txt` read for what the provider needs: the YouTube cookies, kept as the
//! text yt-dlp reads and as the `Cookie` header InnerTube takes (D7).
//!
//! The file is untrusted input: at most 1 MiB, seven tab-separated fields per line, only
//! `youtube.com` and its subdomains are kept, and every name and value is checked, so no value
//! can add a second cookie or a header line. A cookie jar prints as `Jar(<redacted>)`, never
//! its content (hard rule 1).

use std::fmt;

use whipspot_core::secret::Secret;

/// The largest cookies file read.
pub const MAX_FILE_BYTES: usize = 1 << 20;
const MAX_COOKIES: usize = 500;
const MAX_NAME: usize = 256;
const MAX_VALUE: usize = 4096;
const MAX_DOMAIN: usize = 253;
const MAX_PATH: usize = 1024;
const STORED_HEADER: &str = "# Netscape HTTP Cookie File\n";
const HTTP_ONLY: &str = "#HttpOnly_";

/// Why a cookies file was not accepted. Fixed messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CookieError {
    TooLarge,
    NotText,
    /// No `SAPISID` or `__Secure-3PAPISID` among the YouTube cookies.
    NoLogin,
}

impl fmt::Display for CookieError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CookieError::TooLarge => "That file is too large",
            CookieError::NotText => "That is not a cookies.txt file",
            CookieError::NoLogin => "No YouTube login in that file",
        })
    }
}

impl std::error::Error for CookieError {}

struct Cookie {
    /// Lower case, no leading dot.
    domain: String,
    include_subdomains: bool,
    path: String,
    secure: bool,
    /// Unix seconds; 0 is a session cookie.
    expiry: u64,
    name: String,
    value: String,
    http_only: bool,
}

/// The YouTube cookies of one file.
pub struct Jar {
    cookies: Vec<Cookie>,
}

impl fmt::Debug for Jar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Jar(<redacted>)")
    }
}

fn is_youtube(domain: &str) -> bool {
    domain == "youtube.com" || domain.ends_with(".youtube.com")
}

fn domain_ok(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= MAX_DOMAIN
        && d.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        && !d.starts_with('.')
        && !d.ends_with('.')
        && !d.contains("..")
}

fn name_ok(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= MAX_NAME
        && n.bytes()
            .all(|b| b.is_ascii_graphic() && !b"()<>@,;:\\\"/[]?={}".contains(&b))
}

fn value_ok(v: &str) -> bool {
    v.len() <= MAX_VALUE && v.bytes().all(|b| b.is_ascii_graphic() && b != b';')
}

fn path_ok(p: &str) -> bool {
    p.starts_with('/') && p.len() <= MAX_PATH && p.bytes().all(|b| b.is_ascii_graphic())
}

fn flag(s: &str) -> Option<bool> {
    if s.eq_ignore_ascii_case("TRUE") {
        Some(true)
    } else if s.eq_ignore_ascii_case("FALSE") {
        Some(false)
    } else {
        None
    }
}

fn parse_line(line: &str) -> Option<Cookie> {
    let (line, http_only) = match line.strip_prefix(HTTP_ONLY) {
        Some(rest) => (rest, true),
        None => (line, false),
    };
    if line.starts_with('#') {
        return None;
    }
    let f: Vec<&str> = line.split('\t').collect();
    if f.len() != 7 {
        return None;
    }
    let domain = f[0].strip_prefix('.').unwrap_or(f[0]).to_ascii_lowercase();
    if !domain_ok(&domain) || !is_youtube(&domain) {
        return None;
    }
    let expiry = f[4].parse::<i64>().ok()?;
    if !path_ok(f[2]) || !name_ok(f[5]) || !value_ok(f[6]) {
        return None;
    }
    Some(Cookie {
        domain,
        include_subdomains: flag(f[1])?,
        path: f[2].to_owned(),
        secure: flag(f[3])?,
        expiry: expiry.max(0) as u64,
        name: f[5].to_owned(),
        value: f[6].to_owned(),
        http_only,
    })
}

impl Jar {
    /// Reads a cookies file. Lines that are not seven tab-separated fields, comments, and every
    /// cookie outside `youtube.com` are skipped; the file needs a login cookie.
    pub fn parse(bytes: &[u8]) -> Result<Jar, CookieError> {
        if bytes.len() > MAX_FILE_BYTES {
            return Err(CookieError::TooLarge);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| CookieError::NotText)?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let cookies: Vec<Cookie> = text
            .lines()
            .filter_map(parse_line)
            .take(MAX_COOKIES)
            .collect();
        let jar = Jar { cookies };
        if jar.sapisid().is_none() {
            return Err(CookieError::NoLogin);
        }
        Ok(jar)
    }

    /// The jar of a stored secret (what [`to_secret`](Self::to_secret) made).
    pub fn from_secret(secret: &Secret) -> Result<Jar, CookieError> {
        Jar::parse(secret.expose().as_bytes())
    }

    /// The value `SAPISIDHASH` is made from: `SAPISID`, else `__Secure-3PAPISID`.
    pub fn sapisid(&self) -> Option<&str> {
        let find = |n: &str| {
            self.cookies
                .iter()
                .find(|c| c.name == n && !c.value.is_empty())
                .map(|c| c.value.as_str())
        };
        find("SAPISID").or_else(|| find("__Secure-3PAPISID"))
    }

    /// The YouTube cookies as a Netscape file: what is stored, and what yt-dlp is given.
    pub fn to_secret(&self) -> Secret {
        let mut text = String::from(STORED_HEADER);
        let upper = |b: bool| if b { "TRUE" } else { "FALSE" };
        for c in &self.cookies {
            let domain = if c.include_subdomains {
                format!(".{}", c.domain)
            } else {
                c.domain.clone()
            };
            text.push_str(&format!(
                "{}{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                if c.http_only { HTTP_ONLY } else { "" },
                domain,
                upper(c.include_subdomains),
                c.path,
                upper(c.secure),
                c.expiry,
                c.name,
                c.value
            ));
        }
        Secret::new(text)
    }

    /// The `Cookie` header for a request to `host` and `path`: the cookies whose domain and path
    /// match and that have not expired at `now_unix`. Empty when none do.
    pub fn header(&self, host: &str, path: &str, now_unix: u64) -> Secret {
        let host = host.to_ascii_lowercase();
        let parts: Vec<String> = self
            .cookies
            .iter()
            .filter(|c| {
                let domain_match = host == c.domain
                    || (c.include_subdomains
                        && host
                            .strip_suffix(c.domain.as_str())
                            .is_some_and(|p| p.ends_with('.')));
                domain_match
                    && path.starts_with(c.path.as_str())
                    && (c.expiry == 0 || c.expiry > now_unix)
            })
            .map(|c| format!("{}={}", c.name, c.value))
            .collect();
        Secret::new(parts.join("; "))
    }

    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }
}

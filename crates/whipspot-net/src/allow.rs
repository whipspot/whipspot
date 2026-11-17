//! The host allow-list (MD65): checked on the one parsed `Url` the request is built from, never
//! on URL text, so no parser differential can separate what was checked from what is fetched.

use reqwest::Url;

/// The longest URL text accepted, checked before parsing.
pub(crate) const MAX_URL_BYTES: usize = 2048;

/// A URL the list refused. Carries nothing: the reason never reaches a caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Refused;

/// Declared hosts, validated. An empty list refuses everything.
#[derive(Debug, Clone)]
pub(crate) struct HostList {
    hosts: Vec<String>,
}

/// A valid declared host: lowercase LDH labels, two or more, the last not all digits, not
/// `localhost`.
fn valid_host(h: &str) -> bool {
    if h.is_empty() || h.len() > 253 || h == "localhost" {
        return false;
    }
    let labels: Vec<&str> = h.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let label_ok = |l: &str| {
        (1..=63).contains(&l.len())
            && l.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !l.starts_with('-')
            && !l.ends_with('-')
    };
    labels.iter().all(|l| label_ok(l))
        && !labels
            .last()
            .is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit()))
}

impl HostList {
    /// Keeps the declared hosts that pass [`valid_host`] after ASCII-lowercasing.
    pub(crate) fn new(declared: &[&str]) -> HostList {
        let mut hosts: Vec<String> = Vec::new();
        for h in declared {
            let h = h.to_ascii_lowercase();
            if valid_host(&h) && !hosts.contains(&h) {
                hosts.push(h);
            }
        }
        HostList { hosts }
    }

    /// Passes only an `https` URL with no userinfo and no explicit port, whose host is a domain
    /// (IP literals, `0x7f.1` and `2130706433` have none), not `localhost`, `*.localhost` or
    /// trailing-dot, and equal to a declared host or a subdomain of one.
    pub(crate) fn check(&self, url: &Url) -> Result<(), Refused> {
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
        {
            return Err(Refused);
        }
        let domain = url.domain().ok_or(Refused)?;
        if domain == "localhost" || domain.ends_with(".localhost") || domain.ends_with('.') {
            return Err(Refused);
        }
        let ok = self.hosts.iter().any(|h| {
            domain == h
                || domain
                    .strip_suffix(h.as_str())
                    .is_some_and(|p| p.ends_with('.'))
        });
        if ok { Ok(()) } else { Err(Refused) }
    }

    /// The text length bound, then the parse, then [`check`](Self::check); returns the one
    /// `Url` the request must be built from.
    pub(crate) fn check_str(&self, text: &str) -> Result<Url, Refused> {
        if text.len() > MAX_URL_BYTES {
            return Err(Refused);
        }
        let url = Url::parse(text).map_err(|_| Refused)?;
        self.check(&url)?;
        Ok(url)
    }

    #[cfg(test)]
    fn hosts(&self) -> Vec<&str> {
        self.hosts.iter().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h1_the_host_table() {
        let list = HostList::new(&["scdn.co", "spotifycdn.com"]);
        let allowed = [
            "https://i.scdn.co/x",
            "https://I.SCDN.CO/x",
            "https://mosaic.scdn.co/640/x",
            "https://image-cdn-ak.spotifycdn.com/image/x",
            "https://scdn.co/x",
            "https://xn--cfa.scdn.co/x",
            "https://i.scdn.co:443/x",
            "https://i%2escdn.co/x",
            "https:i.scdn.co/x",
            // A backslash ends the host: the request really goes to i.scdn.co.
            "https://i.scdn.co\\@evil.example/x",
        ];
        for u in allowed {
            assert!(list.check_str(u).is_ok(), "should be allowed: {u}");
        }
        let long = format!("https://i.scdn.co/{}", "a".repeat(2049 - 18));
        assert_eq!(long.len(), 2049);
        let refused = [
            "http://i.scdn.co/x",
            "https://i.scdn.co@evil.example/x",
            "https://user:pw@i.scdn.co/x",
            "https://i.scdn.co./x",
            "https://i.scdn.co%2e/x",
            "https://i.scdn.co:8443/x",
            "https://127.0.0.1/x",
            "https://0x7f.1/x",
            "https://2130706433/x",
            "https://[::1]/x",
            "https://localhost/x",
            "https://LOCALHOST./x",
            "https://evilscdn.co/x",
            "https://scdn.co.evil.example/x",
            "https://xn--cdn-ehd.co/x",
            "ftp://i.scdn.co/x",
            "not a url",
            "",
            long.as_str(),
        ];
        for u in refused {
            assert!(list.check_str(u).is_err(), "should be refused: {u}");
        }
    }

    #[test]
    fn the_checked_url_is_the_one_returned() {
        let list = HostList::new(&["scdn.co"]);
        let url = list.check_str("https://I.SCDN.CO/Ab").unwrap();
        assert_eq!(url.as_str(), "https://i.scdn.co/Ab");
    }

    #[test]
    fn h2_declared_hosts_are_validated() {
        let list = HostList::new(&[
            "scdn.co",
            "SPOTIFYCDN.COM",
            "",
            "co",
            "127.0.0.1",
            "localhost",
            ".scdn.co",
            "scdn.co.",
            "a..b",
            "-x.com",
        ]);
        assert_eq!(list.hosts(), vec!["scdn.co", "spotifycdn.com"]);
    }

    #[test]
    fn an_empty_list_refuses_everything() {
        let list = HostList::new(&[]);
        assert!(list.check_str("https://i.scdn.co/x").is_err());
    }
}

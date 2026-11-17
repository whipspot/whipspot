//! The `--connect-trace` diagnostic (D16): which dealer messages a Connect session sees.
//!
//! A user-run flag of the real app. Each message the Connect session receives on a known
//! namespace ([`PREFIXES`]) prints one line to stderr: the time (UTC), a running count of that
//! shape and the URI with every path segment outside [`VOCABULARY`] replaced by `<x>`, so ids,
//! user names and connection ids never print. Never a payload, a header or a token. A segment
//! that is a vocabulary word is printed whatever it means in that place (a user named `update`
//! would show as `update`); everything else, including anything with an upper-case letter, `%`
//! or `=`, is `<x>`.
//!
//! Off unless [`enable`] was called: nothing is subscribed, printed or counted, and no logger is
//! installed either way.

use std::{
    collections::HashMap,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

/// The dealer namespaces a trace subscribes to (prefixes of the message URIs). The dealer cannot
/// be asked for every message, so a namespace missing here is not seen.
pub(crate) const PREFIXES: [&str; 12] = [
    "hm://pusher/",
    "hm://connect-state/",
    "hm://playlist/",
    "hm://collection/",
    "hm://social-connect/",
    "hm://track-playback/",
    "hm://remote-config/",
    "hm://notification/",
    "hm://user/",
    "spotify:user:",
    "spotify:playlist:",
    "social-connect/",
];

/// The path segments that print as they are. Lower case only.
pub(crate) const VOCABULARY: [&str; 29] = [
    "pusher",
    "v1",
    "v2",
    "connections",
    "connect-state",
    "connect",
    "cluster",
    "volume",
    "logout",
    "player",
    "command",
    "devices",
    "playlist",
    "collection",
    "json",
    "social-connect",
    "session_update",
    "track-playback",
    "remote-config",
    "notification",
    "user",
    "attributes",
    "update",
    "changed",
    "add",
    "remove",
    "state",
    "session",
    "hm",
];

/// Most path segments printed; the rest is `...`.
const MAX_SEGMENTS: usize = 8;
/// Most distinct shapes counted; a further new one is counted as `<other>`.
const MAX_SHAPES: usize = 128;
const MASK: &str = "<x>";
const OTHER: &str = "<other>";

static ENABLED: AtomicBool = AtomicBool::new(false);
static TRACER: OnceLock<Tracer> = OnceLock::new();

/// Turn the trace on for the Connect sessions started from now on.
pub fn enable() {
    ENABLED.store(true, Ordering::SeqCst);
}

pub(crate) fn enabled() -> bool {
    ENABLED.load(Ordering::SeqCst)
}

/// Print the line for one dealer message URI to stderr. Does nothing when the trace is off.
pub(crate) fn record(uri: &str) {
    if !enabled() {
        return;
    }
    let line = TRACER
        .get_or_init(Tracer::default)
        .line(uri, SystemTime::now());
    eprintln!("{line}");
}

/// `uri` with every segment outside [`VOCABULARY`] replaced by `<x>`. The scheme (`hm://`,
/// `spotify:` or none) and the separators are kept; the dealer's own splitting rules apply.
pub(crate) fn redact(uri: &str) -> String {
    let (scheme, sep, rest) = if let Some(rest) = uri.strip_prefix("hm://") {
        ("hm://", '/', rest)
    } else if let Some(rest) = uri.strip_prefix("spotify:") {
        ("spotify:", ':', rest)
    } else {
        ("", '/', uri)
    };
    let sep_text = sep.to_string();
    let rest = rest.trim_end_matches(sep);
    let mut out = String::from(scheme);
    for (i, segment) in rest.split(sep).enumerate() {
        if i > 0 {
            out.push_str(&sep_text);
        }
        if i == MAX_SEGMENTS {
            out.push_str("...");
            break;
        }
        out.push_str(if VOCABULARY.contains(&segment) {
            segment
        } else {
            MASK
        });
    }
    out
}

/// Counts the shapes seen and formats the lines.
#[derive(Default)]
pub(crate) struct Tracer {
    counts: Mutex<HashMap<String, u64>>,
}

impl Tracer {
    /// The line for `uri` seen at `now`: `connect-trace HH:MM:SSZ #<count> <redacted uri>`.
    pub(crate) fn line(&self, uri: &str, now: SystemTime) -> String {
        let shape = redact(uri);
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        let key = if counts.contains_key(&shape) || counts.len() < MAX_SHAPES {
            shape.clone()
        } else {
            OTHER.to_owned()
        };
        let n = counts.entry(key).or_insert(0);
        *n += 1;
        let n = *n;
        drop(counts);
        let secs = now
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() % 86_400);
        format!(
            "connect-trace {:02}:{:02}:{:02}Z #{n} {shape}",
            secs / 3_600,
            secs % 3_600 / 60,
            secs % 60
        )
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn ids_users_and_connection_ids_are_masked() {
        for (uri, want) in [
            (
                "hm://playlist/v2/playlist/37i9dQZF1DXcBWIGoYBM5M",
                "hm://playlist/v2/playlist/<x>",
            ),
            (
                "hm://collection/collection/someuser/json",
                "hm://collection/collection/<x>/json",
            ),
            (
                "hm://pusher/v1/connections/Zm9vYmFyLWNvbm5lY3Rpb24taWQtd2l0aC1hLWxvbmctYm9keQ==",
                "hm://pusher/v1/connections/<x>",
            ),
            (
                "spotify:user:attributes:update",
                "spotify:user:attributes:update",
            ),
            (
                "social-connect/v2/session_update",
                "social-connect/v2/session_update",
            ),
            (
                "spotify:user:someuser:playlist:abc",
                "spotify:user:<x>:playlist:<x>",
            ),
        ] {
            assert_eq!(redact(uri), want, "{uri}");
        }
    }

    #[test]
    fn percent_equals_and_upper_case_never_print() {
        for uri in [
            "hm://playlist/v2/Playlist/x%20y",
            "hm://pusher/v1/connections/a=b",
            "hm://COLLECTION/collection/User/json?x=1",
            "hm://playlist/v2/playlist/%E2%82%AC",
        ] {
            let out = redact(uri);
            for forbidden in ['%', '=', '?'] {
                assert!(!out.contains(forbidden), "{uri} -> {out}");
            }
            assert!(
                !out.chars().any(|c| c.is_ascii_uppercase()),
                "{uri} -> {out}"
            );
        }
        assert_eq!(
            redact("hm://COLLECTION/collection/User/json?x=1"),
            "hm://<x>/collection/<x>/<x>"
        );
    }

    #[test]
    fn trailing_separators_and_a_long_path_are_bounded() {
        assert_eq!(redact("hm://pusher/v1/"), "hm://pusher/v1");
        let long = format!("hm://{}", ["a"; 40].join("/"));
        let out = redact(&long);
        assert_eq!(out, "hm://<x>/<x>/<x>/<x>/<x>/<x>/<x>/<x>/...");
        assert_eq!(redact(""), "<x>");
    }

    #[test]
    fn a_line_has_the_utc_time_a_running_count_and_only_the_redacted_uri() {
        let tracer = Tracer::default();
        let at = UNIX_EPOCH + Duration::from_secs(86_400 * 3 + 14 * 3_600 + 3 * 60 + 9);
        let a = "hm://playlist/v2/playlist/37i9dQZF1DXcBWIGoYBM5M";
        let b = "hm://playlist/v2/playlist/other";
        assert_eq!(
            tracer.line(a, at),
            "connect-trace 14:03:09Z #1 hm://playlist/v2/playlist/<x>"
        );
        // The same shape counts on, whatever the id.
        assert_eq!(
            tracer.line(b, at),
            "connect-trace 14:03:09Z #2 hm://playlist/v2/playlist/<x>"
        );
        let line = tracer.line("hm://pusher/v1/connections/secret-id", at);
        assert!(
            line.ends_with("#1 hm://pusher/v1/connections/<x>"),
            "{line}"
        );
        assert!(!line.contains("secret-id"));
    }

    #[test]
    fn the_shape_table_is_bounded() {
        let tracer = Tracer::default();
        // More distinct shapes than the table holds: the vocabulary words in differing counts of segments.
        let mut last = String::new();
        for n in 1..=MAX_SEGMENTS {
            for word in VOCABULARY {
                let uri = format!("hm://{}", vec![word; n].join("/"));
                last = tracer.line(&uri, UNIX_EPOCH);
            }
        }
        assert!(VOCABULARY.len() * MAX_SEGMENTS > MAX_SHAPES);
        assert!(last.contains('#'));
        assert!(tracer.counts.lock().unwrap().len() <= MAX_SHAPES + 1);
    }

    #[test]
    fn the_trace_is_off_until_enabled_and_every_prefix_is_all_vocabulary() {
        assert!(!enabled());
        // Off: record() prints nothing and counts nothing.
        record("hm://pusher/v1/connections/x");
        assert!(TRACER.get().is_none());
        for p in PREFIXES {
            assert_eq!(redact(p), p.trim_end_matches(['/', ':']), "{p}");
        }
    }
}

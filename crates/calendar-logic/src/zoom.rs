//! The "join Zoom from the browser" heuristic.
//!
//! Zoom hides the web-client join button behind an app-launch dance. But the web client is
//! reachable directly at `https://{host}/wc/{id}/join?pwd={token}`. So we scan an event's text
//! for any Zoom meeting link (`/j/`, `/wc/…/join`, `/wc/join/…`, `/s/`, `/launch/jc/`), pull out
//! the numeric meeting id, the host, and the `pwd` token, then rebuild the canonical web-client
//! URL — the thing `xdg-open` hands the browser, skipping the app entirely.
//!
//! Note: the `pwd` *token* in the link (`?pwd=…`) is not the human "Passcode: 123456"; only the
//! token authenticates a one-click join, so we never try to synthesise it from a passcode. A
//! link without a token still yields a `/wc/{id}/join` URL — the web client then prompts for the
//! passcode, which is strictly better than the app-launch wall.

/// A Zoom meeting parsed out of free text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoomMeeting {
    /// Host, lowercased — `zoom.us` or a vanity subdomain like `bigcorp.zoom.us`.
    pub host: String,
    /// Numeric meeting id.
    pub id: String,
    /// The `pwd` token from the link's query, if present (not the human passcode).
    pub pwd: Option<String>,
}

impl ZoomMeeting {
    /// The canonical web-client join URL (`/wc/{id}/join`), carrying the `pwd` token when known.
    pub fn web_join_url(&self) -> String {
        match &self.pwd {
            Some(pwd) => format!("https://{}/wc/{}/join?pwd={}", self.host, self.id, pwd),
            None => format!("https://{}/wc/{}/join", self.host, self.id),
        }
    }
}

/// Find the best Zoom meeting in `text` and return its web-client join URL.
pub fn zoom_join_url(text: &str) -> Option<String> {
    best_meeting(text).map(|m| m.web_join_url())
}

/// The best Zoom meeting in `text`: prefer the first link that carries a `pwd` token (so the
/// rebuilt join URL needs no manual passcode), else fall back to the first valid link. This is
/// why a `/j/…?pwd=…` invite link wins over its sibling `/launch/jc/…` chat link.
pub fn best_meeting(text: &str) -> Option<ZoomMeeting> {
    // iCal escapes newlines inside DESCRIPTION as a literal `\n` (and `,`/`;` as `\,`/`\;`), so a
    // raw value can read `…Meeting\nhttps://…` with no whitespace. Unescape first so a `\n` cleanly
    // separates the URL instead of gluing an `n` onto it.
    let cleaned = unescape_ical(text);
    let mut first: Option<ZoomMeeting> = None;
    for tok in tokens(&cleaned) {
        if let Some(m) = parse_zoom(tok) {
            if m.pwd.is_some() {
                return Some(m);
            }
            if first.is_none() {
                first = Some(m);
            }
        }
    }
    first
}

/// Undo RFC 5545 §3.3.11 text escaping: `\n`/`\N` → newline, `\,` → `,`, `\;` → `;`, `\\` → `\`.
/// We only need clean token boundaries, so any other `\X` collapses to its second char.
fn unescape_ical(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') | Some('N') => out.push('\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Split free text into URL-ish tokens. Splits on whitespace and the delimiters that bracket a
/// URL in iCal `DESCRIPTION` text — including a bare backslash, since iCal escapes newlines as a
/// literal `\n` (no whitespace) and a URL never contains `\`. Trailing sentence punctuation is
/// trimmed so `…/join.` or `…/join,` doesn't swallow the dot into the URL.
fn tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '\\' | '<' | '>' | '"' | '\'' | '(' | ')' | '[' | ']' | '|'
            )
    })
    .map(|t| t.trim_matches(|c: char| matches!(c, '.' | ',' | ';' | '!' | '?')))
    .filter(|t| !t.is_empty())
}

/// Parse a single token as a Zoom meeting URL, or `None` if it isn't one.
fn parse_zoom(url: &str) -> Option<ZoomMeeting> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, after) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    if !(host == "zoom.us" || host.ends_with(".zoom.us")) {
        return None;
    }
    let (path, query) = match after.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (after, None),
    };
    // The meeting id is the first all-digit path segment of meeting-id length (≥9). This catches
    // every form uniformly: /j/{id}, /wc/{id}/join, /wc/join/{id}, /s/{id}, /launch/jc/{id}. A
    // personal-room link (/my/{name}) has no numeric segment, so it correctly yields nothing.
    let id = path
        .split('/')
        .find(|seg| seg.len() >= 9 && seg.bytes().all(|b| b.is_ascii_digit()))?;
    let pwd = query.and_then(|q| query_param(q, "pwd"));
    Some(ZoomMeeting {
        host,
        id: id.to_string(),
        pwd,
    })
}

/// First non-empty value of query parameter `key` (case-insensitive).
fn query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k.eq_ignore_ascii_case(key) && !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // SANITIZED fixture — mirrors the structure of a real Google/Zoom invite (a `/j/` invite link
    // with a `pwd` token, a sibling `/launch/jc/` chat link, a separate human Meeting ID/Passcode,
    // and an unrelated link) but every identifier/token is fabricated. No real meeting is encoded.
    const INVITE: &str = "\
Interview type: Debrief

Here is a link to the interview kit:
https://app.example-ats.test/guides/111/people/222/interview?application_id=333

-----
Alex Doe is inviting you to a scheduled Zoom meeting.

Topic: [Debrief] Candidate Name - Senior Engineer
Time: Jun 15, 2026 08:45 AM Pacific Time (US and Canada)
Join Zoom Meeting
https://acme.zoom.us/j/12345678901?pwd=AbCdEfGhIjKlMnOpQrStUv.1

Meeting chat link
https://acme.zoom.us/launch/jc/12345678901

Meeting ID: 123 4567 8901
Passcode: 000000
";

    #[test]
    fn invite_builds_web_client_join_url_with_token() {
        assert_eq!(
            zoom_join_url(INVITE).as_deref(),
            Some("https://acme.zoom.us/wc/12345678901/join?pwd=AbCdEfGhIjKlMnOpQrStUv.1")
        );
    }

    #[test]
    fn prefers_link_with_pwd_over_chat_link() {
        // The /launch/jc/ chat link appears too but has no token; the /j/ invite wins.
        let m = best_meeting(INVITE).unwrap();
        assert_eq!(m.id, "12345678901");
        assert_eq!(m.pwd.as_deref(), Some("AbCdEfGhIjKlMnOpQrStUv.1"));
    }

    #[test]
    fn vanity_subdomain_preserved() {
        let m = best_meeting("see https://bigcorp.zoom.us/j/98765432109").unwrap();
        assert_eq!(m.host, "bigcorp.zoom.us");
        assert_eq!(m.id, "98765432109");
        assert_eq!(m.pwd, None);
        assert_eq!(
            m.web_join_url(),
            "https://bigcorp.zoom.us/wc/98765432109/join"
        );
    }

    #[test]
    fn bare_zoom_us_host() {
        let m = best_meeting("https://zoom.us/j/11122233344?pwd=tok.2").unwrap();
        assert_eq!(m.host, "zoom.us");
        assert_eq!(
            m.web_join_url(),
            "https://zoom.us/wc/11122233344/join?pwd=tok.2"
        );
    }

    #[test]
    fn already_a_wc_join_link_is_idempotent() {
        let url = "https://acme.zoom.us/wc/12345678901/join?pwd=tok.9";
        assert_eq!(zoom_join_url(url).as_deref(), Some(url));
    }

    #[test]
    fn wc_join_id_first_form() {
        let m = best_meeting("https://acme.zoom.us/wc/join/12345678901").unwrap();
        assert_eq!(m.id, "12345678901");
    }

    #[test]
    fn ical_escaped_newline_does_not_glue_url() {
        // iCal folds DESCRIPTION with literal `\n` (backslash-n), not real newlines.
        let text = "Join Zoom Meeting\\nhttps://acme.zoom.us/j/12345678901?pwd=tok.1\\nMeeting ID:";
        assert_eq!(
            zoom_join_url(text).as_deref(),
            Some("https://acme.zoom.us/wc/12345678901/join?pwd=tok.1")
        );
    }

    #[test]
    fn trailing_sentence_punctuation_trimmed() {
        let text = "Join at https://acme.zoom.us/j/12345678901.";
        let m = best_meeting(text).unwrap();
        assert_eq!(m.id, "12345678901");
    }

    #[test]
    fn non_zoom_links_ignored() {
        assert_eq!(
            zoom_join_url("https://app.example-ats.test/guides/111"),
            None
        );
        assert_eq!(
            zoom_join_url("https://notzoom.us.evil.test/j/12345678901"),
            None
        );
    }

    #[test]
    fn personal_room_has_no_numeric_id() {
        assert_eq!(zoom_join_url("https://acme.zoom.us/my/alex.doe"), None);
    }

    #[test]
    fn no_zoom_link_at_all() {
        assert_eq!(zoom_join_url("just some text, no meeting here"), None);
        assert_eq!(zoom_join_url(""), None);
    }

    #[test]
    fn phone_one_tap_is_not_a_url() {
        // "+15551234567,,12345678901#" — a dial string, not an https URL.
        assert_eq!(
            zoom_join_url("One tap mobile +15551234567,,12345678901# US"),
            None
        );
    }
}

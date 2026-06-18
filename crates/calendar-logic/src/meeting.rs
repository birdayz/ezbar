//! The "join the meeting from the browser" heuristic — Zoom and Google Meet.
//!
//! **Zoom** hides the web-client join button behind an app-launch dance. But the web client is
//! reachable directly at `https://{host}/wc/{id}/join?pwd={token}`. So we scan an event's text
//! for any Zoom meeting link (`/j/`, `/wc/…/join`, `/wc/join/…`, `/s/`, `/launch/jc/`), pull out
//! the numeric meeting id, the host, and the `pwd` token, then rebuild the canonical web-client
//! URL — the thing `xdg-open` hands the browser, skipping the app entirely.
//!
//! Note: the `pwd` *token* in the link (`?pwd=…`) is not the human "Passcode: 123456"; only the
//! token authenticates a one-click join, so we never try to synthesise it from a passcode. A
//! link without a token still yields a `/wc/{id}/join` URL — the web client then prompts for the
//! passcode, which is strictly better than the app-launch wall.
//!
//! **Google Meet** is already web-first: `https://meet.google.com/{xxx-xxxx-xxx}` (or a `/lookup/…`
//! link) opens the meeting directly with no app wall and no passcode, so we just find it and pass
//! it through unchanged.
//!
//! [`join_url`] is the entry point — the best click-to-join link for an event, preferring a
//! one-click Zoom (with token) or a Meet link over a passcode-prompting Zoom.

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

/// The best **click-to-join** link in `text`, across providers. A one-click Zoom (carrying a `pwd`
/// token) wins; otherwise a Google Meet link is preferred over a token-less Zoom, because Meet
/// always opens directly while a token-less Zoom would still prompt for the passcode. Returns
/// `None` when there's no recognised meeting link.
pub fn join_url(text: &str) -> Option<String> {
    let cleaned = unescape_ical(text);
    let mut zoom_no_pwd: Option<String> = None;
    let mut meet: Option<String> = None;
    for tok in tokens(&cleaned) {
        if let Some(m) = parse_zoom(tok) {
            if m.pwd.is_some() {
                return Some(m.web_join_url()); // best: one-click Zoom
            }
            zoom_no_pwd.get_or_insert_with(|| m.web_join_url());
        } else if let Some(url) = parse_meet(tok) {
            meet.get_or_insert(url);
        }
    }
    meet.or(zoom_no_pwd)
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
    // Some feeds percent-encode the query separators (`?pwd%3DTOKEN` instead of `?pwd=TOKEN`), so
    // the `pwd` token would otherwise be missed and the join URL would drop the passcode. Zoom
    // ids/hosts/tokens contain no `%`, so decoding only ever normalises those encoded separators.
    let decoded = percent_decode(url);
    let url = decoded.as_str();
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

/// Decode `%XX` byte escapes (URL percent-encoding); invalid/truncated escapes pass through
/// unchanged. Bytes are reassembled before UTF-8 decoding so a multi-byte escape sequence is
/// handled correctly.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(hi), Some(lo)) = (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                out.push(hi << 4 | lo);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Parse a single token as a Google Meet link, returning it unchanged (Meet is web-first, so the
/// link is the join URL). Accepts the standard `meet.google.com/{xxx-xxxx-xxx}` code form and the
/// `meet.google.com/lookup/…` named-meeting form; anything else (a bare host, a non-meet path) is
/// `None`.
fn parse_meet(url: &str) -> Option<String> {
    let decoded = percent_decode(url);
    let rest = decoded
        .strip_prefix("https://")
        .or_else(|| decoded.strip_prefix("http://"))?;
    let (host, after) = rest.split_once('/')?;
    if !host.eq_ignore_ascii_case("meet.google.com") {
        return None;
    }
    let seg = after.split(['/', '?']).next().unwrap_or("");
    if is_meet_code(seg) || seg == "lookup" {
        // Normalise the scheme/host; keep the path + any query (e.g. ?authuser=) intact.
        Some(format!("https://meet.google.com/{after}"))
    } else {
        None
    }
}

/// A Google Meet meeting code: three lowercase-letter groups of length 3-4-3 (`abc-defg-hij`).
fn is_meet_code(seg: &str) -> bool {
    let p: Vec<&str> = seg.split('-').collect();
    p.len() == 3
        && [3, 4, 3] == [p[0].len(), p[1].len(), p[2].len()]
        && p.iter().all(|g| g.bytes().all(|b| b.is_ascii_lowercase()))
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
    fn percent_encoded_pwd_is_decoded() {
        // some feeds percent-encode the query separator: `?pwd%3DTOKEN` instead of `?pwd=TOKEN`.
        assert_eq!(
            zoom_join_url("https://acme.zoom.us/j/12345678901?pwd%3DAbCdEf.1").as_deref(),
            Some("https://acme.zoom.us/wc/12345678901/join?pwd=AbCdEf.1")
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

    #[test]
    fn google_meet_link_passed_through() {
        // Meet is web-first: the link is the join URL, returned unchanged.
        assert_eq!(
            join_url("Join with Google Meet https://meet.google.com/abc-defg-hij").as_deref(),
            Some("https://meet.google.com/abc-defg-hij")
        );
        // escaped-newline form (DESCRIPTION) + trailing bilingual text must not glue onto the URL.
        assert_eq!(
            join_url("Google Meet\\nhttps://meet.google.com/abc-defg-hij\\nOder per Telefon")
                .as_deref(),
            Some("https://meet.google.com/abc-defg-hij")
        );
        // the /lookup/ named-meeting form.
        assert_eq!(
            join_url("https://meet.google.com/lookup/abcdef123").as_deref(),
            Some("https://meet.google.com/lookup/abcdef123")
        );
    }

    #[test]
    fn meet_query_preserved_and_non_meet_rejected() {
        assert_eq!(
            join_url("https://meet.google.com/abc-defg-hij?authuser=1").as_deref(),
            Some("https://meet.google.com/abc-defg-hij?authuser=1")
        );
        assert_eq!(join_url("https://meet.google.com/"), None); // bare host
        assert_eq!(join_url("https://meet.google.com/about"), None); // not a meeting code
        assert_eq!(join_url("https://meet.google.com/ab-cd-ef"), None); // wrong shape (not 3-4-3)
        assert_eq!(join_url("https://google.com/abc-defg-hij"), None); // wrong host
    }

    #[test]
    fn provider_precedence() {
        // one-click Zoom (pwd) beats everything, regardless of order.
        let z = "Meet https://meet.google.com/abc-defg-hij \
                 Zoom https://acme.zoom.us/j/12345678901?pwd=tok.1";
        assert_eq!(
            join_url(z).as_deref(),
            Some("https://acme.zoom.us/wc/12345678901/join?pwd=tok.1")
        );
        // a token-less Zoom would prompt for a passcode, so Meet (clean one-click) is preferred.
        let m = "Zoom https://acme.zoom.us/j/12345678901 \
                 Meet https://meet.google.com/abc-defg-hij";
        assert_eq!(
            join_url(m).as_deref(),
            Some("https://meet.google.com/abc-defg-hij")
        );
    }
}

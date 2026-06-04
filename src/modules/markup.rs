//! A tiny inline-markup subset (RFC 0002 "Text & markup"): `[c=token]…[/c]` colour and
//! `[b]…[/b]` weight spans, resolved against the active theme — *not* Pango, so it stays
//! renderer-agnostic and themeable. Used where a ricer authors the text (the `custom`
//! module's command output), so they can colour or bold a substring with no Rust:
//!
//! ```text
//! ping -c1 1.1.1.1 >/dev/null && echo '[c=ok]up[/c]' || echo '[c=urgent]down[/c]'
//! ```
//!
//! Deliberately forgiving: any `[`…`]` that isn't a recognised tag is passed through as
//! literal text, and unbalanced tags just style to the end / no-op — a command that emits
//! a stray bracket never breaks, and existing markup-free widgets render byte-identically
//! (the no-tag case is a single plain segment → a plain `text`).

use ezbar_plugin::iced::widget::{rich_text, span, text};
use ezbar_plugin::iced::{Color, Element, Font};
use ezbar_plugin::{Ctx, ModMsg};

/// One run of text with resolved style flags. `color` is a theme *token name*
/// (e.g. `"accent"`), resolved against `Ctx` at render time — never a literal colour —
/// so a marked-up substring re-themes with the rest of the bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub text: String,
    pub color: Option<String>,
    pub bold: bool,
}

/// Parse `s` into styled [`Segment`]s. Recognises `[b]…[/b]` and `[c=TOKEN]…[/c]`
/// (nestable, where TOKEN is `[A-Za-z0-9_-]+`); everything else — including an
/// unrecognised or malformed `[…]` — is literal text. Adjacent segments of equal style
/// are coalesced, so markup-free input yields exactly one plain segment.
pub fn parse(s: &str) -> Vec<Segment> {
    let mut segs: Vec<Segment> = Vec::new();
    let mut buf = String::new();
    let mut bold: u32 = 0;
    let mut colors: Vec<String> = Vec::new();
    let mut rest = s;

    // Flush the accumulated text as a segment carrying the *current* style.
    let flush = |buf: &mut String, bold: u32, colors: &[String], segs: &mut Vec<Segment>| {
        if !buf.is_empty() {
            segs.push(Segment {
                text: std::mem::take(buf),
                color: colors.last().cloned(),
                bold: bold > 0,
            });
        }
    };

    while let Some(ch) = rest.chars().next() {
        if ch == '[' {
            if let Some(after) = rest.strip_prefix("[b]") {
                flush(&mut buf, bold, &colors, &mut segs);
                bold += 1;
                rest = after;
                continue;
            }
            if let Some(after) = rest.strip_prefix("[/b]") {
                flush(&mut buf, bold, &colors, &mut segs);
                bold = bold.saturating_sub(1);
                rest = after;
                continue;
            }
            if let Some(after) = rest.strip_prefix("[/c]") {
                flush(&mut buf, bold, &colors, &mut segs);
                colors.pop();
                rest = after;
                continue;
            }
            if let Some(after) = rest.strip_prefix("[c=") {
                if let Some(end) = after.find(']') {
                    let token = &after[..end];
                    let ok = !token.is_empty()
                        && token
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
                    if ok {
                        flush(&mut buf, bold, &colors, &mut segs);
                        colors.push(token.to_string());
                        rest = &after[end + 1..];
                        continue;
                    }
                }
            }
            // Not a recognised tag → a literal '[' (then keep scanning the remainder).
            buf.push('[');
            rest = &rest['['.len_utf8()..];
        } else {
            buf.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    flush(&mut buf, bold, &colors, &mut segs);
    coalesce(segs)
}

/// Merge consecutive segments with identical style — so a no-op close tag (`x[/c]y`) or a
/// markup-free string collapses back to one segment (keeping the plain-`text` fast path).
fn coalesce(segs: Vec<Segment>) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::with_capacity(segs.len());
    for s in segs {
        match out.last_mut() {
            Some(prev) if prev.color == s.color && prev.bold == s.bold => prev.text.push_str(&s.text),
            _ => out.push(s),
        }
    }
    out
}

/// Resolve a theme-token name to a colour via `Ctx`. Unknown tokens return `None` (the
/// span then keeps the default text colour rather than guessing).
fn token_color(token: &str, ctx: &Ctx) -> Option<Color> {
    Some(match token {
        "accent" => ctx.accent(),
        "fg" => ctx.fg(),
        "fg_dim" | "dim" => ctx.fg_dim(),
        "ok" | "good" => ctx.ok(),
        "warn" | "warning" => ctx.warn(),
        "urgent" | "error" | "bad" => ctx.urgent(),
        "sep" => ctx.sep(),
        _ => return None,
    })
}

/// Render `text`'s markup into an `Element`. The common, tag-free case is a single
/// unstyled segment and renders as a plain `text` widget (identical to pre-markup output,
/// no `rich_text` overhead); only genuinely styled text builds spans.
pub fn view<'a>(s: &str, ctx: &Ctx) -> Element<'a, ModMsg> {
    let segs = parse(s);
    let styled = segs.iter().any(|seg| seg.color.is_some() || seg.bold);
    if !styled {
        let plain = segs.into_iter().next().map(|s| s.text).unwrap_or_default();
        return text(plain).into();
    }
    let bold = Font {
        weight: ezbar_plugin::iced::font::Weight::Bold,
        ..Font::DEFAULT
    };
    let spans: Vec<ezbar_plugin::iced::widget::text::Span<'a, ()>> = segs
        .iter()
        .map(|seg| {
            let mut sp = span(seg.text.clone());
            if let Some(c) = seg.color.as_deref().and_then(|t| token_color(t, ctx)) {
                sp = sp.color(c);
            }
            if seg.bold {
                sp = sp.font(bold);
            }
            sp
        })
        .collect();
    rich_text(spans).into()
}

#[cfg(test)]
mod tests {
    use super::{parse, Segment};

    fn seg(text: &str, color: Option<&str>, bold: bool) -> Segment {
        Segment { text: text.into(), color: color.map(Into::into), bold }
    }

    #[test]
    fn plain_text_is_one_unstyled_segment() {
        assert_eq!(parse("hello world"), vec![seg("hello world", None, false)]);
        assert_eq!(parse(""), Vec::<Segment>::new());
    }

    #[test]
    fn colour_and_bold_spans() {
        assert_eq!(parse("[c=ok]up[/c]"), vec![seg("up", Some("ok"), false)]);
        assert_eq!(parse("[b]hi[/b]"), vec![seg("hi", None, true)]);
    }

    #[test]
    fn spans_mix_with_surrounding_plain_text() {
        assert_eq!(
            parse("net [c=urgent]down[/c] now"),
            vec![
                seg("net ", None, false),
                seg("down", Some("urgent"), false),
                seg(" now", None, false),
            ]
        );
    }

    #[test]
    fn tags_nest() {
        assert_eq!(parse("[b][c=ok]x[/c][/b]"), vec![seg("x", Some("ok"), true)]);
    }

    #[test]
    fn unclosed_tag_styles_to_the_end() {
        assert_eq!(parse("[c=warn]rest"), vec![seg("rest", Some("warn"), false)]);
    }

    #[test]
    fn unrecognised_or_malformed_brackets_are_literal() {
        // not a known tag → literal
        assert_eq!(parse("[foo]bar"), vec![seg("[foo]bar", None, false)]);
        // empty / illegal token → literal '[' then the rest as text
        assert_eq!(parse("[c=]x"), vec![seg("[c=]x", None, false)]);
        // a stray close is a no-op and coalesces away
        assert_eq!(parse("x[/c]y"), vec![seg("xy", None, false)]);
    }

    #[test]
    fn view_builds_every_path_without_panicking() {
        // Exercise the render side (not just the parser): a `view` panic isn't contained,
        // so make sure the rich_text/span build runs for styled, plain, and unknown-token
        // input against a real `Ctx`.
        use ezbar_plugin::iced::Element;
        use ezbar_plugin::{Ctx, ModMsg, ThemeTokens};
        let theme = ThemeTokens {
            fg: [1.0; 4],
            fg_dim: [0.6, 0.6, 0.6, 1.0],
            urgent: [1.0, 0.0, 0.0, 1.0],
            warn: [1.0, 0.8, 0.0, 1.0],
            ok: [0.0, 1.0, 0.0, 1.0],
            accent: [0.5, 0.3, 0.9, 1.0],
            sep: [0.3; 4],
            bg: [0.1, 0.1, 0.1, 1.0],
            text_size: 13.0,
            bar_height: 28,
        };
        let ctx = Ctx { instance_id: 0, theme: &theme };
        let _styled: Element<'_, ModMsg> = super::view("net [c=urgent]down[/c] [b]!!![/b]", &ctx);
        let _plain: Element<'_, ModMsg> = super::view("just text", &ctx);
        let _unknown: Element<'_, ModMsg> = super::view("[c=nope]x[/c]", &ctx); // unknown token → builds, no colour
    }
}

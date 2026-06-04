//! `window_title` module: the focused window's title, from the shared sway service.
//!
//! ```toml
//! [modules.window_title]
//! max    = 80                  # truncate long titles (0 = no limit)
//! format = "[c=fg_dim]{title}[/c]"   # optional: a `{title}` placeholder + inline markup
//! ```
//!
//! `format` accepts the RFC 0002 markup subset (`[c=token]…[/c]`, `[b]…[/b]`) around a
//! `{title}` placeholder. The markup is parsed ONCE from the (trusted) format string and
//! the title is substituted *after*, so a window title that itself contains `[` is never
//! interpreted as markup.

use ezbar_plugin::iced::futures::{Stream, StreamExt};
use ezbar_plugin::iced::{Element, Subscription};
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

use crate::modules::markup::{self, Segment};
use crate::sources::sway;

struct Title(String);

pub struct WindowTitle {
    instance: u64,
    max: usize,
    title: String,
    /// The parsed `format` (with a literal `{title}` placeholder in its segment text), or
    /// `None` for the default — a bare, plain title (rendered without any markup machinery).
    format: Option<Vec<Segment>>,
}

impl WindowTitle {
    pub fn new(instance: u64, cfg: &toml::Value) -> Self {
        let max = cfg
            .get("max")
            .and_then(|v| v.as_integer())
            .unwrap_or(80)
            .max(0) as usize;
        // Parse the format ONCE here (it's static config); `view` only substitutes the title.
        // A format without any `{title}` is honoured as-is (a fixed label), which is harmless.
        let format = cfg
            .get("format")
            .and_then(|v| v.as_str())
            .map(markup::parse);
        WindowTitle {
            instance,
            max,
            title: String::new(),
            format,
        }
    }
}

impl Module for WindowTitle {
    fn id(&self) -> &str {
        "window_title"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        ezbar_plugin::sub::keyed(self.instance, title_sub)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        if let Some(Title(t)) = msg.get::<Title>() {
            self.title = if self.max > 0 && t.chars().count() > self.max {
                let cut: String = t.chars().take(self.max.saturating_sub(1)).collect();
                format!("{cut}…")
            } else {
                t.clone()
            };
        }
        Response::none()
    }

    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        match &self.format {
            // Substitute the (already-truncated) title into each format segment's text, then
            // render. Substituting AFTER parsing keeps the title literal — no markup injection.
            Some(fmt) => markup::render(&substitute(fmt, &self.title), ctx),
            None => markup::render(&[Segment { text: self.title.clone(), color: None, bold: false }], ctx),
        }
    }
}

/// Replace the `{title}` placeholder in each format segment's text with `title` (the
/// markup having already been parsed off the trusted format string). The title is dropped
/// in as literal text, so a title containing `[`…`]` is never re-interpreted as markup.
fn substitute(fmt: &[Segment], title: &str) -> Vec<Segment> {
    fmt.iter()
        .map(|s| Segment {
            text: s.text.replace("{title}", title),
            color: s.color.clone(),
            bold: s.bold,
        })
        .collect()
}

fn title_sub(_id: &u64) -> impl Stream<Item = ModMsg> {
    sway::title().map(|t| ModMsg::new(Title(t)))
}

#[cfg(test)]
mod tests {
    use super::substitute;
    use crate::modules::markup::{parse, Segment};

    fn seg(text: &str, color: Option<&str>, bold: bool) -> Segment {
        Segment { text: text.into(), color: color.map(Into::into), bold }
    }

    #[test]
    fn title_is_substituted_into_parsed_markup() {
        // format parsed once → markup structure; the title fills the placeholder after.
        let fmt = parse("[c=accent]{title}[/c]");
        assert_eq!(substitute(&fmt, "Firefox"), vec![seg("Firefox", Some("accent"), false)]);
    }

    #[test]
    fn a_title_with_brackets_stays_literal() {
        // the placeholder substitution is literal — markup tags in the *title* are NOT parsed.
        let fmt = parse("[b]{title}[/b]");
        assert_eq!(
            substitute(&fmt, "[c=urgent]hax[/c]"),
            vec![seg("[c=urgent]hax[/c]", None, true)] // bold from the format; brackets literal
        );
    }
}

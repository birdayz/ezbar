//! ezbar WASM plugin: wintitle — the focused window title, read via the **sway capability**
//! (RFC 0013, a v0.2.0 plugin). The honest proof that a sway-reading widget can be a sandboxed
//! plugin: no sway connection of its own, just `ctx.sway_snapshot()` — and the title renders
//! faithfully in the bounded DSL (it's plain `text`).
//!
//! Grant it the capability (the host denies the read otherwise):
//! ```toml
//! [modules.wintitle]
//! sway = true
//! ```

use ezbar_plugin_wasm::prelude::*;

const MAX: usize = 80; // clamp very long titles

#[derive(Default)]
struct WinTitle {
    title: String,
}

impl Plugin for WinTitle {
    fn update(&mut self, ctx: &mut dyn Ctx, ev: Event) -> bool {
        let Event::Timer = ev else { return false };
        match ctx.sway_snapshot() {
            Ok(s) => {
                let t: String = s.title.chars().take(MAX).collect();
                let changed = t != self.title;
                self.title = t;
                ctx.set_timeout(500); // poll the focused title at ~2 Hz
                changed
            }
            Err(e) => {
                // ungranted (or no source) — degrade to a placeholder, back off.
                ctx.log(&format!("wintitle: {e}"));
                let changed = !self.title.is_empty();
                self.title.clear();
                ctx.set_timeout(5000);
                changed
            }
        }
    }

    fn view(&self) -> Render {
        let label = if self.title.is_empty() {
            "\u{2014}".to_string() // em dash placeholder (no window / denied)
        } else {
            self.title.clone()
        };
        row([
            Icon::Bot.view(14.0, Token::FgDim),
            text(label).color(Token::Fg),
        ])
        .spacing(6.0)
    }
}

export_plugin!(WinTitle);

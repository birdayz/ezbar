//! ezbar WASM plugin: `kube` — a kubectl-context switcher. The chip shows the current context
//! (red on prod, dim `—` when none). **Click** it to open the bar's **native searchable picker**
//! (RFC 0018) over the context list; pick one to switch (`kubectl config use-context`), or pick
//! the top **`⊘ none`** entry to clear it (`kubectl config unset current-context`, à la
//! `kubectx -u`). The picker — search field, fuzzy filter, keyboard, focus, theming — is the
//! host's; this plugin just supplies the list and runs the switch over the sandboxed `exec`.
//!
//! ```toml
//! [modules.kube]
//! exec = ["kubectl"]      # the dangerous tier — grant it explicitly (or `[plugins] yolo`)
//! ```

use ezbar_plugin_wasm::prelude::*;

/// Picker entry that clears the current context instead of switching to one. Distinct enough
/// (the `⊘` glyph) that it can't collide with a real context name.
const UNSET: &str = "⊘ none (unset current context)";

#[derive(Default)]
struct Kube {
    current: String,
    contexts: Vec<String>,
}

impl Kube {
    fn refresh(&mut self, ctx: &mut dyn Ctx) {
        // `kubectl config current-context` exits non-zero ("current-context is not set") when
        // none is selected — so a non-zero code means *empty*, not "keep the old value". Only an
        // exec error (kubectl missing) leaves `current` untouched.
        if let Ok(o) = ctx.exec("kubectl", &["config", "current-context"], None) {
            self.current = if o.code == 0 {
                o.stdout_str()
            } else {
                String::new()
            };
        }
        if let Ok(o) = ctx.exec("kubectl", &["config", "get-contexts", "-o", "name"], None) {
            if o.code == 0 {
                self.contexts = o
                    .stdout_str()
                    .lines()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
    }
}

impl Plugin for Kube {
    fn update(&mut self, ctx: &mut dyn Ctx, ev: Event) -> bool {
        match ev {
            Event::Timer => {
                self.refresh(ctx);
                ctx.set_timeout(5000); // re-poll context + list at ~0.2 Hz
                true
            }
            // Click the chip → open the host's native searchable picker over the contexts, with
            // an `⊘ none` entry at the top to clear the current context.
            Event::Pointer {
                kind: PointerKind::Press,
                ..
            } => {
                if self.contexts.is_empty() {
                    return false;
                }
                let mut items: Vec<&str> = vec![UNSET];
                items.extend(self.contexts.iter().map(|s| s.as_str()));
                // Highlight the current context (shifted by the prepended `⊘ none`), or `⊘ none`
                // itself when nothing is selected.
                let current = if self.current.is_empty() {
                    Some(0)
                } else {
                    self.contexts
                        .iter()
                        .position(|c| *c == self.current)
                        .map(|p| p + 1)
                };
                if let Some(chosen) = ctx.pick("kube context", &items, current) {
                    if chosen == UNSET {
                        let _ = ctx.exec("kubectl", &["config", "unset", "current-context"], None);
                    } else if chosen != self.current {
                        let _ = ctx.exec("kubectl", &["config", "use-context", &chosen], None);
                    }
                    self.refresh(ctx);
                }
                true
            }
            _ => false,
        }
    }

    fn view(&self) -> Render {
        let label = if self.current.is_empty() {
            "\u{2014}".to_string()
        } else {
            self.current.clone()
        };
        // production contexts go red so you notice before you `kubectl delete` the wrong thing.
        let color = if self.current.contains("prod") {
            Token::Urgent
        } else if self.current.is_empty() {
            Token::FgDim
        } else {
            Token::Fg
        };
        // the whole chip is a click target (opens the native picker) — wrap it in a mouse_area so
        // the press reaches `update` as `Event::Pointer{Press}`.
        mouse_area(
            "chip",
            row([
                Icon::Kubernetes.view(14.0, Token::Accent),
                text(label).color(color),
            ])
            .spacing(6.0),
        )
    }
}

export_plugin!(Kube);

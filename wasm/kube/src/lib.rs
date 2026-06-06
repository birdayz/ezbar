//! ezbar WASM plugin: `kube` — a kubectl-context switcher. The chip shows the current context
//! (red on prod). **Click** it to open the bar's **native searchable picker** (RFC 0018) over
//! the context list; pick one to switch (`kubectl config use-context`). The picker — search
//! field, fuzzy filter, keyboard, focus, theming — is the host's; this plugin just supplies the
//! list and runs the switch over the sandboxed `exec` capability.
//!
//! ```toml
//! [modules.kube]
//! exec = ["kubectl"]      # the dangerous tier — grant it explicitly (or `[plugins] yolo`)
//! ```

use ezbar_plugin_wasm::prelude::*;

#[derive(Default)]
struct Kube {
    current: String,
    contexts: Vec<String>,
}

impl Kube {
    fn refresh(&mut self, ctx: &mut dyn Ctx) {
        if let Ok(o) = ctx.exec("kubectl", &["config", "current-context"], None) {
            if o.code == 0 {
                self.current = o.stdout_str();
            }
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
            // Click the chip → open the host's native searchable picker over the contexts.
            Event::Pointer { kind: PointerKind::Press, .. } => {
                if self.contexts.is_empty() {
                    return false;
                }
                let items: Vec<&str> = self.contexts.iter().map(|s| s.as_str()).collect();
                let current = self.contexts.iter().position(|c| *c == self.current);
                if let Some(chosen) = ctx.pick("kube context", &items, current) {
                    if chosen != self.current {
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

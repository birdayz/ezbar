//! ezbar WASM plugin: `kube` — an **interactive** kubectl-context widget, the full motivating
//! example (RFC 0015). The chip shows the current context (red on prod). **Left-click** the
//! chip to open a sticky picker; **left-click a context** to switch to it
//! (`kubectl config use-context`) — both over the sandboxed `exec` capability.
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
            // a picker row was clicked → switch to that context, then refresh the chip
            Event::Pointer { id, kind: PointerKind::Press, .. } => {
                let Some(target) = id.strip_prefix("use:") else {
                    return false;
                };
                let target = target.to_string();
                let _ = ctx.exec("kubectl", &["config", "use-context", &target], None);
                self.refresh(ctx);
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
        row([
            Icon::Kubernetes.view(14.0, Token::Accent),
            text(label).color(color),
        ])
        .spacing(6.0)
    }

    fn popup(&self) -> Option<Render> {
        if self.contexts.is_empty() {
            return None; // nothing to pick → no interactive popup
        }
        let rows: Vec<Render> = self
            .contexts
            .iter()
            .map(|c| {
                let active = *c == self.current;
                let label = text(if active {
                    format!("\u{2713} {c}") // ✓ current
                } else {
                    format!("   {c}")
                })
                .color(if active { Token::Accent } else { Token::Fg });
                // each row is a click target → makes the popup "interactive" (click-to-open)
                mouse_area(format!("use:{c}"), container(label))
            })
            .collect();
        Some(column(rows).spacing(3.0))
    }
}

export_plugin!(Kube);

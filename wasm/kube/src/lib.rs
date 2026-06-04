//! ezbar WASM plugin: `kube` — the current kubectl context, run with `ctx.exec` over the
//! **exec capability** (RFC 0015). The motivating example: a kubectl widget *can* be a
//! sandboxed plugin once it may run an allow-listed program. Red on a `prod*` context.
//!
//! ```toml
//! [modules.kube]
//! exec = ["kubectl"]            # the dangerous tier — grant it explicitly
//! # prog = "kubectl"            # optional: override the program / args (default below)
//! # args = "config current-context"
//! ```

use ezbar_plugin_wasm::prelude::*;

struct Kube {
    prog: String,
    args: Vec<String>,
    ctx_name: String,
}

impl Default for Kube {
    fn default() -> Self {
        Kube {
            prog: "kubectl".into(),
            args: vec!["config".into(), "current-context".into()],
            ctx_name: String::new(),
        }
    }
}

impl Plugin for Kube {
    fn load(&mut self, config: Vec<(String, String)>) {
        for (k, v) in &config {
            match k.as_str() {
                "prog" => self.prog = v.clone(),
                "args" => self.args = v.split_whitespace().map(String::from).collect(),
                _ => {}
            }
        }
    }

    fn update(&mut self, ctx: &mut dyn Ctx, ev: Event) -> bool {
        let Event::Timer = ev else { return false };
        let argv: Vec<&str> = self.args.iter().map(String::as_str).collect();
        let new = match ctx.exec(&self.prog, &argv, None) {
            Ok(o) if o.code == 0 => o.stdout_str().chars().take(40).collect(),
            Ok(o) => format!("err {}", o.code),
            Err(e) => {
                ctx.log(&format!("kube: {e}")); // ungranted / not installed → blank chip
                String::new()
            }
        };
        let changed = new != self.ctx_name;
        self.ctx_name = new;
        ctx.set_timeout(5000); // re-poll the context at ~0.2 Hz
        changed
    }

    fn view(&self) -> Render {
        if self.ctx_name.is_empty() {
            return text("\u{2014}").color(Token::FgDim); // em dash: no context / denied
        }
        // production contexts go red so you notice before you `kubectl delete` the wrong thing.
        let color = if self.ctx_name.contains("prod") {
            Token::Urgent
        } else {
            Token::Fg
        };
        row([
            Icon::Kubernetes.view(14.0, Token::Accent),
            text(self.ctx_name.clone()).color(color),
        ])
        .spacing(6.0)
    }
}

export_plugin!(Kube);

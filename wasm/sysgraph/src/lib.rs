//! ezbar WASM plugin: sysgraph — a live CPU graph, drawn from a host feed (RFC 0012).
//!
//! The proof that a *metric* widget can be a **sandboxed plugin**: no subprocess, no
//! filesystem, no `/proc`. It subscribes to the host's `cpu` feed and the host pushes a
//! sample roughly once a second; the plugin just keeps a ring buffer and draws a sparkline.
//! RFC 0007 found these widgets *couldn't* be plugins before host feeds existed — this is
//! the counter-example.
//!
//! Grant it the capability (the host delivers nothing otherwise):
//! ```toml
//! [modules.sysgraph]
//! feeds = ["cpu"]
//! ```
//!
//! Note how little there is: a `Plugin` impl + `export_plugin!`.

use ezbar_plugin_wasm::prelude::*;

const N: usize = 48; // sparkline window (samples)

#[derive(Default)]
struct SysGraph {
    history: Vec<f64>,
    subscribed: bool,
}

impl Plugin for SysGraph {
    fn update(&mut self, ctx: &mut dyn Ctx, ev: Event) -> bool {
        match ev {
            // On the first (bootstrap) tick, subscribe to the cpu feed and then go fully
            // event-driven: cancel the heartbeat (`set_timeout(0)`) so we wake only when the
            // host pushes a sample — zero idle cost (RFC 0011 + 0012).
            Event::Timer => {
                if !self.subscribed {
                    ctx.feed_subscribe(Feed::Cpu, 1000);
                    ctx.set_timeout(0);
                    self.subscribed = true;
                }
                false
            }
            Event::Feed { feed: Feed::Cpu, value } => {
                self.history.push(value);
                if self.history.len() > N {
                    self.history.remove(0);
                }
                true
            }
            _ => false,
        }
    }

    fn view(&self) -> Render {
        let Some(&last) = self.history.last() else {
            // No sample yet — a quiet placeholder until the first feed lands.
            return row([
                Icon::Cpu.view(14.0, Token::FgDim),
                text("\u{2026}").color(Token::FgDim),
            ])
            .spacing(6.0);
        };
        row([
            Icon::Cpu.view(14.0, Token::Accent),
            Graph {
                values: self.history.clone(),
                kind: GraphKind::Cpu,
                line: Token::Accent.into(),
            }
            .view(),
            text(format!("{last:.0}%")).color(Token::Fg).size(14.0),
        ])
        .spacing(6.0)
    }
}

export_plugin!(SysGraph);

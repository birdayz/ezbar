#!/usr/bin/env bash
# Scaffold a new ezbar WASM plugin (RFC 0006): one `Plugin` impl + `export_plugin!`.
#   ./new-plugin.sh my-widget
# Then: cd my-widget && cargo build --target wasm32-wasip2 --release
set -euo pipefail
name="${1:?usage: new-plugin.sh <name>}"
dir="$(cd "$(dirname "$0")" && pwd)/$name"
[ -e "$dir" ] && { echo "error: $dir already exists" >&2; exit 1; }
# CamelCase the type name from a kebab/snake name
ty="$(echo "$name" | sed -E 's/[-_]+/ /g' | awk '{for(i=1;i<=NF;i++)$i=toupper(substr($i,1,1)) substr($i,2)}1' | tr -d ' ')"
mkdir -p "$dir/src"

cat > "$dir/Cargo.toml" <<EOF
[package]
name = "$name"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]            # a plugin is a wasm component

[dependencies]
ezbar-plugin-wasm = { path = "../../crates/ezbar-plugin-wasm" }
# add your own deps here (e.g. serde_json = "1"); NOT wit-bindgen — the SDK owns it.

[profile.release]                  # keep the .wasm small
opt-level = "s"
lto = true
strip = true
codegen-units = 1
EOF

cat > "$dir/src/lib.rs" <<EOF
//! ezbar WASM plugin: $name. Write your logic; \`export_plugin!\` is the only glue.

use ezbar_plugin_wasm::prelude::*;

#[derive(Default)]
struct $ty {
    label: String,
}

impl Plugin for $ty {
    fn load(&mut self, _config: Vec<(String, String)>) {}

    // \`update\` can use gated host services on \`ctx\` (e.g. ctx.http_get). Return
    // true when the chip changed and should repaint.
    fn update(&mut self, _ctx: &mut dyn Ctx, ev: Event) -> bool {
        match ev {
            Event::Timer => { self.label = "hello".into(); true }
            _ => false,
        }
    }

    // pure + synchronous: build the chip from the widget DSL.
    fn view(&self) -> Render {
        row([Icon::Dot.view(14.0, Token::Accent), text(self.label.clone())]).spacing(5.0)
    }
}

export_plugin!($ty);
EOF

cat > "$dir/README.md" <<EOF
# $name — an ezbar WASM plugin

\`\`\`sh
cargo build --target wasm32-wasip2 --release
# preview it in a window before shipping (add --net/--set if it needs them):
cargo run -p ezbar-wasm --example preview -- target/wasm32-wasip2/release/$name.wasm
cp target/wasm32-wasip2/release/$name.wasm ~/.config/ezbar/plugins/
\`\`\`

## Config & capabilities

The bar reads \`[modules.$name]\` from \`~/.config/ezbar/config.toml\` and hands the
keys to \`load()\`. **This plugin is sandboxed: \`ctx.http_get\` is DENIED unless the
user grants the exact host(s) you fetch from.** If your plugin hits the network,
tell users to add this (replace the host with the one YOUR plugin calls — delete
this section entirely if it makes no network requests):

\`\`\`toml
[modules.$name]
network = "REPLACE-WITH-THE-HOST-YOU-FETCH"   # required for ctx.http_get; e.g. api.open-meteo.com
\`\`\`
EOF

echo "created $dir"
echo "next: (cd $dir && cargo build --target wasm32-wasip2 --release)"

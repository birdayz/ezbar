---
name: ezbar-wasm-plugin-author
description: Write a sandboxed WASM plugin for the ezbar status bar (RFC 0006) — a Plugin that renders a chip from the widget DSL, compiled to a wasm32-wasip2 component, capability-gated and hot-reloadable. Use when the user asks to build/scaffold/"vibe-code" an ezbar plugin, a custom bar widget, or a chip that polls some data source.
---

# Writing an ezbar WASM plugin

An ezbar WASM plugin is a **`.wasm` component** that the bar loads, sandboxes,
and renders. You write a `Plugin`: an Elm loop that builds its chip from a
**bounded widget vocabulary** (text / layout / our `Icon` + `Graph` components);
the host renders it with real iced and themes it. See `rfcs/0006-wasm-plugins.md`
for the full design and `wasm/weather/` for a complete, working example.

**This is NOT arbitrary iced.** There is no `canvas`/`Shader`/custom widget —
that's a compile-in module, not a plugin. The plugin describes *intent*
(text, an icon, a sparkline over your data, a popup list); the host owns the
*look*. If a user wants a bespoke GPU widget, tell them that's a compile-in
module (`.claude/skills/ezbar-plugin-author`), not a wasm plugin.

## The 5-minute path

1. **Copy the example** — it's the template:
   ```bash
   cp -r wasm/weather wasm/<your-plugin>   # then edit name in Cargo.toml
   ```
   The whole file is `impl Plugin for Weather { … }` + `export_plugin!(Weather);`.
   There is **no glue** to copy or understand — the SDK owns wit-bindgen and the
   generated bindings.

2. **Build it to a component:**
   ```bash
   cd wasm/<your-plugin> && cargo build --target wasm32-wasip2 --release
   ```
   The `.wasm` lands in `target/wasm32-wasip2/release/`. Drop it into
   `~/.config/ezbar/plugins/` and the bar loads it as a pill.

3. **Write your `Plugin` impl.** That plus the one `export_plugin!(YourType);`
   line is the entire plugin — see below.

## The whole plugin: a `Plugin` impl + one macro

```rust
use ezbar_plugin_wasm::{export_plugin, widget::*, Ctx, Event, Icon, Plugin, Render, Token};

#[derive(Default)]
struct Clock { now: String }

impl Plugin for Clock {
    fn update(&mut self, ctx: &mut dyn Ctx, ev: Event) -> bool {
        if let Event::Timer = ev { self.now = "12:34".into(); true } else { false }
    }
    fn view(&self) -> Render {
        row([Icon::Clock.view(14.0, Token::Fg), text(self.now.clone())]).spacing(5.0)
    }
}

export_plugin!(Clock);   // ← the ONLY boilerplate
```

The trait (`ezbar_plugin_wasm`), all defaulted except `view`:

```rust
pub trait Plugin {                                       // + a `Default` impl
    fn load(&mut self, config: Vec<(String, String)>) {} // read [modules.<id>] config
    fn update(&mut self, ctx: &mut dyn Ctx, ev: Event) -> bool { false } // true => re-render
    fn view(&self) -> Render;                            // PURE + sync: build the chip
    fn popup(&self) -> Option<Render> { None }           // hover detail surface (auto open/close)
    fn save_state(&self) -> Vec<u8> { Vec::new() }       // kept across a CLEAN reload only
    fn restore(&mut self, state: Vec<u8>) {}
}
```

- **`view`/`popup` are pure + synchronous** — no I/O, no host calls. Build the
  description and return. The host calls `view` only when `update` returned
  `true`, so do your work in `update`.
- **`update` gets a `Ctx`** with the gated host services: `ctx.http_get(url)`
  (real HTTP — see *Capabilities*), `ctx.log(msg)`. It runs off the GUI thread,
  so a blocking fetch is fine. Drive ticks via `Event::Timer`.
- **Hover popups are free:** implement `popup()` and the runtime opens it on
  hover, closes it on leave, and sizes the surface to your content. Nothing else
  to wire.

## The widget DSL

Build a `Render` with the `widget` builders + our components:

```rust
use ezbar_plugin_wasm::{widget::*, Icon, Graph, GraphKind, Token, Paint};

// a chip: ☁ 21°C
row([Icon::Cloud.view(14.0, Token::Fg), text("21°C").color(Token::Fg)]).spacing(5.0)

// a labelled sparkline over YOUR data (the thing a shell script can't do):
row([
    text("BTC").color(Token::FgDim),
    Graph { values: prices, kind: GraphKind::Generic, line: Token::Ok.into() }.view(),
]).spacing(6.0)
```

- Builders: `text`, `row`, `column`, `container`, `mouse_area(id, child)`, `spacer`.
- Fluent setters: `.color(token|rgba)`, `.size(px)`, `.spacing(px)`, `.align(..)`, `.padding(px)`.
- **Colours are theme tokens** (`Token::{Fg,FgDim,Accent,Ok,Warn,Urgent,Bg}`) or
  `Paint::Rgba(..)`. Prefer tokens so the chip respects the user's theme.
- **Icons** are the host set (`Icon::{Cpu,Cloud,Github,Spotify,…}`). **Graph** is
  the host sparkline over your `values`.

## Interactivity

Wrap a region in `mouse_area("id", child)`. The host sends
`Event::Pointer { id, kind, delta }` to `update`; match on `id` and your
`PointerKind` (Press/RightPress/Scroll/Enter/Leave), mutate state, return `true`.
Open a popup by returning `Some(tree)` from `popup()`.

## Capabilities (the manifest)

A plugin ships a `ezbar-plugin.toml` next to the `.wasm`. Declare *only* what you
need — the host enforces it (an ungranted host import isn't even in the linker):

```toml
id = "weather"
name = "Weather"
version = "0.1.0"
# api_version is injected at build time — never hand-write it

[[capabilities]]
kind = "network"          # outbound HTTP, scoped to one host
host = "api.open-meteo.com"

[[capabilities]]
kind = "bar-state"        # read host data feeds (no extra cost — host samples these)
feeds = ["cpu", "mem"]
```

Kinds (v1): `network { host }`, `read-file { path }`, `bar-state { feeds }`.
There is **no `exec`** — a plugin that needs a subprocess is a `custom` script,
not a sandboxed plugin. The user is prompted to grant on first load; changing the
`.wasm` or the manifest re-prompts.

## Rules

- **No `canvas`/`Shader`/arbitrary iced.** There is no such API in the DSL (on any
  target) — reaching for it simply won't compile. It's a compile-in module, not a
  plugin.
- **`view` is pure + synchronous.** Do I/O in `update`.
- **Keep the tree small.** The host enforces a node/depth cap (≈2000 / 32); a
  giant tree is rejected during the lift.
- **Don't hang.** A runaway `view`/`update` is epoch-trapped and the plugin is
  disabled — bound your loops.
- **State is lost on a trap.** `save_state`/`restore` only survive a *clean*
  reload, not a crash.
- **Build flags:** keep the `.wasm` small — `opt-level="s"`, `lto=true`,
  `strip=true`, `codegen-units=1` (already in the example's `Cargo.toml`).

## Cargo.toml

```toml
[lib]
crate-type = ["cdylib"]            # required — a plugin is a component

[dependencies]
ezbar-plugin-wasm = { path = "../../crates/ezbar-plugin-wasm" }
# + whatever your logic needs (e.g. serde_json). NOT wit-bindgen — the SDK owns it.

[profile.release]                  # keep the .wasm small
opt-level = "s"
lto = true
strip = true
codegen-units = 1
```

That's the whole setup: `crate-type=["cdylib"]`, depend on the SDK, write your
`Plugin` impl, `export_plugin!(YourType);`. No `wit/` dir, no `wit_bindgen`, no
generated-binding glue — `export_plugin!` wires your type to the component world
behind the scenes (the SDK does the `wit-bindgen::generate!` and the `Guest`
bridge).

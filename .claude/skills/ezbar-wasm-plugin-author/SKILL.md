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

**Rust or Go.** The default path below is Rust (the richest SDK). The same plugin
can be written in **Go/TinyGo** with a mirror SDK — see *Writing it in Go* at the
end. Pick Rust unless the user asks for Go.

**This is NOT arbitrary iced.** There is no `canvas`/`Shader`/custom widget —
that's a compile-in module, not a plugin. The plugin describes *intent*
(text, an icon, a sparkline over your data, a popup list); the host owns the
*look*. If a user wants a bespoke GPU widget, tell them that's a compile-in
module (`.claude/skills/ezbar-plugin-author`), not a wasm plugin.

## The 5-minute path

1. **Scaffold** — writes a buildable crate (Cargo.toml + a `Plugin` stub + README):
   ```bash
   wasm/new-plugin.sh <your-plugin>            # creates wasm/<your-plugin>/
   ```
   The stub is `impl Plugin for YourType { … }` + `export_plugin!(YourType);` and
   nothing else — there is **no glue** to copy or understand (the SDK owns
   wit-bindgen and the generated bindings). Prefer a richer starting point? Copy
   the worked example instead: `cp -r wasm/weather wasm/<your-plugin>`.

2. **One-time toolchain setup** (only the first time you ever build a plugin):
   ```bash
   rustup target add wasm32-wasip2
   ```

3. **Build it to a component:**
   ```bash
   cd wasm/<your-plugin> && cargo build --target wasm32-wasip2 --release
   ```
   The `.wasm` lands in `target/wasm32-wasip2/release/`.

4. **Preview it in a real window** — *see your chip render* before it touches the
   bar. This runs your component through the actual host runtime + the themed
   harness (same drive loop the bar uses), so the chip, its colours, and the
   hover popup look exactly as they will live:
   ```bash
   cargo run -p ezbar-wasm --example preview -- \
       wasm/<your-plugin>/target/wasm32-wasip2/release/<your-plugin>.wasm
   ```
   - Network plugin? Grant the host and pass config the same way the bar would:
     `--net api.example.com --set key=value` (repeatable). Without `--net`,
     `ctx.http_get` is denied — exactly as in the bar.
   - Hover the chip to fire its popup; check contrast on the swatches; screenshot
     for a visual record. This is your sub-30s "does it render + stay under caps"
     loop — iterate here, *then* drop the `.wasm` into `~/.config/ezbar/plugins/`.

5. **Write your `Plugin` impl.** That plus the one `export_plugin!(YourType);`
   line is the entire plugin — see below.

## The whole plugin: a `Plugin` impl + one macro

```rust
use ezbar_plugin_wasm::prelude::*;   // brings in widget::*, export_plugin!, Icon, Event, Token, …

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
- **`update` gets a `Ctx`** with the gated host services. It runs off the GUI thread,
  so a blocking call is fine:
  - `ctx.http_get(url)` — real HTTP, gated by `network` (see *Capabilities*).
  - `ctx.log(msg)` — a line to the bar's log.
  - `ctx.set_timeout(ms)` — **control your own wake cadence** (RFC 0011). One-shot: ask
    for the next `Event::Timer` in `ms`; re-arm each tick to keep polling, `0` cancels
    (a purely reactive plugin then costs zero). Never called ⇒ a legacy ~2 s heartbeat.
  - `ctx.feed_subscribe(Feed::Cpu, 1000)` — **subscribe to a host system metric** (cpu/
    memory/temperature/battery/net, RFC 0012); the host then delivers `Event::Feed { feed,
    value }`. Gated by `feeds`. Lets a sandboxed plugin draw a cpu graph with no `/proc`.
  - `ctx.sway_snapshot()` — **read-only sway state** (workspace list + focused title,
    RFC 0013); a *pull* call returning `Result<SwayState, String>`. Gated by `sway`.
  - Drive plain ticks via `Event::Timer`; handle `Event::Feed`/`Event::Pointer` likewise.
- **Hover popups are free:** implement `popup()` and the runtime hovers the
  **whole chip** for you — opens the popup on enter, closes on leave, and
  content-sizes the surface (chart, text list, or a mix). You do **not** need a
  `mouse_area` for the hover popup; reach for `mouse_area` only when *you* want to
  handle clicks/scroll yourself (see *Interactivity*).

## The widget DSL

Build a `Render` with the `widget` builders + our components (all under the
`prelude`):

```rust
use ezbar_plugin_wasm::prelude::*;

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
  `.color(..)` is sugar that applies to whatever the node is — text/icon foreground,
  or a graph/chart line; it's a no-op on a node that has no colour (e.g. a `row`).
- **Colours are theme tokens** (`Token::{Fg,FgDim,Accent,Ok,Warn,Urgent,Bg}`) or
  `Paint::Rgba(..)`. Prefer tokens so the chip respects the user's theme.
- **Icons** are the host set (`Icon::{Cpu,Cloud,Github,Spotify,…}`). The view call
  is `Icon::Cloud.view(size_px, color)` — size first, then colour. **Graph** is the
  host sparkline over your `values`; `GraphKind` only hints the host's auto-scaling
  (`Generic` = min/max of your data — a fine default; `Percent`/`Temperature` pin a
  domain), it does **not** change colour — set that with `line`.

## Interactivity

`mouse_area` is **only** for handling your own clicks/scroll — it is *not* needed
for the hover popup (the host already hovers the whole chip; just implement
`popup()`). Wrap a region in `mouse_area("id", child)`, and the host sends
`Event::Pointer { id, kind, delta }` to `update`; match on `id` and your
`PointerKind` (Press/RightPress/Scroll/Enter/Leave), mutate state, return `true`.

## Capabilities — how network access actually works today

`ctx.http_get` is **capability-gated**: it only works if the *user* grants the
host. **Today the grant lives in the user's `~/.config/ezbar/config.toml`**, keyed
by your plugin id (the `.wasm` file stem):

```toml
# the user adds this to grant your "weather.wasm" plugin network access:
[modules.weather]
network = "api.open-meteo.com"        # one host, or an array of hosts
lat = "52.52"                          # any [modules.<id>] keys reach your load()
```

Without that line, `ctx.http_get` returns `Err("capability denied: …")` (sandboxed
by default). The host checks the URL's host against the grant before dialing
(case-insensitive, port-agnostic). There is **no `exec`** — a plugin that needs a
subprocess is a `custom` script, not a sandboxed plugin.

The other capabilities are granted the same way, keyed by your plugin id:

```toml
[modules.sysgraph]
feeds = ["cpu", "net"]   # ctx.feed_subscribe(...) for these kinds (cpu/memory/temperature/battery/net)

[modules.wintitle]
sway = true              # ctx.sway_snapshot() — read-only workspace list + focused title

[modules.notes]          # the fs tier (RFC 0015): preopen dirs into your guest, use std::fs
fs = [{ path = "~/notes", at = "/notes", mode = "rw" }]   # mode: r (default) | rw

[modules.kube]
exec = ["kubectl"]       # ctx.exec("kubectl", &["config","current-context"], None) — any args
```

`ctx.exec(program, args, stdin) -> ExecOutput { code, stdout, stderr }` runs an allow-listed
program to completion (gated by `exec`; `Err` if not granted). Keep it on the timer path —
your guest is parked while it runs. See `wasm/kube` for the worked example.

An ungranted `feed_subscribe` is silently never delivered (fire-and-forget — don't
busy-wait on it); an ungranted `sway_snapshot()` returns `Err` (synchronous denial,
like `http_get`). With `fs`, you use **normal `std::fs`** against the guest mount (`/notes`
above) and WASI jails you there — an ungranted/escaping path just fails like any missing file.

`fs` (write) and `exec` are a **dangerous tier**: a user must grant them by hand (`ezbar add`
won't auto-activate them), or flip `[plugins] yolo = true` to grant every plugin everything.
Either way the *resource* sandbox (cpu/mem/epoch) still holds.

### Declare what you need — `ezbar-plugin.toml` + `ezbar package`

Ship a sidecar `ezbar-plugin.toml` declaring the capabilities your plugin needs, and embed
it into the `.wasm` with the producer step (RFC 0014):

```toml
# ezbar-plugin.toml — beside your built .wasm
id = "weather"
name = "Weather"
version = "1.2.0"
wit = "0.2.0"                 # the WIT version you built against
description = "Forecast chip with an hourly/daily hover panel."
[capabilities]
network = ["api.open-meteo.com", "wttr.in"]
feeds = []
sway = false
```

```sh
ezbar package weather.wasm        # embeds ezbar:manifest, prints the registry entry + sha256
ezbar inspect weather.wasm        # verify: prints the declared caps + the grant block users paste
```

The host **reads** the embedded manifest at load: if your plugin declares a capability the
user didn't grant in `[modules.<id>]`, it logs a clear warning (so an inert widget explains
itself) instead of failing mute. The manifest is a **declaration, not an authority** — the
`[modules.<id>]` grant above is still the enforced gate (per-call, and bound to the wasm's
**content hash**: a user re-approves a changed binary with `ezbar grant <id>`). Still
document the grant lines your plugin needs in your README so users know what to paste.

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

## Writing it in Go (TinyGo)

Same plugin model, a Go SDK (`github.com/birdayz/ezbar/go/ezbar`) that mirrors the
Rust one. The whole plugin is a `Plugin` impl + one `ezbar.Register(...)` call.

1. **Scaffold** (writes `go/examples/<name>/main.go` + a README):
   ```bash
   go/new-plugin.sh <name>
   ```

2. **Toolchain** (one-time): TinyGo with a `wasip2` target (`tinygo targets | grep
   wasip2`). TinyGo typechecks with the Go toolchain and currently supports **Go ≤
   1.24** — if your system `go` is newer, front a 1.24 SDK on PATH for the build:
   ```bash
   go install golang.org/dl/go1.24.4@latest && go1.24.4 download
   # then build with: GOROOT=$HOME/sdk/go1.24.4 PATH=$GOROOT/bin:$PATH …
   ```

3. **Build to a component.** The scaffold writes a `build.sh` that auto-fronts a
   ≤1.24 Go SDK (step 2), runs `gofmt`/`go vet`, and builds — so just:
   ```bash
   go/examples/<name>/build.sh
   ```
   Under the hood it runs (from the plugin dir):
   ```bash
   tinygo build -target=wasip2 -o <name>.wasm --wit-package ../../wit --wit-world plugin-guest .
   ```
   `../../wit` is the shared guest world that unions the WASI imports TinyGo's
   runtime needs with the ezbar plugin world — you never touch it. The generated
   bindings under `go/internal/` are shared infra too; you only write `main.go`.

4. **Preview** the same way (`cargo run -p ezbar-wasm --example preview -- … [--check]`).

The plugin itself:

```go
package main

import "github.com/birdayz/ezbar/go/ezbar"

type Clock struct{ ezbar.Base; now string }   // embed Base for no-op defaults

func (c *Clock) Update(ctx ezbar.Ctx, ev ezbar.Event) bool {
    if ev.Kind == ezbar.EvTimer { c.now = "12:34"; ctx.SetTimeout(10_000); return true }
    return false
}
func (c *Clock) View() ezbar.Render {
    return ezbar.Row(ezbar.IconClock.View(14, ezbar.FgDim), ezbar.Text(c.now)).Spacing(5)
}

func init() { ezbar.Register(&Clock{}) }   // the only glue
func main()  {}                            // required, stays empty
```

The API maps 1:1 to Rust: `ezbar.Text/Row/Column/Container/MouseArea/Spacer`,
`ezbar.IconCloud.View(size, color)`, `ezbar.Graph{…}.View()`, `ezbar.Chart{…}`,
theme colours as values (`ezbar.Fg/Accent/Warn/…`, `ezbar.RGBA(r,g,b,a)`), and
`ctx.HTTPGet/Log/SetTimeout`. Only `View` is required — embed `ezbar.Base` for the
rest. `go.mod` already depends on the SDK; add your own deps (e.g. `encoding/json`)
as usual. No wit-bindgen, no glue — `ezbar.Register` wires the component exports.

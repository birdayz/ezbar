# RFC 0006: WASM plugins

- **Status:** Draft (v1 — for review)
- **Created:** 2026-06-02
- **Target:** ezbar (Rust / iced / wlr-layer-shell)
- **Depends on:** RFC 0001 (the `Module` trait), RFC 0004 (reactive config pipeline)
- **Prior art studied:** Zellij (MIT — wasm plugins that render UI), Zed (Apache-2.0 `zed_extension_api` — Component-Model extensions + interface versioning)

## Summary

Let third parties ship **WASM plugins**: a single `.wasm` dropped into
`~/.config/ezbar/plugins/` is **auto-loaded, sandboxed, hot-reloadable, and
crash-isolated**, written in **any language that targets WebAssembly** (Rust
first, with a first-class SDK). A plugin is a `Module` like any built-in — it
owns its state, an event-driven update loop, and a chip + popup — but it runs
inside a wasmtime sandbox and **describes** its UI through a typed `widget`
surface that the host renders with real iced. The author writes against
`ezbar_plugin::widget` (text/row/column/container/mouse-area + our `Icon` and
`Graph` components); the host owns final theming and rendering.

This is the **safe, portable, marketplace-grade** plugin tier. It deliberately
does **not** carry a live `iced::Element` or custom GPU canvas across the
sandbox — that is physically impossible (a sandbox has no shared host heap or
renderer) and is the job of a future **native dlopen tier** (RFC 0007). The two
are complementary: WASM for untrusted/portable/any-language plugins, dlopen for
trusted/native/Rust-only plugins that need raw `iced`.

## Motivation

ezbar's identity is "a module is just iced." That is wonderful for *built-in*
modules and will stay the contract for the native tier. But a real ecosystem
needs plugins that a user can install **without trusting the author with their
whole session** and **without recompiling against every ezbar release**. Native
dynamic linking (dlopen) can't give that: Rust has no stable ABI, so a native
plugin must be recompiled in lockstep with the host and a panic/segfault in it
takes the bar down. That's fine for a power user building their own widget; it's
unacceptable for "install a stranger's weather applet."

WASM inverts every one of those weaknesses:

| Property | native dlopen | **WASM (this RFC)** |
|---|---|---|
| Crash isolation | a panic/segfault kills the bar | a trap disables one plugin |
| Hot-reload | unsafe (`dlclose` UB) | trivial (re-instantiate) |
| Portability | recompile per ezbar release | one artifact, runs across releases (versioned WIT) |
| Author language | Rust only | any wasm target (Rust, Go/TinyGo, C, Zig, JS) |
| Untrusted code | no | yes — capability-sandboxed |
| Custom GPU canvas | yes | no (host renders a bounded vocabulary) |
| Live `iced::Element` | yes | no (a description, host-rendered) |

The trade is: WASM can't share the live widget tree, so plugins emit a *typed
description* of their chip and the host renders it. For a status-bar chip
(text + icon + graph + a popup list) that vocabulary is ~complete, and the host
staying the sole renderer is a *feature* — themes apply uniformly and a plugin
can't draw outside its lane.

No status bar ships a sandboxed plugin system today (waybar/polybar = shell
scripts emitting JSON; eww/AGS = a config DSL). Zellij and Zed prove the
"Rust host + wasm plugins" model ships and scales. This makes ezbar the first
bar with a real plugin ecosystem.

## Design

### 0. The shape of a plugin

Identical Elm loop to a built-in `Module`, minus the parts that can't cross a
sandbox. The Rust SDK (`ezbar-plugin-wasm`) presents:

```rust
use ezbar_plugin_wasm::{plugin, Plugin, Ctx, Event, Render};
use ezbar_plugin_wasm::widget::{row, text};   // the facade — see §2
use ezbar_plugin_wasm::{Icon, Graph};         // our components

struct Weather { temp: Option<f32>, dirty: bool }

impl Plugin for Weather {
    fn load(&mut self, cfg: Config) { /* subscribe(), schedule a poll */ }

    // returns `true` if the chip needs re-rendering (Zellij's dirty bool)
    fn update(&mut self, ev: Event) -> bool {
        match ev {
            Event::Timer => { /* host async-fetch in a task */ true }
            Event::Pointer(p) if p.id == "chip" => { /* open popup */ true }
            _ => false,
        }
    }

    fn view(&self, ctx: &Ctx) -> Render {            // build the widget description
        row(( Icon::Cloud.view(ctx.text_size(), ctx.fg()),
              text(self.label()) )).spacing(5).into()
    }

    fn popup(&self, ctx: &Ctx) -> Option<Render> { None }
}

plugin!(Weather);   // emits the component exports + embeds the ABI version
```

The author never sees WIT, wit-bindgen output, or the serialization. `plugin!`
generates the Component-Model exports and embeds the SDK's api-version (§4).

### 1. Runtime: wasmtime + Component Model + WASI P2

- **Engine: wasmtime** with the **WebAssembly Component Model** and **WASI
  Preview 2**, bindings via `wasmtime::component::bindgen!` (host) and
  `wit-bindgen` (guest). This is exactly Zed's stack and it is the only mature
  CM toolchain. The Component Model buys us three things that matter more than
  raw speed: **`resource` types** (handles for `Ctx`, host services),
  **language-agnostic guests** (WIT, not Rust types), and the **versioning
  scheme** in §4.
- **Why not wasmi (Zellij's choice)?** wasmi is an interpreter — no JIT, no
  on-disk cache, smaller host, instant reload — attractive for a lightweight
  bar. But its Component-Model support lags, and we want CM for versioning +
  polyglot. We keep wasmtime's startup cost down with the **module compilation
  cache** (compile each plugin once, cache the artifact keyed by content hash)
  and the **pooling allocator**. If binary size or cold-start ever bites, wasmi
  is the documented fallback; the WIT contract is engine-independent.
- **Async host imports.** All host functions are added with
  `add_to_linker_async`, so a plugin that fetches over HTTP or reads `/sys`
  does not block the bar's render/event loop — the call suspends the plugin's
  store and resumes it on the host executor (the same tokio the bar already
  runs).

#### 1a. Robustness — a plugin must never hang or OOM the bar

This is non-negotiable for code on the render path, and it's where naive wasm
embeddings fail:

- **Epoch interruption.** The host bumps a wasmtime *epoch* on a fixed timer
  (e.g. 10 ms). Every plugin `Store` has an epoch deadline; a guest call that
  overruns **traps** instead of spinning. `view()` gets a tight deadline (it
  must be pure and fast); `update()`/async work gets a looser one. A runaway
  plugin disables itself; the bar never stalls. (Epoch over fuel: we want a
  *wall-clock* bound, and epoch is near-zero overhead.)
- **Per-store limits.** Each plugin instance gets a `StoreLimits` cap (memory
  e.g. 64 MiB, table elements, instance count). Exceeding → trap → disabled.
- **Trap = contained.** Any trap (epoch, OOM, `unreachable`, a failed host
  call the guest didn't handle) disables that one plugin and swaps its chip for
  an error chip — reusing the harness's existing panic-containment behaviour.
  The rest of the bar is untouched.
- **No wasm threads.** WASI threads aren't stable; a plugin offloads blocking
  work by *asking the host* (async imports) or via a `set_timeout`, never by
  spawning. (Zellij hit this and bolted on a "worker" instance; we avoid it by
  making host I/O async from day one.)

### 2. The UI boundary: a typed `widget` description, host-rendered

A sandbox cannot hand back an `iced::Element`. So `view()` returns a **`Render`**
— a tree of WIT `widget` nodes — and the host maps each node to a real iced
widget every time it re-renders. This is Zellij's structured-component model
(`Text`/`Ribbon`/`Table` over a byte stream) done with typed CM values instead
of terminal escapes.

```wit
// wit/since-v0.1.0/ui.wit  (sketch)
variant widget {
    text(text-node),
    row(layout-node),
    column(layout-node),
    container(box-node),
    mouse-area(hit-node),         // carries an author-chosen hit-id
    icon(icon-node),              // our SVG set, host-rendered + host-tinted
    graph(graph-node),            // our sparkline component, host-drawn on GPU
    marquee(marquee-node),
    spacer(f32),
}
record text-node   { content: string, style: text-style }
record icon-node   { id: icon-id, color: paint, size: f32 }
record graph-node  { values: list<f64>, kind: graph-kind, line: paint }
// paint = a THEME TOKEN (fg/accent/ok/warn/urgent) or an rgba — host themes it
variant paint { token(theme-token), rgba(tuple<u8,u8,u8,u8>) }
```

Key properties:

- **Colours are theme references, not raw pixels** (a `paint` is a token or an
  rgba). The host applies the user's theme, so a plugin written for a dark theme
  looks right on a light one — and our `Icon`/`Graph` render with the host's
  real GPU pipeline, tinted by the host. Plugins describe *intent*; the host owns
  *look*. (Straight from Zellij's "defer styling to the host.")
- **Bounded vocabulary, on purpose.** The node set is the status-chip vocab
  (text, layout, container, mouse-area, our components, marquee). No arbitrary
  custom widget, no `canvas::Program`, no shaders — those are native-tier only.
  This bound is what keeps the host the sole, safe renderer.
- **Events via hit-ids.** `mouse-area(hit-id)` tags an interactive region; the
  host sends `Event::Pointer { id, kind: press|scroll|enter|leave, delta }` back
  to `update()`. The plugin maps `id → its own message` internally. The
  plugin's message type stays *inside* the sandbox (never crosses), so the Elm
  loop works without the host knowing the plugin's `Msg` (cf. RFC 0001's
  `ModMsg`, but the boundary is now serialization, not `Any`/`TypeId`).
- **Popup** is a second optional `Render` tree, rendered into the existing popup
  surface (RFC 0004 owns popup placement already).
- **Re-render is pull + dirty.** The host calls `view()` only when `update()`
  returned `true` (or on a theme/output change) — never every frame. So the
  serialize→translate cost is paid on state change, not at frame rate. At
  status-bar update rates this is free.

#### 2a. `ezbar_plugin::widget` — same source, two backends

The facade is the bridge between "write iced" and "ships as wasm":

- **native build** (`cfg(not(target_arch = "wasm32"))`): the facade is
  `pub use iced::widget::*` and `Render = iced::Element`. Zero overhead, real
  iced — this is what the future dlopen tier compiles.
- **wasm build**: the same builder API constructs the WIT `widget` tree;
  `Render` is the serialized description.

One author source. `row((a, b)).spacing(5)` is real iced natively and a
serialized `widget::row` on wasm. `Icon`/`Graph` are real iced widgets natively
and `widget::icon`/`widget::graph` opcodes on wasm — and the host renders our
*real* components either way (GPU-accelerated). That is the honest maximum of
"iced-native" a sandbox allows, and it's the same answer Zed/Zellij landed on:
the guest describes, the host renders.

### 3. Host services (the imports), capability-gated

Plugins reach the world only through host imports, every one **gated by a
declared capability** (§5). WASI P2 supplies the primitives; we wrap them in an
ergonomic, gated `Ctx`:

- `ctx.http(req).await` — outbound HTTP (wasi-http), gated `network { host }`.
- `ctx.run(cmd, args).await` — spawn a command, gated `exec { command, args }`.
- `ctx.read_file(path).await` — gated `read-file { path }` (no ambient FS).
- `ctx.feed(Feed::Cpu)` — subscribe to a host-provided **data feed**
  (cpu/mem/temp/net/battery) so a plugin can chart system metrics without any
  capability beyond `bar-state` — the host already samples these.
- `ctx.set_timeout(d)` / `ctx.subscribe(&[EventKind])` — the event loop.
- `ctx.theme()` / `ctx.text_size()` / `ctx.output()` — read-only render context.

Host imports are **async** (§1) and run off the render thread.

### 4. Interface versioning — no recompile-per-release (Zed's scheme, adopted)

The single most important thing we copy from Zed. We **never break an installed
plugin**; we evolve the interface by *freezing* old versions and adapting them
forward.

1. **Frozen `since-vX` WIT directories.** `wit/since-v0.1.0/`,
   `wit/since-v0.2.0/`, … A released directory is **never edited**. A new
   version is a copy + edit.
2. **Host compiles every version** with `bindgen!` into its own module and
   wraps them in one dispatch enum:
   ```rust
   enum LoadedPlugin { V0_2_0(Instance<…>), V0_1_0(Instance<…>) }
   ```
   Each host-facing call is a `match` over the enum.
3. **Forward-adaptation.** For an old variant, the host lifts the old
   record/return into the current internal type (`old.into()`), e.g. a v0.1.0
   `widget` that lacked `marquee` maps onto the current `widget` losslessly. A
   two-release-old `.wasm` keeps running, untouched.
4. **api-version is auto-derived, never hand-written.** The `ezbar-plugin-wasm`
   crate version is embedded in a custom wasm section (`ezbar:api-version`) and
   stamped into the plugin manifest at build time. *Bumping the SDK dependency
   is the opt-in to a new ABI; doing nothing pins you.* (Zed's exact trick.)
5. **Channel staging.** Stable ezbar accepts `since-v0.1.0 ..= last_stable`;
   nightly also accepts the in-development WIT, so a new interface can be
   exercised before it's frozen into stable.
6. **Batch breaking changes.** A `rfcs/wasm-PENDING_CHANGES.md` accumulates
   "vNext" so each new `since-vX` (a permanent dispatch arm forever) is rare,
   and the SDK README carries an ezbar-version ↔ api-version table.

The honest asymmetry: an **old plugin on new ezbar** always works (we adapt
forward); a **new plugin on old ezbar** does not (it declares an api-version the
old host doesn't know) — and the host says so clearly instead of loading it.

### 5. Capabilities & trust — designed in, not bolted on

Zellij's hardest-won lesson: they shipped plugins first and retrofitted
permissions as a breaking change that *still* can't grant perms to background
plugins. We define the capability model **before** v1.

- **Declared in the manifest**, pattern-matched (Zed's model, `*`/`**`):
  ```toml
  # ezbar-plugin.toml
  id = "weather"
  name = "Weather"
  version = "0.1.0"
  # api_version is injected at build time — do NOT hand-write it

  [[capabilities]]
  kind = "network"          # outbound HTTP
  host = "api.open-meteo.com"

  [[capabilities]]
  kind = "bar-state"        # read host data feeds
  feeds = ["cpu", "mem"]
  ```
  Kinds: `network { host }`, `exec { command, args }`, `read-file { path }`,
  `bar-state { feeds }`, `clipboard`, `notify`. Each has an `allows()` matcher
  (~30 lines, lifted from Zed's `ProcessExecCapability`).
- **User consent on first load**, prompted in a popup (Zellij's
  `request_permission`), then **persisted in a grant cache keyed by the
  plugin's wasm content hash** — so a *changed* binary re-prompts (you can't
  silently swap a benign plugin for a malicious update). A user setting
  `granted_capabilities` can pre-grant or hard-deny (`[]` = sandbox-only).
- **Enforcement at the import boundary.** A gated host call with no matching
  grant traps the call; a gated event isn't delivered. Capability == event gate.
- **Signatures (the "safety later").** v1 verifies a **content hash** on
  install. A follow-up adds optional **author signatures** (sign the `.wasm`,
  host verifies a trusted key before load) — the natural home for a curated
  registry. Because grants are keyed by hash, signing slots in cleanly.

Note the dlopen tier (RFC 0007) can't enforce any of this — a native cdylib has
full process rights. There, capabilities are advisory + signature-gated *trust*.
That asymmetry is exactly why untrusted plugins must be WASM.

### 6. Loading, discovery, hot-reload

- **Discovery:** scan `~/.config/ezbar/plugins/*.wasm` (+ a `plugins = [...]`
  config list, and `file:`/`https:` URLs). A remote plugin is downloaded
  **atomically** (temp + rename) and **hash-verified** — fixing the concurrent-
  download corruption bug Zellij documents.
- **Auto-load:** a `.wasm` appearing in the dir is loaded on the next config
  tick — this *is* RFC 0004's reactive pipeline; a plugin is just another
  placeable module id.
- **Hot-reload:** an mtime/inotify change re-instantiates the component (cheap
  and safe in wasm — no `dlclose` hazard), with optional state hand-off via
  `save_state() -> list<u8>` / `restore(state)`. `ezbar plugin reload <id>`
  forces it. This is the headline DX win Zellij ships; we get it for free atop
  RFC 0004.

### 7. Distribution

No central registry at first (Zellij's pragmatic path): a plugin is a `.wasm` +
`ezbar-plugin.toml`, shipped as a **GitHub Release artifact** (a provided GitHub
Action builds `wasm32-wasip2` + uploads), discovered via a curated
**`awesome-ezbar`** list, installed by URL or drop-in. Every install is
hash-verified. A signed registry is a later layer, not a v1 blocker.

### 8. The Claude skill (a first-class deliverable)

Ship `ezbar-wasm-plugin-author` — a skill that teaches Claude to scaffold, write,
build (`cargo build --target wasm32-wasip2`), capability-declare, and harness-test
a plugin. The SDK's narrow surface (`Plugin` trait + `widget` facade + a handful
of `Ctx` calls) is deliberately LLM-shaped: small, typed, example-dense. "Vibe-
code me a crypto-price chip that polls Coinbase and turns red on a 5% drop"
should be a one-shot. This is half the point of the bounded vocabulary.

## What this RFC explicitly does NOT do

- **No live `iced::Element` across the sandbox, no custom `canvas`/GPU
  shaders.** Physically impossible in a sandbox; that's the native dlopen tier
  (RFC 0007). WASM plugins use the bounded `widget` vocab + our components.
- **No GPU-in-wasm.** Even with experimental wasi-webgpu a plugin could only
  render an isolated texture, not join the host tree — all the integration cost,
  none of the nativeness, and it weakens the sandbox. Our `Graph`/`Icon` are
  already host-GPU-rendered in this design, so there's no need.
- **No wasm threads.** Async host imports + timers instead.

## Phasing

1. **PoC (this branch):** wasmtime host harness; one `since-v0.1.0` WIT with
   `text`/`row`/`icon`; a `hello`/`weather` plugin in Rust → `.wasm`,
   drop-in-loaded, rendering a real chip; epoch-trap + capability-deny demoed.
2. **SDK + facade:** `ezbar-plugin-wasm` crate, `widget` facade, `plugin!`
   macro, `Ctx` services, `bar-state` feeds.
3. **Versioning + capabilities + grant UI.**
4. **Hot-reload on the RFC-0004 pipeline; distribution action; the Claude
   skill.**
5. **(Later, RFC 0007)** native dlopen tier sharing the same `widget` facade in
   its native form.

## Open questions

1. **Serialization format across the boundary** — lean on the Component Model's
   canonical ABI (typed `widget` values via wit-bindgen) vs a hand-rolled flat
   buffer (Zellij uses protobuf-over-pipe and regrets the overhead). CM-native
   is cleaner; measure it against a flat buffer if `view()` of a large popup
   shows up in a flamegraph.
2. **Module cache location & invalidation** — `$XDG_CACHE/ezbar/wasm/<hash>`;
   invalidate on wasmtime version change.
3. **Feed granularity** — which host data feeds to expose (cpu/mem/temp/net/
   battery/disk) and at what poll rate the plugin can request.
4. **Per-output plugin placement** — pairs with RFC 0004's per-output surfaces;
   a plugin on the laptop panel but not the 5120 monitor.
5. **Resource budgets** — default epoch deadlines and memory caps; do we expose
   them as `[plugins]` knobs or keep them fixed?
6. **Signature trust model** — TOFU, a keyring, or a registry-issued signature;
   deferred but shape it so §5's hash-keyed grants extend to it.

# RFC 0006: WASM plugins

- **Status:** **Accepted** — ACK'd by a wasm-embedding review (Rockwood) and a systems review (Torvalds); v2.1 nits folded below
- **Created:** 2026-06-02
- **Target:** ezbar (Rust / iced / wlr-layer-shell)
- **Depends on:** RFC 0001 (the `Module` trait + its dlopen/JSON alternatives), RFC 0004 (reactive config pipeline)
- **Prior art studied:** Zellij (MIT — wasm plugins that render UI), Zed (Apache-2.0 `zed_extension_api` — Component-Model extensions + interface versioning)

## Review response (what changed in v2)

This RFC was NAK'd/changes-required on first pass. v2 resolves every blocking item:

- **One plugin system, not two.** WASM is *the* third-party plugin tier. Native
  dynamic linking is **not** a co-equal maintained product (RFC 0001 already
  established dlopen is a same-toolchain pin, not a real ABI). Widgets that need
  raw `canvas`/GPU are **compile-in or fork** — see *Alternatives*. (Torvalds #1)
- **The facade is not "iced".** It's `ezbar_plugin::widget`, an honestly-named,
  **bounded UI vocabulary that lowers to real iced** (the host renders it).
  Reaching for `iced::widget::canvas` in a plugin is a **compile error on the
  wasm target** — the facade doesn't expose it. (Torvalds #2)
- **`exec` is cut.** No arbitrary host-command execution from sandboxed code.
  `clipboard`/`notify`/`marquee` are cut from v1 too. (Both reviewers)
- **We argue against our own cheaper alternative.** A new *Alternatives* section
  confronts RFC 0001's "JSON-over-stdout / extend `custom`" path head-on and
  leads with the only thing wasm uniquely buys: **crash/hang/OOM isolation**.
  (Torvalds #4)
- **Versioning is bounded.** Support the last *N* WIT versions; refuse-and-explain
  an out-of-range plugin. No infinite-history obligation. (Torvalds #5)
- **Render-path safety is corrected and made real.** `view()` runs **off the GUI
  thread** on the plugin's own task, producing a **cached** lifted tree; the iced
  view pass only ever reads that cache. A render-deadline **trap is terminal for
  the instance** (poisoned store, state lost, error chip, never re-entered). A
  **host-side node-count + depth cap** is enforced during the canonical-ABI lift,
  independent of epoch/limits. Memory is bounded **per-store (small) and in
  aggregate**. Every async host import carries **its own timeout** (epoch does
  not fire while a guest is parked in a host future). (Rockwood #1,#2,#3)
- **Capabilities are enforced by linker absence, not just argument checks.**
  Ungranted capability ⇒ the host import **is not added to the linker**. No raw
  `wasi:filesystem`/`wasi:http` is exposed to guests — only narrow custom imports
  the host executes after checking the grant. Grants are keyed by
  `hash(wasm ‖ manifest)`. (Rockwood #4)
- Honesty fixes: wasmi is **not** a drop-in fallback for the safety story; the
  module cache uses `Module::deserialize` (`unsafe`, a trust boundary); the
  pooling allocator is **not** a v1 default. (Rockwood nits)

### v2.1 — review nits, folded as binding design commitments

Both reviewers ACK'd v2 and listed implementation-phase nits. These are now design:

- **Node/depth cap is enforced *incrementally during* the lift** (not lift-then-
  count — the count must not itself be the DoS), **covers the popup tree**, and
  is counted on the **post-adaptation** internal tree (an old→new `From` lift can
  fan out nodes). (Rockwood)
- **One hardening invariant:** hash the `.wasm` first; the source-hash **gates the
  module-cache lookup**; the `ezbar:api-version` custom section is parsed with
  **wasmtime's bounded section reader** *after* hashing — never a hand-rolled
  leb128 decode on attacker bytes before validation. (Rockwood)
- **`ctx.feed` coalescing is one host timer fanning out to N task channels**, not
  a per-task `set_timeout` per plugin. (Rockwood)
- **Resource budgets are fixed constants in v1**, not `[plugins]` knobs — this
  resolves Open Question #2. Don't ship five tunables for zero users. (Torvalds)
- **`hash(wasm ‖ manifest)` is domain-separated / length-prefixed**, not naive
  byte concatenation (hash-confusion footgun). (Torvalds)
- **One source of truth for the api-version** — the `plugin!` macro and the
  `ezbar:api-version` custom section derive from the same SDK-crate constant.
  (Torvalds)
- **Channel-staging + `wasm-PENDING_CHANGES.md` stay trivial / deferred** until
  there's a second WIT version to stage (process-ahead-of-demand). (Torvalds)

## Summary

Let third parties ship **WASM plugins**: a single `.wasm` dropped into
`~/.config/ezbar/plugins/` is **auto-loaded, sandboxed, hot-reloadable, and
crash/hang/OOM-isolated**, written in **any language that targets WebAssembly**
(Rust first, with a first-class SDK). A plugin is a `Module` like any built-in —
it owns its state, an event-driven update loop, and a chip + popup — but it runs
in a wasmtime sandbox and **describes** its UI through a typed `widget`
vocabulary that the host renders with real iced and themes uniformly.

This is the **only** third-party plugin tier. It cannot carry a live
`iced::Element` or a custom GPU `canvas` across the sandbox (physically
impossible), and it doesn't try to. Widgets that need raw canvas/GPU are
compiled in or forked — a deliberately small escape hatch, not a second product.

## Motivation

ezbar ships `custom.rs` today: a module that runs a command on an interval and
renders its stdout. That's the waybar model, and for *most* of what a status
chip does — text, an icon, a number that changes color — it's enough, in any
language, with zero toolchain. RFC 0001's own *Alternatives* section says as
much: a JSON-over-stdout protocol would serve most widgets "with zero ABI risk
and full language freedom." Any plugin RFC has to beat that bar or it's
complexity for its own sake.

So the honest question is narrow: **what does a sandboxed wasm plugin do that a
richer `custom` script cannot?** Exactly three things, and only these are the
justification:

1. **Crash/hang/OOM isolation.** A script that wedges, leaks, or segfaults its
   interpreter is the user's problem to notice; the bar can't bound it. A wasm
   guest that spins, allocates forever, or traps is **bounded by the host** —
   epoch-interrupted, memory-capped, and disabled without touching the bar. This
   is the one thing a script fundamentally cannot match, and it is the headline.
2. **A graph from arbitrary author data.** A script can print text; it cannot
   hand the bar a time series and get our GPU sparkline. A wasm plugin emits a
   `graph` node over real data and the host draws it with the same component the
   built-ins use.
3. **Capability-sandboxed untrusted code.** "Install a stranger's weather chip"
   is safe only if the thing can't read your SSH keys. A script has the user's
   full ambient authority; a wasm guest has **only the capabilities it declared
   and the user granted**.

Everything else wasm is sometimes sold on — "polyglot!" — is weaker than it
sounds: the real baseline isn't "Rust only," it's "any language that can write
to stdout." We claim polyglot honestly (you get a typed SDK and structured UI,
not just a print statement), but it is not the reason to build this. The reason
is isolation.

## Design

### 0. The shape of a plugin

Same Elm loop as a built-in `Module`, minus what can't cross a sandbox. The Rust
SDK (`ezbar-plugin-wasm`) presents:

```rust
use ezbar_plugin_wasm::{plugin, Plugin, Ctx, Event, Render};
use ezbar_plugin_wasm::widget::{row, text};   // the DSL — see §2a (NOT raw iced)
use ezbar_plugin_wasm::{Icon, Graph};          // our components, host-rendered

struct Weather { temp: Option<f32> }

impl Plugin for Weather {
    fn load(&mut self, cfg: Config) { /* subscribe(); schedule a poll */ }

    // returns `true` if the chip needs re-rendering (Zellij's dirty bit)
    fn update(&mut self, ctx: &Ctx, ev: Event) -> bool { /* … */ true }

    // PURE + synchronous: no host imports, no await. Builds the description.
    fn view(&self, ctx: &Ctx) -> Render {
        row(( Icon::Cloud.view(ctx.text_size(), ctx.fg()), text(self.label()) ))
            .spacing(5).into()
    }

    fn popup(&self, ctx: &Ctx) -> Option<Render> { None }
}

plugin!(Weather);   // emits the component exports; embeds the SDK's api-version
```

`update()` may call **async** host services (`ctx.http(...).await`); `view()`
may **not** — it is pure, synchronous, fast, and epoch-bounded (§2). The author
never sees WIT, wit-bindgen, or the serialization.

### 1. Runtime: wasmtime + Component Model + WASI P2

- **Engine: wasmtime** + the **Component Model** + **WASI Preview 2**, host
  bindings via `wasmtime::component::bindgen!`, guest via `wit-bindgen`. This is
  Zed's production stack. We choose it for `resource` handles, polyglot guests,
  and the versioning scheme (§4) — *not* speed.
- **We do not expose raw WASI worlds to guests.** No `wasi:filesystem`, no
  `wasi:http` in the guest linker. The only file/network access is through
  **narrow custom host imports** (`ctx.read_file`, `ctx.http`) that the host
  executes and gates (§3, §5). This keeps the capability boundary enforceable
  (a guest can't construct a request to a host it wasn't granted) and decouples
  our ABI from wasi-http's churn.
- **Async host imports** via `add_to_linker_async`, run on the host's tokio.
  See §2 for the threading model that makes this sound.
- **wasmi is not a drop-in fallback.** Its Component-Model and epoch/limits story
  is not equivalent to wasmtime's, and our entire robustness model (§1a) is built
  on wasmtime's epoch + `StoreLimits`. "Switch to wasmi for size/startup" means
  *re-engineering the safety story*, not recompiling. We stay on wasmtime; the
  WIT *interface* is engine-independent, the *safety implementation* is not.
- **Startup cost** is paid once via the module cache (§1a), not by switching
  engines.

#### 1a. Robustness — a plugin must never stall, OOM, or crash the bar

The hard part, and where v1's claims were wrong. Corrected:

- **`view()` runs off the GUI thread; the iced view pass only reads a cache.**
  A wasmtime `Store` is `!Sync` and not re-entrant mid-call, and you cannot
  `await` inside iced's synchronous `view`. So each plugin lives on **its own
  async task** with its store; the host↔plugin boundary is a channel. When
  `update()` returns dirty, the plugin task runs `view()` (pure, sync,
  epoch-bounded), the host **lifts the result once and caches** the translated
  `iced::Element` (actually a cheap retained description; see §2). iced's view
  pass renders the **cached** tree — it never calls into a store. This is the
  crux the v1 draft left unspecified.
- **Epoch interruption, gated and coarse.** A host ticker bumps a wasmtime epoch
  **only while ≥1 plugin is loaded**, at a coarse period (25–50 ms) — we don't
  add an unconditional 100 Hz wakeup to a battery-powered bar. `view()` gets a
  1-tick deadline; `update`/compute gets a looser one. A guest that overruns
  **traps**.
- **Epoch does not bound host calls.** While a guest is parked at
  `ctx.http().await`, no wasm runs, so epoch can't fire. Therefore **every async
  host import wraps its own `tokio::time::timeout`** — a hung server cannot park
  a plugin forever. (This is the classic wasmtime footgun; v1 hand-waved it.)
- **A trap is terminal for that instance.** A store that trapped mid-call has
  undefined guest memory (a half-built tree, a mid-mutation allocator). The host
  **disables the plugin, shows an error chip, keeps the last-good cached tree
  until then, and never re-enters that store.** `save_state()` is unavailable
  after a trap → **state is lost on a render/OOM trap** (clean reloads keep
  state; crashes don't — stated honestly). Recovery is a fresh instance on a
  later tick, off the frame budget. *(Note: the host itself does not yet contain
  `view()` panics for built-ins — `src/main.rs:665-668` — recovery there is the
  launcher respawn. The wasm tier is strictly better: a guest trap is caught at
  the host trampoline and contained to one chip.)*
- **A host-side bound on the returned tree, enforced during the lift.** Epoch
  bounds a guest that *spins*; it does **not** bound a guest that *returns* a
  10-million-node `column` the host then melts decoding. The lift enforces a
  **node-count + depth cap** (e.g. 2 000 nodes / depth 32) and rejects an
  over-limit tree as a trap. This is a distinct DoS surface from epoch/limits.
- **Memory is bounded per-store and in aggregate.** Per-store default is
  single-digit MiB (a weather chip needs ~nothing), not 64. A host-wide
  **aggregate budget** across all plugin stores prevents N plugins × cap from
  eating the desktop.
- **Module cache is a trust boundary.** Compiled artifacts live in
  `$XDG_CACHE/ezbar/wasm/<hash>`, created `0600` in a host-owned dir.
  `Module::deserialize` is **`unsafe`** and trusts its bytes; we use wasmtime's
  built-in compatibility check and treat any mismatch/parse failure as
  "recompile," and we still verify the source `.wasm` hash. Not "just a perf
  detail."
- **On-demand allocator, not pooling, for v1.** The pooling allocator
  pre-reserves slots × max-memory of virtual address space — wrong for a bar
  that loads 0–2 plugins. Default `mmap`-on-demand; revisit pooling only if
  instance counts grow.

### 2. The UI boundary: a typed `widget` vocabulary, host-rendered

`view()` returns a **`Render`** — a tree of WIT `widget` nodes — produced on the
plugin task and lifted+cached by the host (§1a). The host maps each node to a
real iced widget. This is Zellij's structured-component model done with typed CM
values instead of terminal escapes.

```wit
// wit/since-v0.1.0/ui.wit  (sketch)
variant widget {
    text(text-node),
    row(layout-node),
    column(layout-node),
    container(box-node),
    mouse-area(hit-node),         // carries an author-chosen hit-id
    icon(icon-node),              // our SVG set, host-rendered + host-tinted
    graph(graph-node),            // our sparkline, host-drawn on the host GPU
    spacer(f32),
}
record icon-node  { id: icon-id, color: paint, size: f32 }
record graph-node { values: list<f64>, kind: graph-kind, line: paint }
variant paint { token(theme-token), rgba(tuple<u8,u8,u8,u8>) }  // host themes it
```

- **Colours are theme references, not pixels.** A plugin written for dark looks
  right on light; our `Icon`/`Graph` render on the **host GPU**, tinted by the
  host. Plugins describe intent; the host owns look.
- **Events via hit-ids.** `mouse-area(hit-id)` tags a region; the host sends
  `Event::Pointer { id, kind, delta }` to `update()`, which maps `id → its own
  message` internally. The plugin's message type never crosses the boundary.
- **Pull + dirty.** The host re-runs `view()` only when `update()` returned
  `true` (or theme/output changed), so the lift+translate cost is paid per
  *state change*, not per frame — and bounded by the node cap (§1a).
- **Popup** is a second optional `Render`, rendered into the existing popup
  surface (RFC 0004).

#### 2a. `ezbar_plugin::widget` is a DSL, not iced — and that's stated plainly

The facade gives authors iced's *ergonomics* (the same `row((a,b)).spacing(5)`
builders, our `Icon`/`Graph` components) over a **bounded vocabulary**. It is
**not** arbitrary iced:

- The vocabulary is the status-chip set above: text, layout, container,
  mouse-area, our components, spacer. It covers ~everything a chip needs.
- **`canvas::Program`, `Shader`, and arbitrary custom widgets are not in the
  DSL.** On the `wasm32` target the facade simply doesn't export them, so a
  plugin that writes `iced::widget::canvas(...)` **fails to compile** — a clear,
  early error, not a silent `spacer` or a runtime surprise. (This is the
  honest answer to "what happens when I reach for an unsupported widget.")
- Authors who need raw canvas write a **compile-in module** against the real
  `Module` trait (the existing path, real iced, full GPU) — see *Alternatives*.
  That's the deliberate, small escape hatch; it is not a second plugin product.

So the maintainer's "write iced + use our components" is honored as: **write our
iced-backed widget vocabulary + our component library, rendered by real iced.**
Not "ship arbitrary iced into a sandbox" — which is impossible, and saying
otherwise would just generate "why doesn't my canvas work" bug reports.

### 3. Host services (the imports), capability-gated

Plugins reach the world only through **narrow custom host imports**, each **only
present in the linker if the matching capability was granted** (§5). Ungranted ⇒
the import doesn't exist ⇒ a call is a hard failure, not a checked no-op.

- `ctx.http(req).await` — the **host** performs the request (backed by the
  bar's existing `reqwest`) **after** checking `req.host` against the granted
  `network { host }`. Gated, async, own timeout. (Not `wasi:http`.)
- `ctx.read_file(path).await` — the **host** opens the file **after** checking
  `path` against `read-file { path }`. No guest FS handle, no preopen subtree.
- `ctx.feed(Feed::Cpu)` — subscribe to an existing host data feed
  (cpu/mem/temp/net/battery, already sampled in `src/sources/`). Gated
  `bar-state { feeds }`. Host enforces a poll-rate floor and coalesces, so N
  plugins on one feed don't multiply wakeups.
- `ctx.set_timeout(d)` / `ctx.subscribe(&[EventKind])` — the event loop.
- `ctx.theme()` / `ctx.text_size()` / `ctx.output()` — read-only render context.

**No `exec`/`ctx.run` in v1.** Arbitrary host-command execution from untrusted,
merely-hash-granted code defeats the sandbox's premise. A plugin that needs a
subprocess is a `custom` script, not a sandboxed plugin.

### 4. Interface versioning — bounded, no recompile-per-release (Zed's scheme, trimmed)

We never break an *installed in-range* plugin, and we bound how far back "in
range" goes:

1. **Frozen `since-vX` WIT directories**, never edited after release; a new
   version is a copy + edit. (Zed.)
2. **The host compiles the supported window** of versions, each its own
   `bindgen!` module + linker wiring + `From<old::Widget> for Widget`
   adaptation, behind one dispatch enum. *This is real machinery, not "~30
   lines"* — that estimate was for the capability matcher; the per-version
   bindgen/linker/adaptation cost is budgeted honestly here.
3. **Forward-adaptation**: an old plugin's narrower types are lifted to the
   current internal type; a two-version-old `.wasm` keeps running.
4. **api-version is auto-derived**, embedded in a `ezbar:api-version` custom
   wasm section from the SDK crate version, and **read from the raw bytes before
   instantiation**. Out-of-range ⇒ a clean "this plugin needs ezbar ≥ X / was
   built against a retired API; rebuild against `ezbar-plugin-wasm` x.y" — never
   an opaque link error. Bumping the SDK dep is the opt-in to a new ABI.
5. **Bounded window, not infinite history.** We support the **last N** WIT
   versions (start N≈3). Older ⇒ refuse-and-explain. A status bar's curated
   plugin set does not justify kernel-grade infinite compatibility; we keep the
   *don't-break-userspace* property *within the window* and are explicit at the
   edge. (Torvalds #5.)
6. **Channel staging** (stable accepts frozen versions; nightly also the
   in-dev WIT) + a `wasm-PENDING_CHANGES.md` to batch breaks so new `since-vX`
   bumps are rare.

### 5. Capabilities & trust — designed in, enforced by linker absence

- **Declared in the manifest**, pattern-matched (Zed's `*`/`**` matcher):
  ```toml
  # ezbar-plugin.toml — hashed together with the .wasm for grants
  id = "weather"; name = "Weather"; version = "0.1.0"
  # api_version is injected at build time — never hand-written

  [[capabilities]]
  kind = "network"; host = "api.open-meteo.com"
  [[capabilities]]
  kind = "bar-state"; feeds = ["cpu", "mem"]
  ```
  Kinds (v1): `network { host }`, `read-file { path }`, `bar-state { feeds }`.
  (No `exec`/`clipboard`/`notify` in v1.)
- **Enforcement is linker absence first, argument-matching second.** An
  ungranted capability means its host import is **not added to the linker**;
  a granted-but-scoped one (e.g. `network { host = "x" }`) additionally has its
  *arguments* checked at call time by the matcher.
- **User consent on first load**, prompted in a popup, persisted in a grant
  cache **keyed by `hash(wasm ‖ manifest)`** — so changing *either* the code or
  the declared capabilities re-prompts (you can't swap a benign manifest under a
  granted hash to escalate). A `granted_capabilities` setting can pre-grant or
  hard-deny (`[]` = sandbox-only).
- **Signatures are the later trust layer.** v1 verifies the content hash on
  install; a follow-up adds optional author signatures (the natural home for a
  curated registry). Because grants are hash-keyed, signing slots in cleanly.

### 6. Loading, discovery, hot-reload

- **Discovery:** scan `~/.config/ezbar/plugins/*.wasm` (+ a `plugins = [...]`
  list / `file:`/`https:` URLs). Remote artifacts are downloaded **atomically**
  (temp + rename) and **hash-verified** — fixing the concurrent-download
  corruption Zellij documents.
- **Auto-load** rides RFC 0004's reactive pipeline — a plugin is just another
  placeable module id.
- **Hot-reload:** an mtime/inotify change re-instantiates the component (cheap,
  safe in wasm), with optional `save_state()`/`restore` — **clean reload only**
  (after a trap the store is poisoned and state is lost; §1a). `ezbar plugin
  reload <id>` forces it.

### 7. Distribution

A plugin is a `.wasm` + `ezbar-plugin.toml`, shipped as a **GitHub Release
artifact** (a provided Action builds `wasm32-wasip2` + uploads), discovered via a
curated **`awesome-ezbar`** list, installed by URL or drop-in, always
hash-verified. A signed registry is a later layer, not a v1 blocker.

### 8. The Claude skill (a first-class deliverable)

Ship `ezbar-wasm-plugin-author` — a skill that scaffolds, writes, builds
(`cargo build --target wasm32-wasip2`), capability-declares, and harness-tests a
plugin. The SDK's narrow, typed surface (`Plugin` trait + `widget` DSL + a
handful of `Ctx` calls) is deliberately LLM-shaped. "Vibe-code a crypto chip that
polls Coinbase and turns red on a 5% drop" should be one-shot.

## What this RFC explicitly does NOT do

- **No live `iced::Element`, no `canvas`/`Shader`, no arbitrary widgets across
  the sandbox.** Bounded `widget` vocabulary + our components only. Raw canvas =
  compile-in/fork.
- **No `exec`, no clipboard/notify, no raw WASI fs/http** exposed to guests.
- **No GPU-in-wasm.** Our `Graph`/`Icon` already render on the host GPU; a
  sandboxed guest could at best produce an isolated texture (all the cost, none
  of the integration) — explicitly out of scope.
- **No second, co-equal native plugin product.** A native *dynamic* tier may be
  revisited *only if* real demand for third-party raw-canvas widgets appears;
  until then it does not exist and is not maintained. (Torvalds #1.)

## Alternatives (confronting the cheaper paths)

- **Extend `custom` to emit structured JSON** (text + icon + graph + popup over
  stdout). This is the strongest alternative and RFC 0001 already favours it for
  *most* widgets: zero ABI risk, every language, no toolchain. **We should build
  this regardless** — it's the right tool for "shell out and show a number," and
  it subsumes a lot of would-be plugins. What it *cannot* give: bounding a
  wedged/leaking/crashing author process, or capability-sandboxing untrusted
  code. So: ship `custom`-JSON for the easy 80%, and wasm for the cases whose
  entire reason to exist is **isolation** (untrusted third-party code, or a
  plugin you don't want able to hang your bar). If a plugin doesn't need
  isolation, it should be a `custom` script, and we'll say so in the docs.
- **Native dlopen (RFC 0001 phase 2).** A same-toolchain pin: must be recompiled
  bit-identically against each ezbar build, no crash isolation, unsafe
  hot-reload. Fine for *your own* out-of-tree code; not a tier strangers ship
  into. Demoted to compile-in/fork, not built here.
- **Embed iced+wgpu in the guest (render to a texture).** Maximally "native"
  source, but the guest renders an isolated raster the host composites — all the
  integration cost (events, fonts, fixed-size reflow, a per-plugin iced runtime),
  none of the shared-tree benefit, and it weakens the sandbox. Rejected.

## Phasing

1. **PoC (this branch):** wasmtime host harness; `since-v0.1.0` WIT with
   `text`/`row`/`icon`; a `weather` plugin (Rust → `.wasm`), drop-in-loaded,
   rendering a real chip off the GUI thread via the cached-tree path;
   epoch-trap, node-cap, and capability-deny all demonstrated.
2. **SDK + DSL:** `ezbar-plugin-wasm` crate, `widget` DSL, `plugin!` macro,
   gated async `Ctx` services, `bar-state` feeds.
3. **Versioning window + capabilities + grant UI.**
4. **Hot-reload on the RFC-0004 pipeline; distribution Action; the Claude skill.**

(No native-tier phase. That's the point.)

## Open questions

1. **Popup-tree lift+translate budget.** The real cost isn't "serialization" in
   the abstract — it's the host-side lift + translate of a large popup tree
   (hundreds of rows) on a latency-sensitive path. Benchmark it with the node
   cap in place; the CM canonical ABI is the right encoding (don't reach for a
   flatbuffer for an unmeasured micro-opt).
2. ~~Default resource budgets — fixed or `[plugins]` knobs?~~ **Resolved (v2.1):
   fixed constants for v1** (epoch ticks, per-store + aggregate memory, node/depth
   cap). Expose as knobs only if a real plugin needs it.
3. **Feed set & floor** — which host feeds to expose and the minimum poll period.
4. **Per-output plugin placement** — pairs with RFC 0004's per-output surfaces.
5. **Versioning window size N** and the retirement policy at the edge.
6. **Signature trust model** — TOFU / keyring / registry-issued; shaped so §5's
   hash-keyed grants extend to it.

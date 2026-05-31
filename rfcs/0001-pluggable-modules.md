# RFC 0001: Pluggable bar modules

- **Status:** Accepted (v2 — ACK'd by sway + Hyprland review)
- **Created:** 2026-05-31
- **Target:** ezbar (Rust / iced / wlr-layer-shell)
- **Reviewers:** sway maintainer, Hyprland maintainer

## Changelog (v2)

Addresses round-1 blocking review. Map of fixes:

- **dlopen is a toolchain-pin, not a C-ABI plugin system.** Rewrote *Loading —
  phase 2*: the only `extern "C"`/`repr(C)` surface is one registration struct;
  everything else is Rust-ABI, valid only because host and plugin are
  *bit-identical builds*. ABI stamp is now a **toolchain+deps hash**, not an
  integer.
- **Panic catching moved to the plugin side**; render-time (`canvas::draw`)
  panics declared *not contained*; launcher respawn named as the real recovery.
- **Host control no longer rides `Arc<dyn Any>`.** `HostRequest` is a typed,
  out-of-band return channel; `Any` is intra-module only. `TypeId`-across-builds
  hazard called out.
- **Popups fully specified:** one at a time (host-enforced), host-side anchoring
  from slot bounds, hover=display-only vs click=interactive.
- **Subscriptions keyed by stable instance id**, not positional slot.
- **Lifecycle/unload promoted from Open Questions to Design:** teardown hook;
  leak the `dlopen` handle (never `dlclose`); hot-reload = new-instance-alongside.
- `Ctx` exposes `repr(C)` theme tokens, not `&iced::Theme`.
- Multi-output decided: per-output module instances.

## Summary

A trait-based module system for ezbar. Each widget becomes a `Module` written in
*plain iced* — it owns its drawing and input; the host owns placement, lifecycle,
and the Wayland surfaces. **Phase 1 (this RFC): compile-in modules.** **Phase 2
(designed-for, not built): `dlopen`**, which is explicitly a *same-toolchain*
extension mechanism, not a stable third-party ABI.

## Motivation

Widgets are hardcoded into one `view`/`update`/`subscription`. We want out-of-tree
widgets, config-driven (and per-output) placement, and a path to drop-in plugins
without recompiling the core.

We reject an **IPC/gRPC boundary for first-party widgets**: an iced `Element` is
generic, lifetime-bound, non-`repr(C)`, and cannot cross a process or stable-ABI
boundary, so an out-of-process plugin can only exchange *data* or *draw commands*,
not iced code. Our widgets are also I/O-heavy (procfs, subprocesses, unix
sockets) which a sandbox fights. So in-process iced modules are the default.
**Honest caveat:** the same `repr(C)` problem exists at the `dlopen` boundary —
in-process is not "safe because it's in-process"; phase-2 is safe only by
pinning the toolchain (below). IPC / WASM remain viable for *untrusted /
third-party / other-language* plugins as a separate backend — out of scope.

## Goals / Non-goals

Goals: a "just iced" `Module` trait; host-owned placement & surfaces; module
messages decoupled from the host enum; panic/stall containment to the degree
actually achievable in-process. Non-goals (this RFC): implementing `dlopen`;
WASM/IPC; full multi-output (the *trait shape* for it is decided here, the
rendering is not).

## Design

### The shared API crate (`ezbar-plugin`)

Defines the surface modules build against: `Module`, `Ctx`, `ModMsg`,
`Response`, `HostRequest`, `ThemeTokens`, subscription-keying helpers, and a
**re-export of the pinned `iced`**. Host and (later) plugins depend on this exact
version. For phase 2 this crate's *source + the toolchain* is the contract.

### The trait

```rust
/// Opaque, type-erased *intra-module* message. Modules define their own enums;
/// the host never names or inspects them. `Arc` gives the `Clone` iced needs.
///
/// HARD RULE: `ModMsg` is valid only within one build unit. `Any`/`TypeId` are
/// NOT stable across separately-compiled crates, so the host MUST NOT downcast a
/// `ModMsg` to interpret it. Host control travels via `Response`/`HostRequest`.
pub type ModMsg = std::sync::Arc<dyn std::any::Any + Send + Sync>;

/// Host-owned theme, `repr(C)`-stable so an `iced` bump doesn't churn the ABI.
#[repr(C)]
pub struct ThemeTokens {
    pub fg: [f32; 4], pub fg_dim: [f32; 4], pub urgent: [f32; 4],
    pub warn: [f32; 4], pub ok: [f32; 4], pub accent: [f32; 4],
    pub text_size: f32, pub bar_height: u16,
}

pub struct Ctx<'a> {
    pub instance_id: u64,         // stable, unique per module instance
    pub theme: &'a ThemeTokens,
}

/// Returned from `update`: an iced task PLUS typed host requests. Host control
/// is statically typed end-to-end — it never rides the `Any` channel.
pub struct Response {
    pub task: iced::Task<ModMsg>,
    pub requests: Vec<HostRequest>,
}
impl Response { pub fn task(t: iced::Task<ModMsg>) -> Self; pub fn none() -> Self; }

#[repr(u8)]
pub enum PopupMode { Hover, Click } // Hover: display-only, auto-close. Click: interactive, sticky.

#[repr(C)]
pub enum HostRequest {
    OpenPopup { mode: PopupMode },
    ClosePopup,
}

pub trait Module: Send {
    fn id(&self) -> &str;

    /// ALL I/O lives here. MUST NOT block. Recipes MUST be namespaced by
    /// `instance_id` (use `ezbar_plugin::sub::keyed`) so two instances of the
    /// same module do not collide in iced's recipe-keyed subscription runtime.
    fn subscription(&self) -> iced::Subscription<ModMsg> { iced::Subscription::none() }

    /// State transition. MUST NOT block. Returns a task + typed host requests.
    fn update(&mut self, msg: ModMsg) -> Response { Response::none() }

    /// Bar content. Full iced: `canvas`, `mouse_area`, etc.
    fn view(&self, ctx: &Ctx) -> iced::Element<'_, ModMsg>;

    /// Detail surface; host opens/places a popup surface and renders this.
    fn popup(&self, ctx: &Ctx) -> Option<iced::Element<'_, ModMsg>> { None }

    /// Teardown: stop work, drop resources. Called before the instance is
    /// retired (config change, output removal, reload).
    fn shutdown(&mut self) {}
}
```

### Message routing (keyed by instance id, not slot)

```rust
enum Message {
    Module { instance: u64, msg: ModMsg },
    // host-owned: popup lifecycle, output add/remove, ...
}
```

- **view:** `module.view(ctx).map(move |m| Message::Module { instance, msg: m })`.
- **update:** `Message::Module { instance, msg }` → look up by `instance` (stable
  id, survives reordering/hot-reload) → `module.update(msg)` → host applies the
  returned `Response.requests` (typed) and re-maps `Response.task`.
- **subscription:** batch of each module's mapped subscription; recipes are
  per-instance-keyed so reordering does not tear down/recreate them.

`Arc<dyn Any>` erasure lets a module compile without the host enum (the phase-2
precondition) **for intra-module messages only**. The host never downcasts it.

### Popups (host-owned surfaces, fully specified)

- **At most one popup exists at a time, host-enforced.** A new `OpenPopup`
  closes any existing popup first. (Matches today's single-`Option` model.)
  "One" is **global / per-seat**, not per-output. `popup()` is **leaf-only**: it
  supplies content and may not itself emit `HostRequest` (no nested popups).
- **Triggers & interactivity are explicit via `PopupMode`:**
  - `Hover` — opened from `on_enter`, closed from `on_exit`; rendered
    **display-only** (`events_transparent`, never grabs focus). Moving onto it is
    not required or supported. This is the *only* sound hover model under
    layer-shell (a separate hover surface + interactive content fights pointer
    crossing).
  - `Click` — opened from `on_press` (toggle); **interactive**; input inside
    routes back as `Message::Module { instance, .. }`; sticky until toggled/closed.
- **Anchoring is host-side**, derived from the **triggering instance's rendered
  slot bounds** (the host tracks each instance's layout rectangle), not a
  per-module magic margin. Modules never position in screen space; `popup()` only
  supplies content.

### Placement / layout

```toml
[bar]
height = 34
[[module]]
id = "claude"; zone = "right"; order = 0
[[module]]
id = "cpu"; zone = "right"; order = 10
[module.config] { show_graph = true }
```

Host lays each zone out as a `row`, inserts separators, assigns a stable
`instance_id`, and **owns width-clamping**: it wraps a module's `Element` in a
max-width container with host-side ellipsis. Modules render natural width and
MUST NOT assume a fixed width or self-truncate to the slot.

### Multi-output (trait shape decided; rendering deferred)

Modules are **per-output instances**: each output gets its own `Vec<Module>` with
its own `instance_id`s and subscriptions. `view` therefore does *not* take an
output id (the instance is already output-scoped). Output removal retires that
output's instances (`shutdown` each), deterministically dropping their
subscriptions. This keeps teardown clean and the trait free of an output param.

### Panic / stall safety (honest about what's containable)

- **Phase 1, `update`:** host calls it inside `catch_unwind(AssertUnwindSafe)`.
  On panic the instance is **torn down entirely** (removed, replaced by a static
  error chip, never called again) — not "disabled but still rendered", since
  its state may be torn.
- **Phase 1, `view`/`canvas::draw`:** **NOT contained.** A panic in `view`
  construction or, especially, in a `canvas::Program::draw` closure fires inside
  iced's render pass, far from any guard; it crashes the bar. The existing
  **launcher** (parent process respawns the child, with backoff) is the real
  recovery for these, and is the honest crash-containment story today.
- **`panic` strategy:** phase 1 runs `panic = "unwind"`; the per-`update`
  `catch_unwind` is additive polish on top of the launcher, not the primary
  mechanism. Don't over-invest before phase 2.
- **Stall:** "no blocking in `update`/`view`" is a hard contract; all work goes
  through `subscription`/`Task` (async, off the UI thread). In-process we cannot
  preempt a wedged `update`; **a blocking in-process module will freeze the bar.**
  Stated as the explicit price of the in-process tier. A soft watchdog may log/
  tear down a module whose `update` exceeds a time budget (best-effort).

### Loading — phase 1 (this RFC): compile-in

```rust
type Factory = fn(toml::Value, Ctx) -> Box<dyn Module>;
fn registry() -> &'static [(&'static str, Factory)];
```

Add a module = implement the trait + one registry line + recompile. Sound, full
iced, zero ABI risk.

### Loading — phase 2 (designed-for, NOT built): dlopen as a toolchain pin

**This is not a C-ABI plugin system and we will not pretend otherwise.** What
makes it sound is that the host and every plugin are **bit-identical builds**:
same `rustc` commit, same target triple, same `ezbar-plugin` + `iced`
source/features, same profile. Under that pin, Rust-ABI values (`Element`,
`Task`, `Subscription`, `Arc<dyn Any>`) are laid out identically on both sides
and may be passed across function-pointer calls.

The *only* `extern "C"` / `repr(C)` surface is a single registration struct
behind one stable export symbol:

```rust
#[repr(C)]
pub struct AbiRegistration {
    /// 32-byte hash of {rustc -Vv commit, target triple, ezbar-plugin semver,
    /// iced semver+features, build profile}. Embedded in host and plugin via
    /// build script. Host compares and HARD-FAILS on any mismatch (no best-effort).
    pub abi_hash: [u8; 32],
    pub id: *const std::os::raw::c_char,
    pub constructor: extern "C" fn(cfg: *const u8, cfg_len: usize, ctx: *const Ctx) -> *mut ModuleHandle,
    pub vtable: *const ModuleVtable, // extern "C" thunks: update/view/subscription/popup/shutdown/drop
}
#[no_mangle]
pub extern "C" fn ezbar_module_abi() -> *const AbiRegistration;
```

- The vtable thunks *do* move Rust-ABI types (`Element`, `Task`, …) through their
  return slots. That is sound **only** because of the bit-identical pin — the
  `extern "C"` is just the calling convention for one stable struct, never a
  layout guarantee for those aggregates. We document this loudly so nobody ships
  a plugin from a different toolchain.
- **Version policy:** the `abi_hash` will change on essentially every host
  release; **expect to rebuild every plugin per release** (a build helper /
  `pacman`-style rebuild flow is the intended UX, mirroring established
  dynamic-plugin ecosystems). Mismatch = refuse to load.
- **Panic across FFI is UB:** every vtable thunk wraps its body in
  `catch_unwind` **inside the plugin** and returns a `repr(C)` error sentinel;
  the host NEVER catches a plugin's unwind. Render-time `canvas` draw closures
  are wrapped by an `ezbar-plugin`-provided guarded `canvas` adapter that, on
  panic, paints nothing and flags the instance for teardown — so a draw panic
  does not unwind through iced's renderer into either side. **Scope:** this
  contains only panics originating *inside the draw closure*; panics during
  `view`/`Element` construction remain uncontained (as in phase 1), with the
  launcher as the recovery.
- **`HostRequest` is typed and crosses as a `repr(C)` value** out of the update
  thunk; it never rides `Arc<dyn Any>`/`TypeId`, which are intra-build-unit only.

### Lifecycle / unload (Design, not an open question)

`dyn Any` messages, live `Subscription` recipes, in-flight `Task` futures, and
`Element` closures all hold code/vtable pointers into the plugin image. Proving
none survive before `dlclose` is intractable, and iced's subscription runtime
keeps recipes across frames. Therefore:

- `shutdown()` stops the instance's work; the host then drains its subscriptions/
  tasks and drops every `ModMsg`/`Element` it owns.
- **The host never `dlclose`s a plugin image — it leaks the handle.** This trades
  address-space for soundness; acceptable for a bar. Leaking the image is also
  precisely what makes the drain *best-effort*-safe: a straggler subscription
  future or in-flight task that outlives `shutdown()` then runs against
  still-mapped code rather than a UAF — the host cannot synchronously prove every
  recipe future has dropped, and does not need to.
- **"Hot-reload" = construct the new `.so`'s instance alongside, retire the old
  instance (`shutdown` + drop), keep the old image mapped.** No in-place code
  patching, ever.

## Alternatives considered

- **gRPC / out-of-process / WASM:** cannot carry iced; force a data or draw-list
  protocol; WASM additionally fights our subprocess-heavy widgets. Deferred as a
  *separate* backend for untrusted/3rd-party/other-language plugins.
- **Data-only protocol (swaybar `status_command` / i3blocks):** honestly, *most*
  of our widgets (cpu/mem/temp/ping-as-text, clock, battery, volume, kubectl,
  github-count, stock-ticker) are text+color and would be perfectly served by a
  JSON-over-stdout protocol with **zero ABI risk and full language freedom**.
  Only the `canvas` graphs and the stock/Claude hover charts need "draw your
  own". The in-process trait tier exists to serve that drawing minority; we
  accept its ABI burden across all modules to avoid splitting the model. This is
  a deliberate trade, not a claim that data protocols are weaker.

## Migration

Convert widgets to `Module`s behind the registry one at a time; keep the
hardcoded path until parity; flip to the module-driven loop; delete the old path.
Validate with: a popup+hover module (stock/claude), a popup+click module
(github), and a `canvas`+intra-module-toggle module (cpu).

## Open questions

1. Soft-watchdog budget for `update` — concrete time limit and action. (Note: a
   watchdog can log/tear-down *between* calls but **cannot preempt** a wedged
   synchronous `update`; an in-process blocking module freezes the bar, period.)
2. Inter-module data sharing: if ever allowed, it goes through host-defined
   `repr(C)`/serde types only — never raw `Any` (same `TypeId`-across-builds
   hazard between plugins). Default: modules isolated.
3. *Resolved (see Popups): `popup()` is leaf-only; popups are one-at-a-time,
   global/per-seat.*

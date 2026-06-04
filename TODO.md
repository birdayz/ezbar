# ezbar — roadmap & TODO

## ★ North star — toward the best bar ever

**Thesis:** ezbar has two assets no other bar has — **GPU rendering** and a **safe,
hot-reloadable plugin platform** — and today it uses about half of each. "Best bar
ever" is *not* more built-in modules or theme knobs (that's the waybar treadmill).
It's finishing those two things so the bar does what no other bar can: **feel alive
(motion) and let you build & interact (a real widget platform).**

Each bet runs the drill: RFC → review (2 subagents) → implement → review → commit → merge.

### P0 — make it alive (start here)
- [x] **Pointer events → interactive plugins** — **DONE** (RFC 0009, merged `7e223d5`).
  Host delivers press/right-press/scroll/enter/leave to the guest's `update(Event::Pointer)`
  through the reactor, bounded by the cadence gate + WALL/epoch. Buttons + scroll-to-adjust
  + hover. Drag/release deferred to an additive v0.2.0 WIT.
- [~] **Motion — first eased transition** — built (RFC 0010), **opt-in/default-off**
  (`[modules.workspaces].animate = true`). The original `window::frames()` driver broke hover
  in iced_layershell (frame-callback path corrupts the pointer-seat → `mouse hasn't entered`),
  a layershell-only failure the mainline-iced reviews couldn't catch — found by deploying live.
  Re-driven with `iced::time::every(16ms)` (no frame callbacks → shouldn't touch the seat) and
  gated off by default so the default bar's hover is known-safe. **Needs one live check:** flip
  `animate = true`, switch a workspace, confirm hover still opens popups (cursor warps can't
  tell the drivers apart — only a real hover does).

### P1 — finish the platform
- [x] **Event-driven cadence (`set_timeout`)** — **DONE** (RFC 0011). The reactor honors
  the frozen `host.set-timeout`: one-shot timer, re-armed per tick; a plugin that never
  arms keeps a legacy 2 s heartbeat, `set_timeout(0)` opts out to **zero** wakes. Killed the
  blind poll, added an immediate post-init bootstrap tick (chip paints at t≈0), surfaced
  `set_timeout` on the Rust `Ctx` (Go already had it) with an identical loud docstring, and
  migrated weather/btc/quakes off the 2 s poll (they were hammering open-meteo/Coinbase/USGS
  at 0.5 Hz) onto real cadences with error backoff.
- [~] **Safe host capabilities** so the powerful widgets *can* be plugins (RFC 0007
  showed they can't today). **Host-computed feeds: DONE** (RFC 0012) — a sandboxed plugin
  subscribes to `cpu/memory/temperature/battery/net` via `feed-subscribe`, the host samples
  once and fans out to all subscribers (idle = zero), capability-gated by
  `[modules.<id>].feeds`. The `sysgraph` example draws a live CPU graph with no `/proc`
  access — the thing RFC 0007 said was impossible. **Still TODO: read-only sway IPC**
  (workspaces/title) — needs a new `since-v0.2.0` WIT (not in the frozen surface), so it's
  its own RFC. The sandbox stays a sandbox.

### P2 — ecosystem
- [ ] **Plugin registry + `ezbar install <plugin>`** with a capability **manifest** the
  user approves on install. Multi-language already works (Rust + Go/TinyGo). The network
  effect no other bar has.

### P1 (cont.) — read-only sway IPC: **designed, ready to implement**
- [ ] **Read-only sway state** — **RFC 0013 Accepted** (both reviewers ACK after folding
  push→**pull**). Completes "safe capabilities": a plugin reads the workspace list + focused
  title via a `sway-snapshot() -> result<sway-state, string>` host call, capability-gated by
  `[modules.<id>].sway`. Forces the **first WIT version bump** (`since-v0.2.0`) + the
  frozen-version-window infra (RFC 0006 §4). Two phases:
  - [x] **Phase 1 — the version window — DONE.** `wit/since-v0.2.0` (pure copy + version
    bump); dual `bindgen!` with `types`/`ui`/`events` **remapped** to v0.1.0 (so `Tree`/`Event`
    and the whole drive loop/`lift`/render are shared — only `Plugin` + the `host` trait fork);
    `linker_v2`; `enum DrivenPlugin{V1,V2}`; version detection by introspecting the component's
    imported `ezbar:plugin/host@x.y`; v2 `host` impl delegates to v1 (v1 untouched). **Verified:
    v0.1.0 weather (78 popup nodes, unchanged) AND a v0.2.0 plugin both co-load on one binary.**
  - [x] **Phase 2 — sway-read — DONE (Rust).** `host.sway-snapshot() -> result<sway-state,
    string>` in the v0.2.0 WIT (records in `host`, not `types`, to keep the remap);
    `set_sway_source` injection in `main.rs` over `sources::sway::snapshot()`; the v2
    `sway_snapshot` host impl (gated by `[modules.<id>].sway`, synchronous `Err` denial);
    `Ctx::sway_snapshot()` in the Rust SDK (bumped to v0.2.0); the **`wintitle`** dogfood
    (v0.2.0) reads it and renders — verified, and weather (v0.1.0) unchanged. Read-only (no
    `run_command`). **Follow-up:** Go-SDK parity (needs the v0.2.0 Go bindings regenerated).

### Ongoing — reliability (table stakes)
- [ ] **Multi-monitor / hotplug / sway-reload hardening** + a regression harness for
  output churn (the two-bars saga bit us twice). Stunning-but-flaky ≠ best.

### Backlog from the reactor reviews (non-blocking)
- [ ] Wire `save_state`/`restore` across clean reloads, or drop them from the frozen WIT.
- [x] Cache eviction sweep — **DONE**. After publishing a fresh `.cwasm` (a plugin rebuilt),
  evict the oldest beyond a 24-artifact cap (`sweep_cache`), so the cache can't grow one
  ~MB file per rebuild forever. Self-healing: an evicted-but-active artifact recompiles once.
- [x] Capability matcher — **DONE**. `host_matches` normalizes case (DNS is
  case-insensitive) and treats a port-less grant as authorizing any port, while a
  `:port`-pinned grant must match exactly. Replaces the naive `grant == host` that rejected
  `API.Example.com` or an explicit `:443`. No implicit subdomain match (security). Tested.

### Anti-goals (the Linus part)
No chasing waybar's module count. No config knobs for zero users. Every hour on a
builtin nobody asked for is an hour stolen from the platform + the motion.

---

## Detailed backlog

Outstanding work, tagged by criticality. The bar is fully functional today, so there
are no **CRIT** items (nothing broken/crashing); the highest are promised-but-missing
features and config keys that silently do nothing.

**Legend:** `CRIT` broken/blocking · `HIGH` promised feature missing or misleading
behaviour · `MED` wanted feature / real gap · `LOW` polish / nice-to-have.

---

## Config & theming (RFC 0002)

- [~] **MED** — **Graph knobs — mostly DONE.** `[modules.<id>.graph]` now exposes
  `samples` · `width` · `height` · `line_width` · `fill` (+ the pre-existing `line_color`),
  resolved by `modules::graph_cfg` and clamped to sane bounds; defaults preserve the prior
  look exactly (per-module sample caps, 48×16, 1.5 px, filled). The `Graph` widget gained
  `line_width`/`fill`; a `Graph::new` keeps the reactor/tests on defaults. **Remaining:**
  `smooth` (Catmull-Rom on the metric sparklines) — deferred as a riskier rendering-character
  change. (`modules/{mod,cpu,memory,temperature,ping}.rs`, `ezbar_plugin::ui::graph`)
- [x] **HIGH** — **`[theme.workspaces]` per-state colours were parsed but unused → DROPPED.**
  The chip is fully themed by the global `[theme]` tokens (`accent`/`fg`/`fg_dim`/`urgent`)
  plus `[modules.workspaces].style`, so the parallel `focused/occupied/empty/urgent/colors/
  special` fields in `WorkspaceTheme` were vestigial config that lied — removed (re-wiring
  would just duplicate the global theming). `WorkspaceTheme` now carries only `style`.
  (`config.rs:WorkspaceTheme`)
- [ ] **MED** — **Inline markup unimplemented.** RFC specs a themed `[c=token]…[/c]` /
  `[b]…[/b]` subset for `window_title`/`custom`; not built. (`config.rs`, `modules/
  window_title.rs`)
- [x] **MED** — `[theme.workspaces].colors[]` / `special[]` — **removed** (were
  undocumented + unused; dropped with the other dead `WorkspaceTheme` fields).
- [ ] **LOW** — `[bar].radius` on the **solid** bar surface (margin/float shipped;
  rounding the solid slab needs the transparent-surface + rounded-container path).
- [ ] **LOW** — Per-output config (per-output `scale`/`height`/outputs) — deferred.
- [ ] **LOW** — `include = [...]` for the *whole* config (presets already cover theme).
- [x] **LOW** — `read_parse` policy **made consistent** — DONE. On *reload*, a missing/
  unreadable file now keeps-last-good (like a parse error), instead of flashing the live bar
  to defaults during an editor's atomic save. Startup `load()` still treats no-config as
  defaults (right for a fresh install). (`main.rs:read_parse`)

## Modules & SDK (RFC 0001 / 0003)

- [ ] **MED** — **`Service` layer for non-sway capabilities.** The sway service shares
  one connection; D-Bus/PipeWire need the same before Tier-B, ideally surfaced via
  `Ctx` so third-party plugins use it too (today a module opens its own client).
- [~] **MED** — **`custom` streaming form — `listen_cmd` DONE.** A long-running command
  whose stdout LINES each update the chip (event-driven, no polling); supersedes `command`
  when set, restarts gently on exit, `kill_on_drop` so a config reload doesn't leak the
  process. **Remaining:** the regex→icon map and the `alert` danger dot. (`modules/custom.rs`)
- [ ] **MED** — **Icon metric-normalization / per-glyph nudge table.** RFC says this
  "must not ship empty"; today icons rely on the raw font baseline. Promote to an
  `ezbar_plugin::ui` helper (`ctx.icon(Icon::…)`).
- [x] **LOW** — `graph_widget` helper DRYs the four metric views — **DONE**. The identical
  `canvas(Graph{…}).width().height()` block in cpu/memory/temperature/ping is now one
  `modules::graph_widget(gcfg, kind, values, line_color)`, so a graph change (size/stroke/
  fill, a future `smooth`) touches one spot, not four.
- [ ] **LOW** — Restore the workspaces **urgent blink** (dropped in the module port; the
  module would own a 500ms tick like `calendar`). (`modules/workspaces.rs`)
- [x] **LOW** — `ezbar msg volume` **routes through the volume module** now — DONE. The
  IPC/keybind path dispatches `ModuleMsg` to the volume instance (via `volume::adjust_msg`),
  which changes the level and refreshes its displayed value in one `update` (no lag waiting
  for the 1s poll). Falls back to poking the source directly only if no volume pill is
  placed. (`main.rs:VolumeAdjust`, `modules/volume.rs`)
- [ ] **LOW** — `clock` **weather**, sway **submap** module — RFC Tier-A leftovers.
- [x] **LOW** — `ipc_stream` **probe-before-unlink** — DONE. It now `connect()`s to an
  existing socket first; a live listener is left alone (the instance runs without IPC
  instead of hijacking it), and only a dead/stale socket is unlinked + rebound. Stops a
  second `ezbar` launch from silently stealing `ezbar msg` routing from the running bar.
- [ ] **LOW** — Module instance ids restart at 1 on rebuild (harmless per review, but
  identity isn't stable across reorders).

## Tier-B desktop stack (RFC 0003 — "expensive, demand-gated")

- [ ] **MED** — **`tray`** (StatusNotifierItem) and **`media`** (MPRIS + art + transport)
  — the most-requested Tier-B modules; need the `Service` layer first.
- [ ] **MED** — **OSD** (volume/brightness transient overlay), driven by `ezbar msg
  volume/brightness …` + widget interaction.
- [ ] **LOW** — `privacy` (PipeWire mic/cam/screenshare dots).
- [ ] **LOW** — `settings` quick-panel (audio sink/source + brightness sliders first;
  power menu / peripheral battery / idle-inhibitor after).
- [ ] **LOW** — `network` (WiFi scan + password dialog) + `bluetooth` — demand-gated,
  largest lift, most duplicative of existing applets.
- [ ] **LOW** — `notifications` daemon — acquire `org.freedesktop.Notifications`
  **without replace**, refuse-if-mako/swaync-owned.

## Testing & docs

- [~] **MED** — **Integration/runtime tests — started.** The **placement resolver**
  (`desired_module_specs`) is now covered: default set (workspaces leads, clock end-cap),
  per-key dedup, explicit-zone override of defaults, and chrome (`switcher`) never resolving
  as a module. **Remaining:** the live-wiring loops (file-watch reload keeps the active
  preset, module set rebuilds on placement change, multi-instance routing) — these need
  test seams around the iced `update`/reconcile, not just pure functions.
- [ ] **LOW** — Per-module config reference (a table of every `[modules.<id>]` key).
- [ ] **LOW** — A GIF of the live `▾` switcher cycling presets for the README /
  r/unixporn post (a still doesn't convey "no restart").
- [x] **LOW** — RFC statuses updated: 0002/0003/0004/0005 → **Implemented** (were "Draft").
  0007 stays **Draft** (genuinely pending the port-list decision). The rest already carried
  Accepted/Implemented/their runtime-state.

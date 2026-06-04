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
- [x] **Motion — first eased transition** — **DONE** (RFC 0010). The GPU's unused payoff
  and the r/unixporn "what bar is *that*?". On a workspace switch the active highlight
  **cross-fades** (180 ms `EaseOutCubic`) from the old pill to the new instead of
  hard-cutting — a thing a cairo/damage-repaint bar physically can't do. Per-pill
  `iced::Animation<bool>` inside the `workspaces` module; frames requested **only while a
  fade runs** (idle bar = zero extra redraws). Next motion targets: sliding/fading popups,
  smooth graph scroll, hover micro-interactions — and eventually a plugin-authored tween.

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

### Ongoing — reliability (table stakes)
- [ ] **Multi-monitor / hotplug / sway-reload hardening** + a regression harness for
  output churn (the two-bars saga bit us twice). Stunning-but-flaky ≠ best.

### Backlog from the reactor reviews (non-blocking)
- [ ] Wire `save_state`/`restore` across clean reloads, or drop them from the frozen WIT.
- [ ] Cache eviction sweep (`.cwasm` grows unbounded on plugin rebuilds).
- [ ] Capability matcher: normalize host/port/case (no naive string equality).

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
- [ ] **LOW** — `ui::metric_graph` helper to DRY the four near-identical metric module
  views (cpu/memory/temperature/ping).
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
- [ ] **LOW** — Mark the RFCs Accepted/Implemented now that the bulk has shipped (they
  still say "Draft v3").

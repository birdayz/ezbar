# ezbar — TODO

Outstanding work, tagged by criticality. The bar is fully functional today, so there
are no **CRIT** items (nothing broken/crashing); the highest are promised-but-missing
features and config keys that silently do nothing.

**Legend:** `CRIT` broken/blocking · `HIGH` promised feature missing or misleading
behaviour · `MED` wanted feature / real gap · `LOW` polish / nice-to-have.

---

## Config & theming (RFC 0002)

- [ ] **MED** — **Graph knobs mostly not config-exposed.** `[modules.<id>.graph]`:
  `line_color` is **done** (per-widget `threshold`/token/hex via `Ctx::graph_paint`,
  default green→red) — but `samples` · `height` · `line_width` · `fill = {gradient, alpha}`
  · `smooth` are still hardcoded (48/16/`GraphKind`). RFC headline + the r/unixporn reviewer
  flagged the set. (`modules/{cpu,memory,temperature,ping}.rs`, `ezbar_plugin::ui::graph`,
  `config.rs`)
- [ ] **HIGH** — **`[theme.workspaces]` per-state colours are parsed but unused.** The
  chip reads `ctx.accent/fg/dim/urgent`; `focused/occupied/empty/urgent/colors/special`
  in `WorkspaceTheme` do nothing → config that lies. Either wire them through or drop
  them. (`config.rs:WorkspaceTheme`, `modules/workspaces.rs`)
- [ ] **MED** — **Inline markup unimplemented.** RFC specs a themed `[c=token]…[/c]` /
  `[b]…[/b]` subset for `window_title`/`custom`; not built. (`config.rs`, `modules/
  window_title.rs`)
- [ ] **MED** — Document `[theme.workspaces].colors[]` / `special[]` semantics (or
  remove); currently undocumented and unused.
- [ ] **LOW** — `[bar].radius` on the **solid** bar surface (margin/float shipped;
  rounding the solid slab needs the transparent-surface + rounded-container path).
- [ ] **LOW** — Per-output config (per-output `scale`/`height`/outputs) — deferred.
- [ ] **LOW** — `include = [...]` for the *whole* config (presets already cover theme).
- [ ] **LOW** — `read_parse` policy nit: deleted file → defaults, but parse error →
  keep-last-good. Pick one. (`main.rs:read_parse`)

## Modules & SDK (RFC 0001 / 0003)

- [ ] **MED** — **`Service` layer for non-sway capabilities.** The sway service shares
  one connection; D-Bus/PipeWire need the same before Tier-B, ideally surfaced via
  `Ctx` so third-party plugins use it too (today a module opens its own client).
- [ ] **MED** — **`custom` streaming form.** Only the poll form ships; add `listen_cmd`
  (JSON-line stream), the regex→icon map, and the `alert` danger dot. (`modules/
  custom.rs`)
- [ ] **MED** — **Icon metric-normalization / per-glyph nudge table.** RFC says this
  "must not ship empty"; today icons rely on the raw font baseline. Promote to an
  `ezbar_plugin::ui` helper (`ctx.icon(Icon::…)`).
- [ ] **LOW** — `ui::metric_graph` helper to DRY the four near-identical metric module
  views (cpu/memory/temperature/ping).
- [ ] **LOW** — Restore the workspaces **urgent blink** (dropped in the module port; the
  module would own a 500ms tick like `calendar`). (`modules/workspaces.rs`)
- [ ] **LOW** — `ezbar msg volume` pokes the source directly, so the on-bar % lags up to
  one poll. Route to the volume module instead. (`main.rs:VolumeAdjust`)
- [ ] **LOW** — `clock` **weather**, sway **submap** module — RFC Tier-A leftovers.
- [ ] **LOW** — `ipc_stream` does `remove_file`+`bind` unconditionally; a second manual
  `ezbar` launch steals the socket from a live instance. Probe-before-unlink.
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

- [ ] **MED** — **Integration/runtime tests.** Unit tests cover the parser; the live
  wiring (file-watch reload keeps the active preset, module set rebuilds on placement
  change, multi-instance routing) has none — exactly where the v1 bugs hid.
- [ ] **LOW** — Per-module config reference (a table of every `[modules.<id>]` key).
- [ ] **LOW** — A GIF of the live `▾` switcher cycling presets for the README /
  r/unixporn post (a still doesn't convey "no restart").
- [ ] **LOW** — Mark the RFCs Accepted/Implemented now that the bulk has shipped (they
  still say "Draft v3").

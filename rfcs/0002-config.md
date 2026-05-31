# RFC 0002: Configuration & theming

- **Status:** Draft (v2 — addresses round-1 review: ashell-maintainer, iced/runtime, ricer/visual)
- **Created:** 2026-05-31
- **Target:** ezbar (Rust / iced / wlr-layer-shell)
- **Depends on:** RFC 0001 (modules) — supersedes its `[bar]`/`[[module]]` sketch.

## Changelog (v2)

Round-1 was NACK'd on real holes. Map of fixes:

- **Stable instance identity, fixed.** `(zone, id, occurrence-index)` reordered/
  renumbered on duplicate ids and changed identity across zones — reintroducing the
  ashell teardown it claimed to beat. v2: identity is a user-controllable **`key`**
  (explicit, else `id` for the single-instance common case), **independent of zone
  and position**. Moving/reordering never changes identity.
- **Hot-reload subscriptions, made sound against the iced 0.14 runtime.** Recipe
  identity is `(TypeId, hashed-data, fn-ptr)` — *config is not in it*, so a changed
  poll-interval/symbol baked in a stream closure would be **silently ignored** while
  the module reports "Applied". Fix (v2, round-2): the **host** owns a per-instance
  generation counter and re-keys each module's subscription through its **existing**
  `.with((instance_id, generation))` wrap (`main.rs` already does `.with(instance)`).
  No `Ctx`/trait change — `subscription(&self)` takes no `Ctx`, so the counter must
  live host-side. `reconfigure` returns `Applied { resubscribe } | Reconstruct`.
- **`Factory` hands an *owned* `instance_id`,** not a borrowed `Ctx` (the borrow
  can't be stored). Registry/`Factory`/`reconfigure` are new and **must land in the
  RFC 0001 migration before** hot-reload has anything to call.
- **Theme model rebuilt** from one-scalar-each to a real token set: tiered/structured
  `radius`, a `border` token, an independent `[theme.popup]` (opacity + dim
  `backdrop` scrim — *not* blur), split `spacing`/`padding`, **per-module color
  override**, workspace state colors.
- **Originality fixed.** The v1 example palette was ashell's Tokyo-Night defaults
  byte-for-byte while the prose said "ours". v2's example uses **ezbar's own**
  palette; Tokyo Night is moved to the community-theme bucket. We no longer frame
  the (convergent) semantic-color schema as a differentiator.
- **Added `ezbar msg` IPC** (our own action set) so keybinds drive bar actions +
  the OSD — flagged missing from both RFCs.
- **Surface re-roll honesty:** double-buffered, distinguished from output-removal
  (must not trip the exit-on-bar-close path), closes the popup.

## Summary

A single TOML config — `~/.config/ezbar/config.toml` — driving **placement**,
**per-instance options**, a **token-based theme**, with **hot-reload**, plus an
**`ezbar msg` IPC** for keybind actions. **Zero config == today's bar.** Match or
beat ashell's configurability and looks, on sway, our own way.

## Motivation

Layout is hardcoded; theming is three `const` arrays; no hot-reload; no keybind
IPC. ashell has all four. We close the gap without copying their identity.

## Relationship to ashell (learn, don't copy)

Study their source for *technique*; implement our **own**. No ashell code, assets,
fonts, or default palette is copied. We **deliberately differ**: default style is
ezbar's flat `solid` (islands opt-in); default palette is ezbar's own; placement and
options are co-located; identity leans on graphs (RFC 0003), not a control panel.
Where a choice is just good engineering (tonal palette, file-watch reload), we adopt
the *idea*. We do **not** claim the semantic-color schema as ours — it's convergent
with ashell and with iced's `palette`.

## Design

### File, format, precedence

`$XDG_CONFIG_HOME/ezbar/config.toml`, parsed with `serde` + `#[serde(default)]`
throughout: missing keys fall back to defaults; **no file == the shipped layout**.
`--config <path>` overrides. Secrets/data files stay separate (not in this file).

### Top-level shape

```toml
[bar]
position = "bottom"          # top | bottom
layer    = "top"
height   = 34
outputs  = "all"             # "all" | ["DP-1"]   (per-output scale/height: deferred)
font     = "JetBrainsMono Nerd Font"
scale    = 1.0               # 0 < x ≤ 2

# placement: ordered; nested array = an island group; entry = id | {id,key,config}
left   = [ "workspaces", "window_title" ]
center = [ "clock" ]
right  = [
  ["cpu", "memory", "temperature"],
  "ping",
  { id = "stock", key = "nasdaq", config = { symbol = "NQ=F" } },
  "github", "calendar", "claude",
  ["volume", "battery"],
]

[modules.cpu]        # per-id defaults (merged under each instance's inline config)
show_graph = true
[modules.cpu.theme]  # per-module color override (shadows global tokens)
primary = "#9ece6a"

[theme]
style     = "solid"                      # solid (default) | islands
opacity   = 0.95
font_size = 14                           # bar text size (independent of bar.scale)
spacing   = 6                            # gap BETWEEN items in a zone
padding = 6                              # space INSIDE an island / popup
radius  = { item = 4, group = 8, popup = 10 }   # scalar also allowed
border  = { width = 1, color = "#ffffff14" }    # hairline; separates island from wallpaper
background = { base = "#0d1117", weak = "#161b22", strong = "#21262d" }
text="#e6edf3"; dim="#7d8590"
primary="#58a6ff"; ok="#3fb950"; warn="#d29922"; urgent="#f85149"; separator="#30363d"

[theme.popup]                # the detail surface is its own thing
opacity  = 1.0               # opaque even if the bar is translucent
backdrop = 0.3               # dim SCRIM behind the popup (a quad, NOT gaussian blur)
radius   = 12

[theme.workspaces]           # state-aware, not a flat list
focused="#58a6ff"; occupied="#7d8590"; empty="#30363d"; urgent="#f85149"
colors=["#58a6ff","#3fb950"]; special=["#bc8cff"]
```

The example palette above is **ezbar's own** (its current dark identity). Tokyo
Night, Catppuccin, … are community themes you *drop in*, not the default.

### Placement → instances (RFC 0001 bridge)

Each zone flattens to ordered **entries**: `"id"`, `{ id, key?, config? }`, or a
group array. For each:

- **Identity = `key` if given, else `id`** (good for the common single-instance
  case). Identity is **independent of zone and position** — moving `clock` from
  center to left, or reordering, keeps the same `instance_id = hash(key)` and
  therefore the **same live module, subscriptions, and state**. Two instances of one
  module **must** carry distinct `key`s (validated; duplicate identity = error with
  the offending line).
- The host constructs via the RFC 0001 `Factory(instance_id: u64, cfg: &toml::Value)`
  — an **owned** id (a borrowed `Ctx` can't be stored; modules keep `instance: u64`).
  Config = `[modules.<id>]` defaults ← inline `config` override.
- `[modules.<id>]` is **per-id** (all instances of that id); inline `config` and
  `[modules.<id>.theme]` are **per-instance** overrides.

### Theme: tokens, two layers

- **Host chrome** (bar bg, islands, separators, popup frame, OSD, scrim) uses the
  full token set: `style`, tonal `background.{base,weak,strong}` (omitted tiers
  derived from `base` in **OkLab**, not sRGB, so they don't go muddy), `opacity`,
  `radius.{item,group,popup}`, `border`, `spacing`, `padding`, `[theme.popup]`,
  `[theme.workspaces]`.
- **Modules** receive RFC 0001's `repr(C) ThemeTokens` — *resolved* by the host
  (incl. any `[modules.<id>.theme]` override), plus `background_base` so a canvas
  matches the bar. `ThemeTokens` stays small/`repr(C)` (the phase-2 ABI); the config
  layer is host-only and free to evolve.
- **Islands** wrap a group in a `pill`: `background × opacity`, `radius.group`
  (≈ height/2 for a true pill), `border`. `solid` = one bar bg + hairline
  `separator`s. Modules never know which; the host applies the chrome.

(We expose 3 background tiers, not ashell's 7; deeper layering is derived. If the
derived tiers prove too flat in practice we add named tiers — tracked, not blocking.)

### Hot-reload

```
watch(config DIR) → debounce 150ms → read → parse
   parse err (likely a mid-write read) → retry once after 150ms → still err? keep
       last-good config, show ⚠ chip whose POPUP shows the file:line error → log
   ok → validate (ranges, known ids, unique keys) → err? same keep-last-good path
   ok → diff vs live → apply
```

- **Watch the directory** (editors save via temp-file + rename, firing on the dir,
  not the inode); on a delete/move-from, **re-check existence after a short delay**
  before treating the file as gone (vim/`mv` fire delete-then-create).
- **Never half-apply**: parse+validate fully first; any error leaves the running
  config untouched.
- **Diff/apply:**
  - *theme only* → re-render; no module churn.
  - *a module's config changed* → `reconfigure(&cfg)`:
    - `Applied { resubscribe: false }` — adopted in `view`/`update`, subscription
      kept (its recipe key is `(instance_id, config_generation)`, unchanged).
    - `Applied { resubscribe: true }` — the host **bumps that instance's counter** in
      its `generation: HashMap<instance_id, u64>` and re-keys the module's
      subscription via its outer wrap, `module.subscription().with((instance_id,
      generation))`. Per iced's `With::hash`, changing the wrapped value re-rolls
      **every** recipe the module produced ⇒ old stream dropped, new config in
      effect; the module participates in nothing. **Required** whenever config feeds
      the stream (interval, symbol, target).
    - `Reconstruct` — full rebuild (safe fallback / default).
  - *placement changed* → recompute instance set by `key`: **construct added**,
    `shutdown()` **removed**, reorder the rest (kept instances keep recipes/state —
    recipes are keyed by `instance_id`, already verified to survive reorder).

This is the core correctness story: the host keys every module's subscription by
`(instance_id, generation)` through its **own** `.with((instance_id, generation))`
wrap — never by raw config, and **without the module participating**. iced 0.14
recipe identity is `(TypeId, hashed-data, fn-ptr)` with config nowhere in it, which
is exactly why the **host** (not the recipe, not the module) must own invalidation.
Bump the generation → deterministic re-roll; nothing silently goes stale.

### `ezbar msg` IPC (our own)

A small unix socket (`$XDG_RUNTIME_DIR/ezbar.sock`) so compositor keybinds drive the
bar — and so the **OSD** (RFC 0003) fires on keybind volume/brightness changes, not
only on bar-widget clicks:

```
ezbar msg volume up         # ezbar's own verbs (not ashell's command set)
ezbar msg volume mute
ezbar msg popup toggle <key>
ezbar msg reload
```

Verbs are defined by ezbar and routed to the owning module instance (by `key`) or to
the host. Each carries an optional `--no-osd`.

### Surface lifecycle (re-roll honesty)

`bar.height`/`position`/`layer`/`outputs` changes **re-roll** the layer surface
(can't be live-resized): the host opens the new surface, waits for its first commit,
**then** closes the old one (double-buffer, no visible gap), and **closes any open
popup**. This re-roll is tagged *intentional* so it does **not** trip the
exit-on-bar-close path (today `main.rs` exits when the bar surface closes, for
monitor-removal); the host must distinguish the two. `colors/opacity/style/radius/
border/spacing/padding/font/scale/add-remove-reorder modules/per-module options`
all reload **live** (no re-roll).

### New SDK surface (additive to RFC 0001)

```rust
pub enum Reconfigure { Applied { resubscribe: bool }, Reconstruct }
pub trait Module {
    // … RFC 0001 …
    fn reconfigure(&mut self, _cfg: &toml::Value) -> Reconfigure { Reconfigure::Reconstruct }
}
// Resubscription needs NO Ctx/trait change. The HOST keeps `generation: HashMap<u64,u64>`
// and re-keys via its existing wrap: `module.subscription().with((instance_id, gen))`.
// `subscription(&self)` takes no Ctx, so the generation is host-side by necessity.
```

## Comparison to ashell

| | ezbar (this RFC) | ashell |
|---|---|---|
| Format / placement | TOML, zones + island groups, **placement+options co-located**, **per-instance** | TOML, zones + groups, options in separate tables, per-module |
| Stable identity on reload | ✅ by `key`, zone/position-independent | rebuilds module state |
| Theme: radius/border/popup/scrim | ✅ tiered radius, border, `[theme.popup]` backdrop | ✅ `Radius` tiers, borders, `menu.backdrop` |
| Per-module color override | ✅ `[modules.<id>.theme]` | ⛔ (global only) |
| Background tiers | base + 2 (rest derived, OkLab) | base + 7 |
| Workspace state colors | focused/occupied/empty/urgent/special | colors + special |
| Hot-reload | ✅ live-diff, `config_generation` invalidation | ✅ |
| Keybind IPC | ✅ `ezbar msg` | ✅ `ashell msg` |
| Zero-config default | ✅ = current bar | has defaults too |
| i18n / gradient style | ⛔ deferred | ✅ |

## Migration

1. RFC 0001 **registry + `Factory(owned id, cfg)` + `reconfigure`** land first.
2. `[theme]` resolution (theme-only adoption; no behavior change with no file).
3. Placement (zones/groups/keys) drives the registry; hardcoded list becomes the
   default value.
4. Hot-reload (diff/apply) + `ezbar msg` last.

## Open questions

1. Derived-tier color space confirmed **OkLab**; expose named tiers only if the
   3-tier+derive result is visibly flat.
2. Include files (`include = ["themes/x.toml"]`) for sharable themes — defer.
3. `[modules.<id>.theme]` token coverage — start with the semantic colors; expand on
   demand.

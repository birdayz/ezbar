# RFC 0003: Built-in modules & visual system

- **Status:** Draft (v2 — addresses round-1 review: ashell-maintainer, iced/runtime, ricer/visual)
- **Created:** 2026-05-31
- **Target:** ezbar (Rust / iced / wlr-layer-shell)
- **Depends on:** RFC 0001 (module SDK), RFC 0002 (config, theme tokens, `ezbar msg`)

## Changelog (v2)

- **Services are host-owned singletons,** not per-module subscriptions. Reviewers
  showed per-module D-Bus breaks on singletons (an SNI host registering twice is a
  protocol problem; two PipeWire consumers = two connections). v2 defines a
  host-driven `Service` keyed by *type*, ref-counted by subscriber count, fanned to
  modules.
- **Tier B split by real cost** and the comparison table no longer claims shipped
  parity. ashell's `services/` is ~10k LOC (network alone ~3.3k incl. generated
  D-Bus bindings); v1 hid that behind one paragraph.
- **Icon system honest + metric-correct.** "Nerd Font + typed enum" is ashell's
  *baseline*, not a differentiator; we say so. Our `Icon→codepoint` map is authored
  from the upstream Nerd Font cheat-sheet (not transcribed from theirs); **no custom
  font**. Glyph **baseline/advance normalization** (fixed-width box, vertical-center)
  is promoted to first-class — it's where "non-programmer-made" actually lives.
- **No in-tree `animated_size`.** v1's description matched ashell's widget near
  verbatim (copying risk). v2 uses the external `iced_anim` crate; honest about its
  real cost (it needs `Animated<T>` state + a tick message; the host owns the island
  chrome).
- **Component framing honest:** the decomposition is what any iced bar converges on;
  implementations are independent; we build our **own** button taxonomy (not ashell's
  Solid/Outline × Primary/Danger state matrix).
- **`metric_graph` promoted** to a first-class shared component (the graph-forward
  identity, not six bespoke canvases).
- **A real "Default appearance" section** (palette, modules, look) — defaults are
  what 90% of users see.
- **`custom` module matched to ashell's** (streaming `listen_cmd`, regex→icon map,
  alert dot), bound to the subscription tier.
- **OSD designed** and coupled to `ezbar msg` (RFC 0002); **notifications** daemon is
  a hard refuse-if-owned, not an open question.

## Summary

A **visual foundation** (islands, an icon font with correct metrics, animations, a
shared component library, a first-class sparkline) plus a **built-in catalog** that
reaches modern-bar parity our own way, **keeping** ezbar's GPU graphs + dev widgets.
On RFC 0001 (`Module`s) and RFC 0002 (placement/theme/IPC).

## Relationship to ashell (learn, don't copy) + identity

Study their source for *how*; implement our **own**. No code, assets, fonts, or
glyph maps copied. Identity that's actually ours: **graph-forward** (GPU sparklines
are first-class — ashell has zero canvas graphs), the **dev widgets** (GitHub/
kubectl/stock/Claude/Spotify ashell lacks), and **sway**. The Part-2 desktop
capabilities (tray, media, settings) are *table stakes* across waybar/eww/ashell —
not ashell features we clone. "Flat default" only counts if the flat look is
*designed* (below), not inherited emoji.

## Part 1 — Visual system

### 1a. Islands / solid (host-owned)
From `theme.style` (RFC 0002). `islands` → host wraps each group in a `pill`
(`background × opacity`, `radius.group`, `border`); groups float with `spacing`; the
bar surface is transparent. `solid` → one bar bg + hairline `separator`s. Modules
draw content only — already how the host works (it inserts separators around each
`view` today).

### 1b. Icon font (correct metrics, our own map)
We use a **Nerd Font** — the same neutral baseline ashell uses; this is *not* a
differentiator, just the sane choice over emoji (today's `💾🌡️📅🤖` don't theme,
don't align, vary per font stack). What we own:
- A typed `Icon` enum with an `Icon→codepoint` map **authored from the upstream Nerd
  Font cheat-sheet**, not transcribed from ashell's enum. **No bundled `.otf`.**
- **Metric normalization is first-class** (the actual polish): Nerd glyphs have
  inconsistent vertical metrics + advance widths, so a naive `row![icon, text]` makes
  icons ride high and jiggle. Every icon renders in a **fixed-advance box,
  vertically centered to the text baseline**, with an optional per-glyph nudge table.
  Exposed as `ctx.icon(Icon::Cpu) -> Element` (host owns the font + box), so plugins
  get aligned icons for free. The per-glyph nudge table is **authored by ezbar** for
  the supported Nerd Font glyph set and shipped in `ezbar-plugin`; it is the
  load-bearing polish item — it must **not** ship empty, or icons jiggle and the
  "designed default" wobbles. The font is registered at daemon startup (`.font(...)`).

### 1c. Animations
Use the external **`iced_anim`** crate (compatible with iced 0.14), **not** an
in-tree widget. Honest cost: animated values need `Animated<T>` state + a tick
message; since the **host** owns the island/popup chrome, the host holds that state
and drives ticks — modules don't animate their own width. Used for popup
expand/collapse, group/center width changes, toggle active-color fades. **Gated by
`theme.animations`**; off = instant, zero cost (the escape hatch).
**Center re-balancing:** with independent left/center/right zones, a truly centered
element must account for left+right widths or it twitches when the title grows; the
host animates the center element's offset (our own impl).

### 1d. Shared component library (`src/widgets/ui/`)
Cohesion comes from a shared vocabulary every builtin (and plugin, via re-export)
uses. The *decomposition* is what any iced bar converges on (ashell, eww too); our
**implementations are independent**, and we author our **own** button taxonomy
rather than copy a style matrix:

| component | use |
|---|---|
| `button` / `icon_button` | clickable chrome (our own size/kind taxonomy) |
| `slider` | volume, brightness |
| `toggle` | on/off + optional submenu in the settings panel |
| `popup_frame` | the dark rounded popup chrome (one themed definition; `[theme.popup]`) |
| `metric` | "icon + value", threshold-colored |
| **`metric_graph`** | **icon + value + inline GPU sparkline** — themed fill (gradient-under-line at low alpha), fixed graph **height + width (sample-count)** tokens. The graph-forward identity, shared so cpu/mem/temp/ping/net/disk are visually identical, not bespoke. |
| `pill` | the islands container |

## Default appearance (what 90% see)

Zero-config must look **designed**, not "today's emoji bar":
- `style = solid`, ezbar's own dark palette (RFC 0002 example: `#0d1117` bg, `#58a6ff`
  primary, `#3fb950/#d29922/#f85149` ok/warn/urgent), hairline `separator`, `spacing
  6`, the icon font (no emoji).
- Default modules on: `workspaces · title │ cpu mem temp(graphs) · ping · clock ·
  volume battery`. Dev widgets (github/stock/claude/kubectl/calendar/spotify) ship but
  are **opt-in** in config (they need tokens/setup).

```
 1  2  3   code — main.rs          ▁▃▅ 12%  ▂▂▃ 41%  ▁▁▂ 47°   28ms   14:23   ◢ 62%  ▮ 88%
└ workspaces ┘└ title ┘           └ cpu ──── mem ──── temp(sparklines) ┘ ping  clock  vol  bat
```

(`islands` flips the look to floating rounded pills with `border` + `backdrop` popups.
A real screenshot lands with the implementation.)

## Part 2 — Built-in catalog

Every entry is an RFC 0001 `Module`, placed/configured via RFC 0002.

### Tier A — bar modules (no D-Bus; land first)

| module | origin | does | popup |
|---|---|---|---|
| workspaces | have | sway ws; **state colors** (focused/empty/urgent), **special/scratchpad** ws, **visibility modes** (all/monitor/exclusive), click-switch, **scroll with trackpad pixel-accumulator** | — |
| window_title | have | focused title/app-id; truncate modes; feeds center re-balance | — |
| clock | have→extend | time/date **+ calendar + weather** | calendar+forecast |
| cpu / memory / temperature | **have (graphs)** | `metric_graph` (GPU sparkline + threshold color) | — |
| disk / net / ip | new | `metric` / `metric_graph` (net throughput) | — |
| ping | have | latency `metric_graph` | — |
| updates | new | `check_cmd` count; click runs `update_cmd` | list |
| keyboard layout / submap | new | layout / sway submap | switch |
| battery / volume | have | level icon + %; volume → **OSD** on change | — |
| github / kubectl / stock / spotify / claude | **unique** | ezbar-only; keep | existing |
| `custom` | new | **command-driven, no-code** (below) | optional |

**`custom` (matched to ashell's, our impl):** a `poll` command *or* a long-lived
**`listen_cmd`** streaming JSON lines (`{text, alt}`, swaync-style); a **regex→icon
map** (swap glyph by output, not one fixed icon); an **`alert` regex** that paints a
danger dot; click runs a command. **Bound to the subscription/`Task` tier** (never
`update`/`view`) — it's the one module a non-Rust user can wedge.

### Tier B — desktop/control (host services; phased by cost)

**Cheap (land after Tier A):** `tray` (SNI), `media` (MPRIS + art + transport),
`osd`, `privacy` (PipeWire mic/cam/screenshare dots).

**Expensive (demand-gated, not blanket parity):** `settings` quick-settings panel —
audio sink/source + `slider` and `brightness` `slider` **first**; then power menu
(per-action commands), peripheral battery (kbd/mouse/headset via UPower), idle
inhibitor; **network (WiFi scan + password dialog) and bluetooth gated on demand**
(largest lift, most duplicative of existing applets); `notifications` daemon.

**OSD design:** a transient layer surface, center-bottom, `slider`/arc + icon,
animate in/out, auto-dismiss (~1.5 s), **driven by `ezbar msg volume/brightness …`**
(RFC 0002 IPC) *and* by widget interaction — without the IPC it's only half an OSD.

**Notifications:** acquire `org.freedesktop.Notifications` **without replace** and
**refuse to start if mako/swaync holds it** (hard requirement, not optional).

### The `services` layer (host singletons)

```rust
/// Host-internal: one long-lived D-Bus (or proto) connection, owned by the HOST.
trait Service { type Event; fn run(&self) -> Subscription<Self::Event>; }
```

- The host owns a `Services` registry keyed by **service type**; a service is spun up
  **once**, lazily, the first time a configured module needs it, and torn down when
  the last subscriber goes (ref-counted). **Modules never see `Service` or its
  `Subscription`** — they receive **host-defined `repr(C)`/serde data** via a fanned
  `Message` or a `ctx.service::<Audio>()` handle (consistent with RFC 0001: no
  `Any`/`TypeId` across the boundary). This is the SDK addition that makes "lazy +
  isolated" actually hold for singletons (SNI host, one PipeWire connection).
- Default bar-only config starts **no** services (footprint ≈ today).

## Comparison to ashell (target, honestly phased)

| | ezbar | ashell |
|---|---|---|
| islands / solid / icons / animations / components | ✅ (ours) | ✅ |
| **system graphs** | **GPU `metric_graph`** | text indicators |
| tray / media / privacy / OSD | 🟡 Tier B cheap (planned) | ✅ shipped |
| settings panel (audio+brightness) | 🟡 Tier B (planned) | ✅ shipped |
| settings: network/bluetooth/VPN | 🟠 demand-gated | ✅ shipped |
| notifications daemon | 🟡 planned, refuse-if-owned | ✅ shipped |
| dev widgets / programmable plugins | ✅ unique | ⛔ |
| compositor | sway | Hyprland/Niri |

(🟡 planned, 🟠 maybe — we do **not** claim shipped parity for Tier B.)

## Phasing

1. **Visual foundation** (Part 1) + re-skin existing widgets + the designed default —
   immediate "looks nice" win, no new module.
2. **Tier A** ports (disk/net/ip, updates, keyboard, `custom`, clock+weather).
3. **`services` singletons + Tier B cheap** (tray → media → osd → privacy).
4. **Tier B expensive**, audio/brightness first; network/bt demand-gated.

## Open questions

1. Curated ezbar icon subset vs depend on a Nerd Font (default: depend; revisit if
   missing glyphs/alignment bite).
2. Does `media` (MPRIS) retire the bespoke Spotify widget, or both stay?
3. Settings-panel scope ceiling — commit to audio+brightness+power; gate the rest.

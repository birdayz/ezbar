# RFC 0021: Responsive widget retraction (small/standard forms)

- **Status:** **Proposed**
- **Created:** 2026-06-26
- **Target:** ezbar (Rust / iced / wlr-layer-shell)
- **Depends on:** RFC 0001 (pluggable modules), RFC 0005 (separators & grouping),
  RFC 0006 (WASM plugins), RFC 0004 (per-output surfaces)

## Summary

Let every widget declare **two forms** — a **standard** form and a **small** (retracted)
form — and have the **host** choose between them per surface so the bar fits its output.
When space is scarce the host renders lower-priority widgets small; when there's room it
renders everyone standard. The decision is made **per output**, at iced **layout time**,
where exact widths are known — so the same config renders full on a wide monitor and
retracted on a narrow one with no per-machine config.

## Motivation

The bar composes widgets left/center/right at their **natural size** and the layer surface
is a fixed-width strip — so on a narrow output the right cluster simply **clips** off the
edge. There is no width budgeting today: each module's `view()` renders at whatever size its
content wants (`src/main.rs:2033` `build_widgets`, `src/main.rs:2199` `bar_view`), and the
host never learns a widget's width before render or reacts to overflow. The only existing
size control is per-module, content-specific truncation a few modules opt into by hand
(`window_title` ellipsis `src/modules/window_title.rs:65`, `spotify` marquee
`src/modules/spotify.rs:127`, `media` clip). Nothing is host-driven and nothing is
output-aware.

The pain is real and **output-shaped**: with the same module set, a 5120-wide ultrawide has
room to spare while a 3840 laptop panel overflows. The fix should be one mechanism the host
applies with the width it already tracks per surface (`BarSurface { output, width }`,
`src/main.rs:682`), not N modules each hand-rolling a shrink.

**Why "shrink" not just "hide".** Dropping a widget loses information; a *retracted* form keeps
the signal in less space — a clock shows `14:05` not `Mon Jun 26 14:05`, a cpu chip shows its
sparkline without the `12%` label, a calendar shows its glyph+countdown without the title.
Hiding is the *last* resort (§6), retraction is the first.

## Goals / Non-goals

**Goals.** A per-widget standard/small declaration on **both** render paths (native `Module`
and WASM `Plugin`); **host-driven** selection from real measured widths; **per-output**
behaviour for free; a **priority** order so the right things shrink first; backward
compatibility (a widget that declares no small form simply never retracts).

**Non-goals.** Graduated multi-level shrink (start binary — §"Alternatives"); reflowing
widgets between zones; an overflow/"more" menu (a possible follow-up, §6); animating the
transition; changing what a widget *means* when small (the author owns that).

## Design

### §1 — Two forms, declared by the widget (opt-in)

A widget may offer a **small** form in addition to its standard `view`. Declaring none means
"I never retract" — so this is purely additive and every existing widget keeps working.

**Native `Module`** (`crates/ezbar-plugin/src/lib.rs:191`) gains one defaulted method:

```rust
pub trait Module: Send {
    fn view(&self, ctx: &Ctx) -> iced::Element<'_, ModMsg>;     // standard (unchanged)

    /// The retracted form, when the host asks the bar to save space. `None` (the default)
    /// means this widget never retracts — it always renders `view()`.
    fn view_small(&self, ctx: &Ctx) -> Option<iced::Element<'_, ModMsg>> {
        let _ = ctx;
        None
    }
    // ...
}
```

**WASM `Plugin`** (`crates/ezbar-plugin-wasm/src/lib.rs:454`) gains a parallel method, lowered
to a new WIT export (§4):

```rust
pub trait Plugin {
    fn view(&self) -> Render;                    // standard (unchanged)
    fn view_small(&self) -> Option<Render> { None }   // retracted, or None = never retracts
}
```

Both small forms stay **pure** (no host calls, no events) exactly like `view()` — they are a
second pure projection of the same precomputed state, so nothing about the update/subscription
model changes.

### §2 — `RetractingRow`: fit at layout time

The crux is *measuring*. WASM view-trees are already measurable off-thread (the bounded node
DSL + `measure()` at `crates/ezbar-wasm/src/lib.rs:2293`), but native modules return arbitrary
iced `Element`s (canvas, etc.) whose width iced only computes during its own layout pass — and
the bar's `view()` runs without a renderer, so it cannot pre-measure them.

Resolution: don't pre-measure. Push the decision **into a custom iced widget** that runs at
**layout time**, where it *does* have the renderer and the surface's width limit. The host
composes each retractable widget as a pair and hands them to a `RetractingRow`:

```rust
struct RetractItem<'a> {
    standard: Element<'a, Message>,
    small:    Option<Element<'a, Message>>,   // None ⇒ pinned (never retracts)
    priority: i32,                            // lower retracts sooner (§3)
}

// A Widget whose `layout(limits)` knows the exact bar width.
impl Widget for RetractingRow {
    fn layout(&self, …, limits: &Limits) -> Node {
        // 1. lay out every child's STANDARD element → exact widths, sum + spacing.
        // 2. if total ≤ limits.max().width: keep all standard. Done.
        // 3. else retract children in ascending `priority`, one at a time, swapping to
        //    `small` (re-layout that child) and re-summing, until it fits or all
        //    retractable children are small.
        // 4. lay out the chosen forms into the final row Node.
    }
}
```

This is the clean unifier: by layout time **everything is an iced `Element`** — native
elements and lifted-from-WASM elements alike — so one widget measures and fits both paths with
exact widths and zero pre-pass plumbing. iced already lays each child out once; this lays the
retracted children at most twice (cheap; the bar has ~10 widgets). Both forms are built every
frame in `view()` (also cheap — building an `Element` is just allocating a tree, no render).

Because `layout()` receives the **surface's** width limit, and surfaces are per-output (RFC
0004), retraction is automatically **per monitor** with no per-output config.

### §3 — Priority: what shrinks first

Retraction order is by an integer **priority** (lower = retract sooner). It comes from:

1. a per-widget **default** baked into the module/plugin (e.g. `clock` defaults high — keep it
   readable longest; a decorative graph defaults low), and
2. an optional **config override** `retract_priority = <int>` in `[modules.<id>]`
   (`src/config.rs:538` module tables), so the user tunes their own bar.

Ties break by **distance from the bar's anchor edge** — the outermost widget of the
overflowing side retracts first, so shrink eats inward and the end-caps (clock/tray) stay full
longest. This reuses the same "keep the edges" instinct as RFC 0005's grouping. A widget with
no small form (`view_small → None`) is **pinned**: never retracted, never counted as
retractable (but still counted in the width budget).

### §4 — WASM contract & versioning

Add one export to the `plugin` world, mirroring `view`:

```wit
// wit/since-v0.7.0/world.wit  (new version dir; since-v0.6.0 is current)
world plugin {
    // … unchanged …
    export view: func() -> tree;
    export view-small: func() -> option<tree>;   // NEW: none ⇒ never retracts
    export popup: func() -> option<tree>;
}
```

This is **backward compatible** under RFC 0006's version-window model: a plugin built against
≤ v0.6 has no `view-small` export, so the host treats it as `None` (pinned, never retracts) —
exactly the "didn't opt in" default. New plugins targeting v0.7 may export it. The host lifts
the optional small tree the same way it lifts `view` (`crates/ezbar-wasm/src/lib.rs` glue),
caching both `Element`s for the frame; `RetractingRow` picks one at layout time. No change to
`update`/events — `view-small` is pulled (pure), never pushed.

### §5 — What "small" should be (author guidance, not enforced)

The small form is the author's call, but the house style:

- **Drop the label, keep the signal**: `cpu` → sparkline only; `calendar` → glyph + countdown,
  no title; `clock` → `HH:MM`, no date; `net` → arrows + rate, no `eth0`.
- **Icon-only** is a fine floor for a status glyph (battery, volume, keyboard layout).
- Keep the **hover popup** identical — retraction shrinks the *chip*, the full detail is one
  hover away (and the popup is already separately sized, `MODULE_POPUP_SIZE`).

### §6 — Last resort: hide, with the popup as the escape hatch

If even all-small doesn't fit (a genuinely tiny output), `RetractingRow` **drops** the
lowest-priority *retracted* widgets entirely, lowest priority first, pinned widgets last-ever.
A dropped count could later surface in an overflow popup; v1 just drops and logs, so the bar
never visibly clips. (Open question: drop vs. a `…` overflow affordance — see below.)

## Phasing

1. **Host machinery** — `RetractingRow` + wire it into `build_widgets`/`bar_view`
   (`src/main.rs`), reading `retract_priority` from config. Lands inert (no widget has a small
   form yet) but measurable against the existing layout.
2. **WASM path** — `wit/since-v0.7.0`, the `view-small` export + SDK `Plugin::view_small`,
   host lift/cache of the optional tree. Update `claude`/`calendar`/a graph plugin to declare
   small forms (the claude chip is the poster child: standard = `🤖 7 $188/hr ▁▂▃ 182 t/s 5h
   46%`, small = `🤖 7 $188/hr`).
3. **Native path** — `Module::view_small` + small forms for the chatty native modules
   (`clock`, `cpu`, `net`, `window_title`, `workspaces`).

Each phase is independently shippable; widgets opt in over time.

## Alternatives considered

- **Width hints instead of layout-time fitting** (`Module::width_hint(form) -> f32`): lets the
  host budget in `view()` without a custom widget, but every author must hand-estimate widths
  that drift from reality (fonts, themes, fractional scale) — and it still can't measure native
  canvas content. `RetractingRow` measures the truth for free. Rejected.
- **Hide-only (no small form)**: simpler, but loses information and the user explicitly wants
  *shrink*. Kept as the §6 floor, not the mechanism.
- **Graduated levels** (`view_at(level)`): more flexible, but binary covers the stated need and
  keeps the contract a single extra method. Priority already gives ordered, staged behaviour
  across *widgets*; per-widget multi-step can come later behind the same `RetractingRow`.
- **Auto-truncate text host-side**: only helps text, not graphs/icons/canvas, and mangles
  meaning blindly. The author's `view_small` is the meaning-preserving version.

## Open questions

1. **Priority defaults**: ship a default `retract_priority` per built-in, or default everyone
   equal and lean on edge-distance + user config? (Leaning: sensible per-built-in defaults.)
2. **Drop vs. overflow affordance** (§6): silently drop the tail, or collapse it into a `…`
   popup? (Leaning: drop + log for v1, overflow popup as a later RFC.)
3. **Hysteresis**: a widget oscillating around the fit threshold could flicker between forms on
   width-jitter (e.g. a clock ticking seconds). Add a small dead-band / sticky bias so it
   doesn't thrash. (Leaning: yes, bias toward the current form by a few px.)

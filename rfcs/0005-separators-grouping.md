# RFC 0005: Configurable separators & widget grouping

- **Status:** Draft (v1 — implemented + r/unixporn-reviewed on `rfc-0005-separators`)
- **Created:** 2026-06-01
- **Target:** ezbar (Rust / iced / wlr-layer-shell)
- **Depends on:** RFC 0002 (config, `style`, `[theme].separator`, placement groups)
- **Supersedes:** RFC 0002's one-line separator sketch — separators get a real model.

## Summary

Give the bar **two composable ways to separate widgets**, both config-driven:

1. **Grouping** (macro structure) — the placement already supports groups
   (`Entry::Group`, a nested array). Each group renders as **its own sub-island** in
   `islands` style (the gaps between islands *are* the separators), or a
   **divider-joined run** in `solid`. The shipped default groups the right cluster
   into a few semantic pills.
2. **Separator mark** (micro, optional) — `[theme.separator].style` draws an optional
   mark *between widgets within a group*: `none` (the islands default), `dot` (`·`),
   `line` (a hairline), or `glyph` (a custom string).

Zero-config gives the r/unixporn-conform look: square dark **clusters of pills**, lilac
edges, no glyph clutter.

![default — clusters of pills (5 grouped sub-islands)](assets/0005-grouped-pills.png)

## Motivation

The islands style rendered the whole right zone as **one slab** — 13 widgets in a
single bordered panel, separated only by spacing. They blur together. The obvious fix
(a `|` between each) is exactly what an r/unixporn review *killed* at 9/10 ("reads like
a CSV row"). So "add separation back" must not mean "add dividers back".

A ricer review prescribed the answer: **lean into the islands identity.** The theme is
literally named *lilac islands* — negative space is supposed to be the divider. One
13-widget slab is an island cosplaying as a solid bar; a few square sub-islands is the
premium version of the look already shipped (the square-dark-lilac take on the
ashell/eww "pills" aesthetic). Grouping adds **zero ink**, reinforces the identity, and
gives free semantic clustering. But taste varies (solid bars, minimalists, glyph fans),
so the separator itself is configurable.

## Design

### Config surface

```toml
[theme]
spacing   = 8         # gap between widgets WITHIN a group (px)
group_gap = 14        # gap between groups: sub-island gap (islands) / divider gap (solid)

[theme.separator]
style = "none"        # none | dot | line | glyph  — the optional within-group mark
color = "#585b70"
width = 1             # line thickness / dot size (px)
glyph = "|"           # used when style = "glyph"
# shorthand: `separator = "#585b70"` ⇒ a line of that colour; a bare `glyph` ⇒ glyph style.
```

The `~1.75×` ratio between `spacing` (8) and `group_gap` (14) is load-bearing: the eye
chunks by *relative* gap, so the inter-group gap must read as distinctly bigger than the
intra-group one. Don't let them converge.

### Grouping → rendering

Placement partitions each zone into **groups**: a top-level `Entry::Group` is one group;
a bare entry is its own singleton group; an empty zone uses the shipped default groups.

- **Islands:** each group is its own `container(...).padding([2,10]).style(pill)`
  sub-island; islands are laid out with `group_gap` between them. Within a group, widgets
  get `spacing` + the optional separator mark. *The gaps are the separators.*
- **Solid:** one slab; groups are joined by a divider sitting in a `group_gap` (the
  separator mark, or a hairline when `style = none` so a flat slab is never undivided);
  widgets within a group get `spacing`.

### Separator mark

`style` draws between adjacent widgets *within a run* (never before the `▾` switcher):

| style   | mark |
|---------|------|
| `none`  | nothing — pure spacing (islands default; grouping carries the structure) |
| `dot`   | a dim middle dot `·` in `separator.color` |
| `line`  | a `width`-px vertical hairline in `separator.color` |
| `glyph` | the `separator.glyph` string in `separator.color` |

### Default groups

The shipped right-cluster default (semantic, ~even widths, `clock` last as an end-cap):

```
[cpu memory temperature]  [ping github claude]  [calendar kubectl spotify]
[stock volume battery]    [clock ▾]
```

Five sub-islands, not one slab and not one-per-widget (which would be "island soup").
Fully overridable via the `right = [ […], […] ]` placement.

## r/unixporn review (what tuned the defaults) — **10/10**

Iterated against a ricer reviewer across eight rounds to a pixel-verified **10/10**:

- **One slab → grouping (option D over dots/hairlines):** dots/lines are "a softer CSV
  comma" and a hairline fights the pill's own border + the sparkline axes. Grouping adds
  no ink — chosen.
- **4 groups, then 5:** four was the right *count* (not soup) but the status+time group
  was a "plank" 3–4× the others → split `clock ▾` into a dedicated end-cap → five even
  pills.
- **Border `@50% → @75% → @90%` + a drop shadow:** a lilac edge alone washed out over
  bright wallpaper; the framing now rests on a soft drop shadow (`#000@45%`, y+2, blur 8)
  that lifts each pill off *any* wallpaper, with the `@90%` lilac as accent.
- **Inline sparklines harmonised:** the idle/ok tier recoloured saturated green → flieder
  lilac (green now means only one thing in the bar — money up, on the stock spark); the
  inline graphs shrunk 80→48px and lifted off the baseline with a line-anchored gradient
  area-fill, so even an idle trace reads as a low area chart, not a flat "underline".

## Migration / implementation

Implemented on `rfc-0005-separators`:
- `config`: `SepStyle` enum + `separator.style`/`width`; `[theme].group_gap`; `spacing`
  now means *intra-group* and defaults to 8.
- `main`: `resolve_right_groups` (zone → `Vec<Vec<Placed>>`); `build_widgets` (a run with
  `spacing` + `sep_mark`); `bar_view` renders groups as sub-islands (islands) /
  divider-run (solid); the `▾` switcher trails the last right group.
- `presets/ezbar-dark`: lilac border `@90%`.

No module changes. Zero-config behaviour changes from "one right slab" to "five grouped
pills" — that's the point.

## Resolved in this pass

- Inline graphs seated on the text line (20→16px), idle traces lifted off the baseline
  with a gradient area-fill, and recoloured to the lilac palette (was the review's last
  blocker). The pill drop shadow makes the framing wallpaper-independent.

## Open questions

1. **Idle placeholders.** A `spotify`/`kubectl` showing `--` makes its pill look empty.
   Collapsing an idle widget (`Module::visible()`) would tidy the row — a module concern,
   tracked separately.
2. **Per-output groups.** Pairs with RFC 0004's per-output surfaces: a narrow laptop
   panel might want fewer/tighter groups than a 5120 monitor. Future.
3. **Configurable shadow / spacing.** The pill drop shadow and the `8/14` spacing are
   sensible constants today; exposing them as `[theme]` knobs is a small follow-up.

# ezbar module config reference

Every placed module reads its options from a `[modules.<id>]` table in
`~/.config/ezbar/config.toml`, where `<id>` is the placement id (RFC 0001). This lists
every key the built-in modules read, with defaults. Theme/bar keys (`[theme]`, `[bar]`,
`[presets.*]`, `$palette`) are documented in `rfcs/0002-config.md`.

```toml
[modules.cpu.graph]      # a module's options; sub-tables (like [graph]) nest under it
samples = 60
```

## Modules with no options

These are placed by id alone and read no config: `battery`, `calendar`, `claude`,
`github`, `kubectl`, `spotify`, `stock`, `volume`.

## Per-module options

| Module | Key | Type | Default | Notes |
|--------|-----|------|---------|-------|
| `clock` | `format` | string | `"%Y-%m-%d %H:%M:%S"` | strftime format |
| `cpu` | `[graph]` | table | — | see [Graph sub-table](#graph-sub-table) (samples default 30) |
| `memory` | `[graph]` | table | — | graph (samples default 20) |
| `temperature` | `[graph]` | table | — | graph (samples default 60) |
| `ping` | `target` | string | `"8.8.8.8"` | host to ping |
| | `[graph]` | table | — | graph (samples default 40) |
| `disk` | `path` | string | `"/"` | filesystem to report |
| | `interval` | int (s) | `30` | poll cadence |
| | `icon` | string |  (hdd) | Nerd-Font glyph |
| `net` | `interface` | string | `""` (auto) | NIC name, empty = autodetect |
| | `interval` | int (s) | `2` | |
| | `icon` | string |  (globe) | |
| `ip` | `interval` | int (s) | `30` | |
| | `icon` | string |  (globe) | |
| `updates` | `interval` | int (s) | `3600` | |
| | `check_cmd` | string | `"checkupdates"` | command that lists updates (one line each) |
| | `update_cmd` | string | — | command run on click |
| | `icon` | string |  (cloud-download) | |
| `keyboard` | `icon` | string | — | |
| `media` | `max_len` | int | `40` | truncate "artist – title" (clamped 8–200) |
| `window_title` | `max` | int | `80` | truncate; `0` = no limit |
| | `format` | string | `"{title}"` | `{title}` placeholder + [markup](#inline-markup) |
| `workspaces` | `style` | string | `"boxed"` | `boxed` \| `filled` \| `outlined` \| `underbar` |
| | `animate` | bool | `false` | opt-in focus cross-fade (RFC 0010) |
| `custom` | `command` | string | — | poll: run every `interval`, show stdout |
| | `interval` | int (s) | `5` | poll cadence (min 1) |
| | `listen_cmd` | string | — | stream: run once, each stdout line updates (wins over `command`) |
| | `icon` | string | — | fallback glyph |
| | `on_click` | string | — | shell command run on click |
| | `alert` | regex | — | urgent danger-dot when output matches |
| | `[[icons]]` | array | — | `{ match = "regex", icon = "glyph" }`, first match wins |

`custom` output and `window_title`'s `format` accept the [inline markup](#inline-markup)
subset.

## Graph sub-table

`[modules.<id>.graph]` for the sparkline metric modules (`cpu`, `memory`, `temperature`,
`ping`). Numeric values are clamped to the shown bounds (RFC 0002).

| Key | Type | Default | Bounds |
|-----|------|---------|--------|
| `samples` | int | per-module (cpu 30, memory 20, temperature 60, ping 40) | 2–2048 |
| `width` | float (px) | `48` | 8–400 |
| `height` | float (px) | `16` | 6–200 |
| `line_width` | float (px) | `1.5` | 0.5–8 |
| `fill` | bool | `true` | gradient area fill under the trace |
| `line_color` | string | threshold | a theme token (`accent`/`ok`/`warn`/`urgent`/`fg`/`fg_dim`), `#rrggbb[aa]`, or `threshold` (green→red by load) |

## WASM plugins

A `.wasm` dropped in `~/.config/ezbar/plugins/` is placeable by its file stem. Its
`[modules.<id>]` table grants read-only capabilities (default-deny; RFC 0006/0012/0013),
and **all** other keys are passed verbatim to the plugin's own `load(config)`.

| Key | Type | Capability |
|-----|------|------------|
| `network` | string or array | host allow-list for `http_get` (case-insensitive, port-agnostic, no subdomain match) |
| `feeds` | string or array | host-sampled metrics: `cpu` / `memory` / `temperature` / `battery` / `net` |
| `sway` | bool | read-only sway snapshot (workspace list + focused title) |

Capability consent is bound to the plugin's **content hash** (`grants.toml`, RFC 0014): a
swapped binary withholds all capabilities until re-approved with `ezbar grant <id>`.

## Inline markup

`custom` output and `window_title`'s `format` accept a small themed subset (RFC 0002,
*not* Pango):

- `[c=TOKEN]…[/c]` — colour a span. TOKEN ∈ `accent` `fg` `fg_dim` (`dim`) `ok` (`good`)
  `warn` (`warning`) `urgent` (`error`/`bad`) `sep`.
- `[b]…[/b]` — bold a span.

Tags nest; anything that isn't a valid tag is shown literally, so markup-free text is
unaffected. Example:

```toml
[modules.net]
command = "ping -c1 -W1 1.1.1.1 >/dev/null && echo '[c=ok]up[/c]' || echo '[c=urgent]down[/c]'"
```

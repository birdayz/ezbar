# ezbar

A status bar for [Sway](https://swaywm.org). GPU-rendered with
[iced](https://iced.rs) on `wlr-layer-shell` — no GTK, no config DSL, no IPC.
Widgets are just Rust.

Workspaces, focused-window title, clock, CPU / temperature / memory / ping
graphs, volume, battery, GitHub notifications, Google Calendar, Spotify, kubectl
context, a stock ticker, and a live Claude-usage widget. Clicking, scrolling and
hover popups all work; popups are real layer-shell surfaces, not overlays.

## Build

Needs stable Rust (≥ 1.88) and a Wayland + Vulkan stack.

**Arch:**

```bash
sudo pacman -S --needed rust wayland libxkbcommon vulkan-icd-loader fontconfig
# + a driver: vulkan-radeon | vulkan-intel | nvidia-utils
```

**Debian/Ubuntu:**

```bash
sudo apt install pkg-config libwayland-dev libxkbcommon-dev libvulkan1 \
                 mesa-vulkan-drivers libfontconfig1-dev
```

Then:

```bash
cargo build --release        # -> target/release/ezbar
```

## Run

```bash
./target/release/ezbar
```

Add it to your Sway config:

```
exec /path/to/ezbar
```

`ezbar` is a thin launcher: it respawns the bar if the output disappears (monitor
sleep / hotplug). For a single foreground instance (debugging):

```bash
EZBAR_CHILD=1 ./target/release/ezbar
```

## Configure

Everything lives under `~/.config/ezbar/` and is optional — a widget with no
config just stays quiet. Nothing is hardcoded; no secrets ship in the binary.

| Widget   | Reads from |
|----------|------------|
| Calendar | `calendar_url` (your secret iCal URL) or `$GOOGLE_CALENDAR_ICAL_URL` |
| GitHub   | `$GH_TOKEN` / `$GITHUB_TOKEN` / `gh auth token`; optional `github_config.json` (`reasons`, `exclude_repos`) |
| Spotify  | `spotify_config.json` (`client_id`, `client_secret`); or `$SPOTIFY_ACCESS_TOKEN` |
| Stock    | `$EZBAR_STOCK_SYMBOL` (default `NQ=F`), `$EZBAR_STOCK_API_KEY` (optional) |
| Ping     | target hardcoded to `8.8.8.8` |

## Interactions

| Widget | Action |
|--------|--------|
| cpu / temp / mem / ping | click the label to toggle its graph |
| volume | click to mute, scroll to change |
| kubectl | left-click clears the context, right-click opens the picker |
| calendar | click for today's meetings; blinks when one is imminent/ongoing |
| github | click for the grouped list; click a row to open + mark read, right-click to dismiss, `[clear all]` to mark all |
| spotify | click to play/pause (or authorize), scroll to skip; long titles marquee |

## Write a widget

Widgets are pluggable **modules** — just iced, one trait, no IPC. Develop one in
a normal window without launching the bar:

```bash
cargo run -p ezbar-harness --example counter   # a complete starter plugin
cargo run --example harness -- github          # preview a built-in module
```

Guide: [`.claude/skills/ezbar-plugin-author/SKILL.md`](.claude/skills/ezbar-plugin-author/SKILL.md).
Design: [`rfcs/0001-pluggable-modules.md`](rfcs/0001-pluggable-modules.md).

## Layout

```
src/              the bar: state, update, view, subscriptions, launcher
src/sources/      one module per data source (off-thread I/O via spawn_blocking)
src/modules/      pluggable widgets (RFC 0001)
src/widgets/      canvas line-graphs
crates/ezbar-plugin    the plugin SDK — the Module trait, stable API
crates/ezbar-harness   standalone dev harness for modules
examples/harness.rs    preview built-in modules
```

Elm architecture: one `State`, one `Message`, `update`, `view`, and one
`Subscription` stream per data source. Popups are extra layer-shell surfaces
(the multi-window daemon pattern).

## License

MIT © Johannes Brüderl

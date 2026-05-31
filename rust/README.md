# ezbar (Rust)

A full rewrite of ezbar in Rust, rendered with [`iced`](https://iced.rs) on
`wlr-layer-shell` via [`iced_layershell`](https://github.com/waycrate/exwlshelleventloop).
GPU-rendered (wgpu), anti-aliased canvas graphs, no GTK.

Feature parity with the Go version: workspaces, focused-window title, Google
Calendar, GitHub notifications, kubectl context, CPU / temperature / memory /
ping (with toggleable line-graphs), Spotify, stock ticker, volume, battery, clock.

## Build

Needs a recent stable Rust toolchain (≥ 1.88 — wgpu/zbus MSRV):

```bash
rustup update stable
cargo build --release
```

System dependencies (Arch):

```bash
sudo pacman -S --needed rust wayland libxkbcommon vulkan-icd-loader fontconfig
# plus a Vulkan driver: vulkan-radeon | vulkan-intel | nvidia-utils
```

Debian/Ubuntu: `libwayland-dev libxkbcommon-dev libvulkan1 mesa-vulkan-drivers
libfontconfig-1-dev pkg-config` and a rustup toolchain.

## Run

```bash
./target/release/ezbar
```

The process is a thin launcher that respawns the bar child (env `EZBAR_CHILD=1`)
and restarts it if the output goes away (monitor sleep / hotplug). To run a
single foreground instance (e.g. for debugging): `EZBAR_CHILD=1 ./target/release/ezbar`.

## Configuration

Reuses the same config as the Go version, under `~/.config/ezbar/`:

| What | Source |
| --- | --- |
| Calendar | `calendar_url` file, or `$GOOGLE_CALENDAR_ICAL_URL` (secret iCal URL) |
| GitHub | token via `$GH_TOKEN` / `$GITHUB_TOKEN` / `gh auth token`; optional `github_config.json` (`reasons`, `exclude_repos`) |
| Spotify | `spotify_config.json` (`client_id`, `client_secret`); token cached in `spotify_web_token.json`; or `$SPOTIFY_ACCESS_TOKEN` |
| Stock | `$EZBAR_STOCK_SYMBOL` (default `NQ=F`), `$EZBAR_STOCK_API_KEY` (optional, for Finnhub/Alpha Vantage) |
| Ping | target hardcoded to `8.8.8.8` |

## Interactions

- **cpu / temp / mem / ping** — click the label to toggle its graph.
- **volume** — click to mute, scroll to change.
- **kubectl** — left-click clears the context, right-click opens the context picker.
- **calendar** — click for today's meetings; blinks when a meeting is imminent/ongoing.
- **github** — click for the grouped notification list; in the popup, click a row
  to open it in the browser (and mark read), right-click to dismiss, `[clear all]`
  to mark everything read. Blinks red for ~30s on new notifications.
- **spotify** — click to play/pause (or authorize), scroll to skip tracks; long
  titles marquee.

## Layout / architecture

Elm architecture (`iced`): a single `State`, one `Message` enum, `update` for
state transitions, `view` for rendering, and one `Subscription` stream per data
source (replacing the Go goroutine + callback model). Popups are additional
layer-shell surfaces opened via `NewLayerShell` (the multi-window daemon pattern).

- `src/main.rs` — app shell, state, messages, view, subscriptions, launcher.
- `src/sources/` — one module per data source (sync `/proc`/subprocess work runs
  off-thread via `spawn_blocking`; network sources use async `reqwest`).
- `src/widgets/graph.rs` — canvas line-graph program.
- `src/history.rs` — ring buffer for graph histories.

## Known gaps vs. the Go version

- Calendar does not expand recurring (`RRULE`) events; concrete VEVENT instances
  within today's window are shown.
- Popups open on click (toggle) rather than hover — more robust under layer-shell.

---
name: ezbar-plugin-author
description: Author an ezbar status-bar plugin (a "module"). Use when writing, scaffolding, or testing an ezbar module — covers the Module trait, the iced view/update/subscription/popup model, host requests, theme colors, project layout, and the standalone visual harness for developing without launching the whole bar.
---

# Writing an ezbar plugin

An ezbar plugin is a **module**: a Rust type that implements
[`ezbar_plugin::Module`]. A module is *"just iced"* — it owns its drawing, its
input handling, an optional background I/O subscription, and an optional popup.
The bar host owns *placement* and the *surfaces*; you own everything inside your
chip. See `rfcs/0001-pluggable-modules.md` for the full design.

There is no IPC, no DSL, no config schema to learn. If you can write an iced
widget, you can write an ezbar module.

## The 5-minute path

1. Read the complete, runnable starter and run it:

   ```bash
   cat crates/ezbar-harness/examples/counter.rs     # read it — it's the template
   cargo run -p ezbar-harness --example counter
   ```

2. Lay your own crate out so your module is a **library** and your dev entrypoint
   is an **example**. This matters: a normal `src/main.rs` binary cannot see a
   `dev-dependency`, and you don't want the harness in your shipping build — so
   the harness goes in `[dev-dependencies]` and your `fn main` goes in `examples/`.

   ```toml
   # Cargo.toml
   [package]
   name = "my-module"
   edition = "2021"

   [lib]                                                    # your Module lives in src/lib.rs
   [dependencies]
   ezbar-plugin = { path = ".../ezbar/rust/crates/ezbar-plugin" }   # your only runtime dep

   [dev-dependencies]
   ezbar-harness = { path = ".../ezbar/rust/crates/ezbar-harness" } # dev harness (examples/ only)

   [[example]]                                              # your dev entrypoint
   name = "dev"
   path = "examples/dev.rs"
   ```

   You need **no `tokio` and no `iced` dependency** of your own: iced is reached
   through `ezbar_plugin::iced`, and the async helpers you need
   (`spawn_blocking`, `sleep`) are re-exported as `ezbar_plugin::task::*`.

   ```
   my-module/
     Cargo.toml
     src/lib.rs        ← pub struct MyModule + impl Module
     examples/dev.rs   ← fn main() { ezbar_harness::run(...) }
   ```

3. Put the harness entrypoint in `examples/dev.rs` and run it:

   ```rust
   // examples/dev.rs
   fn main() -> ezbar_plugin::iced::Result {
       ezbar_harness::run(Box::new(my_module::MyModule::new(0)))
   }
   ```

   ```bash
   cargo run --example dev
   ```

   This opens a normal desktop window with a mock bar strip, your chip, a popup
   area, and background swatches to check contrast — no sway, no layer-shell.
   (Your `fn main` lives under `examples/` precisely because the harness is a
   dev-dependency; a `src/main.rs` binary couldn't use it.)

## The Module trait

```rust
use ezbar_plugin::{Ctx, ModMsg, Module, Response};
use ezbar_plugin::iced::{Element, Subscription};

pub trait Module: Send {
    fn id(&self) -> &str;                                   // stable type id, e.g. "weather"
    fn subscription(&self) -> Subscription<ModMsg> { … }    // all I/O lives here
    fn update(&mut self, msg: ModMsg) -> Response { … }     // state transition, non-blocking
    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg>;       // the bar chip (required)
    fn popup(&self, ctx: &Ctx) -> Option<Element<'_, ModMsg>> { None } // optional detail surface
    fn shutdown(&mut self) {}
}
```

You implement it with the standard Elm loop, exactly like an iced app:

| Concept            | In a module                                                            |
|--------------------|-----------------------------------------------------------------------|
| Your message enum  | A private `enum Msg { … }` — the host never sees it                    |
| Wrapping a message | `ModMsg::new(Msg::Foo)` to emit; `msg.get::<Msg>()` to read in `update`|
| Async / timers     | a `Subscription` (long-lived) or a `Task`; off-thread work via `ezbar_plugin::task::spawn_blocking` |
| Opening a popup    | return `Response::request(HostRequest::OpenPopup(mode))`               |
| Theme colors       | `ctx.accent()`, `ctx.warn()`, `ctx.fg()`, … (see *Theme colors*)       |

### Imports at a glance

| from                  | items                                                              |
|-----------------------|-------------------------------------------------------------------|
| `ezbar_plugin`        | `Module, Ctx, ModMsg, Response, HostRequest, PopupMode`           |
| `ezbar_plugin::iced`  | `Element, Subscription, Task, Color`, and all of `iced::widget`   |
| `ezbar_plugin::task`  | `spawn_blocking, sleep` — async helpers; **no `tokio` dep needed** |

All of iced lives under `ezbar_plugin::iced` (the re-export), so you never add an
`iced` dependency either.

### `ModMsg`: the type-erased message

The host routes messages to your module without knowing their type. You box your
own enum on the way out and downcast on the way in:

```rust
enum Msg { Loaded(Data), Clicked }

// emit (in view / a Task):
ModMsg::new(Msg::Clicked)

// receive (in update):
fn update(&mut self, msg: ModMsg) -> Response {
    match msg.get::<Msg>() {
        Some(Msg::Loaded(d)) => { self.data = d.clone(); Response::none() }
        Some(Msg::Clicked)   => Response::request(HostRequest::OpenPopup(PopupMode::Click)),
        None => Response::none(),   // not one of ours — ignore
    }
}
```

`msg.get::<Msg>()` returns `Option<&Msg>` — a **borrow** into the type-erased box,
not an owned value. So `d` above is `&Data`: `clone()` any payload you want to move
into your state, and deref `Copy` payloads (`Some(Msg::Count(n)) => self.n = *n`).

### `Response`: tasks + host requests

`update` returns a `Response` = an iced `Task<ModMsg>` (async follow-up work) plus
typed `HostRequest`s (control the host). Helpers:

```rust
Response::none()                                  // nothing to do
Response::task(Task::perform(fut, |r| ModMsg::new(Msg::Loaded(r))))
Response::request(HostRequest::OpenPopup(PopupMode::Click))
Response::request(HostRequest::ClosePopup)
```

Host control (open/close popup) travels **only** through `HostRequest` — never
encode it in your own message.

### `subscription`: where all I/O goes

Long-lived I/O (timers, sockets, polling a process) belongs in a subscription, so
it survives across frames and is driven off the UI thread. **Key it by your
instance id** so two copies of your module don't collide:

```rust
fn subscription(&self) -> Subscription<ModMsg> {
    ezbar_plugin::sub::keyed(self.instance, poll)   // `poll` is a plain fn(&u64) -> Stream
}

fn poll(_id: &u64) -> impl Stream<Item = ModMsg> {
    use ezbar_plugin::iced::futures::SinkExt;
    use ezbar_plugin::task::{sleep, spawn_blocking};
    ezbar_plugin::iced::stream::channel(1, |mut out| async move {
        loop {
            // blocking work goes inside spawn_blocking, never directly here
            let data = spawn_blocking(read).await.unwrap_or_default();
            let _ = out.send(ModMsg::new(Msg::Loaded(data))).await;
            sleep(std::time::Duration::from_secs(30)).await;
        }
    })
}
```

### Theme colors

The user's bar theme reaches `view`/`popup` through `ctx`. Use the `Ctx` color
accessors — they return ready `iced::Color`s and autocomplete under `ctx.`:

| accessor       | token    | meaning                                |
|----------------|----------|----------------------------------------|
| `ctx.fg()`     | `fg`     | primary foreground / normal text       |
| `ctx.fg_dim()` | `fg_dim` | secondary / muted text                 |
| `ctx.accent()` | `accent` | brand / interactive highlight (blue)   |
| `ctx.ok()`     | `ok`     | good / healthy (green)                 |
| `ctx.warn()`   | `warn`   | nearing a limit (yellow/orange)        |
| `ctx.urgent()` | `urgent` | critical (red)                         |
| `ctx.sep()`    | `sep`    | separators / hairlines                 |

```rust
fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
    let color = if self.over_budget { ctx.urgent() } else { ctx.fg() };
    text(self.label.clone()).color(color).into()
}
```

`ctx.theme` is a `&ThemeTokens` and `ThemeTokens` is `Copy`. Read raw tokens for
non-color values (`ctx.theme.text_size`, `ctx.theme.bar_height`), or build a color
from a token directly with `ThemeTokens::color(ctx.theme.accent)`. Pass `ctx.theme`
(a reference) or `*ctx.theme` (a copy) into helper fns.

### Popups

Return `Some(element)` from `popup` to draw a detail surface. The host opens and
places it *after you ask for it from `update`*:

```rust
Some(Msg::Clicked) => Response::request(HostRequest::OpenPopup(PopupMode::Click)),
// or, from the chip's `on_enter`, a display-only preview that closes on leave:
Some(Msg::Hover)   => Response::request(HostRequest::OpenPopup(PopupMode::Hover)),
```

`PopupMode::Click` is sticky/interactive; `PopupMode::Hover` is display-only and
closes on mouse-leave. Widgets inside the popup emit your `Msg` like any other —
and the resulting `update` may return a `Task`, so a popup button can kick async
I/O:

```rust
// in update():
Some(Msg::Refresh) => Response::task(Task::perform(
    async { ezbar_plugin::task::spawn_blocking(read).await.unwrap_or_default() },
    |data| ModMsg::new(Msg::Loaded(data)),
)),
```

`popup` itself is leaf-only: it draws and may emit `Msg`, but must **not** return
`HostRequest`s (ask for open/close from `update`).

### Handling input on your chip

Wrap your chip in `mouse_area` to receive input — it forwards your `Msg`:

| method | fires on |
|--------|----------|
| `.on_press(msg)` | left click |
| `.on_right_press(msg)` | right click |
| `.on_scroll(\|delta\| msg)` | mouse wheel (`delta` is `iced::mouse::ScrollDelta`) |
| `.on_enter(msg)` / `.on_exit(msg)` | pointer enters / leaves (pair with `PopupMode::Hover`) |

```rust
use ezbar_plugin::iced::mouse::ScrollDelta;
use ezbar_plugin::iced::widget::{mouse_area, text};
mouse_area(text(self.label.clone()).color(ctx.fg()))
    .on_press(ModMsg::new(Msg::Clicked))
    .on_scroll(|delta| {
        // ScrollDelta is an enum — read `y` from either arm
        let y = match delta { ScrollDelta::Lines { y, .. } | ScrollDelta::Pixels { y, .. } => y };
        ModMsg::new(Msg::Scrolled(y > 0.0))
    })
    .into()
```

### Drawing with canvas

For graphs/sparklines, return an `iced::widget::canvas(program)` from `view`. The
one non-obvious rule: a `canvas::Program` returned from `view` **cannot borrow
`&self`** (nor `ctx`) — clone the data and colors it draws into the program struct.

```rust
use ezbar_plugin::iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use ezbar_plugin::iced::{mouse, Color, Length, Point, Rectangle, Renderer, Theme};

struct Spark { points: Vec<f64>, color: Color }   // owns its data + colors

impl canvas::Program<ModMsg> for Spark {
    type State = ();
    fn draw(&self, _state: &(), renderer: &Renderer, _theme: &Theme,
            bounds: Rectangle, _cursor: mouse::Cursor) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let y = bounds.height / 2.0;
        let line = Path::line(Point::new(0.0, y), Point::new(bounds.width, y));
        frame.stroke(&line, Stroke::default().with_width(2.0).with_color(self.color));
        vec![frame.into_geometry()]
    }
}

// in view() — clone data + colors in (the program can't borrow ctx or &self):
// use ezbar_plugin::iced::widget::canvas;
// canvas(Spark { points: self.history.clone(), color: ctx.accent() })
//     .width(Length::Fixed(80.0)).height(Length::Fixed(20.0)).into()
```

## Rules (the host enforces some of these)

- **Never block** in `update`/`view`/`subscription`. Do blocking work with
  `ezbar_plugin::task::spawn_blocking` inside a `Task` or subscription.
- **Key subscriptions by instance** (`sub::keyed`) — unkeyed recipes dedupe and
  two instances will silently share one stream.
- A panic in `update` is contained (your module is disabled and shown as an error
  chip). A panic in `view`/canvas draw is **not** — don't panic in drawing code.
- `view`/`popup` borrow `&self`; clone the small bits you hand to widgets.
- Use the `ctx` color accessors so your module matches the user's bar.

## Use iced via the re-export

Always reach iced through `ezbar_plugin::iced` (re-exported), e.g.
`use ezbar_plugin::iced::widget::{text, mouse_area};`. This guarantees you build
against the *same* iced as the host — a module compiled against a different iced
build will not interoperate.

ezbar tracks **iced 0.14.x**. Look widgets up in the iced 0.14 docs, not older
releases — the builder APIs churn between minor versions (e.g. it's
`Space::new().width(Length::Fixed(12.0))`, not the 0.13 `Space::with_width(..)`).

## The harness

`ezbar_harness::run(Box::new(MyModule::new(0)))` — one module.
`ezbar_harness::run_all(vec![…])` — several side by side.
`ezbar_harness::run_themed(modules, theme)` — custom `ThemeTokens`.

It reproduces the host's real drive loop (subscription → update → view → popup,
host-request routing, panic containment), so behaviour in the harness matches the
bar. Click your chip to fire its popup; use the background swatches to check
contrast on dark/light bars; screenshot for a visual record.

Built-in modules can be previewed too: `cargo run --bin harness -- github`.

## Shipping into the bar

Compile-in (phase 1): add your module to the `modules` list in `src/main.rs`
(`ModuleEntry::new(id, Box::new(MyModule::new(id)))`). The `id` you pass must be
unique per instance. dlopen loading is phase 2 (see the RFC).

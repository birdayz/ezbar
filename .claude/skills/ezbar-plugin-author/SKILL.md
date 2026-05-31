---
name: ezbar-plugin-author
description: Author an ezbar status-bar plugin (a "module"). Use when writing, scaffolding, or testing an ezbar module — covers the Module trait, the iced view/update/subscription/popup model, host requests, theming, and the standalone visual harness for developing without launching the whole bar.
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

1. Copy the complete working starter and run it in its own window:

   ```bash
   cp crates/ezbar-harness/examples/counter.rs /tmp/my_module.rs   # read it first
   cargo run -p ezbar-harness --example counter
   ```

2. In your own crate, depend on the two published ezbar crates and the harness:

   ```toml
   [dependencies]
   ezbar-plugin  = { path = "…/ezbar/rust/crates/ezbar-plugin" }   # or git/version
   [dev-dependencies]
   ezbar-harness = { path = "…/ezbar/rust/crates/ezbar-harness" }
   ```

3. Develop against the **harness**, not the bar:

   ```rust
   fn main() -> ezbar_plugin::iced::Result {
       ezbar_harness::run(Box::new(MyModule::new(0)))
   }
   ```

   This opens a normal desktop window with a mock bar strip, your chip, a popup
   area, and background swatches to check contrast — no sway, no layer-shell.

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
| Async / timers     | a `Subscription` (long-lived) or a `Task` returned from `update`       |
| Opening a popup    | return `Response::request(HostRequest::OpenPopup(mode))`               |
| Theme colors       | `ctx.theme` (`ThemeTokens`) → `ThemeTokens::color(ctx.theme.accent)`   |

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
encode it in your own message. `PopupMode::Click` is sticky/interactive;
`PopupMode::Hover` is display-only and closes on mouse-leave.

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
    ezbar_plugin::iced::stream::channel(1, |mut out| async move {
        loop {
            let data = fetch().await;
            let _ = out.send(ModMsg::new(Msg::Loaded(data))).await;
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    })
}
```

## Rules (the host enforces some of these)

- **Never block** in `update`/`view`/`subscription`. Do blocking work with
  `tokio::task::spawn_blocking` inside a `Task` or subscription.
- **Key subscriptions by instance** (`sub::keyed`) — unkeyed recipes dedupe and
  two instances will silently share one stream.
- **`popup` is leaf-only**: it draws, it may emit your `Msg`, but it must not
  return `HostRequest`s.
- A panic in `update` is contained (your module is disabled and shown as an error
  chip). A panic in `view`/canvas draw is **not** — don't panic in drawing code.
- `view`/`popup` borrow `&self`; clone the small bits you hand to widgets.
- Use `ctx.theme` for colors so your module matches the user's bar.

## Use iced via the re-export

Always reach iced through `ezbar_plugin::iced` (re-exported), e.g.
`use ezbar_plugin::iced::widget::{text, mouse_area};`. This guarantees you build
against the *same* iced as the host — a module compiled against a different iced
build will not interoperate.

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

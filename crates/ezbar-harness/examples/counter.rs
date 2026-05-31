//! A complete, minimal ezbar plugin you can run on its own:
//!
//!     cargo run -p ezbar-harness --example counter
//!
//! It ticks once a second, opens a click popup, routes a synchronous button
//! (reset) AND an asynchronous button (+5 after a 1s delay, via a `Task`) back
//! into `update`, and colors itself from the bar theme. Copy this file as the
//! starting point for your own plugin — it needs no `tokio` dependency.

use std::time::Duration;

use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{button, column, row, text};
use ezbar_plugin::iced::{Element, Subscription, Task};
use ezbar_plugin::{Ctx, HostRequest, ModMsg, Module, PopupMode, Response};

// 1. Your module's private message type. The host never sees or names this — it
//    travels type-erased through `ModMsg` and is only read back inside `update`.
enum Msg {
    Tick,
    TogglePopup,
    Reset,
    BumpLater,      // popup button → kicks async work via a Task
    Bumped(String), // the async work finished, carrying its result
}

// 2. Your module's state.
pub struct Counter {
    instance: u64,
    seconds: u64,
    note: String,
}

impl Counter {
    pub fn new(instance: u64) -> Self {
        Counter {
            instance,
            seconds: 0,
            note: String::new(),
        }
    }
}

// 3. Implement the Module trait. Everything is plain iced.
impl Module for Counter {
    fn id(&self) -> &str {
        "counter"
    }

    // All I/O lives here. Key it by `instance` with `sub::keyed` so two copies of
    // the same module don't collide in iced's recipe-keyed runtime.
    fn subscription(&self) -> Subscription<ModMsg> {
        ezbar_plugin::sub::keyed(self.instance, tick)
    }

    // State transitions. Must not block. Return async follow-up work as a `Task`
    // and host actions (open/close popup) as typed `HostRequest`s.
    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Tick) => {
                self.seconds += 1;
                Response::none()
            }
            Some(Msg::TogglePopup) => Response::request(HostRequest::OpenPopup(PopupMode::Click)),
            Some(Msg::Reset) => {
                self.seconds = 0;
                self.note.clear();
                Response::none()
            }
            // A popup-originated message may return a Task like any other. Here we
            // wait 1s off the UI thread (helpers run on the host executor — no
            // tokio dependency), then carry a result back as a payload. A real
            // plugin returns the data it just fetched.
            Some(Msg::BumpLater) => Response::task(Task::perform(
                async {
                    ezbar_plugin::task::sleep(Duration::from_secs(1)).await;
                    "✓ bumped".to_string()
                },
                |result| ModMsg::new(Msg::Bumped(result)),
            )),
            // `msg.get::<Msg>()` hands back `&Msg`, so `note` here is `&String` —
            // clone whatever payload you want to keep in your state.
            Some(Msg::Bumped(note)) => {
                self.seconds += 5;
                self.note = note.clone();
                Response::none()
            }
            None => Response::none(),
        }
    }

    // The bar chip. Anything iced: text, canvas, mouse_area, buttons…
    // Colors come from the user's bar theme via `ctx` (ctx.accent(), ctx.warn(), …).
    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        button(text(format!("⏱ {}s", self.seconds)).color(ctx.accent()))
            .on_press(ModMsg::new(Msg::TogglePopup))
            .into()
    }

    // Optional detail surface. The host opens & places the popup; you draw it.
    fn popup(&self, ctx: &Ctx) -> Option<Element<'_, ModMsg>> {
        Some(
            column(vec![
                text(format!("Elapsed: {} seconds", self.seconds))
                    .size(16)
                    .color(ctx.fg())
                    .into(),
                text(self.note.clone()).color(ctx.ok()).into(),
                row(vec![
                    button(text("reset"))
                        .on_press(ModMsg::new(Msg::Reset))
                        .into(),
                    button(text("+5 in 1s"))
                        .on_press(ModMsg::new(Msg::BumpLater))
                        .into(),
                ])
                .spacing(8)
                .into(),
            ])
            .spacing(8)
            .into(),
        )
    }
}

// A subscription recipe: a plain `fn(&u64) -> Stream`. It emits `ModMsg`s forever.
fn tick(_id: &u64) -> impl Stream<Item = ModMsg> {
    ezbar_plugin::iced::stream::channel(
        1,
        |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                ezbar_plugin::task::sleep(Duration::from_secs(1)).await;
                let _ = out.send(ModMsg::new(Msg::Tick)).await;
            }
        },
    )
}

fn main() -> ezbar_plugin::iced::Result {
    ezbar_harness::run(Box::new(Counter::new(0)))
}

//! A complete, minimal ezbar plugin you can run on its own:
//!
//!     cargo run -p ezbar-harness --example counter
//!
//! It ticks once a second, shows a click-to-open popup, and routes a button
//! press from inside the popup back into the module. Copy this file as the
//! starting point for your own plugin.

use std::time::Duration;

use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{button, column, text};
use ezbar_plugin::iced::{Element, Subscription};
use ezbar_plugin::{Ctx, HostRequest, ModMsg, Module, PopupMode, Response};

// 1. Your module's private message type. The host never sees or names this — it
//    travels type-erased through `ModMsg` and is only read back inside `update`.
enum Msg {
    Tick,
    TogglePopup,
    Reset,
}

// 2. Your module's state.
pub struct Counter {
    instance: u64,
    seconds: u64,
}

impl Counter {
    pub fn new(instance: u64) -> Self {
        Counter { instance, seconds: 0 }
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

    // State transitions. Must not block. Return work as a `Task` and host actions
    // (open/close popup) as typed `HostRequest`s — never on the message channel.
    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Tick) => {
                self.seconds += 1;
                Response::none()
            }
            Some(Msg::TogglePopup) => Response::request(HostRequest::OpenPopup(PopupMode::Click)),
            Some(Msg::Reset) => {
                self.seconds = 0;
                Response::none()
            }
            None => Response::none(),
        }
    }

    // The bar chip. Anything iced: text, canvas, mouse_area, buttons…
    fn view(&self, _ctx: &Ctx) -> Element<'_, ModMsg> {
        button(text(format!("⏱ {}s", self.seconds)))
            .on_press(ModMsg::new(Msg::TogglePopup))
            .into()
    }

    // Optional detail surface. The host opens & places the popup; you draw it.
    fn popup(&self, _ctx: &Ctx) -> Option<Element<'_, ModMsg>> {
        Some(
            column(vec![
                text(format!("Elapsed: {} seconds", self.seconds)).size(16).into(),
                button(text("reset")).on_press(ModMsg::new(Msg::Reset)).into(),
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
                tokio::time::sleep(Duration::from_secs(1)).await;
                let _ = out.send(ModMsg::new(Msg::Tick)).await;
            }
        },
    )
}

fn main() -> ezbar_plugin::iced::Result {
    ezbar_harness::run(Box::new(Counter::new(0)))
}

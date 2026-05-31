//! `memory` module: usage label + GPU sparkline, click toggles the graph.

use std::time::Duration;

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{canvas, mouse_area, row, text};
use ezbar_plugin::iced::{Element, Length, Subscription};
use ezbar_plugin::ui::graph::{Graph, GraphKind};
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

use crate::history::History;
use crate::sources::system;

enum Msg {
    Data(String),
    Toggle,
}

pub struct Memory {
    instance: u64,
    text: String,
    hist: History,
    show_graph: bool,
}

impl Memory {
    pub fn new(instance: u64) -> Self {
        Memory {
            instance,
            text: " --".to_string(),
            hist: History::new(20),
            show_graph: false,
        }
    }
}

impl Module for Memory {
    fn id(&self) -> &str {
        "memory"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        ezbar_plugin::sub::keyed(self.instance, mem_stream)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Data(s)) => {
                self.hist.add(system::extract_memory_usage_value(s));
                self.text = s.clone();
            }
            Some(Msg::Toggle) => self.show_graph = !self.show_graph,
            None => {}
        }
        Response::none()
    }

    fn view(&self, _ctx: &Ctx) -> Element<'_, ModMsg> {
        let lbl = mouse_area(text(self.text.clone())).on_press(ModMsg::new(Msg::Toggle));
        if self.show_graph {
            let g = canvas(Graph {
                values: self.hist.ordered(),
                kind: GraphKind::Memory,
            })
            .width(Length::Fixed(80.0))
            .height(Length::Fixed(20.0));
            row(vec![lbl.into(), g.into()])
                .spacing(4)
                .align_y(Vertical::Center)
                .into()
        } else {
            lbl.into()
        }
    }
}

fn mem_stream(_id: &u64) -> impl Stream<Item = ModMsg> {
    ezbar_plugin::iced::stream::channel(
        1,
        |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let s = tokio::task::spawn_blocking(system::get_memory_usage)
                    .await
                    .unwrap_or_else(|_| " --".to_string());
                if out.send(ModMsg::new(Msg::Data(s))).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        },
    )
}

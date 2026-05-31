//! CPU module: label + canvas graph, click to toggle the graph.
//! Validates: canvas drawing + intra-module state, no popup.

use std::time::Duration;

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{canvas, mouse_area, row, text};
use ezbar_plugin::iced::{Element, Length, Subscription};
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

use crate::history::History;
use crate::sources::system;
use ezbar_plugin::ui::graph::{Graph, GraphKind};

enum Msg {
    Data(String),
    Toggle,
}

pub struct Cpu {
    instance: u64,
    text: String,
    hist: History,
    show_graph: bool,
}

impl Cpu {
    pub fn new(instance: u64) -> Self {
        Cpu {
            instance,
            text: " --".to_string(),
            hist: History::new(30),
            show_graph: true,
        }
    }
}

impl Module for Cpu {
    fn id(&self) -> &str {
        "cpu"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        ezbar_plugin::sub::keyed(self.instance, cpu_stream)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Data(s)) => {
                self.hist.add(system::extract_cpu_usage_value(s));
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
                kind: GraphKind::Cpu,
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

fn cpu_stream(_id: &u64) -> impl Stream<Item = ModMsg> {
    ezbar_plugin::iced::stream::channel(
        1,
        |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let s = tokio::task::spawn_blocking(system::get_cpu_usage)
                    .await
                    .unwrap_or_else(|_| " --".to_string());
                let _ = out.send(ModMsg::new(Msg::Data(s))).await;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        },
    )
}

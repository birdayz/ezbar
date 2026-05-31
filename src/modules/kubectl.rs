//! `kubectl` module: current context (red if production); left-click clears it,
//! right-click opens the context picker.

use std::time::Duration;

use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{column, mouse_area, scrollable, text};
use ezbar_plugin::iced::{Color, Element, Subscription, Task};
use ezbar_plugin::{Ctx, HostRequest, ModMsg, Module, PopupMode, Response};

use crate::sources::kubectl::{self, KubectlData};

enum Msg {
    Data(KubectlData),
    Contexts(Vec<String>),
    Clear,
    Toggle,
    Select(String),
}

pub struct Kubectl {
    instance: u64,
    data: KubectlData,
    contexts: Vec<String>,
}

impl Kubectl {
    pub fn new(instance: u64) -> Self {
        Kubectl {
            instance,
            data: KubectlData::default(),
            contexts: Vec::new(),
        }
    }
}

fn refresh(action: impl FnOnce() + Send + 'static) -> Response {
    Response::task(Task::perform(
        async move {
            tokio::task::spawn_blocking(move || {
                action();
                kubectl::update_context()
            })
            .await
            .unwrap_or_default()
        },
        |d| ModMsg::new(Msg::Data(d)),
    ))
}

impl Module for Kubectl {
    fn id(&self) -> &str {
        "kubectl"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        ezbar_plugin::sub::keyed(self.instance, kube_stream)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Data(d)) => {
                self.data = d.clone();
                Response::none()
            }
            Some(Msg::Contexts(v)) => {
                self.contexts = v.clone();
                Response::none()
            }
            Some(Msg::Clear) => refresh(kubectl::clear_context),
            Some(Msg::Toggle) => {
                let mut resp = Response::request(HostRequest::OpenPopup(PopupMode::Click));
                resp.task = Task::perform(
                    async {
                        tokio::task::spawn_blocking(kubectl::get_all_contexts)
                            .await
                            .unwrap_or_default()
                    },
                    |v| ModMsg::new(Msg::Contexts(v)),
                );
                resp
            }
            Some(Msg::Select(ctx)) => {
                let ctx = ctx.clone();
                let mut resp = refresh(move || kubectl::set_context(&ctx));
                resp.requests.push(HostRequest::ClosePopup);
                resp
            }
            None => Response::none(),
        }
    }

    fn view(&self, _ctx: &Ctx) -> Element<'_, ModMsg> {
        let color = if self.data.is_production {
            Color::from_rgb(1.0, 0.2, 0.2)
        } else {
            Color::WHITE
        };
        mouse_area(text(self.data.string.clone()).color(color))
            .on_press(ModMsg::new(Msg::Clear))
            .on_right_press(ModMsg::new(Msg::Toggle))
            .into()
    }

    fn popup(&self, _ctx: &Ctx) -> Option<Element<'_, ModMsg>> {
        let mut col: Vec<Element<ModMsg>> = vec![text("Kubectl Context").size(15).into()];
        if self.contexts.is_empty() {
            col.push(
                text("(no contexts)")
                    .color(Color::from_rgb(0.5, 0.5, 0.5))
                    .into(),
            );
        }
        for ctx in &self.contexts {
            let is_current = *ctx == self.data.context;
            let color = if kubectl::is_production_context(ctx) {
                Color::from_rgb(1.0, 0.4, 0.4)
            } else if is_current {
                Color::from_rgb(0.4, 0.9, 0.4)
            } else {
                Color::WHITE
            };
            let marker = if is_current { "\u{25b8} " } else { "  " };
            col.push(
                mouse_area(text(format!("{marker}{ctx}")).color(color))
                    .on_press(ModMsg::new(Msg::Select(ctx.clone())))
                    .into(),
            );
        }
        Some(scrollable(column(col).spacing(4)).into())
    }
}

fn kube_stream(_id: &u64) -> impl Stream<Item = ModMsg> {
    ezbar_plugin::iced::stream::channel(
        1,
        |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let d = tokio::task::spawn_blocking(kubectl::update_context)
                    .await
                    .unwrap_or_default();
                if out.send(ModMsg::new(Msg::Data(d))).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        },
    )
}

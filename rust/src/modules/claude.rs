//! Claude module: instances + 5h-limit + block-cost label, hover popup with the
//! full panel. Validates: hover popup (PopupMode::Hover), multiple subscriptions
//! keyed under one instance, typed HostRequest open/close.

use std::time::Duration;

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{column, mouse_area, row, scrollable, text};
use ezbar_plugin::iced::{Color, Element, Subscription};
use ezbar_plugin::{Ctx, HostRequest, ModMsg, Module, PopupMode, Response};

use crate::sources::claude::{self, Block, Instance, Limits};

enum Msg {
    Instances(Vec<Instance>),
    Block(Option<Block>),
    Limits(Option<Limits>),
    Enter,
    Leave,
}

pub struct Claude {
    instance: u64,
    instances: Vec<Instance>,
    block: Option<Block>,
    limits: Option<Limits>,
}

impl Claude {
    pub fn new(instance: u64) -> Self {
        Claude {
            instance,
            instances: Vec::new(),
            block: None,
            limits: None,
        }
    }
}

impl Module for Claude {
    fn id(&self) -> &str {
        "claude"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        Subscription::batch([
            ezbar_plugin::sub::keyed(self.instance, instances_stream),
            ezbar_plugin::sub::keyed(self.instance, block_stream),
            ezbar_plugin::sub::keyed(self.instance, limits_stream),
        ])
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Instances(v)) => self.instances = v.clone(),
            Some(Msg::Block(b)) => self.block = b.clone(),
            Some(Msg::Limits(l)) => self.limits = l.clone(),
            Some(Msg::Enter) => return Response::request(HostRequest::OpenPopup(PopupMode::Hover)),
            Some(Msg::Leave) => return Response::request(HostRequest::ClosePopup),
            None => {}
        }
        Response::none()
    }

    fn view(&self, _ctx: &Ctx) -> Element<'_, ModMsg> {
        let n = self.instances.len();
        let waiting = self.instances.iter().filter(|i| i.waiting).count();
        let count_color = if waiting > 0 {
            Color::from_rgb(1.0, 0.8, 0.2)
        } else {
            Color::WHITE
        };
        let mut items: Vec<Element<ModMsg>> =
            vec![text("🤖").into(), text(format!("{}", n)).color(count_color).into()];
        if let Some(p) = self.limits.as_ref().and_then(|l| l.five_h_left) {
            let c = if p < 15.0 {
                Color::from_rgb(1.0, 0.2, 0.2)
            } else if p < 30.0 {
                Color::from_rgb(1.0, 0.67, 0.0)
            } else {
                Color::from_rgb(0.6, 0.8, 1.0)
            };
            items.push(text(format!("5h{:.0}%", p)).color(c).into());
        }
        if let Some(b) = &self.block {
            items.push(
                text(format!("${:.0}", b.cost))
                    .color(Color::from_rgb(0.7, 0.7, 0.7))
                    .into(),
            );
        }
        mouse_area(row(items).spacing(4).align_y(Vertical::Center))
            .on_enter(ModMsg::new(Msg::Enter))
            .on_exit(ModMsg::new(Msg::Leave))
            .into()
    }

    fn popup(&self, _ctx: &Ctx) -> Option<Element<'_, ModMsg>> {
        let header = Color::from_rgb(0.345, 0.65, 1.0);
        let dim = Color::from_rgb(0.7, 0.7, 0.7);
        let mut col: Vec<Element<ModMsg>> = vec![text(format!(
            "Claude — {} instance(s)",
            self.instances.len()
        ))
        .size(15)
        .into()];

        for i in &self.instances {
            let (marker, color) = if i.waiting {
                ("⏳", Color::from_rgb(1.0, 0.8, 0.2))
            } else {
                ("▶", Color::from_rgb(0.5, 0.85, 0.5))
            };
            col.push(
                row(vec![text(marker).into(), text(i.project.clone()).color(color).into()])
                    .spacing(8)
                    .into(),
            );
        }
        if let Some(b) = &self.block {
            col.push(text("5-hour block").color(header).into());
            col.push(
                text(format!(
                    "  ${:.2} · ${:.0}/hr · {}m left · resets {}",
                    b.cost, b.burn_per_hour, b.minutes_left, b.reset
                ))
                .into(),
            );
            col.push(
                text(format!("  projected ${:.0} · {}", b.projected_cost, b.model))
                    .color(dim)
                    .into(),
            );
        }
        if let Some(l) = &self.limits {
            col.push(text("Limits").color(header).into());
            if let Some(p) = l.five_h_left {
                col.push(text(format!("  5h: {:.0}% left · resets {}", p, l.five_h_reset)).into());
            }
            if let Some(p) = l.weekly_left {
                col.push(text(format!("  weekly: {:.0}% left · resets {}", p, l.weekly_reset)).into());
            }
        }
        Some(scrollable(column(col).spacing(4)).into())
    }
}

fn instances_stream(_id: &u64) -> impl Stream<Item = ModMsg> {
    ezbar_plugin::iced::stream::channel(
        1,
        |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let v = tokio::task::spawn_blocking(claude::instances)
                    .await
                    .unwrap_or_default();
                let _ = out.send(ModMsg::new(Msg::Instances(v))).await;
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        },
    )
}

fn block_stream(_id: &u64) -> impl Stream<Item = ModMsg> {
    ezbar_plugin::iced::stream::channel(
        1,
        |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let b = claude::block().await;
                let _ = out.send(ModMsg::new(Msg::Block(b))).await;
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        },
    )
}

fn limits_stream(_id: &u64) -> impl Stream<Item = ModMsg> {
    ezbar_plugin::iced::stream::channel(
        1,
        |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let l = tokio::task::spawn_blocking(claude::limits).await.ok().flatten();
                let _ = out.send(ModMsg::new(Msg::Limits(l))).await;
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        },
    )
}

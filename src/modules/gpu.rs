//! GPU utilization + temperature, using the CPU module's sparklines and click toggle.

use std::time::Duration;

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{mouse_area, row, text};
use ezbar_plugin::iced::{Element, Subscription};
use ezbar_plugin::icons::Icon;
use ezbar_plugin::task::{sleep, spawn_blocking};
use ezbar_plugin::ui::graph::GraphKind;
use ezbar_plugin::{Ctx, ModMsg, Module, Response};

use crate::history::History;
use crate::sources::gpu::{self, GpuData};

enum Msg {
    Data(GpuData),
    Toggle,
}

pub struct Gpu {
    instance: u64,
    card: String,
    data: GpuData,
    usage_hist: History,
    temp_hist: History,
    show_graph: bool,
    gcfg: crate::modules::GraphCfg,
}

impl Gpu {
    pub fn new(instance: u64, cfg: &toml::Value) -> Self {
        let gcfg = crate::modules::graph_cfg(cfg, 30);
        let mut usage_hist = History::new(gcfg.samples);
        // The percentage graph skips negative values: don't fabricate idle samples
        // while loading or when a driver doesn't expose utilization.
        for _ in 0..gcfg.samples {
            usage_hist.add(-1.0);
        }
        Self {
            instance,
            card: cfg
                .get("card")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            data: GpuData::default(),
            usage_hist,
            temp_hist: History::new(gcfg.samples),
            show_graph: true,
            gcfg,
        }
    }
}

fn label(value: Option<f64>, unit: &str) -> String {
    value.map_or_else(|| "--".to_string(), |v| format!("{v:.0}{unit}"))
}

impl Module for Gpu {
    fn id(&self) -> &str {
        "gpu"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        Subscription::run_with((self.instance, self.card.clone()), gpu_stream)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Data(data)) => {
                self.usage_hist.add(data.usage.unwrap_or(-1.0));
                self.temp_hist.add(data.temperature.unwrap_or(0.0));
                self.data = *data;
            }
            Some(Msg::Toggle) => self.show_graph = !self.show_graph,
            None => {}
        }
        Response::none()
    }

    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        let usage = mouse_area(
            row(vec![
                Icon::Gpu.view(ctx.theme.text_size, ctx.fg()),
                text(label(self.data.usage, "%")).into(),
            ])
            .spacing(5)
            .align_y(Vertical::Center),
        )
        .on_press(ModMsg::new(Msg::Toggle));
        let temperature = mouse_area(
            row(vec![
                Icon::Temperature.view(ctx.theme.text_size, ctx.fg()),
                text(label(self.data.temperature, "°C")).into(),
            ])
            .spacing(5)
            .align_y(Vertical::Center),
        )
        .on_press(ModMsg::new(Msg::Toggle));
        let mut parts = vec![usage.into()];
        if self.show_graph && self.data.usage.is_some() {
            parts.push(crate::modules::graph_widget(
                &self.gcfg,
                GraphKind::Cpu, // same 0–100% scale and thresholds as CPU utilization
                self.usage_hist.ordered(),
                ctx.graph_paint(self.gcfg.line_color.as_deref()),
            ));
        }
        parts.push(temperature.into());
        if self.show_graph && self.data.temperature.is_some() {
            parts.push(crate::modules::graph_widget(
                &self.gcfg,
                GraphKind::Temperature,
                self.temp_hist.ordered(),
                ctx.graph_paint(self.gcfg.line_color.as_deref()),
            ));
        }
        row(parts).spacing(4).align_y(Vertical::Center).into()
    }
}

fn gpu_stream(data: &(u64, String)) -> impl Stream<Item = ModMsg> {
    let card = data.1.clone();
    ezbar_plugin::iced::stream::channel(
        1,
        move |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            loop {
                let card = card.clone();
                let data = spawn_blocking(move || gpu::read_gpu(&card))
                    .await
                    .unwrap_or_default();
                if out.send(ModMsg::new(Msg::Data(data))).await.is_err() {
                    break;
                }
                sleep(Duration::from_secs(2)).await;
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_distinguish_unknown_from_idle() {
        assert_eq!(label(None, "%"), "--");
        assert_eq!(label(Some(0.0), "%"), "0%");
        assert_eq!(label(Some(37.0), "%"), "37%");
        assert_eq!(label(Some(54.25), "°C"), "54°C");
    }

    #[test]
    fn config_history_and_toggle_match_cpu_behavior() {
        let cfg = "card = 'card2'\n[graph]\nsamples = 2\nwidth = 64\nline_color = 'accent'"
            .parse()
            .unwrap();
        let mut gpu = Gpu::new(7, &cfg);
        assert_eq!(gpu.card, "card2");
        assert_eq!(gpu.instance, 7);
        assert_eq!(gpu.gcfg.width, 64.0);
        assert_eq!(gpu.gcfg.line_color.as_deref(), Some("accent"));
        assert_eq!(gpu.usage_hist.ordered(), vec![-1.0, -1.0]);
        assert!(gpu.show_graph);
        let data = GpuData {
            usage: Some(37.0),
            temperature: Some(54.25),
        };
        gpu.update(ModMsg::new(Msg::Data(data)));
        assert_eq!(gpu.data, data);
        assert_eq!(gpu.usage_hist.ordered(), vec![-1.0, 37.0]);
        assert_eq!(gpu.temp_hist.ordered(), vec![0.0, 54.25]);
        gpu.update(ModMsg::new(Msg::Toggle));
        assert!(!gpu.show_graph);
        gpu.update(ModMsg::new(Msg::Data(GpuData::default())));
        assert_eq!(gpu.data, GpuData::default());
        assert_eq!(gpu.usage_hist.ordered(), vec![37.0, -1.0]);
        assert_eq!(gpu.temp_hist.ordered(), vec![54.25, 0.0]);
        gpu.update(ModMsg::new(Msg::Toggle));
        assert!(gpu.show_graph);
        gpu.update(ModMsg::new("unrelated message"));
        assert!(gpu.show_graph);
    }
}

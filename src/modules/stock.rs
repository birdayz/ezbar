//! `stock` module: ticker (green up / red down) with a hover popup showing a 7-day
//! GPU chart.

use std::time::Duration;

use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::futures::{SinkExt, Stream};
use ezbar_plugin::iced::widget::{canvas, mouse_area, row, text};
use ezbar_plugin::iced::{Color, Element, Length, Subscription};
use ezbar_plugin::ui::graph::{MiniTrend, StockChart};
use ezbar_plugin::{Ctx, HostRequest, ModMsg, Module, PopupMode, Response};

use crate::sources::stock::{self, StockData};

enum Msg {
    Data(StockData),
    Chart(Vec<f64>),
    Enter,
    Leave,
}

pub struct Stock {
    instance: u64,
    symbol: String,
    data: StockData,
    chart: Vec<f64>,
}

impl Stock {
    pub fn new(instance: u64) -> Self {
        let (symbol, _) = stock::config();
        Stock {
            instance,
            symbol,
            data: StockData {
                display_text: " \u{2026}".to_string(),
                ..Default::default()
            },
            chart: Vec::new(),
        }
    }
}

impl Module for Stock {
    fn id(&self) -> &str {
        "stock"
    }

    fn subscription(&self) -> Subscription<ModMsg> {
        ezbar_plugin::sub::keyed(self.instance, stock_stream)
    }

    fn update(&mut self, msg: ModMsg) -> Response {
        match msg.get::<Msg>() {
            Some(Msg::Data(d)) => self.data = d.clone(),
            Some(Msg::Chart(c)) => self.chart = c.clone(),
            Some(Msg::Enter) => return Response::request(HostRequest::OpenPopup(PopupMode::Hover)),
            Some(Msg::Leave) => return Response::request(HostRequest::ClosePopup),
            None => {}
        }
        Response::none()
    }

    fn view(&self, ctx: &Ctx) -> Element<'_, ModMsg> {
        let s = &self.data;
        // Trend colour (stocks are universally green up / red down — semantic, not
        // theme). Tuned a touch brighter so the sparkline pops over the dark island.
        let color = if s.is_positive && s.change != 0.0 {
            Color::from_rgb(0.30, 0.86, 0.42)
        } else if s.is_negative {
            Color::from_rgb(1.0, 0.36, 0.42)
        } else {
            Color::from_rgb(0.62, 0.66, 0.78)
        };

        // The icon is a tiny live gradient sparkline of the price series — the bar's
        // GPU-graph identity at glyph size: crisp, colourful, data-driven, themeable.
        // (Replaces the bitmap trend emoji, which scaled to a blurry blob and was
        // stuck at one colour.) Once price data is in, show "SYMBOL: $price change";
        // before that, the source's status/loading text.
        let rest = if s.price > 0.0 {
            format!("{}: {} {}", s.symbol, s.price_string, s.change_string)
        } else {
            s.display_text.trim_start().to_string()
        };
        let mut parts: Vec<Element<ModMsg>> = Vec::new();
        if self.chart.len() >= 2 {
            parts.push(
                canvas(MiniTrend {
                    values: self.chart.clone(),
                    color,
                    bg: ctx.bg(),
                })
                .width(Length::Fixed(28.0))
                .height(Length::Fixed(18.0))
                .into(),
            );
        }
        parts.push(text(rest).color(color).into());
        mouse_area(row(parts).spacing(8).align_y(Vertical::Center))
            .on_enter(ModMsg::new(Msg::Enter))
            .on_exit(ModMsg::new(Msg::Leave))
            .into()
    }

    fn popup(&self, _ctx: &Ctx) -> Option<Element<'_, ModMsg>> {
        Some(
            canvas(StockChart {
                values: self.chart.clone(),
                symbol: self.symbol.clone(),
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .into(),
        )
    }
}

fn stock_stream(_id: &u64) -> impl Stream<Item = ModMsg> {
    ezbar_plugin::iced::stream::channel(
        1,
        |mut out: ezbar_plugin::iced::futures::channel::mpsc::Sender<ModMsg>| async move {
            let (symbol, api_key) = stock::config();
            loop {
                if let Ok(d) = stock::fetch(&symbol, &api_key).await {
                    if out.send(ModMsg::new(Msg::Data(d))).await.is_err() {
                        break;
                    }
                }
                let chart = stock::fetch_chart(&symbol).await;
                if out.send(ModMsg::new(Msg::Chart(chart))).await.is_err() {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(300)).await;
            }
        },
    )
}

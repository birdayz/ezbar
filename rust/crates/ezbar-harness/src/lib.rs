//! A standalone visual harness for ezbar modules.
//!
//! An ezbar module (`ezbar_plugin::Module`) is "just iced": it owns its drawing,
//! its input, an optional I/O subscription, and an optional popup. The real bar
//! drives it on wlr-layer-shell — which makes a module awkward to *develop*,
//! because you'd have to launch the whole bar to see one chip.
//!
//! This harness reproduces the host's drive loop — subscription → update → view →
//! popup, [`HostRequest`] routing, and panic containment — in a plain desktop
//! window. So you can `cargo run` a single module, see its chip and popup live,
//! click it, scroll it, swap the bar background to check contrast, and screenshot
//! it. No layer-shell, no sway, no real bar.
//!
//! ```no_run
//! // your module implements ezbar_plugin::Module
//! ezbar_harness::run(Box::new(MyModule::new(0)));
//! ```
//!
//! See `examples/counter.rs` for a complete, runnable starter plugin.

use std::sync::Mutex;

// Use iced re-exported from ezbar-plugin so it is byte-identical to the iced your
// module is compiled against (mismatched iced builds will not interoperate).
use ezbar_plugin::iced;
use ezbar_plugin::iced::alignment::Vertical;
use ezbar_plugin::iced::widget::{button, column, container, row, text, Space};
use ezbar_plugin::iced::{Background, Border, Color, Element, Length, Subscription, Task};
use ezbar_plugin::{Ctx, HostRequest, ModMsg, Module, PopupMode, ThemeTokens};

/// Theme tokens matching the real bar's defaults. Pass your own via [`run_themed`].
pub const DEFAULT_THEME: ThemeTokens = ThemeTokens {
    fg: [1.0, 1.0, 1.0, 1.0],
    fg_dim: [0.7, 0.7, 0.7, 1.0],
    urgent: [1.0, 0.2, 0.2, 1.0],
    warn: [1.0, 0.67, 0.0, 1.0],
    ok: [0.2, 0.8, 0.2, 1.0],
    accent: [0.345, 0.65, 1.0, 1.0],
    sep: [0.4, 0.4, 0.4, 1.0],
    text_size: 14.0,
    bar_height: 34,
};

/// Run the harness hosting a single module. The simplest entry point.
pub fn run(module: Box<dyn Module>) -> iced::Result {
    run_all(vec![module])
}

/// Run the harness hosting several modules side by side (like the bar's right zone).
pub fn run_all(modules: Vec<Box<dyn Module>>) -> iced::Result {
    run_themed(modules, DEFAULT_THEME)
}

/// Run the harness with a custom theme.
pub fn run_themed(modules: Vec<Box<dyn Module>>, theme: ThemeTokens) -> iced::Result {
    // `boot` is an `Fn` (iced may call it more than implied), so hand the owned,
    // non-`Clone` modules through a `Mutex<Option<…>>` and `take()` them once.
    let pending = Mutex::new(Some(modules));
    iced::application(
        move || Harness::new(pending.lock().unwrap().take().expect("boot runs once"), theme),
        Harness::update,
        Harness::view,
    )
    .title("ezbar module harness")
    .subscription(Harness::subscription)
    .style(Harness::style)
    .antialiasing(true)
    .run()
}

/// One hosted module plus the routing id the harness assigns it.
struct Entry {
    instance: u64,
    module: Box<dyn Module>,
    /// Set if the module panicked in `update`; it is shown as an error chip and
    /// dropped from the subscription loop (mirrors the real host's containment).
    panicked: bool,
}

struct Harness {
    theme: ThemeTokens,
    modules: Vec<Entry>,
    /// The currently open popup, if any: (owning instance, mode).
    popup: Option<(u64, PopupMode)>,
    bar_bg: BarBg,
}

/// Background swatches for checking chip contrast against different bar themes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BarBg {
    Black,
    Dark,
    Light,
    Accent,
}

impl BarBg {
    const ALL: [BarBg; 4] = [BarBg::Black, BarBg::Dark, BarBg::Light, BarBg::Accent];

    fn color(self) -> Color {
        match self {
            BarBg::Black => Color::from_rgb(0.05, 0.05, 0.06),
            BarBg::Dark => Color::from_rgb(0.12, 0.12, 0.14),
            BarBg::Light => Color::from_rgb(0.90, 0.90, 0.92),
            BarBg::Accent => Color::from_rgb(0.10, 0.16, 0.30),
        }
    }

    fn label(self) -> &'static str {
        match self {
            BarBg::Black => "Black",
            BarBg::Dark => "Dark",
            BarBg::Light => "Light",
            BarBg::Accent => "Accent",
        }
    }
}

#[derive(Debug, Clone)]
enum HMsg {
    /// A message from a hosted module, tagged with the routing instance.
    Module { instance: u64, msg: ModMsg },
    SetBg(BarBg),
}

impl Harness {
    fn new(modules: Vec<Box<dyn Module>>, theme: ThemeTokens) -> Self {
        let modules = modules
            .into_iter()
            .enumerate()
            .map(|(i, module)| Entry { instance: i as u64, module, panicked: false })
            .collect();
        Harness { theme, modules, popup: None, bar_bg: BarBg::Black }
    }

    fn subscription(&self) -> Subscription<HMsg> {
        // Mirror the host: each module's subscription, instance-keyed via `.with`
        // (routes the message AND distinguishes recipes for repeated module types).
        let subs = self.modules.iter().filter(|e| !e.panicked).map(|entry| {
            entry
                .module
                .subscription()
                .with(entry.instance)
                .map(|(instance, msg)| HMsg::Module { instance, msg })
        });
        Subscription::batch(subs)
    }

    fn update(&mut self, message: HMsg) -> Task<HMsg> {
        match message {
            HMsg::SetBg(bg) => {
                self.bar_bg = bg;
                Task::none()
            }
            HMsg::Module { instance, msg } => {
                let idx = match self
                    .modules
                    .iter()
                    .position(|e| e.instance == instance && !e.panicked)
                {
                    Some(i) => i,
                    None => return Task::none(),
                };
                // Contain a panicking module to its own `update` (as the host does).
                let resp = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.modules[idx].module.update(msg)
                })) {
                    Ok(r) => r,
                    Err(_) => {
                        eprintln!(
                            "[harness] module '{}' panicked in update — disabling it",
                            self.modules[idx].module.id()
                        );
                        self.modules[idx].panicked = true;
                        if matches!(self.popup, Some((i, _)) if i == instance) {
                            self.popup = None;
                        }
                        return Task::none();
                    }
                };
                for req in resp.requests {
                    self.apply_request(instance, req);
                }
                resp.task.map(move |m| HMsg::Module { instance, msg: m })
            }
        }
    }

    /// Honor a typed host request. One popup at a time; OpenPopup toggles.
    fn apply_request(&mut self, instance: u64, req: HostRequest) {
        match req {
            HostRequest::OpenPopup(mode) => {
                if matches!(self.popup, Some((i, _)) if i == instance) {
                    self.popup = None; // toggle off
                } else {
                    self.popup = Some((instance, mode));
                }
            }
            HostRequest::ClosePopup => {
                if matches!(self.popup, Some((i, _)) if i == instance) {
                    self.popup = None;
                }
            }
        }
    }

    fn view(&self) -> Element<'_, HMsg> {
        let fg = ThemeTokens::color(self.theme.fg);
        let dim = ThemeTokens::color(self.theme.fg_dim);

        // ---- header ----
        let header = text("ezbar module harness").size(18).color(fg);

        // ---- background swatches ----
        let mut swatches: Vec<Element<HMsg>> =
            vec![text("bar background:").color(dim).into()];
        for bg in BarBg::ALL {
            let selected = bg == self.bar_bg;
            let label = if selected {
                format!("[{}]", bg.label())
            } else {
                format!(" {} ", bg.label())
            };
            swatches.push(button(text(label)).on_press(HMsg::SetBg(bg)).into());
        }
        let swatches = row(swatches).spacing(6).align_y(Vertical::Center);

        // ---- the mock bar strip ----
        let mut chips: Vec<Element<HMsg>> = Vec::new();
        for entry in &self.modules {
            let instance = entry.instance;
            if entry.panicked {
                chips.push(
                    text(format!("⚠ {}", entry.module.id()))
                        .color(ThemeTokens::color(self.theme.urgent))
                        .into(),
                );
            } else {
                let ctx = Ctx { instance_id: instance, theme: &self.theme };
                chips.push(
                    entry
                        .module
                        .view(&ctx)
                        .map(move |m| HMsg::Module { instance, msg: m }),
                );
            }
            chips.push(text("|").color(ThemeTokens::color(self.theme.sep)).into());
        }
        chips.pop(); // drop the trailing separator
        let bar_color = self.bar_bg.color();
        let bar = container(row(chips).spacing(8).align_y(Vertical::Center))
            .height(Length::Fixed(self.theme.bar_height as f32))
            .width(Length::Fill)
            .padding([0, 10])
            .style(move |_theme| container::Style {
                background: Some(Background::Color(bar_color)),
                text_color: Some(fg),
                ..Default::default()
            });

        // ---- the popup surface (rendered below the bar) ----
        let popup: Element<HMsg> = match self.popup {
            Some((instance, _mode)) => {
                match self.modules.iter().find(|e| e.instance == instance && !e.panicked) {
                    Some(entry) => {
                        let ctx = Ctx { instance_id: instance, theme: &self.theme };
                        match entry.module.popup(&ctx) {
                            Some(content) => wrap_popup(
                                content.map(move |m| HMsg::Module { instance, msg: m }),
                            ),
                            None => hint(&format!(
                                "module '{}' requested a popup but its popup() returned None",
                                entry.module.id()
                            )),
                        }
                    }
                    None => Space::new().into(),
                }
            }
            None => hint("no popup open — click or hover a chip to trigger one"),
        };

        container(
            column(vec![
                header.into(),
                swatches.into(),
                text("bar ↓").size(12).color(dim).into(),
                bar.into(),
                text("popup ↓").size(12).color(dim).into(),
                popup,
            ])
            .spacing(12)
            .padding(16),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    fn style(&self, _theme: &iced::Theme) -> iced::theme::Style {
        iced::theme::Style {
            background_color: Color::from_rgb(0.14, 0.14, 0.16),
            text_color: ThemeTokens::color(self.theme.fg),
        }
    }
}

/// The dark rounded chrome the real bar wraps popups in.
fn wrap_popup(body: Element<'_, HMsg>) -> Element<'_, HMsg> {
    container(body)
        .padding(12)
        .style(|_theme| container::Style {
            background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.92))),
            border: Border {
                color: Color::from_rgb(0.2, 0.2, 0.2),
                width: 1.0,
                radius: 8.0.into(),
            },
            text_color: Some(Color::WHITE),
            ..Default::default()
        })
        .into()
}

fn hint(s: &str) -> Element<'static, HMsg> {
    text(s.to_string()).color(Color::from_rgb(0.5, 0.5, 0.55)).into()
}

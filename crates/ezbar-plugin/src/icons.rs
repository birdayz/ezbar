//! The bar's icon set — **real vector (SVG) icons**, not font glyphs.
//!
//! Lucide (system widgets) + Simple Icons (brands), embedded at compile time and
//! rendered by iced's `svg` widget. The source art is monochrome
//! (`stroke="currentColor"` / black fill); iced's svg colour filter recolours
//! each icon to a theme colour at draw time, so one asset serves any theme.
//!
//! This replaces the old Nerd Font glyph approach: crisp at any size, no font
//! dependency, no tofu, and a single coherent designed set.

use crate::iced::widget::svg;
use crate::iced::{Color, ContentFit, Element, Length};

/// Define the icon enum and its embedded bytes from one table, so adding an icon
/// is a single line and the asset path can't drift.
macro_rules! icon_set {
    ($($variant:ident => $file:literal),* $(,)?) => {
        /// A bar icon. Render with [`Icon::view`].
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Icon { $($variant),* }

        impl Icon {
            fn bytes(self) -> &'static [u8] {
                match self {
                    $(Icon::$variant => {
                        include_bytes!(concat!("../assets/icons/", $file, ".svg"))
                    })*
                }
            }
        }
    };
}

icon_set! {
    // ── system (Lucide) ──
    Cpu => "cpu", Memory => "memory", Temperature => "temperature", Ping => "ping",
    VolumeHigh => "volume-high", VolumeMedium => "volume-medium", VolumeMute => "volume-mute",
    Battery => "battery", BatteryCharging => "battery-charging",
    BatteryLow => "battery-low", BatteryWarning => "battery-warning",
    Bot => "bot",
    TrendingUp => "trending-up", TrendingDown => "trending-down", TrendingFlat => "trending-flat",
    Disk => "disk", Net => "net", Ip => "ip", Updates => "updates",
    Keyboard => "keyboard", Clock => "clock", Calendar => "calendar",
    // ── brands (Simple Icons) ──
    Github => "github", Spotify => "spotify", Kubernetes => "kubernetes",
}

impl Icon {
    /// Render as a `size`×`size` square widget, tinted to `color`. Emits no
    /// messages, so it drops into any module's `view`/`popup`.
    pub fn view<'a, Message: 'a>(self, size: f32, color: Color) -> Element<'a, Message> {
        svg(svg::Handle::from_memory(self.bytes()))
            .width(Length::Fixed(size))
            .height(Length::Fixed(size))
            .content_fit(ContentFit::Contain)
            .style(move |_theme, _status| svg::Style { color: Some(color) })
            .into()
    }
}

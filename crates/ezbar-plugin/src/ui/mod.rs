//! `ezbar_plugin::ui` — the shared component library. Standard, reusable iced
//! pieces a module *composes* into its `view()`: GPU sparkline graphs today,
//! more (metric, pill, button, popup_frame, icon) as they're promoted. Built-in
//! and third-party modules pull from the same place — these are conveniences, not
//! a separate widget tier.

pub mod graph;
pub use graph::{Graph, GraphKind, StockChart};

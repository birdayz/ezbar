//! Visual dev harness for the *built-in* ezbar modules. Opens a normal window
//! (no sway / layer-shell needed) showing one or more module chips live:
//!
//!     cargo run --example harness -- github     # just the github chip + popup
//!     cargo run --example harness -- cpu
//!     cargo run --example harness -- claude
//!     cargo run --example harness               # all of them, side by side
//!
//! To develop your *own* module, depend on `ezbar-harness` and call
//! `ezbar_harness::run(Box::new(MyModule::new(0)))`. See the `ezbar-plugin-author`
//! skill and `crates/ezbar-harness/examples/counter.rs`.

use ezbar::modules::{clock::Clock, cpu::Cpu, github::GitHub};
use ezbar_plugin::Module;

fn main() -> ezbar_plugin::iced::Result {
    let which = std::env::args().nth(1).unwrap_or_else(|| "all".to_string());
    let cfg = toml::Value::Table(Default::default()); // modules that read `[modules.<id>]` get an empty table here
    let modules: Vec<Box<dyn Module>> = match which.as_str() {
        "cpu" => vec![Box::new(Cpu::new(0, &cfg))],
        "github" => vec![Box::new(GitHub::new(0))],
        "clock" => {
            // CLOCK_CAL=hover|click picks which popup to preview (default click = full grid).
            let mut t = toml::value::Table::new();
            t.insert(
                "calendar".into(),
                toml::Value::String(std::env::var("CLOCK_CAL").unwrap_or_else(|_| "click".into())),
            );
            vec![Box::new(Clock::new(0, &toml::Value::Table(t)))]
        }
        "all" => vec![Box::new(Cpu::new(0, &cfg)), Box::new(GitHub::new(1))],
        other => {
            eprintln!("unknown module '{other}'. try one of: cpu, github, clock, all");
            std::process::exit(2);
        }
    };
    ezbar_harness::run_all(modules)
}

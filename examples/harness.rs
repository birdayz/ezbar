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

use ezbar::modules::{calendar::Calendar, claude::Claude, cpu::Cpu, github::GitHub};
use ezbar_plugin::Module;

fn main() -> ezbar_plugin::iced::Result {
    let which = std::env::args().nth(1).unwrap_or_else(|| "all".to_string());
    let cfg = toml::Value::Table(Default::default()); // modules that read `[modules.<id>]` get an empty table here
    let modules: Vec<Box<dyn Module>> = match which.as_str() {
        "cpu" => vec![Box::new(Cpu::new(0, &cfg))],
        "github" => vec![Box::new(GitHub::new(0))],
        "claude" => vec![Box::new(Claude::new(0))],
        "calendar" => vec![Box::new(Calendar::new(0))],
        "all" => vec![
            Box::new(Cpu::new(0, &cfg)),
            Box::new(GitHub::new(1)),
            Box::new(Claude::new(2)),
        ],
        other => {
            eprintln!("unknown module '{other}'. try one of: cpu, github, claude, calendar, all");
            std::process::exit(2);
        }
    };
    ezbar_harness::run_all(modules)
}

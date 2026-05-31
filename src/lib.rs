//! Shared library surface of ezbar.
//!
//! The data sources, canvas widgets, the history ring buffer, and the compile-in
//! modules live here (not in `main.rs`) so three consumers share exactly one copy
//! of these types:
//!   * the bar binary           (`src/main.rs`)
//!   * the module dev harness    (`src/bin/harness.rs`, via `ezbar-harness`)
//!   * the unit tests.

pub mod history;
pub mod modules;
pub mod sources;
pub mod widgets;

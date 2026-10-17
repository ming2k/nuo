//! Interactive TUI Component Storybook (ADR-0073).
//!
//! Run with:
//! ```sh
//! cargo run -p nuo-tui --example storybook
//! ```

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut workbench = nuo_tui::declarative::workbench::Workbench::default();
    workbench.run()
}

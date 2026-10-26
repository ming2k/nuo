//! Interactive TUI Component Storybook (ADR-0073).
//!
//! Run with:
//! ```sh
//! cargo run -p nuo --example storybook
//! ```

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut workbench = nuo::shell::declarative::workbench::Workbench::default();
    workbench.run()
}

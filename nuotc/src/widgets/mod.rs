//! High-level declarative UI widgets: `Block`, `Paragraph`, `Clear`, `Span`, `Line`,
//! and layout composition primitives (`Column`, `Row`, `Stack`, `Container`, `Padding`, `Spacer`, `Divider`).

pub mod block;
pub mod clear;
pub mod compose;
pub mod paragraph;

pub use block::{Block, BorderType, Borders};
pub use clear::Clear;
pub use compose::{
    AnyWidget, BoxedWidget, Column, Container, Divider, Padding, Row, Spacer, Stack, WidgetExt,
};
pub use paragraph::{Alignment, Line, Paragraph, ParagraphLines, Span, Wrap};

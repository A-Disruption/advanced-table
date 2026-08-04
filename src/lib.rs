//! A data table for iced with multi-level headers.
//!
//! # Why a custom widget
//!
//! Column width is a property of the *column*, not of any single row, so a
//! table cannot be composed from stock `row!` widgets: each row would run its
//! own flex pass and resolve `Fill` against its own content. This widget runs
//! the sizing passes itself so that columns line up and slack lands where the
//! author asked for it.
//!
//! # Structure
//!
//! - [`sizing`] -- pure width math. No iced types, fully unit tested.
//! - [`scroll`] -- offsets and scrollbar geometry. Also unit tested.
//! - [`header`] -- the header tree and its flattening into spans + leaves.
//! - [`table`] -- the `Widget` impl that wires them together.
//!
//! # Scrolling
//!
//! Scrolling is owned by the widget rather than delegated to `scrollable`,
//! because a `scrollable` wrapper scrolls the header away with the body. The
//! sticky header is not a separate mechanism: header and body are drawn in two
//! layers with different translations, `(-x, 0)` and `(-x, -y)`.
//!
//! Vertical scrolling only engages when the table is given a **bounded
//! height** -- `Length::Fill`, a `Fixed` height, or a sized container. This is
//! the same rule `scrollable` follows.
//!
//! Groups are **presentation only**. The leaves are the real columns: they own
//! sizing, ordering, and (later) sort and resize state. Everything above them
//! is decoration that spans leaf ranges.
//!
//! # Example
//!
//! ```ignore
//! use iced_datatable::{group, leaf, DataTable, Sizing};
//! use iced::widget::text;
//!
//! DataTable::new(vec![
//!     leaf(text("Name")).fill(2),
//!     group(text("Contact"), vec![
//!         leaf(text("Email")).fill(3),
//!         leaf(text("Phone")).fixed(140.0),
//!     ]),
//! ])
//! .rows(&app.contacts, |c, column| match column {
//!     0 => text(&c.name).into(),
//!     1 => text(&c.email).into(),
//!     _ => text(&c.phone).into(),
//! })
//! .spacing(1.0)
//! ```

pub mod header;
pub mod scroll;
pub mod selection;
pub mod sizing;
pub mod sort;
pub mod style;
pub mod table;

pub use header::{group, leaf, ColumnSpec, HeaderNode};
pub use scroll::Policy;
pub use selection::{CellPosition, Mode as SelectionMode};
pub use sizing::{Overflow, Sizing};
pub use sort::{Direction, Sort};
pub use style::{Catalog, Cell, CellStyle, Style};
pub use table::{Click, DataTable};

/// Convenience constructor mirroring iced's `fn`-style widget helpers.
pub fn data_table<'a, Message: 'a, Theme, Renderer>(
    headers: Vec<HeaderNode<'a, Message, Theme, Renderer>>,
) -> DataTable<'a, Message, Theme, Renderer>
where
    Theme: Catalog,
    Renderer: iced::advanced::renderer::Renderer,
{
    DataTable::new(headers)
}

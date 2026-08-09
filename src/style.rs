//! Theming, following the `Catalog` pattern used by iced's own widgets.

use iced::{Background, Border, Color, Theme};

#[derive(Debug, Clone, Copy)]
pub struct Style {
    /// Fills the whole header, every level of it.
    pub header_background: Option<Background>,
    /// Painted over `header_background`, and only across the columns each group
    /// actually spans -- an ungrouped column belongs to no group, so banding
    /// the full width would draw a group level over a column that has none.
    ///
    /// Defaults to the same value as `header_background`, which makes it
    /// invisible: banding the levels in different shades makes the header
    /// compete with the data for attention. Set it separately only if you
    /// actually want the levels distinguished by fill.
    pub group_background: Option<Background>,
    pub row_background: Option<Background>,
    /// Applied to odd-indexed rows. `None` disables striping.
    pub alternate_row_background: Option<Background>,
    /// Drawn over the row background when a row is selected.
    pub selected_row_background: Option<Background>,
    /// Outlines a selected row. Drawn only at the edges of a *contiguous run*
    /// of selected rows, so a multi-row selection reads as one block instead of
    /// as a stack of separately boxed rows.
    pub selected_row_border: Option<Color>,
    /// Drawn under the selection highlight when a row is hovered.
    pub hovered_row_background: Option<Background>,
    /// Vertical band drawn over a selected column, in both header and body.
    pub selected_column_background: Option<Background>,
    /// Fill for the selected cell. Drawn above every row and column highlight,
    /// since a cell is the most specific thing the grid can point at.
    pub selected_cell_background: Option<Background>,
    /// Outline for the selected cell, on all four sides. The fill alone is easy
    /// to lose against a selected row or column band underneath it.
    pub selected_cell_border: Option<Color>,
    /// Drawn around the outside of the whole widget, on top of everything else
    /// so no band or stripe can paint over it.
    ///
    /// A `radius` rounds the whole table, not just the frame line: every band
    /// and rule the widget draws is cut to the outline first, so the header
    /// band, the stripes and the column rules all follow the corner. It is not
    /// a clip, because `iced` only clips to rectangles -- so the one thing it
    /// cannot round is a *cell's own* contents. A cell holding a widget with a
    /// background of its own (a `container` filling the cell, say) will still
    /// square off the corner it sits in; keep the corner cells' backgrounds on
    /// the table, through [`CellStyle`], and they follow the radius.
    pub border: Border,
    /// Vertical rule between columns. `None` draws nothing.
    pub column_divider: Option<Color>,
    /// Vertical rule at the edges of a group's span, run down the full height
    /// of the header. Making this heavier than `column_divider` is what reads
    /// as "these columns belong together" rather than as one flat grid.
    pub group_divider: Option<Color>,
    /// Fill behind the chip that carries a column's header under the pointer
    /// while it is being dragged. `None` draws no chip, leaving only the drop
    /// line -- which makes the gesture read as poking at the header rather than
    /// as picking the column up.
    pub reorder_carry_background: Option<Background>,
    /// Marks where a dragged column would land if released now, on the leaf
    /// header band only. Kept to the row the columns are named on rather than
    /// run full height, so it reads as an insertion point instead of competing
    /// with the frozen seam and the column rules it crosses.
    pub reorder_indicator: Option<Color>,
    /// Vertical rule along the seam between frozen and scrolling columns, run
    /// the full height of the table over both. Make it the heaviest rule in the
    /// style -- it separates two things that move independently, which is a
    /// stronger boundary than any of the others.
    pub frozen_divider: Option<Color>,
    /// Vertical rule closing the gutter off from the columns, at both edges and
    /// running the full height of the table. Without it the gutter reads as
    /// slack space outside the table rather than as part of it.
    pub gutter_divider: Option<Color>,
    /// Vertical rule between rows. `None` draws nothing.
    pub row_divider: Option<Color>,
    /// Heavier rule separating header from body.
    pub header_divider: Option<Color>,
    /// Label colour for leaf (column) headers. `None` inherits from the parent.
    pub header_text: Option<Color>,
    /// Label colour for group headers. Falls back to `header_text`.
    pub group_text: Option<Color>,
    /// Body text colour. `None` inherits from the parent.
    pub text: Option<Color>,
    /// Lights the half of the sort control pointing the way the column runs.
    ///
    /// The control is always the same up/down pair whatever the state -- only
    /// which half is lit changes. Swapping the shape per state makes one button
    /// read as three different ones.
    pub sort_indicator: Option<Color>,
    /// The unlit half, and both halves on a sortable column that is not the
    /// current sort. Drawing nothing at all here would leave the control
    /// invisible until sorting is already active -- a button nobody can find.
    pub sort_indicator_inactive: Option<Color>,
    /// Behind the sort control while the pointer is over it.
    pub sort_hovered_background: Option<Background>,
    pub scrollbar_track: Option<Background>,
    pub scrollbar_thumb: Background,
    pub scrollbar_thumb_hovered: Background,
    pub scrollbar_border: Border,
}

impl Style {
    pub fn divider_width(&self) -> f32 {
        1.0
    }
}

/// Everything known about one body cell at draw time, handed to the closure
/// installed with [`DataTable::cell_style`](crate::DataTable::cell_style).
///
/// Note what is *not* here: the value in the cell. The widget never sees your
/// data, only the `Element` you built from it, so condition on `row`/`column`
/// and read your own data -- exactly as the `rows` view function does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub row: usize,
    pub column: usize,
    /// This cell's row is in the selection.
    pub selected: bool,
    /// The pointer is over this cell's row.
    pub hovered: bool,
    /// This cell's column is in the column selection.
    pub column_selected: bool,
}

/// Per-cell overrides. `None` everywhere -- the default -- costs nothing and
/// leaves the table looking exactly as it does without a hook installed.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CellStyle {
    /// Painted over the row background and column band, and *under* the hover
    /// and selection highlights, so a selected row still reads as selected.
    /// Use a translucent colour if you want the cell to show through both.
    pub background: Option<Background>,
    /// Overrides the inherited text colour for this cell's contents. A `text`
    /// widget with an explicit `.color()` still wins -- this only supplies the
    /// colour a cell would otherwise inherit.
    pub text_color: Option<Color>,
}

impl CellStyle {
    pub fn background(mut self, background: impl Into<Background>) -> Self {
        self.background = Some(background.into());
        self
    }

    pub fn color(mut self, color: Color) -> Self {
        self.text_color = Some(color);
        self
    }
}

pub trait Catalog {
    type Class<'a>;

    fn default<'a>() -> Self::Class<'a>;

    fn style(&self, class: &Self::Class<'_>) -> Style;
}

pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme) -> Style + 'a>;

impl Catalog for Theme {
    type Class<'a> = StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(default)
    }

    fn style(&self, class: &Self::Class<'_>) -> Style {
        class(self)
    }
}

pub fn default(theme: &Theme) -> Style {
    let palette = theme.palette();

    // Every band has to differ from the rule drawn on top of it. Previously
    // `group_background` and `column_divider` were both `background.strong`,
    // so dividers on group header rows were painted in exactly the colour
    // behind them -- drawn correctly, and completely invisible.
    Style {
        header_background: Some(palette.background.weak.color.into()),
        group_background: Some(palette.background.weak.color.into()),
        row_background: Some(palette.background.base.color.into()),
        alternate_row_background: Some(palette.background.weakest.color.into()),
        // Rows and columns are two readings of the same grid, so they get the
        // same wash. A translucent tint also lets the stripes and any cell
        // colouring show through, which an opaque fill would bury.
        selected_row_background: Some(Background::Color(Color {
            a: 0.18,
            ..palette.primary.base.color
        })),
        selected_row_border: Some(palette.primary.base.color),
        hovered_row_background: Some(palette.background.weak.color.into()),
        selected_column_background: Some(Background::Color(Color {
            a: 0.18,
            ..palette.primary.base.color
        })),
        selected_cell_background: Some(Background::Color(Color {
            a: 0.28,
            ..palette.primary.base.color
        })),
        selected_cell_border: Some(palette.primary.base.color),
        border: Border {
            color: palette.background.strongest.color,
            width: 1.0,
            radius: 5.0.into(),
        },
        column_divider: Some(palette.background.strong.color),
        // Deliberately a step heavier than `column_divider`: a group boundary
        // is a stronger statement than a column boundary, and drawing both at
        // the same weight is what flattens a three-level header into a grid.
        group_divider: Some(palette.background.strongest.color),
        reorder_carry_background: Some(palette.background.strong.color.into()),
        reorder_indicator: Some(palette.background.base.text),
        frozen_divider: Some(palette.background.strongest.color),
        gutter_divider: Some(palette.background.strong.color),
        row_divider: Some(palette.background.strong.color),
        header_divider: Some(palette.background.strongest.color),
        header_text: Some(palette.background.base.text),
        group_text: Some(palette.background.weak.text),
        text: None,
        sort_indicator: Some(palette.primary.base.text),
        sort_indicator_inactive: Some(Color {
            a: 0.35,
            ..palette.background.base.text
        }),
        sort_hovered_background: Some(palette.background.strong.color.into()),
        scrollbar_track: Some(palette.background.weakest.color.into()),
        scrollbar_thumb: palette.background.strong.color.into(),
        scrollbar_thumb_hovered: palette.primary.base.color.into(),
        scrollbar_border: Border {
            radius: 4.0.into(),
            ..Border::default()
        },
    }
}

/// No stripes, no vertical rules -- closer to a modern data grid.
pub fn minimal(theme: &Theme) -> Style {
    let palette = theme.palette();

    Style {
        header_background: None,
        group_background: None,
        row_background: None,
        alternate_row_background: None,
        selected_row_background: Some(Background::Color(Color {
            a: 0.12,
            ..palette.primary.base.color
        })),
        selected_row_border: Some(palette.primary.base.color),
        hovered_row_background: Some(palette.background.weakest.color.into()),
        selected_column_background: Some(Background::Color(Color {
            a: 0.12,
            ..palette.primary.base.color
        })),
        selected_cell_background: Some(Background::Color(Color {
            a: 0.2,
            ..palette.primary.base.color
        })),
        selected_cell_border: Some(palette.primary.base.color),
        border: Border::default(),
        column_divider: None,
        group_divider: Some(palette.background.weak.color),
        reorder_carry_background: Some(palette.background.weak.color.into()),
        reorder_indicator: Some(palette.background.base.text),
        frozen_divider: Some(palette.background.strong.color),
        gutter_divider: Some(palette.background.weak.color),
        row_divider: Some(palette.background.weak.color),
        header_divider: Some(palette.background.strong.color),
        header_text: Some(palette.background.base.text),
        group_text: Some(palette.background.weak.text),
        text: None,
        sort_indicator: Some(palette.primary.base.color),
        sort_indicator_inactive: Some(Color {
            a: 0.3,
            ..palette.background.base.text
        }),
        sort_hovered_background: Some(palette.background.weak.color.into()),
        scrollbar_track: None,
        scrollbar_thumb: palette.background.strong.color.into(),
        scrollbar_thumb_hovered: palette.primary.base.color.into(),
        scrollbar_border: Border {
            radius: 4.0.into(),
            ..Border::default()
        },
    }
}

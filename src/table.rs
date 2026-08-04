//! The widget.
//!
//! # Why this is a real `Widget`
//!
//! Column width is a property of the *column*, not of any single row. Cell
//! (3, 1) cannot know how wide it should be until every other cell in column 1
//! has been measured. Built out of stock `row!`s, each row runs its own flex
//! pass and resolves `Fill` against its own content -- which is how you get
//! columns that do not line up and slack that lands in the wrong place. So the
//! sizing passes happen here, inside `layout`.
//!
//! # Why scrolling is owned rather than delegated
//!
//! Wrapping this in `scrollable` would scroll the header away with the body.
//! The usual workaround -- two scrollables kept in sync through messages --
//! pushes widget-internal state into the application's `Message` type.
//!
//! Owning it is simpler than it sounds, because a sticky header is not a
//! separate mechanism. Everything lives in one content-space coordinate
//! system; the header and body are just drawn in two layers with **different
//! translations**:
//!
//! - body   -> `(-offset.x, -offset.y)`  scrolls both ways
//! - header -> `(-offset.x,  0.0)`       scrolls sideways, pinned vertically
//!
//! That is the whole trick. Frozen leading columns would be a third layer
//! translated by `(0.0, -offset.y)`, which is why it is worth structuring this
//! way even before that feature exists.
//!
//! # Coordinates
//!
//! Child layout nodes are positioned in **content space** and never move.
//! Scrolling shifts the renderer, not the layout. Events therefore translate
//! the *cursor* into content space rather than translating layouts -- which is
//! also what `iced`'s own `scrollable` does, since `Layout::with_offset` needs
//! a `&Node` that a parent widget cannot get hold of.

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer;
use iced::advanced::widget::{tree, Operation, Tree};
use iced::advanced::{Shell, Widget};
use std::collections::BTreeSet;
use iced::{
    alignment, keyboard, mouse, touch, Background, Border, Color, Element, Event, Length, Padding,
    Point, Rectangle, Size, Vector,
};

use crate::header::{self, ColumnSpec, HeaderCell, HeaderNode};
use crate::scroll::{self, Policy};
use crate::selection::{self, Mode};
use crate::sizing::{self, Overflow, Sizing, SpanRequest};
use crate::sort::{self, Direction, Sort};
use crate::style::{Catalog, Cell, CellStyle, Style};

/// Fallback scroll step when a wheel reports lines and rows are tiny.
const MIN_WHEEL_STEP: f32 = 16.0;

/// Width reserved at the trailing end of a sortable leaf header for its sort
/// control, and the click target for it.
const SORT_ZONE: f32 = 16.0;

/// Width of the arrow drawn inside that zone.
const SORT_ARROW: f32 = 8.0;

pub struct DataTable<'a, Message, Theme, Renderer>
where
    Theme: Catalog,
    Renderer: renderer::Renderer,
{
    /// Header elements first, then body cells row-major. One contiguous Vec so
    /// `Tree::diff_children` has a single slice and child indices line up with
    /// `tree.children` with no translation table.
    elements: Vec<Element<'a, Message, Theme, Renderer>>,
    header_len: usize,

    header_cells: Vec<HeaderCell>,
    header_rows: usize,
    columns: Vec<ColumnSpec>,
    row_count: usize,

    spacing: f32,
    padding: Padding,
    overflow: Overflow,
    measure_sample: Option<usize>,
    min_row_height: f32,

    scrollbar_width: f32,
    vertical: Policy,
    horizontal: Policy,

    mode: Mode,
    selected: BTreeSet<usize>,
    on_select: Option<Box<dyn Fn(BTreeSet<usize>) -> Message + 'a>>,

    column_mode: Mode,
    selected_columns: BTreeSet<usize>,
    on_select_column: Option<Box<dyn Fn(BTreeSet<usize>) -> Message + 'a>>,

    sort: Option<Sort>,
    on_sort: Option<Box<dyn Fn(Option<Sort>) -> Message + 'a>>,

    /// Blank strip at the left and right edges, inside the widget but outside
    /// every column.
    gutter: f32,

    resizable: bool,
    /// Half-width of the grab zone either side of a column edge.
    resize_tolerance: f32,
    /// Floor a column can be dragged to.
    min_column_width: f32,

    width: Length,
    height: Length,
    class: Theme::Class<'a>,
    /// Consulted once per *visible* cell per frame, so the cost is bounded by
    /// the viewport rather than by the row count.
    cell_style: Option<Box<dyn Fn(&Theme, Cell) -> CellStyle + 'a>>,
}

/// Persisted across frames. Geometry is computed in `layout` and read back in
/// `draw`/`update` so neither has to re-derive it.
#[derive(Debug, Default)]
struct State {
    widths: Vec<f32>,
    offsets: Vec<f32>,
    header_row_height: f32,
    header_height: f32,
    row_height: f32,
    /// Full extent of the body content, excluding the header.
    content: Size,

    /// Scroll position in content pixels.
    offset: Vector,
    /// Grab point within the thumb while dragging, per axis.
    y_grab: Option<f32>,
    x_grab: Option<f32>,

    /// Per-column widths the user has dragged to. `None` means "follow the
    /// declared sizing policy".
    overrides: Vec<Option<f32>>,
    /// Column being resized, plus where the drag started and how wide the
    /// column was at that moment. Anchoring to the start rather than
    /// accumulating per-frame deltas keeps the edge glued to the pointer even
    /// if a frame is dropped mid-drag.
    resizing: Option<Resize>,

    /// Tracked so Shift+wheel can be turned into horizontal scrolling.
    modifiers: keyboard::Modifiers,

    /// Row a Shift-range extends from. Ephemeral interaction state, which is
    /// why it lives here while the selection set itself does not.
    anchor: Option<usize>,
    column_anchor: Option<usize>,
    hovered: Option<usize>,
    /// Leaf column whose sort control the pointer is over.
    hovered_sort: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
struct Resize {
    column: usize,
    origin_x: f32,
    origin_width: f32,
}

impl<'a, Message: 'a, Theme, Renderer> DataTable<'a, Message, Theme, Renderer>
where
    Theme: Catalog,
    Renderer: renderer::Renderer,
{
    pub fn new(headers: Vec<HeaderNode<'a, Message, Theme, Renderer>>) -> Self {
        let flat = header::flatten(headers);
        let header_len = flat.elements.len();

        Self {
            elements: flat.elements,
            header_len,
            header_cells: flat.cells,
            header_rows: flat.rows,
            columns: flat.columns,
            row_count: 0,
            spacing: 0.0,
            padding: Padding::from([6, 10]),
            overflow: Overflow::default(),
            measure_sample: Some(200),
            min_row_height: 0.0,
            scrollbar_width: 10.0,
            vertical: Policy::Auto,
            horizontal: Policy::Auto,
            mode: Mode::None,
            selected: BTreeSet::new(),
            on_select: None,
            column_mode: Mode::None,
            selected_columns: BTreeSet::new(),
            on_select_column: None,
            sort: None,
            on_sort: None,
            gutter: 8.0,
            resizable: true,
            resize_tolerance: 4.0,
            min_column_width: 32.0,
            width: Length::Fill,
            height: Length::Fill,
            class: Theme::default(),
            cell_style: None,
        }
    }

    /// Append one row. Extra cells are dropped and missing cells are padded, so
    /// a malformed row degrades instead of panicking.
    pub fn push(&mut self, cells: impl IntoIterator<Item = Element<'a, Message, Theme, Renderer>>) {
        let mut count = 0;

        for cell in cells.into_iter().take(self.columns.len()) {
            self.elements.push(cell);
            count += 1;
        }

        for _ in count..self.columns.len() {
            self.elements.push(iced::widget::Space::new().into());
        }

        self.row_count += 1;
    }

    pub fn row(
        mut self,
        cells: impl IntoIterator<Item = Element<'a, Message, Theme, Renderer>>,
    ) -> Self {
        self.push(cells);
        self
    }

    /// Fill the body from a data slice with a per-cell view function.
    pub fn rows<T>(
        mut self,
        data: impl IntoIterator<Item = &'a T>,
        view: impl Fn(&'a T, usize) -> Element<'a, Message, Theme, Renderer>,
    ) -> Self
    where
        T: 'a,
    {
        let columns = self.columns.len();

        for item in data {
            self.push((0..columns).map(|c| view(item, c)));
        }

        self
    }

    pub fn spacing(mut self, spacing: impl Into<f32>) -> Self {
        self.spacing = spacing.into();
        self
    }

    pub fn padding(mut self, padding: impl Into<Padding>) -> Self {
        self.padding = padding.into();
        self
    }

    pub fn overflow(mut self, overflow: Overflow) -> Self {
        self.overflow = overflow;
        self
    }

    /// Measure every row when deriving intrinsic widths. Accurate but O(n).
    pub fn measure_all(mut self) -> Self {
        self.measure_sample = None;
        self
    }

    pub fn measure_sample(mut self, rows: usize) -> Self {
        self.measure_sample = Some(rows);
        self
    }

    pub fn min_row_height(mut self, height: f32) -> Self {
        self.min_row_height = height;
        self
    }

    pub fn scrollbar_width(mut self, width: f32) -> Self {
        self.scrollbar_width = width;
        self
    }

    pub fn vertical_scroll(mut self, policy: Policy) -> Self {
        self.vertical = policy;
        self
    }

    pub fn horizontal_scroll(mut self, policy: Policy) -> Self {
        self.horizontal = policy;
        self
    }

    /// Enable row selection and report changes.
    ///
    /// The widget does not keep the selection -- it renders the set you pass
    /// and hands back what the set should become. Store it in your own state
    /// and feed it back in on the next `view`.
    pub fn selection(
        mut self,
        mode: Mode,
        selected: &BTreeSet<usize>,
        on_select: impl Fn(BTreeSet<usize>) -> Message + 'a,
    ) -> Self {
        self.mode = mode;
        self.selected = selected.clone();
        self.on_select = Some(Box::new(on_select));
        self
    }

    /// Enable column selection by clicking header cells.
    ///
    /// Clicking a group header selects every leaf column beneath it. Same
    /// ownership rule as row selection: you hold the set, the widget reports
    /// what it should become.
    pub fn column_selection(
        mut self,
        mode: Mode,
        selected: &BTreeSet<usize>,
        on_select: impl Fn(BTreeSet<usize>) -> Message + 'a,
    ) -> Self {
        self.column_mode = mode;
        self.selected_columns = selected.clone();
        self.on_select_column = Some(Box::new(on_select));
        self
    }

    /// Show sort controls on columns marked `.sortable()` and report clicks.
    ///
    /// The table **does not reorder anything**. Same ownership rule as the
    /// selection: you hold the sort, the widget draws it and tells you what it
    /// should become. Sort your own data in `update` and hand back the rows in
    /// the new order -- which is also the only arrangement that survives paging,
    /// filtering, or a server that sorts for you.
    ///
    /// The control is the arrow at the trailing end of the header cell, not the
    /// whole cell, so that clicking a header can still mean "select this
    /// column". If you would rather the entire header sort, that is a one-line
    /// change to the hit test in `update`.
    pub fn sorting(
        mut self,
        sort: Option<Sort>,
        on_sort: impl Fn(Option<Sort>) -> Message + 'a,
    ) -> Self {
        self.sort = sort;
        self.on_sort = Some(Box::new(on_sort));
        self
    }

    /// Blank strip at each side of the table, inside the widget but belonging
    /// to no column.
    ///
    /// It exists so there is somewhere to click that unambiguously means "this
    /// row" rather than "this cell" -- without it, every pixel of a row is
    /// owned by some column and row selection has to fight cell interaction
    /// for the same clicks.
    pub fn gutter(mut self, gutter: f32) -> Self {
        self.gutter = gutter;
        self
    }

    /// Allow dragging leaf column edges in the header. On by default.
    pub fn resizable(mut self, resizable: bool) -> Self {
        self.resizable = resizable;
        self
    }

    pub fn min_column_width(mut self, width: f32) -> Self {
        self.min_column_width = width;
        self
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    /// Note: vertical scrolling only engages when the table is given a bounded
    /// height. With `Length::Shrink` in an unbounded parent the table simply
    /// grows and there is nothing to scroll -- same rule as `scrollable`.
    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }

    pub fn style(mut self, style: impl Fn(&Theme) -> Style + 'a) -> Self
    where
        Theme::Class<'a>: From<crate::style::StyleFn<'a, Theme>>,
    {
        self.class = (Box::new(style) as crate::style::StyleFn<'a, Theme>).into();
        self
    }

    /// Style individual cells conditionally -- a red background on overdue
    /// rows, green text on positive deltas, a muted column.
    ///
    /// This is the counterpart to [`style`](Self::style), which dresses the
    /// table as a whole. It is a hook rather than a property of `leaf()`
    /// because the interesting conditions are almost never "this column" alone;
    /// they are "this column, when the value is negative", and the widget does
    /// not have the value. Match on `cell.column` for column-wide rules and on
    /// `cell.row` to reach back into your own data:
    ///
    /// ```ignore
    /// .cell_style(move |theme, cell| match cell.column {
    ///     3 if records[cell.row].change < 0.0 => {
    ///         CellStyle::default().color(theme.palette().danger)
    ///     }
    ///     _ => CellStyle::default(),
    /// })
    /// ```
    ///
    /// Anything you can express by building a different `Element` -- text
    /// colour, weight, an icon -- is better done in the `rows` view function.
    /// Reach for this when you need what the *widget* draws: the cell's own
    /// background, or a colour that cell contents should inherit.
    pub fn cell_style(mut self, style: impl Fn(&Theme, Cell) -> CellStyle + 'a) -> Self {
        self.cell_style = Some(Box::new(style));
        self
    }

    fn sizing(&self) -> Vec<Sizing> {
        self.columns.iter().map(|c| c.sizing).collect()
    }

    fn cell_index(&self, row: usize, column: usize) -> usize {
        self.header_len + row * self.columns.len() + column
    }

    fn is_sortable(&self, cell: &HeaderCell) -> bool {
        self.on_sort.is_some() && cell.is_leaf() && self.columns[cell.start].sortable
    }

    /// The sort control's rectangle, in **content space** -- subtract the
    /// horizontal scroll offset to hit-test it against the pointer.
    ///
    /// It sits inside the trailing padding rather than against the cell edge,
    /// which is also what keeps it clear of the resize grab zone: that zone is
    /// `resize_tolerance` either side of the boundary, and the padding is
    /// wider. Shrink `padding` below `resize_tolerance` and the resize wins,
    /// since it is tested first.
    fn sort_zone(&self, cell: &HeaderCell, state: &State, bounds: Rectangle) -> Rectangle {
        let right =
            bounds.x + state.offsets[cell.end] + state.widths[cell.end] - self.padding.right;

        Rectangle {
            x: right - SORT_ZONE,
            // The band the *label* is on, not the middle of the cell. An
            // ungrouped column is a full-height header cell whose label is
            // bottom-aligned against the body, so centring the control in the
            // cell floats it a whole band above the text it belongs to -- which
            // is why "ID" had its arrows stranded up on the group row.
            y: bounds.y + state.header_row_height * (cell.bottom() - 1) as f32,
            width: SORT_ZONE,
            height: state.header_row_height,
        }
    }

    /// Which sortable column's control is under this *screen* point.
    fn sort_zone_at(&self, point: Point, bounds: Rectangle, state: &State) -> Option<usize> {
        if state.widths.len() != self.columns.len() {
            return None;
        }

        self.header_cells
            .iter()
            .filter(|cell| self.is_sortable(cell))
            .find(|cell| {
                let zone = self.sort_zone(cell, state, bounds);

                Rectangle {
                    x: zone.x - state.offset.x,
                    ..zone
                }
                .contains(point)
            })
            .map(|cell| cell.start)
    }
}

/// A solid triangle, rasterised as a stack of quads.
///
/// `fill_quad` is the only primitive `renderer::Renderer` offers -- no paths,
/// no rotation, no glyphs -- so the arrowhead is stepped by hand. Four steps is
/// plenty at 8x5, and it keeps the widget from having to require a text or
/// geometry renderer just to draw a sort marker.
fn arrow<Renderer: renderer::Renderer>(
    renderer: &mut Renderer,
    bounds: Rectangle,
    up: bool,
    color: Color,
) {
    const STEPS: usize = 4;

    let step = bounds.height / STEPS as f32;

    for i in 0..STEPS {
        // An ascending arrow tapers upward, so the narrow end is whichever end
        // the point is at.
        let taper = if up { STEPS - 1 - i } else { i } as f32 / STEPS as f32;
        let width = bounds.width * (1.0 - taper);

        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle {
                    x: bounds.x + (bounds.width - width) / 2.0,
                    y: bounds.y + step * i as f32,
                    width,
                    height: step,
                },
                ..Default::default()
            },
            color,
        );
    }
}

/// The region below the header -- the part that scrolls vertically.
fn body_region(bounds: Rectangle, header_height: f32) -> Rectangle {
    Rectangle {
        x: bounds.x,
        y: bounds.y + header_height,
        width: bounds.width,
        height: (bounds.height - header_height).max(0.0),
    }
}

/// Move the cursor into content space, or report it unavailable if it is not
/// over the region at all. Translating the cursor rather than the layout is
/// what keeps child hit-testing correct while scrolled.
fn local_cursor(cursor: mouse::Cursor, region: Rectangle, translation: Vector) -> mouse::Cursor {
    match cursor.position_over(region) {
        Some(position) => mouse::Cursor::Available(position + translation),
        None => mouse::Cursor::Unavailable,
    }
}

/// Which leaf column edge, if any, sits under this point.
///
/// Edges are hit-tested in *screen* space, so the horizontal scroll offset has
/// to be subtracted -- the widths and offsets in `State` are content space.
/// Scanning right to left means that when two edges overlap (a column dragged
/// to near-zero width) the drag grabs the rightmost one, which is the one the
/// pointer is visually nearest.
///
/// Restricted to where **both** columns the edge separates show their own leaf
/// headers. A group label sits directly above the edges of every column beneath
/// it, so without this each group is also a row of invisible drag handles --
/// and since the grab zone outranks every other interaction, those handles
/// swallow the clicks meant to select the group.
///
/// Requiring both sides rather than just the left one matters where an
/// ungrouped full-height column meets a grouped one: the tall column's edge
/// would otherwise stay grabbable right through the neighbouring group's label.
fn resize_edge_at(
    point: Point,
    bounds: Rectangle,
    header: Rectangle,
    cells: &[HeaderCell],
    state: &State,
    spacing: f32,
    tolerance: f32,
) -> Option<usize> {
    if !header.contains(point) {
        return None;
    }

    let over_leaf = |column: usize| {
        cells
            .iter()
            .find(|cell| cell.is_leaf() && cell.start == column)
            .is_some_and(|cell| {
                let top = bounds.y + state.header_row_height * cell.row as f32;
                let bottom = bounds.y + state.header_row_height * cell.bottom() as f32;

                point.y >= top && point.y < bottom
            })
    };

    let last = state.widths.len().saturating_sub(1);

    (0..state.widths.len()).rev().find(|&i| {
        let edge = bounds.x + state.offsets[i] + state.widths[i] + spacing / 2.0 - state.offset.x;

        (point.x - edge).abs() <= tolerance
            && over_leaf(i)
            // The trailing edge has no column on its right to agree with.
            && (i == last || over_leaf(i + 1))
    })
}

/// Which header cell sits under a point, in screen coordinates.
///
/// Searched deepest-row-first so a leaf wins over the group stacked above it;
/// their rectangles do not overlap, but ordering the search this way keeps it
/// correct if a future change lets them.
fn header_cell_at(
    point: Point,
    bounds: Rectangle,
    header: Rectangle,
    cells: &[HeaderCell],
    state: &State,
    spacing: f32,
) -> Option<usize> {
    if !header.contains(point) || state.widths.is_empty() {
        return None;
    }

    let mut best: Option<(usize, usize)> = None;

    for (i, cell) in cells.iter().enumerate() {
        let left = bounds.x + state.offsets[cell.start] - state.offset.x;
        let right =
            bounds.x + state.offsets[cell.end] + state.widths[cell.end] - state.offset.x + spacing;
        // `top`, not `row`: the blank band above a group that was pushed down
        // to meet its children is part of that group's hit box, so there is no
        // dead strip in the header that swallows clicks.
        let top = bounds.y + state.header_row_height * cell.top as f32;
        let bottom = bounds.y + state.header_row_height * cell.bottom() as f32;

        if point.x >= left && point.x < right && point.y >= top && point.y < bottom {
            if best.is_none_or(|(_, row)| cell.row >= row) {
                best = Some((i, cell.row));
            }
        }
    }

    best.map(|(i, _)| i)
}

impl<'a, Message: 'a, Theme, Renderer> Widget<Message, Theme, Renderer>
    for DataTable<'a, Message, Theme, Renderer>
where
    Theme: Catalog,
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(&mut self.elements);
    }

    fn size(&self) -> Size<Length> {
        Size::new(self.width, self.height)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let columns = self.columns.len();

        if columns == 0 {
            return layout::Node::new(Size::ZERO);
        }

        let h_pad = self.padding.x();
        let v_pad = self.padding.y();
        let loose = layout::Limits::new(Size::ZERO, Size::INFINITE);

        // Read overrides up front: the measure passes below borrow
        // `tree.children` mutably, so the state borrow has to be finished
        // first. Also resets them if the column count changed, since an
        // override is meaningless once the columns are not the same columns.
        let overrides = {
            let state = tree.state.downcast_mut::<State>();

            if state.overrides.len() != columns {
                state.overrides = vec![None; columns];
            }

            state.overrides.clone()
        };

        // ------------------------------------------------------------------
        // Pass 1 -- measure against loose limits to discover max-content
        // sizes. Nothing is positioned yet.
        // ------------------------------------------------------------------
        let mut intrinsic = vec![0.0f32; columns];
        let mut spans: Vec<SpanRequest> = Vec::new();
        let mut header_row_height = 0.0f32;

        for i in 0..self.header_cells.len() {
            let cell = self.header_cells[i];
            let node = self.elements[cell.element].as_widget_mut().layout(
                &mut tree.children[cell.element],
                renderer,
                &loose,
            );
            let size = node.size();
            // A sortable column has to be wide enough for its label *and* its
            // arrow, or the two overlap the moment the label fills the cell.
            let width = size.width + h_pad + if self.is_sortable(&cell) { SORT_ZONE } else { 0.0 };

            if cell.is_leaf() {
                intrinsic[cell.start] = intrinsic[cell.start].max(width);
            } else {
                spans.push(SpanRequest {
                    start: cell.start,
                    end: cell.end,
                    needed: width,
                });
            }

            // A cell spanning k rows only needs 1/k of its height from each.
            header_row_height =
                header_row_height.max((size.height + v_pad) / cell.row_span.max(1) as f32);
        }

        let sample = self
            .measure_sample
            .unwrap_or(self.row_count)
            .min(self.row_count);
        let mut row_height = self.min_row_height;

        for row in 0..sample {
            for column in 0..columns {
                let index = self.cell_index(row, column);
                let node = self.elements[index].as_widget_mut().layout(
                    &mut tree.children[index],
                    renderer,
                    &loose,
                );
                let size = node.size();

                intrinsic[column] = intrinsic[column].max(size.width + h_pad);
                row_height = row_height.max(size.height + v_pad);
            }
        }

        // ------------------------------------------------------------------
        // Pass 2 -- let group headers push their children wider.
        // ------------------------------------------------------------------
        let sizing = sizing::with_overrides(&self.sizing(), &overrides);
        sizing::apply_span_constraints(&mut intrinsic, &spans, &sizing, self.spacing);

        // ------------------------------------------------------------------
        // Pass 3 -- resolve widths. `Fill` resolves against the viewport, so
        // columns fill the visible area and only overflow (and therefore
        // scroll) when their intrinsic content genuinely exceeds it.
        // ------------------------------------------------------------------
        let header_height = header_row_height * self.header_rows as f32;
        let body_height = row_height * self.row_count as f32;

        // The vertical scrollbar is an overlay, so its lane has to be reserved
        // here or it is drawn on top of the right-hand gutter and the last
        // column -- which makes the one strip that is deliberately not owned by
        // any column the one strip you cannot click.
        //
        // No feedback loop: `row_height` comes from the loose measure in pass 1
        // and does not depend on the final widths, so narrowing the columns
        // cannot change whether the bar was needed.
        let reserved = if self.vertical == Policy::Auto
            && body_height > (limits.max().height - header_height).max(0.0)
        {
            self.scrollbar_width
        } else {
            0.0
        };

        // The gutter is carved out of the available width next, then folded
        // into every column offset. Doing it here means nothing downstream --
        // hit tests, dividers, column bands -- needs to know it exists.
        let available = (limits.max().width - self.gutter * 2.0 - reserved).max(0.0);
        let widths =
            sizing::resolve_widths(&sizing, &intrinsic, available, self.spacing, self.overflow);
        let offsets: Vec<f32> = sizing::offsets(&widths, self.spacing)
            .into_iter()
            .map(|x| x + self.gutter)
            .collect();

        let content_width = widths.iter().sum::<f32>()
            + self.spacing * (columns.saturating_sub(1)) as f32
            + self.gutter * 2.0;

        // ------------------------------------------------------------------
        // Pass 4 -- lay children out again against the width they actually
        // got, then position them in content space. Re-laying out rather than
        // reusing pass 1 because text wrapping (and so height) depends on the
        // final width.
        // ------------------------------------------------------------------
        let mut nodes: Vec<layout::Node> = Vec::with_capacity(self.elements.len());
        nodes.resize_with(self.elements.len(), || layout::Node::new(Size::ZERO));

        for i in 0..self.header_cells.len() {
            let cell = self.header_cells[i];
            let width = sizing::span_width(&widths, cell.start, cell.end, self.spacing);
            let height = header_row_height * cell.row_span as f32;
            let reserve = if self.is_sortable(&cell) { SORT_ZONE } else { 0.0 };
            let inner = Size::new(
                (width - h_pad - reserve).max(0.0),
                (height - v_pad).max(0.0),
            );

            // `align` ADDS an offset; `move_to` SETS the position. Doing them
            // the other way round silently discards the alignment, which is
            // why everything rendered top-left regardless of what was asked
            // for. Position first, then align within the cell.
            nodes[cell.element] = self.elements[cell.element]
                .as_widget_mut()
                .layout(
                    &mut tree.children[cell.element],
                    renderer,
                    &layout::Limits::new(Size::ZERO, inner),
                )
                .move_to(Point::new(
                    offsets[cell.start] + self.padding.left,
                    header_row_height * cell.row as f32 + self.padding.top,
                ))
                .align(
                    if cell.is_leaf() {
                        // A leaf header labels one column, so it should sit
                        // over that column the way the data does.
                        self.columns[cell.start].align.into()
                    } else {
                        // A group label belongs to its whole span, so centre
                        // it across the span rather than over its first leaf.
                        alignment::Alignment::Center
                    },
                    if cell.is_leaf() {
                        // Leaf headers drop to the bottom of their span, next
                        // to the rows they describe. A leaf floating at the
                        // top of a three-row stack reads as belonging to the
                        // group level rather than to the column.
                        alignment::Alignment::End
                    } else {
                        alignment::Alignment::Center
                    },
                    inner,
                );
        }

        for row in 0..self.row_count {
            let y = header_height + row_height * row as f32;

            for column in 0..columns {
                let index = self.cell_index(row, column);
                let inner = Size::new(
                    (widths[column] - h_pad).max(0.0),
                    (row_height - v_pad).max(0.0),
                );

                nodes[index] = self.elements[index]
                    .as_widget_mut()
                    .layout(
                        &mut tree.children[index],
                        renderer,
                        &layout::Limits::new(Size::ZERO, inner),
                    )
                    .move_to(Point::new(
                        offsets[column] + self.padding.left,
                        y + self.padding.top,
                    ))
                    .align(
                        self.columns[column].align.into(),
                        alignment::Alignment::Center,
                        inner,
                    );
            }
        }

        // The node reports the *viewport*, not the content. Anything beyond it
        // is what scrolling reaches.
        let size = limits.resolve(
            self.width,
            self.height,
            Size::new(content_width, header_height + body_height),
        );

        let state = tree.state.downcast_mut::<State>();
        state.widths = widths;
        state.offsets = offsets;
        state.header_row_height = header_row_height;
        state.header_height = header_height;
        state.row_height = row_height;
        state.content = Size::new(content_width, body_height);

        // Content may have shrunk since the last frame (a filter was applied,
        // rows were deleted). Re-clamp so we are never scrolled past the end.
        let viewport = Size::new(size.width, (size.height - header_height).max(0.0));
        state.offset = scroll::clamp_offset(state.offset, viewport, state.content);

        layout::Node::with_children(size, nodes)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let appearance = theme.style(&self.class);
        let bounds = layout.bounds();

        if bounds.intersection(viewport).is_none() {
            return;
        }

        let body = body_region(bounds, state.header_height);
        let offset = state.offset;
        let columns = self.columns.len();

        // Base background only. The frame is drawn at the very end of this
        // method instead of here, because everything below paints over it: the
        // row bands run the full content width, the header band runs the full
        // viewport width, and the stripes run both. Drawn first, a 1px border
        // survives only where nothing happens to cover it -- which is exactly
        // the "border appears on the left of every other row" symptom.
        renderer.fill_quad(
            renderer::Quad {
                bounds,
                ..Default::default()
            },
            appearance
                .row_background
                .unwrap_or(Background::Color(Color::TRANSPARENT)),
        );

        let body_style = renderer::Style {
            text_color: appearance.text.unwrap_or(style.text_color),
        };

        // Inner edges of the two gutters, in content space. Drawing a rule on
        // each is what makes the gutter read as a part of the table -- a margin
        // belonging to the row -- rather than as empty space the table failed
        // to fill. The row backgrounds still run through it, so it stays a
        // place you can click to mean "this row" and nothing else.
        let gutter_edges = (!state.widths.is_empty()).then(|| {
            let last = state.widths.len() - 1;

            (
                bounds.x + self.gutter,
                bounds.x + state.offsets[last] + state.widths[last],
            )
        });

        // ------------------------------------------------------------------
        // Body layer -- clipped to the region below the header, translated on
        // both axes.
        // ------------------------------------------------------------------
        let body_cursor = local_cursor(cursor, body, offset);
        let body_viewport = Rectangle {
            x: body.x + offset.x,
            y: body.y + offset.y,
            width: body.width,
            height: body.height,
        };

        renderer.with_layer(body, |renderer| {
            renderer.with_translation(Vector::new(-offset.x, -offset.y), |renderer| {
                let (first, last) = scroll::visible_rows(
                    offset.y,
                    body.height,
                    state.row_height,
                    self.row_count,
                );

                // Stripes span the full content width, not the viewport, so
                // they stay continuous while scrolled sideways.
                let full_width = state.content.width.max(bounds.width);

                let row_rect = |row: usize| Rectangle {
                    x: bounds.x,
                    y: bounds.y + state.header_height + state.row_height * row as f32,
                    width: full_width,
                    height: state.row_height,
                };

                if let Some(background) = appearance.alternate_row_background {
                    for row in (first..last).filter(|r| r % 2 == 1) {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: row_rect(row),
                                ..Default::default()
                            },
                            background,
                        );
                    }
                }

                // Column bands sit above the stripes but below the row
                // highlights, so a selected row still reads as selected where
                // the two cross.
                if let Some(background) = appearance.selected_column_background {
                    for column in &self.selected_columns {
                        if *column < state.widths.len() {
                            renderer.fill_quad(
                                renderer::Quad {
                                    bounds: Rectangle {
                                        x: bounds.x + state.offsets[*column],
                                        y: body.y + offset.y,
                                        width: state.widths[*column],
                                        height: body.height,
                                    },
                                    ..Default::default()
                                },
                                background,
                            );
                        }
                    }
                }

                // Per-cell overrides, resolved once for the visible slice and
                // then read twice -- once for the background here, once for the
                // inherited text colour further down. Calling the hook twice
                // instead would double whatever the caller does in it.
                let cell_styles: Vec<CellStyle> = match &self.cell_style {
                    Some(cell_style) => (first..last)
                        .flat_map(|row| (0..columns).map(move |column| (row, column)))
                        .map(|(row, column)| {
                            cell_style(
                                theme,
                                Cell {
                                    row,
                                    column,
                                    selected: self.selected.contains(&row),
                                    hovered: state.hovered == Some(row),
                                    column_selected: self.selected_columns.contains(&column),
                                },
                            )
                        })
                        .collect(),
                    None => Vec::new(),
                };

                let style_of = |row: usize, column: usize| {
                    cell_styles
                        .get((row - first) * columns + column)
                        .copied()
                        .unwrap_or_default()
                };

                let cell_rect = |row: usize, column: usize| Rectangle {
                    x: bounds.x + state.offsets[column],
                    y: bounds.y + state.header_height + state.row_height * row as f32,
                    width: state.widths[column],
                    height: state.row_height,
                };

                // Above the stripes and the column band, below hover and
                // selection -- so a selected row still reads as selected across
                // a coloured cell. A translucent colour shows through both.
                if !cell_styles.is_empty() {
                    for row in first..last {
                        for column in 0..columns {
                            if let Some(background) = style_of(row, column).background {
                                renderer.fill_quad(
                                    renderer::Quad {
                                        bounds: cell_rect(row, column),
                                        ..Default::default()
                                    },
                                    background,
                                );
                            }
                        }
                    }
                }

                // Hover first, selection over it: a selected row that is also
                // hovered should still read as selected.
                if let (Some(background), Some(row)) =
                    (appearance.hovered_row_background, state.hovered)
                {
                    if row >= first && row < last && !self.selected.contains(&row) {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: row_rect(row),
                                ..Default::default()
                            },
                            background,
                        );
                    }
                }

                if let Some(background) = appearance.selected_row_background {
                    // Only the visible slice -- `selected` may hold thousands
                    // of rows after a Shift-range over a large table.
                    for row in self.selected.range(first..last) {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: row_rect(*row),
                                ..Default::default()
                            },
                            background,
                        );
                    }
                }

                let rule = appearance.divider_width();

                if let Some(color) = appearance.row_divider {
                    for row in first.max(1)..last {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle {
                                    x: bounds.x,
                                    y: bounds.y
                                        + state.header_height
                                        + state.row_height * row as f32,
                                    width: state.content.width.max(bounds.width),
                                    height: rule,
                                },
                                ..Default::default()
                            },
                            color,
                        );
                    }
                }

                if let Some(color) = appearance.column_divider {
                    for offset_x in state.offsets.iter().skip(1) {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle {
                                    x: bounds.x + offset_x - self.spacing / 2.0 - rule / 2.0,
                                    y: body.y + offset.y,
                                    width: rule,
                                    height: body.height,
                                },
                                ..Default::default()
                            },
                            color,
                        );
                    }
                }

                if let (Some(color), Some((left, right))) =
                    (appearance.gutter_divider, gutter_edges)
                {
                    for x in [left, right] {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle {
                                    x: x - rule / 2.0,
                                    y: body.y + offset.y,
                                    width: rule,
                                    height: body.height,
                                },
                                ..Default::default()
                            },
                            color,
                        );
                    }
                }

                // Outline the selection, but only where a run of selected rows
                // actually begins and ends -- boxing each row individually
                // turns a Shift-range into a ladder of lines instead of one
                // block.
                //
                // After the dividers, not before. A row divider sits at exactly
                // the y of the row's top edge, so drawing the outline first
                // means the divider repaints over it and only the bottom edge
                // (which is inset by one rule) ever survives.
                if let Some(color) = appearance.selected_row_border {
                    for row in self.selected.range(first..last).copied() {
                        let band = row_rect(row);
                        let opens = row == 0 || !self.selected.contains(&(row - 1));
                        let closes = !self.selected.contains(&(row + 1));

                        for (y, draw) in [(band.y, opens), (band.y + band.height - rule, closes)] {
                            if draw {
                                renderer.fill_quad(
                                    renderer::Quad {
                                        bounds: Rectangle {
                                            y,
                                            height: rule,
                                            ..band
                                        },
                                        ..Default::default()
                                    },
                                    color,
                                );
                            }
                        }
                    }
                }

                // Only visible rows are drawn. This is arithmetic on the row
                // range rather than a rectangle test per child, which is what
                // makes large tables cheap to redraw.
                for row in first..last {
                    for column in 0..columns {
                        let index = self.cell_index(row, column);

                        // A cell colour is *inherited*, not imposed: a `text`
                        // with an explicit `.color()` still wins, because it
                        // never consults the renderer style at all.
                        let child_style = match style_of(row, column).text_color {
                            Some(text_color) => renderer::Style { text_color },
                            None => body_style,
                        };

                        self.elements[index].as_widget().draw(
                            &tree.children[index],
                            renderer,
                            theme,
                            &child_style,
                            layout.child(index),
                            body_cursor,
                            &body_viewport,
                        );
                    }
                }
            });
        });

        // ------------------------------------------------------------------
        // Header layer -- same clip discipline, but translated only on X.
        // That single difference is what makes it sticky.
        // ------------------------------------------------------------------
        let header_region = Rectangle {
            x: bounds.x,
            y: bounds.y,
            width: bounds.width,
            height: state.header_height,
        };
        let header_cursor = local_cursor(cursor, header_region, Vector::new(offset.x, 0.0));
        // Children sit in content space, so their viewport has to be pushed by
        // the same offset the body's is -- otherwise a header element scrolled
        // sideways is clipped against a rectangle it is no longer in.
        let header_viewport = Rectangle {
            x: header_region.x + offset.x,
            ..header_region
        };

        let header_style = renderer::Style {
            text_color: appearance.header_text.unwrap_or(style.text_color),
        };
        let group_style = renderer::Style {
            text_color: appearance
                .group_text
                .or(appearance.header_text)
                .unwrap_or(style.text_color),
        };

        renderer.with_layer(header_region, |renderer| {
            // One band behind the whole header, outside the translation so it
            // covers the viewport regardless of horizontal scroll. Group levels
            // are then painted per span on top, rather than as a full-width
            // stripe -- an ungrouped column has no group, and banding across it
            // draws a group level over a column that is not in one.
            if let Some(background) = appearance.header_background {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: header_region,
                        ..Default::default()
                    },
                    background,
                );
            }

            renderer.with_translation(Vector::new(-offset.x, 0.0), |renderer| {
                let rule = appearance.divider_width();
                let band = state.header_row_height;
                let foot = bounds.y + state.header_height;

                let left = |cell: &HeaderCell| bounds.x + state.offsets[cell.start];
                let right =
                    |cell: &HeaderCell| bounds.x + state.offsets[cell.end] + state.widths[cell.end];

                if let Some(background) = appearance.group_background {
                    for cell in self.header_cells.iter().filter(|c| !c.is_leaf()) {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle {
                                    x: left(cell),
                                    y: bounds.y + band * cell.top as f32,
                                    width: (right(cell) - left(cell)).max(0.0),
                                    height: band * (cell.bottom() - cell.top) as f32,
                                },
                                ..Default::default()
                            },
                            background,
                        );
                    }
                }

                // The column band stops at the header on purpose. Tinting the
                // header too turns the label unreadable and, worse, makes the
                // header look like the thing that is selected rather than the
                // control that selected it.

                // Rules follow the *spans*, not the leaf columns -- slicing a
                // group header with the dividers of the columns beneath it is
                // what makes every level look like the same flat grid.
                //
                // Every vertical rule runs from the top of its cell down to the
                // body. Stopping it at the cell's own band leaves the header
                // looking like a grid with pieces missing: a boundary appears
                // on the group row, vanishes on the blank row above it, then
                // reappears over the leaves.
                for cell in &self.header_cells {
                    let top = bounds.y + band * cell.top as f32;
                    let height = (foot - top).max(0.0);

                    let mut rail = |x: f32, color| {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle {
                                    x: x - rule / 2.0,
                                    y: top,
                                    width: rule,
                                    height,
                                },
                                ..Default::default()
                            },
                            color,
                        );
                    };

                    let closing = if cell.is_leaf() {
                        appearance.column_divider
                    } else {
                        appearance.group_divider.or(appearance.column_divider)
                    };

                    if let Some(color) = closing.filter(|_| cell.end + 1 < self.columns.len()) {
                        rail(right(cell) + self.spacing / 2.0, color);
                    }

                    // A group also gets its opening edge, so it is bounded on
                    // both sides instead of inheriting whatever weight its
                    // left-hand neighbour happened to draw.
                    if let Some(color) = appearance
                        .group_divider
                        .filter(|_| !cell.is_leaf() && cell.start > 0)
                    {
                        rail(left(cell) - self.spacing / 2.0, color);
                    }

                    // Underline separating a label from the level below it.
                    // Leaves reach the body and bottom out against
                    // `header_divider` instead.
                    if let Some(color) =
                        appearance.row_divider.filter(|_| cell.bottom() < self.header_rows)
                    {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle {
                                    x: left(cell),
                                    y: bounds.y + band * cell.bottom() as f32 - rule,
                                    width: (right(cell) - left(cell)).max(0.0),
                                    height: rule,
                                },
                                ..Default::default()
                            },
                            color,
                        );
                    }
                }

                // Carried through the header so the gutter is bounded top to
                // bottom, not just alongside the rows.
                if let (Some(color), Some((left, right))) =
                    (appearance.gutter_divider, gutter_edges)
                {
                    for x in [left, right] {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle {
                                    x: x - rule / 2.0,
                                    y: bounds.y,
                                    width: rule,
                                    height: state.header_height,
                                },
                                ..Default::default()
                            },
                            color,
                        );
                    }
                }

                if state.widths.len() == self.columns.len() {
                    for cell in self.header_cells.iter().filter(|c| self.is_sortable(c)) {
                        let zone = self.sort_zone(cell, state, bounds);

                        if state.hovered_sort == Some(cell.start) {
                            if let Some(background) = appearance.sort_hovered_background {
                                renderer.fill_quad(
                                    renderer::Quad {
                                        bounds: Rectangle {
                                            y: zone.center_y() - 9.0,
                                            height: 18.0,
                                            ..zone
                                        },
                                        border: Border {
                                            radius: 3.0.into(),
                                            ..Default::default()
                                        },
                                        ..Default::default()
                                    },
                                    background,
                                );
                            }
                        }

                        // The pair is always drawn, and always the same shape.
                        // Only the emphasis moves: unsorted dims both, sorted
                        // lights the one pointing the way the column runs.
                        // Swapping the glyph out per state makes the control
                        // read as three different buttons rather than as one
                        // button in three states.
                        let sort = self.sort.filter(|sort| sort.column == cell.start);
                        let x = zone.center_x() - SORT_ARROW / 2.0;
                        let top = zone.center_y() - 5.5;

                        for (index, up) in [(0.0, true), (1.0, false)] {
                            let lit = sort.is_some_and(|sort| {
                                (sort.direction == Direction::Ascending) == up
                            });

                            let color = if lit {
                                appearance.sort_indicator
                            } else {
                                appearance.sort_indicator_inactive
                            };

                            if let Some(color) = color {
                                arrow(
                                    renderer,
                                    Rectangle {
                                        x,
                                        y: top + index * 7.0,
                                        width: SORT_ARROW,
                                        height: 4.0,
                                    },
                                    up,
                                    color,
                                );
                            }
                        }
                    }
                }

                for cell in &self.header_cells {
                    self.elements[cell.element].as_widget().draw(
                        &tree.children[cell.element],
                        renderer,
                        theme,
                        if cell.is_leaf() {
                            &header_style
                        } else {
                            &group_style
                        },
                        layout.child(cell.element),
                        header_cursor,
                        &header_viewport,
                    );
                }
            });

            if let Some(color) = appearance.header_divider {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle {
                            y: bounds.y + state.header_height - appearance.divider_width(),
                            height: appearance.divider_width(),
                            ..header_region
                        },
                        ..Default::default()
                    },
                    color,
                );
            }
        });

        // ------------------------------------------------------------------
        // Scrollbars -- untranslated, and in a layer of their own.
        //
        // Layers render in creation order, and drawing into the base layer
        // after a sub-layer closes still puts the primitives in the base
        // layer, which renders *underneath*. Opening a fresh layer here is
        // what actually puts the scrollbars on top of the stripes and rules.
        // ------------------------------------------------------------------
        let (vertical, horizontal) = scroll::scrollbars(
            body,
            state.content,
            offset,
            self.scrollbar_width,
            self.vertical,
            self.horizontal,
        );

        renderer.with_layer(bounds, |renderer| {
            for bar in [vertical, horizontal].into_iter().flatten() {
                if let Some(track) = appearance.scrollbar_track {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: bar.track,
                            ..Default::default()
                        },
                        track,
                    );
                }

                let hovered = cursor
                    .position()
                    .is_some_and(|point| bar.track.contains(point));

                renderer.fill_quad(
                    renderer::Quad {
                        bounds: bar.thumb,
                        border: appearance.scrollbar_border,
                        ..Default::default()
                    },
                    if hovered {
                        appearance.scrollbar_thumb_hovered
                    } else {
                        appearance.scrollbar_thumb
                    },
                );
            }
        });

        // No separate resize indicator. The columns now relayout on every frame
        // of the drag, so the moving edge *is* the feedback -- a second mark
        // drawn over it only competes with the thing it was meant to point at.
        // It existed to stand in for the live update that was missing.

        // ------------------------------------------------------------------
        // Frame -- last, and in its own layer.
        //
        // A table without a visible outer edge is genuinely hard to read: with
        // the gutter there is blank space at both sides that belongs to the
        // table, and nothing to say where the table stops and the container
        // behind it starts. Drawing the frame up front does not work, because
        // every band drawn afterwards is full-width and paints straight over
        // it. Drawn here it survives, including over the scrollbars.
        // ------------------------------------------------------------------
        if appearance.border.width > 0.0 {
            renderer.with_layer(bounds, |renderer| {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds,
                        border: appearance.border,
                        ..Default::default()
                    },
                    Background::Color(Color::TRANSPARENT),
                );
            });
        }
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();

        // Tracked first: every branch below can return early, and a missed
        // ModifiersChanged leaves Shift stuck in whatever state it was last
        // seen in.
        if let Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) = event {
            tree.state.downcast_mut::<State>().modifiers = *modifiers;
        }

        let (header_height, row_height, content, offset) = {
            let state = tree.state.downcast_ref::<State>();
            (
                state.header_height,
                state.row_height,
                state.content,
                state.offset,
            )
        };

        let body = body_region(bounds, header_height);
        let viewport_size = Size::new(body.width, body.height);
        let (vertical, horizontal) = scroll::scrollbars(
            body,
            content,
            offset,
            self.scrollbar_width,
            self.vertical,
            self.horizontal,
        );

        // --- 1. column resizing, which outranks everything else ---
        if self.resizable {
            let header_region = Rectangle {
                height: header_height,
                ..bounds
            };

            let state = tree.state.downcast_mut::<State>();

            match event {
                Event::Mouse(mouse::Event::CursorMoved { .. })
                | Event::Touch(touch::Event::FingerMoved { .. }) => {
                    if let (Some(resize), Some(point)) = (state.resizing, cursor.position()) {
                        let width = (resize.origin_width + (point.x - resize.origin_x))
                            .max(self.min_column_width);

                        if state.overrides.len() > resize.column {
                            state.overrides[resize.column] = Some(width);
                        }

                        // Both, and neither on its own. `invalidate_layout`
                        // only sets a dirty flag -- it never touches the shell's
                        // redraw request -- so on its own it re-runs layout for
                        // whatever frame happens to come next and schedules no
                        // frame at all. That is why the drag appeared frozen
                        // until the button came up: `ButtonReleased` below asks
                        // for a redraw, and that was the first frame the whole
                        // gesture produced. `request_redraw` alone is the
                        // opposite mistake -- it would paint stale geometry,
                        // since column widths are layout, not paint.
                        shell.invalidate_layout();
                        shell.request_redraw();
                        shell.capture_event();
                        return;
                    }
                }
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                    if let Some(point) = cursor.position() {
                        if let Some(column) = resize_edge_at(
                            point,
                            bounds,
                            header_region,
                            &self.header_cells,
                            state,
                            self.spacing,
                            self.resize_tolerance,
                        ) {
                            // Pin every column to the width it has right now,
                            // before the drag starts.
                            //
                            // Otherwise the dragged edge does not move at all
                            // whenever any `Fill` column is present. Shrinking
                            // the dragged column hands its width back to the
                            // slack pool, `Fill` immediately re-absorbs it, and
                            // the boundary lands exactly where it started --
                            // while some unrelated column silently changes size
                            // to pay for it. Dragging the left edge of a column
                            // would shrink its *neighbour* and leave the bar
                            // under the pointer untouched.
                            //
                            // Pinning is deliberately permanent: once the user
                            // has sized a column by hand, re-deriving widths
                            // from content or from leftover space would undo
                            // their work on the next relayout.
                            let pinned = state.widths.len().min(state.overrides.len());

                            for i in 0..pinned {
                                if state.overrides[i].is_none() {
                                    state.overrides[i] = Some(state.widths[i]);
                                }
                            }

                            state.resizing = Some(Resize {
                                column,
                                origin_x: point.x,
                                origin_width: state.widths[column],
                            });
                            shell.capture_event();
                            shell.invalidate_layout();
                            shell.request_redraw();
                            return;
                        }
                    }
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                    if state.resizing.take().is_some() {
                        shell.request_redraw();
                    }
                }
                _ => {}
            }
        }

        // --- 2. scrollbar interaction ---
        {
            let state = tree.state.downcast_mut::<State>();

            match event {
                Event::Mouse(mouse::Event::CursorMoved { .. })
                | Event::Touch(touch::Event::FingerMoved { .. }) => {
                    if let Some(point) = cursor.position() {
                        if let (Some(grab), Some(bar)) = (state.y_grab, vertical) {
                            state.offset.y = scroll::offset_from_thumb(
                                bar.track.height,
                                viewport_size.height,
                                content.height,
                                point.y - bar.track.y - grab,
                            );
                            shell.request_redraw();
                            shell.capture_event();
                            return;
                        }

                        if let (Some(grab), Some(bar)) = (state.x_grab, horizontal) {
                            state.offset.x = scroll::offset_from_thumb(
                                bar.track.width,
                                viewport_size.width,
                                content.width,
                                point.x - bar.track.x - grab,
                            );
                            shell.request_redraw();
                            shell.capture_event();
                            return;
                        }
                    }
                }
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
                | Event::Touch(touch::Event::FingerPressed { .. }) => {
                    if let Some(point) = cursor.position() {
                        if let Some(bar) = vertical {
                            if bar.track.contains(point) {
                                // Clicking the track jumps the thumb to the
                                // cursor and then drags from its centre, which
                                // is what every native scrollbar does.
                                let grab = if bar.thumb.contains(point) {
                                    point.y - bar.thumb.y
                                } else {
                                    let centred = bar.thumb.height / 2.0;
                                    state.offset.y = scroll::offset_from_thumb(
                                        bar.track.height,
                                        viewport_size.height,
                                        content.height,
                                        point.y - bar.track.y - centred,
                                    );
                                    centred
                                };

                                state.y_grab = Some(grab);
                                shell.request_redraw();
                                shell.capture_event();
                                return;
                            }
                        }

                        if let Some(bar) = horizontal {
                            if bar.track.contains(point) {
                                let grab = if bar.thumb.contains(point) {
                                    point.x - bar.thumb.x
                                } else {
                                    let centred = bar.thumb.width / 2.0;
                                    state.offset.x = scroll::offset_from_thumb(
                                        bar.track.width,
                                        viewport_size.width,
                                        content.width,
                                        point.x - bar.track.x - centred,
                                    );
                                    centred
                                };

                                state.x_grab = Some(grab);
                                shell.request_redraw();
                                shell.capture_event();
                                return;
                            }
                        }
                    }
                }
                Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
                | Event::Touch(touch::Event::FingerLifted { .. })
                | Event::Touch(touch::Event::FingerLost { .. }) => {
                    if state.y_grab.take().is_some() | state.x_grab.take().is_some() {
                        shell.request_redraw();
                    }
                }
                _ => {}
            }
        }

        // --- 3. forward to children in their own coordinate space ---
        let header_region = Rectangle {
            height: header_height,
            ..bounds
        };
        let header_cursor = local_cursor(cursor, header_region, Vector::new(offset.x, 0.0));
        let header_viewport = Rectangle {
            x: header_region.x + offset.x,
            ..header_region
        };
        let body_cursor = local_cursor(cursor, body, offset);
        let body_viewport = Rectangle {
            x: body.x + offset.x,
            y: body.y + offset.y,
            width: body.width,
            height: body.height,
        };

        for index in 0..self.elements.len() {
            let is_header = index < self.header_len;

            let (child_cursor, child_viewport) = if is_header {
                (header_cursor, header_viewport)
            } else {
                (body_cursor, body_viewport)
            };

            self.elements[index].as_widget_mut().update(
                &mut tree.children[index],
                event,
                layout.child(index),
                child_cursor,
                renderer,
                shell,
                &child_viewport,
            );
        }

        // --- 4. row hover and selection ---
        //
        // Deliberately after child forwarding: a button inside a cell captures
        // its own press, so clicking "Open" does not also select the row.
        if shell.is_event_captured() {
            return;
        }

        if self.mode != Mode::None {
            match event {
                Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                    let hovered = cursor
                        .position()
                        .and_then(|point| {
                            scroll::row_at(point, body, offset.y, row_height, self.row_count)
                        });

                    let state = tree.state.downcast_mut::<State>();

                    if state.hovered != hovered {
                        state.hovered = hovered;
                        shell.request_redraw();
                    }
                }
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                    if let Some(row) = cursor.position().and_then(|point| {
                        scroll::row_at(point, body, offset.y, row_height, self.row_count)
                    }) {
                        let state = tree.state.downcast_mut::<State>();
                        let modifiers = state.modifiers;

                        if let Some(outcome) = selection::apply(
                            self.mode,
                            &self.selected,
                            state.anchor,
                            row,
                            modifiers.command(),
                            modifiers.shift(),
                        ) {
                            state.anchor = outcome.anchor;

                            if let Some(on_select) = &self.on_select {
                                shell.publish(on_select(outcome.selection));
                            }

                            // Rows and columns are alternative readings of the
                            // same grid, so selecting one clears the other.
                            // The widget cannot mutate a set it does not own,
                            // so it publishes a second message instead --
                            // guarded, to avoid emitting a no-op every click.
                            if !self.selected_columns.is_empty() {
                                if let Some(on_columns) = &self.on_select_column {
                                    shell.publish(on_columns(BTreeSet::new()));
                                }
                            }

                            shell.capture_event();
                            shell.request_redraw();
                            return;
                        }
                    }
                }
                _ => {}
            }
        }

        // --- 5. sorting, from the control at the end of a sortable header ---
        //
        // Ahead of column selection so the two never fight over one click: the
        // sort control owns a small zone inside the header cell, and every
        // other pixel of that cell still selects the column.
        if self.on_sort.is_some() {
            let hit = cursor.position().and_then(|point| {
                self.sort_zone_at(point, bounds, tree.state.downcast_ref::<State>())
            });

            match event {
                // Tracked so the control can light up before it is clicked. A
                // 16px target that gives no feedback until you land on it is a
                // target you cannot find.
                Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                    let state = tree.state.downcast_mut::<State>();

                    if state.hovered_sort != hit {
                        state.hovered_sort = hit;
                        shell.request_redraw();
                    }
                }
                Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                    if let (Some(column), Some(on_sort)) = (hit, &self.on_sort) {
                        shell.publish(on_sort(sort::next(self.sort, column)));
                        shell.capture_event();
                        shell.request_redraw();
                        return;
                    }
                }
                _ => {}
            }
        }

        // --- 6. column selection from header clicks ---
        if self.column_mode != Mode::None {
            if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event {
                let header_region = Rectangle {
                    height: header_height,
                    ..bounds
                };

                let hit = cursor.position().and_then(|point| {
                    let state = tree.state.downcast_ref::<State>();
                    header_cell_at(
                        point,
                        bounds,
                        header_region,
                        &self.header_cells,
                        state,
                        self.spacing,
                    )
                });

                if let Some(i) = hit {
                    let cell = self.header_cells[i];
                    let state = tree.state.downcast_mut::<State>();
                    let modifiers = state.modifiers;
                    let toggle = modifiers.command();

                    let outcome = if cell.is_leaf() {
                        selection::apply(
                            self.column_mode,
                            &self.selected_columns,
                            state.column_anchor,
                            cell.start,
                            toggle,
                            modifiers.shift(),
                        )
                    } else if self.column_mode == Mode::Single {
                        selection::apply(
                            self.column_mode,
                            &self.selected_columns,
                            state.column_anchor,
                            cell.start,
                            false,
                            false,
                        )
                    } else {
                        // A group covers its whole span. That is exactly a
                        // range extension from its first leaf to its last, so
                        // it reuses the same resolution logic rather than
                        // needing a second code path.
                        selection::apply(
                            self.column_mode,
                            &self.selected_columns,
                            Some(cell.start),
                            cell.end,
                            toggle,
                            true,
                        )
                    };

                    if let Some(outcome) = outcome {
                        state.column_anchor = outcome.anchor;

                        if let Some(on_select) = &self.on_select_column {
                            shell.publish(on_select(outcome.selection));
                        }

                        if !self.selected.is_empty() {
                            if let Some(on_rows) = &self.on_select {
                                shell.publish(on_rows(BTreeSet::new()));
                            }
                        }

                        shell.capture_event();
                        shell.request_redraw();
                        return;
                    }
                }
            }
        }

        // --- 7. wheel, only if nothing downstream claimed the event ---
        if shell.is_event_captured() {
            return;
        }

        if let Event::Mouse(mouse::Event::WheelScrolled { delta }) = event {
            if cursor.is_over(bounds) {
                let state = tree.state.downcast_mut::<State>();
                let shift = state.modifiers.shift();
                let step = row_height.max(MIN_WHEEL_STEP);

                let (dx, dy) = match delta {
                    mouse::ScrollDelta::Lines { x, y } => {
                        // macOS already swaps the axes itself when Shift is
                        // held, so swapping again here would cancel it out.
                        // Everywhere else the wheel still reports on Y and the
                        // swap is ours to do.
                        let (x, y) = if cfg!(target_os = "macos") && shift {
                            (*y, *x)
                        } else {
                            (*x, *y)
                        };

                        if shift {
                            (y * step, x * step)
                        } else {
                            (x * step, y * step)
                        }
                    }
                    mouse::ScrollDelta::Pixels { x, y } => {
                        if shift {
                            (*y, *x)
                        } else {
                            (*x, *y)
                        }
                    }
                };

                let before = state.offset;

                state.offset = scroll::clamp_offset(
                    Vector::new(state.offset.x - dx, state.offset.y - dy),
                    viewport_size,
                    content,
                );

                if state.offset != before {
                    shell.request_redraw();
                    shell.capture_event();
                }
            }
        }

        let _ = viewport;
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();
        let body = body_region(bounds, state.header_height);

        let (vertical, horizontal) = scroll::scrollbars(
            body,
            state.content,
            state.offset,
            self.scrollbar_width,
            self.vertical,
            self.horizontal,
        );

        if state.y_grab.is_some() || state.x_grab.is_some() {
            return mouse::Interaction::Grabbing;
        }

        let header_region = Rectangle {
            height: state.header_height,
            ..bounds
        };

        if self.resizable {
            if state.resizing.is_some() {
                return mouse::Interaction::ResizingHorizontally;
            }

            if let Some(point) = cursor.position() {
                if resize_edge_at(
                    point,
                    bounds,
                    header_region,
                    &self.header_cells,
                    state,
                    self.spacing,
                    self.resize_tolerance,
                )
                .is_some()
                {
                    return mouse::Interaction::ResizingHorizontally;
                }
            }
        }

        if let Some(point) = cursor.position() {
            let on_bar = [vertical, horizontal]
                .into_iter()
                .flatten()
                .any(|bar| bar.track.contains(point));

            if on_bar {
                return mouse::Interaction::Idle;
            }
        }

        let header_cursor = local_cursor(cursor, header_region, Vector::new(state.offset.x, 0.0));
        let body_cursor = local_cursor(cursor, body, state.offset);

        (0..self.elements.len())
            .map(|index| {
                let child_cursor = if index < self.header_len {
                    header_cursor
                } else {
                    body_cursor
                };

                self.elements[index].as_widget().mouse_interaction(
                    &tree.children[index],
                    layout.child(index),
                    child_cursor,
                    &bounds,
                    renderer,
                )
            })
            .max()
            .unwrap_or_default()
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        // On master, `container` only reports this widget's own bounds; child
        // traversal is a separate `traverse` call. Same split the built-in
        // container widget uses.
        operation.container(None, layout.bounds());

        operation.traverse(&mut |operation| {
            for index in 0..self.elements.len() {
                self.elements[index].as_widget_mut().operate(
                    &mut tree.children[index],
                    layout.child(index),
                    renderer,
                    operation,
                );
            }
        });
    }

    fn overlay<'b>(
        &'b mut self,
        tree: &'b mut Tree,
        layout: Layout<'b>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<iced::advanced::overlay::Element<'b, Message, Theme, Renderer>> {
        let offset = tree.state.downcast_ref::<State>().offset;

        // Overlays (menus, tooltips) opened from a scrolled cell have to be
        // pushed by the same amount the cell was visually shifted, or they
        // appear at the unscrolled position.
        iced::advanced::overlay::from_children(
            &mut self.elements,
            tree,
            layout,
            renderer,
            viewport,
            translation - offset,
        )
    }
}

impl<'a, Message, Theme, Renderer> From<DataTable<'a, Message, Theme, Renderer>>
    for Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: Catalog + 'a,
    Renderer: renderer::Renderer + 'a,
{
    fn from(table: DataTable<'a, Message, Theme, Renderer>) -> Self {
        Element::new(table)
    }
}

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
use iced::border::Radius;
use iced::{
    alignment, keyboard, mouse, touch, Background, Border, Color, Element, Event, Length, Padding,
    Point, Rectangle, Size, Vector,
};

use crate::header::{self, ColumnSpec, HeaderCell, HeaderNode};
use crate::scroll::{self, Policy};
use crate::selection::{self, CellPosition, Mode};
use crate::sizing::{self, Overflow, Sizing, SpanRequest};
use crate::sort::{self, Direction, Sort};
use crate::style::{Catalog, Cell, CellStyle, Style};

/// Fallback scroll step when a wheel reports lines and rows are tiny.
const MIN_WHEEL_STEP: f32 = 16.0;

/// Width of the sort control at the trailing end of a sortable leaf header.
/// Doubles as its click target, so it is wider than the arrows inside it.
const SORT_ZONE: f32 = 16.0;

/// Width of the arrows drawn inside that zone.
const SORT_ARROW: f32 = 8.0;

/// How far the pointer must travel before a press on a header becomes a
/// reorder rather than a click that selects the column.
const REORDER_THRESHOLD: f32 = 4.0;

/// Clear space between a header's label and its sort control.
///
/// Reserved on top of `SORT_ZONE` rather than taken out of it, so the gap does
/// not eat the click target. It only shows up on labels that reach the control
/// -- a right-aligned header like "Revenue", or any header whose text fills the
/// column -- which is why widening the column never opened the gap on its own.
const SORT_GAP: f32 = 5.0;

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
    sticky_columns: usize,

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

    on_right_click: Option<Box<dyn Fn(Click) -> Message + 'a>>,
    on_copy: Option<Box<dyn Fn() -> Message + 'a>>,
    on_reorder: Option<Box<dyn Fn(usize, usize) -> Message + 'a>>,

    cell_mode: Mode,
    selected_cells: BTreeSet<CellPosition>,
    on_select_cell: Option<Box<dyn Fn(BTreeSet<CellPosition>) -> Message + 'a>>,

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
    /// Content-space x at which the frozen columns end. Zero when none are
    /// sticky, which makes every frozen-aware calculation collapse back to the
    /// plain one.
    frozen_width: f32,
    /// Full extent of the body content, excluding the header.
    content: Size,

    /// Scroll position in content pixels.
    offset: Vector,
    /// Grab point within the thumb while dragging, per axis.
    y_grab: Option<f32>,
    x_grab: Option<f32>,

    /// Widest content ever measured per column, and the row count that
    /// measurement belongs to. Together they keep column widths from twitching
    /// when the same rows are merely reordered.
    measured: Vec<f32>,
    measured_rows: usize,

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
    cell_anchor: Option<CellPosition>,

    /// The moving end of the selection -- the "active" cell a spreadsheet draws
    /// its heavy outline around.
    ///
    /// Distinct from the anchor, and it has to be. A Shift+arrow steps the
    /// *focus* one place and re-spans from the anchor; stepping the anchor
    /// instead makes every extend measure one step from the origin, so the
    /// block never grows past two items however long you hold the key.
    row_focus: Option<usize>,
    column_focus: Option<usize>,
    cell_focus: Option<CellPosition>,
    hovered: Option<usize>,
    /// Leaf column whose sort control the pointer is over.
    hovered_sort: Option<usize>,

    /// A sweep in progress. Ephemeral, like the anchors.
    drag: Option<Drag>,
    /// A column being dragged to a new position.
    reorder: Option<ReorderDrag>,

    /// Whether the last click landed inside the table.
    ///
    /// Arrow keys are global -- every widget sees them -- so without this the
    /// table would fight every other focusable thing on the page for them.
    /// Click-to-focus rather than a real focus operation: the widget has no
    /// `Id`, and this is enough to make the keys behave.
    focused: bool,
}

/// A column being dragged to a new position.
#[derive(Debug, Clone)]
struct ReorderDrag {
    column: usize,
    /// Pointer x at the press, to measure the threshold from.
    origin: f32,
    /// How far into the column the press landed, so the carried chip sits under
    /// the pointer where it was picked up instead of jumping to centre itself.
    grab: f32,
    /// Boundaries this column is allowed to land on, worked out once at the
    /// press rather than per frame -- they cannot change mid-drag.
    slots: Vec<usize>,
    /// The boundary currently under the pointer. `None` until the drag clears
    /// the threshold, which is what keeps a plain click on a header from
    /// flashing a drop indicator before it resolves into a selection.
    target: Option<usize>,
}

/// A drag-select in progress.
#[derive(Debug, Clone)]
struct Drag {
    kind: DragKind,
    /// The selection to union each sweep onto.
    ///
    /// Empty for a plain drag, which makes the sweep a straight replace. For a
    /// Ctrl-drag it holds the selection as it stood just after the press, so
    /// the second block accumulates onto the first. It has to be a *snapshot*:
    /// unioning against the live selection can only ever grow, so dragging
    /// back over your own path would never give anything up.
    base: DragBase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragKind {
    Cell,
    Row,
    Column,
}

#[derive(Debug, Clone)]
enum DragBase {
    Cells(BTreeSet<CellPosition>),
    Linear(BTreeSet<usize>),
}

/// Where a click landed, in table terms rather than pixels.
///
/// Both coordinates are optional because both can genuinely miss: the header
/// has no row, and the gutters belong to no column. A context menu usually
/// wants to offer different items for each case, so the distinction is kept
/// rather than collapsed to a "nearest" guess.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Click {
    /// Body row, or `None` in the header.
    pub row: Option<usize>,
    /// Leaf column, or `None` in a gutter.
    pub column: Option<usize>,
    /// Screen position, for placing a menu at the pointer.
    pub position: Point,
}

/// The one thing a click resolves to. Rows, columns and cells are three
/// readings of the same grid, so a click picks exactly one and the other two
/// are cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Cell(CellPosition),
    Row(usize),
    Column(usize),
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
            sticky_columns: flat.sticky_columns,
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
            on_right_click: None,
            on_copy: None,
            on_reorder: None,
            cell_mode: Mode::None,
            selected_cells: BTreeSet::new(),
            on_select_cell: None,
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

    /// Enable single-cell selection.
    ///
    /// Turning this on **changes what a body click means**: a click inside any
    /// column now selects that cell, and the [`gutter`](Self::gutter) becomes
    /// the only place a click still means "this row". That is the whole reason
    /// the gutter exists -- without it, enabling cell selection would leave row
    /// selection with nowhere to live, because every pixel of a row is owned by
    /// some column.
    ///
    /// Cell, row and column selections are mutually exclusive: they are three
    /// readings of the same grid, and holding two at once leaves no way to tell
    /// which one an action would apply to. Selecting a cell therefore clears
    /// the other two, and they clear it.
    ///
    /// Same ownership rule as everything else here -- you hold the selection,
    /// the widget renders it and reports what it should become.
    /// `Mode::Multiple` gives the standard spreadsheet gestures: Ctrl toggles
    /// one cell, Shift extends a **rectangle** from the anchor, and dragging
    /// sweeps a rectangle out live.
    pub fn cell_selection(
        mut self,
        mode: Mode,
        selected: &BTreeSet<CellPosition>,
        on_select: impl Fn(BTreeSet<CellPosition>) -> Message + 'a,
    ) -> Self {
        self.cell_mode = mode;
        self.selected_cells = selected.clone();
        self.on_select_cell = Some(Box::new(on_select));
        self
    }

    /// Let leaf columns be dragged to new positions, and report where they land.
    ///
    /// The arguments are leaf column indices, meant exactly as
    /// [`selection::reorder`](crate::selection::reorder) applies them: remove
    /// `from`, insert at `to`. For a flat header that is a straight
    /// `Vec::remove` + `Vec::insert` on your own column list.
    ///
    /// The table does **not** reorder itself, for the same reason it does not
    /// sort itself: the order belongs to your column definitions, and a widget
    /// that quietly kept its own permutation would disagree with them the
    /// moment you added or removed a column.
    ///
    /// A column may only move **among its own siblings**, and never across the
    /// frozen boundary. Both constraints exist to keep each group's leaves
    /// contiguous -- a group whose columns are no longer adjacent has no
    /// rectangle to draw its label in. So a top-level column steps over a whole
    /// group in one move rather than landing inside it.
    pub fn on_reorder(mut self, on_reorder: impl Fn(usize, usize) -> Message + 'a) -> Self {
        self.on_reorder = Some(Box::new(on_reorder));
        self
    }

    /// Fire on Ctrl+C (Cmd+C on macOS) while the table has focus and something
    /// is selected.
    ///
    /// The table cannot do the copying, and it is worth being clear why: it
    /// only ever sees the `Element` you built from a value, never the value
    /// itself. There is no text for it to put on the clipboard.
    ///
    /// What it *can* contribute is the part that is easy to get wrong -- when
    /// the shortcut belongs to this table rather than to some other focused
    /// widget, and, via [`selection::grid`](crate::selection::grid), the
    /// rectangle a spreadsheet expects. Build the text from your own data and
    /// hand it to `iced::clipboard::write`.
    pub fn on_copy(mut self, on_copy: impl Fn() -> Message + 'a) -> Self {
        self.on_copy = Some(Box::new(on_copy));
        self
    }

    /// Report right-clicks, with the cell they landed on.
    ///
    /// The table does not open a menu -- it tells you where the click was and
    /// leaves the menu to you. Anything else would mean the widget owning a
    /// list of actions it cannot know, and a popup it cannot style.
    ///
    /// A right-click on a cell whose own contents handle the event (a button,
    /// a text input) never reaches here, the same rule left-clicks follow.
    pub fn on_right_click(mut self, on_right_click: impl Fn(Click) -> Message + 'a) -> Self {
        self.on_right_click = Some(Box::new(on_right_click));
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

    /// The body element indices belonging to the rows currently on screen.
    ///
    /// `elements` is row-major and contiguous, so a row range maps onto a
    /// single index range with no gaps. That is what lets a pass be culled to
    /// the viewport with one range rather than a rectangle test per child.
    fn visible_body_range(
        &self,
        offset_y: f32,
        body_height: f32,
        row_height: f32,
    ) -> std::ops::Range<usize> {
        let (first, last) = scroll::visible_rows(offset_y, body_height, row_height, self.row_count);
        let columns = self.columns.len();

        self.header_len + first * columns..self.header_len + last * columns
    }

    /// Screen x of a drop boundary -- the gap a column would be inserted into.
    fn slot_x(&self, slot: usize, bounds: Rectangle, state: &State) -> Option<f32> {
        let last = state.widths.len().checked_sub(1)?;

        let content = if slot < state.offsets.len() {
            state.offsets[slot] - self.spacing / 2.0
        } else {
            state.offsets[last] + state.widths[last] + self.spacing / 2.0
        };

        Some(screen_x(content, bounds, state))
    }

    /// Continue an in-progress column reorder. Returns whether the event was
    /// consumed.
    fn drag_reorder(
        &self,
        tree: &mut Tree,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
        shell: &mut Shell<'_, Message>,
    ) -> bool {
        match event {
            Event::Mouse(mouse::Event::CursorMoved { .. })
            | Event::Touch(touch::Event::FingerMoved { .. }) => {
                let Some(point) = cursor.position() else {
                    return false;
                };

                let Some((column, origin, slots)) = ({
                    let state = tree.state.downcast_ref::<State>();

                    state
                        .reorder
                        .as_ref()
                        .map(|drag| (drag.column, drag.origin, drag.slots.clone()))
                }) else {
                    return false;
                };

                // Below the threshold this is still a click. Committing to a
                // drag on the first pixel of movement makes a header
                // impossible to click without nudging a column.
                if (point.x - origin).abs() < REORDER_THRESHOLD {
                    return false;
                }

                let nearest = slots
                    .iter()
                    .copied()
                    .filter_map(|slot| {
                        let state = tree.state.downcast_ref::<State>();

                        self.slot_x(slot, bounds, state)
                            .map(|x| (slot, (x - point.x).abs()))
                    })
                    .min_by(|(_, a), (_, b)| a.total_cmp(b))
                    .map(|(slot, _)| slot);

                let state = tree.state.downcast_mut::<State>();

                if let Some(drag) = state.reorder.as_mut() {
                    drag.target = nearest;
                }

                // Every frame of the drag, not just the ones where the drop
                // target changed. The carried chip follows the pointer
                // continuously, so redrawing only on a target change leaves it
                // frozen wherever the line last moved.
                let _ = column;
                shell.request_redraw();
                shell.capture_event();
                true
            }

            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. })
            | Event::Touch(touch::Event::FingerLost { .. }) => {
                let Some(drag) = tree.state.downcast_mut::<State>().reorder.take() else {
                    return false;
                };

                // No target at all means the threshold was never cleared, so
                // this was a click and the column selection it made on press
                // stands. A target that is not a *move* means the column was
                // picked up and put back down where it started -- also nothing
                // to report, and the case that made a small nudge shove the
                // column onto its neighbour.
                let Some(slot) = drag.target.filter(|&slot| header::is_move(drag.column, slot))
                else {
                    shell.request_redraw();
                    return false;
                };

                // A slot is a gap in the *current* order. Taking the column out
                // first shifts everything above it down, so a rightward move
                // lands one place short unless the index is stepped back.
                let to = if slot > drag.column { slot - 1 } else { slot };

                if let Some(on_reorder) = &self.on_reorder {
                    shell.publish(on_reorder(drag.column, to));
                }

                shell.capture_event();
                shell.request_redraw();
                true
            }

            _ => false,
        }
    }

    /// Continue an in-progress drag-select. Returns whether the event was
    /// consumed.
    ///
    /// Every kind re-resolves from the anchor fixed at the press rather than
    /// accumulating per-move deltas, which is what lets a sweep give ground
    /// back when you drag toward where you started.
    #[allow(clippy::too_many_arguments)]
    fn sweep(
        &self,
        tree: &mut Tree,
        event: &Event,
        bounds: Rectangle,
        body: Rectangle,
        cursor: mouse::Cursor,
        row_height: f32,
        offset: Vector,
        shell: &mut Shell<'_, Message>,
    ) -> bool {
        match event {
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. })
            | Event::Touch(touch::Event::FingerLost { .. }) => {
                // Cleared, but the release itself is left alone: other handlers
                // still want it, and capturing here would swallow a child's
                // button press completing.
                tree.state.downcast_mut::<State>().drag = None;
                false
            }

            Event::Mouse(mouse::Event::CursorMoved { .. })
            | Event::Touch(touch::Event::FingerMoved { .. }) => {
                let Some(point) = cursor.position() else {
                    return false;
                };

                let (drag, cell_anchor, row_anchor, column_anchor, column) = {
                    let state = tree.state.downcast_ref::<State>();

                    (
                        state.drag.clone(),
                        state.cell_anchor,
                        state.anchor,
                        state.column_anchor,
                        self.column_at(point, bounds, state),
                    )
                };

                let Some(drag) = drag else {
                    return false;
                };

                let row = scroll::row_at(point, body, offset.y, row_height, self.row_count);

                // A sweep also moves the focus, so releasing the button and
                // pressing Shift+arrow continues from where the drag stopped.
                match (drag.kind, &drag.base) {
                    (DragKind::Cell, DragBase::Cells(base)) => {
                        let (Some(anchor), Some(row), Some(column)) = (cell_anchor, row, column)
                        else {
                            return false;
                        };

                        tree.state.downcast_mut::<State>().cell_focus =
                            Some(CellPosition::new(row, column));

                        let mut selection =
                            selection::rectangle(anchor, CellPosition::new(row, column));
                        selection.extend(base.iter().copied());

                        if selection != self.selected_cells {
                            if let Some(on_cell) = &self.on_select_cell {
                                shell.publish(on_cell(selection));
                            }
                            shell.request_redraw();
                        }
                    }

                    (DragKind::Row, DragBase::Linear(base)) => {
                        let (Some(anchor), Some(row)) = (row_anchor, row) else {
                            return false;
                        };

                        tree.state.downcast_mut::<State>().row_focus = Some(row);

                        let mut selection = selection::span(anchor, row);
                        selection.extend(base.iter().copied());

                        if selection != self.selected {
                            if let Some(on_rows) = &self.on_select {
                                shell.publish(on_rows(selection));
                            }
                            shell.request_redraw();
                        }
                    }

                    (DragKind::Column, DragBase::Linear(base)) => {
                        let (Some(anchor), Some(column)) = (column_anchor, column) else {
                            return false;
                        };

                        tree.state.downcast_mut::<State>().column_focus = Some(column);

                        let mut selection = selection::span(anchor, column);
                        selection.extend(base.iter().copied());

                        if selection != self.selected_columns {
                            if let Some(on_columns) = &self.on_select_column {
                                shell.publish(on_columns(selection));
                            }
                            shell.request_redraw();
                        }
                    }

                    _ => return false,
                }

                shell.capture_event();
                true
            }

            _ => false,
        }
    }

    /// Move the keyboard focus by one step and re-select. Returns whether
    /// anything was moved.
    fn move_focus(
        &self,
        tree: &mut Tree,
        (dy, dx): (isize, isize),
        extend: bool,
        body: Rectangle,
        shell: &mut Shell<'_, Message>,
    ) -> bool {
        let columns = self.columns.len();

        if columns == 0 || self.row_count == 0 {
            return false;
        }

        let step = |current: usize, delta: isize, count: usize| {
            (current as isize + delta).clamp(0, count as isize - 1) as usize
        };

        let state = tree.state.downcast_mut::<State>();

        if self.cell_mode != Mode::None {
            // Step from the focus -- the end that moves -- not the anchor.
            let from = state
                .cell_focus
                .or(state.cell_anchor)
                .or_else(|| self.selected_cells.iter().next_back().copied())
                .unwrap_or(CellPosition::new(0, 0));

            let target = CellPosition::new(
                step(from.row, dy, self.row_count),
                step(from.column, dx, columns),
            );

            // Shift spans from the anchor and leaves it where it is; a plain
            // arrow drags anchor and focus along together.
            let anchor = if extend {
                state.cell_anchor.or(Some(from))
            } else {
                Some(target)
            };

            let selection = if extend {
                selection::rectangle(anchor.unwrap_or(target), target)
            } else {
                BTreeSet::from([target])
            };

            state.cell_anchor = anchor;
            state.cell_focus = Some(target);
            self.reveal_cell(state, body, target);

            if selection != self.selected_cells {
                if let Some(on_cell) = &self.on_select_cell {
                    shell.publish(on_cell(selection));
                }
            }

            self.clear_except(Target::Cell(target), shell);
            return true;
        }

        // No cell selection, so the arrows drive rows. Left/right have nothing
        // to move along and are left for whatever else wants them.
        if self.mode != Mode::None && dy != 0 {
            let from = state
                .row_focus
                .or(state.anchor)
                .or_else(|| self.selected.iter().next_back().copied())
                .unwrap_or(0);

            let target = step(from, dy, self.row_count);

            let anchor = if extend { state.anchor.or(Some(from)) } else { Some(target) };

            let selection = if extend {
                selection::span(anchor.unwrap_or(target), target)
            } else {
                BTreeSet::from([target])
            };

            state.anchor = anchor;
            state.row_focus = Some(target);
            self.reveal_row(state, body, target);

            if selection != self.selected {
                if let Some(on_rows) = &self.on_select {
                    shell.publish(on_rows(selection));
                }
            }

            self.clear_except(Target::Row(target), shell);
            return true;
        }

        false
    }

    /// Clear the two readings of the grid that `target` is not.
    ///
    /// Rows, columns and cells are alternative views of the same data, and
    /// holding two at once leaves nothing on screen to say which one an action
    /// would apply to. The widget cannot mutate sets it does not own, so it
    /// publishes empties instead -- guarded, to avoid a no-op every click.
    fn clear_except(&self, target: Target, shell: &mut Shell<'_, Message>) {
        if !matches!(target, Target::Cell(_)) && !self.selected_cells.is_empty() {
            if let Some(on_cell) = &self.on_select_cell {
                shell.publish(on_cell(BTreeSet::new()));
            }
        }

        if !matches!(target, Target::Row(_)) && !self.selected.is_empty() {
            if let Some(on_rows) = &self.on_select {
                shell.publish(on_rows(BTreeSet::new()));
            }
        }

        if !matches!(target, Target::Column(_)) && !self.selected_columns.is_empty() {
            if let Some(on_columns) = &self.on_select_column {
                shell.publish(on_columns(BTreeSet::new()));
            }
        }
    }

    /// Scroll the least amount that brings a row fully into view.
    fn reveal_row(&self, state: &mut State, body: Rectangle, row: usize) {
        let top = state.row_height * row as f32;
        let bottom = top + state.row_height;

        if top < state.offset.y {
            state.offset.y = top;
        } else if bottom > state.offset.y + body.height {
            state.offset.y = bottom - body.height;
        }

        state.offset = scroll::clamp_offset(
            state.offset,
            Size::new(body.width, body.height),
            state.content,
        );
    }

    /// As [`reveal_row`](Self::reveal_row), plus the horizontal axis.
    ///
    /// Without this, arrowing past the edge of the viewport moves the selection
    /// somewhere you cannot see, which reads as the keys having done nothing.
    fn reveal_cell(&self, state: &mut State, body: Rectangle, cell: CellPosition) {
        self.reveal_row(state, body, cell.row);

        // A frozen column is on screen by definition. Scrolling to "reveal" one
        // would haul the view back to the left edge for no reason.
        if cell.column < self.sticky_columns || cell.column >= state.widths.len() {
            return;
        }

        let left = state.offsets[cell.column];
        let right = left + state.widths[cell.column];

        // The scrolling band begins after the frozen strip, so the near edge of
        // the visible window is pushed in by it while the far edge is not.
        if left < state.offset.x + state.frozen_width {
            state.offset.x = left - state.frozen_width;
        } else if right > state.offset.x + body.width {
            state.offset.x = right - body.width;
        }

        state.offset = scroll::clamp_offset(
            state.offset,
            Size::new(body.width, body.height),
            state.content,
        );
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

    /// Which leaf column contains this *screen* point.
    ///
    /// `None` in the gutters and in the inter-column spacing, which is the
    /// answer callers want: those pixels deliberately belong to no column.
    fn column_at(&self, point: Point, bounds: Rectangle, state: &State) -> Option<usize> {
        let x = content_x(point.x, bounds, state);
        let last = state.widths.len().checked_sub(1)?;

        if x < state.offsets[0] || x >= state.offsets[last] + state.widths[last] {
            return None;
        }

        // Inside the columns, the spacing between two of them belongs to the
        // one on its left. Testing each column's own width instead leaves a
        // `spacing`-wide dead strip between every pair that reports "gutter",
        // which would silently fall through to row selection.
        (0..state.widths.len())
            .rev()
            .find(|&i| x >= state.offsets[i])
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
                    x: screen_x(zone.x - bounds.x, bounds, state),
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

/// The region below the header, split into the part content may use and the
/// full strip the scrollbars are placed in.
///
/// They differ by the horizontal bar's thickness. The bar is an overlay drawn
/// along the bottom of the full strip, so content allowed to reach that far
/// ends up *underneath* it -- and because the bar also caps how far the content
/// can scroll, the last row could never be brought clear of it. Reserving the
/// strip up front costs one row's worth of viewport and makes the bottom row
/// reachable.
fn body_regions(
    bounds: Rectangle,
    header_height: f32,
    content_width: f32,
    scrollbar_width: f32,
    horizontal: Policy,
) -> (Rectangle, Rectangle) {
    let bars = Rectangle {
        x: bounds.x,
        y: bounds.y + header_height,
        width: bounds.width,
        height: (bounds.height - header_height).max(0.0),
    };

    let reserved = if horizontal == Policy::Auto && content_width > bars.width {
        scrollbar_width
    } else {
        0.0
    };

    (
        Rectangle {
            height: (bars.height - reserved).max(0.0),
            ..bars
        },
        bars,
    )
}

/// The rounded outline every layer has to stay inside.
///
/// `iced` clips a layer to a **rectangle**, so a radius on `Style::border`
/// rounds the frame *line* and nothing under it: the header band, the stripes
/// and the rules keep their square corners and go on showing outside the arc.
/// There is no rounded clip to reach for, so each quad is fitted to the shape
/// on its way to the renderer instead.
///
/// Two ways to fit, because a quad can only carry a radius on its **own**
/// corners:
///
/// - A fill that reaches a corner of the frame takes that corner's radius.
/// - A rule is thinner than the arc, so a radius on it would round away to
///   nothing. It is shortened along its length instead, back to wherever the
///   arc has got to by the time it reaches it.
///
/// What this cannot reach is the cells' own contents -- those are foreign
/// widgets drawing themselves, and a `Background` a caller sets on one is its
/// business, not the table's.
#[derive(Debug, Clone, Copy)]
struct Frame {
    bounds: Rectangle,
    radius: Radius,
}

/// Corners in `Radius` order, as (is_left, is_top).
const CORNERS: [(bool, bool); 4] = [(true, true), (false, true), (false, false), (true, false)];

/// A quad edge this close to the frame's is treated as being on it. Fills are
/// built from the same numbers the frame is, so the slack only has to absorb
/// arithmetic, not layout.
const ON_EDGE: f32 = 0.5;

impl Frame {
    fn new(bounds: Rectangle, border: Border) -> Self {
        Self {
            bounds,
            radius: border.radius,
        }
    }

    /// The same frame seen from inside a layer drawn under `translation`. Body
    /// quads are built in content space; the frame is in screen space, and
    /// comparing the two directly is how the corner ends up in the wrong place
    /// the moment anything is scrolled.
    fn translated(self, translation: Vector) -> Self {
        Self {
            bounds: self.bounds - translation,
            ..self
        }
    }

    /// Fit a quad to the shape. `None` if nothing of it survives, which saves
    /// the renderer a primitive it would only clip away.
    fn fit(&self, quad: Rectangle) -> Option<renderer::Quad> {
        let mut bounds = quad.intersection(&self.bounds)?;

        let arcs = [
            self.radius.top_left,
            self.radius.top_right,
            self.radius.bottom_right,
            self.radius.bottom_left,
        ];

        // Which way a rule runs. Shortening its long axis keeps it a rule;
        // shortening the short one would rub it out.
        let flat = bounds.width >= bounds.height;

        let mut rounded = [0.0_f32; 4];
        let mut trim = [0.0_f32; 4]; // left, top, right, bottom

        for (index, (at_left, at_top)) in CORNERS.into_iter().enumerate() {
            let arc = arcs[index];

            if arc <= 0.0 {
                continue;
            }

            // How far the quad already keeps clear of this corner on each axis.
            // Never negative: `bounds` is inside the frame by construction.
            let dx = if at_left {
                bounds.x - self.bounds.x
            } else {
                (self.bounds.x + self.bounds.width) - (bounds.x + bounds.width)
            };
            let dy = if at_top {
                bounds.y - self.bounds.y
            } else {
                (self.bounds.y + self.bounds.height) - (bounds.y + bounds.height)
            };

            // Clear of the arc on either axis means clear of it entirely.
            if dx >= arc || dy >= arc {
                continue;
            }

            if bounds.width >= arc && bounds.height >= arc {
                // Big enough to carry the arc -- but only if it is actually
                // anchored on both edges. A fill that starts inside the corner
                // has no edge to round against, and rounding its own corner
                // would just punch a notch out of the middle of the band.
                if dx <= ON_EDGE && dy <= ON_EDGE {
                    // Halved dimensions are the renderer's own clamp; applying
                    // it here keeps `rounded` honest about what gets drawn.
                    rounded[index] = arc.min(bounds.width / 2.0).min(bounds.height / 2.0);
                }
            } else if flat {
                let side = if at_left { 0 } else { 2 };
                trim[side] = trim[side].max(arc_inset(arc, dy) - dx);
            } else {
                let side = if at_top { 1 } else { 3 };
                trim[side] = trim[side].max(arc_inset(arc, dx) - dy);
            }
        }

        bounds.x += trim[0];
        bounds.y += trim[1];
        bounds.width -= trim[0] + trim[2];
        bounds.height -= trim[1] + trim[3];

        (bounds.width > 0.0 && bounds.height > 0.0).then(|| renderer::Quad {
            bounds,
            border: Border {
                radius: Radius {
                    top_left: rounded[0],
                    top_right: rounded[1],
                    bottom_right: rounded[2],
                    bottom_left: rounded[3],
                },
                ..Border::default()
            },
            ..Default::default()
        })
    }

    fn fill<Renderer: renderer::Renderer>(
        &self,
        renderer: &mut Renderer,
        bounds: Rectangle,
        background: impl Into<Background>,
    ) {
        if let Some(quad) = self.fit(bounds) {
            renderer.fill_quad(quad, background);
        }
    }

    /// As [`fill`](Self::fill), for a quad that carries a border of its own.
    /// The two radii are merged per corner rather than replaced: a scrollbar
    /// thumb keeps its own rounding everywhere the frame does not impose more.
    fn fill_bordered<Renderer: renderer::Renderer>(
        &self,
        renderer: &mut Renderer,
        bounds: Rectangle,
        border: Border,
        background: impl Into<Background>,
    ) {
        if let Some(quad) = self.fit(bounds) {
            let fitted = quad.border.radius;

            renderer.fill_quad(
                renderer::Quad {
                    border: Border {
                        radius: Radius {
                            top_left: fitted.top_left.max(border.radius.top_left),
                            top_right: fitted.top_right.max(border.radius.top_right),
                            bottom_right: fitted.bottom_right.max(border.radius.bottom_right),
                            bottom_left: fitted.bottom_left.max(border.radius.bottom_left),
                        },
                        ..border
                    },
                    ..quad
                },
                background,
            );
        }
    }
}

/// How far in from one edge a corner's arc has come, `depth` along the other.
///
/// Zero once past the corner, `radius` at the corner itself -- which is what
/// makes it usable as "how much to shorten this rule by so it stops at the
/// outline rather than at the square the outline was cut from".
fn arc_inset(radius: f32, depth: f32) -> f32 {
    if depth >= radius {
        0.0
    } else {
        radius - (radius * radius - (radius - depth) * (radius - depth)).sqrt()
    }
}

/// Screen x for a point in content space.
///
/// The frozen strip does not move, so anything inside it maps straight through
/// while everything else is shifted by the scroll offset. Every hit test goes
/// through here rather than subtracting `offset.x` directly -- doing it by hand
/// puts the resize handles and sort controls of the frozen columns wherever the
/// scrolling ones happen to be.
fn screen_x(content: f32, bounds: Rectangle, state: &State) -> f32 {
    if content <= state.frozen_width {
        bounds.x + content
    } else {
        bounds.x + content - state.offset.x
    }
}

/// Content-space x for a point on screen. Inverse of [`screen_x`].
///
/// Cannot land on a frozen column by accident: a point outside the strip is
/// shifted by a non-negative offset, so it always resolves past the strip.
fn content_x(x: f32, bounds: Rectangle, state: &State) -> f32 {
    let local = x - bounds.x;

    if local < state.frozen_width {
        local
    } else {
        local + state.offset.x
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

/// Whether an event has to reach every row, or only the ones on screen.
///
/// Positional events are culled to the viewport: a cell the pointer cannot
/// possibly be over has nothing to do with them, and `CursorMoved` is the one
/// event that arrives continuously, so this is where the cost lives.
///
/// Everything else is still delivered in full, for two reasons that both cause
/// real bugs if ignored:
///
/// - A cell scrolled out of view can still hold **keyboard focus**. Culling
///   key events would type into nothing.
/// - A child that captured a **press** has to see the matching release, or it
///   is left stuck in its pressed state when scrolled back into view. Wheel
///   scrolling with the button held is enough to reach that.
///
/// Releases are rare, so delivering them everywhere costs nothing that is felt.
fn reaches_every_row(event: &Event) -> bool {
    match event {
        Event::Mouse(mouse::Event::ButtonReleased(_))
        | Event::Touch(touch::Event::FingerLifted { .. })
        | Event::Touch(touch::Event::FingerLost { .. }) => true,

        Event::Mouse(_) | Event::Touch(_) => false,

        _ => true,
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
        let edge = screen_x(
            state.offsets[i] + state.widths[i] + spacing / 2.0,
            bounds,
            state,
        );

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
        let left = screen_x(state.offsets[cell.start], bounds, state);
        let right = screen_x(
            state.offsets[cell.end] + state.widths[cell.end] + spacing,
            bounds,
            state,
        );
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
            let width = size.width + h_pad + if self.is_sortable(&cell) { SORT_ZONE + SORT_GAP } else { 0.0 };

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

        // Spread the sample across the whole table rather than taking the first
        // N rows.
        //
        // Sampling the head makes every column's width a function of **row
        // order**. Re-sorting moves different values into the sampled window,
        // the measured intrinsic changes, and the columns visibly resize even
        // though not one cell's content changed -- which is exactly why sorting
        // by First or Last used to shuffle the Contact columns around. A
        // constant stride sees every part of the data whatever order it is in.
        let sample = self
            .measure_sample
            .unwrap_or(self.row_count)
            .min(self.row_count);
        let mut row_height = self.min_row_height;

        for step in 0..sample {
            // `step * row_count / sample` rather than `step * stride`: it
            // reaches the last row instead of stopping a whole stride short of
            // it, so the tail of the table is sampled like everywhere else.
            let row = step * self.row_count / sample;

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

        // Latch the measurement so a column can never *shrink* while it holds
        // the same rows.
        //
        // A stride still only looks at a subset, so a re-sort can still move a
        // long value out of the sampled set. Remembering the widest we have
        // ever seen turns any residual jitter into one-way growth that settles,
        // instead of columns twitching back and forth on every sort.
        //
        // Keyed on the row count so it does not become a permanent floor: a
        // sort reorders rows without changing how many there are, so the latch
        // holds across it, while loading or filtering the data re-measures from
        // scratch.
        {
            let state = tree.state.downcast_mut::<State>();

            if state.measured.len() != columns || state.measured_rows != self.row_count {
                state.measured = intrinsic.clone();
                state.measured_rows = self.row_count;
            } else {
                for (column, width) in intrinsic.iter_mut().enumerate() {
                    state.measured[column] = state.measured[column].max(*width);
                    *width = state.measured[column];
                }
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
            let reserve = if self.is_sortable(&cell) { SORT_ZONE + SORT_GAP } else { 0.0 };
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

        // Where the frozen strip ends, in content space. Includes the leading
        // gutter, since that scrolls with the first column and would otherwise
        // slide out from under it.
        let frozen_width = self
            .sticky_columns
            .checked_sub(1)
            .map(|last| offsets[last] + widths[last])
            .unwrap_or(0.0);

        let state = tree.state.downcast_mut::<State>();
        state.frozen_width = frozen_width;
        state.widths = widths;
        state.offsets = offsets;
        state.header_row_height = header_row_height;
        state.header_height = header_height;
        state.row_height = row_height;
        state.content = Size::new(content_width, body_height);

        // Content may have shrunk since the last frame (a filter was applied,
        // rows were deleted). Re-clamp so we are never scrolled past the end.
        //
        // Minus the horizontal bar's strip, matching `body_regions`: if the
        // clamp thinks the viewport reaches the bottom of the widget, it stops
        // scrolling one bar-height early and the last row stays trapped
        // underneath the bar.
        let bar = if self.horizontal == Policy::Auto && content_width > size.width {
            self.scrollbar_width
        } else {
            0.0
        };

        let viewport = Size::new(size.width, (size.height - header_height - bar).max(0.0));
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

        let (body, bars) = body_regions(
            bounds,
            state.header_height,
            state.content.width,
            self.scrollbar_width,
            self.horizontal,
        );
        let offset = state.offset;
        let columns = self.columns.len();

        // The shape everything below is fitted to. Layer clipping is
        // rectangular, so a radius on the frame is only ever a radius on the
        // frame unless each band and rule is cut to the outline itself.
        let frame = Frame::new(bounds, appearance.border);

        // Base background only. The frame is drawn at the very end of this
        // method instead of here, because everything below paints over it: the
        // row bands run the full content width, the header band runs the full
        // viewport width, and the stripes run both. Drawn first, a 1px border
        // survives only where nothing happens to cover it -- which is exactly
        // the "border appears on the left of every other row" symptom.
        frame.fill(
            renderer,
            bounds,
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
        // Split into horizontal bands: the scrolling columns, then the frozen
        // ones painted over them in their own clip with **no X translation**.
        // That single difference is what makes a column stick, exactly as the
        // header's missing Y translation is what makes it stick -- which is why
        // this was worth structuring in layers before the feature existed.
        //
        // Each band redraws the full-width furniture (stripes, row highlights,
        // dividers) and lets its own clip cut it down. Trying to restrict each
        // piece to its band by arithmetic instead means every one of them has
        // to learn about freezing.
        let frozen = state.frozen_width.min(body.width);
        let bands: [(Rectangle, f32, std::ops::Range<usize>); 2] = [
            (
                Rectangle {
                    x: body.x + frozen,
                    width: (body.width - frozen).max(0.0),
                    ..body
                },
                -offset.x,
                self.sticky_columns..columns,
            ),
            (
                Rectangle {
                    width: frozen,
                    ..body
                },
                0.0,
                0..self.sticky_columns,
            ),
        ];

        let paint_body = |renderer: &mut Renderer,
                              region: Rectangle,
                              shift_x: f32,
                              range: std::ops::Range<usize>| {
        // Per band, never shared. Children are laid out in content space, so
        // the viewport they are culled and clipped against has to be *this
        // band's* slice of content space -- and the frozen band's slice is not
        // shifted by the scroll offset.
        //
        // Handing both bands the scrolling viewport clips the frozen columns'
        // contents against a rectangle they are not inside, so their text
        // vanishes a character at a time as you scroll sideways while their
        // backgrounds, rules and hit boxes all stay put and keep working.
        let body_cursor = local_cursor(cursor, region, Vector::new(-shift_x, offset.y));
        let body_viewport = Rectangle {
            x: region.x - shift_x,
            y: region.y + offset.y,
            width: region.width,
            height: region.height,
        };

        // Everything in this layer is built in content space, so the outline it
        // has to stay inside has to be moved into content space too.
        let frame = frame.translated(Vector::new(shift_x, -offset.y));

        renderer.with_layer(region, |renderer| {
            renderer.with_translation(Vector::new(shift_x, -offset.y), |renderer| {
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
                        frame.fill(renderer, row_rect(row), background);
                    }
                }

                // Column bands sit above the stripes but below the row
                // highlights, so a selected row still reads as selected where
                // the two cross.
                if let Some(background) = appearance.selected_column_background {
                    for column in &self.selected_columns {
                        if *column < state.widths.len() {
                            frame.fill(
                                renderer,
                                Rectangle {
                                    x: bounds.x + state.offsets[*column],
                                    y: body.y + offset.y,
                                    width: state.widths[*column],
                                    height: body.height,
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
                        for column in range.clone() {
                            if let Some(background) = style_of(row, column).background {
                                frame.fill(renderer, cell_rect(row, column), background);
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
                        frame.fill(renderer, row_rect(row), background);
                    }
                }

                if let Some(background) = appearance.selected_row_background {
                    // Only the visible slice -- `selected` may hold thousands
                    // of rows after a Shift-range over a large table.
                    for row in self.selected.range(first..last) {
                        frame.fill(renderer, row_rect(*row), background);
                    }
                }

                let rule = appearance.divider_width();

                if let Some(color) = appearance.row_divider {
                    for row in first.max(1)..last {
                        frame.fill(
                            renderer,
                            Rectangle {
                                x: bounds.x,
                                y: bounds.y + state.header_height + state.row_height * row as f32,
                                width: state.content.width.max(bounds.width),
                                height: rule,
                            },
                            color,
                        );
                    }
                }

                if let Some(color) = appearance.column_divider {
                    for offset_x in state.offsets.iter().skip(1) {
                        frame.fill(
                            renderer,
                            Rectangle {
                                x: bounds.x + offset_x - self.spacing / 2.0 - rule / 2.0,
                                y: body.y + offset.y,
                                width: rule,
                                height: body.height,
                            },
                            color,
                        );
                    }
                }

                if let (Some(color), Some((left, right))) =
                    (appearance.gutter_divider, gutter_edges)
                {
                    for x in [left, right] {
                        frame.fill(
                            renderer,
                            Rectangle {
                                x: x - rule / 2.0,
                                y: body.y + offset.y,
                                width: rule,
                                height: body.height,
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
                                frame.fill(
                                    renderer,
                                    Rectangle {
                                        y,
                                        height: rule,
                                        ..band
                                    },
                                    color,
                                );
                            }
                        }
                    }
                }

                // The selected cell, last of the highlights and above the
                // dividers: it is the most specific thing the grid can point
                // at, so nothing else should be able to obscure it.
                //
                // Only the visible rows: the set can hold every cell in the
                // table after a sweep, and `CellPosition` sorts by row first,
                // so the visible slice is one range query.
                let visible_cells = self
                    .selected_cells
                    .range(CellPosition::new(first, 0)..CellPosition::new(last, 0))
                    .copied()
                    .filter(|c| c.column < state.widths.len() && range.contains(&c.column));

                for position in visible_cells {
                    let band = cell_rect(position.row, position.column);

                    if let Some(background) = appearance.selected_cell_background {
                        frame.fill(renderer, band, background);
                    }

                    // An edge is drawn only where the neighbour on that side is
                    // *not* selected, so any shape the selection takes comes out
                    // outlined as one block rather than as a grid of boxed
                    // cells with the internal edges doubled.
                    if let Some(color) = appearance.selected_cell_border {
                        let selected = |row: usize, column: usize| {
                            self.selected_cells
                                .contains(&CellPosition::new(row, column))
                        };

                        let edges = [
                            (
                                Rectangle { height: rule, ..band },
                                position.row > 0 && selected(position.row - 1, position.column),
                            ),
                            (
                                Rectangle {
                                    y: band.y + band.height - rule,
                                    height: rule,
                                    ..band
                                },
                                selected(position.row + 1, position.column),
                            ),
                            (
                                Rectangle { width: rule, ..band },
                                position.column > 0
                                    && selected(position.row, position.column - 1),
                            ),
                            (
                                Rectangle {
                                    x: band.x + band.width - rule,
                                    width: rule,
                                    ..band
                                },
                                selected(position.row, position.column + 1),
                            ),
                        ];

                        for (edge, joined) in edges {
                            if !joined {
                                frame.fill(renderer, edge, color);
                            }
                        }
                    }
                }

                // Only visible rows are drawn. This is arithmetic on the row
                // range rather than a rectangle test per child, which is what
                // makes large tables cheap to redraw.
                for row in first..last {
                    for column in range.clone() {
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
        };

        for (region, shift_x, range) in bands {
            if !range.is_empty() && region.width > 0.0 {
                paint_body(renderer, region, shift_x, range);
            }
        }

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
        let header_style = renderer::Style {
            text_color: appearance.header_text.unwrap_or(style.text_color),
        };
        let group_style = renderer::Style {
            text_color: appearance
                .group_text
                .or(appearance.header_text)
                .unwrap_or(style.text_color),
        };

        // Same two bands as the body, and for the same reason. A header cell is
        // wholly inside the frozen run or wholly outside it -- never split --
        // because the run is a whole number of top-level nodes.
        let header_bands: [(Rectangle, f32, std::ops::Range<usize>); 2] = [
            (
                Rectangle {
                    x: header_region.x + frozen,
                    width: (header_region.width - frozen).max(0.0),
                    ..header_region
                },
                -offset.x,
                self.sticky_columns..columns,
            ),
            (
                Rectangle {
                    width: frozen,
                    ..header_region
                },
                0.0,
                0..self.sticky_columns,
            ),
        ];

        let paint_header = |renderer: &mut Renderer,
                                region: Rectangle,
                                shift_x: f32,
                                range: std::ops::Range<usize>| {
        // Per band, for the same reason the body's are.
        let header_cursor = local_cursor(cursor, region, Vector::new(-shift_x, 0.0));
        let header_viewport = Rectangle {
            x: region.x - shift_x,
            ..region
        };

        renderer.with_layer(region, |renderer| {
            // One band behind the whole header, outside the translation so it
            // covers the viewport regardless of horizontal scroll. Group levels
            // are then painted per span on top, rather than as a full-width
            // stripe -- an ungrouped column has no group, and banding across it
            // draws a group level over a column that is not in one.
            if let Some(background) = appearance.header_background {
                frame.fill(renderer, region, background);
            }

            renderer.with_translation(Vector::new(shift_x, 0.0), |renderer| {
                // As in the body: the furniture below is placed in content
                // space, so the outline it is cut against has to be too. Scoped
                // to this closure -- the header divider after it is not
                // translated, and cutting it against a shifted outline would
                // pull its ends in by the scroll offset.
                let frame = frame.translated(Vector::new(shift_x, 0.0));

                let rule = appearance.divider_width();
                let band = state.header_row_height;
                let foot = bounds.y + state.header_height;

                let left = |cell: &HeaderCell| bounds.x + state.offsets[cell.start];
                let right =
                    |cell: &HeaderCell| bounds.x + state.offsets[cell.end] + state.widths[cell.end];

                if let Some(background) = appearance.group_background {
                    for cell in self.header_cells.iter().filter(|c| !c.is_leaf()) {
                        frame.fill(
                            renderer,
                            Rectangle {
                                x: left(cell),
                                y: bounds.y + band * cell.top as f32,
                                width: (right(cell) - left(cell)).max(0.0),
                                height: band * (cell.bottom() - cell.top) as f32,
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
                        frame.fill(
                            renderer,
                            Rectangle {
                                x: x - rule / 2.0,
                                y: top,
                                width: rule,
                                height,
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
                        frame.fill(
                            renderer,
                            Rectangle {
                                x: left(cell),
                                y: bounds.y + band * cell.bottom() as f32 - rule,
                                width: (right(cell) - left(cell)).max(0.0),
                                height: rule,
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
                        frame.fill(
                            renderer,
                            Rectangle {
                                x: x - rule / 2.0,
                                y: bounds.y,
                                width: rule,
                                height: state.header_height,
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
                                frame.fill_bordered(
                                    renderer,
                                    Rectangle {
                                        y: zone.center_y() - 9.0,
                                        height: 18.0,
                                        ..zone
                                    },
                                    Border {
                                        radius: 3.0.into(),
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

                // The furniture above is left to the clip, the same as in the
                // body. The *labels* are filtered instead: they are real
                // widgets, and drawing each one twice only to throw one copy
                // away is work, not just overdraw.
                for cell in self
                    .header_cells
                    .iter()
                    .filter(|cell| cell.start >= range.start && cell.end < range.end)
                {
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
                frame.fill(
                    renderer,
                    Rectangle {
                        y: bounds.y + state.header_height - appearance.divider_width(),
                        height: appearance.divider_width(),
                        ..region
                    },
                    color,
                );
            }
        });
        };

        for (region, shift_x, range) in header_bands {
            if !range.is_empty() && region.width > 0.0 {
                paint_header(renderer, region, shift_x, range);
            }
        }

        // The seam between frozen and scrolling columns, running the full
        // height over both. Without it the frozen columns look like they are
        // simply refusing to scroll rather than like a pinned region.
        //
        // In a layer of its own, and this is not optional. Layers render in
        // creation order, and drawing into the *base* layer after a sub-layer
        // has closed still files the primitive in the base layer -- which
        // renders underneath. Filled directly here, the seam was painted over
        // by every band that came before it and survived only on the rows the
        // stripes happened to leave alone.
        if self.sticky_columns > 0 && frozen > 0.0 {
            if let Some(color) = appearance.frozen_divider {
                let rule = appearance.divider_width();

                renderer.with_layer(bounds, |renderer| {
                    frame.fill(
                        renderer,
                        Rectangle {
                            x: bounds.x + frozen - rule,
                            y: bounds.y,
                            width: rule,
                            height: bounds.height,
                        },
                        color,
                    );
                });
            }
        }

        // ------------------------------------------------------------------
        // Scrollbars -- untranslated, and in a layer of their own.
        //
        // Layers render in creation order, and drawing into the base layer
        // after a sub-layer closes still puts the primitives in the base
        // layer, which renders *underneath*. Opening a fresh layer here is
        // what actually puts the scrollbars on top of the stripes and rules.
        // ------------------------------------------------------------------
        let (vertical, horizontal) = scroll::scrollbars(
            bars,
            state.content,
            offset,
            self.scrollbar_width,
            self.vertical,
            self.horizontal,
        );

        renderer.with_layer(bounds, |renderer| {
            for bar in [vertical, horizontal].into_iter().flatten() {
                if let Some(track) = appearance.scrollbar_track {
                    frame.fill(renderer, bar.track, track);
                }

                let hovered = cursor
                    .position()
                    .is_some_and(|point| bar.track.contains(point));

                frame.fill_bordered(
                    renderer,
                    bar.thumb,
                    appearance.scrollbar_border,
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

        // The reorder overlay: a chip carrying the column's own header under
        // the pointer, plus a line showing where it would land.
        //
        // The chip matters more than it looks. Without it the only feedback is
        // a line somewhere else on screen, and the gesture reads as "poking at
        // the header" rather than as picking the column up and putting it
        // down -- which is the thing that makes the drag legible at all.
        if let Some(drag) = state.reorder.as_ref().filter(|drag| drag.target.is_some()) {
            let carried = self
                .header_cells
                .iter()
                .find(|cell| cell.is_leaf() && cell.start == drag.column);

            renderer.with_layer(bounds, |renderer| {
                if let (Some(slot), Some(color)) = (drag.target, appearance.reorder_indicator) {
                    if let Some(x) = self.slot_x(slot, bounds, state) {
                        let width = (appearance.divider_width() * 2.0).max(2.0);

                        frame.fill(
                            renderer,
                            Rectangle {
                                x: x - width / 2.0,
                                // The leaf band only. A full-height rule
                                // competes with the frozen seam and the
                                // column dividers it is drawn over; kept to
                                // the row the columns are named on, it
                                // reads as an insertion point.
                                y: bounds.y
                                    + state.header_row_height
                                        * self.header_rows.saturating_sub(1) as f32,
                                width,
                                height: state.header_row_height,
                            },
                            color,
                        );
                    }
                }

                let Some((cell, point)) = carried.zip(cursor.position()) else {
                    return;
                };

                let chip = Rectangle {
                    x: point.x - drag.grab,
                    y: point.y - state.header_row_height / 2.0,
                    width: state.widths[drag.column],
                    height: state.header_row_height,
                };

                if let Some(background) = appearance.reorder_carry_background {
                    frame.fill_bordered(
                        renderer,
                        chip,
                        Border {
                            color: appearance
                                .reorder_indicator
                                .unwrap_or(Color::TRANSPARENT),
                            width: appearance.divider_width(),
                            radius: 3.0.into(),
                        },
                        background,
                    );
                }

                // The label is the real header widget, drawn a second time at an
                // offset. Its layout node stays where it is -- translating the
                // renderer is the same trick the whole widget uses to scroll.
                //
                // The shift moves the column's whole *box*, not the element, so
                // whatever alignment the label has inside its cell survives the
                // trip. Anchoring on the element's own bounds instead would
                // flush a right-aligned header against the left of the chip.
                let band = Rectangle {
                    x: bounds.x + state.offsets[drag.column],
                    y: bounds.y + state.header_row_height * cell.row as f32,
                    width: chip.width,
                    height: chip.height,
                };

                renderer.with_layer(chip, |renderer| {
                    renderer.with_translation(
                        Vector::new(chip.x - band.x, chip.y - band.y),
                        |renderer| {
                            self.elements[cell.element].as_widget().draw(
                                &tree.children[cell.element],
                                renderer,
                                theme,
                                &header_style,
                                layout.child(cell.element),
                                mouse::Cursor::Unavailable,
                                // Untranslated: children cull against their own
                                // layout coordinates, which the translation has
                                // not touched.
                                &band,
                            );
                        },
                    );
                });
            });
        }

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

        // Click-to-focus. Arrow keys are delivered to every widget, so without
        // a focus flag the table would fight everything else on the page for
        // them. Tracked on press rather than on release so a click that starts
        // a drag has already taken focus by the time the drag runs.
        if let Event::Mouse(mouse::Event::ButtonPressed(_)) = event {
            let focused = cursor.is_over(bounds);
            let state = tree.state.downcast_mut::<State>();

            if state.focused != focused {
                state.focused = focused;
            }
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

        let (body, bars) = body_regions(
            bounds,
            header_height,
            content.width,
            self.scrollbar_width,
            self.horizontal,
        );
        let viewport_size = Size::new(body.width, body.height);
        let (vertical, horizontal) = scroll::scrollbars(
            bars,
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

        // --- 1b. column reorder, once one is armed ---
        //
        // Ahead of everything below it: while a column is being dragged the
        // pointer is still inside the header, and every other handler there
        // would happily claim the same movement.
        if self.drag_reorder(tree, event, bounds, cursor, shell) {
            return;
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

        // Header cells always take the event. Body cells take positional ones
        // only while they are on screen -- the same range `draw` has always
        // used, which is what keeps the per-event cost tied to the viewport
        // instead of the row count.
        let body_elements = if reaches_every_row(event) {
            self.header_len..self.elements.len()
        } else {
            self.visible_body_range(offset.y, body.height, row_height)
        };

        for index in (0..self.header_len).chain(body_elements) {
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

        // Ahead of row selection: a right-click opens a menu, it does not move
        // the selection out from under the menu that is about to appear.
        if let (Some(on_right_click), Event::Mouse(mouse::Event::ButtonPressed(
            mouse::Button::Right,
        ))) = (&self.on_right_click, event)
        {
            if let Some(point) = cursor.position_over(bounds) {
                let state = tree.state.downcast_ref::<State>();
                let row = scroll::row_at(point, body, offset.y, row_height, self.row_count);
                let column = self.column_at(point, bounds, state);

                // Move the selection under the pointer first, so a menu acts on
                // what was actually right-clicked.
                //
                // A cell wins wherever cell selection is on and the pointer is
                // inside a column; a gutter click has no column and falls
                // through to the row; a header click has no row and resolves to
                // the column. Exactly the precedence a left-click follows.
                let target = match (row, column) {
                    (Some(row), Some(column)) if self.cell_mode != Mode::None => {
                        Some(Target::Cell(CellPosition::new(row, column)))
                    }
                    (Some(row), _) if self.mode != Mode::None => Some(Target::Row(row)),
                    (None, Some(column)) if self.column_mode != Mode::None => {
                        Some(Target::Column(column))
                    }
                    _ => None,
                };

                if let Some(target) = target {
                    // Re-point the selection, but only when the target is
                    // *outside* the current one: right-clicking one of several
                    // selected rows has to keep all of them, or "delete
                    // selected" silently narrows to one the moment the menu
                    // opens on it.
                    match target {
                        Target::Cell(position) if !self.selected_cells.contains(&position) => {
                            if let Some(on_cell) = &self.on_select_cell {
                                shell.publish(on_cell(BTreeSet::from([position])));
                            }
                        }
                        Target::Row(row) if !self.selected.contains(&row) => {
                            if let Some(on_rows) = &self.on_select {
                                shell.publish(on_rows(BTreeSet::from([row])));
                            }
                        }
                        Target::Column(column) if !self.selected_columns.contains(&column) => {
                            if let Some(on_columns) = &self.on_select_column {
                                shell.publish(on_columns(BTreeSet::from([column])));
                            }
                        }
                        _ => {}
                    }

                    self.clear_except(target, shell);
                }

                shell.publish(on_right_click(Click {
                    row,
                    column,
                    position: point,
                }));
                shell.capture_event();
                return;
            }
        }

        // --- 4a. cell selection, and the sweep that any of the three kinds
        // can be dragging ---
        //
        // Cells go ahead of rows, and only claim clicks that land *inside* a
        // column. A click in a gutter finds no column and falls through --
        // which is the whole point of the gutter, and the only reason row
        // selection still has somewhere to live once every cell is
        // individually selectable.
        if let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event {
            if self.cell_mode != Mode::None {
                if let Some(point) = cursor.position_over(body) {
                    let hit = {
                        let state = tree.state.downcast_ref::<State>();

                        scroll::row_at(point, body, offset.y, row_height, self.row_count)
                            .zip(self.column_at(point, bounds, state))
                    };

                    if let Some((row, column)) = hit {
                        let target = CellPosition::new(row, column);
                        let state = tree.state.downcast_mut::<State>();
                        let modifiers = state.modifiers;

                        let outcome = selection::apply_cells(
                            self.cell_mode,
                            &self.selected_cells,
                            state.cell_anchor,
                            target,
                            modifiers.command(),
                            modifiers.shift(),
                        );

                        // The anchor has to move even when the selection did
                        // not change, or a plain click on the one already
                        // selected cell leaves the next Shift-click measuring
                        // from wherever the anchor last happened to be.
                        let selection = outcome
                            .as_ref()
                            .map(|outcome| outcome.selection.clone())
                            .unwrap_or_else(|| self.selected_cells.clone());

                        state.cell_anchor = outcome
                            .as_ref()
                            .and_then(|outcome| outcome.anchor)
                            .or(Some(target));

                        // The click *is* the new moving end, so a Shift+arrow
                        // straight afterwards carries on from where the pointer
                        // left off rather than from the anchor.
                        state.cell_focus = Some(target);

                        state.drag = Some(Drag {
                            kind: DragKind::Cell,
                            base: DragBase::Cells(if modifiers.command() {
                                selection.clone()
                            } else {
                                BTreeSet::new()
                            }),
                        });

                        if let Some(outcome) = outcome {
                            if let Some(on_cell) = &self.on_select_cell {
                                shell.publish(on_cell(outcome.selection));
                            }
                        }

                        self.clear_except(Target::Cell(target), shell);
                        shell.capture_event();
                        shell.request_redraw();
                        return;
                    }
                }
            }
        }

        if self.sweep(tree, event, bounds, body, cursor, row_height, offset, shell) {
            return;
        }

        // --- 4b. keyboard navigation ---
        if let Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event {
            if tree.state.downcast_ref::<State>().focused {
                // `command()` is Cmd on macOS and Ctrl everywhere else, so the
                // platform difference is already handled.
                let copying = modifiers.command()
                    && matches!(key, keyboard::Key::Character(c) if c.eq_ignore_ascii_case("c"));

                if copying {
                    let empty = self.selected_cells.is_empty()
                        && self.selected.is_empty()
                        && self.selected_columns.is_empty();

                    if let (false, Some(on_copy)) = (empty, &self.on_copy) {
                        shell.publish(on_copy());
                        shell.capture_event();
                        return;
                    }
                }

                let delta = match key {
                    keyboard::Key::Named(keyboard::key::Named::ArrowUp) => Some((-1, 0)),
                    keyboard::Key::Named(keyboard::key::Named::ArrowDown) => Some((1, 0)),
                    keyboard::Key::Named(keyboard::key::Named::ArrowLeft) => Some((0, -1)),
                    keyboard::Key::Named(keyboard::key::Named::ArrowRight) => Some((0, 1)),
                    _ => None,
                };

                if let Some(delta) = delta {
                    if self.move_focus(tree, delta, modifiers.shift(), body, shell) {
                        shell.capture_event();
                        shell.request_redraw();
                        return;
                    }
                }
            }
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
                            state.anchor = outcome.anchor.or(Some(row));
                            state.row_focus = Some(row);
                            state.drag = Some(Drag {
                                kind: DragKind::Row,
                                base: DragBase::Linear(if modifiers.command() {
                                    outcome.selection.clone()
                                } else {
                                    BTreeSet::new()
                                }),
                            });

                            if let Some(on_select) = &self.on_select {
                                shell.publish(on_select(outcome.selection));
                            }

                            self.clear_except(Target::Row(row), shell);
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

                    // A plain press on a leaf header arms a reorder. It only
                    // *becomes* one past the threshold, so the click still
                    // selects the column either way.
                    //
                    // With a modifier held the gesture stays a selection sweep:
                    // Ctrl and Shift mean "extend the selection" everywhere
                    // else in this widget, and reordering on them would make
                    // the header the one place they mean something different.
                    let reordering = self.on_reorder.is_some()
                        && cell.is_leaf()
                        && !toggle
                        && !modifiers.shift();

                    let slots = if reordering {
                        header::drop_slots(&self.header_cells, cell.start, self.sticky_columns)
                    } else {
                        Vec::new()
                    };

                    // More than one slot means at least one of them is a real
                    // move; a lone slot is the column's own position.
                    if slots.len() > 1 {
                        let pointer = cursor.position().map(|point| point.x).unwrap_or_default();
                        let left = screen_x(state.offsets[cell.start], bounds, state);

                        state.reorder = Some(ReorderDrag {
                            column: cell.start,
                            origin: pointer,
                            grab: pointer - left,
                            slots,
                            target: None,
                        });
                    }

                    if let Some(outcome) = outcome {
                        state.column_anchor = outcome.anchor.or(Some(cell.start));
                        state.column_focus = Some(cell.end);

                        // Only one drag at a time. A column sweep and a reorder
                        // are the same gesture on the same pixels, and running
                        // both means the selection smears out behind the column
                        // you are dragging.
                        state.drag = if state.reorder.is_some() {
                            None
                        } else {
                            Some(Drag {
                                kind: DragKind::Column,
                                base: DragBase::Linear(if toggle {
                                    outcome.selection.clone()
                                } else {
                                    BTreeSet::new()
                                }),
                            })
                        };

                        if let Some(on_select) = &self.on_select_column {
                            shell.publish(on_select(outcome.selection));
                        }

                        self.clear_except(Target::Column(cell.start), shell);
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
        let (body, bars) = body_regions(
            bounds,
            state.header_height,
            state.content.width,
            self.scrollbar_width,
            self.horizontal,
        );

        let (vertical, horizontal) = scroll::scrollbars(
            bars,
            state.content,
            state.offset,
            self.scrollbar_width,
            self.vertical,
            self.horizontal,
        );

        if state.y_grab.is_some()
            || state.x_grab.is_some()
            || state
                .reorder
                .as_ref()
                .is_some_and(|drag| drag.target.is_some())
        {
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

        // Purely positional, so it is always culled: a row the cursor cannot
        // be over cannot be the one setting the interaction.
        let body_elements =
            self.visible_body_range(state.offset.y, body.height, state.row_height);

        (0..self.header_len)
            .chain(body_elements)
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
        let (offset, row_height, header_height) = {
            let state = tree.state.downcast_ref::<State>();
            (state.offset, state.row_height, state.header_height)
        };

        // Overlays (menus, tooltips) opened from a scrolled cell have to be
        // pushed by the same amount the cell was visually shifted, or they
        // appear at the unscrolled position.
        let translation = translation - offset;

        // `overlay::from_children` would ask every cell in the table, and
        // `UserInterface::update` calls this at the top of *every* event pass --
        // which made it the single most expensive thing a mouse move did on a
        // large table, dwarfing the event forwarding itself.
        //
        // So it is culled like the rest. An overlay owned by a row that has
        // been scrolled out of view stops being reported: its anchor is off
        // screen, so it had nowhere correct to draw anyway. The cell keeps its
        // own state, and the overlay comes back when the row does.
        let body_height = (layout.bounds().height - header_height).max(0.0);
        let visible = self.visible_body_range(offset.y, body_height, row_height);

        let header_len = self.header_len;
        let (header_elements, body_elements) = self.elements.split_at_mut(header_len);
        let (header_trees, body_trees) = tree.children.split_at_mut(header_len);

        let body = visible.start - header_len..visible.end - header_len;

        let children: Vec<_> = header_elements
            .iter_mut()
            .zip(header_trees)
            .zip(layout.children())
            .chain(
                body_elements[body.clone()]
                    .iter_mut()
                    .zip(&mut body_trees[body])
                    .zip(layout.children().skip(visible.start)),
            )
            .filter_map(|((child, state), layout)| {
                child
                    .as_widget_mut()
                    .overlay(state, layout, renderer, viewport, translation)
            })
            .collect();

        (!children.is_empty())
            .then(|| iced::advanced::overlay::Group::with_children(children).overlay())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::leaf;

    const COLUMNS: usize = 8;
    const ROWS: usize = 1_000;

    fn table() -> DataTable<'static, (), iced::Theme, ()> {
        let headers = (0..COLUMNS)
            .map(|_| leaf(iced::widget::Space::new()))
            .collect();

        let mut table = DataTable::new(headers);

        for _ in 0..ROWS {
            table.push((0..COLUMNS).map(|_| iced::widget::Space::new().into()));
        }

        table
    }

    /// The row ranges here are the ones `scroll::visible_rows` is tested
    /// against, so this pins the *index* mapping on top of them. An off-by-one
    /// would silently stop delivering events to the last visible row, which is
    /// the kind of thing that only shows up as "the bottom row is dead".
    #[test]
    fn visible_range_covers_exactly_the_rows_on_screen() {
        let table = table();
        let header_len = table.header_len;

        let cell = |row: usize| header_len + row * COLUMNS;

        // At the top, 300px of 30px rows.
        assert_eq!(table.visible_body_range(0.0, 300.0, 30.0), cell(0)..cell(11));

        // Scrolled a little past the tenth row.
        assert_eq!(
            table.visible_body_range(305.0, 300.0, 30.0),
            cell(10)..cell(21)
        );

        // Hard against the bottom: clamped to the last row, never past it.
        let end = table.visible_body_range(29_999.0, 300.0, 30.0);
        assert_eq!(end, cell(999)..cell(1_000));
        assert_eq!(end.end, table.elements.len());
    }

    /// Culling is only ever safe for events that have a position.
    #[test]
    fn only_positional_events_are_culled() {
        let culled = [
            Event::Mouse(mouse::Event::CursorMoved {
                position: Point::ORIGIN,
            }),
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
        ];

        for event in culled {
            assert!(!reaches_every_row(&event), "{event:?} should be culled");
        }

        let delivered = [
            // A cell scrolled out of view can still hold keyboard focus.
            Event::Keyboard(keyboard::Event::ModifiersChanged(
                keyboard::Modifiers::default(),
            )),
            // And a child that captured the press has to see this, or it stays
            // stuck looking pressed.
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
        ];

        for event in delivered {
            assert!(
                reaches_every_row(&event),
                "{event:?} must reach every row"
            );
        }
    }

    const RADIUS: f32 = 8.0;

    fn frame() -> Frame {
        Frame::new(
            Rectangle {
                x: 100.0,
                y: 50.0,
                width: 400.0,
                height: 300.0,
            },
            Border {
                radius: RADIUS.into(),
                ..Border::default()
            },
        )
    }

    /// The symptom that started this: a header band drawn square across the top
    /// of a table with a radius, showing outside the arc at both corners. The
    /// band has to come back carrying the *frame's* radius, and only on the two
    /// corners it actually reaches.
    #[test]
    fn a_band_takes_the_corners_it_reaches() {
        let frame = frame();

        let header = frame
            .fit(Rectangle {
                x: 100.0,
                y: 50.0,
                width: 400.0,
                height: 30.0,
            })
            .expect("the header band survives");

        assert_eq!(header.border.radius.top_left, RADIUS);
        assert_eq!(header.border.radius.top_right, RADIUS);

        // Nowhere near the bottom of the frame, so those corners stay square --
        // rounding them would notch the band where it meets the first row.
        assert_eq!(header.border.radius.bottom_left, 0.0);
        assert_eq!(header.border.radius.bottom_right, 0.0);

        // A row in the middle of the table reaches no corner at all.
        let middle = frame
            .fit(Rectangle {
                x: 100.0,
                y: 200.0,
                width: 400.0,
                height: 30.0,
            })
            .expect("the row survives");

        assert_eq!(middle.border.radius, Radius::default());
        assert_eq!(middle.bounds.width, 400.0);
    }

    /// A row band runs the full *content* width, which is wider than the widget
    /// whenever the table scrolls sideways. Rounding its own far corner would
    /// put the arc off-screen; it has to be cut to the frame first.
    #[test]
    fn an_overhanging_band_is_cut_to_the_frame_before_it_is_rounded() {
        let last_row = frame()
            .fit(Rectangle {
                x: 100.0,
                y: 320.0,
                width: 1_200.0,
                height: 30.0,
            })
            .expect("the row survives");

        assert_eq!(last_row.bounds.width, 400.0);
        assert_eq!(last_row.border.radius.bottom_right, RADIUS);
        assert_eq!(last_row.border.radius.bottom_left, RADIUS);
    }

    /// A rule is thinner than the arc, so a radius on it rounds away to
    /// nothing. It gets shortened to where the outline actually is instead --
    /// which is the other half of the reported symptom, column lines running
    /// on past the corner they should have stopped at.
    #[test]
    fn a_rule_is_shortened_out_of_the_corner_rather_than_rounded() {
        let frame = frame();

        // Hard against the left edge, running the full height: both left
        // corners cut into it, so it loses the whole radius at each end.
        let rule = frame
            .fit(Rectangle {
                x: 100.0,
                y: 50.0,
                width: 1.0,
                height: 300.0,
            })
            .expect("the rule survives");

        assert_eq!(rule.border.radius, Radius::default());
        assert_eq!(rule.bounds.y, 50.0 + RADIUS);
        assert_eq!(rule.bounds.height, 300.0 - RADIUS * 2.0);

        // Further in, the arc has already turned: the cut is smaller, and it is
        // still only ever taken off the ends.
        let inset = frame
            .fit(Rectangle {
                x: 100.0 + RADIUS / 2.0,
                y: 50.0,
                width: 1.0,
                height: 300.0,
            })
            .expect("the rule survives");

        assert!(inset.bounds.y > 50.0 && inset.bounds.y < 50.0 + RADIUS);
        assert_eq!(inset.bounds.x, 100.0 + RADIUS / 2.0);

        // Past the corner entirely -- the overwhelming majority of them --
        // nothing is touched at all.
        let interior = frame
            .fit(Rectangle {
                x: 250.0,
                y: 50.0,
                width: 1.0,
                height: 300.0,
            })
            .expect("the rule survives");

        assert_eq!(interior.bounds.y, 50.0);
        assert_eq!(interior.bounds.height, 300.0);
    }

    /// Body quads are built in content space and the frame is not, so a scrolled
    /// table has to compare the two in the same space. Getting this wrong puts
    /// the corner treatment on whichever row happens to be `radius` px from the
    /// top of the *content*.
    #[test]
    fn the_frame_follows_the_layer_translation() {
        let offset = Vector::new(0.0, 120.0);
        let frame = frame().translated(Vector::new(0.0, -offset.y));

        // The row that will land at the bottom of the widget once translated.
        let row = frame
            .fit(Rectangle {
                x: 100.0,
                y: 50.0 + 300.0 + offset.y - 30.0,
                width: 400.0,
                height: 30.0,
            })
            .expect("the row survives");

        assert_eq!(row.border.radius.bottom_left, RADIUS);

        // And one two screens further down is outside the frame entirely, so it
        // never reaches the renderer.
        assert!(frame
            .fit(Rectangle {
                x: 100.0,
                y: 50.0 + 600.0 + offset.y,
                width: 400.0,
                height: 30.0,
            })
            .is_none());
    }

    /// The default style has no radius, and that path has to stay exactly what
    /// it was: no trimming, no rounding, nothing but the intersection.
    #[test]
    fn a_square_frame_changes_nothing() {
        let frame = Frame::new(
            Rectangle {
                x: 100.0,
                y: 50.0,
                width: 400.0,
                height: 300.0,
            },
            Border::default(),
        );

        let quad = Rectangle {
            x: 100.0,
            y: 50.0,
            width: 400.0,
            height: 30.0,
        };

        let fitted = frame.fit(quad).expect("the band survives");

        assert_eq!(fitted.bounds, quad);
        assert_eq!(fitted.border.radius, Radius::default());
    }
}

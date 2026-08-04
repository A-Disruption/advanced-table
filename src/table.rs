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
use iced::{
    alignment, keyboard, mouse, touch, Element, Event, Length, Padding, Point, Rectangle, Size,
    Vector,
};

use crate::header::{self, ColumnSpec, HeaderCell, HeaderNode};
use crate::scroll::{self, Policy};
use crate::sizing::{self, Overflow, Sizing, SpanRequest};
use crate::style::{Catalog, Style};

/// Fallback scroll step when a wheel reports lines and rows are tiny.
const MIN_WHEEL_STEP: f32 = 16.0;

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

    resizable: bool,
    /// Half-width of the grab zone either side of a column edge.
    resize_tolerance: f32,
    /// Floor a column can be dragged to.
    min_column_width: f32,

    width: Length,
    height: Length,
    class: Theme::Class<'a>,
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
            resizable: true,
            resize_tolerance: 4.0,
            min_column_width: 32.0,
            width: Length::Fill,
            height: Length::Fill,
            class: Theme::default(),
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

    fn sizing(&self) -> Vec<Sizing> {
        self.columns.iter().map(|c| c.sizing).collect()
    }

    fn cell_index(&self, row: usize, column: usize) -> usize {
        self.header_len + row * self.columns.len() + column
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
fn resize_edge_at(
    point: Point,
    bounds: Rectangle,
    header: Rectangle,
    state: &State,
    spacing: f32,
    tolerance: f32,
) -> Option<usize> {
    if !header.contains(point) {
        return None;
    }

    (0..state.widths.len()).rev().find(|&i| {
        let edge = bounds.x + state.offsets[i] + state.widths[i] + spacing / 2.0 - state.offset.x;
        (point.x - edge).abs() <= tolerance
    })
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
            let width = size.width + h_pad;

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
        let available = limits.max().width;
        let widths =
            sizing::resolve_widths(&sizing, &intrinsic, available, self.spacing, self.overflow);
        let offsets = sizing::offsets(&widths, self.spacing);

        let content_width =
            widths.iter().sum::<f32>() + self.spacing * (columns.saturating_sub(1)) as f32;
        let header_height = header_row_height * self.header_rows as f32;
        let body_height = row_height * self.row_count as f32;

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
            let inner = Size::new((width - h_pad).max(0.0), (height - v_pad).max(0.0));

            nodes[cell.element] = self.elements[cell.element]
                .as_widget_mut()
                .layout(
                    &mut tree.children[cell.element],
                    renderer,
                    &layout::Limits::new(Size::ZERO, inner),
                )
                .align(
                    alignment::Alignment::Center,
                    alignment::Alignment::Center,
                    inner,
                )
                .move_to(Point::new(
                    offsets[cell.start] + self.padding.left,
                    header_row_height * cell.row as f32 + self.padding.top,
                ));
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
                    .align(
                        self.columns[column].align.into(),
                        alignment::Alignment::Center,
                        inner,
                    )
                    .move_to(Point::new(
                        offsets[column] + self.padding.left,
                        y + self.padding.top,
                    ));
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

        // Frame + base background across the whole widget.
        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border: appearance.border,
                ..Default::default()
            },
            appearance
                .row_background
                .unwrap_or(iced::Background::Color(iced::Color::TRANSPARENT)),
        );

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
                if let Some(background) = appearance.alternate_row_background {
                    for row in (first..last).filter(|r| r % 2 == 1) {
                        renderer.fill_quad(
                            renderer::Quad {
                                bounds: Rectangle {
                                    x: bounds.x,
                                    y: bounds.y
                                        + state.header_height
                                        + state.row_height * row as f32,
                                    width: state.content.width.max(bounds.width),
                                    height: state.row_height,
                                },
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

                // Only visible rows are drawn. This is arithmetic on the row
                // range rather than a rectangle test per child, which is what
                // makes large tables cheap to redraw.
                for row in first..last {
                    for column in 0..columns {
                        let index = self.cell_index(row, column);

                        self.elements[index].as_widget().draw(
                            &tree.children[index],
                            renderer,
                            theme,
                            style,
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

        renderer.with_layer(header_region, |renderer| {
            // Backgrounds outside the translation: they should cover the
            // viewport regardless of horizontal scroll.
            if let Some(background) = appearance.group_background {
                if self.header_rows > 1 {
                    renderer.fill_quad(
                        renderer::Quad {
                            bounds: Rectangle {
                                height: state.header_row_height * (self.header_rows - 1) as f32,
                                ..header_region
                            },
                            ..Default::default()
                        },
                        background,
                    );
                }
            }

            if let Some(background) = appearance.header_background {
                renderer.fill_quad(
                    renderer::Quad {
                        bounds: Rectangle {
                            y: bounds.y
                                + state.header_row_height * (self.header_rows.saturating_sub(1))
                                    as f32,
                            height: state.header_row_height,
                            ..header_region
                        },
                        ..Default::default()
                    },
                    background,
                );
            }

            renderer.with_translation(Vector::new(-offset.x, 0.0), |renderer| {
                let rule = appearance.divider_width();

                // Borders follow the *spans*, not the leaf columns. Slicing a
                // group header with the dividers of the columns beneath it is
                // what made the hierarchy unreadable: every level looked like
                // the same flat grid. Each cell gets its own right edge, and
                // group rows get an underline separating them from the level
                // below.
                for i in 0..self.header_cells.len() {
                    let cell = self.header_cells[i];

                    let top = bounds.y + state.header_row_height * cell.row as f32;
                    let height = state.header_row_height * cell.row_span as f32;
                    let left = bounds.x + state.offsets[cell.start];
                    let right = bounds.x + state.offsets[cell.end] + state.widths[cell.end];

                    if let Some(color) = appearance.column_divider {
                        if cell.end + 1 < self.columns.len() {
                            renderer.fill_quad(
                                renderer::Quad {
                                    bounds: Rectangle {
                                        x: right + self.spacing / 2.0 - rule / 2.0,
                                        y: top,
                                        width: rule,
                                        height,
                                    },
                                    ..Default::default()
                                },
                                color,
                            );
                        }
                    }

                    if let Some(color) = appearance.row_divider {
                        // Only bands that have another level below them. A
                        // leaf bottoms out against the header_divider.
                        if cell.row + cell.row_span < self.header_rows {
                            renderer.fill_quad(
                                renderer::Quad {
                                    bounds: Rectangle {
                                        x: left,
                                        y: top + height - rule,
                                        width: (right - left).max(0.0),
                                        height: rule,
                                    },
                                    ..Default::default()
                                },
                                color,
                            );
                        }
                    }
                }

                for i in 0..self.header_cells.len() {
                    let element = self.header_cells[i].element;

                    self.elements[element].as_widget().draw(
                        &tree.children[element],
                        renderer,
                        theme,
                        style,
                        layout.child(element),
                        header_cursor,
                        &header_region,
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

                        // Column widths are layout, not paint -- a redraw
                        // alone would show stale geometry.
                        shell.invalidate_layout();
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
                            state,
                            self.spacing,
                            self.resize_tolerance,
                        ) {
                            state.resizing = Some(Resize {
                                column,
                                origin_x: point.x,
                                origin_width: state.widths[column],
                            });
                            shell.capture_event();
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
                (header_cursor, header_region)
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

        // --- 4. wheel, only if nothing downstream claimed the event ---
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
//! Visual test harness.
//!
//! Each control here targets something that is hard to verify by reading the
//! code. Notes on what to actually look for are in `CHECKS` below.

use advanced_table::{
    group, leaf, CellPosition, CellStyle, Click, DataTable, Direction, Overflow, Policy,
    SelectionMode, Sizing, Sort,
};

use std::collections::BTreeSet;

use iced::widget::{button, checkbox, column, container, pick_list, row, text};
use iced::{alignment, Color, Element, Fill, Length, Task};

/// What to look at once it runs:
///
/// 1. SCROLL DOWN. The three header rows must stay put while rows move.
/// 2. SCROLL RIGHT. The header must move *with* the columns -- sideways only.
///    If it stays still, the header layer is missing its X translation.
/// 3. Scroll down a long way, then click an "Open" button. The status line
///    must name the row you actually clicked. This is the real test of cursor
///    translation: if the offset is applied with the wrong sign you will hit
///    a row roughly `offset / row_height` away from the one under the pointer,
///    or nothing at all.
/// 4. Drag the vertical thumb to the very bottom. It should sit flush with the
///    end of its track and stay glued to the pointer the whole way down.
/// 5. Click the empty part of the track. The thumb should jump to the pointer
///    and continue dragging from there.
/// 6. Switch to 3 rows. Scrollbars should disappear and the offset should
///    reset rather than leaving the view scrolled past the end.
/// 7. "Financials" is wider than the four numeric columns under it. Those
///    columns must widen to fit it, staying in proportion to each other --
///    and the body cells must stay aligned under their headers.
/// 8. Drag the window narrow. With Scroll, columns keep their width and a
///    horizontal bar appears. With Shrink, they compress toward their minimums
///    and Fixed columns stay put.
/// 9. The outer frame must stay visible on all four sides with stripes on, at
///    every scroll position, and over the scrollbars.
/// 10. Turn on "Conditional" -- Cost cells go red where the quarter lost money,
///     Revenue goes green where it made money, and the colours must survive
///     scrolling, selection and column selection.
/// 11. "ID" and "Actions" are ungrouped, so their header cells run the full
///     height of the header. Their column rules must run the full height too,
///     and clicking the blank band at the top of either must select the column.
/// 12. Drag a column edge. The indicator must appear and track the pointer for
///     the whole drag, the edge must stay glued to the cursor, and both must
///     stop the instant the button comes up. Nothing may happen only on release.
/// 13. Click a sort arrow three times: ascending, descending, unsorted. Click a
///     different column's arrow -- it must restart at ascending. Clicking the
///     header anywhere *other* than the arrow must still select the column.
/// 14. Shift-select a block of rows. The outline must box the whole block once,
///     not each row separately.
/// 15. Sort by First, then by Last, then back. No column may change width --
///     the same rows are present either way, only in a different order.
/// 16. Turn on "Cell selection". Clicking any cell must select just that cell,
///     and the only place left that still selects a row is the gutter at either
///     edge. Right-click anywhere and the status line names what was under it.
/// 17. Drag across cells: a rectangle sweeps out live, and dragging back toward
///     the start gives ground back rather than only ever growing. Ctrl-drag
///     adds a second block without disturbing the first. The same gesture must
///     work on rows (drag down the gutter) and columns (drag across headers).
/// 18. Click a cell, then arrow around. The selection follows, and arrowing
///     past the viewport edge scrolls just enough to keep it visible. Shift plus
///     arrows grows a rectangle from the anchor instead of moving it. Click
///     outside the table first and the arrows must do nothing.
/// 19. Hold Shift and press an arrow repeatedly. The block must keep growing
///     one step per press, in the direction pressed -- not snap back to two
///     items. Reverse direction and it shrinks back through the anchor.
/// 20. Select a block and press Ctrl+C, then paste into a spreadsheet. Columns
///     land in columns and rows in rows. Ctrl-click a ragged set and the paste
///     still arrives as a rectangle with the gaps blank.
/// 21. Drag a leaf header sideways. A drop indicator appears only after a few
///     pixels of movement -- a plain click must still just select the column --
///     and the column lands where the indicator was. With groups on, "First"
///     may only swap with "Last": it must never land outside Contact. With
///     sticky on, nothing may cross the frozen seam in either direction.
const CHECKS: () = ();

/// Leaf columns carrying data. "Actions" holds buttons, so it is not one.
const COLUMNS: usize = 7;

fn main() -> iced::Result {
    iced::run(Demo::update, Demo::view)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Record {
    id: u32,
    first: &'static str,
    last: &'static str,
    q3_revenue: u32,
    q3_cost: u32,
    q4_revenue: u32,
    q4_cost: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowCount {
    Few,
    Many,
    Huge,
}

impl RowCount {
    const ALL: [RowCount; 3] = [RowCount::Few, RowCount::Many, RowCount::Huge];

    fn value(self) -> usize {
        match self {
            RowCount::Few => 3,
            RowCount::Many => 500,
            RowCount::Huge => 50_000,
        }
    }
}

impl std::fmt::Display for RowCount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} rows", self.value())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverflowChoice {
    Scroll,
    Shrink,
}

impl OverflowChoice {
    const ALL: [OverflowChoice; 2] = [OverflowChoice::Scroll, OverflowChoice::Shrink];
}

impl std::fmt::Display for OverflowChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            OverflowChoice::Scroll => "Overflow: Scroll",
            OverflowChoice::Shrink => "Overflow: Shrink",
        })
    }
}

#[derive(Debug, Clone)]
enum Message {
    Open(u32),
    SelectionChanged(BTreeSet<usize>),
    ColumnsChanged(BTreeSet<usize>),
    RowsChanged(RowCount),
    OverflowChanged(OverflowChoice),
    ToggleGroups(bool),
    ToggleStripes(bool),
    ToggleConditional(bool),
    SortChanged(Option<Sort>),
    RightClicked(Click),
    ToggleCells(bool),
    ToggleSticky(bool),
    CellsChanged(BTreeSet<CellPosition>),
    Copy,
    Reordered(usize, usize),
}

struct Demo {
    records: Vec<Record>,
    rows: RowCount,
    overflow: OverflowChoice,
    groups: bool,
    stripes: bool,
    conditional: bool,
    cells: bool,
    sticky: bool,
    /// Display position -> logical column. Reordering permutes this; every
    /// index the widget hands back is a *display* position and is read through
    /// it before touching the data.
    order: Vec<usize>,
    sort: Option<Sort>,
    selected_cells: BTreeSet<CellPosition>,
    selected: BTreeSet<usize>,
    selected_columns: BTreeSet<usize>,
    status: String,
}

impl Default for Demo {
    fn default() -> Self {
        let mut demo = Self {
            records: Vec::new(),
            rows: RowCount::Many,
            overflow: OverflowChoice::Scroll,
            groups: true,
            stripes: true,
            conditional: true,
            cells: false,
            sticky: true,
            order: (0..8).collect(),
            sort: None,
            selected_cells: BTreeSet::new(),
            selected: BTreeSet::new(),
            selected_columns: BTreeSet::new(),
            status: String::from("no row clicked yet"),
        };

        demo.regenerate();
        demo
    }
}

impl Demo {
    fn regenerate(&mut self) {
        const FIRST: [&str; 8] = [
            "Ada", "Grace", "Alan", "Barbara", "Edsger", "Margaret", "Ken", "Radia",
        ];
        // A deliberately long value, to prove Fit columns size to content and
        // that a wide cell does not drag its neighbours along with it.
        const LAST: [&str; 8] = [
            "Lovelace",
            "Hopper",
            "Turing",
            "Liskov",
            "Dijkstra-van-der-Meulen",
            "Hamilton",
            "Thompson",
            "Perlman",
        ];

        self.records = (0..self.rows.value())
            .map(|i| Record {
                id: i as u32 + 1,
                first: FIRST[i % FIRST.len()],
                last: LAST[i % LAST.len()],
                q3_revenue: ((i * 7919) % 90_000 + 10_000) as u32,
                q3_cost: ((i * 4241) % 40_000 + 5_000) as u32,
                q4_revenue: ((i * 6673) % 90_000 + 10_000) as u32,
                q4_cost: ((i * 3499) % 40_000 + 5_000) as u32,
            })
            .collect();

        self.resort();
    }

    /// One leaf header, by **logical** column. Reordering never changes these;
    /// it only changes the order they are emitted in.
    fn column_header(&self, logical: usize) -> advanced_table::HeaderNode<'_, Message, iced::Theme, iced::Renderer> {
        let right = alignment::Horizontal::Right;

        match logical {
            0 => leaf(text("ID")).fixed(70.0).align(right).sortable(),
            1 => leaf(text("First")).fill(1).sortable(),
            2 => leaf(text("Last")).fill(2).sortable(),
            3 | 4 | 5 | 6 => {
                let flat = ["Q3 Revenue", "Q3 Cost", "Q4 Revenue", "Q4 Cost"][logical - 3];
                let nested = ["Revenue", "Cost", "Revenue", "Cost"][logical - 3];

                leaf(text(if self.groups { nested } else { flat }))
                    .align(right)
                    .sortable()
            }
            // Buttons, so nothing to sort by.
            _ => leaf(text("Actions")).fixed(110.0),
        }
    }

    /// Rebuild the three-level header from `order`.
    ///
    /// Works by scanning for *consecutive* runs sharing a parent. That is sound
    /// precisely because the widget only ever reports sibling moves, which is
    /// what keeps a group's columns adjacent -- a group whose leaves were
    /// scattered would have no rectangle to draw its label in.
    fn grouped_headers(&self) -> Vec<advanced_table::HeaderNode<'_, Message, iced::Theme, iced::Renderer>> {
        fn top(logical: usize) -> Option<&'static str> {
            match logical {
                1 | 2 => Some("Contact"),
                3..=6 => Some("Financials (Restated, Unaudited)"),
                _ => None,
            }
        }

        fn quarter(logical: usize) -> &'static str {
            if logical <= 4 {
                "Q3"
            } else {
                "Q4"
            }
        }

        let runs = |slice: &[usize], key: &dyn Fn(usize) -> Option<&'static str>| {
            let mut out: Vec<(Option<&'static str>, Vec<usize>)> = Vec::new();

            for &logical in slice {
                let k = key(logical);

                match out.last_mut() {
                    Some((last, run)) if *last == k && k.is_some() => run.push(logical),
                    _ => out.push((k, vec![logical])),
                }
            }

            out
        };

        runs(&self.order, &top)
            .into_iter()
            .flat_map(|(name, run)| match name {
                None => run
                    .into_iter()
                    .map(|logical| self.column_header(logical))
                    .collect::<Vec<_>>(),
                Some(name) if name.starts_with("Financials") => {
                    let quarters = runs(&run, &|logical| Some(quarter(logical)))
                        .into_iter()
                        .map(|(q, leaves)| {
                            group(
                                text(q.unwrap_or_default()),
                                leaves
                                    .into_iter()
                                    .map(|logical| self.column_header(logical))
                                    .collect::<Vec<_>>(),
                            )
                        })
                        .collect::<Vec<_>>();

                    vec![group(text(name), quarters)]
                }
                Some(name) => vec![group(
                    text(name),
                    run.into_iter()
                        .map(|logical| self.column_header(logical))
                        .collect::<Vec<_>>(),
                )],
            })
            .collect()
    }

    /// One cell's value as text. The table never sees these -- it only holds
    /// the `Element` built from them -- so producing them for the clipboard is
    /// necessarily the application's job.
    fn value(&self, row: usize, column: usize) -> String {
        let Some(record) = self.records.get(row) else {
            return String::new();
        };

        match self.order.get(column).copied().unwrap_or(column) {
            0 => record.id.to_string(),
            1 => record.first.to_string(),
            2 => record.last.to_string(),
            3 => record.q3_revenue.to_string(),
            4 => record.q3_cost.to_string(),
            5 => record.q4_revenue.to_string(),
            6 => record.q4_cost.to_string(),
            _ => String::new(),
        }
    }

    /// Whatever is selected, as the tab/newline text a spreadsheet pastes.
    fn clipboard_text(&self) -> String {
        let rows: Vec<Vec<String>> = if !self.selected_cells.is_empty() {
            // `grid` squares a ragged Ctrl-clicked selection off into the
            // rectangle a paste needs, marking the holes.
            advanced_table::selection::grid(&self.selected_cells)
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|cell| {
                            cell.map(|c| self.value(c.row, c.column)).unwrap_or_default()
                        })
                        .collect()
                })
                .collect()
        } else if !self.selected.is_empty() {
            // A whole row means every column of it.
            self.selected
                .iter()
                .map(|&row| (0..COLUMNS).map(|c| self.value(row, c)).collect())
                .collect()
        } else if !self.selected_columns.is_empty() {
            (0..self.records.len())
                .map(|row| {
                    self.selected_columns
                        .iter()
                        .map(|&column| self.value(row, column))
                        .collect()
                })
                .collect()
        } else {
            Vec::new()
        };

        rows.into_iter()
            .map(|row| row.join("	"))
            .collect::<Vec<_>>()
            .join("
")
    }

    /// The table reports what the sort should become; putting the rows in that
    /// order is the application's job. `sort.column` is the leaf column index,
    /// which is the same index the `rows` view function matches on -- so the
    /// two `match`es line up one to one.
    fn resort(&mut self) {
        let Some(sort) = self.sort else {
            self.records.sort_by_key(|record| record.id);
            return;
        };

        self.records.sort_by(|a, b| {
            let ordering = match self.order.get(sort.column).copied().unwrap_or(sort.column) {
                0 => a.id.cmp(&b.id),
                1 => a.first.cmp(b.first),
                2 => a.last.cmp(b.last),
                3 => a.q3_revenue.cmp(&b.q3_revenue),
                4 => a.q3_cost.cmp(&b.q3_cost),
                5 => a.q4_revenue.cmp(&b.q4_revenue),
                6 => a.q4_cost.cmp(&b.q4_cost),
                _ => std::cmp::Ordering::Equal,
            };

            match sort.direction {
                Direction::Ascending => ordering,
                Direction::Descending => ordering.reverse(),
            }
        });
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Open(id) => {
                self.status = format!("clicked row id {id}");
            }
            Message::SelectionChanged(selection) => {
                self.status = match selection.len() {
                    0 => "nothing selected".to_string(),
                    1 => format!("selected row {}", selection.iter().next().unwrap()),
                    n => format!(
                        "selected {n} rows ({}..={})",
                        selection.iter().next().unwrap(),
                        selection.iter().next_back().unwrap()
                    ),
                };
                self.selected = selection;
            }
            Message::ColumnsChanged(columns) => {
                self.status = format!("{} column(s) selected", columns.len());
                self.selected_columns = columns;
            }
            Message::RowsChanged(rows) => {
                self.rows = rows;
                self.selected.clear();
                self.regenerate();
            }
            Message::OverflowChanged(overflow) => self.overflow = overflow,
            Message::ToggleGroups(groups) => self.groups = groups,
            Message::ToggleStripes(stripes) => self.stripes = stripes,
            Message::ToggleConditional(conditional) => self.conditional = conditional,
            Message::SortChanged(sort) => {
                self.sort = sort;
                self.status = match sort {
                    Some(sort) => format!("sorted by column {} ({:?})", sort.column, sort.direction),
                    None => "unsorted".to_string(),
                };
                // Row indices are positional, so reordering the data re-points
                // every selected index at a different record. A real app would
                // key its selection by id; the demo just drops it.
                self.selected.clear();
                self.resort();
            }
            // A real app would open a context menu here, positioned at
            // `click.position`. The status line stands in for it.
            Message::ToggleSticky(sticky) => self.sticky = sticky,
            Message::Reordered(from, to) => {
                advanced_table::reorder(&mut self.order, from, to);

                // Column and cell selections are keyed by display position, so
                // after a permutation they point at different columns than the
                // user picked. Dropping them is honest; silently re-pointing
                // them is not.
                self.selected_columns.clear();
                self.selected_cells.clear();

                // The sort *follows its column*: work out where the moved
                // column ended up by applying the same permutation to an
                // identity list.
                if let Some(sort) = &mut self.sort {
                    let mut positions: Vec<usize> = (0..self.order.len()).collect();
                    advanced_table::reorder(&mut positions, from, to);

                    if let Some(now) = positions.iter().position(|&was| was == sort.column) {
                        sort.column = now;
                    }
                }

                self.status = format!("moved column {from} to {to}");
            }
            Message::Copy => {
                let text = self.clipboard_text();

                if !text.is_empty() {
                    self.status = format!("copied {} lines", text.lines().count());
                    return iced::clipboard::write(text).discard();
                }
            }
            Message::ToggleCells(cells) => {
                self.cells = cells;
                self.selected_cells.clear();
            }
            Message::CellsChanged(cells) => {
                self.status = match cells.len() {
                    0 => "no cells selected".to_string(),
                    1 => {
                        let cell = cells.iter().next().unwrap();
                        format!("selected cell ({}, {})", cell.row, cell.column)
                    }
                    n => format!("selected {n} cells"),
                };
                self.selected_cells = cells;
            }
            Message::RightClicked(click) => {
                self.status = match (click.row, click.column) {
                    (Some(row), Some(column)) => format!("right-click on cell ({row}, {column})"),
                    (Some(row), None) => format!("right-click on row {row}'s gutter"),
                    (None, Some(column)) => format!("right-click on column {column}'s header"),
                    (None, None) => "right-click outside every cell".to_string(),
                };
            }
        }

        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        // pick_list on master takes (selected, options, to_string) in that
        // order, and the callback moved to `.on_select`. It no longer uses
        // Display implicitly -- hence the explicit `to_string` closures.
        // checkbox takes only `is_checked`; the label is a builder.
        let controls = row![
            pick_list(Some(self.rows), RowCount::ALL, |rows| rows.to_string())
                .on_select(Message::RowsChanged),
            pick_list(
                Some(self.overflow),
                OverflowChoice::ALL,
                |choice| choice.to_string()
            )
            .on_select(Message::OverflowChanged),
            checkbox(self.groups)
                .label("Grouped headers")
                .on_toggle(Message::ToggleGroups),
            checkbox(self.stripes)
                .label("Stripes")
                .on_toggle(Message::ToggleStripes),
            checkbox(self.conditional)
                .label("Conditional")
                .on_toggle(Message::ToggleConditional),
            checkbox(self.cells)
                .label("Cell selection")
                .on_toggle(Message::ToggleCells),
            checkbox(self.sticky)
                .label("Sticky ID + Contact")
                .on_toggle(Message::ToggleSticky),
        ]
        .spacing(12)
        .align_y(alignment::Vertical::Center);

        // Two header shapes off the same column set, so the flat path and the
        // three-level path can be compared without changing anything else.
        // Built from `order`, so a reorder is genuinely reflected rather than
        // just reported. Sticky is applied to the first two *top-level* nodes
        // whatever they now are, which keeps it a leading run.
        let headers: Vec<_> = if self.groups {
            self.grouped_headers()
        } else {
            self.order.iter().map(|&l| self.column_header(l)).collect()
        }
        .into_iter()
        .enumerate()
        .map(|(i, node)| pin(node, self.sticky && i < 2))
        .collect();

        let stripes = self.stripes;
        let conditional = self.conditional;
        let records = &self.records;
        let order = self.order.clone();

        let mut table = DataTable::new(headers)
            .rows(&self.records, |record, column| match order[column] {
                0 => text(record.id).into(),
                1 => text(record.first).into(),
                2 => text(record.last).into(),
                3 => text(format!("${}", record.q3_revenue)).into(),
                4 => text(format!("${}", record.q3_cost)).into(),
                5 => text(format!("${}", record.q4_revenue)).into(),
                6 => text(format!("${}", record.q4_cost)).into(),
                // An interactive cell. Clicking this while scrolled is the
                // only way to confirm the cursor translation is right.
                _ => button(text("Open").size(12))
                    .padding([2, 8])
                    .on_press(Message::Open(record.id))
                    .into(),
            })
            .selection(
                SelectionMode::Multiple,
                &self.selected,
                Message::SelectionChanged,
            )
            .column_selection(
                SelectionMode::Multiple,
                &self.selected_columns,
                Message::ColumnsChanged,
            )
            .sorting(self.sort, Message::SortChanged)
            .on_right_click(Message::RightClicked)
            .on_copy(|| Message::Copy)
            .on_reorder(Message::Reordered)
            .overflow(match self.overflow {
                OverflowChoice::Scroll => Overflow::Scroll,
                OverflowChoice::Shrink => Overflow::Shrink,
            })
            .vertical_scroll(Policy::Auto)
            .horizontal_scroll(Policy::Auto)
            .spacing(1.0)
            .height(Fill)
            .style(move |theme| {
                let mut style = advanced_table::style::default(theme);

                if !stripes {
                    style.alternate_row_background = None;
                }

                style
            })
            // The answer to "how do I do conditional styling per column?".
            // Note that the condition is not really about the column -- it is
            // about the *value*, which the widget cannot see. So the hook hands
            // back the coordinates and we look the record up ourselves, exactly
            // as the `rows` view function above does.
            .cell_style(move |theme, cell| {
                if !conditional {
                    return CellStyle::default();
                }

                let Some(record) = records.get(cell.row) else {
                    return CellStyle::default();
                };

                let (revenue, cost) = match order[cell.column] {
                    3 | 4 => (record.q3_revenue, record.q3_cost),
                    5 | 6 => (record.q4_revenue, record.q4_cost),
                    _ => return CellStyle::default(),
                };

                let palette = theme.palette();
                let profitable = revenue > cost * 2;

                match order[cell.column] {
                    // Revenue: colour alone, so the number still reads as a
                    // number rather than as a badge.
                    3 | 5 if profitable => CellStyle::default().color(palette.success.base.color),
                    // Cost: colour plus a wash, to show a cell background
                    // composing with the stripes and the selection highlight.
                    // It is translucent on purpose -- an opaque fill here would
                    // hide the row selection underneath it.
                    4 | 6 if !profitable => CellStyle::default()
                        .color(palette.danger.base.color)
                        .background(Color {
                            a: 0.15,
                            ..palette.danger.base.color
                        }),
                    _ => CellStyle::default(),
                }
            });

        // Left off entirely when the toggle is down, so the "does enabling
        // this take row selection away?" question can be answered by flipping
        // one checkbox: with it on, only the gutters still select a row.
        if self.cells {
            table = table.cell_selection(
                SelectionMode::Multiple,
                &self.selected_cells,
                Message::CellsChanged,
            );
        }

        column![
            controls,
            container(table).height(Fill).width(Fill),
            text(&self.status).size(13),
        ]
        .spacing(10)
        .padding(16)
        .height(Fill)
        .into()
    }
}

/// `.sticky()` is a builder, so toggling it needs a conditional rather than a
/// chained call. A free function rather than a closure: closure lifetime
/// inference ties the argument and the return value to different regions and
/// then refuses to unify them.
fn pin<'a, Message, Theme, Renderer>(
    node: advanced_table::HeaderNode<'a, Message, Theme, Renderer>,
    sticky: bool,
) -> advanced_table::HeaderNode<'a, Message, Theme, Renderer> {
    if sticky {
        node.sticky()
    } else {
        node
    }
}

/// Sizing modes referenced above, kept so the import does not drift out of
/// sync while experimenting: Fixed / Fit / Fill.
#[allow(dead_code)]
fn sizing_reference() -> [Sizing; 3] {
    [
        Sizing::Fixed(80.0),
        Sizing::Fit {
            min: 60.0,
            max: 320.0,
        },
        Sizing::Fill {
            weight: 2,
            min: 48.0,
        },
    ]
}

#[allow(dead_code)]
fn length_reference() -> Length {
    Length::Fill
}

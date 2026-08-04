//! Visual test harness.
//!
//! Each control here targets something that is hard to verify by reading the
//! code. Notes on what to actually look for are in `CHECKS` below.

use advanced_table::{group, leaf, DataTable, Overflow, Policy, SelectionMode, Sizing};

use std::collections::BTreeSet;

use iced::widget::{button, checkbox, column, container, pick_list, row, text};
use iced::{alignment, Element, Fill, Length, Task};

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
const CHECKS: () = ();

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
}

struct Demo {
    records: Vec<Record>,
    rows: RowCount,
    overflow: OverflowChoice,
    groups: bool,
    stripes: bool,
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
        ]
        .spacing(12)
        .align_y(alignment::Vertical::Center);

        // Two header shapes off the same column set, so the flat path and the
        // three-level path can be compared without changing anything else.
        let headers = if self.groups {
            vec![
                leaf(text("ID"))
                    .fixed(70.0)
                    .align(alignment::Horizontal::Right),
                group(
                    text("Contact"),
                    vec![leaf(text("First")).fill(1), leaf(text("Last")).fill(2)],
                ),
                // This label is wider than the four numeric columns beneath it,
                // which is what exercises apply_span_constraints.
                group(
                    text("Financials (Restated, Unaudited)"),
                    vec![
                        group(
                            text("Q3"),
                            vec![
                                leaf(text("Revenue")).align(alignment::Horizontal::Right),
                                leaf(text("Cost")).align(alignment::Horizontal::Right),
                            ],
                        ),
                        group(
                            text("Q4"),
                            vec![
                                leaf(text("Revenue")).align(alignment::Horizontal::Right),
                                leaf(text("Cost")).align(alignment::Horizontal::Right),
                            ],
                        ),
                    ],
                ),
                leaf(text("Actions")).fixed(110.0),
            ]
        } else {
            vec![
                leaf(text("ID"))
                    .fixed(70.0)
                    .align(alignment::Horizontal::Right),
                leaf(text("First")).fill(1),
                leaf(text("Last")).fill(2),
                leaf(text("Q3 Revenue")).align(alignment::Horizontal::Right),
                leaf(text("Q3 Cost")).align(alignment::Horizontal::Right),
                leaf(text("Q4 Revenue")).align(alignment::Horizontal::Right),
                leaf(text("Q4 Cost")).align(alignment::Horizontal::Right),
                leaf(text("Actions")).fixed(110.0),
            ]
        };

        let stripes = self.stripes;

        let table = DataTable::new(headers)
            .rows(&self.records, |record, column| match column {
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
            });

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

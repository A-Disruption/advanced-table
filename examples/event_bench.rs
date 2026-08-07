//! Measures what one input event costs as the row count grows.
//!
//! ```text
//! cargo run --profile measure --example event_bench
//! ```
//!
//! The `measure` profile is release codegen with `debug_assertions` left on,
//! because iced's null renderer is gated behind it.
//!
//! Drives a real `UserInterface` headlessly against the null renderer, so the
//! numbers are the widget's own work — no window, no GPU, no compositor.
//!
//! A `CursorMoved` is the event to watch: it is the one the user generates
//! continuously, and every one of them is forwarded to every cell. `draw` is
//! timed alongside it as a control, because `draw` already culls to the visible
//! row range and so should stay flat while the others climb.

use std::time::{Duration, Instant};

use advanced_table::{data_table, leaf};
use iced::advanced::shell;
use iced::widget::text;
use iced::{Event, Length, Point, Size, mouse};
use iced_runtime::user_interface::{Cache, UserInterface};

const COLUMNS: usize = 8;
const VIEWPORT: Size = Size::new(1200.0, 700.0);

/// How many times each pass is repeated before dividing. Enough that a single
/// slow sample cannot dominate.
const SAMPLES: u32 = 30;

struct Row {
    cells: Vec<String>,
}

fn rows(count: usize) -> Vec<Row> {
    (0..count)
        .map(|row| Row {
            cells: (0..COLUMNS)
                .map(|column| format!("r{row}c{column}"))
                .collect(),
        })
        .collect()
}

fn main() {
    println!(
        "{:>9}  {:>12}  {:>12}  {:>12}",
        "rows", "build+layout", "cursor move", "draw"
    );
    println!("{}", "-".repeat(51));

    for count in [1_000usize, 10_000, 50_000, 100_000] {
        let data = rows(count);

        let (build, event, draw) = measure(&data);

        println!(
            "{:>9}  {:>12}  {:>12}  {:>12}",
            count,
            millis(build),
            millis(event),
            millis(draw),
        );
    }
}

fn measure(data: &[Row]) -> (Duration, Duration, Duration) {
    let mut renderer = ();

    // The cursor sits over the body, a little below the header, so it is
    // genuinely over a row rather than off the end of the table.
    let cursor = mouse::Cursor::Available(Point::new(400.0, 200.0));

    let build_start = Instant::now();
    let mut ui = UserInterface::build(
        table(data),
        VIEWPORT,
        Cache::default(),
        &mut renderer,
    );
    let build = build_start.elapsed();

    // One warm-up pass: the first update settles hover state and any lazily
    // built caches, and timing that would flatter every later sample.
    let _ = update(&mut ui, &[], cursor, &mut renderer);

    let event_start = Instant::now();
    for i in 0..SAMPLES {
        // Nudge the cursor each time so nothing can short-circuit on "the
        // pointer has not actually moved".
        let moved = Point::new(400.0 + (i % 5) as f32, 200.0 + (i % 7) as f32);

        let _ = update(
            &mut ui,
            &[Event::Mouse(mouse::Event::CursorMoved { position: moved })],
            mouse::Cursor::Available(moved),
            &mut renderer,
        );
    }
    let event = event_start.elapsed() / SAMPLES;

    let draw_start = Instant::now();
    for _ in 0..SAMPLES {
        ui.draw(
            &mut renderer,
            &iced::Theme::Dark,
            &iced::advanced::renderer::Style::default(),
            cursor,
        );
    }
    let draw = draw_start.elapsed() / SAMPLES;

    (build, event, draw)
}

fn table(data: &[Row]) -> iced::Element<'_, (), iced::Theme, ()> {
    let headers = (0..COLUMNS)
        .map(|column| leaf(text(format!("Column {column}"))))
        .collect();

    data_table(headers)
        .rows(data, |row: &Row, column| text(&row.cells[column]).into())
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn update(
    ui: &mut UserInterface<'_, (), iced::Theme, ()>,
    events: &[Event],
    cursor: mouse::Cursor,
    renderer: &mut (),
) -> iced_runtime::user_interface::State {
    let mut bus = shell::Bus::new();

    let (state, _statuses) = ui.update(
        &iced::window::Headless,
        &shell::Waker::noop(),
        events,
        cursor,
        renderer,
        &mut bus,
    );

    state
}

fn millis(duration: Duration) -> String {
    format!("{:.3} ms", duration.as_secs_f64() * 1_000.0)
}

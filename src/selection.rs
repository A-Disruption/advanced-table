//! Row selection.
//!
//! # Who owns the selection?
//!
//! The application does. The widget is handed the current set and reports back
//! what the set *should* become; it never keeps its own copy.
//!
//! The alternative -- the widget owning a `BTreeSet` internally and the app
//! reading it back -- looks more convenient right up until the app needs to
//! act on the selection, which is essentially always ("delete selected",
//! "export selected", "enable the toolbar button"). At that point the app
//! keeps its own copy anyway and the two can drift: filter the rows, sort
//! them, delete one, and the widget's indices now refer to different records
//! than the app's do, with nothing to force reconciliation.
//!
//! What *is* genuinely widget-internal is the **anchor**: the row a Shift-range
//! extends from. It is ephemeral interaction state, meaningless to the app, and
//! nothing breaks if it is lost. So that lives in the widget and the set does
//! not.

use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Rows cannot be selected. Clicks pass through to cells as normal.
    #[default]
    None,
    /// At most one row at a time.
    Single,
    /// Ctrl/Cmd toggles, Shift extends a range from the anchor.
    Multiple,
}

/// One cell, by position, in the same leaf-column coordinates the `rows` view
/// function and [`Click`](crate::Click) use.
///
/// Single cell rather than a set: a cell selection that spans a range is a
/// *rectangle*, not a list, and the set-plus-anchor machinery rows use does not
/// describe one. Ranges can be added later without breaking this, since a
/// single cell is the degenerate rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CellPosition {
    pub row: usize,
    pub column: usize,
}

impl CellPosition {
    pub fn new(row: usize, column: usize) -> Self {
        Self { row, column }
    }
}

/// What a click should produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome<T = usize> {
    pub selection: BTreeSet<T>,
    /// The new anchor, for a later Shift-click to extend from.
    pub anchor: Option<T>,
}

/// Every item between two anchors, for a linear axis -- rows or columns.
pub fn span(a: usize, b: usize) -> BTreeSet<usize> {
    (a.min(b)..=a.max(b)).collect()
}

/// Every cell in the rectangle two corners describe.
///
/// A cell range is genuinely a *rectangle*, not the run of cells you would get
/// by reading left-to-right top-to-bottom between the two corners. Dragging
/// from B2 to D4 selects nine cells, not the eleven that lie between them in
/// reading order -- which is why cells cannot reuse [`span`].
pub fn rectangle(a: CellPosition, b: CellPosition) -> BTreeSet<CellPosition> {
    let rows = a.row.min(b.row)..=a.row.max(b.row);
    let columns = a.column.min(b.column)..=a.column.max(b.column);

    rows.flat_map(|row| {
        columns
            .clone()
            .map(move |column| CellPosition::new(row, column))
    })
    .collect()
}

/// Move the item at `from` so that it ends up at index `to`.
///
/// `to` is the index the item should occupy **once it has been taken out**,
/// which is what [`DataTable::on_reorder`] reports -- one less than the gap it
/// was dropped into whenever the column moved right. Provided so that
/// adjustment has a single definition instead of one per caller.
///
/// [`DataTable::on_reorder`]: crate::DataTable::on_reorder
pub fn reorder<T>(items: &mut Vec<T>, from: usize, to: usize) {
    if from == to || from >= items.len() || to >= items.len() {
        return;
    }

    let item = items.remove(from);
    items.insert(to, item);
}

/// Lay a cell selection out as the rectangle enclosing it, in reading order.
///
/// Spreadsheets exchange a **rectangle**: tabs between columns, newlines
/// between rows. A selection built with Ctrl-clicks is not necessarily
/// rectangular, so the bounding box is squared off and the positions that were
/// never selected come back as `None` -- write those as empty strings and the
/// paste still lands in the right shape.
///
/// The table cannot do the copying itself: it only ever sees the `Element` you
/// built from a value, never the value. What it can do is hand you the shape,
/// which is the part that is fiddly to get right.
pub fn grid(selection: &BTreeSet<CellPosition>) -> Vec<Vec<Option<CellPosition>>> {
    let Some(first) = selection.iter().next() else {
        return Vec::new();
    };

    let (mut top, mut bottom) = (first.row, first.row);
    let (mut left, mut right) = (first.column, first.column);

    for cell in selection {
        top = top.min(cell.row);
        bottom = bottom.max(cell.row);
        left = left.min(cell.column);
        right = right.max(cell.column);
    }

    (top..=bottom)
        .map(|row| {
            (left..=right)
                .map(|column| {
                    let cell = CellPosition::new(row, column);

                    selection.contains(&cell).then_some(cell)
                })
                .collect()
        })
        .collect()
}

/// Resolve a click into a new selection.
///
/// Returns `None` when nothing should change, so the caller can skip emitting
/// a message rather than spamming identical updates.
///
/// `span` is what an extend (Shift) covers between the anchor and the target.
/// It is the only thing that differs between rows, columns and cells, so all
/// three share this one implementation and cannot drift apart in behaviour.
pub fn resolve<T: Ord + Copy>(
    mode: Mode,
    current: &BTreeSet<T>,
    anchor: Option<T>,
    target: T,
    toggle: bool,
    extend: bool,
    span: impl Fn(T, T) -> BTreeSet<T>,
) -> Option<Outcome<T>> {
    let outcome = match mode {
        Mode::None => return None,

        Mode::Single => Outcome {
            selection: BTreeSet::from([target]),
            anchor: Some(target),
        },

        Mode::Multiple => {
            if extend {
                // Shift extends from the anchor. Without an anchor there is
                // nothing to extend from, so it degrades to a plain click.
                let range = span(anchor.unwrap_or(target), target);

                Outcome {
                    selection: if toggle {
                        // Ctrl+Shift unions onto what is already there,
                        // matching how file managers and spreadsheets behave.
                        current.union(&range).copied().collect()
                    } else {
                        range
                    },
                    // Deliberately preserved: repeated Shift-clicks should
                    // all measure from the same origin, not walk it forward.
                    anchor,
                }
            } else if toggle {
                let mut selection = current.clone();

                if !selection.remove(&target) {
                    selection.insert(target);
                }

                Outcome {
                    selection,
                    anchor: Some(target),
                }
            } else {
                Outcome {
                    selection: BTreeSet::from([target]),
                    anchor: Some(target),
                }
            }
        }
    };

    if outcome.selection == *current && outcome.anchor == anchor {
        None
    } else {
        Some(outcome)
    }
}

/// [`resolve`] for a linear axis -- rows or columns.
pub fn apply(
    mode: Mode,
    current: &BTreeSet<usize>,
    anchor: Option<usize>,
    row: usize,
    toggle: bool,
    extend: bool,
) -> Option<Outcome> {
    resolve(mode, current, anchor, row, toggle, extend, span)
}

/// [`resolve`] for cells, where an extend covers a rectangle.
pub fn apply_cells(
    mode: Mode,
    current: &BTreeSet<CellPosition>,
    anchor: Option<CellPosition>,
    target: CellPosition,
    toggle: bool,
    extend: bool,
) -> Option<Outcome<CellPosition>> {
    resolve(mode, current, anchor, target, toggle, extend, rectangle)
}

#[cfg(test)]
mod cell_tests {
    use super::*;

    fn at(row: usize, column: usize) -> CellPosition {
        CellPosition::new(row, column)
    }

    #[test]
    fn extending_covers_a_rectangle_not_a_reading_order_run() {
        let outcome = apply_cells(
            Mode::Multiple,
            &BTreeSet::from([at(1, 1)]),
            Some(at(1, 1)),
            at(3, 2),
            false,
            true,
        )
        .unwrap();

        // Rows 1..=3 x columns 1..=2 -- six cells. Reading order between the
        // two corners would have given eight.
        assert_eq!(outcome.selection.len(), 6);
        assert!(outcome.selection.contains(&at(2, 1)));
        assert!(outcome.selection.contains(&at(3, 2)));
        assert!(!outcome.selection.contains(&at(2, 0)));
        assert!(!outcome.selection.contains(&at(2, 3)));
    }

    #[test]
    fn grid_squares_off_a_ragged_selection_and_marks_the_holes() {
        // Two Ctrl-clicked cells on a diagonal. Pasting needs a 2x2 block with
        // the off-diagonal left blank, not a two-cell run.
        let selection = BTreeSet::from([at(3, 1), at(4, 2)]);
        let grid = grid(&selection);

        assert_eq!(grid.len(), 2);
        assert_eq!(grid[0], vec![Some(at(3, 1)), None]);
        assert_eq!(grid[1], vec![None, Some(at(4, 2))]);
    }

    #[test]
    fn grid_of_a_swept_rectangle_has_no_holes() {
        let grid = grid(&rectangle(at(2, 1), at(4, 3)));

        assert_eq!(grid.len(), 3);
        assert!(grid.iter().all(|row| row.len() == 3));
        assert!(grid.iter().flatten().all(Option::is_some));
    }

    #[test]
    fn grid_of_nothing_is_nothing() {
        assert!(grid(&BTreeSet::new()).is_empty());
    }

    #[test]
    fn a_rectangle_is_the_same_whichever_corner_you_start_from() {
        let forward = rectangle(at(1, 1), at(3, 4));
        let backward = rectangle(at(3, 4), at(1, 1));

        assert_eq!(forward, backward);
        assert_eq!(forward.len(), 12);
    }

    #[test]
    fn ctrl_toggles_one_cell_and_leaves_the_rest() {
        let current = BTreeSet::from([at(0, 0), at(5, 2)]);

        let added = apply_cells(Mode::Multiple, &current, None, at(9, 1), true, false).unwrap();
        assert_eq!(added.selection.len(), 3);
        assert!(added.selection.contains(&at(9, 1)));

        let removed =
            apply_cells(Mode::Multiple, &current, None, at(5, 2), true, false).unwrap();
        assert_eq!(removed.selection, BTreeSet::from([at(0, 0)]));
    }

    #[test]
    fn ctrl_shift_unions_a_second_block_onto_the_first() {
        let current = rectangle(at(0, 0), at(1, 1));

        let outcome =
            apply_cells(Mode::Multiple, &current, Some(at(5, 0)), at(6, 1), true, true).unwrap();

        // Both blocks survive: four cells each, none shared.
        assert_eq!(outcome.selection.len(), 8);
        assert!(outcome.selection.contains(&at(0, 0)));
        assert!(outcome.selection.contains(&at(6, 1)));
    }

    #[test]
    fn single_mode_ignores_modifiers() {
        for (toggle, extend) in [(true, false), (false, true), (true, true)] {
            let outcome = apply_cells(
                Mode::Single,
                &BTreeSet::from([at(0, 0)]),
                Some(at(0, 0)),
                at(4, 3),
                toggle,
                extend,
            )
            .unwrap();

            assert_eq!(outcome.selection, BTreeSet::from([at(4, 3)]));
        }
    }

    #[test]
    fn repeated_shift_drags_all_measure_from_the_same_anchor() {
        // This is what a drag is: the anchor is fixed at the press and every
        // move re-resolves against it. If the anchor walked forward, dragging
        // back over your own path would leave cells behind.
        let anchor = at(2, 2);
        let mut selection = BTreeSet::from([anchor]);

        for target in [at(4, 4), at(6, 5), at(3, 3)] {
            let outcome =
                apply_cells(Mode::Multiple, &selection, Some(anchor), target, false, true).unwrap();

            assert_eq!(outcome.selection, rectangle(anchor, target));
            assert_eq!(outcome.anchor, Some(anchor));
            selection = outcome.selection;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(values: impl IntoIterator<Item = usize>) -> BTreeSet<usize> {
        values.into_iter().collect()
    }

    #[test]
    fn mode_none_never_changes_anything() {
        assert_eq!(apply(Mode::None, &set([]), None, 3, false, false), None);
        assert_eq!(apply(Mode::None, &set([1]), Some(1), 3, true, true), None);
    }

    #[test]
    fn single_mode_replaces_and_ignores_modifiers() {
        let out = apply(Mode::Single, &set([1]), Some(1), 4, true, true).unwrap();

        assert_eq!(out.selection, set([4]));
        assert_eq!(out.anchor, Some(4));
    }

    #[test]
    fn plain_click_replaces_the_selection() {
        let out = apply(Mode::Multiple, &set([1, 2, 3]), Some(1), 7, false, false).unwrap();

        assert_eq!(out.selection, set([7]));
        assert_eq!(out.anchor, Some(7));
    }

    #[test]
    fn ctrl_click_toggles_both_ways() {
        let added = apply(Mode::Multiple, &set([1]), Some(1), 5, true, false).unwrap();
        assert_eq!(added.selection, set([1, 5]));
        assert_eq!(added.anchor, Some(5));

        let removed = apply(Mode::Multiple, &set([1, 5]), Some(5), 5, true, false).unwrap();
        assert_eq!(removed.selection, set([1]));
    }

    #[test]
    fn shift_selects_an_inclusive_range_in_either_direction() {
        let down = apply(Mode::Multiple, &set([2]), Some(2), 5, false, true).unwrap();
        assert_eq!(down.selection, set([2, 3, 4, 5]));

        let up = apply(Mode::Multiple, &set([5]), Some(5), 2, false, true).unwrap();
        assert_eq!(up.selection, set([2, 3, 4, 5]));
    }

    #[test]
    fn repeated_shift_clicks_measure_from_the_same_anchor() {
        let first = apply(Mode::Multiple, &set([2]), Some(2), 6, false, true).unwrap();
        assert_eq!(first.anchor, Some(2), "anchor must not walk forward");

        // Shrinking the range back down has to work, which it only does if
        // the anchor stayed at 2 rather than moving to 6.
        let second = apply(Mode::Multiple, &first.selection, first.anchor, 4, false, true).unwrap();
        assert_eq!(second.selection, set([2, 3, 4]));
    }

    #[test]
    fn ctrl_shift_unions_onto_the_existing_selection() {
        let out = apply(Mode::Multiple, &set([9]), Some(2), 4, true, true).unwrap();
        assert_eq!(out.selection, set([2, 3, 4, 9]));
    }

    #[test]
    fn shift_without_an_anchor_degrades_to_a_plain_click() {
        let out = apply(Mode::Multiple, &set([]), None, 3, false, true).unwrap();
        assert_eq!(out.selection, set([3]));
    }

    #[test]
    fn a_no_op_click_reports_no_change() {
        // Clicking the only selected row again changes nothing.
        assert_eq!(apply(Mode::Single, &set([4]), Some(4), 4, false, false), None);
    }
}

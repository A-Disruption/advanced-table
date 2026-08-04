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

/// What a click should produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub selection: BTreeSet<usize>,
    /// The new anchor, for a later Shift-click to extend from.
    pub anchor: Option<usize>,
}

/// Resolve a click into a new selection.
///
/// Returns `None` when nothing should change, so the caller can skip emitting
/// a message rather than spamming identical updates.
pub fn apply(
    mode: Mode,
    current: &BTreeSet<usize>,
    anchor: Option<usize>,
    row: usize,
    toggle: bool,
    extend: bool,
) -> Option<Outcome> {
    let outcome = match mode {
        Mode::None => return None,

        Mode::Single => Outcome {
            selection: BTreeSet::from([row]),
            anchor: Some(row),
        },

        Mode::Multiple => {
            if extend {
                // Shift extends from the anchor. Without an anchor there is
                // nothing to extend from, so it degrades to a plain click.
                let start = anchor.unwrap_or(row);
                let range = (start.min(row)..=start.max(row)).collect::<BTreeSet<_>>();

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

                if !selection.remove(&row) {
                    selection.insert(row);
                }

                Outcome {
                    selection,
                    anchor: Some(row),
                }
            } else {
                Outcome {
                    selection: BTreeSet::from([row]),
                    anchor: Some(row),
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

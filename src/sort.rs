//! Sort state.
//!
//! Same ownership rule as [`selection`](crate::selection): the table renders
//! the sort you hand it and reports what the sort should become. It never
//! reorders anything itself.
//!
//! That is not laziness. A table that sorts its own rows can only sort the rows
//! it was given, which is wrong the moment the data is paged, filtered, or
//! ordered by the server -- and it has no idea how to compare your values
//! anyway, since it only ever sees the `Element` you built from them.

/// Which way a sorted column runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Ascending,
    Descending,
}

impl Direction {
    pub fn reverse(self) -> Self {
        match self {
            Direction::Ascending => Direction::Descending,
            Direction::Descending => Direction::Ascending,
        }
    }
}

/// The column currently sorted, and which way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    /// Leaf column index -- the same index the `rows` view function receives.
    pub column: usize,
    pub direction: Direction,
}

impl Sort {
    pub fn ascending(column: usize) -> Self {
        Self {
            column,
            direction: Direction::Ascending,
        }
    }

    pub fn descending(column: usize) -> Self {
        Self {
            column,
            direction: Direction::Descending,
        }
    }
}

/// What activating `column`'s sort control should produce.
///
/// Three states, not two: ascending, descending, then back to unsorted. Cutting
/// the third leaves no way to undo a sort short of reloading, and the unsorted
/// order is very often the meaningful one -- insertion order, relevance, a
/// ranking the server computed.
///
/// Moving to a different column always restarts at ascending rather than
/// carrying the previous direction over, because a descending sort inherited
/// from an unrelated column reads as the table ignoring the click.
pub fn next(current: Option<Sort>, column: usize) -> Option<Sort> {
    match current {
        Some(sort) if sort.column == column => match sort.direction {
            Direction::Ascending => Some(Sort::descending(column)),
            Direction::Descending => None,
        },
        _ => Some(Sort::ascending(column)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_column_cycles_through_three_states_and_back() {
        let a = next(None, 2);
        assert_eq!(a, Some(Sort::ascending(2)));

        let b = next(a, 2);
        assert_eq!(b, Some(Sort::descending(2)));

        // The third click clears it. Without this there is no way back to the
        // order the data arrived in.
        assert_eq!(next(b, 2), None);
    }

    #[test]
    fn switching_columns_restarts_at_ascending() {
        for from in [Sort::ascending(0), Sort::descending(0)] {
            assert_eq!(next(Some(from), 5), Some(Sort::ascending(5)));
        }
    }

    #[test]
    fn reverse_round_trips() {
        for direction in [Direction::Ascending, Direction::Descending] {
            assert_eq!(direction.reverse().reverse(), direction);
        }
    }
}

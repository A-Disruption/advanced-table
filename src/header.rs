//! Multi-level headers.
//!
//! The authoring API is a tree; the widget consumes a flat list. Groups are
//! presentation only -- the leaves are the real columns, and they own sizing,
//! ordering, and eventually sort/resize state.

use iced::alignment;
use iced::Element;

use crate::sizing::Sizing;

/// Everything the layout pass needs to know about one leaf column.
#[derive(Debug, Clone)]
pub struct ColumnSpec {
    pub sizing: Sizing,
    pub align: alignment::Horizontal,
    /// Draw a sort control in this column's header and let it be clicked.
    pub sortable: bool,
}

impl Default for ColumnSpec {
    fn default() -> Self {
        Self {
            sizing: Sizing::default(),
            align: alignment::Horizontal::Left,
            sortable: false,
        }
    }
}

impl ColumnSpec {
    pub fn sizing(mut self, sizing: Sizing) -> Self {
        self.sizing = sizing;
        self
    }

    pub fn align(mut self, align: alignment::Horizontal) -> Self {
        self.align = align;
        self
    }

    pub fn sortable(mut self, sortable: bool) -> Self {
        self.sortable = sortable;
        self
    }
}

/// A node in the header tree: either a group covering other nodes, or a leaf
/// that corresponds to an actual column of data.
pub struct HeaderNode<'a, Message, Theme, Renderer> {
    pub(crate) content: Element<'a, Message, Theme, Renderer>,
    pub(crate) kind: NodeKind<'a, Message, Theme, Renderer>,
    pub(crate) sticky: bool,
}

pub(crate) enum NodeKind<'a, Message, Theme, Renderer> {
    Group(Vec<HeaderNode<'a, Message, Theme, Renderer>>),
    Leaf(ColumnSpec),
}

/// A leaf header -- one real column.
pub fn leaf<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
) -> HeaderNode<'a, Message, Theme, Renderer> {
    HeaderNode {
        content: content.into(),
        kind: NodeKind::Leaf(ColumnSpec::default()),
        sticky: false,
    }
}

/// A group header spanning the columns beneath it.
pub fn group<'a, Message, Theme, Renderer>(
    content: impl Into<Element<'a, Message, Theme, Renderer>>,
    children: impl IntoIterator<Item = HeaderNode<'a, Message, Theme, Renderer>>,
) -> HeaderNode<'a, Message, Theme, Renderer> {
    HeaderNode {
        content: content.into(),
        kind: NodeKind::Group(children.into_iter().collect()),
        sticky: false,
    }
}

impl<'a, Message, Theme, Renderer> HeaderNode<'a, Message, Theme, Renderer> {
    /// Set the sizing policy. No-op on group nodes -- groups do not own width,
    /// their leaves do.
    pub fn sizing(mut self, sizing: Sizing) -> Self {
        if let NodeKind::Leaf(spec) = &mut self.kind {
            spec.sizing = sizing;
        }
        self
    }

    pub fn align(mut self, align: alignment::Horizontal) -> Self {
        if let NodeKind::Leaf(spec) = &mut self.kind {
            spec.align = align;
        }
        self
    }

    /// Give this column a sort control. No-op on a group -- a group spans
    /// several columns and there is no single ordering it could stand for.
    pub fn sortable(mut self) -> Self {
        if let NodeKind::Leaf(spec) = &mut self.kind {
            spec.sortable = true;
        }
        self
    }

    /// Pin this node against the leading edge so it stays put while the rest of
    /// the table scrolls sideways.
    ///
    /// Freezing is **positional**: a frozen region has to be attached to an
    /// edge, or there is no coherent place to draw it. So only an unbroken run
    /// of top-level nodes starting at the very first one is honoured -- see
    /// [`flatten`]. Marking a nested node, or one that comes after a
    /// non-sticky sibling, is ignored rather than producing a frozen island
    /// with scrolling columns on both sides of it.
    ///
    /// A sticky group takes every column beneath it, which is what keeps a
    /// group from being split down the middle by the frozen boundary.
    pub fn sticky(mut self) -> Self {
        self.sticky = true;
        self
    }

    pub fn fill(self, weight: u16) -> Self {
        self.sizing(Sizing::Fill { weight, min: 48.0 })
    }

    pub fn fixed(self, width: f32) -> Self {
        self.sizing(Sizing::Fixed(width))
    }
}

/// One header element after flattening, positioned in the header grid.
#[derive(Debug, Clone, Copy)]
pub struct HeaderCell {
    /// Index into the flattened header element list.
    pub element: usize,
    /// First leaf column covered, inclusive.
    pub start: usize,
    /// Last leaf column covered, inclusive.
    pub end: usize,
    /// Header row this cell's *label* sits on, 0 = topmost.
    pub row: usize,
    /// How many header rows the label band occupies.
    pub row_span: usize,
    /// Topmost row this cell owns.
    ///
    /// Differs from [`row`](Self::row) only for a group that was pushed down to
    /// meet shallower children: the blank band above it still belongs to it, so
    /// its dividers, its background and its hit box all start here rather than
    /// at the label. Without this the blank bands are owned by nobody, which is
    /// what leaves the header looking like a grid with pieces missing.
    pub top: usize,
    /// True for a real column. Deliberately not derived from `start == end` --
    /// a group with a single child covers one column and would be mistaken for
    /// the column itself.
    pub leaf: bool,
}

impl HeaderCell {
    pub fn is_leaf(&self) -> bool {
        self.leaf
    }

    /// Row just past the bottom of the label band.
    pub fn bottom(&self) -> usize {
        self.row + self.row_span
    }

    /// Does this cell own `row`? Counts the blank band above a group that was
    /// pushed down, which is why it starts at `top` rather than `row`.
    pub fn covers(&self, row: usize) -> bool {
        row >= self.top && row < self.bottom()
    }
}

/// The result of flattening the header tree.
pub struct Flattened<'a, Message, Theme, Renderer> {
    pub elements: Vec<Element<'a, Message, Theme, Renderer>>,
    pub cells: Vec<HeaderCell>,
    pub columns: Vec<ColumnSpec>,
    pub rows: usize,
    /// How many leading leaf columns are frozen. Always a prefix, so every
    /// header cell is either wholly inside it or wholly outside.
    pub sticky_columns: usize,
}

fn leaf_count<Message, Theme, Renderer>(node: &HeaderNode<'_, Message, Theme, Renderer>) -> usize {
    match &node.kind {
        NodeKind::Leaf(_) => 1,
        NodeKind::Group(children) => children.iter().map(leaf_count).sum(),
    }
}

/// Depth-first flatten. Leaf order becomes column order, which is what makes
/// the whole thing tractable: after this point nothing downstream knows or
/// cares that groups exist.
///
/// Groups are placed at `total_rows - subtree_depth`, which pushes each branch
/// **down** so it sits directly on top of its children and leaves the blank
/// space at the top. Placing by distance-from-root instead strands a group at
/// the top with an empty band between it and the columns it labels, which reads
/// as though the group belongs to some other level entirely.
///
/// Leaves are the other way round: a leaf claims every row from where its
/// parent stops all the way down to the body. An ungrouped column in a
/// three-level header is therefore *one* cell three rows tall, not a label on
/// the bottom band with two rows of unowned blank above it. That is what makes
/// its column rule run the full height of the header, its background cover the
/// full height, and a click anywhere in the stack select the column -- the
/// three things that separate a real data grid from a stack of labels.
pub fn flatten<'a, Message, Theme, Renderer>(
    nodes: Vec<HeaderNode<'a, Message, Theme, Renderer>>,
) -> Flattened<'a, Message, Theme, Renderer> {
    let depth = nodes.iter().map(max_depth).max().unwrap_or(1);

    // The frozen region is the leading *run* of sticky top-level nodes, counted
    // before the walk consumes them. Taking only the run -- rather than every
    // node that happens to be marked -- is what guarantees the result is a
    // prefix of the columns, and therefore that it has a leading edge to be
    // pinned against and never splits a group in half.
    let sticky_columns = nodes
        .iter()
        .take_while(|node| node.sticky)
        .map(leaf_count)
        .sum();

    let mut out = Flattened {
        elements: Vec::new(),
        cells: Vec::new(),
        columns: Vec::new(),
        rows: depth,
        sticky_columns,
    };

    for node in nodes {
        walk(node, depth, 0, &mut out);
    }

    out
}

fn max_depth<Message, Theme, Renderer>(node: &HeaderNode<'_, Message, Theme, Renderer>) -> usize {
    match &node.kind {
        NodeKind::Leaf(_) => 1,
        NodeKind::Group(children) => 1 + children.iter().map(max_depth).max().unwrap_or(0),
    }
}

/// `natural` is the row the node would sit on counting plainly down from the
/// root -- the row immediately below its parent's label.
fn walk<'a, Message, Theme, Renderer>(
    node: HeaderNode<'a, Message, Theme, Renderer>,
    total_rows: usize,
    natural: usize,
    out: &mut Flattened<'a, Message, Theme, Renderer>,
) -> (usize, usize) {
    let element = out.elements.len();
    // Bottom-anchored: a subtree three deep starts at the top, one deep sits
    // on the last row. Never above `natural`, or a shallow group would climb
    // past its own parent.
    let row = total_rows.saturating_sub(max_depth(&node)).max(natural);

    out.elements.push(node.content);

    match node.kind {
        NodeKind::Leaf(spec) => {
            let index = out.columns.len();
            out.columns.push(spec);
            out.cells.push(HeaderCell {
                element,
                start: index,
                end: index,
                // Leaves bottom out against the body rather than floating on
                // the last band, so the cell covers everything its parent
                // left over.
                row: natural,
                row_span: total_rows.saturating_sub(natural).max(1),
                top: natural,
                leaf: true,
            });
            (index, index)
        }
        NodeKind::Group(children) => {
            // Reserve the cell now so header elements stay in tree order,
            // then fill in the span once the children have claimed their
            // leaf indices.
            let slot = out.cells.len();
            out.cells.push(HeaderCell {
                element,
                start: 0,
                end: 0,
                row,
                row_span: 1,
                top: natural,
                leaf: false,
            });

            let mut start = usize::MAX;
            let mut end = 0;

            for child in children {
                // `total_rows` stays constant. The placement formula already
                // accounts for depth via the subtree height, so decrementing
                // here counts it twice and collapses every deeper level onto
                // row zero, stacking group labels on top of their own leaves.
                // Children hang off the row this group's *label* landed on,
                // not off `natural` -- a group pushed down to meet its
                // children has to take them with it.
                let (s, e) = walk(child, total_rows, row + 1, out);
                start = start.min(s);
                end = end.max(e);
            }

            if start == usize::MAX {
                // Empty group. Degenerate but not worth panicking over --
                // collapse it onto the next column boundary.
                start = out.columns.len();
                end = start;
            }

            out.cells[slot].start = start;
            out.cells[slot].end = end;
            (start, end)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests need a concrete Element, so they run against the default
    // iced Theme/Renderer with a unit Message.
    type El = iced::Element<'static, (), iced::Theme, iced::Renderer>;

    fn text_leaf(s: &'static str) -> HeaderNode<'static, (), iced::Theme, iced::Renderer> {
        leaf::<(), iced::Theme, iced::Renderer>(iced::widget::text(s))
    }

    #[test]
    fn flat_header_is_single_row() {
        let f = flatten(vec![text_leaf("A"), text_leaf("B")]);

        assert_eq!(f.rows, 1);
        assert_eq!(f.columns.len(), 2);
        assert!(f.cells.iter().all(|c| c.is_leaf() && c.row == 0 && c.row_span == 1));
    }

    #[test]
    fn group_spans_its_leaves_and_leaf_order_is_column_order() {
        let f = flatten(vec![
            text_leaf("Name"),
            group(
                iced::widget::text("Contact"),
                vec![text_leaf("Email"), text_leaf("Phone")],
            ),
        ]);

        assert_eq!(f.rows, 2);
        assert_eq!(f.columns.len(), 3);

        let group_cell = f.cells.iter().find(|c| !c.is_leaf()).unwrap();
        assert_eq!((group_cell.start, group_cell.end), (1, 2));

        // The group sits directly on top of its children, not at the root.
        assert_eq!(group_cell.row, 0);

        // The ungrouped leaf owns the whole header stack rather than sitting
        // on the last band under an orphaned blank.
        let name = f.cells.iter().find(|c| c.start == 0 && c.is_leaf()).unwrap();
        assert_eq!(name.row, 0);
        assert_eq!(name.row_span, 2);
    }

    #[test]
    fn sticky_is_the_leading_run_and_takes_whole_groups() {
        let f = flatten(vec![
            text_leaf("ID").sticky(),
            group(
                iced::widget::text("Contact"),
                vec![text_leaf("First"), text_leaf("Last")],
            )
            .sticky(),
            text_leaf("Revenue"),
            // Marked, but the run is already broken. Honouring this would
            // freeze a column with a scrolling one to its left, which has
            // nowhere coherent to be drawn.
            text_leaf("Actions").sticky(),
        ]);

        assert_eq!(f.sticky_columns, 3, "ID plus both of Contact's leaves");
    }

    #[test]
    fn sticky_is_ignored_below_the_top_level() {
        // Freezing an inner leaf would split its group across the boundary.
        let f = flatten(vec![group(
            iced::widget::text("Contact"),
            vec![text_leaf("First").sticky(), text_leaf("Last")],
        )]);

        assert_eq!(f.sticky_columns, 0);
    }

    #[test]
    fn nothing_is_sticky_by_default() {
        let f = flatten(vec![text_leaf("A"), text_leaf("B")]);
        assert_eq!(f.sticky_columns, 0);
    }

    #[test]
    fn a_single_child_group_is_not_mistaken_for_a_leaf() {
        // `start == end` is true of both, which is why the flag is explicit.
        // Getting this wrong aligns the group label over its one column like a
        // column header and lets a click on it fall through to the leaf.
        let f = flatten(vec![group(
            iced::widget::text("Solo"),
            vec![text_leaf("Only")],
        )]);

        let solo = f.cells.iter().find(|c| c.row == 0).unwrap();
        assert!(!solo.is_leaf());
        assert_eq!((solo.start, solo.end), (0, 0));

        let only = f.cells.iter().find(|c| c.row == 1).unwrap();
        assert!(only.is_leaf());
    }

    #[test]
    fn every_header_band_is_tiled_exactly_once() {
        // Two failures at once, both obvious on screen and neither obvious in
        // the code. Overlap: labels from different levels drawn over each
        // other. Gaps: blank bands owned by nobody, which is what leaves the
        // column rules stopping short and the group backgrounds ragged.
        let f = flatten(vec![
            text_leaf("ID"),
            group(
                iced::widget::text("Contact"),
                vec![text_leaf("First"), text_leaf("Last")],
            ),
            group(
                iced::widget::text("Financials"),
                vec![
                    group(
                        iced::widget::text("Q3"),
                        vec![text_leaf("Rev"), text_leaf("Cost")],
                    ),
                    group(
                        iced::widget::text("Q4"),
                        vec![text_leaf("Rev"), text_leaf("Cost")],
                    ),
                ],
            ),
            text_leaf("Actions"),
        ]);

        assert_eq!(f.rows, 3);
        assert_eq!(f.columns.len(), 8);

        for row in 0..f.rows {
            let mut band: Vec<_> = f.cells.iter().filter(|c| c.covers(row)).collect();
            band.sort_by_key(|c| c.start);

            for pair in band.windows(2) {
                assert!(
                    pair[0].end < pair[1].start,
                    "row {row}: cells {:?} and {:?} overlap",
                    (pair[0].start, pair[0].end),
                    (pair[1].start, pair[1].end),
                );
            }

            assert_eq!(
                band.iter().map(|c| c.end - c.start + 1).sum::<usize>(),
                f.columns.len(),
                "row {row} does not cover every column",
            );
        }

        // Every leaf reaches the body, whatever level it branched off at.
        assert!(f.cells.iter().filter(|c| c.is_leaf()).all(|c| c.bottom() == 3));

        // ID and Actions are ungrouped, so they are full-height cells.
        let id = f.cells.iter().find(|c| c.is_leaf() && c.start == 0).unwrap();
        assert_eq!((id.row, id.row_span), (0, 3));
    }

    #[test]
    fn a_shallow_group_is_pushed_down_to_meet_its_children() {
        // "Financials" is 3 deep, "Contact" only 2. Contact must sit on row 1,
        // directly above its leaves -- not on row 0 with a gap beneath it.
        let f = flatten(vec![
            text_leaf("ID"),
            group(
                iced::widget::text("Contact"),
                vec![text_leaf("First"), text_leaf("Last")],
            ),
            group(
                iced::widget::text("Financials"),
                vec![group(
                    iced::widget::text("Q3"),
                    vec![text_leaf("Rev"), text_leaf("Cost")],
                )],
            ),
        ]);

        assert_eq!(f.rows, 3);

        let contact = f.cells.iter().find(|c| c.start == 1 && c.end == 2).unwrap();
        assert_eq!(contact.row, 1, "shallow group must sit above its children");

        let financials = f.cells.iter().find(|c| c.start == 3 && c.end == 4).unwrap();
        assert_eq!(financials.row, 0);

        // The blank band above Contact still belongs to Contact, so nothing in
        // the header is unowned.
        assert_eq!(contact.top, 0);

        // Every leaf bottoms out against the body.
        for cell in f.cells.iter().filter(|c| c.is_leaf()) {
            assert_eq!(cell.bottom(), 3, "leaves bottom out against the body");
        }

        // First/Last hang off Contact's *label* row, not off the root.
        let first = f.cells.iter().find(|c| c.is_leaf() && c.start == 1).unwrap();
        assert_eq!((first.row, first.row_span), (2, 1));
    }

    #[test]
    fn three_levels_nest_correctly() {
        let f = flatten(vec![group(
            iced::widget::text("Financials"),
            vec![
                group(
                    iced::widget::text("Q3"),
                    vec![text_leaf("Rev"), text_leaf("Cost")],
                ),
                group(iced::widget::text("Q4"), vec![text_leaf("Rev")]),
            ],
        )]);

        assert_eq!(f.rows, 3);
        assert_eq!(f.columns.len(), 3);

        let outer = f.cells.iter().find(|c| c.row == 0).unwrap();
        assert_eq!((outer.start, outer.end), (0, 2));

        let q3 = f.cells.iter().find(|c| !c.is_leaf() && c.row == 1 && c.start == 0).unwrap();
        assert_eq!((q3.start, q3.end), (0, 1));

        // Q4 has only one leaf, and every leaf sits on the bottom row.
        let q4_leaf = f.cells.iter().find(|c| c.start == 2 && c.is_leaf()).unwrap();
        assert_eq!(q4_leaf.row, 2);
        assert_eq!(q4_leaf.row_span, 1);
    }
}
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
}

impl Default for ColumnSpec {
    fn default() -> Self {
        Self {
            sizing: Sizing::default(),
            align: alignment::Horizontal::Left,
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
}

/// A node in the header tree: either a group covering other nodes, or a leaf
/// that corresponds to an actual column of data.
pub struct HeaderNode<'a, Message, Theme, Renderer> {
    pub(crate) content: Element<'a, Message, Theme, Renderer>,
    pub(crate) kind: NodeKind<'a, Message, Theme, Renderer>,
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
    /// Header row this cell begins on, 0 = topmost.
    pub row: usize,
    /// How many header rows this cell occupies.
    pub row_span: usize,
}

impl HeaderCell {
    pub fn is_leaf(&self) -> bool {
        self.start == self.end
    }
}

/// The result of flattening the header tree.
pub struct Flattened<'a, Message, Theme, Renderer> {
    pub elements: Vec<Element<'a, Message, Theme, Renderer>>,
    pub cells: Vec<HeaderCell>,
    pub columns: Vec<ColumnSpec>,
    pub rows: usize,
}

/// Depth-first flatten. Leaf order becomes column order, which is what makes
/// the whole thing tractable: after this point nothing downstream knows or
/// cares that groups exist.
///
/// Every node is placed at `total_rows - subtree_depth`, which pushes each
/// branch **down** so it sits directly on top of its children and leaves the
/// blank space at the top. Placing by distance-from-root instead strands a
/// group at the top with an empty band between it and the columns it labels,
/// which reads as though the group belongs to some other level entirely.
///
/// A consequence is that every cell occupies exactly one row: children always
/// fill the row immediately below their parent, so nothing needs to span
/// downward to reach the body.
pub fn flatten<'a, Message, Theme, Renderer>(
    nodes: Vec<HeaderNode<'a, Message, Theme, Renderer>>,
) -> Flattened<'a, Message, Theme, Renderer> {
    let depth = nodes.iter().map(max_depth).max().unwrap_or(1);

    let mut out = Flattened {
        elements: Vec::new(),
        cells: Vec::new(),
        columns: Vec::new(),
        rows: depth,
    };

    for node in nodes {
        walk(node, depth, &mut out);
    }

    out
}

fn max_depth<Message, Theme, Renderer>(node: &HeaderNode<'_, Message, Theme, Renderer>) -> usize {
    match &node.kind {
        NodeKind::Leaf(_) => 1,
        NodeKind::Group(children) => 1 + children.iter().map(max_depth).max().unwrap_or(0),
    }
}

fn walk<'a, Message, Theme, Renderer>(
    node: HeaderNode<'a, Message, Theme, Renderer>,
    total_rows: usize,
    out: &mut Flattened<'a, Message, Theme, Renderer>,
) -> (usize, usize) {
    let element = out.elements.len();
    // Bottom-anchored: a subtree three deep starts at the top, one deep sits
    // on the last row.
    let row = total_rows.saturating_sub(max_depth(&node));

    out.elements.push(node.content);

    match node.kind {
        NodeKind::Leaf(spec) => {
            let index = out.columns.len();
            out.columns.push(spec);
            out.cells.push(HeaderCell {
                element,
                start: index,
                end: index,
                row,
                row_span: 1,
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
            });

            let mut start = usize::MAX;
            let mut end = 0;

            for child in children {
                // `total_rows` stays constant. The placement formula already
                // accounts for depth via the subtree height, so decrementing
                // here counts it twice and collapses every deeper level onto
                // row zero, stacking group labels on top of their own leaves.
                let (s, e) = walk(child, total_rows, out);
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
        assert_eq!(group_cell.row, 0);

        // The group sits directly on top of its children, not at the root.
        assert_eq!(group_cell.row, 0);

        // The ungrouped leaf drops to the bottom row alongside Email/Phone,
        // leaving the blank band above it rather than below.
        let name = f.cells.iter().find(|c| c.start == 0 && c.is_leaf()).unwrap();
        assert_eq!(name.row, 1);
        assert_eq!(name.row_span, 1);
    }

    #[test]
    fn no_two_cells_in_a_row_ever_overlap() {
        // The failure this guards against is subtle to read but obvious on
        // screen: labels from different levels stacked on the same band,
        // drawn over each other. It only shows up with three levels and a
        // shallow sibling, so the simpler tests above all pass without it.
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
            let mut band: Vec<_> = f.cells.iter().filter(|c| c.row == row).collect();
            band.sort_by_key(|c| c.start);

            for pair in band.windows(2) {
                assert!(
                    pair[0].end < pair[1].start,
                    "row {row}: cells {:?} and {:?} overlap",
                    (pair[0].start, pair[0].end),
                    (pair[1].start, pair[1].end),
                );
            }
        }

        // Exactly one cell on the top band, and every leaf on the last.
        assert_eq!(f.cells.iter().filter(|c| c.row == 0).count(), 1);
        assert!(f.cells.iter().filter(|c| c.is_leaf()).all(|c| c.row == 2));
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

        // Every leaf lands on the last row.
        for cell in f.cells.iter().filter(|c| c.is_leaf()) {
            assert_eq!(cell.row, 2, "leaves bottom out against the body");
            assert_eq!(cell.row_span, 1);
        }
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

        let q3 = f.cells.iter().find(|c| c.row == 1 && c.start == 0).unwrap();
        assert_eq!((q3.start, q3.end), (0, 1));

        // Q4 has only one leaf, and every leaf sits on the bottom row.
        let q4_leaf = f.cells.iter().find(|c| c.start == 2 && c.is_leaf()).unwrap();
        assert_eq!(q4_leaf.row, 2);
        assert_eq!(q4_leaf.row_span, 1);
    }
}
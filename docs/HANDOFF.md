# advanced-table — handoff

Context for picking this up cold. Two features remain: **drag-to-reorder columns**
and a **`#[derive(TableRow)]` proc macro**.

Written 2026-08-04. 49 tests passing, ~6k lines.

---

## 1. What exists

A custom `iced` `Widget` (not composed from stock widgets — column width is a
property of the *column*, so every row would otherwise run its own flex pass and
resolve `Fill` against its own content).

Pinned to `iced` git rev `4cad51ba61e3d76a7e696ad5923d675a20994e34`.

| Module | Lines | What it owns |
|---|---|---|
| `sizing.rs` | 394 | Pure width math. No iced types, fully unit tested. |
| `scroll.rs` | 307 | Offsets, scrollbar geometry, visible-row range. Unit tested. |
| `selection.rs` | 420 | Selection resolution for rows, columns **and** cells. `grid()` for clipboard. |
| `sort.rs` | 102 | Three-state sort cycle. |
| `header.rs` | 559 | Header tree → flat cells + leaf columns. Sticky run. |
| `style.rs` | 271 | `Catalog` pattern, ~30 style knobs. |
| `table.rs` | 3182 | The `Widget` impl. |
| `main.rs` | 659 | Visual test harness. `CHECKS` lists 20 things to eyeball. |

Working: multi-level headers, per-column sizing (`Fixed`/`Fit`/`Fill`), owned
scrolling with sticky header, column resize, three-state sorting, row/column/cell
selection with Ctrl/Shift/drag, keyboard arrow navigation, sticky (frozen) leading
columns, conditional per-cell styling, right-click reporting, Ctrl+C copy hook,
gutters.

---

## 2. Invariants to know before touching `draw` or `update`

These are the things that have actually caused bugs. Read this section before
editing rendering or input.

### 2.1 Ownership: the widget owns no selection state

Selection, sort and (soon) column order live in the **application**. The widget
renders what it is handed and publishes what the value *should become*. Only
ephemeral interaction state is widget-side: anchors, focus, drag, hover,
scroll offset, resize overrides.

Rationale is in the `selection.rs` module doc. Follow this for reordering too.

### 2.2 Layer / paint order — this has bitten three times

`renderer.with_layer` opens a sub-layer. Sub-layers render **in creation order**.
Drawing into the *base* layer after a sub-layer has closed still files the
primitive in the base layer, which renders **underneath everything**.

So anything that must sit on top needs **its own `with_layer`**. Currently:
scrollbars, the frozen seam, the outer frame.

Bugs this caused, in order: the outer border invisible except a fragment on
alternating rows; the frozen seam visible only on alternating rows.

### 2.3 Row dividers repaint the top edge of a row

A row divider is drawn at exactly the y of a row's top edge. Anything drawn there
*before* the divider loop gets painted over. This is why the selected-row outline
lives **after** the divider block — drawn before, only its bottom edge survived
(inset by one rule, so it cleared the next row's divider).

### 2.4 Content space vs screen space

Child layout nodes are positioned in **content space and never move**. Scrolling
translates the renderer, not the layout. Events therefore translate the *cursor*.

With frozen columns there are two mappings, so **never subtract `offset.x` by
hand**. Use the free functions in `table.rs`:

- `screen_x(content, bounds, state)`
- `content_x(x, bounds, state)`

Doing it by hand puts the frozen columns' resize handles and sort controls
wherever the scrolling ones happen to be.

### 2.5 Two bands, and each needs its own cursor *and* viewport

Body and header each paint twice: scrolling columns, then frozen columns over
them in their own clip with **no X translation**. That single difference is what
makes a column stick — same trick as the header's missing *Y* translation.

Each band derives its own `cursor` and `viewport` from `region` and `shift_x`.
Children clip themselves against the viewport, so handing both bands the
scrolling viewport makes frozen cells' *text* vanish a character at a time while
their backgrounds and hit boxes keep working (this exact bug happened).

Full-width furniture (stripes, highlights, dividers) is drawn in **both** bands
and cut down by the clip. Only the child elements are filtered by column range —
they are real widgets and drawing each twice is work, not just overdraw.

### 2.6 `invalidate_layout` does not request a redraw

`Shell::invalidate_layout` sets a dirty flag and nothing else. During a drag you
need **both** it and `request_redraw`. With only the former the whole gesture
schedules zero frames and appears frozen until some other event forces one.
With only the latter you paint stale geometry.

### 2.7 Column widths must not depend on row order

`measure_sample` samples rows to derive intrinsic widths. Two protections:

- **Stride sampling** — `step * row_count / sample`, not the first N rows.
  Head-sampling makes width a function of row order, so sorting resized columns.
- **Latch** — widths never shrink while the row *count* is unchanged. Keyed on
  row count so a sort holds the latch and a filter/reload re-measures.

### 2.8 Header cell geometry

`HeaderCell` has `row` (label band), `row_span`, `top` (topmost row it *owns*,
which differs from `row` for a group pushed down to meet shallow children) and an
explicit `leaf` flag (**not** `start == end` — a single-child group covers one
column and would be misread as that column).

Invariant, tested: every header band is tiled **exactly once** — no overlaps, no
gaps. Ungrouped columns are full-height cells.

### 2.9 Header gesture precedence

In `update`, in order:

1. **Resize** — within `resize_tolerance` (4px) of a column edge, and only where
   *both* adjacent leaves show their leaf headers. This is what keeps group
   labels from being a row of invisible drag handles.
2. **Sort** — the 16px `SORT_ZONE` at the trailing end of a sortable leaf.
3. **Column selection** — everything else in the header.

Reorder has to slot into this. See §3.

Resize also **pins every column width** at drag start; otherwise `Fill` columns
re-absorb the slack and the dragged edge never moves.

---

## 3. Feature: drag to reorder columns

Nothing implemented yet.

### Recommended shape

Keep the ownership rule: **the widget reports, the app reorders**. Publish
`on_reorder(from: usize, to: usize)` (leaf column indices) and let the app permute
its own `leaf()`/`group()` calls in `view`.

The alternative — a display-order permutation inside the widget — has to thread
through layout, draw, hit-testing, sticky bands and the `elements` Vec (which is
row-major by logical column index). Not worth it.

### Decisions needed

1. **Scope of a move.** Suggest allowing reorder only **within the same parent
   group**, plus reordering of top-level nodes. Dragging a leaf out of its group
   is a tree edit with no obvious correct result, and it can break the "sticky is
   a leading run of whole top-level nodes" invariant.
2. **Frozen boundary.** Suggest forbidding drags that cross it initially —
   otherwise a drop silently changes what is frozen.
3. **Drag threshold.** Needs ~4px of movement before it becomes a reorder,
   or every column-selection click starts one.
4. **Drop indicator.** Vertical line at the insertion point. **Own layer** (§2.2).
5. **Interaction with resize.** Resize is tested first and wins near edges. A
   reorder drag should start only outside the resize tolerance *and* outside the
   sort zone.

### Where the code goes

- `header.rs` — nothing, if the app owns the order.
- `table.rs` — a `DragKind::Reorder` variant alongside the existing sweep
  machinery in `State.drag`; hit-test via `header_cell_at`; indicator in `draw`.
- `style.rs` — a `reorder_indicator: Option<Color>` knob.

---

## 4. Feature: `#[derive(TableRow)]`

Target ergonomics (user's own sketch):

```rust
#[derive(TableRow)]
struct Contact {
    first_name: String,                              // no group
    #[table(group = "Contact Info")]
    email: String,
    #[table(group = "Contact Info")]
    phone_number: String,
    #[table(group = "Address / Street", fill = 2)]
    street: String,
    #[table(group = "Address / City")]
    city: String,
}
```

then something like `table(&contacts)` to get a populated `DataTable`.

### Crate layout

Proc-macro crates can only export macros, so this needs a **workspace**:

```
advanced-table/            # workspace root
  advanced-table/          # the widget (current src/)
  advanced-table-derive/   # proc-macro crate
```

The widget crate re-exports the derive behind a `derive` feature, the usual
`serde`/`serde_derive` arrangement.

### Trait shape

The key problem is that `HeaderNode` and `Element` are generic over
`Message`/`Theme`/`Renderer`, and a derived impl cannot know them. Solution: make
the **trait** generic. Derived cells are non-interactive `text(...)`, which
produces no messages and so works for *any* `Message`:

```rust
pub trait TableRow<Message, Theme = iced::Theme, Renderer = iced::Renderer> {
    fn headers<'a>() -> Vec<HeaderNode<'a, Message, Theme, Renderer>>;
    fn cell<'a>(&'a self, column: usize) -> Element<'a, Message, Theme, Renderer>;
}
```

Generated impl carries the bounds `text` needs:

```rust
impl<Message, Theme, Renderer> TableRow<Message, Theme, Renderer> for Contact
where
    Theme: iced::widget::text::Catalog,
    Renderer: iced::advanced::text::Renderer,
```

### Construction needs no macro

Only the *derive* has to be a proc macro. The constructor is a plain function in
the widget crate:

```rust
pub fn table<'a, T, Message, Theme, Renderer>(
    rows: &'a [T],
) -> DataTable<'a, Message, Theme, Renderer>
where
    T: TableRow<Message, Theme, Renderer>,
{
    DataTable::new(T::headers()).rows(rows, |item, column| item.cell(column))
}
```

Prefer this over a `table![...]` macro_rules — better errors, better IDE support.

### Attributes to support

Map onto the existing builders: `group`, `fill = n`, `fixed = n`, `align`,
`sortable`, `sticky`, `skip`, `header = "..."` (label override, default is the
field name title-cased), and `format = "..."` or `with = path` for values that
are not `Display`.

### Open questions

1. **`"Address / Street"` — path or literal?** The header tree supports arbitrary
   nesting, so reading `/` as a path is natural and is *probably* the intent:
   `Address > Street > street` and `Address > City > city` as sibling subgroups.
   But it could equally be two flat groups literally named that. **Ask before
   building.** If path syntax wins, needs an escape for literal slashes.
2. **Grouping is by consecutive run.** Fields with the same group name that are
   *not* adjacent cannot form one group without reordering the columns. Decide:
   compile error, or silently split into two groups?
3. **Cell values.** Default to `text(field.to_string())`, requiring `Display`.
   `Option<T>` should probably render empty rather than `"None"`. Non-`Display`
   fields need `with = path` or `skip`.
4. **Interactive columns** (the demo's "Open" button) can't be derived. Needs an
   escape hatch — likely a `#[table(skip)]` plus a manual `.row()` append, or an
   `extra_columns` hook.

---

## 5. Dev workflow notes

- Build/test: `cargo test --lib`, `cargo build`. Demo is `src/main.rs`.
- **The running demo locks `advanced-table.exe`** — `cargo build` fails with
  "Access is denied (os error 5)" until the process is killed. Kill, build,
  relaunch:
  ```powershell
  Get-Process -Name "advanced-table" -ErrorAction SilentlyContinue | Stop-Process -Force
  Start-Sleep -Milliseconds 500
  cargo build
  Start-Process -FilePath "C:\Rust Projects\advanced-table\target\debug\advanced-table.exe"
  ```
- Claude's computer-use `request_access` resolves the app as **`advanced-table.exe`**,
  not `advanced-table` or the window title. `open_application` launches a *second*
  instance rather than fronting the existing one — use PowerShell
  `SetForegroundWindow` instead.
- The user's display is ultrawide; screenshots downsample ~3.3x, so 1px rules and
  small text are **not** verifiable from a screenshot. Verify fine visual detail
  by reasoning about the code, or ask the user to look.
- `src/main.rs` `CHECKS` is a numbered list of 20 things to verify by eye. Add to
  it when adding a feature.

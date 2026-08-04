//! Column width resolution.
//!
//! Deliberately free of any `iced` types. Everything here is `f32` in, `f32`
//! out, which means it is unit-testable without a renderer and survives any
//! future change to the widget internals.

/// How a single leaf column claims horizontal space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sizing {
    /// Exact width. Never grows, never shrinks, never absorbs slack.
    Fixed(f32),
    /// Widest measured content, clamped to `[min, max]`.
    Fit { min: f32, max: f32 },
    /// Fits its content, then takes a weighted share of leftover space.
    ///
    /// `min` is a shrink floor, not a starting width. A `Fill` column still
    /// has to fit what is in it -- "take a share of the leftover" is not
    /// "collapse to nothing when there is no leftover".
    Fill { weight: u16, min: f32 },
}

impl Sizing {
    /// The floor this column may never be shrunk below.
    pub fn min_width(&self) -> f32 {
        match self {
            Sizing::Fixed(w) => *w,
            Sizing::Fit { min, .. } => *min,
            Sizing::Fill { min, .. } => *min,
        }
    }

    pub fn is_fixed(&self) -> bool {
        matches!(self, Sizing::Fixed(_))
    }
}

impl Default for Sizing {
    fn default() -> Self {
        Sizing::Fit {
            min: 48.0,
            max: f32::INFINITY,
        }
    }
}

/// What to do when the resolved columns are wider than the space available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overflow {
    /// Let the content exceed the viewport. The caller scrolls or clips.
    ///
    /// This is the default on purpose: shrinking every column at once
    /// truncates text everywhere and reads as broken, whereas a horizontal
    /// scroll reads as intentional. It also keeps `Fixed` meaning fixed.
    #[default]
    Scroll,
    /// Shrink non-`Fixed` columns toward their minimums, proportional to how
    /// much headroom each one has above its own minimum.
    Shrink,
}

/// A header cell that covers more than one leaf column and therefore may force
/// those leaves to grow.
#[derive(Debug, Clone, Copy)]
pub struct SpanRequest {
    /// First leaf column covered, inclusive.
    pub start: usize,
    /// Last leaf column covered, inclusive.
    pub end: usize,
    /// Intrinsic width of the header element sitting on top of the span.
    pub needed: f32,
}

/// Grow leaf intrinsics so that every group header fits over its children.
///
/// This is the colspan problem: a header reading "Q3 Financials (Restated)"
/// spanning two narrow numeric columns has to push those columns wider, or the
/// header clips and the body stops lining up with it.
///
/// Spans are processed **narrowest first** so that a group nested inside
/// another group settles before the outer group measures its children. Reverse
/// that order and the outer group distributes against stale numbers, which
/// shows up as a visible drift between the header and the body.
///
/// The deficit is spread *proportionally* to current width rather than
/// equally, so a 40px status column next to a 200px name column stays narrow
/// instead of being dragged up to match it.
pub fn apply_span_constraints(
    intrinsic: &mut [f32],
    spans: &[SpanRequest],
    sizing: &[Sizing],
    spacing: f32,
) {
    debug_assert_eq!(intrinsic.len(), sizing.len());

    let mut ordered: Vec<&SpanRequest> = spans.iter().filter(|s| s.end > s.start).collect();
    ordered.sort_by_key(|s| s.end - s.start);

    for span in ordered {
        let range = span.start..=span.end;
        let count = span.end - span.start + 1;
        let gaps = spacing * (count.saturating_sub(1)) as f32;
        let current: f32 = intrinsic[range.clone()].iter().sum::<f32>() + gaps;

        if span.needed <= current {
            continue;
        }

        // Fixed columns opted out of being resized. If the whole span is
        // fixed there is nothing to give, and the header will have to clip.
        let growable: Vec<usize> = range.clone().filter(|&i| !sizing[i].is_fixed()).collect();

        if growable.is_empty() {
            continue;
        }

        let deficit = span.needed - current;
        let base: f32 = growable.iter().map(|&i| intrinsic[i]).sum();

        for &i in &growable {
            let share = if base > 0.0 {
                intrinsic[i] / base
            } else {
                1.0 / growable.len() as f32
            };
            intrinsic[i] += deficit * share;
        }
    }
}

/// Apply user resize overrides on top of the declared sizing policy.
///
/// A width the user dragged to is a `Fixed` width for every later pass: it
/// must not be re-derived from content, must not absorb slack, and must not be
/// grown by a group header above it. Folding overrides into the `Sizing` list
/// rather than special-casing them downstream means all three fall out for
/// free, because `Fixed` already has exactly those properties.
pub fn with_overrides(sizing: &[Sizing], overrides: &[Option<f32>]) -> Vec<Sizing> {
    sizing
        .iter()
        .enumerate()
        .map(|(i, spec)| match overrides.get(i).copied().flatten() {
            Some(width) => Sizing::Fixed(width),
            None => *spec,
        })
        .collect()
}

/// Turn measured intrinsics into final column widths.
///
/// Two passes: everything that does not depend on leftover space resolves
/// first, then `Fill` columns split whatever is left by weight. This is what
/// makes slack land where the author asked for it rather than in column zero.
pub fn resolve_widths(
    sizing: &[Sizing],
    intrinsic: &[f32],
    available: f32,
    spacing: f32,
    overflow: Overflow,
) -> Vec<f32> {
    debug_assert_eq!(sizing.len(), intrinsic.len());

    let count = sizing.len();
    if count == 0 {
        return Vec::new();
    }

    let gaps = spacing * (count - 1) as f32;
    let mut widths = vec![0.0f32; count];
    let mut fill_weight: u32 = 0;

    // Pass A -- independent of leftover space.
    for (i, spec) in sizing.iter().enumerate() {
        match *spec {
            Sizing::Fixed(w) => widths[i] = w,
            Sizing::Fit { min, max } => widths[i] = intrinsic[i].clamp(min, max),
            Sizing::Fill { weight, min } => {
                // Content-based floor. Starting Fill columns at `min` made
                // them silently absorb every shortfall: under a narrow
                // viewport they collapsed to 48px, total width never exceeded
                // the viewport, slack was never negative, and so `Overflow`
                // had nothing left to act on.
                widths[i] = intrinsic[i].max(min);
                fill_weight += weight.max(1) as u32;
            }
        }
    }

    let slack = available - (widths.iter().sum::<f32>() + gaps);

    // Pass B -- hand out (or claw back) the difference.
    if slack > 0.0 && fill_weight > 0 {
        for (i, spec) in sizing.iter().enumerate() {
            if let Sizing::Fill { weight, .. } = *spec {
                widths[i] += slack * (weight.max(1) as f32 / fill_weight as f32);
            }
        }
    } else if slack < 0.0 && overflow == Overflow::Shrink {
        let headroom: Vec<f32> = sizing
            .iter()
            .enumerate()
            .map(|(i, spec)| {
                if spec.is_fixed() {
                    0.0
                } else {
                    (widths[i] - spec.min_width()).max(0.0)
                }
            })
            .collect();

        let total: f32 = headroom.iter().sum();

        if total > 0.0 {
            let deficit = (-slack).min(total);
            for i in 0..count {
                widths[i] -= deficit * (headroom[i] / total);
            }
        }
    }

    widths
}

/// Left edge of each column, given resolved widths.
pub fn offsets(widths: &[f32], spacing: f32) -> Vec<f32> {
    let mut x = 0.0;
    widths
        .iter()
        .map(|w| {
            let here = x;
            x += w + spacing;
            here
        })
        .collect()
}

/// Total width covered by columns `start..=end`, gaps included.
pub fn span_width(widths: &[f32], start: usize, end: usize, spacing: f32) -> f32 {
    let count = end.saturating_sub(start) + 1;
    widths[start..=end].iter().sum::<f32>() + spacing * count.saturating_sub(1) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn slack_goes_to_fill_columns_by_weight() {
        let sizing = [
            Sizing::Fixed(60.0),
            Sizing::Fit { min: 40.0, max: 400.0 },
            Sizing::Fill { weight: 1, min: 80.0 },
            Sizing::Fill { weight: 3, min: 80.0 },
        ];
        let widths = resolve_widths(&sizing, &[60.0, 120.0, 100.0, 100.0], 1000.0, 10.0, Overflow::Scroll);

        assert!(approx(widths[0], 60.0));
        assert!(approx(widths[1], 120.0));
        // Fill columns start at their content width (100), not their min (80),
        // then split the remaining 590 as 1:3.
        assert!(approx(widths[2], 247.5));
        assert!(approx(widths[3], 542.5));
        assert!(approx(widths.iter().sum::<f32>() + 30.0, 1000.0));
    }

    #[test]
    fn fill_columns_do_not_collapse_under_a_narrow_viewport() {
        let sizing = [
            Sizing::Fill { weight: 1, min: 48.0 },
            Sizing::Fill { weight: 2, min: 48.0 },
        ];
        // Content needs 600px total; only 200px is on offer.
        let widths = resolve_widths(&sizing, &[250.0, 350.0], 200.0, 0.0, Overflow::Scroll);

        // Scroll keeps content width so there is something to scroll to.
        assert!(approx(widths[0], 250.0));
        assert!(approx(widths[1], 350.0));
        assert!(widths.iter().sum::<f32>() > 200.0);
    }

    #[test]
    fn shrink_compresses_fill_columns_toward_their_min() {
        let sizing = [
            Sizing::Fill { weight: 1, min: 48.0 },
            Sizing::Fill { weight: 2, min: 48.0 },
        ];
        let widths = resolve_widths(&sizing, &[250.0, 350.0], 200.0, 0.0, Overflow::Shrink);

        assert!(widths[0] >= 48.0 && widths[1] >= 48.0);
        assert!(approx(widths.iter().sum::<f32>(), 200.0));
    }

    #[test]
    fn fit_clamps_at_max_and_overflows_by_default() {
        let sizing = [
            Sizing::Fit { min: 40.0, max: 100.0 },
            Sizing::Fit { min: 40.0, max: 100.0 },
        ];
        let widths = resolve_widths(&sizing, &[500.0, 500.0], 80.0, 10.0, Overflow::Scroll);
        assert!(approx(widths[0], 100.0) && approx(widths[1], 100.0));
    }

    #[test]
    fn shrink_respects_minimums_and_leaves_fixed_alone() {
        let sizing = [
            Sizing::Fixed(100.0),
            Sizing::Fit { min: 50.0, max: 400.0 },
            Sizing::Fit { min: 50.0, max: 400.0 },
        ];
        let widths = resolve_widths(&sizing, &[100.0, 300.0, 300.0], 300.0, 0.0, Overflow::Shrink);

        assert!(approx(widths[0], 100.0));
        assert!(widths[1] >= 50.0 && widths[2] >= 50.0);
        assert!(approx(widths.iter().sum::<f32>(), 300.0));
    }

    #[test]
    fn group_header_grows_children_proportionally() {
        let sizing = [Sizing::default(), Sizing::default()];
        let mut intrinsic = [40.0, 120.0];
        let spans = [SpanRequest { start: 0, end: 1, needed: 300.0 }];

        apply_span_constraints(&mut intrinsic, &spans, &sizing, 10.0);

        assert!(approx(intrinsic.iter().sum::<f32>() + 10.0, 300.0));
        // 1:3 ratio preserved -- the narrow column stays narrow
        assert!(approx(intrinsic[1] / intrinsic[0], 3.0));
    }

    #[test]
    fn nested_spans_settle_inner_first() {
        let sizing = [Sizing::default(); 4];
        let mut intrinsic = [50.0, 50.0, 50.0, 50.0];
        let spans = [
            SpanRequest { start: 0, end: 3, needed: 500.0 },
            SpanRequest { start: 0, end: 1, needed: 400.0 },
        ];

        apply_span_constraints(&mut intrinsic, &spans, &sizing, 10.0);

        // inner span satisfied exactly
        assert!(approx(intrinsic[0] + intrinsic[1] + 10.0, 400.0));
        // outer span already satisfied as a consequence, so no further growth
        assert!(intrinsic.iter().sum::<f32>() + 30.0 >= 500.0);
        assert!(approx(intrinsic[2], 50.0) && approx(intrinsic[3], 50.0));
    }

    #[test]
    fn all_fixed_span_is_skipped_cleanly() {
        let sizing = [Sizing::Fixed(80.0), Sizing::Fixed(80.0)];
        let mut intrinsic = [80.0, 80.0];
        let spans = [SpanRequest { start: 0, end: 1, needed: 500.0 }];

        apply_span_constraints(&mut intrinsic, &spans, &sizing, 10.0);

        assert_eq!(intrinsic, [80.0, 80.0]);
    }

    #[test]
    fn an_override_behaves_exactly_like_fixed() {
        let declared = [
            Sizing::Fill { weight: 1, min: 48.0 },
            Sizing::Fit { min: 40.0, max: 400.0 },
        ];
        let sizing = with_overrides(&declared, &[Some(120.0), None]);

        assert_eq!(sizing[0], Sizing::Fixed(120.0));
        assert_eq!(sizing[1], declared[1]);

        // It must not absorb slack...
        let widths = resolve_widths(&sizing, &[300.0, 100.0], 1000.0, 0.0, Overflow::Scroll);
        assert!(approx(widths[0], 120.0));

        // ...nor be grown by a group header spanning it.
        let mut intrinsic = [120.0, 100.0];
        let spans = [SpanRequest { start: 0, end: 1, needed: 800.0 }];
        apply_span_constraints(&mut intrinsic, &spans, &sizing, 0.0);
        assert!(approx(intrinsic[0], 120.0));
        assert!(intrinsic[1] > 100.0);
    }

    #[test]
    fn zero_intrinsic_span_splits_equally() {
        let sizing = [Sizing::default(); 3];
        let mut intrinsic = [0.0, 0.0, 0.0];
        let spans = [SpanRequest { start: 0, end: 2, needed: 300.0 }];

        apply_span_constraints(&mut intrinsic, &spans, &sizing, 0.0);

        assert!(intrinsic.iter().all(|w| approx(*w, 100.0)));
    }
}
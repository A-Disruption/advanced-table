//! Scroll offsets and scrollbar geometry.
//!
//! The mapping between scroll offset and thumb position is the part worth
//! isolating. Once a thumb has a minimum length, the range it travels is
//! `track - thumb`, **not** `track`. Using `track` is the standard hand-rolled
//! scrollbar bug: on a long list the thumb runs past the end of its track and
//! visually detaches from the cursor over the last stretch of the drag.

use iced::{Point, Rectangle, Size, Vector};

/// Below this, a proportional thumb becomes impossible to grab.
pub const MIN_THUMB: f32 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Policy {
    /// Scrollbar is always present once content overflows.
    #[default]
    Auto,
    /// Never show a scrollbar on this axis; content is clipped.
    Hidden,
}

#[derive(Debug, Clone, Copy)]
pub struct Scrollbar {
    pub track: Rectangle,
    pub thumb: Rectangle,
}

impl Scrollbar {
    pub fn contains(&self, point: Point) -> bool {
        self.track.contains(point)
    }

    pub fn thumb_contains(&self, point: Point) -> bool {
        self.thumb.contains(point)
    }
}

/// How far the content can scroll on each axis.
pub fn max_offset(viewport: Size, content: Size) -> Vector {
    Vector::new(
        (content.width - viewport.width).max(0.0),
        (content.height - viewport.height).max(0.0),
    )
}

pub fn clamp_offset(offset: Vector, viewport: Size, content: Size) -> Vector {
    let max = max_offset(viewport, content);

    Vector::new(offset.x.clamp(0.0, max.x), offset.y.clamp(0.0, max.y))
}

/// Length of the thumb along the scrolling axis.
pub fn thumb_length(track: f32, viewport: f32, content: f32) -> f32 {
    if content <= viewport || content <= 0.0 {
        return track;
    }

    (track * (viewport / content)).max(MIN_THUMB).min(track)
}

/// Distance of the thumb's leading edge from the start of the track.
pub fn thumb_position(track: f32, viewport: f32, content: f32, offset: f32) -> f32 {
    let thumb = thumb_length(track, viewport, content);
    let travel = track - thumb;
    let max = (content - viewport).max(0.0);

    if travel <= 0.0 || max <= 0.0 {
        0.0
    } else {
        travel * (offset / max)
    }
}

/// Inverse of [`thumb_position`], clamped to the legal offset range.
pub fn offset_from_thumb(track: f32, viewport: f32, content: f32, position: f32) -> f32 {
    let thumb = thumb_length(track, viewport, content);
    let travel = track - thumb;
    let max = (content - viewport).max(0.0);

    if travel <= 0.0 {
        0.0
    } else {
        (max * (position / travel)).clamp(0.0, max)
    }
}

/// Which rows fall inside the viewport. Cheaper than testing every row's
/// rectangle, and the basis for real virtualization later.
pub fn visible_rows(offset_y: f32, viewport_height: f32, row_height: f32, count: usize) -> (usize, usize) {
    if row_height <= 0.0 {
        return (0, count);
    }

    let first = (offset_y / row_height).floor().max(0.0) as usize;
    let last = (((offset_y + viewport_height) / row_height).floor() as usize + 1).min(count);

    (first.min(count), last)
}

/// Which row sits under a point, in screen coordinates.
///
/// `body` is the region below the header. The point is converted into content
/// space by subtracting the region origin and adding the scroll offset -- the
/// exact inverse of the translation `draw` applies, which is why the two stay
/// consistent when scrolled.
pub fn row_at(
    point: Point,
    body: Rectangle,
    offset_y: f32,
    row_height: f32,
    count: usize,
) -> Option<usize> {
    if row_height <= 0.0 || count == 0 || !body.contains(point) {
        return None;
    }

    let content_y = point.y - body.y + offset_y;

    if content_y < 0.0 {
        return None;
    }

    let row = (content_y / row_height).floor() as usize;

    (row < count).then_some(row)
}

/// Build scrollbar geometry for the body region.
///
/// `body` is the region *below* the header -- the header does not scroll
/// vertically, so it must not be part of the vertical track.
pub fn scrollbars(
    body: Rectangle,
    content: Size,
    offset: Vector,
    width: f32,
    vertical: Policy,
    horizontal: Policy,
) -> (Option<Scrollbar>, Option<Scrollbar>) {
    let needs_h = horizontal == Policy::Auto && content.width > body.width;
    let track_height = body.height - if needs_h { width } else { 0.0 };
    let needs_v = vertical == Policy::Auto && content.height > track_height;
    let track_width = body.width - if needs_v { width } else { 0.0 };
    let viewport = Size::new(body.width, track_height);

    let v = needs_v.then(|| {
        let length = thumb_length(track_height, viewport.height, content.height);
        let start = thumb_position(track_height, viewport.height, content.height, offset.y);

        Scrollbar {
            track: Rectangle {
                x: body.x + track_width,
                y: body.y,
                width,
                height: track_height,
            },
            thumb: Rectangle {
                x: body.x + track_width,
                y: body.y + start,
                width,
                height: length,
            },
        }
    });

    let h = needs_h.then(|| {
        let length = thumb_length(track_width, viewport.width, content.width);
        let start = thumb_position(track_width, viewport.width, content.width, offset.x);

        Scrollbar {
            track: Rectangle {
                x: body.x,
                y: body.y + track_height,
                width: track_width,
                height: width,
            },
            thumb: Rectangle {
                x: body.x + start,
                y: body.y + track_height,
                width: length,
                height: width,
            },
        }
    });

    (v, h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    const TRACK: f32 = 400.0;
    const VIEW: f32 = 300.0;
    const CONTENT: f32 = 40_000.0;

    #[test]
    fn thumb_respects_minimum_length() {
        // Proportional would be 3px -- unusable.
        assert!(approx(thumb_length(TRACK, VIEW, CONTENT), MIN_THUMB));
    }

    #[test]
    fn fully_scrolled_puts_thumb_flush_with_track_end() {
        let max = CONTENT - VIEW;
        let pos = thumb_position(TRACK, VIEW, CONTENT, max);
        let len = thumb_length(TRACK, VIEW, CONTENT);

        // This is the assertion that catches the travel-range bug: using
        // `track` instead of `track - thumb` overshoots by ~thumb_len.
        assert!(approx(pos + len, TRACK));
    }

    /// The same property, but end to end through the geometry the widget
    /// actually draws -- which is where it was being lost.
    ///
    /// `fully_scrolled_puts_thumb_flush_with_track_end` passes whatever
    /// `scrollbars` hands the mapping, because it does the handing itself. The
    /// scroll range is set by `clamp_offset` against the *viewport*, so that is
    /// what the thumb has to be measured against too; measured against its own
    /// track instead, the horizontal thumb stops one bar-width's worth of
    /// travel short of the end and never quite gets there.
    #[test]
    fn both_thumbs_reach_both_ends_of_their_tracks() {
        const WIDTH: f32 = 10.0;

        let bars = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 500.0,
            height: 300.0,
        };

        // Barely overflowing on each axis, which is the worst case: the gap is
        // a fraction of the *travel*, so the less there is to scroll the more
        // of the track the thumb fails to cover.
        let content = Size::new(560.0, 360.0);

        // Exactly what `layout` clamps the offset against: the full width (the
        // vertical bar is an overlay), minus the horizontal bar's reserved
        // strip on the height.
        let viewport = Size::new(bars.width, bars.height - WIDTH);
        let max = max_offset(viewport, content);

        let bar = |offset| {
            let (v, h) = scrollbars(bars, content, offset, WIDTH, Policy::Auto, Policy::Auto);

            (v.expect("vertical bar"), h.expect("horizontal bar"))
        };

        let (v, h) = bar(Vector::new(0.0, 0.0));
        assert!(approx(v.thumb.y, v.track.y), "vertical thumb off the top");
        assert!(approx(h.thumb.x, h.track.x), "horizontal thumb off the left");

        let (v, h) = bar(max);
        assert!(
            approx(v.thumb.y + v.thumb.height, v.track.y + v.track.height),
            "vertical thumb short of the bottom"
        );
        assert!(
            approx(h.thumb.x + h.thumb.width, h.track.x + h.track.width),
            "horizontal thumb short of the right"
        );
    }

    #[test]
    fn offset_and_thumb_round_trip() {
        let max = CONTENT - VIEW;

        for fraction in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let offset = max * fraction;
            let back = offset_from_thumb(
                TRACK,
                VIEW,
                CONTENT,
                thumb_position(TRACK, VIEW, CONTENT, offset),
            );
            assert!(approx(back, offset), "{fraction}: {offset} -> {back}");
        }
    }

    #[test]
    fn dragging_past_either_end_clamps() {
        let max = CONTENT - VIEW;

        assert!(approx(offset_from_thumb(TRACK, VIEW, CONTENT, -500.0), 0.0));
        assert!(approx(offset_from_thumb(TRACK, VIEW, CONTENT, 9999.0), max));
    }

    #[test]
    fn content_smaller_than_viewport_is_inert() {
        assert!(approx(thumb_length(400.0, 500.0, 400.0), 400.0));
        assert_eq!(max_offset(Size::new(500.0, 500.0), Size::new(400.0, 400.0)), Vector::ZERO);
        assert!(approx(offset_from_thumb(400.0, 500.0, 400.0, 999.0), 0.0));
    }

    #[test]
    fn visible_row_range_is_bounded() {
        assert_eq!(visible_rows(0.0, 300.0, 30.0, 1000), (0, 11));
        assert_eq!(visible_rows(305.0, 300.0, 30.0, 1000), (10, 21));
        assert_eq!(visible_rows(29_999.0, 300.0, 30.0, 1000), (999, 1000));
    }

    #[test]
    fn row_at_matches_the_drawn_position_while_scrolled() {
        let body = Rectangle { x: 0.0, y: 100.0, width: 400.0, height: 300.0 };

        // Unscrolled: first row starts at the top of the body.
        assert_eq!(row_at(Point::new(10.0, 105.0), body, 0.0, 30.0, 1000), Some(0));
        assert_eq!(row_at(Point::new(10.0, 135.0), body, 0.0, 30.0, 1000), Some(1));

        // Scrolled by exactly ten rows: the row under the top edge is row 10.
        assert_eq!(row_at(Point::new(10.0, 105.0), body, 300.0, 30.0, 1000), Some(10));

        // Consistent with what draw() would have painted there.
        let (first, _) = visible_rows(300.0, body.height, 30.0, 1000);
        assert_eq!(first, 10);
    }

    #[test]
    fn row_at_rejects_points_outside_the_body_or_past_the_end() {
        let body = Rectangle { x: 0.0, y: 100.0, width: 400.0, height: 300.0 };

        // In the header, above the body.
        assert_eq!(row_at(Point::new(10.0, 50.0), body, 0.0, 30.0, 1000), None);
        // Below the body.
        assert_eq!(row_at(Point::new(10.0, 500.0), body, 0.0, 30.0, 1000), None);
        // Past the last row: only 5 rows exist.
        assert_eq!(row_at(Point::new(10.0, 390.0), body, 0.0, 30.0, 5), None);
        // Degenerate.
        assert_eq!(row_at(Point::new(10.0, 105.0), body, 0.0, 0.0, 5), None);
    }

    #[test]
    fn each_bar_shortens_the_other_track() {
        let body = Rectangle { x: 0.0, y: 0.0, width: 200.0, height: 200.0 };
        let (v, h) = scrollbars(
            body,
            Size::new(500.0, 500.0),
            Vector::ZERO,
            10.0,
            Policy::Auto,
            Policy::Auto,
        );

        let v = v.expect("vertical bar");
        let h = h.expect("horizontal bar");

        assert!(approx(v.track.height, 190.0));
        assert!(approx(h.track.width, 190.0));
    }

    /// The other half of "each bar eats into the other": content that fits the
    /// body but not what is left of it once the horizontal bar has taken its
    /// strip is scrollable, so it has to get a bar. Measured against the body,
    /// the last few pixels scroll with nothing on screen to say they can.
    #[test]
    fn a_horizontal_bar_can_be_the_reason_a_vertical_one_is_needed() {
        let body = Rectangle {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 200.0,
        };

        // Taller than the 190 left over, shorter than the body itself.
        let content = Size::new(500.0, 195.0);

        let (v, h) = scrollbars(body, content, Vector::ZERO, 10.0, Policy::Auto, Policy::Auto);

        assert!(h.is_some(), "the horizontal bar is what starts this");
        assert!(v.is_some(), "and the strip it takes makes the body overflow");

        // The offset clamp agrees, which is the point: the two are computed
        // from the same viewport.
        assert!(max_offset(Size::new(body.width, 190.0), content).y > 0.0);
    }
}

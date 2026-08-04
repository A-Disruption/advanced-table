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
    let needs_v = vertical == Policy::Auto && content.height > body.height;
    let needs_h = horizontal == Policy::Auto && content.width > body.width;

    // Each bar eats into the other's track, so a bar can appear purely because
    // the other one appeared. Resolve that before computing geometry.
    let track_height = body.height - if needs_h { width } else { 0.0 };
    let track_width = body.width - if needs_v { width } else { 0.0 };

    let v = needs_v.then(|| {
        let length = thumb_length(track_height, track_height, content.height);
        let start = thumb_position(track_height, track_height, content.height, offset.y);

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
        let length = thumb_length(track_width, track_width, content.width);
        let start = thumb_position(track_width, track_width, content.width, offset.x);

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
}

//! Theming, following the `Catalog` pattern used by iced's own widgets.

use iced::{Background, Border, Color, Theme};

#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub header_background: Option<Background>,
    /// Background for group header rows (anything above the leaf row).
    pub group_background: Option<Background>,
    pub row_background: Option<Background>,
    /// Applied to odd-indexed rows. `None` disables striping.
    pub alternate_row_background: Option<Background>,
    pub border: Border,
    /// Vertical rule between columns. `None` draws nothing.
    pub column_divider: Option<Color>,
    /// Horizontal rule between rows. `None` draws nothing.
    pub row_divider: Option<Color>,
    /// Heavier rule separating header from body.
    pub header_divider: Option<Color>,
    pub scrollbar_track: Option<Background>,
    pub scrollbar_thumb: Background,
    pub scrollbar_thumb_hovered: Background,
    pub scrollbar_border: Border,
}

impl Style {
    pub fn divider_width(&self) -> f32 {
        1.0
    }
}

pub trait Catalog {
    type Class<'a>;

    fn default<'a>() -> Self::Class<'a>;

    fn style(&self, class: &Self::Class<'_>) -> Style;
}

pub type StyleFn<'a, Theme> = Box<dyn Fn(&Theme) -> Style + 'a>;

impl Catalog for Theme {
    type Class<'a> = StyleFn<'a, Self>;

    fn default<'a>() -> Self::Class<'a> {
        Box::new(default)
    }

    fn style(&self, class: &Self::Class<'_>) -> Style {
        class(self)
    }
}

pub fn default(theme: &Theme) -> Style {
    let palette = theme.palette();

    // Every band has to differ from the rule drawn on top of it. Previously
    // `group_background` and `column_divider` were both `background.strong`,
    // so dividers on group header rows were painted in exactly the colour
    // behind them -- drawn correctly, and completely invisible.
    Style {
        header_background: Some(palette.background.weaker.color.into()),
        group_background: Some(palette.background.weak.color.into()),
        row_background: Some(palette.background.base.color.into()),
        alternate_row_background: Some(palette.background.weakest.color.into()),
        border: Border {
            color: palette.background.strong.color,
            width: 1.0,
            radius: 0.0.into(),
        },
        column_divider: Some(palette.background.strong.color),
        row_divider: Some(palette.background.strong.color),
        header_divider: Some(palette.background.strongest.color),
        scrollbar_track: Some(palette.background.weakest.color.into()),
        scrollbar_thumb: palette.background.strong.color.into(),
        scrollbar_thumb_hovered: palette.primary.base.color.into(),
        scrollbar_border: Border {
            radius: 4.0.into(),
            ..Border::default()
        },
    }
}

/// No stripes, no vertical rules -- closer to a modern data grid.
pub fn minimal(theme: &Theme) -> Style {
    let palette = theme.palette();

    Style {
        header_background: None,
        group_background: None,
        row_background: None,
        alternate_row_background: None,
        border: Border::default(),
        column_divider: None,
        row_divider: Some(palette.background.weak.color),
        header_divider: Some(palette.background.strong.color),
        scrollbar_track: None,
        scrollbar_thumb: palette.background.strong.color.into(),
        scrollbar_thumb_hovered: palette.primary.base.color.into(),
        scrollbar_border: Border {
            radius: 4.0.into(),
            ..Border::default()
        },
    }
}
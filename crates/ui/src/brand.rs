//! The Starlet mark.
//!
//! GPUI rasterises an SVG into an alpha mask and tints it with the element's
//! text colour, so the mark is a single-colour asset by construction and picks
//! up the theme foreground rather than carrying a palette of its own.

use gpui::{Rems, Styled as _, Svg, rems, svg};

/// Asset path, relative to `crates/ui/assets`.
pub const MARK: &str = "brand/starlet.svg";

/// Size on the sign-in screen, where the mark is the focal point.
pub const HERO: Rems = Rems(3.0);

/// The mark at `size`. The caller sets the colour with `text_color`, which is
/// what makes it follow the theme.
pub fn mark(size: Rems) -> Svg {
    svg().path(MARK).size(rems(size.0)).flex_none()
}

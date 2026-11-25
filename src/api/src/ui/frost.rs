//! Frosted glass: CSS `backdrop-filter: blur(..) saturate(..)`, which every
//! `.glass` surface of the web interface carries (`--blur-soft: blur(24px)
//! saturate(180%)`, components.css, themes.css).
//!
//! egui has no backdrop filter, so this is a paint callback on the OpenGL
//! renderer. When egui reaches it, everything under the panel (the map,
//! the track, the markers) is already in the framebuffer. The callback
//! copies that region with a margin, builds its mipmaps, and draws it back
//! inside the panel's rounded rect through a Gaussian of the CSS radius and
//! the CSS saturate matrix. The panel's own fill and content then paint
//! over it as they always did, so the result is the browser's: blur and
//! saturate the backdrop, then the translucent white on top.
//!
//! A plugin has no renderer of its own: there the four calls leave a [`Marker`]
//! in its paint list, and the program runs the filter where the marker is.

#[cfg(not(target_family = "wasm"))]
mod gl;
#[cfg(not(target_family = "wasm"))]
pub use gl::*;
#[cfg(target_family = "wasm")]
mod marker;
#[cfg(target_family = "wasm")]
pub use marker::*;

/// `--blur-soft` of the theme in force: what `.glass`, the map rail and the
/// attribution pill use. The light themes blur 24px and saturate 180%; the
/// dark ones saturate 190% and brighten by 10%, which is what lets the frost
/// come through over a dark surface.
pub fn soft() -> Filter {
    filter_of(&crate::ui::style::palette())
}

/// `--blur-soft` of a palette.
pub fn filter_of(p: &crate::ui::style::Palette) -> Filter {
    let (blur, saturate, brightness) = p.blur_soft;
    Filter {
        blur,
        saturate,
        brightness,
    }
}

/// One backdrop filter: a Gaussian of `blur` CSS px, then `saturate`, then
/// `brightness`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Filter {
    pub blur: f32,
    pub saturate: f32,
    pub brightness: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_glass_filter_is_the_css_one_per_theme() {
        use crate::ui::style::palette_of;
        assert_eq!(
            filter_of(&palette_of("light")),
            Filter {
                blur: 24.0,
                saturate: 1.8,
                brightness: 1.0
            }
        );
        assert_eq!(
            filter_of(&palette_of("blue")),
            Filter {
                blur: 24.0,
                saturate: 1.9,
                brightness: 1.1
            }
        );
    }
}

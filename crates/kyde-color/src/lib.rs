#![deny(missing_docs)]
//! A tiny opaque-friendly RGBA colour value (`f32` channels, `0.0..=1.0`), shared by Kyde's
//! pure model crates (`kyde-theme`, `kyde-syntax`) so they don't depend on a GUI framework just
//! to name a colour. Enable the **`gpui`** feature (the binary + `kyde-ui` do) for conversions
//! into gpui's `Rgba`/`Hsla`/`Fill`/`Background`, so gpui builders accept a [`Color`] directly:
//! `.bg(color)` / `.text_color(color)` work because those take `impl Into<Fill>` / `Into<Hsla>`.

/// An RGBA colour with `f32` channels in `0.0..=1.0`. Field-compatible with `gpui::Rgba`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    /// Red channel (`0.0..=1.0`).
    pub r: f32,
    /// Green channel (`0.0..=1.0`).
    pub g: f32,
    /// Blue channel (`0.0..=1.0`).
    pub b: f32,
    /// Alpha channel (`0.0..=1.0`).
    pub a: f32,
}

impl Color {
    /// Opaque colour from a `0xRRGGBB` hex literal.
    ///
    /// ```
    /// let c = kyde_color::Color::rgb(0xFF8000);
    /// assert_eq!(c.r, 1.0);
    /// assert_eq!(c.a, 1.0);
    /// ```
    pub const fn rgb(hex: u32) -> Self {
        Self {
            r: ((hex >> 16) & 0xff) as f32 / 255.0,
            g: ((hex >> 8) & 0xff) as f32 / 255.0,
            b: (hex & 0xff) as f32 / 255.0,
            a: 1.0,
        }
    }
}

#[cfg(feature = "gpui")]
impl From<Color> for gpui::Rgba {
    fn from(c: Color) -> Self {
        gpui::Rgba {
            r: c.r,
            g: c.g,
            b: c.b,
            a: c.a,
        }
    }
}

#[cfg(feature = "gpui")]
impl From<Color> for gpui::Hsla {
    fn from(c: Color) -> Self {
        gpui::Rgba::from(c).into()
    }
}

#[cfg(feature = "gpui")]
impl From<Color> for gpui::Fill {
    fn from(c: Color) -> Self {
        gpui::Rgba::from(c).into()
    }
}

#[cfg(feature = "gpui")]
impl From<Color> for gpui::Background {
    fn from(c: Color) -> Self {
        gpui::Rgba::from(c).into()
    }
}

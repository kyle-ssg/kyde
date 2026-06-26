//! Small colour helpers.

/// Linearly interpolate two `0xRRGGBB` colors (`t` in 0..1) → opaque `Rgba`. Used for the
/// welcome-screen ASCII shimmer.
pub fn lerp_rgb(a: u32, b: u32, t: f32) -> gpui::Rgba {
    let t = t.clamp(0.0, 1.0);
    let chan = |hex: u32, shift: u32| ((hex >> shift) & 0xFF) as f32 / 255.0;
    let mix = |x: f32, y: f32| x + (y - x) * t;
    gpui::Rgba {
        r: mix(chan(a, 16), chan(b, 16)),
        g: mix(chan(a, 8), chan(b, 8)),
        b: mix(chan(a, 0), chan(b, 0)),
        a: 1.0,
    }
}

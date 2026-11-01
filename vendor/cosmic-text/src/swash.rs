// SPDX-License-Identifier: MIT OR Apache-2.0

// Kyde patch (cosmic-text 0.14.2, src/swash.rs): glyphs can be drawn with antialiasing off and a
// hinting strength of the caller's choice (`GlyphRendering`), carried in the cache key's flags so
// two renderings never share a cache entry. A key without those flags takes the rendering the
// process chose once (`set_glyph_rendering`), else draws exactly as before: `swash_image` below
// is unchanged. The changes: the imports, `SwashCache::hinters`, the `with_chosen_rendering` and
// `image` calls in `get_image` and `get_image_uncached`, and everything after `impl SwashCache`.
// Why: vendor/README.md in Kyde's repository.

#[cfg(not(feature = "std"))]
use alloc::vec::Vec;
use core::fmt;
use core::sync::atomic::{AtomicU32, Ordering};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{
    DrawSettings, Engine, HintingInstance, HintingOptions, OutlineGlyphCollection, OutlinePen,
    SmoothMode, Target,
};
use skrifa::MetadataProvider as _;
use swash::scale::{image::Content, ScaleContext};
use swash::scale::{Render, Source, StrikeWith};
use swash::zeno::{Format, Mask, Origin, Vector};

use crate::{CacheKey, CacheKeyFlags, Color, FontSystem, HashMap};

pub use swash::scale::image::{Content as SwashContent, Image as SwashImage};
pub use swash::zeno::{Angle, Command, Placement, Transform};

fn swash_image(
    font_system: &mut FontSystem,
    context: &mut ScaleContext,
    cache_key: CacheKey,
) -> Option<SwashImage> {
    let font = match font_system.get_font(cache_key.font_id) {
        Some(some) => some,
        None => {
            log::warn!("did not find font {:?}", cache_key.font_id);
            return None;
        }
    };

    // Build the scaler
    let mut scaler = context
        .builder(font.as_swash())
        .size(f32::from_bits(cache_key.font_size_bits))
        .hint(true)
        .build();

    // Compute the fractional offset-- you'll likely want to quantize this
    // in a real renderer
    let offset = Vector::new(cache_key.x_bin.as_float(), cache_key.y_bin.as_float());

    // Select our source order
    Render::new(&[
        // Color outline with the first palette
        Source::ColorOutline(0),
        // Color bitmap with best fit selection mode
        Source::ColorBitmap(StrikeWith::BestFit),
        // Standard scalable outline
        Source::Outline,
    ])
    // Select a subpixel format
    .format(Format::Alpha)
    // Apply the fractional offset
    .offset(offset)
    .transform(if cache_key.flags.contains(CacheKeyFlags::FAKE_ITALIC) {
        Some(Transform::skew(
            Angle::from_degrees(14.0),
            Angle::from_degrees(0.0),
        ))
    } else {
        None
    })
    // Render the image
    .render(&mut scaler, cache_key.glyph_id)
}

fn swash_outline_commands(
    font_system: &mut FontSystem,
    context: &mut ScaleContext,
    cache_key: CacheKey,
) -> Option<Box<[swash::zeno::Command]>> {
    use swash::zeno::PathData as _;

    let font = match font_system.get_font(cache_key.font_id) {
        Some(some) => some,
        None => {
            log::warn!("did not find font {:?}", cache_key.font_id);
            return None;
        }
    };

    // Build the scaler
    let mut scaler = context
        .builder(font.as_swash())
        .size(f32::from_bits(cache_key.font_size_bits))
        .hint(true)
        .build();

    // Scale the outline
    let outline = scaler
        .scale_outline(cache_key.glyph_id)
        .or_else(|| scaler.scale_color_outline(cache_key.glyph_id))?;

    // Get the path information of the outline
    let path = outline.path();

    // Return the commands
    Some(path.commands().collect())
}

/// Cache for rasterizing with the swash scaler
pub struct SwashCache {
    context: ScaleContext,
    pub image_cache: HashMap<CacheKey, Option<SwashImage>>,
    pub outline_command_cache: HashMap<CacheKey, Option<Box<[swash::zeno::Command]>>>,
    /// Kyde patch: hinting instances for `rendered_image`.
    hinters: Hinters,
}

impl fmt::Debug for SwashCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("SwashCache { .. }")
    }
}

impl SwashCache {
    /// Create a new swash cache
    pub fn new() -> Self {
        Self {
            context: ScaleContext::new(),
            image_cache: HashMap::default(),
            outline_command_cache: HashMap::default(),
            hinters: Hinters::default(),
        }
    }

    /// Create a swash Image from a cache key, without caching results
    pub fn get_image_uncached(
        &mut self,
        font_system: &mut FontSystem,
        cache_key: CacheKey,
    ) -> Option<SwashImage> {
        let cache_key = with_chosen_rendering(cache_key);
        image(font_system, &mut self.context, &mut self.hinters, cache_key)
    }

    /// Create a swash Image from a cache key, caching results
    pub fn get_image(
        &mut self,
        font_system: &mut FontSystem,
        cache_key: CacheKey,
    ) -> &Option<SwashImage> {
        let cache_key = with_chosen_rendering(cache_key);
        self.image_cache
            .entry(cache_key)
            .or_insert_with(|| image(font_system, &mut self.context, &mut self.hinters, cache_key))
    }

    /// Creates outline commands
    pub fn get_outline_commands(
        &mut self,
        font_system: &mut FontSystem,
        cache_key: CacheKey,
    ) -> Option<&[swash::zeno::Command]> {
        self.outline_command_cache
            .entry(cache_key)
            .or_insert_with(|| swash_outline_commands(font_system, &mut self.context, cache_key))
            .as_deref()
    }

    /// Creates outline commands, without caching results
    pub fn get_outline_commands_uncached(
        &mut self,
        font_system: &mut FontSystem,
        cache_key: CacheKey,
    ) -> Option<Box<[swash::zeno::Command]>> {
        swash_outline_commands(font_system, &mut self.context, cache_key)
    }

    /// Enumerate pixels in an Image, use `with_image` for better performance
    pub fn with_pixels<F: FnMut(i32, i32, Color)>(
        &mut self,
        font_system: &mut FontSystem,
        cache_key: CacheKey,
        base: Color,
        mut f: F,
    ) {
        if let Some(image) = self.get_image(font_system, cache_key) {
            let x = image.placement.left;
            let y = -image.placement.top;

            match image.content {
                Content::Mask => {
                    let mut i = 0;
                    for off_y in 0..image.placement.height as i32 {
                        for off_x in 0..image.placement.width as i32 {
                            //TODO: blend base alpha?
                            f(
                                x + off_x,
                                y + off_y,
                                Color(((image.data[i] as u32) << 24) | base.0 & 0xFF_FF_FF),
                            );
                            i += 1;
                        }
                    }
                }
                Content::Color => {
                    let mut i = 0;
                    for off_y in 0..image.placement.height as i32 {
                        for off_x in 0..image.placement.width as i32 {
                            //TODO: blend base alpha?
                            f(
                                x + off_x,
                                y + off_y,
                                Color::rgba(
                                    image.data[i],
                                    image.data[i + 1],
                                    image.data[i + 2],
                                    image.data[i + 3],
                                ),
                            );
                            i += 4;
                        }
                    }
                }
                Content::SubpixelMask => {
                    log::warn!("TODO: SubpixelMask");
                }
            }
        }
    }
}

// Kyde patch: everything below.

/// How glyph outlines become pixels. The default is what cosmic-text has always drawn:
/// antialiased, with swash's own hinting. Colour glyphs (emoji) always draw as before.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphRendering {
    /// Greyscale antialiasing. Off draws a mask of only fully on and off pixels.
    pub antialias: bool,
    /// How far outlines are fitted to the pixel grid.
    pub hinting: GlyphHinting,
}

/// The hinting strength of a [`GlyphRendering`], as fontconfig and the Linux desktops name them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GlyphHinting {
    /// swash's own hinting, as cosmic-text has always drawn.
    #[default]
    Default,
    /// No hinting: the outline as designed.
    None,
    /// Slight: FreeType's light target (mostly vertical fitting).
    Slight,
    /// Medium: FreeType's normal target.
    Medium,
    /// Full: FreeType's normal target, as for medium.
    ///
    /// Without antialiasing, slight, medium and full all take FreeType's mono target, as cairo
    /// does.
    Full,
}

impl Default for GlyphRendering {
    fn default() -> Self {
        Self {
            antialias: true,
            hinting: GlyphHinting::Default,
        }
    }
}

impl GlyphRendering {
    /// Every [`CacheKeyFlags`] bit a rendering sets.
    pub const FLAGS: CacheKeyFlags = CacheKeyFlags::NO_ANTIALIAS
        .union(CacheKeyFlags::HINT_NONE)
        .union(CacheKeyFlags::HINT_SLIGHT)
        .union(CacheKeyFlags::HINT_MEDIUM)
        .union(CacheKeyFlags::HINT_FULL);

    /// The cache key flags that ask for this rendering: none for the default.
    pub fn flags(self) -> CacheKeyFlags {
        let aa = if self.antialias {
            CacheKeyFlags::empty()
        } else {
            CacheKeyFlags::NO_ANTIALIAS
        };
        aa | match self.hinting {
            GlyphHinting::Default => CacheKeyFlags::empty(),
            GlyphHinting::None => CacheKeyFlags::HINT_NONE,
            GlyphHinting::Slight => CacheKeyFlags::HINT_SLIGHT,
            GlyphHinting::Medium => CacheKeyFlags::HINT_MEDIUM,
            GlyphHinting::Full => CacheKeyFlags::HINT_FULL,
        }
    }

    /// The rendering `flags` ask for (the inverse of [`Self::flags`]).
    pub fn from_flags(flags: CacheKeyFlags) -> Self {
        let hinting = [
            (CacheKeyFlags::HINT_NONE, GlyphHinting::None),
            (CacheKeyFlags::HINT_SLIGHT, GlyphHinting::Slight),
            (CacheKeyFlags::HINT_MEDIUM, GlyphHinting::Medium),
            (CacheKeyFlags::HINT_FULL, GlyphHinting::Full),
        ]
        .into_iter()
        .find_map(|(flag, hinting)| flags.contains(flag).then_some(hinting))
        .unwrap_or_default();
        Self {
            antialias: !flags.contains(CacheKeyFlags::NO_ANTIALIAS),
            hinting,
        }
    }
}

/// The rendering this process chose for keys without rendering flags: 0 while unchosen, else
/// `CHOSEN` with the rendering's flags.
static RENDERING: AtomicU32 = AtomicU32::new(0);
const CHOSEN: u32 = 1 << 31;

/// Draw every glyph whose cache key carries no rendering flags (gpui's are all
/// `CacheKeyFlags::empty()`) with `rendering`. Only before the first glyph is drawn, so images of
/// two renderings never mix in caches built on top of this one: returns false, changing
/// nothing, once a rendering is chosen or a glyph has been drawn.
pub fn set_glyph_rendering(rendering: GlyphRendering) -> bool {
    RENDERING
        .compare_exchange(
            0,
            CHOSEN | rendering.flags().bits(),
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_ok()
}

/// The rendering glyphs without rendering flags are drawn with.
pub fn glyph_rendering() -> GlyphRendering {
    GlyphRendering::from_flags(CacheKeyFlags::from_bits_truncate(
        RENDERING.load(Ordering::Acquire),
    ))
}

/// `cache_key` with the chosen rendering's flags when it carries none of its own. The first call
/// fixes the choice: the default unless `set_glyph_rendering` came first.
fn with_chosen_rendering(mut cache_key: CacheKey) -> CacheKey {
    if !cache_key.flags.intersects(GlyphRendering::FLAGS) {
        let mut chosen = RENDERING.load(Ordering::Acquire);
        if chosen == 0 {
            chosen =
                match RENDERING.compare_exchange(0, CHOSEN, Ordering::AcqRel, Ordering::Acquire) {
                    Ok(_) => CHOSEN,
                    Err(now) => now,
                };
        }
        cache_key.flags |= CacheKeyFlags::from_bits_truncate(chosen) & GlyphRendering::FLAGS;
    }
    cache_key
}

/// A glyph image: unchanged `swash_image` for the default rendering, else `rendered_image`.
fn image(
    font_system: &mut FontSystem,
    context: &mut ScaleContext,
    hinters: &mut Hinters,
    cache_key: CacheKey,
) -> Option<SwashImage> {
    if cache_key.flags.intersects(GlyphRendering::FLAGS) {
        rendered_image(font_system, context, hinters, cache_key)
    } else {
        swash_image(font_system, context, cache_key)
    }
}

/// A glyph image in the rendering its key's flags ask for.
fn rendered_image(
    font_system: &mut FontSystem,
    context: &mut ScaleContext,
    hinters: &mut Hinters,
    cache_key: CacheKey,
) -> Option<SwashImage> {
    let rendering = GlyphRendering::from_flags(cache_key.flags);
    let font = match font_system.get_font(cache_key.font_id) {
        Some(some) => some,
        None => {
            log::warn!("did not find font {:?}", cache_key.font_id);
            return None;
        }
    };
    let offset = Vector::new(cache_key.x_bin.as_float(), cache_key.y_bin.as_float());
    let transform = if cache_key.flags.contains(CacheKeyFlags::FAKE_ITALIC) {
        Some(Transform::skew(
            Angle::from_degrees(14.0),
            Angle::from_degrees(0.0),
        ))
    } else {
        None
    };
    let mut scaler = context
        .builder(font.as_swash())
        .size(f32::from_bits(cache_key.font_size_bits))
        .hint(true)
        .build();

    // Colour glyphs exactly as the default rendering draws them.
    if let Some(image) = Render::new(&[
        Source::ColorOutline(0),
        Source::ColorBitmap(StrikeWith::BestFit),
    ])
    .format(Format::Alpha)
    .offset(offset)
    .transform(transform)
    .render(&mut scaler, cache_key.glyph_id)
    {
        return Some(image);
    }

    // Text fitted to the pixel grid sits on whole pixels: a subpixel offset would blur the
    // fitted stems again, and without antialiasing move them by a pixel from glyph to glyph.
    let offset = if !rendering.antialias
        || matches!(rendering.hinting, GlyphHinting::Medium | GlyphHinting::Full)
    {
        Vector::ZERO
    } else {
        offset
    };
    let mut image = match rendering.hinting {
        GlyphHinting::Default => Render::new(&[Source::Outline])
            .format(Format::Alpha)
            .offset(offset)
            .transform(transform)
            .render(&mut scaler, cache_key.glyph_id)?,
        hinting => {
            let target = match hinting {
                GlyphHinting::None | GlyphHinting::Default => None,
                _ if !rendering.antialias => Some(Target::Mono),
                GlyphHinting::Slight => Some(SmoothMode::Light.into()),
                GlyphHinting::Medium | GlyphHinting::Full => Some(SmoothMode::Normal.into()),
            };
            let face = font_system.db().face(cache_key.font_id)?.index;
            outline_image(
                font.data(),
                face,
                cache_key,
                target,
                hinters,
                offset,
                transform,
            )?
        }
    };
    if !rendering.antialias && image.content == Content::Mask {
        // Each pixel fully on or off: on where the outline covers at least half of it.
        for alpha in &mut image.data {
            *alpha = if *alpha >= 128 { 255 } else { 0 };
        }
    }
    Some(image)
}

/// A glyph's outline, hinted for `target` (unhinted for `None`, or when the font's hinting
/// fails), rasterised as swash's `Render` rasterises an outline.
fn outline_image(
    data: &[u8],
    face: u32,
    cache_key: CacheKey,
    target: Option<Target>,
    hinters: &mut Hinters,
    offset: Vector,
    transform: Option<Transform>,
) -> Option<SwashImage> {
    let font = skrifa::FontRef::from_index(data, face).ok()?;
    let outlines = font.outline_glyphs();
    let glyph = outlines.get(skrifa::GlyphId::new(cache_key.glyph_id.into()))?;
    let mut pen = Pen::default();
    let hinted = target
        .and_then(|target| hinters.get(cache_key, target, &outlines))
        .is_some_and(|instance| {
            glyph
                .draw(DrawSettings::hinted(instance, false), &mut pen)
                .is_ok()
        });
    if !hinted {
        pen.0.clear();
        let size = Size::new(f32::from_bits(cache_key.font_size_bits));
        glyph
            .draw(
                DrawSettings::unhinted(size, LocationRef::default()),
                &mut pen,
            )
            .ok()?;
    }
    let mut image = SwashImage::new();
    let placement = Mask::new(&pen.0[..])
        .format(Format::Alpha)
        .origin(Origin::BottomLeft)
        .transform(transform)
        .offset(offset)
        .render_offset(offset)
        .inspect(|fmt, w, h| {
            image.data.resize(fmt.buffer_size(w, h), 0);
        })
        .render_into(&mut image.data[..], None);
    image.placement = placement;
    image.content = Content::Mask;
    image.source = Source::Outline;
    Some(image)
}

/// An outline as zeno path commands.
#[derive(Default)]
struct Pen(Vec<Command>);

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(Command::MoveTo(Vector::new(x, y)));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.0.push(Command::LineTo(Vector::new(x, y)));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.0
            .push(Command::QuadTo(Vector::new(cx0, cy0), Vector::new(x, y)));
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.0.push(Command::CurveTo(
            Vector::new(cx0, cy0),
            Vector::new(cx1, cy1),
            Vector::new(x, y),
        ));
    }

    fn close(&mut self) {
        self.0.push(Command::Close);
    }
}

/// The most recently used hinting instances (one per font, size and target), as swash keeps its
/// own: building one runs the font's hinting programs, too slow to repeat for every glyph.
#[derive(Default)]
struct Hinters {
    entries: Vec<Hinter>,
    clock: u64,
}

struct Hinter {
    font_id: fontdb::ID,
    font_size_bits: u32,
    target: Target,
    instance: HintingInstance,
    used: u64,
}

impl Hinters {
    const MAX: usize = 16;

    /// The instance for `cache_key`'s font and size and `target`; `None` when the font's hinting
    /// can't be set up.
    fn get(
        &mut self,
        cache_key: CacheKey,
        target: Target,
        outlines: &OutlineGlyphCollection,
    ) -> Option<&HintingInstance> {
        self.clock += 1;
        let found = self.entries.iter().position(|h| {
            h.font_id == cache_key.font_id
                && h.font_size_bits == cache_key.font_size_bits
                && h.target == target
        });
        let at = match found {
            Some(at) => at,
            None => {
                let instance = HintingInstance::new(
                    outlines,
                    Size::new(f32::from_bits(cache_key.font_size_bits)),
                    LocationRef::default(),
                    HintingOptions {
                        engine: Engine::AutoFallback,
                        target,
                    },
                )
                .ok()?;
                let hinter = Hinter {
                    font_id: cache_key.font_id,
                    font_size_bits: cache_key.font_size_bits,
                    target,
                    instance,
                    used: 0,
                };
                if self.entries.len() < Self::MAX {
                    self.entries.push(hinter);
                    self.entries.len() - 1
                } else {
                    let oldest = (0..self.entries.len()).min_by_key(|&i| self.entries[i].used)?;
                    self.entries[oldest] = hinter;
                    oldest
                }
            }
        };
        let entry = &mut self.entries[at];
        entry.used = self.clock;
        Some(&entry.instance)
    }
}

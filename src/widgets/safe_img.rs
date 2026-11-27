//! Images, checked before gpui decodes them.
//!
//! gpui's image loader decodes whatever it is handed, with no limits: every frame of a GIF or
//! an animated WebP at the full size of its canvas, any other picture at its full size. A
//! file of a few hundred bytes can claim a 65535 × 65535 canvas, which is 17 GB a frame —
//! and a README preview shows whatever pictures its repository (or a URL in it) holds.
//!
//! So an image's header is read first ([`decoded_size`]): one that would decode to more than
//! [`MAX_DECODED`] is never handed over. Local files go through [`local`]; remote ones are
//! checked where they arrive (`remote_img`). SVGs aren't covered: gpui draws those itself,
//! from text.

use gpui::{
    img, App, Asset, ImageAssetLoader, ImageCacheError, ImageSource, Img, RenderImage, Resource,
    Window,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The most memory one image may decode to (RGBA, every frame of an animation): 2 GiB.
/// A long screen recording saved as a GIF runs to hundreds of megabytes decoded, so this
/// stops what would take the machine down, not what is merely big.
pub(crate) const MAX_DECODED: u64 = 2 << 30;

/// The biggest local file read as an image at all.
const MAX_FILE: u64 = 256 << 20;

/// How many bytes gpui would decode the image in `bytes` to: its canvas, RGBA, times its
/// frames. Read from its header and block structure alone, nothing is decoded. `None` when
/// that can't be told (not an image gpui decodes this way — it fails there, or is an SVG).
pub(crate) fn decoded_size(bytes: &[u8]) -> Option<u64> {
    let (w, h, frames) = if bytes.starts_with(b"GIF8") {
        let dim = |at: usize| {
            let pair = [*bytes.get(at)?, *bytes.get(at + 1)?];
            Some(u64::from(u16::from_le_bytes(pair)))
        };
        (dim(6)?, dim(8)?, gif_frames(bytes))
    } else {
        let (w, h) = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .ok()?
            .into_dimensions()
            .ok()?;
        (u64::from(w), u64::from(h), webp_frames(bytes))
    };
    Some(
        w.saturating_mul(h)
            .saturating_mul(4)
            .saturating_mul(frames.max(1)),
    )
}

/// `Err(why)` when the image in `bytes` would decode to more than [`MAX_DECODED`].
pub(crate) fn check(bytes: &[u8]) -> Result<(), String> {
    match decoded_size(bytes) {
        Some(size) if size > MAX_DECODED => Err(format!(
            "this image would take {} MB to show: too big to load",
            size >> 20
        )),
        _ => Ok(()),
    }
}

/// The frames in a GIF: its image blocks, counted by walking its block structure (header,
/// screen descriptor and colour table, then extension and image blocks up to the trailer).
fn gif_frames(gif: &[u8]) -> u64 {
    // A colour table's bytes, from the packed field that says whether there is one.
    let table = |packed: u8| {
        if packed & 0x80 == 0 {
            0
        } else {
            3usize << ((packed & 7) + 1)
        }
    };
    // Past a run of data sub-blocks: each led by its length, the run ended by a 0.
    let past_blocks = |mut at: usize| loop {
        let len = usize::from(*gif.get(at)?);
        at += 1 + len;
        if len == 0 {
            return Some(at);
        }
    };
    let Some(&packed) = gif.get(10) else {
        return 0;
    };
    let (mut at, mut frames) = (13 + table(packed), 0);
    loop {
        let next = match gif.get(at) {
            // An image: its descriptor (10 bytes), its colour table, the code size, its data.
            Some(0x2C) => {
                frames += 1;
                gif.get(at + 9)
                    .and_then(|&packed| past_blocks(at + 10 + table(packed) + 1))
            }
            // An extension: its marker and label, then data.
            Some(0x21) => past_blocks(at + 2),
            // The trailer, or the end.
            _ => None,
        };
        match next {
            Some(next) => at = next,
            None => return frames,
        }
    }
}

/// The frames in a WebP: its `ANMF` chunks (1 when it isn't animated, or isn't a WebP).
fn webp_frames(webp: &[u8]) -> u64 {
    if webp.get(..4) != Some(b"RIFF") || webp.get(8..12) != Some(b"WEBP") {
        return 1;
    }
    let (mut at, mut frames) = (12usize, 0u64);
    while let (Some(tag), Some(&[a, b, c, d])) = (webp.get(at..at + 4), webp.get(at + 4..at + 8)) {
        if tag == b"ANMF" {
            frames += 1;
        }
        let len = usize::try_from(u32::from_le_bytes([a, b, c, d])).unwrap_or(usize::MAX);
        // A chunk's data is padded to an even length.
        at = at
            .saturating_add(8)
            .saturating_add(len)
            .saturating_add(len & 1);
    }
    frames.max(1)
}

/// `img()` of the local file at `path` — which a project chose, not you — checked off the UI
/// thread before gpui decodes it: a regular file (a link to `/dev/zero` is read forever),
/// of a sane size, that [`check`] passes. One that isn't draws nothing.
pub(crate) fn local(path: PathBuf) -> Img {
    img(ImageSource::Custom(Arc::new(
        move |window: &mut Window, cx: &mut App| window.use_asset::<CheckedFile>(&path, cx),
    )))
}

/// gpui's image loader for a local file, behind [`check`].
enum CheckedFile {}

impl Asset for CheckedFile {
    type Source = PathBuf;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        let decode = ImageAssetLoader::load(Resource::Path(source.clone().into()), cx);
        async move {
            let refused = |why: String| ImageCacheError::Other(Arc::new(anyhow::anyhow!(why)));
            let bytes = read_image_file(&source).map_err(|e| ImageCacheError::Io(Arc::new(e)))?;
            check(&bytes).map_err(refused)?;
            drop(bytes);
            decode.await
        }
    }
}

/// `path`'s bytes, if it is (or links to) a regular file no bigger than [`MAX_FILE`].
fn read_image_file(path: &Path) -> std::io::Result<Vec<u8>> {
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() || meta.len() > MAX_FILE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a file to show as an image",
        ));
    }
    std::fs::read(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A GIF of `frames` 1 × 1 images on a `w` × `h` canvas, with the extension block real
    /// ones put before each frame.
    fn gif(w: u16, h: u16, frames: usize) -> Vec<u8> {
        let mut out = b"GIF89a".to_vec();
        out.extend(w.to_le_bytes());
        out.extend(h.to_le_bytes());
        out.extend([0, 0, 0]);
        for _ in 0..frames {
            out.extend([0x21, 0xF9, 4, 0, 0, 0, 0, 0]);
            out.extend([0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0]);
            out.extend([2, 2, 0x4C, 0x01, 0]);
        }
        out.push(0x3B);
        out
    }

    /// An image is sized from its header before anything decodes it: a few bytes claiming
    /// a huge canvas, or a modest canvas with thousands of frames, are refused; ordinary
    /// pictures pass.
    #[test]
    fn an_image_too_big_to_decode_is_refused_from_its_header() {
        assert_eq!(decoded_size(&gif(100, 50, 3)), Some(100 * 50 * 4 * 3));
        assert!(check(&gif(100, 50, 3)).is_ok());
        assert!(check(&gif(u16::MAX, u16::MAX, 1)).is_err(), "17 GB a frame");
        assert!(check(&gif(4000, 4000, 100)).is_err(), "6 GB of frames");
        // Every other format gpui decodes is sized by the `image` crate's header read.
        let logo = include_bytes!("../../assets/logo-square.png");
        assert_eq!(decoded_size(logo), Some(1024 * 1024 * 4));
        assert_eq!(
            decoded_size(b"<svg xmlns='http://www.w3.org/2000/svg'/>"),
            None
        );
    }

    /// The loader the previews draw with: a real picture still comes out decoded, one too
    /// big never reaches gpui's decoder, and neither does a link to a device.
    #[gpui::test]
    async fn the_preview_loader_decodes_pictures_and_refuses_bombs(cx: &mut gpui::TestAppContext) {
        let dir = std::env::temp_dir().join(format!("kyde-safe-img-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let logo = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/logo-square.png");
        std::fs::write(dir.join("bomb.gif"), gif(u16::MAX, u16::MAX, 1)).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/dev/zero", dir.join("zero.png")).unwrap();

        let load = |path: PathBuf, cx: &mut gpui::TestAppContext| {
            cx.update(|cx| cx.fetch_asset::<CheckedFile>(&path).0)
        };
        let shown = load(logo, cx).await.expect("a real picture loads");
        assert_eq!(shown.size(0).width.0, 1024);
        assert!(load(dir.join("bomb.gif"), cx).await.is_err());
        #[cfg(unix)]
        assert!(load(dir.join("zero.png"), cx).await.is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}

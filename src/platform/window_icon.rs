//! Kyde's icon on its X11 windows (`_NET_WM_ICON`). A panel or window switcher finds an app's
//! icon through its `.desktop` entry, matched on the window's `WM_CLASS` (see `APP_ID`) — but
//! Kyde run straight from the `AppImage` or a build has no installed entry, and gpui sets no
//! icon of its own, so the panel and Alt+Tab showed a blank one. This sets the bundled logo
//! on every Kyde window, over a short-lived X connection of its own, on a background thread.
//! (gpui exposes no X window id, so the windows are found by `_NET_WM_PID`.) Wayland has no
//! such property: there the icon comes from the `.desktop` entry alone. No gpui.

use anyhow::Context as _;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, PropMode, Window};
use x11rb::wrapper::ConnectionExt as _;

/// The sizes put in the property: a panel or switcher picks the nearest.
const SIZES: [u32; 5] = [16, 32, 48, 64, 128];

/// How many times (100 ms apart) to look for the new window.
const TRIES: usize = 30;

/// How deep below the root Kyde's windows can sit: a reparenting window manager puts each in
/// a frame (sometimes a frame inside a frame).
const DEPTH: usize = 3;

/// Give every Kyde window that has no icon yet the logo, without blocking: call after opening
/// a window. Failure (no X server, a property refused) leaves the windows as they were.
pub(crate) fn apply() {
    std::thread::spawn(|| {
        let _ = apply_now();
    });
}

fn apply_now() -> anyhow::Result<()> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots.get(screen).context("no X screen")?.root;
    let pid_atom = conn.intern_atom(false, b"_NET_WM_PID")?.reply()?.atom;
    let icon_atom = conn.intern_atom(false, b"_NET_WM_ICON")?.reply()?.atom;
    let mut icon = None;
    // The window was only just opened, and gpui's own connection may not have sent it to the
    // server yet: look again for a moment, until one without the icon turns up.
    for _ in 0..TRIES {
        let mut set = 0;
        for win in own_windows(&conn, root, pid_atom, std::process::id())? {
            // A window can close between the walk and here (a modal): skip it, keep going.
            let Ok(has) = conn
                .get_property(false, win, icon_atom, AtomEnum::CARDINAL, 0, 0)?
                .reply()
            else {
                continue;
            };
            if has.type_ != u32::from(AtomEnum::NONE) {
                continue;
            }
            if icon.is_none() {
                icon = Some(net_wm_icon(
                    include_bytes!("../../assets/logo.png"),
                    &SIZES,
                )?);
            }
            if let Some(data) = &icon {
                // Checked: waits for the server to apply it (or refuse it) before going on —
                // a request still in flight when this short-lived connection closes can be lost.
                // Refused (the window just closed): skip it, keep going.
                if conn
                    .change_property32(PropMode::REPLACE, win, icon_atom, AtomEnum::CARDINAL, data)?
                    .check()
                    .is_ok()
                {
                    set += 1;
                }
            }
        }
        if set > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Ok(())
}

/// The windows within `DEPTH` levels below `root` whose `_NET_WM_PID` is `pid`.
fn own_windows(
    conn: &impl Connection,
    root: Window,
    pid_atom: u32,
    pid: u32,
) -> anyhow::Result<Vec<Window>> {
    let mut out = Vec::new();
    let mut level = vec![root];
    for _ in 0..DEPTH {
        let mut next = Vec::new();
        for parent in level {
            // A window can vanish mid-walk: skip it, keep going.
            let Ok(Ok(tree)) = conn.query_tree(parent).map(x11rb::cookie::Cookie::reply) else {
                continue;
            };
            for win in tree.children {
                let owner = conn
                    .get_property(false, win, pid_atom, AtomEnum::CARDINAL, 0, 1)?
                    .reply()
                    .ok()
                    .and_then(|r| r.value32().and_then(|mut v| v.next()));
                if owner == Some(pid) {
                    out.push(win);
                } else {
                    next.push(win);
                }
            }
        }
        level = next;
    }
    Ok(out)
}

/// `_NET_WM_ICON` data for `png` at each of `sizes`: per size its width, height, then the
/// pixels row by row as `0xAARRGGBB`. Pure.
fn net_wm_icon(png: &[u8], sizes: &[u32]) -> anyhow::Result<Vec<u32>> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)?.into_rgba8();
    let mut out = Vec::new();
    for &n in sizes {
        let scaled = image::imageops::resize(&img, n, n, image::imageops::FilterType::Triangle);
        out.extend([n, n]);
        out.extend(scaled.pixels().map(|p| {
            let [r, g, b, a] = p.0;
            u32::from_be_bytes([a, r, g, b])
        }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_data_is_each_size_then_its_argb_pixels() {
        let data = net_wm_icon(include_bytes!("../../assets/logo.png"), &[2, 16]).unwrap();
        assert_eq!(data.len(), 2 + 2 * 2 + 2 + 16 * 16);
        assert_eq!(&data[..2], &[2, 2]);
        assert_eq!(&data[6..8], &[16, 16]);
        // The logo is a rounded tile with a transparent margin: corners clear, middle opaque.
        let px16 = &data[8..];
        assert_eq!(px16[0] >> 24, 0, "corner is transparent");
        assert_eq!(px16[8 * 16 + 8] >> 24, 0xFF, "centre is opaque");
    }

    #[test]
    fn a_broken_png_is_an_error_not_a_panic() {
        assert!(net_wm_icon(b"not a png", &[16]).is_err());
    }
}

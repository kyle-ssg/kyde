"""Build the README / site hero collage from three screenshots.

    python3 scripts/hero.py

Fans out the Agents view (front and centre) over the commit view's side-by-side diff (left)
and the 3-way merge (right), with soft shadows on a transparent background, into
assets/screenshots/hero.png. Run it after scripts/screenshots.sh retakes any of agents.png,
git-diff.png or merge.png. Needs Pillow (`pip3 install pillow`).
"""

import os

from PIL import Image, ImageFilter, ImageOps

SHOTS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "assets", "screenshots")
OUT_WIDTH = 2400  # README / site show it ~900-1000 CSS px wide: sharp on 2x screens.


def load(name):
    return Image.open(os.path.join(SHOTS, f"{name}.png")).convert("RGBA")


def round_corners_like(im, ref, k=64):
    """Give `im` the rounded-corner alpha of window capture `ref`: merge.png is a region grab,
    so its corners hold desktop wallpaper."""
    corner = ref.getchannel("A").crop((0, 0, k, k))
    alpha = Image.new("L", im.size, 255)
    w, h = im.size
    alpha.paste(corner, (0, 0))
    alpha.paste(ImageOps.mirror(corner), (w - k, 0))
    alpha.paste(ImageOps.flip(corner), (0, h - k))
    alpha.paste(ImageOps.flip(ImageOps.mirror(corner)), (w - k, h - k))
    im.putalpha(alpha)
    return im


def scaled(im, width):
    return im.resize((width, round(im.height * width / im.width)), Image.LANCZOS)


def place(canvas, im, x, y, blur=40, drop=24, opacity=150):
    """Composite `im` at (x, y) over a soft drop shadow."""
    pad = blur * 3
    mask = Image.new("L", (im.width + pad * 2, im.height + pad * 2), 0)
    mask.paste(im.getchannel("A").point(lambda v: opacity if v > 0 else 0), (pad, pad))
    shadow = Image.new("RGBA", mask.size, (0, 0, 0, 0))
    shadow.putalpha(mask.filter(ImageFilter.GaussianBlur(blur)))
    canvas.alpha_composite(shadow, (x - pad, y - pad + drop))
    canvas.alpha_composite(im, (x, y))


def main():
    agents = load("agents")
    diff = load("git-diff")
    merge = round_corners_like(load("merge"), agents)

    canvas = Image.new("RGBA", (4200, 2000), (0, 0, 0, 0))
    place(canvas, scaled(diff, 1700), 100, 300)
    place(canvas, scaled(merge, 1700), 2400, 300)
    place(canvas, scaled(agents, 2000), 1100, 150)

    margin = 60
    left, top, right, bottom = canvas.getchannel("A").point(lambda v: 255 if v > 8 else 0).getbbox()
    canvas = canvas.crop((left - margin, top - margin, right + margin, bottom + margin))
    canvas = scaled(canvas, OUT_WIDTH)
    out = os.path.join(SHOTS, "hero.png")
    canvas.save(out, optimize=True)
    print(f"wrote {os.path.normpath(out)} {canvas.size[0]}x{canvas.size[1]}")


if __name__ == "__main__":
    main()

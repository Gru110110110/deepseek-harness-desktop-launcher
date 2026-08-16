# SPDX-License-Identifier: MIT
"""Generate platform icons from the approved DSH Launcher artwork.

The checked-in ``assets/dsh-launcher-logo-approved.png`` is the single source
of truth. This maintainer tool emits the application/window PNGs, macOS ICNS,
and Windows ICO. The generated files are checked in, so CI never needs this
tool.

Requires Pillow. Run: ``python build/generate_icons.py``.
"""
from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
SOURCE = ASSETS / "dsh-launcher-logo-approved.png"
BRAND = (77, 107, 254, 255)  # #4d6bfe
TECH_WHITE = (246, 249, 255, 255)
PLATE_TOP = (250, 251, 254, 255)
PLATE_BOTTOM = (226, 232, 241, 255)
# Include exact Win32 tray/taskbar sizes at common 100–400% scale factors so
# Windows rarely needs to interpolate an ICO frame at display time.
ICO_SIZES = [16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 128, 256]
APP_ICON_SIZE = 1024
APP_MARK_SIZE = 790
TRAY_ICON_SIZE = 128
TRAY_MARK_SIZE = 124


def load_source() -> Image.Image:
    """Load the approved transparent square artwork."""
    source = Image.open(SOURCE).convert("RGBA")
    if source.width != source.height:
        raise ValueError(f"logo source must be square, got {source.size}")
    if source.getextrema()[3][0] == 255:
        raise ValueError("logo source must contain real transparency")
    return source


def rounded_button(width: int, height: int, radius: int, color: tuple[int, int, int, int]) -> Image.Image:
    """A solid rounded rectangle used as a tkinter button background."""
    canvas = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    draw = ImageDraw.Draw(canvas)
    draw.rounded_rectangle([(0, 0), (width - 1, height - 1)], radius=radius, fill=color)
    return canvas


def fill_internal_chevron(source: Image.Image) -> Image.Image:
    """Fill only the logo's closed transparent chevron with tech white."""
    alpha = source.getchannel("A")
    transparent = alpha.point(lambda value: 255 if value < 250 else 0)
    seed = (source.width // 2, source.height // 2)
    if transparent.getpixel(seed) == 0:
        raise ValueError("logo center is not inside the transparent chevron")
    ImageDraw.floodfill(transparent, seed, 128)
    chevron_mask = transparent.point(lambda value: 255 if value == 128 else 0)
    if chevron_mask.getbbox() is None:
        raise ValueError("logo chevron could not be isolated")

    white = Image.new("RGBA", source.size, TECH_WHITE)
    white.putalpha(chevron_mask)
    return Image.alpha_composite(white, source)


def tight_mark(source: Image.Image) -> Image.Image:
    """Return the visible brand mark without the source canvas padding."""
    bbox = source.getchannel("A").getbbox()
    if bbox is None:
        raise ValueError("logo source has no visible pixels")
    return source.crop(bbox)


def place_mark(canvas: Image.Image, mark: Image.Image, maximum: int) -> None:
    """Center one mark at a fixed maximum edge while preserving aspect ratio."""
    scale = maximum / max(mark.size)
    width = max(1, round(mark.width * scale))
    height = max(1, round(mark.height * scale))
    resized = mark.resize((width, height), Image.Resampling.LANCZOS)
    canvas.alpha_composite(
        resized,
        ((canvas.width - width) // 2, (canvas.height - height) // 2),
    )


def app_icon(mark: Image.Image) -> Image.Image:
    """Build a modern rounded application tile with a large brand mark."""
    canvas = Image.new("RGBA", (APP_ICON_SIZE, APP_ICON_SIZE), (0, 0, 0, 0))
    gradient_strip = Image.new("RGBA", (1, APP_ICON_SIZE))
    pixels = gradient_strip.load()
    for y in range(APP_ICON_SIZE):
        fraction = y / (APP_ICON_SIZE - 1)
        color = tuple(
            round(top + (bottom - top) * fraction)
            for top, bottom in zip(PLATE_TOP, PLATE_BOTTOM)
        )
        pixels[0, y] = color
    gradient = gradient_strip.resize(canvas.size)
    mask = Image.new("L", canvas.size, 0)
    mask_draw = ImageDraw.Draw(mask)
    mask_draw.rounded_rectangle((22, 22, 1001, 1001), radius=224, fill=255)
    canvas.paste(gradient, (0, 0), mask)
    ImageDraw.Draw(canvas).rounded_rectangle(
        (22, 22, 1001, 1001),
        radius=224,
        outline=(255, 255, 255, 210),
        width=5,
    )
    place_mark(canvas, mark, APP_MARK_SIZE)
    return canvas


def tray_icon(mark: Image.Image) -> Image.Image:
    """Build a margin-minimal colored tray mark for small-size rendering."""
    canvas = Image.new("RGBA", (TRAY_ICON_SIZE, TRAY_ICON_SIZE), (0, 0, 0, 0))
    place_mark(canvas, mark, TRAY_MARK_SIZE)
    return canvas


def main() -> int:
    source = load_source()
    mark = tight_mark(fill_internal_chevron(source))
    icon = app_icon(mark)
    icon.save(ASSETS / "icon.png")

    tray_icon(mark).save(ASSETS / "tray-icon.png")

    # The same independent launcher mark is used in the window header.
    logo = Image.new("RGBA", (48, 48), (0, 0, 0, 0))
    place_mark(logo, mark, 46)
    logo.save(ASSETS / "logo.png")
    logo.save(ASSETS / "logo-blue.png")

    rounded_button(220, 46, 14, BRAND).save(ASSETS / "button.png")
    rounded_button(220, 46, 14, (199, 206, 222, 255)).save(ASSETS / "button-disabled.png")

    icon.save(ASSETS / "icon.ico", format="ICO", sizes=[(s, s) for s in ICO_SIZES])
    icon.save(ASSETS / "icon.icns", format="ICNS")

    print(
        "wrote icon.png, tray-icon.png, logo.png, logo-blue.png, button.png, "
        "button-disabled.png, icon.ico, icon.icns under",
        ASSETS,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

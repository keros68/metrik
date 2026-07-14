"""Generate the macOS menu bar template icon from the Metrik logo silhouette.

Template images are pure black + alpha; macOS inverts/tints them to match the
menu bar. tray-icon normalises the image to 18pt tall regardless of its pixel
size, so ship one high-resolution square (44px) with almost no padding and let
the platform scale it: crisp on Retina, correctly sized on any display.
"""
from PIL import Image, ImageDraw

SS = 8  # supersample, then downscale for clean antialiasing
SIZE = 44
STROKE = 5.0  # px at 44 (~2pt once scaled to the 18pt menu bar height)


def render() -> Image.Image:
    s = SIZE * SS
    img = Image.new("RGBA", (s, s), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    width = STROKE * SS
    pad = 1.0 * SS
    box = [pad + width / 2, pad + width / 2, s - pad - width / 2, s - pad - width / 2]

    # Gauge arc: a "C" open at the bottom, matching the app icon.
    d.arc(box, start=125, end=55, fill=(0, 0, 0, 255), width=int(round(width)))

    # The needle dot sits inside the opening, lower left.
    r = width * 0.85
    d.ellipse(
        [s * 0.36 - r, s * 0.64 - r, s * 0.36 + r, s * 0.64 + r],
        fill=(0, 0, 0, 255),
    )

    return img.resize((SIZE, SIZE), Image.LANCZOS)


render().save("D:/work/usage/src-tauri/icons/tray-macos.png")
print("wrote tray-macos.png")

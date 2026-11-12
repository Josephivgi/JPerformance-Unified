# Draws the JPerformance Unified icon: red "JPU" letters on a dark rounded square.
# Usage: python make_icon.py <out dir>
import sys
from PIL import Image, ImageDraw, ImageFont

out = sys.argv[1]
RED = (229, 57, 53, 255)
BG = (16, 19, 23, 255)
FONT = "C:/Windows/Fonts/seguibl.ttf"  # Segoe UI Black


def draw(S, text_w):
    """Square icon of size S; the letters fill text_w of the width."""
    img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    m = round(S * 0.04)
    d.rounded_rectangle((m, m, S - 1 - m, S - 1 - m), radius=round(S * 0.2), fill=BG)
    # find the font size that makes "JPU" text_w wide
    size = S
    while True:
        f = ImageFont.truetype(FONT, size)
        l, t, r, b = d.textbbox((0, 0), "JPU", font=f)
        if r - l <= S * text_w:
            break
        size -= 1
    x = (S - (r - l)) / 2 - l
    y = (S - (b - t)) / 2 - t
    d.text((x, y), "JPU", font=f, fill=RED)
    return img


big = draw(1024, 0.74)
big.save(out + "/icon.png")
# Small sizes are drawn from the big one; letters get a bit wider at 16/24 px to stay readable.
frames = []
for s in (256, 128, 64, 48, 32, 24, 16):
    src = draw(1024, 0.84 if s <= 24 else 0.74)
    frames.append(src.resize((s, s), Image.LANCZOS))
frames[0].save(out + "/icon.ico", format="ICO", sizes=[f.size for f in frames], append_images=frames[1:])

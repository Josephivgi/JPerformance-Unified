# Draws the JPerformance Unified icon: a pulse line on a dark rounded square.
import sys
from PIL import Image, ImageDraw
out = sys.argv[1]
S = 1024
img = Image.new("RGBA", (S, S), (0, 0, 0, 0))
d = ImageDraw.Draw(img)
d.rounded_rectangle((40, 40, S - 40, S - 40), radius=210, fill=(16, 19, 23, 255))
# pulse: flat, up-stroke, deep down-stroke, up, flat (same path as the mockup logo, 24px grid)
pts = [(3, 12), (7, 12), (9, 7), (13, 17), (15, 12), (21, 12)]
sc = (S - 280) / 18
pts = [(140 + (x - 3) * sc, S / 2 + (y - 12) * sc) for x, y in pts]
d.line(pts, fill=(57, 135, 229, 255), width=92, joint="curve")
for p in (pts[0], pts[-1]):
    d.ellipse((p[0] - 46, p[1] - 46, p[0] + 46, p[1] + 46), fill=(57, 135, 229, 255))
img.save(out + "/icon.png")
img.resize((256, 256), Image.LANCZOS).save(out + "/icon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])

"""Generate the plugin's simple band/wave symbol at UXP icon sizes."""
from pathlib import Path
import math
from PIL import Image, ImageDraw

out = Path(__file__).resolve().parents[1] / 'plugin' / 'icons'
out.mkdir(parents=True, exist_ok=True)
for size in (23, 46, 48, 96):
    scale = 4
    im = Image.new('RGBA', (size*scale, size*scale))
    draw = ImageDraw.Draw(im)
    s = size*scale
    draw.rounded_rectangle((1, 1, s-2, s-2), radius=s*.2, fill=(55, 63, 72, 255))
    for x in (.26, .5, .74):
        draw.line((s*x, s*.22, s*x, s*.78), fill=(104, 121, 137, 255), width=max(1, int(s*.07)))
    points=[(s*(.12+.76*i/80),s*(.5+.20*math.sin(i/80*math.tau*1.5))) for i in range(81)]
    draw.line(points, fill=(159, 226, 232, 255), width=max(1,int(s*.055)))
    filename={23:'banding-23.png',46:'banding-23@2x.png',48:'banding-48.png',96:'banding-48@2x.png'}[size]
    im.resize((size,size), Image.Resampling.LANCZOS).save(out/filename)

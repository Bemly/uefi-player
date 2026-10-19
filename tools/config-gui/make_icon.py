#!/usr/bin/env python3
"""从一张原图生成全套应用图标（黑底像素风原图，按内容框裁留白后透明垫正方形）。

用法：
    ./make_icon.py <source.png> [assets_dir]

产物（默认写到本目录的 assets/）：
    icon.png        1024 主图（NEAREST，保持像素颗粒）
    icon-256.rgba   256x256 预烘焙 RGBA，主程序 include_bytes! 用（零新依赖）
    icon.ico        Windows 多尺寸合一
    icon.icns       macOS（经 iconutil 由 icon.iconset 生成，本脚本只产 iconset）
"""
import os
import sys

from PIL import Image

SRC = sys.argv[1]
OUT = sys.argv[2] if len(sys.argv) > 2 else os.path.join(os.path.dirname(__file__), "assets")

img = Image.open(SRC).convert("RGBA")
W, H = img.size

# 内容 = 非近黑像素；下边框留白大，统一按内容框 + 均等边距裁掉。
px = img.load()
xs, ys = [], []
for y in range(0, H, 4):
    for x in range(0, W, 4):
        r, g, b, _ = px[x, y]
        if max(r, g, b) > 12:
            xs.append(x)
            ys.append(y)
x0, x1, y0, y1 = min(xs), max(xs), min(ys), max(ys)

m = max(8, int(0.04 * max(x1 - x0, y1 - y0)))
cropped = img.crop((
    max(0, x0 - m), max(0, y0 - m),
    min(W, x1 + 1 + m), min(H, y1 + 1 + m),
))

side = max(cropped.size)
canvas = Image.new("RGBA", (side, side), (0, 0, 0, 0))
canvas.paste(cropped, ((side - cropped.width) // 2, (side - cropped.height) // 2))

os.makedirs(OUT, exist_ok=True)
canvas.resize((1024, 1024), Image.NEAREST).save(os.path.join(OUT, "icon.png"))
with open(os.path.join(OUT, "icon-256.rgba"), "wb") as f:
    f.write(canvas.resize((256, 256), Image.NEAREST).tobytes())
canvas.resize((256, 256), Image.NEAREST).save(
    os.path.join(OUT, "icon.ico"),
    sizes=[(16, 16), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)],
)
iconset = os.path.join(OUT, "icon.iconset")
os.makedirs(iconset, exist_ok=True)
for s, name in [
    (16, "icon_16x16.png"), (32, "icon_16x16@2x.png"),
    (32, "icon_32x32.png"), (64, "icon_32x32@2x.png"),
    (128, "icon_128x128.png"), (256, "icon_128x128@2x.png"),
    (256, "icon_256x256.png"), (512, "icon_256x256@2x.png"),
    (512, "icon_512x512.png"), (1024, "icon_512x512@2x.png"),
]:
    canvas.resize((s, s), Image.NEAREST).save(os.path.join(iconset, name))
print("done: icon.png icon-256.rgba icon.ico icon.iconset/ -> %s" % OUT)
print("macOS: iconutil -c icns %s -o %s && rm -rf %s" % (iconset, os.path.join(OUT, "icon.icns"), iconset))

"""生成 Sonic Tray 的多尺寸 ICO 资源（4 个音量等级 × 浅/深主题）。

图形按 Windows 11 托盘音量图标的视觉比例手绘：**描边的扬声器** + 若干道声波，
静音时把声波换成叉号。扬声器部分在所有等级中完全一致，只有声波数量变化。

每个尺寸独立光栅化（8 倍超采样后 LANCZOS 缩小），因此 16/20/24/32 px 下
线条依然分开、不粘连。

输出（assets/）：
    volume_muted_light.ico / volume_low_light.ico / volume_medium_light.ico / volume_high_light.ico
    volume_muted_dark.ico  / volume_low_dark.ico  / volume_medium_dark.ico  / volume_high_dark.ico
    app.ico（exe 图标，含 256 PNG 帧）

用法：
    python tools/gen_icons.py     # 同时输出 tools/icon_preview.png 供目视检查
"""

from __future__ import annotations

import io
import math
import os
import struct

from PIL import Image, ImageDraw

GRID = 24.0            # 设计栅格：24×24 逻辑单位
SS = 8                 # 超采样倍数
ICON_SIZES = (16, 20, 24, 32, 40, 48, 64)

GLYPH_DARK = (0x17, 0x17, 0x17)   # 浅色任务栏上的深色图形
GLYPH_LIGHT = (0xFF, 0xFF, 0xFF)  # 深色任务栏上的浅色图形

# --- 扬声器（描边，所有等级完全一致）--------------------------------------
SPEAKER_PATH = ((0.9, 9.0), (4.4, 9.0), (9.4, 4.3), (9.4, 19.7), (4.4, 15.0), (0.9, 15.0))
SPEAKER_STROKE = 1.5

# --- 声波（同心圆弧，从内向外依次出现）------------------------------------
WAVE_CENTER = (9.0, 12.0)
WAVE_RADII = (4.9, 9.4, 13.9)
WAVE_HALF_ANGLE = 38.0
WAVE_STROKE = 1.5

# --- 静音叉号 -------------------------------------------------------------
X_LINES = ((14.2, 8.2, 21.4, 15.4), (21.4, 8.2, 14.2, 15.4))
X_STROKE = 1.6

#: 等级名 -> 声波数量（0 表示画叉号）
LEVELS = (("muted", 0), ("low", 1), ("medium", 2), ("high", 3))


def _disc(draw: ImageDraw.ImageDraw, x: float, y: float, radius: float, color, u: float) -> None:
    r = radius * u
    draw.ellipse(((x * u) - r, (y * u) - r, (x * u) + r, (y * u) + r), fill=color)


def _stroke_path(draw, points, stroke: float, color, u: float) -> None:
    """闭合折线描边：直线段 + 顶点圆点，得到圆角连接。"""
    pts = [(x * u, y * u) for x, y in points]
    width = max(1, int(round(stroke * u)))
    draw.line(pts + [pts[0]], fill=color, width=width, joint="curve")
    for x, y in points:
        _disc(draw, x, y, stroke / 2.0, color, u)


def _stroke_arc(draw, center, radius: float, half_angle: float, stroke: float, color, u: float) -> None:
    """圆弧描边：沿弧线铺圆点，端点自然是圆头。"""
    steps = max(16, int(radius * half_angle))
    for i in range(steps + 1):
        angle = math.radians(-half_angle + 2.0 * half_angle * i / steps)
        _disc(
            draw,
            center[0] + radius * math.cos(angle),
            center[1] + radius * math.sin(angle),
            stroke / 2.0,
            color,
            u,
        )


def _stroke_line(draw, x0: float, y0: float, x1: float, y1: float, stroke: float, color, u: float) -> None:
    draw.line((x0 * u, y0 * u, x1 * u, y1 * u), fill=color, width=max(1, int(round(stroke * u))))
    _disc(draw, x0, y0, stroke / 2.0, color, u)
    _disc(draw, x1, y1, stroke / 2.0, color, u)


def render(size: int, color: tuple[int, int, int], waves: int) -> Image.Image:
    """在给定像素尺寸下渲染一张 RGBA 图标。"""
    px = size * SS
    img = Image.new("RGBA", (px, px), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    u = px / GRID
    ink = color + (255,)

    _stroke_path(draw, SPEAKER_PATH, SPEAKER_STROKE, ink, u)

    if waves == 0:
        for x0, y0, x1, y1 in X_LINES:
            _stroke_line(draw, x0, y0, x1, y1, X_STROKE, ink, u)
    else:
        for radius in WAVE_RADII[:waves]:
            _stroke_arc(draw, WAVE_CENTER, radius, WAVE_HALF_ANGLE, WAVE_STROKE, ink, u)

    return img.resize((size, size), Image.LANCZOS)


def _bmp_frame(img: Image.Image) -> bytes:
    """把 RGBA 图写成 ICO 使用的 32bpp DIB（BITMAPINFOHEADER + BGRA + AND 掩码）。"""
    w, h = img.size
    px = img.load()
    xor = bytearray()
    for y in range(h - 1, -1, -1):          # DIB 自下而上
        for x in range(w):
            r, g, b, a = px[x, y]
            xor += bytes((b, g, r, a))
    mask_stride = ((w + 31) // 32) * 4
    and_mask = bytes(mask_stride * h)       # 全 0：完全依赖 alpha 通道
    header = struct.pack("<IiiHHIIiiII", 40, w, h * 2, 1, 32, 0, len(xor), 0, 0, 0, 0)
    return header + bytes(xor) + and_mask


def _png_frame(img: Image.Image) -> bytes:
    buf = io.BytesIO()
    img.save(buf, format="PNG", optimize=True)
    return buf.getvalue()


def write_ico(path: str, frames: list[tuple[int, bytes]]) -> None:
    """按 ICO 容器格式打包：ICONDIR + ICONDIRENTRY*n + 图像数据。"""
    header = struct.pack("<HHH", 0, 1, len(frames))
    offset = 6 + 16 * len(frames)
    entries = bytearray()
    payload = bytearray()
    for size, data in frames:
        entries += struct.pack(
            "<BBBBHHII",
            size if size < 256 else 0,
            size if size < 256 else 0,
            0, 0, 1, 32, len(data), offset,
        )
        payload += data
        offset += len(data)
    with open(path, "wb") as fp:
        fp.write(header + bytes(entries) + bytes(payload))


def build(path: str, color: tuple[int, int, int], waves: int, with_256: bool = False) -> None:
    frames = [(s, _bmp_frame(render(s, color, waves))) for s in ICON_SIZES]
    if with_256:
        frames.append((256, _png_frame(render(256, color, waves))))
    write_ico(path, frames)


def build_preview(path: str) -> None:
    """人工检查用：4 个等级 × 2 种主题，含实际尺寸与放大视图。"""
    cell_w, cell_h = 104, 104
    pad = 10
    cols = [(16, 1), (20, 1), (24, 1), (32, 1), (16, 4), (24, 3), (32, 2)]
    rows = [(name, waves, theme) for theme in ("light", "dark") for name, waves in LEVELS]
    width = pad * 2 + cell_w * len(cols)
    height = pad * 2 + cell_h * len(rows)
    sheet = Image.new("RGB", (width, height), (120, 120, 120))

    for row, (_name, waves, theme) in enumerate(rows):
        light_bg = theme == "light"
        bg = (244, 244, 244) if light_bg else (32, 32, 32)
        ink = GLYPH_DARK if light_bg else GLYPH_LIGHT
        top = pad + row * cell_h
        ImageDraw.Draw(sheet).rectangle([pad, top, width - pad, top + cell_h - 4], fill=bg)
        for col, (size, scale) in enumerate(cols):
            icon = render(size, ink, waves)
            big = icon if scale == 1 else icon.resize((size * scale, size * scale), Image.NEAREST)
            x = pad + col * cell_w + (cell_w - big.width) // 2
            y = top + (cell_h - 4 - big.height) // 2
            sheet.paste(big, (x, y), big)

    sheet.save(path)


def main() -> None:
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    assets = os.path.join(root, "assets")
    os.makedirs(assets, exist_ok=True)

    for theme, color in (("light", GLYPH_DARK), ("dark", GLYPH_LIGHT)):
        for name, waves in LEVELS:
            path = os.path.join(assets, f"volume_{name}_{theme}.ico")
            build(path, color, waves)
            print(f"{os.path.basename(path):26s} {os.path.getsize(path):7d} bytes")

    app_icon = os.path.join(assets, "app.ico")
    build(app_icon, GLYPH_DARK, waves=3, with_256=True)
    print(f"{'app.ico':26s} {os.path.getsize(app_icon):7d} bytes")
    build_preview(os.path.join(root, "tools", "icon_preview.png"))


if __name__ == "__main__":
    main()

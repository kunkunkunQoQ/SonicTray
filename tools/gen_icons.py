"""生成 Sonic Tray 的四个多尺寸 ICO 资源（以及 exe 图标）。

图形按 Windows 11 音量图标的视觉比例手工绘制：实心扬声器 + 右侧两道圆弧（正常），
或实心扬声器 + 右侧叉号（静音）。每个尺寸独立光栅化（8 倍超采样后 LANCZOS 缩小），
因此 16/20/24/32 px 下边缘依然清晰。

输出（assets/）：
    volume_light.ico  浅色任务栏用（深色图形）
    volume_dark.ico   深色任务栏用（浅色图形）
    mute_light.ico    浅色任务栏用（深色图形）
    mute_dark.ico     深色任务栏用（浅色图形）
    app.ico           exe 自身图标（含 256 尺寸 PNG 帧）

用法：
    python tools/gen_icons.py            # 生成 assets/*.ico 与 tools/icon_preview.png
"""

from __future__ import annotations

import math
import os
import struct

from PIL import Image, ImageDraw

GRID = 24.0            # 设计栅格：24×24 逻辑单位
SS = 8                 # 超采样倍数
ICON_SIZES = (16, 20, 24, 32, 40, 48, 64)

GLYPH_DARK = (0x17, 0x17, 0x17)   # 浅色任务栏上的深色图形
GLYPH_LIGHT = (0xFF, 0xFF, 0xFF)  # 深色任务栏上的浅色图形

# --- 几何参数（24×24 栅格）------------------------------------------------
BODY_BOX = (2.2, 9.2, 6.4, 14.8)   # 扬声器箱体
BODY_RADIUS = 0.85
CONE = ((6.0, 9.2), (11.6, 3.4), (13.0, 4.4), (13.0, 19.6), (11.6, 20.6), (6.0, 14.8))
WAVE_CENTER = (13.3, 12.0)
WAVE_RADII = (3.5, 7.1)
WAVE_HALF_ANGLE = 45.0             # 度
WAVE_STROKE = 1.95
X_STROKE = 2.1
X_BOX = (15.3, 9.3, 21.0, 15.0)


def _stroke_arc(draw: ImageDraw.ImageDraw, center, radius, half_angle, stroke, color, u):
    """用圆点沿弧线铺点，得到带圆头端帽的均匀描边。"""
    steps = max(12, int(radius * half_angle))
    dab = stroke * u
    for i in range(steps + 1):
        ang = math.radians(-half_angle + 2.0 * half_angle * i / steps)
        x = (center[0] + radius * math.cos(ang)) * u
        y = (center[1] + radius * math.sin(ang)) * u
        r = dab / 2.0
        draw.ellipse((x - r, y - r, x + r, y + r), fill=color)


def _stroke_line(draw: ImageDraw.ImageDraw, p0, p1, stroke, color, u):
    """粗线 + 两端圆帽。"""
    draw.line((p0[0] * u, p0[1] * u, p1[0] * u, p1[1] * u), fill=color, width=max(1, int(round(stroke * u))))
    r = stroke * u / 2.0
    for p in (p0, p1):
        draw.ellipse((p[0] * u - r, p[1] * u - r, p[0] * u + r, p[1] * u + r), fill=color)


def render(size: int, color: tuple[int, int, int], muted: bool) -> Image.Image:
    """在给定像素尺寸下渲染一张 RGBA 图标。"""
    px = size * SS
    img = Image.new("RGBA", (px, px), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    u = px / GRID
    fill = color + (255,)

    # 扬声器：箱体 + 锥体
    draw.rounded_rectangle(
        [BODY_BOX[0] * u, BODY_BOX[1] * u, BODY_BOX[2] * u, BODY_BOX[3] * u],
        radius=BODY_RADIUS * u,
        fill=fill,
    )
    draw.polygon([(x * u, y * u) for x, y in CONE], fill=fill)

    if muted:
        p0 = (X_BOX[0], X_BOX[1])
        p1 = (X_BOX[2], X_BOX[3])
        _stroke_line(draw, p0, p1, X_STROKE, fill, u)
        _stroke_line(draw, (p1[0], p0[1]), (p0[0], p1[1]), X_STROKE, fill, u)
    else:
        for radius in WAVE_RADII:
            _stroke_arc(draw, WAVE_CENTER, radius, WAVE_HALF_ANGLE, WAVE_STROKE, fill, u)

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
    import io

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


def build(path: str, color: tuple[int, int, int], muted: bool, with_256: bool = False) -> None:
    frames = [(s, _bmp_frame(render(s, color, muted))) for s in ICON_SIZES]
    if with_256:
        frames.append((256, _png_frame(render(256, color, muted))))
    write_ico(path, frames)


def build_preview(path: str) -> None:
    """生成人工检查用的预览图：4 种状态 × 实际尺寸 / 放大视图。"""
    cell_w, cell_h = 96, 96
    pad = 12
    cols = [
        (16, 1), (20, 1), (24, 1), (32, 1),   # 真实像素尺寸
        (16, 4), (24, 3), (32, 2),            # 2~4 倍放大，便于看形状
    ]
    rows = [
        ("volume on light", (244, 244, 244), GLYPH_DARK, False),
        ("mute on light", (244, 244, 244), GLYPH_DARK, True),
        ("volume on dark", (32, 32, 32), GLYPH_LIGHT, False),
        ("mute on dark", (32, 32, 32), GLYPH_LIGHT, True),
    ]
    width = pad * 2 + cell_w * len(cols)
    height = pad * 2 + cell_h * len(rows)
    sheet = Image.new("RGB", (width, height), (128, 128, 128))
    for row, (_label, bg, color, muted) in enumerate(rows):
        top = pad + row * cell_h
        ImageDraw.Draw(sheet).rectangle([pad, top, width - pad, top + cell_h - 4], fill=bg)
        for col, (size, scale) in enumerate(cols):
            icon = render(size, color, muted)
            big = icon if scale == 1 else icon.resize((size * scale, size * scale), Image.NEAREST)
            x = pad + col * cell_w + (cell_w - big.width) // 2
            y = top + (cell_h - 4 - big.height) // 2
            sheet.paste(big, (x, y), big)
    sheet.save(path)


def main() -> None:
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    assets = os.path.join(root, "assets")
    os.makedirs(assets, exist_ok=True)

    build(os.path.join(assets, "volume_light.ico"), GLYPH_DARK, muted=False)
    build(os.path.join(assets, "mute_light.ico"), GLYPH_DARK, muted=True)
    build(os.path.join(assets, "volume_dark.ico"), GLYPH_LIGHT, muted=False)
    build(os.path.join(assets, "mute_dark.ico"), GLYPH_LIGHT, muted=True)
    build(os.path.join(assets, "app.ico"), GLYPH_DARK, muted=False, with_256=True)
    build_preview(os.path.join(root, "tools", "icon_preview.png"))

    for name in ("volume_light.ico", "mute_light.ico", "volume_dark.ico", "mute_dark.ico", "app.ico"):
        p = os.path.join(assets, name)
        print(f"{name:18s} {os.path.getsize(p):7d} bytes")
    print("icon_preview.png 生成于 tools/")


if __name__ == "__main__":
    main()

"""从设计几何重绘 codex-notify 图标，生成多尺寸 .ico。

为什么不直接缩 1024 PNG：
- 位图缩小到 16/24/32 会把 2px 级别的描边糊成灰边；
- 重绘可以按尺寸做"光学补偿"（小尺寸稍微加粗描边），并用 8 倍超采样做抗锯齿。

几何尺寸全部以 512 基准表达，和 Ardot 画布上的设计稿一一对应。
"""

from PIL import Image, ImageDraw

# ── 设计参数（512 基准）────────────────────────────────────────────
TILE_RADIUS = 116
GRAD_TOP = (0x1B, 0x29, 0x38)
GRAD_BOTTOM = (0x0C, 0x14, 0x1D)
TILE_STROKE_A = 0.10          # 顶部高光描边（白色 10%）

BUBBLE_BOX = (100, 184, 272, 200)   # x, y, w, h
BUBBLE_R = 68
BUBBLE_BL = 16                # 左下角锐角 —— 气泡的"尾巴"

CHEV_ORIGIN = (182, 221)
CHEV_PTS = [(20, 16), (84, 63), (20, 110)]
CHEV_W = 26
CHEV_COLOR = (0x0A, 0x10, 0x18)

BADGE_C = (382.5, 173.5)
BADGE_R = 36
BADGE_RING = 7
BADGE_COLOR = (0x2B, 0xD2, 0xFF)
RING_COLOR = (0x0C, 0x14, 0x1D)

BUBBLE_COLOR = (0xF5, 0xFA, 0xFF)

BASE = 512


def rrect_mask(size, r_tl, r_tr, r_br, r_bl, steps=40):
    """支持四角不同半径的圆角矩形遮罩。

    用多边形沿圆弧描边，避免"矩形拼接"在四角半径不一致时留下缺口。
    """
    import math
    w, h = size

    def arc(cx, cy, r, a0, a1):
        if r <= 0:
            return [(cx, cy)]
        return [
            (cx + r * math.cos(math.radians(a0 + (a1 - a0) * i / steps)),
             cy + r * math.sin(math.radians(a0 + (a1 - a0) * i / steps)))
            for i in range(steps + 1)
        ]

    pts = []
    pts += arc(w - r_tr, r_tr, r_tr, 270, 360)   # 右上
    pts += arc(w - r_br, h - r_br, r_br, 0, 90)  # 右下
    pts += arc(r_bl, h - r_bl, r_bl, 90, 180)    # 左下
    pts += arc(r_tl, r_tl, r_tl, 180, 270)       # 左上

    m = Image.new("L", (w, h), 0)
    ImageDraw.Draw(m).polygon(pts, fill=255)
    return m


def render(size, boost=1.0, mark=1.0, min_chev=0.0, min_dot=0.0):
    """在 size×size 上重绘图标，内部按 8x 超采样。

    mark      —— 图形整体缩放（小尺寸稍微放大，换来更多白块面积）
    min_chev  —— chevron 描边的最小像素宽度（16px 下按比例只有 0.8px，必须兜底）
    min_dot   —— 圆点的最小直径
    """
    ss = 8 if size <= 128 else 4
    S = size * ss
    k = S / BASE                      # 512 基准 → 超采样画布
    c = BASE / 2

    def px(v):                        # 512 坐标 → 超采样像素（含整体缩放）
        return (c + (v - c) * mark) * k

    canvas = Image.new("RGBA", (S, S), (0, 0, 0, 0))

    # 1. 砖块：竖向渐变 + 圆角遮罩
    grad = Image.linear_gradient("L").resize((S, S))
    grad_rgb = Image.composite(
        Image.new("RGB", (S, S), GRAD_BOTTOM),
        Image.new("RGB", (S, S), GRAD_TOP),
        grad,
    )
    tile_mask = rrect_mask((S, S), *(round(TILE_RADIUS * k),) * 4)
    canvas.paste(grad_rgb, (0, 0), tile_mask)

    # 2. 砖块内描边（白色 10%，2px）
    stroke = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    sd = ImageDraw.Draw(stroke)
    sw = max(1, round(2 * k))
    sd.rounded_rectangle(
        [0, 0, S - 1, S - 1],
        radius=TILE_RADIUS * k,
        outline=(255, 255, 255, int(255 * TILE_STROKE_A)),
        width=sw,
    )
    canvas = Image.alpha_composite(canvas, Image.composite(stroke, Image.new("RGBA", (S, S), (0, 0, 0, 0)), tile_mask))

    # 3. 气泡
    bx, by = px(BUBBLE_BOX[0]), px(BUBBLE_BOX[1])
    bw, bh = BUBBLE_BOX[2] * mark * k, BUBBLE_BOX[3] * mark * k
    bubble_mask = rrect_mask(
        (round(bw), round(bh)),
        round(BUBBLE_R * mark * k), round(BUBBLE_R * mark * k),
        round(BUBBLE_R * mark * k), round(BUBBLE_BL * mark * k),
    )
    bubble = Image.new("RGBA", (round(bw), round(bh)), BUBBLE_COLOR + (255,))
    canvas.paste(bubble, (round(bx), round(by)), bubble_mask)

    # 4. chevron：折线 + 圆头圆角
    draw = ImageDraw.Draw(canvas)
    w = CHEV_W * mark * k * boost
    w = max(w, min_chev * ss)
    pts = [(px(CHEV_ORIGIN[0] + ox), px(CHEV_ORIGIN[1] + oy)) for ox, oy in CHEV_PTS]
    draw.line(pts, fill=CHEV_COLOR + (255,), width=round(w), joint="curve")
    for p in (pts[0], pts[-1]):                 # 圆头端点
        draw.ellipse([p[0] - w / 2, p[1] - w / 2, p[0] + w / 2, p[1] + w / 2], fill=CHEV_COLOR + (255,))

    # 5. 未读圆点：先铺外圈（描边色），再画点
    cx, cy = px(BADGE_C[0]), px(BADGE_C[1])
    dot_r = max(BADGE_R * mark * k, min_dot * ss / 2)
    ring_r = dot_r + BADGE_RING * mark * k
    draw.ellipse([cx - ring_r, cy - ring_r, cx + ring_r, cy + ring_r], fill=RING_COLOR + (255,))
    draw.ellipse([cx - dot_r, cy - dot_r, cx + dot_r, cy + dot_r], fill=BADGE_COLOR + (255,))

    return canvas.resize((size, size), Image.LANCZOS)


# 小尺寸做光学补偿：按比例缩下去描边会细到发灰。
# 32 以下除了加粗，还要把整体略微放大，保住白块面积。
HINTS = {
    16: dict(boost=1.0, mark=1.10, min_chev=1.55, min_dot=3.0),
    24: dict(boost=1.12, mark=1.06, min_chev=1.55, min_dot=2.8),
    32: dict(boost=1.12, mark=1.0),
    48: dict(boost=1.04, mark=1.0),
}

if __name__ == "__main__":
    import sys
    out = sys.argv[1] if len(sys.argv) > 1 else "."
    for s in (512, 256, 128, 64, 48, 32, 24, 16):
        im = render(s, **HINTS.get(s, {}))
        im.save(f"{out}/icon-{s}.png")
        print(f"rendered {s}x{s}")

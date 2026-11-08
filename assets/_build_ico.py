"""把 codex-notify 图标打包成多尺寸 Windows .ico。

几何来自 _build_icon.py，全程在内存里渲染，不落中间文件。
用法：python _build_ico.py   →  codex-notify.ico

ICO 条目 256 用 PNG（省体积），其余用 32 位带 Alpha 的 BMP
—— 资源编译器（rc.exe / windres）对 BMP 条目的兼容性最好。
"""

import io
import struct

from PIL import Image

from _build_icon import HINTS, render

SIZES = [256, 128, 64, 48, 32, 24, 16]


def dib_bytes(img):
    """32 位 BGRA 的 DIB：BITMAPINFOHEADER + 自下而上的像素 + 全零 AND 掩码。"""
    w, h = img.size
    px = img.convert("RGBA")
    src = px.load()
    rows = []
    for y in range(h - 1, -1, -1):                     # BMP 自下而上
        row = bytearray()
        for x in range(w):
            r, g, b, a = src[x, y]
            row += bytes((b, g, r, a))                 # BGRA
        rows.append(bytes(row))
    xor = b"".join(rows)
    and_stride = ((w + 31) // 32) * 4
    and_mask = b"\x00" * (and_stride * h)              # Alpha 生效，掩码留空

    header = struct.pack(
        "<IiiHHIIiiII",
        40,          # biSize
        w, h * 2,    # biWidth, biHeight（XOR + AND 两倍高）
        1, 32,       # biPlanes, biBitCount
        0,           # biCompression = BI_RGB
        len(xor) + len(and_mask),
        0, 0, 0, 0,
    )
    return header + xor + and_mask


def frame(size):
    """渲染一个尺寸并编码成 ico 条目数据。"""
    img = render(size, **HINTS.get(size, {}))
    if size >= 256:
        buf = io.BytesIO()
        img.save(buf, format="PNG", optimize=True)
        return buf.getvalue()
    return dib_bytes(img)


def build(path):
    blobs = [(s, frame(s)) for s in SIZES]
    with open(path, "wb") as out:
        out.write(struct.pack("<HHH", 0, 1, len(blobs)))
        offset = 6 + 16 * len(blobs)
        for s, data in blobs:
            out.write(struct.pack(
                "<BBBBHHII",
                s if s < 256 else 0,
                s if s < 256 else 0,
                0, 0,      # 调色板数、保留位
                1, 32,     # planes, bitCount
                len(data), offset,
            ))
            offset += len(data)
        for _, data in blobs:
            out.write(data)
    return path


if __name__ == "__main__":
    print("wrote", build("codex-notify.ico"))

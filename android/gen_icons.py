#!/usr/bin/env python3
"""Regenerate Android launcher PNGs from docs/logo-432.png.

Downscales the 432px RGBA source to every density bucket with bilinear
filtering (stdlib only: struct + zlib). Re-run after touching the logo:
    python3 android/gen_icons.py
Densities follow the adaptive-icon foreground convention (108dp @mdpi).
"""
import struct
import sys
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "docs" / "logo-432.png"
RES = ROOT / "android" / "app" / "src" / "main" / "res"
SIZES = {"mdpi": 108, "hdpi": 162, "xhdpi": 216, "xxhdpi": 324, "xxxhdpi": 432}


def read_png(p: Path):
    d = p.read_bytes()
    assert d[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
    pos, w, h, bitd, ctype, idat = 8, 0, 0, 0, 0, b""
    while pos < len(d):
        (ln,) = struct.unpack(">I", d[pos:pos + 4])
        typ = d[pos + 4:pos + 8]
        data = d[pos + 8:pos + 8 + ln]
        if typ == b"IHDR":
            w, h, bitd, ctype, _, _, _ = struct.unpack(">IIBBBBB", data)
        elif typ == b"IDAT":
            idat += data
        elif typ == b"IEND":
            break
        pos += 12 + ln
    assert bitd == 8 and ctype == 6, f"need 8-bit RGBA, got depth={bitd} type={ctype}"
    raw = zlib.decompress(idat)
    ch = 4
    stride = w * ch
    px = bytearray(w * h * ch)
    prev = bytearray(stride)
    pos = 0
    for y in range(h):
        f = raw[pos]
        pos += 1
        line = bytearray(raw[pos:pos + stride])
        pos += stride
        recon = bytearray(stride)
        for x in range(stride):
            a = recon[x - ch] if x >= ch else 0
            b = prev[x]
            c = prev[x - ch] if x >= ch else 0
            v = line[x]
            if f == 1:
                v = (v + a) & 0xFF
            elif f == 2:
                v = (v + b) & 0xFF
            elif f == 3:
                v = (v + (a + b) // 2) & 0xFF
            elif f == 4:
                p_ = a + b - c
                pa, pb, pc = abs(p_ - a), abs(p_ - b), abs(p_ - c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                v = (v + pr) & 0xFF
            recon[x] = v
        px[y * stride:(y + 1) * stride] = recon
        prev = recon
    return w, h, px


def write_png(p: Path, w: int, h: int, px: bytes):
    def chunk(typ: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + typ + data + struct.pack(">I", zlib.crc32(typ + data))
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)
    stride = w * 4
    raw = bytearray()
    for y in range(h):
        raw.append(0)
        raw += px[y * stride:(y + 1) * stride]
    out = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(bytes(raw))) + chunk(b"IEND", b"")
    p.write_bytes(out)


def downscale(w: int, h: int, px: bytearray, nw: int, nh: int) -> bytes:
    out = bytearray(nw * nh * 4)
    for y in range(nh):
        for x in range(nw):
            sx, sy = (x + 0.5) * w / nw - 0.5, (y + 0.5) * h / nh - 0.5
            x0, y0 = max(0, int(sx)), max(0, int(sy))
            x1, y1 = min(w - 1, x0 + 1), min(h - 1, y0 + 1)
            fx, fy = min(1.0, max(0.0, sx - x0)), min(1.0, max(0.0, sy - y0))
            acc = [0.0] * 4
            for yy, wy in ((y0, 1 - fy), (y1, fy)):
                for xx, wx in ((x0, 1 - fx), (x1, fx)):
                    o = (yy * w + xx) * 4
                    for c in range(4):
                        acc[c] += px[o + c] * wx * wy
            o = (y * nw + x) * 4
            for c in range(4):
                out[o + c] = int(acc[c] + 0.5)
    return bytes(out)


def main() -> None:
    w, h, px = read_png(SRC)
    assert (w, h) == (432, 432), f"source must be 432x432, got {w}x{h}"
    for bucket, size in SIZES.items():
        d = RES / f"drawable-{bucket}"
        d.mkdir(parents=True, exist_ok=True)
        out = downscale(w, h, px, size, size) if size != w else bytes(px)
        write_png(d / "ic_logo_fg.png", size, size, out)
        print(f"drawable-{bucket}/ic_logo_fg.png ({size}x{size})")


if __name__ == "__main__":
    sys.exit(main())

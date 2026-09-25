"""Generates the application icons with nothing but the standard library.

Run from this directory:  python make_icons.py

The icon is a small branching network on a green tile: a rhizome, a root system
with no centre. Shapes are drawn as signed-distance fields and supersampled, so
the edges are smooth at every size without an imaging library.
"""

import math
import os
import struct
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "icons")

TILE = (0x1F, 0x5F, 0x4A)  # deep green
TILE_HI = (0x2E, 0x8B, 0x6A)  # lighter green, for a subtle gradient
INK = (0xF4, 0xF1, 0xE8)  # warm off-white

# Nodes and the roots joining them, in a 0..1 square.
NODES = [
    (0.50, 0.30, 0.085),
    (0.27, 0.52, 0.065),
    (0.73, 0.50, 0.065),
    (0.40, 0.74, 0.055),
    (0.62, 0.76, 0.055),
    (0.16, 0.76, 0.04),
    (0.84, 0.74, 0.04),
]
EDGES = [(0, 1), (0, 2), (1, 3), (2, 4), (1, 5), (2, 6)]
ROOT_WIDTH = 0.026


def dist_segment(px, py, ax, ay, bx, by):
    dx, dy = bx - ax, by - ay
    length2 = dx * dx + dy * dy
    t = 0.0 if length2 == 0 else max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / length2))
    return math.hypot(px - (ax + t * dx), py - (ay + t * dy))


def rounded_square(px, py, radius):
    """Signed distance to a rounded square filling the unit square."""
    qx, qy = abs(px - 0.5) - (0.5 - radius), abs(py - 0.5) - (0.5 - radius)
    return math.hypot(max(qx, 0.0), max(qy, 0.0)) + min(max(qx, qy), 0.0) - radius


def shade(px, py):
    """Returns (r, g, b, a) at a point in the unit square, before antialiasing."""
    if rounded_square(px, py, 0.22) > 0:
        return (0, 0, 0, 0)
    mix = min(1.0, max(0.0, (px + (1 - py)) / 2))
    base = tuple(TILE[i] + (TILE_HI[i] - TILE[i]) * mix * 0.6 for i in range(3))

    ink = min(
        min(math.hypot(px - x, py - y) - r for x, y, r in NODES),
        min(
            dist_segment(px, py, NODES[a][0], NODES[a][1], NODES[b][0], NODES[b][1]) - ROOT_WIDTH
            for a, b in EDGES
        ),
    )
    if ink <= 0:
        return (*INK, 255)
    return (*(int(c) for c in base), 255)


def render(size, samples=3):
    rows = []
    for y in range(size):
        row = bytearray()
        for x in range(size):
            r = g = b = a = 0.0
            for sy in range(samples):
                for sx in range(samples):
                    px = (x + (sx + 0.5) / samples) / size
                    py = (y + (sy + 0.5) / samples) / size
                    cr, cg, cb, ca = shade(px, py)
                    # Premultiply so transparent samples do not darken the edge.
                    r += cr * ca
                    g += cg * ca
                    b += cb * ca
                    a += ca
            n = samples * samples
            if a == 0:
                row += bytes((0, 0, 0, 0))
            else:
                row += bytes((int(r / a), int(g / a), int(b / a), int(a / n)))
        rows.append(bytes(row))
    return rows


def png(size):
    rows = render(size)
    raw = b"".join(b"\x00" + row for row in rows)

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def ico(frames):
    """An .ico holding PNG-compressed frames (supported since Windows Vista)."""
    header = struct.pack("<HHH", 0, 1, len(frames))
    offset = 6 + 16 * len(frames)
    directory, payload = b"", b""
    for size, data in frames:
        dim = 0 if size >= 256 else size  # 0 means 256 in the directory
        directory += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        payload += data
        offset += len(data)
    return header + directory + payload


def main():
    os.makedirs(OUT, exist_ok=True)
    cache = {size: png(size) for size in (16, 32, 48, 64, 128, 256, 512)}

    def write(name, data):
        with open(os.path.join(OUT, name), "wb") as f:
            f.write(data)
        print("wrote", name, len(data), "bytes")

    write("32x32.png", cache[32])
    write("128x128.png", cache[128])
    write("128x128@2x.png", cache[256])
    write("icon.png", cache[512])
    write("icon.ico", ico([(s, cache[s]) for s in (16, 32, 48, 64, 128, 256)]))


if __name__ == "__main__":
    main()

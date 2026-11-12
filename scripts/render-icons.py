#!/usr/bin/env python3
"""Render the app icon set from assets/icon.svg and assets/icon-light.svg.

Needs: pip install cairosvg   (uses the system cairo library)
Writes: apps/desktop/src-tauri/icons/{32x32,128x128,128x128@2x,icon}.png, icon.ico, icon.icns,
        tray.png, apps/desktop/ui/brand.png (dark) + brand-light.png, assets/icon.png (1024 master),
        assets/icon-light.png (512).
"""
import struct
from pathlib import Path

import cairosvg

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
ICONS = ROOT / "apps" / "desktop" / "src-tauri" / "icons"
UI = ROOT / "apps" / "desktop" / "ui"


def render(svg: Path, size: int) -> bytes:
    return cairosvg.svg2png(url=str(svg), output_width=size, output_height=size)


def ico(entries):
    head = struct.pack("<HHH", 0, 1, len(entries))
    off = 6 + 16 * len(entries)
    dirs, data = b"", b""
    for size, png in entries:
        dirs += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(png), off + len(data))
        data += png
    return head + dirs + data


def icns(entries):
    body = b"".join(tag + struct.pack(">I", 8 + len(png)) + png for tag, png in entries)
    return b"icns" + struct.pack(">I", 8 + len(body)) + body


def main():
    sizes = [16, 24, 32, 48, 64, 128, 256, 512, 1024]
    dark = {n: render(ASSETS / "icon.svg", n) for n in sizes}
    light = {n: render(ASSETS / "icon-light.svg", n) for n in sizes}
    ICONS.mkdir(parents=True, exist_ok=True)
    (ICONS / "32x32.png").write_bytes(dark[32])
    (ICONS / "128x128.png").write_bytes(dark[128])
    (ICONS / "128x128@2x.png").write_bytes(dark[256])
    (ICONS / "icon.png").write_bytes(dark[512])
    (ICONS / "tray.png").write_bytes(dark[32])
    (ICONS / "icon.ico").write_bytes(ico([(n, dark[n]) for n in (16, 24, 32, 48, 64, 128, 256)]))
    (ICONS / "icon.icns").write_bytes(icns([
        (b"ic07", dark[128]), (b"ic08", dark[256]), (b"ic09", dark[512]), (b"ic10", dark[1024]),
        (b"ic11", dark[32]), (b"ic12", dark[64]), (b"ic13", dark[256]), (b"ic14", dark[512]),
    ]))
    (UI / "brand.png").write_bytes(dark[64])
    (UI / "brand-light.png").write_bytes(light[64])
    (ASSETS / "icon.png").write_bytes(dark[1024])
    (ASSETS / "icon-light.png").write_bytes(light[512])
    print("icons rendered")


if __name__ == "__main__":
    main()

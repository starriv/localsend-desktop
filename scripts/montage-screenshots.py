#!/usr/bin/env python3
"""Compose the README hero montage from the Chinese screenshot tiles.

The README is Chinese, so its montage is captured with the app in `zh` by
`ScreenshotsZh.test.tsx` (see docs/BUILD.md); the English
`Screenshots.test.tsx` keeps the label-based assertions. This script reads the
nine tiles from `screenshots/zh/` and writes `docs/screenshots/overview.png`:
a 3 × 3 grid on the app's light background, rounded tiles with a 1 px border.

Docs-only tool, not part of the app build: it needs Pillow
(`python3 -m pip install pillow`).
"""

from __future__ import annotations

import sys
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent

# Geometry of the committed montage (kept identical when regenerating).
CANVAS = (2512, 1711)
MARGIN = 31
GAP = 22
TILE = (802, 535)
RADIUS = 15
BACKGROUND = (236, 237, 240)
BORDER = (210, 212, 218)

# Row-major; the tiles are the release screenshots of the same names.
TILES = [
    "send",
    "receive",
    "history",
    "incoming",
    "pin",
    "transfer-sheet",
    "settings",
    "send-dark",
    "history-dark",
]

SOURCE_DIR = ROOT / "screenshots" / "zh"
OUTPUT = ROOT / "docs" / "screenshots" / "overview.png"


def rounded_mask(size: tuple[int, int], radius: int, scale: int = 4) -> Image.Image:
    """A white rounded rectangle on black, anti-aliased by supersampling."""
    big = Image.new("L", (size[0] * scale, size[1] * scale), 0)
    draw = ImageDraw.Draw(big)
    draw.rounded_rectangle(
        (0, 0, size[0] * scale - 1, size[1] * scale - 1),
        radius=radius * scale,
        fill=255,
    )
    return big.resize(size, Image.LANCZOS)


def main() -> int:
    missing = [name for name in TILES if not (SOURCE_DIR / f"{name}.png").is_file()]
    if missing:
        print(
            f"missing tiles in {SOURCE_DIR.relative_to(ROOT)}: {', '.join(missing)}\n"
            "run ScreenshotsZh.test.tsx first (see docs/BUILD.md)",
            file=sys.stderr,
        )
        return 1

    canvas = Image.new("RGB", CANVAS, BACKGROUND)
    border = ImageDraw.Draw(canvas)
    mask = rounded_mask(TILE, RADIUS)
    for index, name in enumerate(TILES):
        tile = Image.open(SOURCE_DIR / f"{name}.png").convert("RGB")
        tile = tile.resize(TILE, Image.LANCZOS)
        column, row = index % 3, index // 3
        x = MARGIN + column * (TILE[0] + GAP)
        y = MARGIN + row * (TILE[1] + GAP)
        canvas.paste(tile, (x, y), mask)
        border.rounded_rectangle(
            (x, y, x + TILE[0] - 1, y + TILE[1] - 1),
            radius=RADIUS,
            outline=BORDER,
            width=1,
        )

    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    canvas.save(OUTPUT, optimize=True)
    print(f"wrote {OUTPUT.relative_to(ROOT)} ({CANVAS[0]}x{CANVAS[1]})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

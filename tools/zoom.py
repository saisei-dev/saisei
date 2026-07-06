#!/usr/bin/env python3
"""zoom.py — pick a fixed-grid chunk of a screenshot to look at closer.

The screen is divided into a 4-column x 4-row grid (16 chunks total).
Each chunk is 80x50 pixels for a 320x200 source. You name a chunk by
(col, row) where col is 0..3 (left to right) and row is 0..3 (top to
bottom). No rectangle picking, no upscaling — feeding a smaller crop
directly already gives more output tokens per source pixel.

In some programs the rows usually carry:
  row 0: sky / mountain tops
  row 1: tall scenery (building bodies, tree tops)
  row 2: walking band (player and door slots live here)
  row 3: HUD strip + foreground plants

Column 0..3 is left to right of the visible play area.

Usage:
  zoom.py SRC COL ROW [--out PATH]
"""
from __future__ import annotations

import argparse
import sys
from pathlib import Path

try:
    from PIL import Image
except ImportError:
    sys.exit("zoom: needs Pillow — `pip install Pillow`")

GRID_COLS = 4
GRID_ROWS = 4


def main() -> None:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("src", help="source PNG to crop")
    parser.add_argument(
        "col", type=int,
        help=f"grid column 0..{GRID_COLS - 1} (left to right)")
    parser.add_argument(
        "row", type=int,
        help=f"grid row 0..{GRID_ROWS - 1} (top to bottom)")
    parser.add_argument(
        "--out", default=None,
        help="output path (default: <src>_zoom_<col>_<row><ext>)")
    args = parser.parse_args()

    if not (0 <= args.col < GRID_COLS) or not (0 <= args.row < GRID_ROWS):
        sys.exit(f"zoom: col must be 0..{GRID_COLS - 1}, "
                 f"row must be 0..{GRID_ROWS - 1}")

    src_path = Path(args.src)
    if not src_path.exists():
        sys.exit(f"zoom: source not found: {src_path}")
    img = Image.open(src_path).convert("RGB")
    sx, sy = img.size

    cw, ch = sx // GRID_COLS, sy // GRID_ROWS
    x_lo = args.col * cw
    y_lo = args.row * ch
    # Extend the right/bottom-most chunk to absorb the integer-division remainder.
    x_hi = sx if args.col == GRID_COLS - 1 else x_lo + cw
    y_hi = sy if args.row == GRID_ROWS - 1 else y_lo + ch

    crop = img.crop((x_lo, y_lo, x_hi, y_hi))
    out = Path(args.out) if args.out else src_path.with_name(
        f"{src_path.stem}_zoom_{args.col}_{args.row}{src_path.suffix}"
    )
    out.parent.mkdir(parents=True, exist_ok=True)
    crop.save(out)
    print(out)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 singularity098
"""Writes the Modometer icon and banner as SVG to resources/branding/.

The text is drawn as outlines from resources/fonts/Jost.ttf, so the SVGs look the same
everywhere (GitHub shows SVGs without web fonts). Needs `pip install fonttools uharfbuzz`.
PNG copies are made with Chromium (see scripts/branding_png.js).
"""
import os
import uharfbuzz as hb
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FONT = os.path.join(ROOT, "resources", "fonts", "Jost.ttf")
OUT = os.path.join(ROOT, "resources", "branding")

TEXT = "#ece6f5"
BG = "#0d0b12"
TILE = "#15121c"
DIM = "#2a2433"
IRIS = ("#c688d6", "#a85eea", "#7278fe")

# The Velocity dots mark: an M in a 5x5 grid, each dot sized like a hit's velocity.
M_CELLS = {(0, r) for r in range(5)} | {(4, r) for r in range(5)} | {(1, 1), (2, 2), (3, 1)}


def stops():
    return "".join(f'<stop offset="{o}" stop-color="{c}"/>' for o, c in zip(("0", ".5", "1"), IRIS))


def mark_dots(dim=True):
    out = []
    for c in range(5):
        for r in range(5):
            on = (c, r) in M_CELLS
            if not on and not dim:
                continue
            rad = 5.6 - r * 0.45 if on else 1.8
            fill = "url(#iris)" if on else DIM
            out.append(f'<circle cx="{22 + c * 14}" cy="{22 + r * 14}" r="{rad:g}" fill="{fill}"/>')
    return "".join(out)


# Bounds of the lit dots in the mark's 100-unit box.
M_LEFT, M_RIGHT = 22 - 5.6, 78 + 5.6
M_TOP, M_BOTTOM = 22 - 5.6, 78 + (5.6 - 4 * 0.45)


def text_path(text, x, baseline, size, weight, spacing=0.0):
    """Shapes `text` with HarfBuzz and returns (svg path data, advance width)."""
    blob = hb.Blob.from_file_path(FONT)
    font = hb.Font(hb.Face(blob))
    font.set_variations({"wght": weight})
    buf = hb.Buffer()
    buf.add_str(text)
    buf.guess_segment_properties()
    hb.shape(font, buf, {"kern": True, "liga": True})
    scale = size / font.face.upem
    pen = SVGPathPen(None)
    cx = 0.0
    for info, pos in zip(buf.glyph_infos, buf.glyph_positions):
        gx = x + (cx + pos.x_offset) * scale
        gy = baseline - pos.y_offset * scale
        font.draw_glyph_with_pen(info.codepoint, TransformPen(pen, (scale, 0, 0, -scale, gx, gy)))
        cx += pos.x_advance + spacing / scale
    return pen.getCommands(), cx * scale


def icon_svg():
    return (
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" width="512" height="512">'
        f'<defs><linearGradient id="iris" gradientUnits="userSpaceOnUse" x1="14" y1="14" x2="86" y2="86">{stops()}</linearGradient></defs>'
        f'<rect x="1" y="1" width="98" height="98" rx="22" fill="{TILE}" stroke="{DIM}" stroke-width="1.2"/>'
        f"{mark_dots()}</svg>\n"
    )


def banner_svg():
    w, h = 1280, 320
    size, weight, baseline = 124, 300, 178
    cap = TTFont(FONT)["OS/2"].sCapHeight * size / 1000
    # The dot M is a capital set larger than the text: 1.45 x the cap height, sitting on the baseline.
    m_scale = cap * 1.45 / (M_BOTTOM - M_TOP)
    x0 = 80
    mx, my = x0 - M_LEFT * m_scale, baseline - M_BOTTOM * m_scale
    tx = x0 + (M_RIGHT - M_LEFT) * m_scale + size * 0.06
    word, _ = text_path("odometer", tx, baseline, size, weight, spacing=1)

    # The hit lane: a bar of drum hits along a line, sized by velocity.
    vel = [1, .3, .55, .3, .85, .3, .55, .3, 1, .3, .55, .65, .85, .4, .9, .6]
    lane_y, n = 262, 48
    step = (1200 - 92) / (n - 1)
    lane = [f'<line x1="{x0}" y1="{lane_y}" x2="1200" y2="{lane_y}" stroke="{DIM}" stroke-width="2"/>']
    import math
    for i in range(n):
        v = vel[i % 16] * (0.8 + 0.2 * math.sin(i / 5))
        lane.append(f'<circle cx="{92 + i * step:.1f}" cy="{lane_y}" r="{2 + v * 8:.2f}" fill="url(#lane)"/>')

    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}">'
        "<title>Modometer</title>"
        "<defs>"
        f'<linearGradient id="iris" gradientUnits="userSpaceOnUse" x1="14" y1="14" x2="86" y2="86">{stops()}</linearGradient>'
        f'<linearGradient id="lane" gradientUnits="userSpaceOnUse" x1="0" y1="0" x2="{w}" y2="0">{stops()}</linearGradient>'
        "</defs>"
        f'<rect width="{w}" height="{h}" rx="16" fill="{BG}"/>'
        f'<g transform="translate({mx:.2f} {my:.2f}) scale({m_scale:.4f})">{mark_dots(dim=False)}</g>'
        f'<path d="{word}" fill="{TEXT}"/>'
        f'{"".join(lane)}</svg>\n'
    )


if __name__ == "__main__":
    os.makedirs(OUT, exist_ok=True)
    for name, svg in (("icon.svg", icon_svg()), ("banner.svg", banner_svg())):
        with open(os.path.join(OUT, name), "w") as f:
            f.write(svg)
        print("wrote", os.path.join("resources", "branding", name))

#!/usr/bin/env python3
"""Write the performance graph: site/performance.py docs/performance.svg

The times are representative medians from benchmarks/BENCHMARKS.md; update
PANELS after a new measurement. Each panel has its own linear scale."""
import sys

FONT = "ui-sans-serif,system-ui,Segoe UI,Helvetica,Arial,sans-serif"
TEAL, GREY, INK = "#5fafaf", "#8a8a8a", "#17181a"
COLOURS = {"em": "#5faf5f", "emerge": "#d78787", "pkgcraft": "#d75f5f"}
PANELS = [
    ("Pretend and search, wall clock", 6000, 1000, [
        ("firefox -p", "em", 900, "0.9 s"),
        ("firefox -p", "emerge", 3700, "3.7 s"),
        ("libreoffice -p", "em", 1000, "1.0 s"),
        ("libreoffice -p", "emerge", 4000, "4.0 s"),
        ("five packages -p", "em", 1000, "1.0 s"),
        ("five packages -p", "emerge", 4700, "4.7 s"),
        ("gcc -s", "em", 100, "0.1 s"),
        ("gcc -s", "emerge", 5200, "5.2 s"),
    ]),
    ("Cache regeneration, 32k ebuilds, j=8", 60000, 10000, [
        ("regen", "em", 18100, "18.1 s"),
        ("regen", "pkgcraft", 48300, "48.3 s"),
    ]),
]
W, X0, BAR, ROW, BAR_H, SIZE = 720, 150, 520, 24, 14, 13
out = []


def text(x, y, value, anchor="start", fill=GREY, weight=None):
    w = f' font-weight="{weight}"' if weight else ""
    out.append(
        f'<text x="{x:.1f}" y="{y}" font-family="{FONT}" font-size="{SIZE}" '
        f'fill="{fill}" text-anchor="{anchor}"{w}>{value}</text>'
    )


def tick_label(ms):
    if ms == 0:
        return "0"
    return f"{ms // 1000} s" if ms >= 1000 else f"{ms} ms"


y = 0
for title, maximum, step, rows in PANELS:
    text(10, y + 18, title, fill=TEAL, weight=600)
    top, bottom = y + 30, y + 30 + ROW * len(rows)
    tick = 0
    while tick <= maximum:
        x = X0 + tick / maximum * BAR
        out.append(
            f'<line x1="{x:.1f}" y1="{top}" x2="{x:.1f}" y2="{bottom}" '
            f'stroke="{GREY}" stroke-opacity="0.35" stroke-width="1"/>'
        )
        text(x, bottom + 16, tick_label(tick), "middle")
        tick += step
    for n, (command, tool, ms, label) in enumerate(rows):
        ry = top + n * ROW + (ROW - BAR_H) // 2
        text(X0 - 8, ry + BAR_H - 3, f"{command} · {tool}", "end",
             INK if tool == "em" else GREY, 600 if tool == "em" else None)
        width = ms / maximum * BAR
        out.append(
            f'<rect x="{X0}" y="{ry}" width="{width:.1f}" height="{BAR_H}" '
            f'rx="2" fill="{COLOURS[tool]}"/>'
        )
        text(X0 + width + 6, ry + BAR_H - 3, label, fill=INK)
    y = bottom + 34
text(X0 + BAR / 2, y - 4, "wall clock, linear scale", "middle")
H = y + 12
svg = (
    f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="{round(H * 1024 / W)}" '
    f'viewBox="0 0 {W} {H}" role="img" aria-label="Wall-clock benchmarks: '
    f'em against emerge and pkgcraft on thalia.">\n'
    f'<rect width="{W}" height="{H}" fill="none"/>\n' + "\n".join(out) + "\n</svg>\n"
)
open(sys.argv[1], "w", encoding="utf-8").write(svg)

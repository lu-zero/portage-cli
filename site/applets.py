#!/usr/bin/env python3
"""Write the applet-coverage figure: site/applets.py docs/applets.svg

The counts come from the Applet status table in README.md."""
import pathlib
import re
import sys

FONT = "ui-sans-serif,system-ui,Segoe UI,Helvetica,Arial,sans-serif"
INK, MUTED, ACCENT = "#17181a", "#8a8a8a", "#5fafaf"
COLOURS = {"Working": "#5faf5f", "Partial": "#87afaf"}
W, X0, BAR, SIZE = 760, 120, 520, 13
out = []


def text(x, y, value, anchor="start", fill=MUTED, weight=None):
    w = f' font-weight="{weight}"' if weight else ""
    out.append(
        f'<text x="{x:.1f}" y="{y}" font-family="{FONT}" font-size="{SIZE}" '
        f'fill="{fill}" text-anchor="{anchor}"{w}>{value}</text>'
    )


def counts(path):
    result = dict.fromkeys(COLOURS, 0)
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.startswith("|"):
            continue
        cells = [cell.strip() for cell in line.strip("|").split("|")]
        if len(cells) != 3 or cells[0] == "Applet" or set(cells[0]) <= set("-:"):
            continue
        status = cells[2].split("—")[0].strip().split()[0]
        result[status] += 1
    return result


data = counts(pathlib.Path(__file__).resolve().parent.parent / "README.md")
total = sum(data.values())
text(10, 22, "Applet coverage", fill=ACCENT, weight=600)
text(10, 40, f"{total} applets from the README status table", fill=MUTED)

text(X0 - 10, 78, "em", "end", INK, 600)
x = X0
for status, colour in COLOURS.items():
    width = data[status] / total * BAR
    out.append(f'<rect x="{x:.1f}" y="59" width="{width:.1f}" height="18" fill="{colour}"/>')
    x += width
text(X0 + BAR + 12, 74, " · ".join(str(data[s]) for s in COLOURS), fill=INK)

for i, (status, colour) in enumerate(COLOURS.items()):
    x = 10 + i * 205
    out.append(f'<rect x="{x}" y="104" width="11" height="11" fill="{colour}"/>')
    text(x + 18, 114, status.lower())

H = 132
svg = (
    f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="{round(H * 1024 / W)}" '
    f'viewBox="0 0 {W} {H}" role="img" aria-label="Applet coverage: {total} applets, '
    f'{data["Working"]} working, {data["Partial"]} partial.">\n'
    f'<rect width="{W}" height="{H}" fill="none"/>\n' + "\n".join(out) + "\n</svg>\n"
)
open(sys.argv[1], "w", encoding="utf-8").write(svg)

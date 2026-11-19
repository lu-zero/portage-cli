#!/usr/bin/env python3
"""Write the option-parity figure: site/parity.py docs/parity.svg

Counts are long options of Portage 3.0.83 emerge and crossdev-20260623,
compared with em's generated CLI reference. Grouped forms such as
`--[bdgkl]cat` are expanded once. Recount when either side changes."""
import sys

FONT = "ui-sans-serif,system-ui,Segoe UI,Helvetica,Arial,sans-serif"
TEAL, GREY, INK = "#5fafaf", "#8a8a8a", "#17181a"
COLOURS = {"same": "#5faf5f", "different": "#87afaf", "missing": "#d75f5f"}
ROWS = [
    ("emerge", {"same": 56, "different": 6, "missing": 78}, 13),
    ("crossdev", {"same": 2, "different": 4, "missing": 66}, 51),
]
W, X0, SCALE, ROW, BAR = 760, 130, 4, 60, 18
out = []


def text(x, y, value, anchor="start", fill=GREY, weight=None):
    w = f' font-weight="{weight}"' if weight else ""
    out.append(
        f'<text x="{x:.1f}" y="{y}" font-family="{FONT}" font-size="13" '
        f'fill="{fill}" text-anchor="{anchor}"{w}>{value}</text>'
    )


text(10, 22, "Option parity with the real tools", fill=TEAL, weight=600)
text(10, 40, "Long options accepted by em, counted against the installed tool")

for i, (name, counts, em_only) in enumerate(ROWS):
    y = 62 + i * ROW
    total = sum(counts.values())
    present = counts["same"] + counts["different"]
    text(X0 - 10, y + BAR - 3, name, "end", INK, 600)
    x = X0
    for kind, colour in COLOURS.items():
        width = counts[kind] * SCALE
        out.append(f'<rect x="{x:.1f}" y="{y}" width="{width:.1f}" height="{BAR}" fill="{colour}"/>')
        x += width
    text(x + 10, y + BAR - 3, f"{present} / {total}", fill=INK)
    text(X0, y + BAR + 17, f"em-only flags: {em_only}")

legend_y = 62 + len(ROWS) * ROW + 6
for i, (kind, colour) in enumerate(COLOURS.items()):
    x = 10 + i * 205
    out.append(f'<rect x="{x}" y="{legend_y - 10}" width="11" height="11" fill="{colour}"/>')
    label = {"same": "same semantics", "different": "different semantics",
             "missing": "missing"}[kind]
    text(x + 18, legend_y, label)

H = legend_y + 20
svg = (
    f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="{round(H * 1024 / W)}" '
    f'viewBox="0 0 {W} {H}" role="img" aria-label="Option parity: em accepts 62 of 140 '
    f'emerge options and 6 of 72 crossdev options.">\n'
    f'<rect width="{W}" height="{H}" fill="none"/>\n' + "\n".join(out) + "\n</svg>\n"
)
open(sys.argv[1], "w", encoding="utf-8").write(svg)

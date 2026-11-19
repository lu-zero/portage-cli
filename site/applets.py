#!/usr/bin/env python3
"""Write the applet-coverage figure: site/applets.py docs/applets.svg

The statuses come from the Applet status table in README.md. The three named
rows are the applets with their own comparison baselines; the rest share one
bar."""
import pathlib
import sys

FONT = "ui-sans-serif,system-ui,Segoe UI,Helvetica,Arial,sans-serif"
TEAL, GREY, INK = "#5fafaf", "#8a8a8a", "#17181a"
COLOURS = {"Working": "#5faf5f", "Partial": "#87afaf"}
MAIN = [("default", "emerge"), ("crossdev", "crossdev"), ("search", "qsearch")]
W, X0, BAR, ROW = 720, 170, 360, 44
out = []


def text(x, y, value, anchor="start", fill=GREY, weight=None):
    w = f' font-weight="{weight}"' if weight else ""
    out.append(
        f'<text x="{x:.1f}" y="{y}" font-family="{FONT}" font-size="13" '
        f'fill="{fill}" text-anchor="{anchor}"{w}>{value}</text>'
    )


def read_table(path):
    rows = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.startswith("|"):
            continue
        cells = [cell.strip() for cell in line.strip("|").split("|")]
        if len(cells) != 3 or cells[0] == "Applet" or set(cells[0]) <= set("-:"):
            continue
        key = cells[0].replace("`", "").replace("*", "").replace("(", "").replace(")", "")
        rows[key] = cells[2].split("—")[0].strip().split()[0]
    return rows


rows = read_table(pathlib.Path(__file__).resolve().parent.parent / "README.md")
total = len(rows)
named = [(label, rows[applet]) for applet, label in MAIN if applet in rows]
named_keys = {applet for applet, _ in MAIN}
rest = {status: sum(1 for applet, s in rows.items()
                    if applet not in named_keys and s == status) for status in COLOURS}

text(10, 22, "Applet coverage", fill=TEAL, weight=600)
text(10, 40, f"{total} applets from the README status table")

for i, (label, status) in enumerate(named):
    y = 62 + i * ROW
    text(X0 - 10, y + 15, label, "end", INK, 600)
    out.append(f'<rect x="{X0}" y="{y}" width="{BAR}" height="18" fill="{COLOURS[status]}"/>')
    text(X0 + BAR + 10, y + 14, status)

y = 62 + len(named) * ROW
text(X0 - 10, y + 15, "smaller applets", "end", INK, 600)
x = X0
for status, colour in COLOURS.items():
    width = rest[status] / sum(rest.values()) * BAR
    out.append(f'<rect x="{x:.1f}" y="{y}" width="{width:.1f}" height="18" fill="{colour}"/>')
    x += width
text(X0 + BAR + 10, y + 14,
     f"{rest['Working']} working · {rest['Partial']} partial", fill=INK)

for i, (status, colour) in enumerate(COLOURS.items()):
    x = 10 + i * 130
    out.append(f'<rect x="{x}" y="{y + 34}" width="11" height="11" fill="{colour}"/>')
    text(x + 18, y + 44, status.lower())

H = y + 62
svg = (
    f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="{round(H * 1024 / W)}" '
    f'viewBox="0 0 {W} {H}" role="img" aria-label="Applet coverage: {total} applets, '
    f'{sum(s == "Working" for s in rows.values())} working, '
    f'{sum(s == "Partial" for s in rows.values())} partial.">\n'
    f'<rect width="{W}" height="{H}" fill="none"/>\n' + "\n".join(out) + "\n</svg>\n"
)
open(sys.argv[1], "w", encoding="utf-8").write(svg)

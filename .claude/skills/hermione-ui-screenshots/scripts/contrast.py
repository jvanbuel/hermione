#!/usr/bin/env python3
"""WCAG contrast check for the file/diff pane's colours, straight from tokens.css.

    python3 .claude/skills/hermione-ui-screenshots/scripts/contrast.py

Every text colour in the pane is measured against each ground it actually sits
on — the plain board, the cursor line, an added row, a removed row — in both
themes, with the row washes composited over the board the way the browser does.
Exits 1 if anything is under 4.5:1, so it can gate a change to tokens.css.

It reads the real token values (resolving `var()` aliases), so it can't drift
from the stylesheet the way a hand-kept table of hex codes would.
"""
import pathlib
import re
import sys

CSS = pathlib.Path(__file__).resolve().parents[4] / 'crates/server/static/tokens.css'
MIN = 4.5


def block(css, selector):
    m = re.search(selector + r'\s*\{(.*?)\n\}', css, re.S | re.M)
    return dict(re.findall(r'(--[\w-]+)\s*:\s*([^;]+);', m.group(1)))


def resolve(theme, tok):
    v = theme[tok].strip()
    m = re.fullmatch(r'var\((--[\w-]+)\)', v)
    return resolve(theme, m.group(1)) if m else v


def rgb(hexv):
    h = hexv.lstrip('#')
    return tuple(int(h[i:i + 2], 16) / 255 for i in (0, 2, 4))


def lum(c):
    f = lambda x: x / 12.92 if x <= .03928 else ((x + .055) / 1.055) ** 2.4
    r, g, b = map(f, c)
    return .2126 * r + .7152 * g + .0722 * b


def ratio(a, b):
    la, lb = sorted((lum(a), lum(b)), reverse=True)
    return (la + .05) / (lb + .05)


def wash(theme, tok, base):
    """`color-mix(in srgb, <colour> N%, transparent)` composited over `base`."""
    m = re.search(r'color-mix\(in srgb, var\((--[\w-]+)\) (\d+)%', theme[tok])
    fg, a = rgb(resolve(theme, m.group(1))), int(m.group(2)) / 100
    return tuple(base[i] * (1 - a) + fg[i] * a for i in range(3))


# What sits on what. A token is only checked against grounds it really appears
# on: syntax colours are drawn on every row kind; the cursor line's own number
# (`--accent-text`) only on the cursor line; and so on.
PLAIN, CURSOR, ADDED, REMOVED = 'plain row', 'cursor line', 'added row', 'removed row'
# Elsewhere on the board: a card flagged needs-help / watch (and the header's
# needs-help pill, which sits on the same wash), and the filled primary button.
HELP_CARD, WATCH_CARD = 'help card', 'watch card'
PRIMARY, PRIMARY_HOVER = 'primary btn', 'primary hover'
SELECTED = 'selected ctl'  # e.g. the pressed Diff toggle: accent text on the accent wash
PAIRS = [
    ('text', '--fg', [PLAIN, CURSOR, ADDED, REMOVED]),
    ('comment', '--hl-comment', [PLAIN, CURSOR, ADDED, REMOVED]),
    ('string', '--hl-string', [PLAIN, CURSOR, ADDED, REMOVED]),
    ('number', '--hl-number', [PLAIN, CURSOR, ADDED, REMOVED]),
    ('keyword', '--hl-keyword', [PLAIN, CURSOR, ADDED, REMOVED]),
    ('function', '--hl-function', [PLAIN, CURSOR, ADDED, REMOVED]),
    ('type', '--hl-type', [PLAIN, CURSOR, ADDED, REMOVED]),
    ('operator', '--hl-operator', [PLAIN, CURSOR, ADDED, REMOVED]),
    ('line number', '--fg-subtle', [PLAIN, ADDED, REMOVED]),
    ('cursor line number', '--accent-text', [CURSOR]),
    ('+ marker', '--green-text', [ADDED]),
    ('- marker', '--red-text', [REMOVED]),
    ('stale / warning', '--amber-text', [PLAIN, HELP_CARD, WATCH_CARD]),
    ('typing', '--green-text', [PLAIN, HELP_CARD, WATCH_CARD]),
    ('muted text', '--fg-muted', [PLAIN, HELP_CARD, WATCH_CARD]),
    ('subtle text', '--fg-subtle', [HELP_CARD, WATCH_CARD]),
    ('help text / pill', '--red-text', [HELP_CARD]),
    # A literal colour: white text sits on the button's background token.
    ('white on button', '#ffffff', [PRIMARY, PRIMARY_HOVER]),
    ('accent on wash', '--accent-text', [SELECTED]),
]


def main():
    css = CSS.read_text()
    dark = block(css, r'^:root')
    light = {**dark, **block(css, r':root\[data-theme="light"\]')}
    failed = []
    for name, theme in (('dark (chalkboard)', dark), ('light (whiteboard)', light)):
        bg = rgb(resolve(theme, '--bg'))
        grounds = {
            PLAIN: bg,
            CURSOR: wash(theme, '--cursor-line-wash', bg),
            ADDED: wash(theme, '--diff-add-wash', bg),
            REMOVED: wash(theme, '--diff-del-wash', bg),
            HELP_CARD: wash(theme, '--help-wash', rgb(resolve(theme, '--surface'))),
            WATCH_CARD: wash(theme, '--watch-wash', rgb(resolve(theme, '--surface'))),
            SELECTED: wash(theme, '--accent-wash', rgb(resolve(theme, '--surface'))),
            PRIMARY: rgb(resolve(theme, '--primary-bg')),
            PRIMARY_HOVER: rgb(resolve(theme, '--primary-hover')),
        }
        print(f'\n{name}  (board {resolve(theme, "--bg")})')
        print(f'  {"":20}' + ''.join(f'{g:>14}' for g in grounds))
        for label, tok, on in PAIRS:
            cells = ''
            for g, gb in grounds.items():
                if g not in on:
                    cells += f'{"·":>14}'
                    continue
                r = ratio(rgb(tok if tok.startswith('#') else resolve(theme, tok)), gb)
                if r < MIN:
                    failed.append((name, label, g, round(r, 2)))
                cells += f'{r:>13.2f}{"!" if r < MIN else " "}'
            print(f'  {label:20}{cells}')
    print()
    if failed:
        print(f'FAIL — under {MIN}:1:')
        for f in failed:
            print('  ', *f)
        return 1
    print(f'ok — every pairing clears {MIN}:1')
    return 0


sys.exit(main())

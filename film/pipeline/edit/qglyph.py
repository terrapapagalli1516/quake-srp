"""qglyph: a bronze Q that reads as a Q, for everything the film draws in id's conchars.

id's bronze `Q` (conchars cell 209, and `q`, 241) is a bowl open at the top on a centre stem, so on our
overlays "QUAKE" reads as "ΨUAKE" at small sizes. This swaps those two cells for a Q that was tried
against others and read best: id's own bronze `O` with a tail out of the bottom right. It uses the same palette indices (96 the shadow, 100-104 the face, 118 the highlight)
and the same one-pixel shadow to the right and below. Only our overlay letters change; the game's footage
keeps id's glyph, because that is the game's own text.

    import qglyph; qglyph.install()      # once, before the first letter is drawn

`install()` patches the diagram kit's atlas (`qkit.text._glyph_indices`) in this process and clears the
atlas caches, so every colour (bronze, tints, the drop shadow) takes the new cells. It is idempotent.
The grey set's Q (cells 81 and 113) is left as id drew it: nothing of ours draws a grey Q.
"""

from __future__ import annotations

import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "diagrams"))   # film/pipeline/diagrams/qkit

# The cells, row by row, as palette indices (0 is transparent).
Q_GOLD = [
    [0, 0, 0, 0, 0, 0, 0, 0],
    [0, 102, 102, 100, 103, 103, 0, 0],
    [102, 102, 96, 96, 96, 103, 103, 0],
    [103, 102, 96, 0, 0, 103, 102, 96],
    [102, 102, 96, 0, 0, 102, 100, 96],
    [118, 102, 96, 0, 103, 103, 118, 96],
    [0, 103, 104, 100, 103, 102, 103, 96],
    [0, 0, 96, 96, 96, 96, 102, 103],
]
q_GOLD = [
    [0, 0, 0, 0, 0, 0, 0, 0],
    [0, 0, 0, 0, 0, 0, 0, 0],
    [0, 103, 102, 100, 104, 103, 0, 0],
    [103, 104, 96, 96, 96, 102, 118, 0],
    [103, 103, 96, 0, 0, 118, 103, 96],
    [102, 101, 96, 0, 103, 103, 102, 96],
    [0, 102, 104, 100, 103, 102, 103, 96],
    [0, 0, 96, 96, 96, 96, 102, 103],
]
CELLS = {209: Q_GOLD, 241: q_GOLD}  # bronze 'Q' (81 | 0x80) and 'q' (113 | 0x80)


def patch(indices: np.ndarray) -> np.ndarray:
    """A copy of the conchars atlas (palette indices, 16 cells of 8x8 to a row) with the Q cells replaced."""
    out = indices.copy()
    for code, rows in CELLS.items():
        r, c = divmod(code, 16)
        out[8 * r:8 * r + 8, 8 * c:8 * c + 8] = np.array(rows, np.uint8)
    return out


def install() -> None:
    """Use the new Q for everything this process draws with the kit's conchars."""
    from qkit import text
    if getattr(text._glyph_indices, "_qglyph", False):
        return
    orig = text._glyph_indices
    cached = {}

    def patched() -> np.ndarray:
        if "a" not in cached:
            cached["a"] = patch(orig())
        return cached["a"]

    patched._qglyph = True
    text._glyph_indices = patched
    text._atlas.cache_clear()

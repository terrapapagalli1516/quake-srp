"""What the overlays share: panels that read over a busy game picture, a box drawn in its
own coordinates, and the overlay's command line.

An overlay is rendered with `--alpha` (the kit's ProRes 4444 .mov): nothing is drawn where
the footage should show. Text sits on dark translucent panels; lines are 3 px or more;
labels are conchars at scale 2 at least, 3 for anything said aloud.

A script here puts this folder on its path and does `from common import *`, which brings in
the kit (`from qkit import *`, `quake`) as well.
"""

from __future__ import annotations

import sys
from contextlib import contextmanager
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))  # qkit/
sys.path.insert(0, str(HERE.parent))  # filmroot

import filmroot  # noqa: E402
from qkit import *  # noqa: E402,F403
from qkit import quake  # noqa: E402

PANEL_A = 0.74  # a panel's darkness over the footage


def panel(c, x, y, w, h, a: float = 1.0, dark: float = PANEL_A, border="rule", radius: float = 10) -> None:
    """A dark translucent panel with a thin rim: what dense parts sit on over the footage."""
    if a <= 0:
        return
    c.rect(x, y, w, h, fill="bg_deep", alpha=a * dark, radius=radius)
    c.rect(x, y, w, h, stroke=border, lw=2, alpha=a * 0.8, radius=radius)


@contextmanager
def boxed(c, where):
    """Draw in the box's own coordinates, placed at where = (dx, dy, scale)."""
    dx, dy, s = where
    with c.moved(dx, dy, s):
        yield c


# --------------------------------------------------------- stand-in frames ----

def stand_in(kind: str):
    """A game frame like the footage an overlay will sit on (for previews only)."""
    if kind == "e1m6":
        return quake.game_frame("view", "e1m6", "--res", "1920x1080", "--origin", "504,500,242", "--angles", "0,100,0",
                                "--video", "modern", "--perspspan", "16")
    if kind == "e1m1":
        return quake.game_frame("shot", "e1m1", "--res", "960x540", "--zoom", "2", "--video", "modern",
                                "--scaled2d", "1", "--sbaroverlay", "1")
    if kind == "start":
        return quake.game_frame("view", "start", "--res", "1920x1080", "--origin", "544,330,54", "--angles", "0,90,0",
                                "--video", "modern")
    raise KeyError(kind)


def preview(frame_fn, t: float, kind: str, out: Path, dim: float = 1.0) -> None:
    """The overlay at time t composited over a stand-in frame, for checking placement and legibility."""
    import numpy as np
    from PIL import Image

    from qkit import render

    render._FRAME, render._ALPHA = frame_fn, True
    over = render.render_frame(t)
    bg = Image.fromarray((np.asarray(stand_in(kind)).astype(float) * dim).astype(np.uint8)).convert("RGBA")
    bg = bg.resize((W, H))
    bg.alpha_composite(over)
    bg.convert("RGB").save(out)


def main(frame_fn, duration: float, name: str, cues, kind: str, dim: float = 1.0) -> None:
    """The overlay's command line: the kit's (always --alpha), plus `--preview T ...`, which
    writes previews/NAME_T.png under FILM_SCRATCH's diagrams/: the overlay over a stand-in
    game frame (`kind`: e1m6, e1m1 or start)."""
    if "--preview" in sys.argv:
        i = sys.argv.index("--preview")
        pdir = filmroot.scratch("diagrams") / "previews"
        pdir.mkdir(exist_ok=True)
        for tv in sys.argv[i + 1 :]:
            out = pdir / f"{name}_{float(tv):05.2f}.png"
            preview(frame_fn, float(tv), kind, out, dim)
            print(out)
        return
    if "--alpha" not in sys.argv:
        sys.argv.append("--alpha")
    run(frame_fn, duration=duration, name=name, cues=cues)

#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["pycairo", "numpy", "pillow"]
# [tool.uv]
# exclude-newer = "2026-10-05T00:00:00Z"  # the versions the film was made with
# ///
"""D06a, perspective as an inset (PER1, 8.2 s), 1000x560 at the bottom left.

A real row: e1m6's long corridor at 1920x1080 (FRAMERATE.md's view: origin 504,500,242,
angles 0,100,0, the slop preset's video), row 450, x 1157-1189, 33 pixels of one wall at a
grazing angle. Its depth and texture coordinates come from the map (a ray per pixel centre,
the engine's Hor+ projection, the face's texture axes from `texturemins`); the strip under the
graph is the engine's own pixels (`quaketool view --perspspan 1`, every pixel exact).

u curves; 1/z and u/z are straight; the divide at pixel 20 (2.585 / 0.003096 = 835.0) and
the equation; they fade out (cue `eq_out`) before the heading comes in, in the same place.
Then id's renderer: a divide every sixteen pixels (D_DrawSpans16), straight lines between,
and the red gap between the chords and the curve, magnified x6 in a lens. Beats on PER1's
words: the divide flashes on "divided", the heading and id's divides on "sixteen", the
chords on "drew", the gap on "lines", the lens just after.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import *  # noqa: F403

import numpy as np  # noqa: E402

# ------------------------------------------------------------ the facts ----

MAP, ORIGIN, ANGLES, RES, ROW = "e1m6", (504, 500, 242), (0, 100, 0), (1920, 1080), 450
VIEW = ["--res", f"{RES[0]}x{RES[1]}", "--origin", ",".join(map(str, ORIGIN)), "--angles",
        ",".join(map(str, ANGLES)), "--video", "modern"]


def row(x0: int, n: int = 33, y: int = ROW) -> dict:
    """Pixels x0 .. x0+n-1 of row y, all on one face (asserted): depth, texture u, the engine's pixels."""
    bsp = quake.bsp(MAP)
    cam = quake.Camera.horplus(ORIGIN, ANGLES, *RES)
    xs = np.arange(x0, x0 + n, dtype=float)
    dirs = cam.ray(xs, np.full_like(xs, y))
    faces, dist = bsp.cast(cam.origin, dirs)
    assert (faces == faces[0]).all(), f"row {y} x {x0}..{x0 + n - 1}: more than one face {set(faces)}"
    face = int(faces[0])
    pts = cam.origin + dist[:, None] * dirs
    poly_st = bsp.face_st(face, bsp.face_polygon(face))
    mins = np.floor(poly_st.min(axis=0) / 16) * 16  # texturemins: the surface block's corner
    st = bsp.face_st(face, pts) - mins
    exact = quake.game_frame("view", MAP, *VIEW, "--perspspan", "1")
    return {"z": cam.depth(pts), "u_raw": st[:, 0] + mins[0], "strip": exact[y, x0 : x0 + n]}


def affine(x: np.ndarray, span: int) -> np.ndarray:
    """id's affine steps: exact at every span-th pixel, straight between."""
    out = np.empty_like(x)
    n = len(x) - 1
    for k0 in range(0, n, span):
        k1 = min(k0 + span, n)
        w = np.arange(k1 - k0 + 1) / (k1 - k0)
        out[k0 : k1 + 1] = x[k0] + (x[k1] - x[k0]) * w
    return out


D = row(1157)
N = 32
K = np.arange(N + 1)
U, Z = D["u_raw"], D["z"]
ZI, UZ = 1 / Z, U / Z
AFF = affine(U, 16)
STRIP = np.clip(D["strip"] * 1.7, 0, 255)
PICK = 20
GREEN = shade(Q.SB["green"], 1.6)
SLATE = "slate"
LAVA, RUST, RED = Q.SB["lava"], Q.SB["rust"], Q.SB["red"]

# ----------------------------------------------------------------- time ----

CUES = Cues(curve=0.0, zi=0.7, uz=1.2, eq=1.8, cursor=2.7, read=3.1, divide=3.55, land=3.9, eq_out=4.25,
            heading=4.8, knots=4.85, chords=6.3, gap=6.9, lens=7.05).argv()
DURATION = duration_arg(8.2)

# --------------------------------------------------------------- layout ----

BW, BH = 1000, 560
WHERE = (40, H - 40 - BH)
CELL = 26
SX = 110
LENS_AT, LENS_R, ZOOM = (850, 215), 62, 6.0  # in the inset: above the curve's right half


def cx(k):
    return SX + CELL * k + CELL / 2


def inset(c, t):
    a = fade(t, 0.0, d_in=0.4)
    with c.moved(WHERE[0], WHERE[1]):
        panel(c, 0, 0, BW, BH, a, dark=0.8)
        c.text("PERSPECTIVE", 24, 20, 3, "gold", a)
        ax = Axes(c, cx(0), 150, cx(N) - cx(0), 270, xlim=(0, N), ylim=(785, 940))
        ax.frame(xticks=[0, 8, 16, 24, 32], xlabels=[""] * 5, yticks=[800, 850, 900], grid=True, alpha=a,
                 draw=ramp(t, CUES.curve, 0.5))
        c.text("u", ax.x - 60, ax.y + 4, 3, LAVA, a, "center", "middle")
        # 1/z and u/z, straight: they give way to id's chords
        lines_a = a * (1 - ramp(t, CUES.eq_out, 0.4, "smooth") * 0.75)
        for vals, col, lab, lo, t0 in ((ZI, SLATE, "1/z", 795, CUES.zi), (UZ, GREEN, "u/z", 865, CUES.uz)):
            la = lines_a * ramp(t, t0, 0.4)
            v = (vals - vals.min()) / (vals.max() - vals.min())
            ax.plot(K, lo + v * 58, col, 5, la, draw=ramp(t, t0, 1.0))
            c.text(lab, ax.x + ax.w - 6, float(ax.py(lo + 58)) - 46, 3, "slime_light" if lab == "u/z" else col,
                   la * ramp(t, t0 + 0.8, 0.3), "right")
        ax.plot(K, U, LAVA, 5, a, draw=ramp(t, CUES.curve + 0.2, 1.4))

        # the divide at pixel 20, the equation: both out by eq_out + 0.3
        out = 1 - ramp(t, CUES.eq_out, 0.3, "smooth")
        ca = a * fade(t, CUES.cursor, d_in=0.3) * out
        if ca > 0:
            kx = lerp(0, PICK, ramp(t, CUES.cursor, 0.6, "inout"))
            ax.vline(kx, Q.SB["yellow"], 3, ca)
            v_uz = 865 + (UZ[PICK] - UZ.min()) / (UZ.max() - UZ.min()) * 58
            v_zi = 795 + (ZI[PICK] - ZI.min()) / (ZI.max() - ZI.min()) * 58
            ax.dots([PICK], [v_uz], 10, GREEN, ca * ramp(t, CUES.read, 0.3), stroke="bg_deep")
            ax.dots([PICK], [v_zi], 10, SLATE, ca * ramp(t, CUES.read + 0.2, 0.3), stroke="bg_deep")
            fl = 1 - abs(2 * ramp(t, CUES.divide, 0.5, "linear") - 1) if t < CUES.divide + 0.5 else 0.0
            px, _ = ax.pt(PICK, 900)
            yd = (ax.pt(PICK, v_uz)[1] + ax.pt(PICK, v_zi)[1]) / 2
            c.text("÷", px + 30, yd, 6, Q.SB["yellow"], ca * fl, "left", "middle")
            ax.dots([PICK], [U[PICK]], 11, LAVA, ca * ramp(t, CUES.land, 0.3, "back"), stroke="white", lw=3)
            c.text(f"{UZ[PICK]:.3f} ÷ {ZI[PICK]:.6f} = {U[PICK]:.1f}", 24, 70, 3, "text", ca * ramp(t, CUES.divide, 0.3))
        c.eq(r"\c{235}{u} = \frac{\c{slime_light}{u/z}}{\c{slate}{1/z}}", BW - 24, 78, 4, "text",
             a * ramp(t, CUES.eq, 0.5) * out, "right", "middle")

        # id's sixteen: the heading where the equation and readout were, the divides, the chords, the gap
        ha = a * ramp(t, max(CUES.heading, CUES.eq_out + 0.3), 0.4)
        c.text("ID: A DIVIDE EVERY 16 PIXELS,", 24, 62, 3, RUST, ha)
        c.text("STRAIGHT LINES BETWEEN", 24, 98, 3, RUST, ha * ramp(t, CUES.chords, 0.4))
        for i, k in enumerate((0, 16, 32)):
            ka = a * ramp(t, CUES.knots + 0.12 * i, 0.3, "out")
            ax.dots([k], [U[k]], 9, "white", ka, stroke="bg_deep", lw=3)
            px, py = ax.pt(k, U[k])
            c.text("÷", px, py - 40, 3, "white", ka, "center")
        ch = ramp(t, CUES.chords, 0.7)
        ax.fill_between(K, AFF, U, RED, 0.5 * a * ramp(t, CUES.gap, 0.5))
        ax.plot(K, AFF, RUST, 4, a * min(1, ch * 3), draw=ch)

        c.pixel_row(SX, 460, STRIP[:N], CELL, gap=2, alpha=a)
        c.text("ROW 450 OF THE PICTURE, 1.7X BRIGHTER", SX, 504, 2, "dim", a)


def frame(c, t):
    c.background()
    inset(c, t)
    # the gap between id's chords and the curve, magnified
    la = fade(t, 0.0, d_in=0.4) * ramp(t, CUES.lens, 0.4)
    if la <= 0:
        return
    with c.moved(WHERE[0], WHERE[1]):
        ax = Axes(c, cx(0), 150, cx(N) - cx(0), 270, xlim=(0, N), ylim=(785, 940))
        src = ax.pt(8, (U[8] + AFF[8]) / 2)
        with c.lens(src, LENS_AT, ZOOM, LENS_R, ring="dim", alpha=la) as z:
            ax.fill_between(K, AFF, U, RED, 0.6)
            ax.plot(K, U, LAVA, 5 / z)
            ax.plot(K, AFF, RUST, 4 / z)
        c.text("x6", LENS_AT[0] + 46, LENS_AT[1] + 46, 3, "dim", la)


if __name__ == "__main__":
    main(frame, DURATION, "D06a-perspective", CUES, "e1m6")

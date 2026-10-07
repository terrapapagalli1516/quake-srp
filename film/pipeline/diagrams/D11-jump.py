#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["pycairo", "numpy", "pillow"]
# [tool.uv]
# exclude-newer = "2026-10-05T00:00:00Z"  # the versions the film was made with
# ///
"""D11, a jump at 72 and 480 Hz, and the lead (LAB4c, 9.6 s): a 900x700 inset at the right.

The curves are computed from id's rules, not drawn by hand:
- jump speed 270 u/s, sv_gravity 800; id's physics is semi-implicit Euler (velocity, then the
  move), so its frames lie g*t*dt/2 below the parabola 270 t - 400 t^2: g t/144 at 72 Hz,
  g t/960 at 480 Hz (FRAMERATE.md, "How the uncapped step works"). Peaks 43.70 at 72 Hz and
  45.28 at 480 Hz: the jump table's 43.70 and 45.28 ("jumps peaked 1.6 units higher (45.3
  against 43.7); a 44-unit ledge becomes reachable", "In short").
- The uncapped step moves with v + g(dt - 1/72)/2, "which lands on id's 72 Hz curve at every
  rate": computed here at 480 Hz, peak 43.71 (the table's).
- Part a, the grenade: fired level up e1m1's runway (600 u/s forward, 200 up, from 24 above
  the floor; MOVETYPE_BOUNCE halves the vertical speed at each bounce and stops under 60), id's
  code at 72 and 480 Hz. It comes to rest at 648.5 and 670.2 units here; FRAMERATE.md's
  grenade table measures where it explodes, 648.5 and 671.4: "grenades landed 23 units
  further".
- Part b, the lead g(dt - 1/72)/2, and the zoom (x6) on the peaks.

The three peaks stay at full brightness through part b (id's drifted 45.3 is the point), their
heads at 3x on two lines each. Only conchars text.
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import *  # noqa: F403

import numpy as np  # noqa: E402

# ------------------------------------------------------------ the facts ----

SB = Q.SB
GREEN = shade(SB["green"], 1.6)
G, V0, TICK = 800.0, 270.0, 1 / 72


def jump(hz, lead=False):
    dt, v, z, ts, zs, n = 1 / hz, V0, 0.0, [0.0], [0.0], 0
    while True:
        n += 1
        v -= G * dt
        z += (v + (G * (dt - TICK) / 2 if lead else 0.0)) * dt
        ts.append(n * dt)
        zs.append(max(z, 0.0))
        if z < 0:
            return np.array(ts), np.array(zs)


def grenade(hz):
    dt, y, z, vy, vz, path, rest = 1 / hz, 0.0, 24.0, 600.0, 200.0, [(0.0, 24.0)], None
    while rest is None:
        vz -= G * dt
        ny, nz = y + vy * dt, z + vz * dt
        if nz <= 0:
            f = z / (z - nz)
            y, z = y + (ny - y) * f, 0.0
            vz = -0.5 * vz
            if vz < 60:
                rest = y
        else:
            y, z = ny, nz
        path.append((y, z))
    return np.array(path), rest


J72, J480, JSLOP = jump(72), jump(480), jump(480, lead=True)
PEAK = {k: v[1].max() for k, v in (("72", J72), ("480", J480), ("slop", JSLOP))}
assert round(PEAK["72"], 2) == 43.70 and round(PEAK["480"], 2) == 45.28 and round(PEAK["slop"], 2) == 43.71
GR72, REST72 = grenade(72)
GR480, REST480 = grenade(480)


def h72(t):  # id's 72 Hz curve: its frames lie on it
    return np.maximum(0, V0 * t - G * t * t / 2 - G * t * TICK / 2)


def parabola(t):
    return np.maximum(0, V0 * t - G * t * t / 2)


# ----------------------------------------------------------------- time ----

CUES = Cues(axes=0.0, parabola=0.2, id72=0.4, peak72=2.0, id480=2.0, peak480=3.4, ledge=3.2, clear=3.7,
            inset=3.6, partb=4.8, slop=5.0, peakslop=6.6, lead=6.0, zoom=7.4, caption=8.6).argv()
DURATION = duration_arg(9.6)

# --------------------------------------------------------------- layout ----

BW, BH = 900, 700
INSET = (W - 40 - BW, (H - BH) / 2)
GX, GY, GW, GH = 90, 80, 770, 330
HEADS_Y = 462
APEX = (0.335, 44.6)


def limits(t):
    """The graph's limits: the whole jump, then 6x on the peak."""
    u = ramp(t, CUES.zoom, 1.6, "inout")
    z = lerp_log(1.0, 6.0, u)
    xw, yh = 0.7 / z, 50.0 / z
    cx, cy = lerp(0.35, APEX[0], u), lerp(25.0, APEX[1], u)
    return (cx - xw / 2, cx + xw / 2), (cy - yh / 2, cy + yh / 2), z


def draw_box(c, t):
    a = fade(t, 0.0, d_in=0.4)
    panel(c, 0, 0, BW, BH, a, dark=0.82)
    c.text("A JUMP AT 72 AND 480 HZ", 24, 20, 3, "gold", a)
    xlim, ylim, zf = limits(t)
    ax = Axes(c, GX, GY, GW, GH, xlim=xlim, ylim=ylim)
    xt = [v for v in nice_ticks(*xlim, 5) if xlim[0] <= v <= xlim[1]]
    yt = [v for v in nice_ticks(*ylim, 4) if ylim[0] <= v <= ylim[1]]
    ax.frame(xticks=xt, yticks=yt, xlabels=[f"{v:g}" for v in xt], ylabels=[f"{v:g}" for v in yt], grid=True,
             alpha=a, draw=ramp(t, CUES.axes, 0.6))
    c.text("x6", GX + 14, GY + 12, 3, "dim", a * ramp(t, CUES.zoom + 0.8, 0.4))
    ts = np.linspace(0, 0.7, 700)
    pa = a * ramp(t, CUES.parabola, 0.5)
    ax.plot(ts, parabola(ts), "white", 2, pa, draw=ramp(t, CUES.parabola, 0.9), dash=[8, 8])
    la = a * ramp(t, CUES.ledge, 0.5)
    ax.hline(44, SB["green"], 4, la)
    lx, ly = ax.pt(xlim[1], 44)
    c.text("A 44-UNIT LEDGE", lx - 10, ly - 26, 2, GREEN, la, "right")
    ax.plot(ts, h72(ts), SB["rust"], 3, a, draw=ramp(t, CUES.id72, 2.0, "linear"))
    n72 = int(len(J72[0]) * ramp(t, CUES.id72, 2.0, "linear"))
    ax.dots(J72[0][:n72], J72[1][:n72], 6 if zf < 2 else 8, SB["rust"], a, stroke="bg_deep", lw=2)
    b = a * (1 - ramp(t, CUES.partb, 0.6)) if t < CUES.zoom else 0.0
    n480 = int(len(J480[0]) * ramp(t, CUES.id480, 1.8, "linear"))
    ax.dots(J480[0][:n480], J480[1][:n480], 3.2, SB["lava"], b)
    ns = int(len(JSLOP[0]) * ramp(t, CUES.slop, 2.0, "linear"))
    ax.dots(JSLOP[0][:ns], JSLOP[1][:ns], 3.5 if zf < 2 else 5.5, GREEN, a, stroke="bg_deep", lw=1)
    ca = a * fade(t, CUES.clear, CUES.partb, 0.3, 0.4)
    cx, cy = ax.pt(0.3375, 45.28)
    c.text("!", cx, cy - 44, 4, SB["red"], ca, "center")

    # the peaks: three columns, their heads at 3x on two lines, every number at full brightness
    rows = [(CUES.peak72, SB["rust"], ("ID", "72 HZ"), PEAK["72"], 0),
            (CUES.peak480, SB["lava"], ("ID'S CODE", "480 HZ"), PEAK["480"], 1),
            (CUES.peakslop, GREEN, ("SLOP", "480 HZ"), PEAK["slop"], 2)]
    for t_in, col, (h1, h2), v, i in rows:
        ra = a * ramp(t, t_in, 0.4)
        x = 24 + 288 * i
        c.text(h1, x, HEADS_Y, 3, col, ra)
        c.text(h2, x, HEADS_Y + 30, 3, col, ra)
        c.text(f"{v:.1f}", x, HEADS_Y + 66, 4, SB["flame"], ra)
    c.text("PEAKS", BW - 24, HEADS_Y + 76, 2, "dim", a * ramp(t, CUES.peak72, 0.3), "right")

    # part a: the grenade, resting 23 units further at 480 Hz
    ga = a * fade(t, CUES.inset, CUES.lead, 0.5, 0.4)
    if ga > 0:
        gax = Axes(c, 40, 612, BW - 80, 60, xlim=(0, 720), ylim=(0, 60))
        gax.hline(0, "axis", 2, ga)
        d = ramp(t, CUES.inset + 0.2, 1.0, "inout")
        gax.plot(GR72[:, 0], GR72[:, 1], SB["rust"], 3, ga, draw=d)
        gax.plot(GR480[:, 0], GR480[:, 1], SB["lava"], 3, ga, draw=d)
        ra = ga * ramp(t, CUES.inset + 1.0, 0.4)
        for rest, col in ((REST72, SB["rust"]), (REST480, SB["lava"])):
            X = float(gax.px(rest))
            c.line(X, 652, X, 684, col, 5, ra)
        c.text("A GRENADE: 23 UNITS FURTHER AT 480 HZ", 24, 584, 2, SB["flame"], ra)
    # part b: the lead, and where it lands
    ea = a * ramp(t, CUES.lead, 0.5)
    c.text("SLOP'S MOVE IS LED BY", 24, 588, 2, "dim", ea)
    c.eq(r"lead = \frac{g(dt - 1/72)}{2}", 24, 656, 3, GREEN, ea)
    c.text("LANDS ON ID'S 72 HZ CURVE", GX + GW - 12, GY + GH - 44, 3, SB["flame"], a * ramp(t, CUES.caption, 0.5),
           "right")


def frame(c, t):
    c.background()
    with c.moved(*INSET):
        draw_box(c, t)


if __name__ == "__main__":
    main(frame, DURATION, "D11-jump", CUES, "e1m1")

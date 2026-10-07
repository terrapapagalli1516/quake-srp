"""The edit's own pictures, drawn with the diagram kit (film/pipeline/diagrams/qkit) in Quake's palette and font.

- slates: a placeholder for a shot whose source does not exist yet (its id, what it shows, its length);
- cards: shots the edit draws whole (S14 the title, S27 the terminal, S64 the end card);
- overlays: transparent layers over a shot (the montage's captions, S28's title, S37's proof
  crawl, S59's rules, a stand-in's tag).

Every function draws one frame on a qkit Canvas; `t` is seconds from the shot's first frame
and `sh` is the shot's entry in timeline.json (its length, its cues from the narration's
word times). build.py renders them to PNG frames.
"""

from __future__ import annotations

import math
import re
import sys
import textwrap
from pathlib import Path

import cairo
from PIL import Image

EDIT = Path(__file__).resolve().parent
sys.path.insert(0, str(EDIT.parent))
from filmroot import FILM, PIPELINE, REPO, scratch  # noqa: E402

sys.path.insert(0, str(PIPELINE / "diagrams"))
from qkit import Canvas, fade, ramp  # noqa: E402
from qkit import look  # noqa: E402
from qkit import text as qtext  # noqa: E402

sys.path.insert(0, str(EDIT))
import qglyph  # noqa: E402  (id's bronze Q reads as "Ψ" at small sizes: a Q with a tail instead)

qglyph.install()

W, H = look.W, look.H
BRONZE = "gold"        # id's bronze menu letters (palette 106's set), lifted a little for the screen
WHITE = 254

_MAP = {"–": "-", "—": "-", "’": "'", "‘": "'", "“": '"', "”": '"', "…": "...", "²": "2", "³": "3",
        "¼": "1/4", "½": "1/2", "é": "e", "©": "(c)", "≠": "!=", "∙": "·"}


def qsafe(s: str) -> str:
    """Text Quake's font can draw: ASCII, plus the kit's extra signs."""
    out = []
    for ch in s or "":
        ch = _MAP.get(ch, ch)
        if all(ord(x) < 128 or x in qtext.EXTRA_CHARS for x in ch):
            out.append(ch)
        else:
            out.append("?")
    return "".join(out)


def wrap(s: str, width: int, lines: int) -> list[str]:
    rows = textwrap.wrap(qsafe(s), width)
    if len(rows) > lines:
        rows = rows[:lines]
        rows[-1] = rows[-1][: width - 3].rstrip() + "..."
    return rows


def tc(t: float) -> str:
    return f"{int(t // 60)}:{t % 60:05.2f}"


SCALE = 1   # the frame's size is SCALE x 1920x1080 (build.py --scale); every card is drawn in 1080 units


def set_scale(n: int) -> None:
    """Draw every card at n x its 1080 geometry: a canvas n times the size, a cairo transform of n, and so
    Quake's glyphs at n x their scale (nearest neighbour), lines and fills at n x their width."""
    global SCALE
    SCALE = int(n)


def new(alpha: bool) -> Canvas:
    surf = cairo.ImageSurface(cairo.FORMAT_ARGB32 if alpha else cairo.FORMAT_RGB24, W * SCALE, H * SCALE)
    c = Canvas(surf, transparent=alpha)
    if SCALE != 1:
        c.ctx.scale(SCALE, SCALE)
        c.w, c.h = W, H   # the cards' own coordinates stay in 1080 units
    return c


def save(c: Canvas, path: Path) -> None:
    c.surface.flush()
    data, stride = bytes(c.surface.get_data()), c.surface.get_stride()
    size = (c.surface.get_width(), c.surface.get_height())
    if c.transparent:
        im = Image.frombuffer("RGBA", size, data, "raw", "BGRa", stride, 1)
    else:
        im = Image.frombuffer("RGB", size, data, "raw", "BGRX", stride, 1)
    im.save(path, compress_level=1)


# ------------------------------------------------------------------ slate ----

KIND_COLOR = {"game": "lava", "demo": "ember", "compare": "slate", "diagram": "slime_light", "title": "gold",
              "page capture": "sky_deep"}


def slate(c: Canvas, sh: dict, section_title: str = "", quiet: bool = False) -> None:
    """A placeholder: dark, the shot's id, what it shows and how long it is."""
    c.background(0.65)
    c.rect(40, 40, W - 80, H - 80, stroke="rust_dark", lw=2)
    kind = sh["kind"].upper() + (" · " + qsafe(sh["extra"]).upper() if sh["extra"] else "")
    if quiet:  # an overlay carries this shot: keep the slate's words small and high
        c.text(sh["id"], 96, 80, 4, BRONZE)
        c.text(kind[:100], 96 + 4 * 8 * (len(sh["id"]) + 1), 88, 2, KIND_COLOR.get(sh["kind"], "text"))
        c.text(qsafe(sh["desc"])[:110], 96, 128, 2, "dim")
        c.text(f"{sh['len']:.2f} S · PLACEHOLDER", W - 96, 80, 2, "rust", align="right")
        return
    c.text(sh["id"], 96, 96, 12, BRONZE)
    c.bignum(f"{sh['len']:.2f}", W - 150, 100, 2, align="right")
    c.text("S", W - 96, 124, 4, "dim", align="right")
    c.text("PLACEHOLDER", W - 96, 178, 2, "rust", align="right")
    c.text(kind[:72], 96, 228, 3, KIND_COLOR.get(sh["kind"], "text"))
    y = 300
    for row in wrap(sh["desc"], 70, 3):
        c.text(row, 96, y, 3, "text")
        y += 40
    if sh.get("notice"):
        y += 24
        for row in wrap(sh["notice"], 104, 5):
            c.text(row, 96, y, 2, "dim")
            y += 28
    if sh.get("over"):
        y = max(y + 24, 700)
        for row in wrap("\"" + sh["over"] + "\"", 104, 3):
            c.text(row, 96, y, 2, "rust")
            y += 28
    foot = f"{sh['section']} {qsafe(section_title).upper()} · EDIT {tc(sh['start'])}-{tc(sh['start'] + sh['len'])} · PLANNED {tc(sh['planned_start'])}, {sh['planned_len']:.1f} S"
    c.text(foot, W - 96, H - 92, 2, "axis", align="right")


def tag(c: Canvas, s: str, color="rust") -> None:
    """A small tag at the top right, left of the review build's timecode (a stand-in, a shot id): v4's top-left
    tag covered the films' own labels ("S17OP")."""
    s = qsafe(s).upper()
    w = len(s) * 16
    x = W - 210 - w
    c.rect(x - 12, 4, w + 24, 32, fill="bg_deep", alpha=0.7)
    c.text(s, x, 12, 2, color)


# ------------------------------------------------------------------ cards ----

def card_title(c: Canvas, t: float, sh: dict) -> None:
    """S14: "quake-srp" cut in on the impact; "slop rust port" at 1.2 s (the shot's fade-out is ffmpeg's)."""
    c.rect(0, 0, W, H, fill="bg_deep")
    c.text("quake-srp", W / 2, 430, 12, BRONZE, align="center")
    c.text("slop rust port", W / 2, 600, 4, BRONZE, alpha=1.0 if t >= 1.2 else 0.0, align="center")


def card_terminal(c: Canvas, t: float, sh: dict, over: bool = False) -> None:
    """S27: id's console, the command typed at 40 characters a second, nine PASS lines, ALL PASS on the words,
    in white at 6x, centred low. `over`: on the game at 50% black instead of a black card (v2)."""
    from timeline import TERMINAL_CHECKS, TERMINAL_CMD, terminal_times
    c.rect(0, 0, W, H, fill="bg_deep", alpha=0.5 if over else 1.0)
    x, y0, lh, sc = 120, 150, 54, 4      # v4: 4x (3x's small capitals were 4.5 px on a phone)
    t_type, _, passes, allpass = terminal_times(sh["len"], sh["cues"].get("allpass"))
    typed_n = int(max(0.0, t - t_type) * 40)
    shown = TERMINAL_CMD[:typed_n]
    c.text(shown, x, y0, sc, BRONZE)
    n_lines = sum(1 for p in passes if t >= p)
    from timeline import TERMINAL_AGAINST_C
    against = sh["cues"].get("against")  # v6: on "against id's own C" the six checks run against id's C brighten
    for i in range(n_lines):
        hot = against is not None and t >= against and TERMINAL_CHECKS[i] in TERMINAL_AGAINST_C
        c.text(f"PASS  {TERMINAL_CHECKS[i]}", x, y0 + (i + 1) * lh + 12, sc, WHITE if hot else BRONZE)
    done = t >= allpass
    if done:
        c.rect(0, 790, W, 140, fill="bg_deep", alpha=0.6)
        c.text("ALL PASS", W / 2, 812, 6, WHITE, align="center")
    if int(t * 4) % 2 == 0 and not done:  # the console's blinking cursor, after whatever was typed last
        if typed_n < len(TERMINAL_CMD):
            cx, cy = x + len(shown) * 8 * sc, y0
        else:
            cx, cy = x, y0 + (n_lines + 1) * lh + 12
        c.rect(cx, cy, 8 * sc, 8 * sc, fill="gold", alpha=0.8)  # v5: a block cursor (a bar read as 2 px of text)


def qtext_color(name):
    return look.GOLD if name == "gold" else name


def card_endcard(c: Canvas, t: float, sh: dict) -> None:
    """S64: the end card, its lines in turn; the fade-out over the last second is ffmpeg's."""
    c.rect(0, 0, W, H, fill="bg_deep")
    c.text("quake-srp", W / 2, 330, 8, BRONZE, alpha=fade(t, 0.0, None, 0.3), align="center")
    rows = ["id Software's Quake (1996)",
            "Quake shareware data (c) id Software",
            "written by Claude · score synthesized · a stock voice"]
    for i, r in enumerate(rows):
        c.text(qsafe(r), W / 2, 520 + i * 64, 3, BRONZE, alpha=fade(t, 0.35 + 0.3 * i, None, 0.3), align="center")


CARDS = {"title": card_title, "terminal": card_terminal, "endcard": card_endcard}


# --------------------------------------------------------------- overlays ----

def ov_slop_title(c: Canvas, t: float, sh: dict) -> None:
    """S28: "the slop options" in Quake's letters, in at 0.3 s, out 0.7 s before the cut."""
    a = fade(t, 0.3, sh["len"] - 0.7, 0.4, 0.4)
    if a <= 0:
        return
    sub = sh.get("ov", {}).get("sub")
    c.rect(0, 440, W, 260 if sub else 200, fill="bg_deep", alpha=0.55 * a)
    c.text("the slop options", W / 2, 500, 10, BRONZE, alpha=a, align="center")
    if sub:
        c.text(qsafe(sub), W / 2, 620, 4, BRONZE, alpha=a, align="center")


def ov_rules(c: Canvas, t: float, sh: dict) -> None:
    """S59: the user's three rules, each typed on its words."""
    rules = ["1. ZERO DEPENDENCIES", "2. NO UNSAFE", "3. CLASSIC IS ID'S GAME, PROVEN FOR ANYTHING TOUCHED"]  # v4: capitals
    cues = [sh["cues"].get(k) for k in ("rule1", "rule2", "rule3")]
    defaults = [0.3, 1.5, 2.7]
    for i, (r, cu) in enumerate(zip(rules, cues)):
        t0 = (cu if cu is not None else defaults[i]) - 0.05
        n = int(max(0.0, t - t0) * (120 if i == 2 else 45))  # rule 3 at 120 a second: readable whole on its word
        if n > 0:
            c.rect(216, 400 + i * 96 - 16, len(r) * 24 + 48, 56, fill="bg_deep", alpha=0.85)  # the label band
            c.text(r[:n], 240, 400 + i * 96, 3, BRONZE)


def _proof_lines() -> list[str]:
    src = (REPO / "quake-rs" / "src" / "render" / "raster.rs").read_text().splitlines()
    start = next(i for i, l in enumerate(src) if "Why the guard vouches for a pixel" in l)
    out = []
    for l in src[start:]:
        if not l.startswith("//"):
            break
        out.append(l)
    return [qsafe(l) for l in out]


_PROOF = None


def ov_proof_crawl(c: Canvas, t: float, sh: dict) -> None:
    """S37: raster.rs's proof that the guard vouches for a pixel, (a) to (h), crawling up; "same pixels" on its word."""
    global _PROOF
    if _PROOF is None:
        _PROOF = _proof_lines()
    lh = 26
    y0 = 140 - 70 * t  # a reading pace
    for i, l in enumerate(_PROOF):
        y = y0 + i * lh
        if -20 < y < H:
            c.text(l, 150, y, 2, BRONZE, alpha=0.9)
    same = sh["cues"].get("same")
    if same is None:
        same = 0.0
    if t >= same:
        a = ramp(t, same, 0.15)
        c.rect(0, 470, W, 140, fill="bg_deep", alpha=0.75 * a)
        c.text("same pixels", W / 2, 500, 10, WHITE, alpha=a, align="center")


def ov_caption(c: Canvas, k: int, text: str, sub: str | None, frames: int = 6) -> None:
    """A montage caption: 4x, upper case, bronze, on a black band at 60%, bottom left, typed in 6 frames."""
    main = qsafe(text).upper()
    sub = qsafe(sub or "")
    total = len(main) + len(sub)
    n = int(round(total * min(1.0, k / frames)))
    x, bottom = 96, H - 120
    h_main, gap, h_sub = 32, 12, 16
    block = h_main + ((gap + h_sub) if sub else 0)
    width = max(len(main) * 32, len(sub) * 16)
    c.rect(x - 20, bottom - block - 18, width + 40, block + 36, fill="bg_deep", alpha=0.85)  # v4: 85%, for a phone
    c.text(main[:n], x, bottom - block, 4, BRONZE)
    if sub and n > len(main):
        c.text(sub[: n - len(main)], x, bottom - h_sub, 2, BRONZE)


_SAV = None


def ov_save_text(c: Canvas, t: float, sh: dict, at: float = 0.9) -> None:
    """F09: from the save (0.9 s), the first lines of the real s0.sav the page wrote, one every 0.09 s, on black."""
    global _SAV
    if t < at:
        return
    if _SAV is None:
        raw = (FILM / "footage" / "web" / "F09-s0.sav").read_text(errors="replace").splitlines()
        _SAV = [qsafe(l) for l in raw[:12]]
    c.rect(0, 0, W, H, fill="bg_deep")
    c.text("id1/s0.sav", 240, 120, 2, "dim")
    n = min(len(_SAV), 1 + int((t - at) / 0.09))
    for i in range(n):
        c.text(_SAV[i], 240, 180 + i * 48, 3, BRONZE)


def ov_offline_badge(c: Canvas, t: float, sh: dict, at: float = 1.0) -> None:
    """F07: an OFFLINE badge at the top right from the offline boot (1.0 s)."""
    if t < at:
        return
    c.rect(W - 96 - 7 * 24 - 24, 72, 7 * 24 + 48, 48, fill="bg_deep", alpha=0.75)
    c.rect(W - 96 - 7 * 24 - 24, 72, 7 * 24 + 48, 48, stroke=BRONZE, lw=2)
    c.text("OFFLINE", W - 96, 84, 3, BRONZE, align="right")


def ov_frame_counter(c: Canvas, t: float, sh: dict) -> None:
    """S45: on id's side, the 240 Hz frame within the grunt's 0.1 s step, 1 ... 24, from each step (footage's
    `id_steps_shot_s`: the first, then every `step` s); at 1/8 speed a 240 Hz frame shows for 1/30 s."""
    ov = sh.get("ov", {})
    first, step = float(ov.get("first", -0.033)), float(ov.get("step", 0.8))
    k = int(math.floor((t - first) / (step / 24))) % 24 + 1
    x, y = int(ov.get("x", 48)), int(ov.get("y", 990))
    s = f"FRAME {k:2d}"
    c.rect(x - 12, y - 14, len(s) * 32 + 24, 56, fill="bg_deep", alpha=0.75)   # v5: 4x
    c.text(s, x, y - 4, 4, "white" if k == 24 else BRONZE)


# ------------------------------------------------------------- version 2 ----

_STILLS: dict = {}
SCRATCH_STILLS = scratch("edit") / "stills"


def still(path: str, t: float):
    """A frame of a film source as a cairo surface (extracted once, cached on disk and in the process)."""
    key = (path, round(t, 4))
    if key not in _STILLS:
        SCRATCH_STILLS.mkdir(parents=True, exist_ok=True)
        src = FILM / path
        out = SCRATCH_STILLS / f"{Path(path).stem}-{t:.4f}-{int(src.stat().st_mtime)}.png"
        if not out.exists():  # several drawing processes may want it at once: write aside, then rename
            import os
            import subprocess
            tmp = out.with_name(f"{out.stem}.{os.getpid()}.png")
            subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-ss", f"{t:.4f}", "-i", str(src), "-frames:v", "1",
                            str(tmp)], check=True)
            os.replace(tmp, out)
        from qkit import load_image
        _STILLS[key] = load_image(str(out))
    return _STILLS[key]


# S23's last frame (32x on e1m7's two pixels): the rings' centres, found in the frame (red 251)
S23_LAST = ("footage/game/S23.mp4", 3.18)
RING_UPPER = (1536, 252)   # pixel (169, 98), on the pillar's edge
RING_LOWER = (384, 828)    # pixel (133, 113), on the lava's left edge


def card_black(c: Canvas, t: float, sh: dict) -> None:
    """G2: black, for the silence after the cut-off."""
    c.rect(0, 0, W, H, fill="bg_deep")


def card_still_rings(c: Canvas, t: float, sh: dict) -> None:
    """G3, S63b: S23's last frame, the two red rings; on S63b they blink twice on "two"."""
    c.rect(0, 0, W, H, fill="bg_deep")
    c.image(still(*S23_LAST), 0, 0, W / still(*S23_LAST).get_width())
    two = sh["cues"].get("two")
    if two is not None:
        for k in range(2):
            t0 = two - 0.05 + 0.6 * k
            if t0 <= t < t0 + 0.2:
                for cx, cy in (RING_UPPER, RING_LOWER):
                    c.circle(cx, cy, 52, stroke="white", lw=6)


def card_push(c: Canvas, t: float, sh: dict) -> None:
    """G1a: S23's last frame keeps pushing in, 32x -> 96x, on pixel (169, 98); a texel-boundary grid
    (schematic) on "texel", "ROUNDING" on "rounding"."""
    from qkit import lerp_log, ramp as rmp
    c.rect(0, 0, W, H, fill="bg_deep")
    u = rmp(t, 0.0, max(1.0, sh["len"] - 0.3), "inout")
    z = lerp_log(1.0, 3.0, u)
    cx, cy = RING_UPPER
    tx = cx + (W / 2 - cx) * u  # the pixel drifts to the centre as the push goes in
    ty = cy + (H / 2 - cy) * u
    with c.moved(tx - cx * z, ty - cy * z, z):
        c.image(still(*S23_LAST), 0, 0, W / still(*S23_LAST).get_width())
    grid = sh["cues"].get("texel")
    if grid is not None and t >= grid:
        a = rmp(t, grid, 0.4)
        px, py = tx, ty  # the pixel's centre on screen; a texel boundary passes along its corner
        half = 16 * z
        with c.group(a):
            for k in range(-6, 7):
                off = k * 96 * z / 3
                c.line(px - half - 900 + off, py + half + 900, px - half + 900 + off, py + half - 900, "rust", 3,
                       alpha=0.9 if k == 0 else 0.45)
            c.arrow(px + 380, py + 260, px + half + 10, py + half + 10, "rust", 4, head=18)
            c.text("TEXEL BOUNDARY", px + 390, py + 250, 3, BRONZE)
            c.text("schematic", px + 390, py + 290, 2, "dim")
    rnd = sh["cues"].get("rounding")
    if rnd is not None and t >= rnd:
        c.text("ROUNDING \u2193", tx - 120, ty - 160, 3, BRONZE, alpha=ramp(t, rnd, 0.3))


def ov_terminal_over(c: Canvas, t: float, sh: dict) -> None:
    """S27 (v2): the console over the game at 50%."""
    card_terminal(c, t, sh, over=True)


def ov_subtitle(c: Canvas, t: float, sh: dict) -> None:
    """A line not recorded yet: its words under the picture, while the voice would speak them."""
    ov = sh.get("ov", {})
    if not (ov.get("from", 0) <= t < ov.get("to", 1e9)):
        return
    rows = textwrap.wrap(ov.get("text", ""), 70)[:3]
    y = H - 60 - 46 * len(rows)
    c.rect(W / 2 - 860, y - 46, 1720, 46 * len(rows) + 58, fill="bg_deep", alpha=0.75)
    c.sans(f"[{ov.get('id', '')}: not recorded yet]", W / 2, y - 14, 22, "yellow", align="center")
    for i, r in enumerate(rows):
        c.sans(r, W / 2, y + 30 + 46 * i, 34, "white", align="center")


def ov_label(c: Canvas, t: float, sh: dict) -> None:
    """A label in Quake's letters on a dark band, at (x, y); with `ax, ay`, an arrow to there."""
    ov = sh.get("ov", {})
    text = qsafe(ov.get("text", "")).upper() if ov.get("upper", True) else qsafe(ov.get("text", ""))
    sc = int(ov.get("scale", 3))
    x, y = ov.get("x", 40), ov.get("y", 40)
    w = max(len(text) * 8 * sc, int(ov.get("min_w", 0)))
    if ov.get("box"):  # an explicit band [x0, y0, x1, y1] (over a burned-in label), the lines centred in it
        x0, y0, x1, y1 = ov["box"]
        c.rect(x0, y0, x1 - x0, y1 - y0, fill="bg_deep", alpha=float(ov.get("band", 1.0)))
        lines = text.split("\n")
        pitch = 8 * sc + 8
        top = (y0 + y1) / 2 - (len(lines) * pitch - 8) / 2
        for k, ln in enumerate(lines):
            c.text(ln, (x0 + x1) / 2 - len(ln) * 4 * sc, top + k * pitch, sc, ov.get("color", BRONZE))
        return
    else:
        pad = int(ov.get("pad", 0))  # wider than the text by this much each side, beyond the usual margin
        c.rect(x - 12 - pad, y - 10 - pad, w + 24 + 2 * pad, 8 * sc + 20 + int(ov.get("pad_h", 0)) + 2 * pad,
               fill="bg_deep", alpha=float(ov.get("band", 0.75)))
    c.text(text, x, y, sc, ov.get("color", BRONZE))
    if "ax" in ov:
        c.arrow(x - 16, y + 4 * sc, ov["ax"], ov["ay"], ov.get("color", "gold"), 3, head=14)


def ov_bumper(c: Canvas, t: float, text: str, opts: dict) -> None:
    """A bumper: the option's name, big, centred, over the moving game; typed on in 6 frames, held, faded.
    With `flicker`, each letter's brightness follows a Quake light style (TORCHES!)."""
    hold = float(opts.get("hold", 1.5))
    if t >= hold:
        return
    text = qsafe(text).upper()
    n = int(round(len(text) * min(1.0, (t * 60 + 1) / 6)))
    a = 1.0 - ramp(t, hold - 0.3, 0.3)
    sc = int(opts.get("scale", 10))
    w = len(text) * 8 * sc
    x0, y0 = (W - w) / 2, H / 2 - 4 * sc - 40
    c.rect(0, y0 - 40, W, 8 * sc + 80, fill="bg_deep", alpha=0.55 * a)
    style = opts.get("flicker")
    for i, ch in enumerate(text[:n]):
        col = BRONZE
        if style:
            v = (ord(style[(int(t * 10) + 3 * i) % len(style)]) - ord("a")) * 22
            col = look.shade(look.GOLD, max(0.25, min(1.6, v / 264)))
        c.text(ch, x0 + i * 8 * sc, y0, sc, col, alpha=a)


def _menu_panel(c: Canvas, x: float, y: float, w: float, label: str) -> None:
    c.rect(x - 24, y - 20, w + 48, 64, fill="bg_deep", alpha=0.8)
    c.rect(x - 24, y - 20, w + 48, 64, stroke="rust_dark", lw=2)
    c.text(qsafe(label), x, y, 3, "white")


def ov_menu_row(c: Canvas, t: float, sh: dict) -> None:
    """S34b: the port's menu row "Perspective span", in id's style, its value ticking with the flip."""
    ov = sh.get("ov", {})
    steps = ov.get("steps", [])
    off = float(ov.get("offset", 0.0))  # shot time of the source = t + offset
    val = None
    for ts, v in steps:
        if t + off >= ts - 1e-6:
            val = v
    if val is None:
        return
    x, y = W - 96 - 600, 84
    _menu_panel(c, x, y, 600, "Perspective span")
    c.text(str(val), x + 600, y, 3, BRONZE, align="right")
    if int(t * 4) % 2 == 0:  # id's blinking menu cursor
        c.text(chr(13), x - 32, y, 3, BRONZE)


def ov_slider_row(c: Canvas, t: float, sh: dict) -> None:
    """N5b: the port's menu row "Torch flicker", id's conchars slider (0 to 2), its knob stepping 1 -> 2 on
    the clicks, back to 1 on "Please don't." """
    ov = sh.get("ov", {})
    val = 1.0
    for ts, v in ov.get("steps", []):
        if t >= ts - 1e-6:
            val = v
    reset = ov.get("reset_at")
    if reset is not None and t >= reset:
        val = 1.0
    x, y = 420, 860
    label = "Torch flicker"
    _menu_panel(c, x, y, 1080, label)
    sx = x + 560
    # id's M_DrawSlider: 128 (left end), 129 x SLIDER_RANGE, 130 (right end), 131 (the knob)
    rng = 10
    for i in range(rng + 2):
        ch = chr(0) if i == 0 else chr(2) if i == rng + 1 else chr(1)
        c.text(ch, sx + i * 24, y, 3, "gold")
    knob = sx + 24 + (rng - 1) * 24 * (val / 2.0)
    c.text(chr(3), knob, y, 3, "gold")
    c.text(f"{val:.1f}", sx + (rng + 3) * 24, y, 3, BRONZE)


# G1c: v1's maths slams in, faster and faster (the lecture's crescendo), then all of it at once
FRAGMENTS = [
    ("diagrams/final/D06-perspective.mp4", 7.0), ("diagrams/final/D07-square.mp4", 2.8),
    ("diagrams/final/D08-cheap-exact.mp4", 8.0), ("diagrams/final/D11-jump.mp4", 7.0),
    ("crawl", 0.0), ("diagrams/final/D01-fixed-point.mp4", 0.9), ("diagrams/final/D03-oracle.mp4", 4.0),
    ("diagrams/final/D10-gate.mp4", 4.0), ("diagrams/final/D12-light-letters.mp4", 5.0),
]
PLACES = [(-260, -170, -5), (300, 140, 4), (-120, 200, 3), (380, -190, -4), (-420, 60, 6), (160, -40, -2),
          (-330, -250, 3), (420, 230, -6), (0, 0, 2)]


def _fragment(c: Canvas, k: int, cx: float, cy: float, scale: float, rot: float, flash: float) -> None:
    import math as _m
    path, t = FRAGMENTS[k % len(FRAGMENTS)]
    ctx = c.ctx
    ctx.save()
    ctx.translate(cx, cy)
    ctx.rotate(_m.radians(rot))
    ctx.scale(scale, scale)
    ctx.translate(-W / 2, -H / 2)
    c.rect(0, 0, W, H, fill="bg_deep")
    if path == "crawl":
        global _PROOF
        if _PROOF is None:
            _PROOF = _proof_lines()
        for i, l in enumerate(_PROOF[:38]):
            c.text(l, 60, 40 + i * 26, 2, BRONZE)
    else:
        c.image(still(path, t), 0, 0, W / still(path, t).get_width())
    c.rect(0, 0, W, H, stroke="white" if flash > 0 else "rust", lw=10 if flash > 0 else 6, alpha=max(flash, 0.9))
    ctx.restore()


def ov_fragments(c: Canvas, t: float, sh: dict) -> None:
    ov = sh.get("ov", {})
    slams = ov.get("slams", [])
    shown = [i for i, ts in enumerate(slams) if t >= ts]
    if not shown:
        return
    last = len(slams) - 1
    if shown[-1] == last:  # everything at once: all of them, small, scattered, in one hit
        dt = t - slams[last]
        pop = 1.0 + 0.12 * max(0.0, 1 - dt / 0.08)
        for i in range(len(FRAGMENTS)):
            dx, dy, r = PLACES[i]
            _fragment(c, i, W / 2 + dx * 1.35, H / 2 + dy * 1.35, 0.30 * pop, r * 1.5, max(0.0, 1 - dt / 0.12))
        return
    for i in shown:
        dt = t - slams[i]
        pop = 1.0 + 0.12 * max(0.0, 1 - dt / 0.08)  # it lands: a little big for 5 frames
        dx, dy, r = PLACES[i % len(PLACES)]
        _fragment(c, i, W / 2 + dx, H / 2 + dy, 0.52 * pop, r, max(0.0, 1 - dt / 0.12))


# ------------------------------------------------------------- version 3 ----

def ov_ladder(c: Canvas, t: float, sh: dict) -> None:
    """The SLOP OPTIONS ladder (script/v3/shots.md): items light one by one; in the box phase it sits in the
    right pillar, after the burst in a panel at the top right; on a lab shot only around an item's lighting."""
    ov = sh.get("ov", {})
    P = ov["plan"]
    T = sh["start"] + t
    items = P["items"]
    if P.get("appear") is not None and T < P["appear"]:
        return
    # where: the pillar (box phase) or the panel, flying between them over the burst
    u = 0.0 if P.get("panel_at") is None else ramp(T, P["panel_at"], P["panel_dur"], "out")
    alpha = 1.0
    if ov.get("mode") == "lab":
        hits = [x for x in P["on"].values() if x is not None and sh["start"] <= x < sh["start"] + sh["len"]]
        alpha = max(fade(T, h - 0.2, h + 1.8, 0.2, 0.3) for h in hits) if hits else 0.0
    if P.get("all_fade_by") is not None and P.get("all_flash") is not None and T >= P["all_flash"]:
        alpha *= 1.0 - ramp(T, P["all_fade_by"] - 0.6, 0.6)
    if P.get("appear") is not None:
        alpha *= ramp(T, P["appear"], 0.4)
    if alpha <= 0.01:
        return
    bx, by_, step_b = 1688, 160, 52      # the pillar
    many = len(items) > 13               # v4's 14 rows: the panel's pitch tightens to 44 px, and it grows
    px, py, step_p = (1646, 92, 44) if many else (1646, 92, 50)
    dx, dy = (ov.get("where") or [0, 0]) if isinstance(ov.get("where"), (list, tuple)) else (0, 0)

    def lines(name: str, width: int = 14) -> list[str]:  # a long name wraps at a word break inside its row
        out, cur = [], ""
        for w in name.split():
            if cur and len(cur) + 1 + len(w) > width:
                out.append(cur)
                cur = w
            else:
                cur = (cur + " " + w).strip()
        return out + [cur]

    with c.group(alpha):
        if u > 0:
            c.rect(1630 + dx, 30 + dy, 270, 730 if many else 710, fill="bg_deep", alpha=0.55 * u)
        hx, hy = bx + (px - bx) * u + dx, (by_ - 70) + (44 - (by_ - 70)) * u + dy
        c.text("SLOP OPTIONS", hx, hy, 2, BRONZE)
        for i, it in enumerate(items):
            x = bx + (px - bx) * u + dx
            y = by_ + i * step_b + ((py + i * step_p) - (by_ + i * step_b)) * u + dy
            labels = lines(it) if many else [(f"{i + 1} " if u >= 0.5 else "") + it]
            t_on, t_off = P["on"].get(it), P["off"].get(it)
            lit = t_on is not None and T >= t_on and not (t_off is not None and T >= t_off)
            flash = (t_on is not None and 0 <= T - t_on < 0.1) or (
                P.get("all_flash") is not None and 0 <= T - P["all_flash"] < 0.3)
            for k, label in enumerate(labels):
                yy = y + k * 18
                if flash:
                    c.text(label, x, yy, 2, "white")
                elif lit:
                    c.text(label, x, yy, 2, BRONZE)
                else:
                    c.text(label, x, yy, 2, 26, alpha=0.45)


_PPM = {}


def _first(p):
    """A path, or the first that exists of a list (the post-fix diffs first, then the ones we have)."""
    for x in (p if isinstance(p, (list, tuple)) else [p]):
        q = Path(x) if Path(x).is_absolute() else FILM / x
        if q.exists():
            return str(q)
    return None


def _ppm(path: str):
    if path not in _PPM:
        import numpy as np
        _PPM[path] = np.asarray(Image.open(path).convert("RGB")).astype(int)
    return _PPM[path]


def _diff_surface(cpath: str, ppath: str):
    """compare.py's diff panel: max |dRGB| x4 on black, and the share of pixels that differ."""
    import numpy as np
    from qkit import surface_from_array
    a, b = _ppm(cpath), _ppm(ppath)
    d = np.abs(a - b).max(axis=2)
    img = np.clip(d * 4, 0, 255).astype(np.uint8)
    rgb = np.dstack([img, img, img])
    return surface_from_array(rgb), int((d > 0).sum()), d.size


def _image43(c: Canvas, surf, x: float, y: float, w: float, h: float, alpha: float = 1.0) -> None:
    """A 320x200 frame shown 4:3 (w x h), nearest neighbour, as id's 16:10 mode on a 4:3 monitor."""
    ctx = c.ctx
    ctx.save()
    ctx.translate(x, y)
    ctx.scale(w / surf.get_width(), h / surf.get_height())
    ctx.set_source_surface(surf, 0, 0)
    ctx.get_source().set_filter(cairo.FILTER_NEAREST)
    ctx.rectangle(0, 0, surf.get_width(), surf.get_height())
    ctx.clip()
    ctx.paint_with_alpha(alpha)
    ctx.restore()


def card_bd1(c: Canvas, t: float, sh: dict) -> None:
    """BD1: e1m1's two frames (id's C, the port) slide together; a minus sign on "subtract"; they merge
    into the diff, which grows to fill the frame (black)."""
    from qkit import load_image
    o = dict(sh.get("card_opts", {}))
    o["c"], o["port"] = _first(o["c"]), _first(o["port"])
    L = sh["len"]
    c.rect(0, 0, W, H, fill="bg_deep")
    if not (o["c"] and o["port"]):
        c.text("BD1: compare.py's frames not rendered yet", W / 2, H / 2, 3, "dim", align="center")
        return
    cu = sh["cues"].get("minus") or 1.0
    merge = max(cu + 0.8, L - 1.2)
    u = ramp(t, 0.0, merge, "inout")             # the slide
    gw, gh = 880, 660
    lx = 60 + (W / 2 - gw / 2 - 60) * u
    rx = (W - 60 - gw) - ((W - 60 - gw) - (W / 2 - gw / 2)) * u
    y = (H - gh) / 2
    m = ramp(t, merge, 0.6, "inout")             # the merge into the diff, then its growth to the full frame
    if m < 1:
        with c.group(1 - m):
            _image43(c, load_image(o["c"]), lx, y, gw, gh)
            _image43(c, load_image(o["port"]), rx, y, gw, gh)
            if u < 0.85:
                c.text("ID'S C", lx, y - 40, 3, BRONZE, alpha=1 - ramp(u, 0.6, 0.25))
                c.text("THE PORT", rx, y - 40, 3, BRONZE, alpha=1 - ramp(u, 0.6, 0.25))
    if cu is not None and cu <= t < merge + 0.3:
        a_ = 1.0 if t < merge else 1 - ramp(t, merge, 0.3)
        c.rect(W / 2 - 96, H / 2 - 18, 192, 36, fill="bg_deep", alpha=0.85 * a_)   # v5 (newcomer I8): a large,
        c.rect(W / 2 - 80, H / 2 - 10, 160, 20, fill="white", alpha=a_)            # bright minus in the gap
    if m > 0:
        surf, n, tot = _diff_surface(o["c"], o["port"])
        g = ramp(t, merge + 0.2, max(0.2, L - merge - 0.4), "inout")
        w_, h_ = gw + (1440 - gw) * g, gh + (1080 - gh) * g
        _image43(c, surf, (W - w_) / 2, (H - h_) / 2, w_, h_, alpha=m)
        c.rect((W - w_) / 2, (H - h_) / 2, w_, h_, stroke="rust_dark", lw=2, alpha=m * (1 - g))


def _bd_label(c: Canvas, o: dict, alpha: float = 1.0) -> None:
    c.text(qsafe(o.get("label", "ID'S C - THE PORT · E1M1 · 320×200")), 96, H - 104, 4, BRONZE, alpha=0.85 * alpha)  # v5: 4x


def card_bd3(c: Canvas, t: float, sh: dict) -> None:
    """BD3: still black; the small label; on "On the standard views", the four standard maps' real diffs in a
    2x2 grid, each with its measured share of matching pixels."""
    o = sh.get("card_opts", {})
    c.rect(0, 0, W, H, fill="bg_deep")
    lab = sh["cues"].get("label", 1.9)
    grid = sh["cues"].get("grid")
    if t >= lab:  # the label names E1M1 until the grid shows the four maps (`label_grid`)
        o2 = dict(o, label=o["label_grid"]) if (o.get("label_grid") and grid is not None and t >= grid) else o
        _bd_label(c, o2, ramp(t, lab, 0.5))
    if grid is None or t < grid:
        return
    tiles = o.get("maps", [])
    tw, th = 520, 390
    x0, y0 = (W - 2 * tw - 60) / 2, 70
    for k, mp in enumerate(tiles):
        a = ramp(t, grid + 0.12 * k, 0.15)
        if a <= 0:
            continue
        x, y = x0 + (k % 2) * (tw + 60), y0 + (k // 2) * (th + 70)
        c.rect(x - 2, y - 2, tw + 4, th + 4, stroke="rust_dark", lw=2, alpha=a)
        cp, pp = _first(mp.get("c")), _first(mp.get("port"))
        if cp and pp:
            surf, n, tot = _diff_surface(cp, pp)
            _image43(c, surf, x, y, tw, th, alpha=a)
            share = f"{100.0 * (tot - n) / tot:.2f}%" if n else ("100.0000%" if o.get("four_places") else "100.00%")
            if n:
                share = f"{100.0 * (tot - n) / tot:.4f}% · {n} PIXEL{'S' if n != 1 else ''}"
            c.text(f"{mp['name'].upper()}  {share}", x, y + th + 10, 4, BRONZE, alpha=a)  # v5: 4x (5 px at 3x on a phone)
        else:
            c.text(f"{mp['name'].upper()}  (diff not rendered yet)", x, y + th + 14, 2, "dim", alpha=a)


def card_bd4(c: Canvas, t: float, sh: dict) -> None:
    """BD4: BD2's black frame with BD3's small label."""
    c.rect(0, 0, W, H, fill="bg_deep")
    _bd_label(c, sh.get("card_opts", {}), ramp(t, 0.2, 0.4))


def card_endcard_v3(c: Canvas, t: float, sh: dict) -> None:
    """S64 (v3): the end card; its last line the user's credit gag."""
    c.rect(0, 0, W, H, fill="bg_deep")
    c.text("quake-srp", W / 2, 300, 8, BRONZE, alpha=fade(t, 0.0, None, 0.3), align="center")
    rows = ["id Software's Quake (1996)", "Quake shareware data (c) id Software",
            "written by Claude · score synthesized"]
    for i, r in enumerate(rows):
        c.text(qsafe(r), W / 2, 480 + i * 64, 3, BRONZE, alpha=fade(t, 0.35 + 0.3 * i, None, 0.3), align="center")
    c.text("Narrated in SRP: Slop Received Pronunciation", W / 2, 720, 3, "white", alpha=fade(t, 1.6, None, 0.4),
           align="center")


def card_endcard_v4(c: Canvas, t: float, sh: dict) -> None:
    """S64 (v4): v3's end card, with Quake's programmers after "id Software's Quake (1996)", as id's own quit
    screen credits them (quake-c/WinQuake/menu.c), and the gag held at full opacity from 2.4 s."""
    c.rect(0, 0, W, H, fill="bg_deep")
    c.text("quake-srp", W / 2, 190, 8, BRONZE, alpha=fade(t, 0.0, None, 0.3), align="center")
    rows = [("id Software's Quake (1996)", 4, BRONZE), ("programming: John Carmack · Michael Abrash", 4, BRONZE),
            ("John Cash · Dave 'Zoid' Kirsch", 4, BRONZE),
            ("Quake shareware data (c) id Software", 4, BRONZE), ("written by Claude · score synthesized", 4, BRONZE),
            ("narrated in SRP: Slop Received Pronunciation", 4, BRONZE)]   # a credit like the others, no emphasis
    ys = [330, 404, 452, 540, 610, 680]
    for i, (r, sc, col) in enumerate(rows):
        c.text(qsafe(r), W / 2, ys[i], sc, col, alpha=fade(t, 0.35 + 0.25 * i, None, 0.3), align="center")


CARDS.update({"bd1": card_bd1, "bd3": card_bd3, "bd4": card_bd4, "endcard_v3": card_endcard_v3,
              "endcard_v4": card_endcard_v4})

def ov_disclaimer(c: Canvas, t: float, sh: dict) -> None:
    """S01: the disclaimer, small and quiet at the foot of the very first screen, before the voice starts."""
    ln = sh.get("frames", 0) / 60 or 4.5
    c.text("NARRATED BY AI", W / 2, 1016, 3, BRONZE, alpha=0.85 * fade(t, 0.5, ln - 0.9, 0.5, 0.5), align="center")


CARDS.update({"black": card_black, "still_rings": card_still_rings, "push": card_push})
STATIC_OVERLAYS = {"label"}

OVERLAYS = {"ladder": ov_ladder, "menu_row": ov_menu_row, "slider_row": ov_slider_row, "fragments": ov_fragments,
            "terminal_over": ov_terminal_over, "subtitle": ov_subtitle, "label": ov_label,
            "frame_counter": ov_frame_counter, "slop_title": ov_slop_title, "rules": ov_rules, "proof_crawl": ov_proof_crawl,
            "save_text": ov_save_text, "offline_badge": ov_offline_badge, "disclaimer": ov_disclaimer}

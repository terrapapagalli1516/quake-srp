"""qkit: the film's diagram kit. Quake's palette and lettering, a timeline, and primitives.

    from qkit import *          # Canvas, Axes, Cues, ramp, fade, keys, run, look as Q ...

A diagram is a script with four parts: the facts (computed from the pak or the repository,
never typed in where they can be derived), the cues (`CUES = Cues(title=0.2, ...)`, the
moments in seconds, retimed with `--cues FILE`), `frame(c, t)` (the whole picture at time t,
drawn from nothing every frame), and `run(frame, duration=..., name=..., cues=CUES)`, which
gives the script its command line (see `render`). The scripts beside this package are the
examples.

Modules: `anim` (time: ramp, fade, keys, Cues), `look` (the frame size, rate and colours
from id's palette), `text` (conchars, the status bar's digits, Inter, equations), `canvas`
(the primitives), `graph` (Axes), `quake` (id's pak: palette, gfx.wad, a BSP reader, and
frames from quaketool), `render` (frames, video, stills, contact sheets).
"""

from . import look
from . import look as Q
from . import quake, text
from .anim import Cues, EASE, clamp01, fade, keys, lerp, lerp_log, ramp, stagger, typed
from .canvas import Canvas, load_image, new_canvas, surface_from_array
from .graph import Axes, nice_ticks
from .look import FPS, H, W, color, mix, shade
from .render import contact_sheet, duration_arg, encode, render_frame, run

__all__ = [
    "Q", "look", "quake", "text", "Cues", "EASE", "clamp01", "fade", "keys", "lerp", "lerp_log", "ramp", "stagger",
    "typed", "Canvas", "load_image", "new_canvas", "surface_from_array", "Axes", "nice_ticks", "FPS", "H", "W",
    "color", "mix", "shade", "contact_sheet", "duration_arg", "encode", "render_frame", "run",
]

"""VAAPI, the GPU's video encoders, as the film's stages use them, and the check every file they encode passes.

hevc_vaapi's stream carries two different PPSs: the one in the file's global header says init_qp_minus26 for its QP,
the in-band one its slices use says 0. Tagged hvc1, an mp4 keeps only the global one, and every slice then decodes
10 QP off: CABAC errors and garbage frames. So hevc_vaapi output is never tagged hvc1 (the default, hev1, keeps the
in-band headers; hvc1 stays only on software x265's phone file, whose headers agree), and a file a VAAPI encoder
wrote is accepted, into a cache or an output, only once all of it decodes clean in software. The GPU's own decoder
is no check: it decodes those garbage frames without a word. The software decode costs about 6 s of CPU per 7 s
of 4K.

    sys.path.insert(0, str(PIPELINE))       # film/pipeline/, as for filmroot
    import vaapi
    vaapi.encode(cmd, out)                  # run an ffmpeg command that writes `out` through a VAAPI encoder, check it
    vaapi.decodes_clean(path)               # None if clean, else ffmpeg's first errors

A file that fails is encoded once more; a second failure stops the stage with an error. The device is VAAPI_DEVICE
(default /dev/dri/renderD128).
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

DEVICE = os.environ.get("VAAPI_DEVICE", "/dev/dri/renderD128")


def decodes_clean(path) -> str | None:
    """Decode the whole file in software: None if ffmpeg reports nothing, else its first lines of errors."""
    r = subprocess.run(["ffmpeg", "-nostdin", "-v", "error", "-i", str(path), "-f", "null", "-"],
                       stdin=subprocess.DEVNULL, capture_output=True, text=True)
    err = (r.stderr or "").strip()
    if r.returncode == 0 and not err:
        return None
    return "\n".join(err.splitlines()[:4]) or f"ffmpeg exited {r.returncode}"


def encode(cmd: list[str], out, tries: int = 2, log=None) -> None:
    """Run `cmd` (an ffmpeg command writing `out` through a VAAPI encoder), then decode all of `out`. A file that does
    not decode clean is encoded again, up to `tries` times in all; then RuntimeError."""
    out = Path(out)
    say = log or (lambda m: print(m, file=sys.stderr, flush=True))
    for attempt in range(1, tries + 1):
        r = subprocess.run([str(c) for c in cmd], stdin=subprocess.DEVNULL, capture_output=True, text=True)
        if r.returncode:
            raise RuntimeError(f"{cmd[0]} failed ({r.returncode}):\n{' '.join(map(str, cmd))}\n{r.stderr[-3000:]}")
        bad = decodes_clean(out)
        if bad is None:
            return
        say(f"vaapi: {out.name} does not decode clean (try {attempt} of {tries}): {bad.splitlines()[0]}")
    raise RuntimeError(f"vaapi: {out} still does not decode clean after {tries} encodes; the last errors:\n{bad}")

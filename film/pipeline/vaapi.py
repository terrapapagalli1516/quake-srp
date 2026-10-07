"""VAAPI, the GPU's video encoders, as the film's stages use them, and the checks every file they encode passes.

Mesa's radeonsi writes its own parameter sets into the stream (VPS, SPS, PPS), and they are not the ones FFmpeg
puts in the file's header: the PPS's initial QP is 0 in-band against the real QP in the header (H.264 and HEVC),
and the SPS differs too (HEVC's transform depth and VPS timing; H.264's 8x8 transform and motion-vector lengths).
Mesa issue 15529, open. A player that trusts the header decodes every slice against the wrong sets: garbage, and
quietly (FFmpeg drops the in-band sets for an hvc1 file, and then neither its software nor the GPU's decoder says
a word on a small clip). So:

- encode() builds the header from the stream's own sets (`-flags:v -global_header` and the
  `extract_extradata=remove=0` bitstream filter, Jellyfin's workaround), so the two agree for every player;
- a file is accepted, into a cache or an output, only if its HEVC is tagged hev1 (never hvc1, which drops the
  in-band sets), its header's parameter sets are field for field the ones in its first packet (headers_agree),
  and all of it decodes clean in software (the GPU's decoder is no check). The software decode costs about 6 s
  of CPU per 7 s of 4K.

    sys.path.insert(0, str(PIPELINE))       # film/pipeline/, as for filmroot
    import vaapi
    vaapi.encode(cmd, out)                  # run an ffmpeg command that writes `out` through a VAAPI encoder, check it
    vaapi.check(path)                       # the checks alone: None if the file passes, else why not

A file that fails to decode is encoded once more; a second failure, or a header that disagrees, stops the stage with
an error. The device is VAAPI_DEVICE (default /dev/dri/renderD128). CHECKS names this version of the encode and its
checks, for caches to key on.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
from pathlib import Path

DEVICE = os.environ.get("VAAPI_DEVICE", "/dev/dri/renderD128")
CHECKS = "vaapi-v2: header from the stream, hev1, headers agree, software decode"
UNITS = ("Video Parameter Set", "Sequence Parameter Set", "Picture Parameter Set")


def decodes_clean(path) -> str | None:
    """Decode the whole file in software: None if ffmpeg reports nothing, else its first lines of errors."""
    r = subprocess.run(["ffmpeg", "-nostdin", "-v", "error", "-i", str(path), "-f", "null", "-"],
                       stdin=subprocess.DEVNULL, capture_output=True, text=True)
    err = (r.stderr or "").strip()
    if r.returncode == 0 and not err:
        return None
    return "\n".join(err.splitlines()[:4]) or f"ffmpeg exited {r.returncode}"


def codec_tag(path) -> tuple[str, str]:
    """(codec name, codec tag) of the first video stream, e.g. ("hevc", "hev1")."""
    r = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
                        "stream=codec_name,codec_tag_string", "-of", "csv=p=0", str(path)],
                       stdin=subprocess.DEVNULL, capture_output=True, text=True)
    return tuple((r.stdout.strip().split(",") + ["", ""])[:2])


def parameter_sets(path) -> dict:
    """The file's parameter sets, field by field, as FFmpeg's trace_headers reads them: {"extradata": {unit: [(field,
    value)]}, "packet": {...}}, the header's and the first packet's."""
    r = subprocess.run(["ffmpeg", "-nostdin", "-v", "debug", "-i", str(path), "-map", "0:v:0", "-c:v", "copy",
                        "-bsf:v", "trace_headers", "-frames:v", "1", "-f", "null", "-"],
                       stdin=subprocess.DEVNULL, capture_output=True, text=True)
    out: dict = {"extradata": {}, "packet": {}}
    sec = unit = None
    for line in r.stderr.splitlines():
        m = re.match(r"\[trace_headers @ [^\]]+\] (.*)$", line)
        if not m:
            continue
        t = m.group(1)
        if t == "Extradata":
            sec, unit = "extradata", None
        elif t.startswith("Packet"):
            if out["packet"]:
                break
            sec, unit = "packet", None
        elif t in UNITS and sec:
            unit = t
            out[sec].setdefault(unit, [])
        elif unit and (f := re.match(r"\d+\s+(\S+)\s+[01]+ = (-?\d+)$", t)):
            out[sec][unit].append((f.group(1), f.group(2)))
        elif not re.match(r"\d+\s", t):
            unit = None
    return out


def headers_agree(path) -> str | None:
    """None if every parameter set in the file's header is field for field the one in its first packet (or the
    stream carries none in-band), else the first fields that differ."""
    ps = parameter_sets(path)
    bad = []
    for unit in UNITS:
        a, b = ps["extradata"].get(unit), ps["packet"].get(unit)
        if a is None or b is None or a == b:
            continue
        da, db = dict(a), dict(b)
        diff = [f"{k} {da.get(k)} in the header, {db.get(k)} in-band" for k in dict.fromkeys([*da, *db])
                if da.get(k) != db.get(k)]
        bad.append(f"{unit}: " + "; ".join(diff[:3]))
    return " | ".join(bad) or None


def structure(path) -> str | None:
    """The structural checks (cheap): HEVC tagged hev1, and the header's parameter sets the stream's."""
    name, tag = codec_tag(path)
    if name == "hevc" and tag != "hev1":
        return f"HEVC tagged {tag or 'nothing'}, not hev1: an hvc1 file drops the stream's own parameter sets"
    disagree = headers_agree(path)
    if disagree:
        return f"its header's parameter sets are not the stream's: {disagree}"
    return None


def check(path) -> str | None:
    """Every check: None if the file can be accepted, else why not."""
    return structure(path) or decodes_clean(path)


def consistent(cmd: list[str]) -> list[str]:
    """`cmd` with its header built from the stream's own parameter sets: -flags:v -global_header, and
    extract_extradata=remove=0 last in its video bitstream filters. The output is the command's last argument."""
    cmd = [str(c) for c in cmd]
    if "-bsf:v" in cmd:
        i = cmd.index("-bsf:v") + 1
        if "extract_extradata" not in cmd[i]:
            cmd[i] += ",extract_extradata=remove=0"
        extra = []
    else:
        extra = ["-bsf:v", "extract_extradata=remove=0"]
    if "-global_header" not in cmd:
        extra = ["-flags:v", "-global_header", *extra]
    return cmd[:-1] + extra + cmd[-1:]


def encode(cmd: list[str], out, tries: int = 2, log=None) -> None:
    """Run `cmd` (an ffmpeg command writing `out`, its last argument, through a VAAPI encoder) with its header made
    consistent, then check `out`. A file that does not decode clean is encoded again, up to `tries` times in all;
    a structural failure (the command's, not chance) is not retried. RuntimeError on a failure."""
    out = Path(out)
    say = log or (lambda m: print(m, file=sys.stderr, flush=True))
    cmd = consistent(cmd)
    for attempt in range(1, tries + 1):
        r = subprocess.run(cmd, stdin=subprocess.DEVNULL, capture_output=True, text=True)
        if r.returncode:
            raise RuntimeError(f"{cmd[0]} failed ({r.returncode}):\n{' '.join(cmd)}\n{r.stderr[-3000:]}")
        wrong = structure(out)
        if wrong:
            raise RuntimeError(f"vaapi: {out}: {wrong}")
        bad = decodes_clean(out)
        if bad is None:
            return
        say(f"vaapi: {out.name} does not decode clean (try {attempt} of {tries}): {bad.splitlines()[0]}")
    raise RuntimeError(f"vaapi: {out} still does not decode clean after {tries} encodes; the last errors:\n{bad}")

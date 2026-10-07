"""Shared pieces of the viewing kit: where things are, the cut's edit, time codes, fonts, small helpers.

Where things are comes from film/pipeline/filmroot.py: FILM (FILM_ROOT) holds the film's media in its own
layout (edit/vN/build-NNN/, voice/, music/, sound/, footage/, diagrams/), and the kit's cache and default
report folders go under FILM_SCRATCH/watch/. WATCH_CACHE, if set, moves the cache (Scribe's transcripts and
the re-made buses) elsewhere, so several report folders can share it."""

from __future__ import annotations

import json
import math
import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path

KIT = Path(__file__).resolve().parent
PIPELINE = KIT.parent.parent                      # film/pipeline: filmroot, and the edit's and the sound's code
if str(PIPELINE) not in sys.path:
    sys.path.insert(0, str(PIPELINE))
import filmroot  # noqa: E402

FILM = filmroot.FILM
CACHE = Path(os.environ.get("WATCH_CACHE") or filmroot.SCRATCH / "watch" / "cache").resolve()   # made on first use
FONTS = ["JetBrains Mono", "DejaVu Sans Mono"]   # the first one installed; matplotlib carries DejaVu

T0 = time.time()


def log(*a) -> None:
    print(f"[watch {time.time() - T0:6.1f}s]", *a, file=sys.stderr, flush=True)


def tc(t: float | None, places: int = 2) -> str:
    """Film time as m:ss.ss."""
    if t is None:
        return "-"
    sign = "-" if t < 0 else ""
    t = abs(t)
    m = int(t // 60)
    s = t - 60 * m
    w = 3 + places
    return f"{sign}{m}:{s:0{w}.{places}f}"


def parse_tc(s: str) -> float:
    """'1:17.2', '77.2', '0:05' -> seconds."""
    s = s.strip()
    if ":" in s:
        m, x = s.split(":", 1)
        return int(m) * 60 + float(x)
    return float(s)


def parse_range(s: str) -> tuple[float, float]:
    """'1:17.2-1:24.1' -> (77.2, 84.1)."""
    m = re.match(r"^\s*([\d:.]+)\s*[-–]\s*([\d:.]+)\s*$", s)
    if not m:
        raise SystemExit(f"--range {s!r}: want A-B, as 1:17.2-1:24.1")
    a, b = parse_tc(m.group(1)), parse_tc(m.group(2))
    if b <= a:
        raise SystemExit(f"--range {s!r}: the end is before the start")
    return a, b


def run(cmd: list[str], **kw) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, check=True, **kw)


def probe(path: Path) -> dict:
    r = subprocess.run(["ffprobe", "-v", "error", "-show_entries",
                        "stream=codec_type,width,height,r_frame_rate,nb_frames,duration,sample_rate,channels",
                        "-show_entries", "format=duration,size", "-of", "json", str(path)],
                       capture_output=True, text=True, check=True)
    j = json.loads(r.stdout)
    v = next((s for s in j["streams"] if s["codec_type"] == "video"), {})
    a = next((s for s in j["streams"] if s["codec_type"] == "audio"), {})
    num, den = (v.get("r_frame_rate") or "60/1").split("/")
    return {"width": int(v.get("width", 0)), "height": int(v.get("height", 0)), "fps": float(num) / float(den),
            "frames": int(v.get("nb_frames") or 0), "duration": float(j["format"].get("duration", 0)),
            "size": int(j["format"].get("size", 0)), "audio": bool(a),
            "sample_rate": int(a.get("sample_rate", 0) or 0), "channels": int(a.get("channels", 0) or 0)}


def absolute(p: Path | str) -> Path:
    """An absolute path, symlinks kept: a FILM_ROOT may be put together from links to files kept elsewhere."""
    return Path(os.path.abspath(p))


def rel(p: Path | str) -> str:
    """A path as the report shows it: relative to FILM_ROOT where it is under it."""
    for q in (absolute(p), Path(p).resolve()):
        try:
            return str(q.relative_to(FILM))
        except ValueError:
            pass
    return str(p)


# ---------------------------------------------------------------- the edit ----

@dataclass
class Edit:
    """The cut and what is known about how it was made."""
    cut: Path
    info: dict
    build: Path | None = None          # edit/.../build-NNN that made this file
    timeline: dict | None = None
    timeline_path: Path | None = None
    clock: dict | None = None
    clock_path: Path | None = None
    config_path: Path | None = None
    config: dict | None = None
    mix: dict | None = None
    notes: list[str] = field(default_factory=list)

    @property
    def fps(self) -> float:
        """The file's own frame rate (a preview may not run at the timeline's 60)."""
        return float(self.info["fps"] or (self.timeline or {}).get("fps") or 60.0)

    @property
    def nframes(self) -> int:
        return int(self.info["frames"] or round(self.info["duration"] * self.fps))

    @property
    def duration(self) -> float:
        return self.nframes / self.fps

    @property
    def shots(self) -> list[dict]:
        return (self.timeline or {}).get("shots", [])

    @property
    def sections(self) -> list[dict]:
        return (self.timeline or {}).get("sections", [])

    def shot_at(self, t: float) -> dict | None:
        """The shot on screen at film time t (by frame: frame f shows from f/fps)."""
        return self.shot_at_frame(int(math.floor(t * self.fps + 1e-3)))

    def shot_at_frame(self, f: int) -> dict | None:
        for s in self.shots:
            if s["frame"] <= f < s["frame"] + s["frames"]:
                return s
        return self.shots[-1] if self.shots and f >= self.shots[-1]["frame"] else None

    def shot_id_at(self, t: float) -> str:
        s = self.shot_at(t)
        return s["id"] if s else "-"

    def cuts(self) -> list[float]:
        """Film times of the cuts between shots (not 0, not the end)."""
        return [s["start"] for s in self.shots[1:]]

    def section_of(self, s: dict) -> dict | None:
        return next((x for x in self.sections if x["id"] == s.get("section")), None)


def _same_film(a: Path, size: int, dur: float) -> bool:
    try:
        return a.stat().st_size == size
    except OSError:
        return False


def find_build(cut: Path, size: int) -> Path | None:
    """The build-NNN whose film.mp4 is this file (published copies keep the bytes)."""
    if cut.name == "film.mp4" and cut.parent.name.startswith("build-"):
        return cut.parent
    roots = {cut.parent, cut.parent.parent, FILM / "edit"}
    roots |= {p for p in (FILM / "edit").glob("v*") if p.is_dir()}
    cands = []
    for r in roots:
        for b in r.glob("build-[0-9][0-9][0-9]"):
            f = b / "film.mp4"
            if f.exists() and f.stat().st_size == size:
                cands.append(b)
    if not cands:
        return None
    return max(cands, key=lambda b: (b / "film.mp4").stat().st_mtime)


def load_toml(p: Path) -> dict:
    import tomllib
    return tomllib.loads(p.read_text())


def locate(cut: Path, timeline: str | None = None, build: str | None = None, clock: str | None = None,
           config: str | None = None) -> Edit:
    cut = absolute(cut)
    info = probe(cut)
    e = Edit(cut=cut, info=info)
    e.build = absolute(build) if build else find_build(cut, info["size"])
    if e.build:
        e.notes.append(f"made by `{rel(e.build)}` (its film.mp4 has this file's bytes)")
    # the timeline: given, else the build's, else one beside the cut
    if timeline:
        e.timeline_path = absolute(timeline)
    elif e.build and (e.build / "timeline.json").exists():
        e.timeline_path = e.build / "timeline.json"
    elif (cut.parent / "timeline.json").exists():
        e.timeline_path = cut.parent / "timeline.json"
        e.notes.append("no build found for this file: using the timeline beside it, which may be newer than the cut")
    if e.timeline_path:
        e.timeline = json.loads(e.timeline_path.read_text())
        tfps = float(e.timeline.get("fps") or 60)
        if info["fps"] and abs(tfps - info["fps"]) > 0.01:   # a preview at another rate: shots in its own frames
            for s in e.timeline.get("shots", []):
                s["frame"] = int(round(s["start"] * info["fps"]))
                s["frames"] = max(1, int(round((s["start"] + s["len"]) * info["fps"])) - s["frame"])
            e.notes.append(f"the file runs at {info['fps']:g} fps, the timeline at {tfps:g}: shots mapped by time")
        d = abs(e.timeline["duration"] - info["duration"])
        if d > 1.5 / e.fps:
            e.notes.append(f"WARNING: the timeline lasts {e.timeline['duration']:.3f} s, the file {info['duration']:.3f} s: "
                           "they may not be the same edit")
    else:
        e.notes.append("no timeline: shots, words-per-shot and sync are left out; sheets run on time alone")
    # the config: beside the build (edit/v7/build-NNN -> edit/v7/edit.toml)
    if config:
        e.config_path = absolute(config)
    elif e.build and (e.build.parent / "edit.toml").exists():
        e.config_path = e.build.parent / "edit.toml"
    elif e.timeline_path and (e.timeline_path.parent / "edit.toml").exists():
        e.config_path = e.timeline_path.parent / "edit.toml"
    if e.config_path:
        try:
            e.config = load_toml(e.config_path)
        except Exception as ex:  # noqa: BLE001
            e.notes.append(f"could not read {rel(e.config_path)}: {ex}")
    # the clock: only if it is this edit's
    cands = [absolute(clock)] if clock else []
    if e.build:
        cands += [e.build / "clock.json", e.build.parent / "clock.json"]
    if e.timeline_path:
        cands.append(e.timeline_path.parent / "clock.json")
    for c in cands:
        if c.exists():
            j = json.loads(c.read_text())
            if e.timeline and not clock:
                starts = [round(x["start"], 3) for x in j.get("shots", [])]
                want = [round(x["start"], 3) for x in e.shots]
                if starts != want:
                    e.notes.append(f"`{rel(c)}` is another edit's clock (its shots differ): not used")
                    continue
            e.clock, e.clock_path = j, c
            break
    if e.build and (e.build / "mix.json").exists():
        e.mix = json.loads((e.build / "mix.json").read_text())
    return e


def pick_sfx(mix_note: str | None, timeline_dir: str, duration: float, glob: str | None) -> tuple[Path | None, str]:
    """The sound designer's stem for this cut. The build's mix.json names the file it used, but a stem can be
    rewritten for a later edit under the same name: a stem counts only if its sidecar says it was made on this
    edit's clock (the timeline's folder) and lasts as long as the film."""
    named = (mix_note or "").split(" (")[0].strip()
    cands = ([FILM / named] if named else []) + (sorted(FILM.glob(glob), key=lambda p: p.stat().st_mtime, reverse=True) if glob else [])
    seen = set()
    for p in cands:
        if p in seen or not p.exists():
            continue
        seen.add(p)
        side = p.with_suffix(".json")
        if not side.exists():
            continue
        try:
            meta = json.loads(side.read_text())
        except Exception:  # noqa: BLE001
            continue
        if Path(meta.get("timeline", "")).parent.name == Path(timeline_dir).name and abs(float(meta.get("duration_s", 0)) - duration) < 0.05:
            if named and p != FILM / named:
                return p, (f"the build used `{named}`, which has since been rewritten for another clock; `{rel(p)}` is made "
                           "on this cut's clock and may differ from what the build mixed")
            return p, f"`{rel(p)}`"
    return None, (f"no SFX stem made on this cut's clock is left (the build used `{named}`, since rewritten)" if named
                  else "no SFX stem for this cut")


def ensure_dir(p: Path) -> Path:
    p.mkdir(parents=True, exist_ok=True)
    return p


def fresh_run_dir(cut: Path) -> Path:
    """A new report folder, FILM_SCRATCH/watch/runs/<cut>-NNN."""
    base = ensure_dir(filmroot.scratch("watch") / "runs")
    n = 1
    while (base / f"{cut.stem}-{n:03d}").exists():
        n += 1
    d = base / f"{cut.stem}-{n:03d}"
    d.mkdir()
    return d


_FONT_FILES: dict[bool, str | None] = {}


def font_file(bold: bool = False) -> str | None:
    """The sheets' monospace font: the first of FONTS installed, found by matplotlib's font manager."""
    if bold not in _FONT_FILES:
        try:
            from matplotlib import font_manager
            prop = font_manager.FontProperties(family=FONTS, weight="bold" if bold else "normal")
            _FONT_FILES[bold] = font_manager.findfont(prop, fallback_to_default=True)
        except Exception:  # noqa: BLE001
            _FONT_FILES[bold] = None
    return _FONT_FILES[bold]


def font(size: int, bold: bool = False):
    from PIL import ImageFont
    p = font_file(bold)
    try:
        return ImageFont.truetype(p, size) if p else ImageFont.load_default()
    except OSError:
        return ImageFont.load_default()


def md_table(head: list[str], rows: list[list], align: str | None = None) -> str:
    """A Markdown table; align is one letter per column: l, r or c."""
    def cell(x) -> str:
        return str(x).replace("|", "\\|").replace("\n", " ")
    sep = {"l": "---", "r": "--:", "c": ":-:"}
    al = (align or "").ljust(len(head), "l")
    out = ["| " + " | ".join(head) + " |", "|" + "|".join(sep.get(a, "---") for a in al) + "|"]
    out += ["| " + " | ".join(cell(x) for x in r) + " |" for r in rows]
    return "\n".join(out)


class Flags:
    """Findings, each with a time, a severity and where the evidence is. Severity: 'fix' (likely a real
    problem), 'look' (worth a look), 'info'."""

    def __init__(self, part: str):
        self.part = part
        self.items: list[dict] = []

    def add(self, sev: str, t: float | None, what: str, evidence: str = "", t1: float | None = None, **kw) -> None:
        self.items.append({"part": self.part, "sev": sev, "t": None if t is None else round(float(t), 4),
                           "t1": None if t1 is None else round(float(t1), 4), "what": what, "evidence": evidence} | kw)

    def save(self, out: Path) -> None:
        (out / f"flags-{self.part}.json").write_text(json.dumps(self.items, indent=1))


def save_json(p: Path, x) -> None:
    p.write_text(json.dumps(x, indent=1, default=float))


def env_niceness() -> int:
    try:
        return os.getpriority(os.PRIO_PROCESS, 0)
    except OSError:
        return 0

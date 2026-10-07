#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Render the film, start to finish, in one command:

    uv run film/make.py --res 3840x2160 --voice VOICE_DIR --out OUT_DIR

It starts from what film/ holds (the shot files, edit.toml, the score's and the effects' sources, the diagram
scripts, the narration's map voice.json) and two outside inputs: id's shareware pak (quake-data/, as the rest of
the repository expects) and the voice clips (VOICE_DIR: the recorded takes, which are not in the repository).
Everything else is rendered into OUT_DIR, stage by stage:

  voice     the clips voice.json names, copied in                       OUT/voice/
  clock     the edit's clock from the narration                         OUT/edit/{timeline,clock,clock-score,ladder-events}.json
  footage   every game shot, the composites, the proof's frames         OUT/footage/game/, OUT/footage/proof/
  web       the browser captures (the page, its menus, the phone)       OUT/footage/web/
  diagrams  the diagrams and the slop-options ladder, on the clock      OUT/diagrams/
  score     the score, fitted to the clock and synthesized              OUT/music/score.wav (+ stems, MIDI)
  sfx       the sound effects on the clock                              OUT/sound/sfx.wav
  edit      the cut: the master, a 1080p copy and the phone files       OUT/cut.mp4, cut-1080p.mp4, cut-480p30-hevc.mp4,
                                                                        cut-480p30.mp4, cut-loudness.md, cut-contact.png
  subs      subtitles                                                   OUT/cut.srt, cut.vtt

It is incremental: each stage records what it was made from (OUT/.make/STAGE.json), and a stage whose inputs did
not change is skipped. The clock runs again as the media land (cheap), since the edit's timeline names them; its
times come from the narration and edit.toml alone, so they are the same at any resolution.

  --res WxH         3840x2160 (the film at 4K: every overlay, card and label at 2x its 1080 geometry) or 1920x1080
  --hw vaapi|none   vaapi (default): the GPU (VAAPI_DEVICE, default /dev/dri/renderD128) encodes the footage and the
                    edit's segments, decodes the footage, and makes the 1080p copy; the master is the segments joined
                    as they are. Every VAAPI file is checked (pipeline/vaapi.py). none: all in software, for a machine
                    without VAAPI. The phone files are always software x265/x264: at under 10 MB, quality per bit
                    decides.
  --stages a,b      only these stages (each still skips if up to date); --force a,b: remake these even if up to date
  --media STAGE=PATH  take a stage's output from elsewhere instead of rendering it: footage=DIR (with game/, proof/,
                    web/ in it), web=DIR, diagrams=DIR, score=WAV, sfx=WAV. Copied in (reflinks where the disk has
                    them). For trying the edit on media that exist; the film itself comes from the stages.
  --range A-B       the edit renders only this stretch (film seconds), as OUT/range.mp4: a quick look
  --burn-in         a review build: the timecode and the shot ids burned in
  --quaketool BIN   use this quaketool (default: build quake-rs's, as oracle/classic_check.py does)
  --jobs N          the edit's segments rendered at once (default: build.py's: 4 at 1080, 2 above)
  --mem-cap SIZE    each stage runs in a memory cap, a systemd user scope with MemoryMax=SIZE and no swap (default
                    12G; 0 for none), so a job that runs away is killed alone instead of the system's services

Caches and intermediates go to FILM_SCRATCH (default OUT/scratch: at 4K the edit's segments are large), and the
stages' temporary files under it (TMPDIR), on disk rather than in a tmpfs /tmp. Stopping make.py (SIGTERM, Ctrl-C)
stops the running stage the same way, so its temporary files are removed. Needs
what film/README.md lists (Rust, ffmpeg, uv, cairo, fonts), and VAAPI for --hw vaapi. Python only through uv.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

FILM = Path(__file__).resolve().parent          # the repository's film/: the film's source
REPO = FILM.parent
PIPE = FILM / "pipeline"
STAGES = ["voice", "clock", "footage", "web", "diagrams", "score", "sfx", "edit", "subs"]
MEDIA_STAGES = {"footage", "web", "diagrams", "score", "sfx"}


def log(*a) -> None:
    print(time.strftime("%H:%M:%S"), "make:", *a, flush=True)


class Make:
    def __init__(self, a: argparse.Namespace):
        w, _, h = a.res.partition("x")
        self.w, self.h = int(w), int(h)
        if self.w % 1920 or self.h != self.w * 9 // 16 or self.w // 1920 < 1:
            sys.exit(f"--res {a.res}: the film is 16:9 at a whole multiple of 1920x1080 (1920x1080, 3840x2160)")
        self.scale = self.w // 1920
        self.out = Path(a.out).resolve()
        self.voice = Path(a.voice).resolve() if a.voice else None
        self.hw = a.hw
        self.range = a.range
        self.burn_in = a.burn_in
        self.jobs = a.jobs
        self.cap = None
        if a.mem_cap.lower() not in ("0", "none", "off"):
            ok = shutil.which("systemd-run") and subprocess.run(
                ["systemd-run", "--user", "--scope", "-q", "-p", "MemoryMax=64M", "--", "true"],
                capture_output=True).returncode == 0
            if ok:
                self.cap = a.mem_cap
            else:
                log("no systemd user manager (systemd-run --user --scope): the stages run without a memory cap")
        self.force = set(filter(None, (a.force or "").split(",")))
        self.media = {}
        for m in a.media or []:
            k, _, v = m.partition("=")
            if k not in MEDIA_STAGES or not v:
                sys.exit(f"--media {m}: STAGE=PATH, STAGE one of {', '.join(sorted(MEDIA_STAGES))}")
            self.media[k] = Path(v).resolve()
        self.quaketool = Path(a.quaketool).resolve() if a.quaketool else None
        self.env = dict(os.environ, FILM_ROOT=str(self.out),
                        FILM_SCRATCH=os.environ.get("FILM_SCRATCH") or str(self.out / "scratch"))
        tmp = Path(self.env["FILM_SCRATCH"]) / "tmp"   # the stages' temporary files on disk too, not in a tmpfs /tmp
        tmp.mkdir(parents=True, exist_ok=True)
        self.env["TMPDIR"] = str(tmp)
        if self.quaketool:
            self.env["QUAKETOOL"] = str(self.quaketool)  # filmroot.py's, for every stage
        (self.out / ".make").mkdir(parents=True, exist_ok=True)

    # ------------------------------------------------------------ running ----

    def run(self, *cmd, cwd: Path | None = None) -> None:
        cmd = [str(c) for c in cmd]
        log("$", " ".join(cmd))
        cap = ["systemd-run", "--user", "--scope", "-q", "-p", f"MemoryMax={self.cap}", "-p", "MemorySwapMax=0",
               "--"] if self.cap else []
        # its own process group, so that stopping make.py (SIGTERM, Ctrl-C) stops the whole stage the same way:
        # every process in it gets SIGTERM and exits normally, removing its temporary files (filmroot.py)
        p = subprocess.Popen(cap + cmd, cwd=cwd or REPO, env=self.env, start_new_session=True)
        try:
            rc = p.wait()
        except BaseException:
            for sig, wait in ((signal.SIGTERM, 60), (signal.SIGKILL, 10)):
                try:
                    os.killpg(p.pid, sig)
                    p.wait(wait)
                    break
                except (ProcessLookupError, subprocess.TimeoutExpired):
                    continue
            raise
        r = subprocess.CompletedProcess(cmd, rc)
        if r.returncode:
            name = Path(cmd[2] if cmd[:2] == ["uv", "run"] else cmd[0]).name
            hint = (f" (a process killed with -9 or 137 reached --mem-cap {self.cap}: raise it, or lower --jobs)"
                    if self.cap else "")
            sys.exit(f"make: {name} failed ({r.returncode}){hint}")

    def uv(self, script: Path, *args) -> None:
        self.run("uv", "run", script, *args)

    def stamp(self, stage: str) -> Path:
        return self.out / ".make" / f"{stage}.json"

    def up_to_date(self, stage: str, sig: str, outputs: list[Path]) -> bool:
        st = self.stamp(stage)
        if stage in self.force or not st.exists() or not all(p.exists() for p in outputs):
            return False
        return json.loads(st.read_text()).get("sig") == sig

    def done(self, stage: str, sig: str, t0: float) -> None:
        self.stamp(stage).write_text(json.dumps({"sig": sig, "seconds": round(time.time() - t0, 1),
                                                 "when": time.strftime("%Y-%m-%d %H:%M:%S")}))

    def copy_in(self, src: Path, dst: Path) -> None:
        """A file or a folder copied into OUT (reflinks where the disk has them): the stages and the edit read only
        files under OUT, so a build keeps a copy of what it read."""
        dst.parent.mkdir(parents=True, exist_ok=True)
        if src.is_dir():
            dst.mkdir(parents=True, exist_ok=True)
            subprocess.run(["cp", "-a", "--reflink=auto", f"{src}/.", str(dst)], check=True)
        else:
            subprocess.run(["cp", "--reflink=auto", str(src), str(dst)], check=True)

    # --------------------------------------------------------- signatures ----

    @staticmethod
    def code(*paths: Path) -> list:
        """Source files by their content (small: code, configs, cue lists)."""
        out = []
        for p in paths:
            files = sorted(f for f in p.rglob("*") if f.is_file() and "__pycache__" not in f.parts) if p.is_dir() else [p]
            out += [[str(f.relative_to(REPO)), hashlib.sha256(f.read_bytes()).hexdigest()] for f in files if f.exists()]
        return out

    def files(self, *paths: Path, suffixes: tuple = ()) -> list:
        """Media by size and time (large: footage, movies, sound)."""
        out = []
        for p in paths:
            if not p.exists():
                continue
            for f in (sorted(p.rglob("*")) if p.is_dir() else [p]):
                if f.is_file() and (not suffixes or f.suffix in suffixes) and "scratch" not in f.parts:
                    st = f.stat()
                    out.append([str(f), st.st_size, st.st_mtime_ns])
        return out

    def clock_times(self) -> list:
        """The clock (its shots, lines, words and named moments, but not when it was written) and the ladder's
        events: what the diagrams, the score and the effects are made from."""
        out = []
        for name in ("clock.json", "ladder-events.json"):
            c = self.out / "edit" / name
            j = json.loads(c.read_text()) if c.exists() else None
            out.append({k: v for k, v in j.items() if k != "film"} if isinstance(j, dict) else j)
        return out

    @staticmethod
    def sig(*parts) -> str:
        return hashlib.sha256(json.dumps(parts, sort_keys=True, default=str).encode()).hexdigest()[:20]

    # ------------------------------------------------------------- stages ----

    def tool(self) -> Path:
        if self.quaketool:
            return self.quaketool
        log("quaketool: cargo build --release")
        self.run("cargo", "build", "--release", "--quiet", "--bin", "quaketool", cwd=REPO / "quake-rs")
        self.quaketool = REPO / "quake-rs" / "target" / "release" / "quaketool"
        self.env["QUAKETOOL"] = str(self.quaketool)
        return self.quaketool

    def stage_voice(self) -> None:
        vmap = FILM / "voice.json"
        lines = json.loads(vmap.read_text())["lines"]
        if self.voice is None:
            if self.up_to_date("voice", self.stamp_sig_voice(None, lines), [self.out / "voice" / "voice.json"]):
                log("voice: up to date")
                return
            sys.exit("make: --voice VOICE_DIR (the narration's clips) is needed: voice.json names the files in it")
        missing = [ln["file"] for ln in lines if not (self.voice / ln["file"]).exists()]
        if missing:
            sys.exit(f"make: {len(missing)} clips voice.json names are not in {self.voice}: {', '.join(missing[:4])}…")
        sig = self.stamp_sig_voice(self.voice, lines)
        outs = [self.out / "voice" / "voice.json"] + [self.out / "voice" / ln["file"] for ln in lines]
        if self.up_to_date("voice", sig, outs):
            log("voice: up to date")
            return
        t0 = time.time()
        for ln in lines:
            self.copy_in(self.voice / ln["file"], self.out / "voice" / ln["file"])
        shutil.copyfile(vmap, self.out / "voice" / "voice.json")
        log(f"voice: {len(lines)} clips")
        self.done("voice", sig, t0)

    def stamp_sig_voice(self, vdir: Path | None, lines: list) -> str:
        return self.sig(self.code(FILM / "voice.json"),
                        [self.files(vdir / ln["file"]) for ln in lines] if vdir else "stamped")

    def stage_clock(self) -> None:
        sig = self.sig(self.code(FILM / "edit.toml", FILM / "script.md", PIPE / "edit" / "timeline.py",
                                 PIPE / "filmroot.py"),
                       self.files(self.out / "voice" / "voice.json"),
                       [f[0] for f in self.files(self.out / "footage", self.out / "diagrams",
                                                 suffixes=(".mp4", ".mov", ".json", ".wav", ".ppm"))],
                       [f[0] for f in self.files(self.out / "music" / "score.wav", self.out / "sound" / "sfx.wav")])
        e = self.out / "edit"
        outs = [e / "timeline.json", e / "clock.json", e / "clock-score.json", e / "ladder-events.json"]
        if self.up_to_date("clock", sig, outs):
            log("clock: up to date")
            return
        t0 = time.time()
        self.uv(PIPE / "edit" / "timeline.py", "--config", FILM / "edit.toml")
        # the score is fitted to clock-score.json, a frozen copy: it changes only when the times do
        tl = json.loads((e / "timeline.json").read_text())
        times = [[s["id"], s["frame"], s["frames"]] for s in tl["shots"]] + [[v["id"], v["at"]] for v in tl["voice"]]
        cs = e / "clock-score.json"
        old = json.loads(cs.read_text()) if cs.exists() else None
        if old is None or times != [[s["id"], s["frame"], s["frames"]] for s in old["shots"]] + \
                [[v["id"], v["at"]] for v in old["voice"]]:
            shutil.copyfile(e / "timeline.json", cs)
            log("clock: clock-score.json renewed (the times changed)")
        self.done("clock", sig, t0)

    def media_stage(self, stage: str, dst: Path) -> bool:
        """--media STAGE=PATH: the stage's output copied in instead of rendered. True if it was."""
        src = self.media.get(stage)
        if src is None:
            return False
        sig = self.sig("media", str(src), len(self.files(src)))
        if self.up_to_date(stage, sig, [dst]):
            log(f"{stage}: media from {src}, up to date")
            return True
        t0 = time.time()
        log(f"{stage}: media from {src}")
        self.copy_in(src, dst)
        self.done(stage, sig, t0)
        return True

    def stage_footage(self) -> None:
        if self.media_stage("footage", self.out / "footage"):
            return
        script = PIPE / "footage" / "make_footage.py"
        if not script.exists():
            sys.exit("make: the footage stage (film/pipeline/footage/make_footage.py) is not here yet: "
                     "--media footage=DIR uses rendered footage")
        qt = self.tool()
        sig = self.sig(self.code(FILM / "shots", PIPE / "footage"), self.files(qt), self.w, self.h, self.hw)
        if self.up_to_date("footage", sig, [self.out / "footage" / "game"]):
            log("footage: up to date")
            return
        t0 = time.time()
        self.uv(script, "--res", f"{self.w}x{self.h}", "--out", self.out / "footage", "--quaketool", qt,
                "--hw", self.hw)
        self.done("footage", sig, t0)

    def stage_web(self) -> None:
        if self.media_stage("web", self.out / "footage" / "web"):
            return
        if "footage" in self.media and (self.out / "footage" / "web").exists():
            log("web: from the footage media")
            return
        script = PIPE / "web" / "capture.py"
        if not script.exists():
            sys.exit("make: the web stage (film/pipeline/web/capture.py) is not here yet: --media web=DIR")
        sig = self.sig(self.code(PIPE / "web", REPO / "web"), self.scale)
        if self.up_to_date("web", sig, [self.out / "footage" / "web"]):
            log("web: up to date")
            return
        t0 = time.time()
        self.uv(script, "--scale", self.scale, "--out", self.out / "footage" / "web")
        self.done("web", sig, t0)

    def stage_diagrams(self) -> None:
        if self.media_stage("diagrams", self.out / "diagrams"):
            return
        script = PIPE / "diagrams" / "render_all.py"
        if not script.exists():
            sys.exit("make: the diagrams stage (film/pipeline/diagrams/render_all.py) is not here yet: "
                     "--media diagrams=DIR")
        qt = self.tool()
        sig = self.sig(self.code(PIPE / "diagrams"), self.clock_times(), self.files(qt), self.scale)
        if self.up_to_date("diagrams", sig, [self.out / "diagrams"]):
            log("diagrams: up to date")
            return
        t0 = time.time()
        self.uv(script, "--scale", self.scale, "--clock", self.out / "edit", "--out", self.out / "diagrams")
        self.done("diagrams", sig, t0)

    def stage_score(self) -> None:
        wav = self.out / "music" / "score.wav"
        if "score" in self.media:
            src = self.media["score"]
            sig = self.sig("media", self.files(src))
            if not self.up_to_date("score", sig, [wav]):
                t0 = time.time()
                log(f"score: media from {src}")
                self.copy_in(src, wav)
                if src.with_suffix(".json").exists():
                    self.copy_in(src.with_suffix(".json"), wav.with_suffix(".json"))
                self.done("score", sig, t0)
            else:
                log(f"score: media from {src}, up to date")
            return
        music = PIPE / "music"
        sig = self.sig(self.code(music, PIPE / "filmroot.py"), self.clock_times())
        if self.up_to_date("score", sig, [wav, wav.with_suffix(".json")]):
            log("score: up to date")
            return
        t0 = time.time()
        # A working copy of the score's source: fitting it to the clock rewrites its .inc files and its .score, which
        # stay in the repository as the v7 cut's (filmroot.py beside it, as in film/pipeline/).
        work = self.out / "music" / "src" / "pipeline"
        if work.exists():
            shutil.rmtree(work)
        shutil.copytree(music, work / "music", ignore=shutil.ignore_patterns("__pycache__"))
        shutil.copyfile(PIPE / "filmroot.py", work / "filmroot.py")
        v7 = work / "music" / "v7"
        self.uv(v7 / "fit_events.py", self.out / "edit" / "clock.json", "-o", v7 / "events-v7.inc")
        self.uv(v7 / "make_scores.py")
        self.uv(v7 / "kills.py", work / "music" / "score-v10.score")
        self.uv(work / "music" / "synth" / "render.py", work / "music" / "score-v10.score", "-o", wav,
                "--stems", self.out / "music" / "stems", "--midi", self.out / "music" / "score.mid")
        self.done("score", sig, t0)

    def stage_sfx(self) -> None:
        wav = self.out / "sound" / "sfx.wav"
        if "sfx" in self.media:
            src = self.media["sfx"]
            sig = self.sig("media", self.files(src))
            if not self.up_to_date("sfx", sig, [wav]):
                t0 = time.time()
                log(f"sfx: media from {src}")
                self.copy_in(src, wav)
                meta = json.loads(src.with_suffix(".json").read_text()) if src.with_suffix(".json").exists() else {}
                meta.update(wav="sfx.wav", timeline="edit/timeline.json", clock="edit/clock.json")  # this clock's
                wav.with_suffix(".json").write_text(json.dumps(meta, indent=1))
                self.done("sfx", sig, t0)
            else:
                log(f"sfx: media from {src}, up to date")
            return
        snd = PIPE / "sound"
        sig = self.sig(self.code(snd, PIPE / "filmroot.py"), self.clock_times(),
                       self.files(self.out / "footage" / "game", suffixes=(".json", ".wav")))
        if self.up_to_date("sfx", sig, [wav, wav.with_suffix(".json")]):
            log("sfx: up to date")
            return
        t0 = time.time()
        self.uv(snd / "extract_id.py")
        self.uv(snd / "design.py")
        self.uv(snd / "render.py", snd / "cues-v7.md", "-o", wav, "--timeline", self.out / "edit" / "timeline.json")
        self.done("sfx", sig, t0)

    def stage_edit(self) -> None:
        build = PIPE / "edit" / "build.py"
        outs = [self.out / n for n in ("cut.mp4", "cut-480p30-hevc.mp4", "cut-480p30.mp4")]
        if self.scale > 1:
            outs.append(self.out / "cut-1080p.mp4")
        sig = self.sig(self.code(FILM / "edit.toml", FILM / "script.md", PIPE / "edit", PIPE / "diagrams" / "qkit",
                                 PIPE / "filmroot.py"),
                       self.files(self.out / "voice", self.out / "footage", self.out / "diagrams",
                                  self.out / "music" / "score.wav", self.out / "sound" / "sfx.wav"),
                       self.w, self.hw, self.burn_in, self.range)
        if not self.range and self.up_to_date("edit", sig, outs):
            log("edit: up to date")
            return
        t0 = time.time()
        self.tool()  # S26's lines are drawn in Quake's lettering by quaketool filmtext
        self.uv(PIPE / "edit" / "s26_lines.py", "--scale", self.scale, "--out", self.out / "edit" / "v5" / "art")
        args = ["--config", FILM / "edit.toml", "--root", self.out, "--scale", self.scale, "--hw", self.hw]
        if self.jobs:
            args += ["--jobs", self.jobs]
        if not self.burn_in:
            args.append("--no-burn-in")
        if self.range:
            self.uv(build, *args, "--range", self.range)
            b = sorted((self.out / "edit").glob("build-[0-9][0-9][0-9]"))[-1]
            shutil.copyfile(b / "range.mp4", self.out / "range.mp4")
            log(f"edit: {self.out / 'range.mp4'} ({b.name})")
            return
        self.uv(build, *args, "--publish", "cut", "--publish-to", self.out, "--preview")
        self.done("edit", sig, t0)

    def stage_subs(self) -> None:
        cut = self.out / "cut.mp4"
        if self.range or not cut.exists():
            log("subs: no cut yet")
            return
        sig = self.sig(self.code(PIPE / "edit" / "subs"), self.files(cut), self.clock_times())
        if self.up_to_date("subs", sig, [self.out / "cut.srt", self.out / "cut.vtt"]):
            log("subs: up to date")
            return
        t0 = time.time()
        self.uv(PIPE / "edit" / "subs" / "make_subs.py", "--cut", cut, "--voice", self.out / "voice" / "voice.json",
                "--clock", self.out / "edit" / "clock.json", "--out", self.out, "--name", "cut", "--no-burn")
        self.done("subs", sig, t0)


def _exit_on_term(signum, _frame) -> None:
    sys.exit(128 + signum)


def main() -> None:
    signal.signal(signal.SIGTERM, _exit_on_term)  # stopped: the running stage is stopped too, then a normal exit
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0],
                                 formatter_class=argparse.RawDescriptionHelpFormatter, epilog=__doc__)
    ap.add_argument("--res", default="3840x2160", help="3840x2160 (default) or 1920x1080")
    ap.add_argument("--voice", help="the narration's clips (the files film/voice.json names)")
    ap.add_argument("--out", required=True, help="the folder the film is rendered into")
    ap.add_argument("--hw", choices=["vaapi", "none"], default="vaapi")
    ap.add_argument("--stages", help=f"only these stages: {','.join(STAGES)}")
    ap.add_argument("--force", help="remake these stages even if they are up to date")
    ap.add_argument("--media", action="append", metavar="STAGE=PATH")
    ap.add_argument("--range", help="the edit renders only this stretch, A-B in film seconds, as OUT/range.mp4")
    ap.add_argument("--burn-in", action="store_true", help="a review build: timecode and shot ids burned in")
    ap.add_argument("--quaketool", help="use this quaketool binary instead of building quake-rs's")
    ap.add_argument("--jobs", type=int, help="the edit's segments rendered at once (build.py's default otherwise)")
    ap.add_argument("--mem-cap", default="12G", help="each stage's memory limit (systemd-run --user --scope, "
                    "MemoryMax, no swap): a runaway job is killed alone, not the system's services; 0 for none")
    a = ap.parse_args()
    m = Make(a)
    only = set(filter(None, (a.stages or "").split(",")))
    unknown = only - set(STAGES)
    if unknown:
        sys.exit(f"make: no stage {', '.join(sorted(unknown))}: {', '.join(STAGES)}")
    t0 = time.time()
    log(f"{m.w}x{m.h} (scale {m.scale}), hw {m.hw}, memory cap {m.cap or 'none'}, into {m.out}")
    # the clock runs first on the narration alone (the diagrams, the score and the effects are timed by it), then
    # again as the media land, since the timeline names them (a second each, skipped when nothing changed)
    plan = ["voice", "clock", "footage", "web", "diagrams", "clock", "score", "sfx", "clock", "edit", "subs"]
    for st in plan:
        if not only or st in only:
            getattr(m, f"stage_{st}")()
    log(f"done in {time.time() - t0:.0f} s")


if __name__ == "__main__":
    main()

#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = ["numpy", "pillow"]
# ///
"""The film's game footage at any size: every footage file the cut reads, from film/shots.

    uv run film/pipeline/footage/make_footage.py --res 3840x2160 --out OUT/footage
    uv run film/pipeline/footage/make_footage.py --res 1920x1080 --out OUT/footage --only S45 HERO7 S21m
    uv run film/pipeline/footage/make_footage.py --list

Writes OUT/game/NAME.mp4 with the files the edit reads beside it (NAME.json, NAME.wav,
NAME.events.json: the names the cut used), and OUT/proof/post-fix-ents/, the proof's frames.
The slop shots render natively at the target size; Classic shots keep their modes (320x200,
960x600 in the 4:3 box) and are scaled as the tool scales them, the box 1440x1080 times the
scale. The composites (wipes, halves, crops, the proof's S21m) are laid out at 1080p's
geometry times the scale. --res must be a whole multiple of 1920x1080.

Each file is made by its own process in a systemd user scope capped at --mem-max (its renders
and encoder with it). It is incremental: a file is made again only when a shot file it reads,
its sidecar, the quaketool binary, the size, the encoder or its own layout code changed
(`--force` makes it anyway). Video is HEVC on the GPU through VAAPI (`--hw vaapi`, the default where
/dev/dri/renderD128 encodes), or the production's H.264 in software (`--hw none`: libx264,
CRF 16, which at 1920x1080 gives v7's own files).

Needs cargo (it builds quaketool unless --quaketool names one), id's shareware pak at
quake-data/ID1/PAK0.PAK, ffmpeg, and docker for the proof's frames (id's C).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import traceback
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))

import filmroot  # noqa: E402
import kit as K  # noqa: E402
import proof as P  # noqa: E402
import recipes as R  # noqa: E402

PROOF, S21M = "proof", "S21m"


def parse_res(text: str) -> int:
    try:
        w, h = (int(v) for v in text.lower().split("x"))
    except ValueError:
        raise argparse.ArgumentTypeError(f"--res {text}: WxH")
    if w % 1920 or h % 1080 or w // 1920 != h // 1080 or w <= 0:
        raise argparse.ArgumentTypeError(f"--res {text}: a whole multiple of 1920x1080 (1920x1080, 3840x2160, ...)")
    return w // 1920


def vaapi_works() -> bool:
    if not os.path.exists(K.VAAPI_DEVICE) or not shutil.which("ffmpeg"):
        return False
    res = subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-vaapi_device", K.VAAPI_DEVICE,
                          "-f", "lavfi", "-i", "color=black:s=256x256:d=0.1", "-vf", "format=nv12,hwupload",
                          "-c:v", "hevc_vaapi", "-f", "null", "-"], capture_output=True)
    return res.returncode == 0


def build_quaketool() -> Path:
    subprocess.run(["cargo", "build", "--release", "--quiet", "--bin", "quaketool"], cwd=K.REPO / "quake-rs",
                   check=True)
    return filmroot.QUAKETOOL


# ---------------------------------------------------------------------------
# what is made, and whether it must be made again
# ---------------------------------------------------------------------------


def jobs_for(only: list[str] | None) -> list[str]:
    """The jobs to run, in order: the proof's frames first (S21m reads them), then the recipes."""
    every = [PROOF, *R.RECIPES, S21M]
    if not only:
        return every
    by_output = {}
    for r in R.RECIPES.values():
        for o in r.outputs:
            by_output.setdefault(o.rsplit(".", 1)[0], r.name)
    want = []
    for name in only:
        job = name if name in every else by_output.get(name)
        if job is None:
            sys.exit(f"make_footage.py: no footage named {name} (--list shows them)")
        want.append(job)
    if S21M in want:
        want.append(PROOF)
    return [j for j in every if j in want]


def stamp_inputs(job: str, s: int, qt_sha: str, hw: str, proof_dir: Path) -> dict:
    """Everything that decides a job's bytes."""
    if job == PROOF:
        return {"job": job, "quaketool": qt_sha, "views": P.views_json(),
                "oracle": {p.name: K.sha256(p) for p in P.compare_sources()}}
    if job == S21M:
        return {"job": job, "scale": s, "quaketool": qt_sha, "encoder": encoder_key(hw),
                "frames": {p.name: K.sha256(p) for p in P.inputs(proof_dir)},
                "sidecar": K.sha256(K.SIDECARS / "S21m.json"),
                "code": hashlib.sha256((_src(P.s21m)).encode()).hexdigest()}
    r = R.RECIPES[job]
    free = job in R.SIZE_FREE
    return {"job": job, "scale": None if free else s, "quaketool": qt_sha,
            "encoder": None if free else encoder_key(hw),
            "shots": {n: K.sha256(K.shot_path(n)) for n in r.shots},
            "sidecars": {n: K.sha256(K.SIDECARS / f"{n}.json") for n in r.sidecars},
            "code": hashlib.sha256(r.source().encode()).hexdigest()}


def encoder_key(hw: str) -> str:
    """The encoder's whole command but the frame size and the file: a change to any flag counts."""
    return " ".join(K.encoder_cmd(hw, 0, 0, Path("OUT")))


def _src(fn) -> str:
    import inspect
    return inspect.getsource(fn)


def outputs_of(job: str, out: Path) -> list[Path]:
    game = out / "game"
    proof_dir = out / "proof" / "post-fix-ents"
    if job == PROOF:
        return P.inputs(proof_dir)
    if job == S21M:
        return [game / "S21m.mp4", game / "S21m.json"]
    return [game / o for o in R.RECIPES[job].outputs]


def up_to_date(job: str, out: Path, key: str) -> bool:
    st = out / ".stamps" / f"{job}.json"
    if not st.exists() or not all(p.exists() for p in outputs_of(job, out)):
        return False
    try:
        return json.loads(st.read_text()).get("key") == key
    except json.JSONDecodeError:
        return False


# ---------------------------------------------------------------------------
# a job, in a worker process
# ---------------------------------------------------------------------------


def run_job(job: str, cfg: dict) -> str | None:
    """One job, in this process: None, or what went wrong."""
    out = Path(cfg["out"])
    work = Path(tempfile.mkdtemp(prefix=f"{job}-", dir=cfg["scratch"]))
    ctx = K.Ctx(s=cfg["scale"], quaketool=Path(cfg["quaketool"]), pak=Path(cfg["pak"]), hw=cfg["hw"],
                game=out / "game", work=work, texts=Path(cfg["scratch"]) / "text")
    try:
        if job == PROOF:
            P.frames(out / "proof" / "post-fix-ents", ctx.quaketool, work)
        elif job == S21M:
            P.s21m(ctx, out / "proof" / "post-fix-ents")
        else:
            R.RECIPES[job].make(ctx)
    except Exception:
        ctx.abandon()
        return traceback.format_exc()
    shutil.rmtree(work, ignore_errors=True)  # this job's own scratch: the renders' sound, events, reports
    return None


def memory_cap_works() -> bool:
    if not shutil.which("systemd-run"):
        return False
    res = subprocess.run(["systemd-run", "--user", "--scope", "-q", "-p", "MemoryMax=64M", "--", "true"],
                         capture_output=True)
    return res.returncode == 0


class Child:
    """A job in its own process, inside a systemd scope capped at `mem` (its renders and encoders
    with it), so a job that outgrows its share is stopped alone instead of the system running out."""

    def __init__(self, job: str, cfg: dict, mem: str | None, log: Path):
        cmd = [sys.executable, str(Path(__file__).resolve()), "--run-job", job, "--cfg", json.dumps(cfg)]
        if mem:
            cmd = ["systemd-run", "--user", "--scope", "-q", "-p", f"MemoryMax={mem}", "-p", "MemorySwapMax=0",
                   "--"] + cmd
        self.job, self.mem, self.log = job, mem, log
        self.t0 = time.monotonic()
        self.fh = open(log, "w")
        self.proc = subprocess.Popen(cmd, stdout=self.fh, stderr=subprocess.STDOUT)

    def result(self) -> tuple[float, str | None]:
        code = self.proc.wait()
        self.fh.close()
        seconds = time.monotonic() - self.t0
        if code == 0:
            return seconds, None
        text = self.log.read_text(errors="replace")[-4000:]
        if code < 0 or code in (137, 143):
            text += f"\n(stopped by signal; out of memory under its {self.mem} cap?)" if self.mem else ""
        return seconds, text or f"exit code {code}"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--res", type=parse_res, help="the film's size, a whole multiple of 1920x1080")
    ap.add_argument("--out", type=Path, help="the footage folder (OUT/footage): game/ and proof/ under it")
    ap.add_argument("--only", nargs="+", default=[], help="these footage files or recipes (names, or a,b,c)")
    ap.add_argument("--quaketool", type=Path, help="this quaketool instead of building quake-rs")
    ap.add_argument("--pak", type=Path, default=filmroot.PAK)
    ap.add_argument("--hw", choices=["auto", "vaapi", "none"], default="auto",
                    help="video encoder: vaapi (HEVC on the GPU), none (libx264); auto: vaapi where it works")
    ap.add_argument("--jobs", type=int, default=1, help="files made at once (each runs its renders side by side)")
    ap.add_argument("--mem-max", default="6G",
                    help="each job's memory cap (a systemd scope: MemoryMax, no swap); 'none' for no cap")
    ap.add_argument("--force", action="store_true", help="make them even if nothing changed")
    ap.add_argument("--list", action="store_true", help="list the footage files and what makes each")
    ap.add_argument("--run-job", help=argparse.SUPPRESS)  # internal: one job, in this process
    ap.add_argument("--cfg", help=argparse.SUPPRESS)
    a = ap.parse_args()

    if a.run_job:
        err = run_job(a.run_job, json.loads(a.cfg))
        if err:
            print(err, file=sys.stderr, flush=True)
            return 1
        return 0

    if a.list:
        print(f"{PROOF:<14} proof/post-fix-ents/: oracle/compare.py --sse, {', '.join(P.MAPS)} (docker)")
        for r in R.RECIPES.values():
            what = ", ".join(r.shots) if r.shots else r.note
            print(f"{r.name:<14} {' '.join(r.outputs)}\n{'':<14}   from {what}")
        print(f"{S21M:<14} S21m.mp4 S21m.json\n{'':<14}   from the proof's frames")
        return 0
    if a.res is None or a.out is None:
        ap.error("--res and --out are needed")
    only = [n for part in a.only for n in part.split(",") if n]
    jobs = jobs_for(only)

    if not a.pak.exists():
        sys.exit(f"make_footage.py: no pak at {a.pak} (README.md, \"Build and run it\", fetches it)")
    hw = a.hw
    if hw == "auto":
        hw = "vaapi" if vaapi_works() else "none"
    elif hw == "vaapi" and not vaapi_works():
        sys.exit(f"make_footage.py: hevc_vaapi does not encode on {K.VAAPI_DEVICE} (try --hw none)")
    mem = None if a.mem_max.lower() in ("none", "0", "") else a.mem_max
    if mem and not memory_cap_works():
        print("footage: no systemd user scope here: the jobs run without a memory cap", flush=True)
        mem = None
    qt = a.quaketool.resolve() if a.quaketool else build_quaketool()
    qt_sha = K.sha256(qt)
    out = a.out.resolve()
    for d in (out / "game", out / "proof" / "post-fix-ents", out / ".stamps"):
        d.mkdir(parents=True, exist_ok=True)
    scratch = filmroot.scratch("footage")
    cfg = {"out": str(out), "scale": a.res, "quaketool": str(qt), "pak": str(a.pak.resolve()), "hw": hw,
           "scratch": str(scratch)}
    logs = Path(tempfile.mkdtemp(prefix="logs-", dir=scratch))
    print(f"footage: {1920 * a.res}x{1080 * a.res}, {K.encoder_id(hw)}, {len(jobs)} job(s) into {out}"
          f"{f', each capped at {mem}' if mem else ''}", flush=True)

    failed: list[str] = []
    done = 0

    def finish(job: str, seconds: float, err: str | None, key: str, inputs: dict) -> None:
        nonlocal done
        done += 1
        if err:
            failed.append(job)
            for o in outputs_of(job, out):  # a job stopped mid-file leaves its part files
                o.with_name(o.stem + ".part.mp4").unlink(missing_ok=True)
            print(f"[{done}/{len(jobs)}] {job}: FAILED after {seconds:.0f} s\n{err}", flush=True)
            return
        K.write_json(out / ".stamps" / f"{job}.json", {"key": key, "inputs": inputs, "seconds": round(seconds, 1),
                                                      "made": time.strftime("%Y-%m-%d %H:%M:%S")})
        print(f"[{done}/{len(jobs)}] {job}: made in {seconds:.0f} s", flush=True)

    def plan(job: str) -> tuple[str, dict] | None:
        nonlocal done
        inputs = stamp_inputs(job, a.res, qt_sha, hw, out / "proof" / "post-fix-ents")
        key = hashlib.sha256(json.dumps(inputs, sort_keys=True).encode()).hexdigest()
        if not a.force and up_to_date(job, out, key):
            done += 1
            print(f"[{done}/{len(jobs)}] {job}: unchanged", flush=True)
            return None
        return key, inputs

    def run(job: str) -> Child:
        return Child(job, cfg, mem, logs / f"{job}.log")

    # The proof's frames first, alone (S21m reads them; compare.py builds id's C on first use).
    rest = [j for j in jobs if j != PROOF]
    if PROOF in jobs and (p := plan(PROOF)):
        finish(PROOF, *run(PROOF).result(), *p)
        if PROOF in failed and S21M in rest:  # it reads the proof's frames
            rest.remove(S21M)
            failed.append(S21M)
    queue = [(j, p) for j in rest if (p := plan(j))]
    running: dict[str, tuple[Child, tuple]] = {}
    retried: set[str] = set()
    while queue or running:
        while queue and len(running) < max(1, a.jobs):
            job, p = queue.pop(0)
            running[job] = (run(job), p)
        for job, (child, p) in list(running.items()):
            if child.proc.poll() is not None:
                del running[job]
                seconds, err = child.result()
                if err and K.DECODE_FAILED in err and job not in retried:  # an encode that broke: once more
                    retried.add(job)
                    print(f"  {job}: its video {K.DECODE_FAILED}; making it again", flush=True)
                    queue.insert(0, (job, p))
                    continue
                finish(job, seconds, err, *p)
        if running:
            time.sleep(0.5)
    if failed:
        print(f"footage: FAILED: {' '.join(failed)}", flush=True)
        return 1
    print("footage: done", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())

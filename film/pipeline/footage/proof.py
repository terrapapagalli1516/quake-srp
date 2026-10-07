"""The proof's frames: id's C beside the port on four maps with monsters in view, and S21m, the
film's layout of them.

`oracle/compare.py --modes world,ents --aspect 0.8333333 --spans 16 --sse`, one view a map
(film/shots/INDEX.md, "S20, S21"): once the player is in the game, id's console moves him to
the camera, so the monsters near it wake, turn and attack, and the frame is `--settle` frames
after signon, at id's own clock; the port draws id's entity list, particles and dynamic lights
of that frame. It needs docker (id's C, built on first use) and gives the same 320x200 frames
at any film size. S21m lays them out at the film's size, enlarged with nearest neighbour; the
edit's BD1 and BD3 cards read the same frames (FILM_ROOT/footage/proof/post-fix-ents/).
"""

from __future__ import annotations

import json
import shutil
import subprocess
import tempfile
from pathlib import Path

import numpy as np
from PIL import Image

import kit as K
from kit import FPS, HEAD, RUST, Ctx, frames_of, smooth

# map: (camera x,y,z,pitch,yaw,roll; the player's origin under it; frames after signon)
VIEWS = {
    "e1m1": ((596.7, 2808, -34, 4, 180, 0), (596.7, 2808, -56), 15),
    "e1m2": ((1543, 1385, 226, 4, 270, 0), (1543, 1385, 204), 19),
    "e1m3": ((1283.8, 912.2, 582, 4, 315, 0), (1283.8, 912.2, 560), 11),
    "e1m5": ((160.7, 2549.3, 446, 4, 0, 0), (160.7, 2549.3, 424), 15),
}
MAPS = list(VIEWS)
CASE = "{m}_ents_320x200"
KEEP = (".c.ppm", ".c.pgm", ".c.json", ".c.ents", ".c.parts", ".port.ppm", ".side.png")


def compare_cmd(m: str, quaketool: Path, out: Path) -> list[str]:
    view, player, settle = VIEWS[m]
    cmd = ["uv", "run", "oracle/compare.py", "--maps", m, "--modes", "world,ents", "--aspect", "0.8333333",
           "--spans", "16", "--sse", "--quaketool", str(quaketool), "--view=" + ",".join(f"{v:g}" for v in view),
           "--settle", str(settle)]
    for c in ["wait"] * 6 + ["noclip", "god", "oracle_field origin " + " ".join(f"{v:g}" for v in player)]:
        cmd += ["--c-post", c]
    return cmd + ["--out", str(out)]


def frames(proof_dir: Path, quaketool: Path, work: Path, log=print) -> dict:
    """Run compare.py for the four views into a scratch folder, then keep each case's files in
    `proof_dir`. Returns each map's count of differing pixels (every one has been 0)."""
    if not shutil.which("docker"):
        raise RuntimeError("the proof's frames need docker (oracle/README.md): id's C runs in it")
    run = Path(tempfile.mkdtemp(prefix="proof-", dir=work))
    proof_dir.mkdir(parents=True, exist_ok=True)
    tables, diffs = [], {}
    for m in MAPS:
        d = run / m
        cmd = compare_cmd(m, quaketool, d)
        res = subprocess.run(cmd, cwd=K.REPO, capture_output=True, text=True, timeout=1800)
        if res.returncode != 0:
            raise RuntimeError(f"compare.py failed for {m}:\n{res.stdout[-2000:]}{res.stderr[-2000:]}")
        tables.append(f"$ {' '.join(repr(a) if ' ' in a else a for a in cmd[:-2])} --out DIR\n{res.stdout}")
        case = CASE.format(m=m)
        for ext in KEEP:
            shutil.copyfile(d / f"{case}{ext}", proof_dir / f"{case}{ext}.part")
            (proof_dir / f"{case}{ext}.part").replace(proof_dir / f"{case}{ext}")
        c = np.asarray(Image.open(proof_dir / f"{case}.c.ppm").convert("RGB")).astype(int)
        p = np.asarray(Image.open(proof_dir / f"{case}.port.ppm").convert("RGB")).astype(int)
        dmax = np.abs(c - p).max(axis=2)
        Image.fromarray(np.clip(dmax * 4, 0, 255).astype(np.uint8)).save(proof_dir / f"{case}.diff.png")
        diffs[m] = int((dmax > 0).sum())
        log(f"  proof {m}: {diffs[m]} of {dmax.size} pixels differ")
    (proof_dir / "compare.txt").write_text("\n".join(tables))
    K.write_json(proof_dir / "diffs.json", {"differing_pixels": diffs})
    return diffs


def s21m(c: Ctx, proof_dir: Path) -> None:
    """S21's layout and timing on the four frames: id's C and the port side by side, enlarged
    (nearest) to 880x660 at 1080p, their names above and the map's below, 1.95 s a map from shot
    second 0; 0.5 s after each map appears its difference (the largest channel's |dRGB| x4, in a
    rust frame) fades in over 0.4 s. A second of handle each end; the head is e1m1 with no diff."""
    s = c.s
    per = 1.95
    pw, ph = 880 * s, 660 * s
    lx, rx, py = 60 * s, 980 * s, 84 * s
    dw, dh = 360 * s, 270 * s
    dx, dy = (c.W - dw) // 2, py + ph + 40 * s
    b = 2 * s  # the rust frame
    imgs = {}
    for m in MAPS:
        case = CASE.format(m=m)
        cc = np.asarray(Image.open(proof_dir / f"{case}.c.ppm").convert("RGB"))
        pp = np.asarray(Image.open(proof_dir / f"{case}.port.ppm").convert("RGB"))
        diff = np.minimum(255, np.abs(cc.astype(int) - pp.astype(int)).max(axis=2) * 4).astype(np.uint8)
        imgs[m] = (K.nearest(cc, pw, ph), K.nearest(pp, pw, ph),
                   K.nearest(np.repeat(diff[..., None], 3, axis=2), dw, dh))
    lab_c, lab_p, lab_d = c.text("id's C"), c.text("the port"), c.text("difference", 2)
    names = {m: c.text(m, 2) for m in MAPS}
    wr = c.writer("S21m")
    for k in range(frames_of(HEAD + 7.8 + 1.0)):
        sh = k / FPS - HEAD
        i = min(len(MAPS) - 1, max(0, int(sh // per)))
        m = MAPS[i]
        cimg, pimg, dimg = imgs[m]
        out = c.canvas()
        K.place(out, cimg, lx, py)
        K.place(out, pimg, rx, py)
        K.paste(out, lab_c, lx, py - 36 * s)
        K.paste(out, lab_p, rx, py - 36 * s)
        K.paste(out, names[m], lx, py + ph + 16 * s)
        since = sh - i * per if sh >= 0 else -1
        u = smooth((since - 0.5) / 0.4) if sh >= 0 else 0.0
        if u > 0:
            panel = np.zeros((dh + 2 * b, dw + 2 * b, 3), np.uint8)
            panel[:] = RUST
            panel[b:-b, b:-b] = dimg
            region = out[dy - b:dy + dh + b, dx - b:dx + dw + b]
            out[dy - b:dy + dh + b, dx - b:dx + dw + b] = K.blend(region, panel, u)
            K.paste(out, lab_d, dx + dw + 20 * s, dy + dh - 16 * s, u)
        wr.write(out)
    K.sidecar(c, "S21m", wr.close())


def inputs(proof_dir: Path) -> list[Path]:
    return [proof_dir / f"{CASE.format(m=m)}{ext}" for m in MAPS for ext in (".c.ppm", ".port.ppm")]


def compare_sources() -> list[Path]:
    """The oracle's code, whose change means new frames: the harness, its build and id's patched C."""
    o = K.REPO / "oracle"
    return ([o / "compare.py", o / "oraclebin.py", o / "build.sh"]
            + sorted(p for p in (o / "c").rglob("*") if p.is_file()))


def views_json() -> str:
    return json.dumps(VIEWS, sort_keys=True)

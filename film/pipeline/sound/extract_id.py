#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Extract every sound/*.wav from id's shareware PAK0.PAK into FILM_ROOT/sound/id/, names kept.

    uv run film/pipeline/sound/extract_id.py [--pak PATH]

The pak is the repository's quake-data/ID1/PAK0.PAK unless --pak names another. The bytes are
copied as they are in the pak (id's own RIFF files, mostly 8-bit mono 11,025 Hz), so the
originals keep their rates. Also writes id/index.json: for each file its format (rate, bits,
channels, frames, seconds) and its loop start where id's 'cue ' chunk marks one (the ambient
loops); and id/LISTING.md, by category, with each sound's loudest 400 ms (loudness_id.py)."""
import argparse
import json
import struct
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent))
from filmroot import FILM, PAK  # noqa: E402
from id_catalog import WHAT, category  # noqa: E402

OUT = FILM / "sound" / "id"


def parse_wav(b: bytes) -> dict:
    assert b[:4] == b"RIFF" and b[8:12] == b"WAVE", "not a RIFF WAVE"
    i, info = 12, {}
    while i + 8 <= len(b):
        cid, n = b[i:i + 4], struct.unpack_from("<I", b, i + 4)[0]
        body = b[i + 8:i + 8 + n]
        if cid == b"fmt ":
            fmt, ch, rate, _, _, bits = struct.unpack_from("<HHIIHH", body)
            info.update(format=fmt, channels=ch, rate=rate, bits=bits)
        elif cid == b"data":
            info["data_bytes"] = len(body)
        elif cid == b"cue " and len(body) >= 28:
            # first cue point: dwSampleOffset is the last field of the 24-byte record (id reads it as the loop start)
            info["loop_start"] = struct.unpack_from("<I", body, 4 + 20)[0]
        elif cid == b"LIST" and body[:4] == b"adtl":
            # id's ambient loops carry a 'ltxt' with the loop length after the cue
            j = 4
            while j + 8 <= len(body):
                sid, sn = body[j:j + 4], struct.unpack_from("<I", body, j + 4)[0]
                if sid == b"ltxt":
                    info["loop_len"] = struct.unpack_from("<I", body, j + 12)[0]
                j += 8 + sn + (sn & 1)
        i += 8 + n + (n & 1)
    fr = info["data_bytes"] // (info["channels"] * info["bits"] // 8)
    info["frames"] = fr
    info["seconds"] = round(fr / info["rate"], 4)
    return info


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--pak", type=Path, default=PAK, help="id's shareware PAK0.PAK (default: the repository's)")
    a = ap.parse_args()
    if not a.pak.exists():
        sys.exit(f"no pak at {a.pak}: id's shareware PAK0.PAK is not in the repository; link quake-data/ or pass --pak")
    data = a.pak.read_bytes()
    assert data[:4] == b"PACK"
    dirofs, dirlen = struct.unpack_from("<ii", data, 4)
    OUT.mkdir(parents=True, exist_ok=True)
    index = []
    for k in range(dirlen // 64):
        name, pos, n = struct.unpack_from("<56sii", data, dirofs + 64 * k)
        name = name.split(b"\0")[0].decode()
        if not (name.startswith("sound/") and name.endswith(".wav")):
            continue
        rel = name[len("sound/"):]
        dst = OUT / rel
        dst.parent.mkdir(parents=True, exist_ok=True)
        b = data[pos:pos + n]
        dst.write_bytes(b)
        key = rel.removesuffix(".wav")
        index.append({"name": rel, "category": category(key), "what": WHAT.get(key, "(unknown)"),
                      "bytes": n} | parse_wav(b))
    index.sort(key=lambda e: e["name"])
    (OUT / "index.json").write_text(json.dumps(index, indent=1) + "\n")
    write_listing(index)
    print(f"{len(index)} sounds -> {OUT}", file=sys.stderr)


def write_listing(index: list[dict]) -> None:
    """id/LISTING.md: by category, each sound's length, rate, loop and loudest 400 ms."""
    import subprocess
    lufs = {}
    try:  # momentary loudness from the renderer's own loader (48 kHz), if its dependencies are at hand
        out = subprocess.run(["uv", "run", str(Path(__file__).resolve().parent / "loudness_id.py")],
                             capture_output=True, text=True, check=True).stdout
        lufs = json.loads(out)
    except Exception as e:  # noqa: BLE001
        print(f"(no loudness column: {e})", file=sys.stderr)
    cats: dict[str, list[dict]] = {}
    for e in index:
        cats.setdefault(e["category"], []).append(e)
    order = ["weapons", "items", "player", "menu", "misc", "ambience", "world: doors", "world: platforms",
             "world: buttons"] + sorted(c for c in cats if c.startswith("monster"))
    L = ["# id's sounds (shareware PAK0.PAK)", "",
         f"{len(index)} sounds, extracted unchanged into `sound/id/` (names kept). Use one in a cue as "
         "`weapons/rocket1i` or `id:weapons/rocket1i`. Rates are id's (the renderer resamples to 48 kHz). "
         "**M** = the loudest 400 ms at 48 kHz, in LUFS (id's sounds are hot: median about -9; a cue's gain is relative "
         "to this). **loop** = id's loop start (the sound repeats from there while the entity lives). "
         "'(by name)' = the use is a guess from the name.", ""]
    for c in order:
        if c not in cats:
            continue
        L += [f"## {c}", "", "| sound | s | rate | loop | M | what |", "|---|---|---|---|---|---|"]
        for e in cats[c]:
            nm = e["name"].removesuffix(".wav")
            loop = f"{e['loop_start'] / e['rate']:.2f} s" if "loop_start" in e else ""
            m = f"{lufs[e['name']]:.0f}" if e["name"] in lufs else ""
            L.append(f"| `{nm}` | {e['seconds']:.2f} | {e['rate'] // 1000 if e['rate'] % 1000 == 0 else e['rate']} "
                     f"| {loop} | {m} | {e['what']} |")
        L.append("")
    (OUT / "LISTING.md").write_text("\n".join(L))


if __name__ == "__main__":
    main()

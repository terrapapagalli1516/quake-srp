"""id's C, built and current: where every tool gets `oracle/build/quake-oracle`
(the x87 build, as 1996's compilers made it) or `oracle/build/quake-oracle-sse`
(the same C with SSE2 floats: strict single precision, the port's target).

A binary is built when it is missing and rebuilt when `build.sh` or a file
of the harness (`oracle/c/`) is newer than it, so that no tool compares the
port against an older harness than the one in the tree: a trace the harness
has only just learnt to write would otherwise read as "id's wrote nothing".
(No dependencies, so the scripts that have none can import it.)
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ORACLE_BIN = HERE / "build" / "quake-oracle"
ORACLE_SSE_BIN = HERE / "build" / "quake-oracle-sse"


def ensure_oracle(explicit: str | Path | None = None, sse: bool = False, fpmath: str | None = None) -> Path:
    """The oracle binary: `explicit` as given, or the tree's own, rebuilt if
    stale: the x87 build, the SSE build with `sse`, or `fpmath`'s
    (`oracle/build.sh`'s ORACLE_FPMATH: x87, sse, x87store)."""
    if explicit:
        return Path(explicit).resolve()
    fpmath = fpmath or ("sse" if sse else "x87")
    binary = ORACLE_BIN if fpmath == "x87" else HERE / "build" / f"quake-oracle-{fpmath}"
    sources = [*(HERE / "c").iterdir(), HERE / "build.sh"]
    if not binary.exists() or any(p.stat().st_mtime > binary.stat().st_mtime for p in sources):
        print(f"building the C oracle (ORACLE_FPMATH={fpmath} oracle/build.sh) ...", file=sys.stderr)
        # build.sh's own lines go to stderr: a tool's stdout is its report
        env = {**os.environ, "ORACLE_FPMATH": fpmath}
        subprocess.run([str(HERE / "build.sh")], check=True, stdout=sys.stderr, env=env)
    return binary

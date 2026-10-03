"""id's C, built and current: where every tool gets `oracle/build/quake-oracle`.

The binary is built when it is missing and rebuilt when `build.sh` or a file
of the harness (`oracle/c/`) is newer than it, so that no tool compares the
port against an older harness than the one in the tree: a trace the harness
has only just learnt to write would otherwise read as "id's wrote nothing".
(No dependencies, so the scripts that have none can import it.)
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ORACLE_BIN = HERE / "build" / "quake-oracle"


def ensure_oracle(explicit: str | Path | None = None) -> Path:
    """The oracle binary: `explicit` as given, or the tree's own, rebuilt if stale."""
    if explicit:
        return Path(explicit).resolve()
    sources = [*(HERE / "c").iterdir(), HERE / "build.sh"]
    if not ORACLE_BIN.exists() or any(p.stat().st_mtime > ORACLE_BIN.stat().st_mtime for p in sources):
        print("building the C oracle (oracle/build.sh) ...", file=sys.stderr)
        # build.sh's own lines go to stderr: a tool's stdout is its report
        subprocess.run([str(HERE / "build.sh")], check=True, stdout=sys.stderr)
    return ORACLE_BIN

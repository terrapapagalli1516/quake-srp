"""Scratch space for the oracle's scripts: on disk, and cleaned up when a run is stopped.

/tmp is often a tmpfs, held in RAM, and the oracle's frames, builds and sweeps are large. A run that is
stopped by a signal used to leave its temporary directory behind, so a few interrupted sweeps could hold
gigabytes of RAM until the next reboot. So scratch goes to $QUAKE_SCRATCH if it is set, else /var/tmp (on
disk on the usual Linux setups), else the system's temp dir. Importing this module also turns SIGTERM (a
`timeout`, a lane's time limit) into a normal exit, so `with tempdir(...)` blocks still remove their
directory. Directories made with `mkdtemp` are outputs that the script keeps on purpose; it prints where.
"""

from __future__ import annotations

import os
import signal
import sys
import tempfile
from pathlib import Path


def base() -> str:
    """The directory scratch is made in."""
    env = os.environ.get("QUAKE_SCRATCH")
    if env:
        return env
    return "/var/tmp" if os.path.isdir("/var/tmp") else tempfile.gettempdir()


def tempdir(prefix: str) -> tempfile.TemporaryDirectory:
    """A temporary directory under base(), removed when its `with` block ends."""
    return tempfile.TemporaryDirectory(prefix=prefix, dir=base())


def mkdtemp(prefix: str) -> Path:
    """A directory under base() that the caller keeps (an output)."""
    return Path(tempfile.mkdtemp(prefix=prefix, dir=base()))


def _exit_on_term(signum, _frame) -> None:
    sys.exit(128 + signum)


if signal.getsignal(signal.SIGTERM) in (signal.SIG_DFL, None):
    signal.signal(signal.SIGTERM, _exit_on_term)

"""Where the film's pipeline reads and writes: one root, one scratch folder.

Every script under film/pipeline/ imports this module (it puts film/pipeline/ on sys.path
first), so the whole pipeline moves with two environment variables:

- FILM_ROOT: the film's tree. Every relative path a script or a config names
  (`footage/game/S01.mp4`, `voice/v7-lines.json`, `diagrams/v7/ladder_ST0.mov`,
  `music/score-v10.wav`, `edit/v7/…`) is taken from here. Default: `film/`, the folder
  that holds film/pipeline/. The media folders under it are not in the repository
  (film/.gitignore); the renders write them.
- FILM_SCRATCH: caches and intermediate files, which can be large (the edit's lossless
  segments run to gigabytes). Default: `quake-srp-film/` in the system's temp folder.

The repository's own paths (id's pak, the quaketool binary) are found from this file.
"""

from __future__ import annotations

import os
import tempfile
from pathlib import Path

PIPELINE = Path(__file__).resolve().parent
REPO = PIPELINE.parents[1]
FILM = Path(os.environ.get("FILM_ROOT") or PIPELINE.parent).resolve()
SCRATCH = Path(os.environ.get("FILM_SCRATCH") or Path(tempfile.gettempdir()) / "quake-srp-film").resolve()
PAK = REPO / "quake-data" / "ID1" / "PAK0.PAK"
QUAKETOOL = REPO / "quake-rs" / "target" / "release" / "quaketool"


def scratch(part: str) -> Path:
    """A part's own folder under FILM_SCRATCH, made on first use."""
    d = SCRATCH / part
    d.mkdir(parents=True, exist_ok=True)
    return d

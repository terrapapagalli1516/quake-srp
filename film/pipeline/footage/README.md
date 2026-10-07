# film/pipeline/footage: the game footage at any size

The footage stage of the film's one command. It makes every game footage file the cut reads,
from the shot files in [`film/shots/`](../../shots/), at the film's size: the single renders,
the composites the production laid together (wipes, halves, crops that follow a monster,
dissolves, the 480 Hz strip), the sound and the event logs beside them, and the proof's frames
from id's C with the film's layout of them, S21m.

```sh
uv run film/pipeline/footage/make_footage.py --res 3840x2160 --out OUT/footage
uv run film/pipeline/footage/make_footage.py --res 1920x1080 --out OUT/footage --only S45 HERO7 S21m
uv run film/pipeline/footage/make_footage.py --list          # every file, and the shot files that make it
```

| flag | |
|---|---|
| `--res WxH` | the film's size: 1920x1080 or a whole multiple of it (3840x2160 is scale 2) |
| `--out DIR` | the footage folder; the edit reads it as `FILM_ROOT/footage` |
| `--only NAME ...` | only these files (`S45`, `N2e.clean`, `S39`, `proof`, `S21m`, or `a,b,c`) |
| `--quaketool BIN` | use this binary; by default it builds `quake-rs`'s and uses that |
| `--hw vaapi\|none\|auto` | the video encoder (below); `auto`, the default, is VAAPI where it encodes |
| `--jobs N` | files made at once (default 1); each already runs its renders side by side |
| `--mem-max SIZE` | each job's memory cap (default 6G; `none` for none) |
| `--force` | make the files even if nothing they come from changed |

It writes `DIR/game/NAME.mp4` and, where the cut's file had them, `NAME.json` (the sidecar:
handles, sound, the times of what happens, from [`film/shots/sidecars/`](../../shots/sidecars/),
plus the size, tool and encoder of this render), `NAME.wav` (the engine's own mixer, sample 0 at
frame 0) and `NAME.events.json` (the tool's log on the mp4's clock; `NAME.events-a.json` for the
other side of a comparison). The names are the cut's: `N2e.clean.mp4` beside `N2e.json`,
`S62.clean.mp4`, `N3w-box.zoom.mp4`, `ST3.g065.mp4`, and the two sound files of LAB9's click,
`N9-click-id.wav` and `N9-click-slop.wav`. `DIR/proof/post-fix-ents/` holds the proof's frames,
which S21m and the edit's BD1 and BD3 cards read.

**What it needs:** cargo (to build `quaketool`), id's shareware pak at `quake-data/ID1/PAK0.PAK`,
ffmpeg with libx264 (and hevc_vaapi for the GPU path), `uv`, and docker for the proof's frames
(`oracle/compare.py` builds id's C in it on first use). Caches and each run's scratch go under
`FILM_SCRATCH/footage/` (see [`filmroot.py`](../filmroot.py)).

## The pixels at a larger size

- **Slop shots** render natively at the film's size: each shot file's own `size` times the scale,
  given to `quaketool film` last, as an override.
- **Classic shots** keep their modes, 320x200 or 960x600 drawn into the 4:3 box: the tool scales
  them into the box as it always does, and the box is 1440x1080 times the scale.
- **The composites** are the production's layouts with every position, crop, band, line and
  label in 1080p pixels times the scale (rounded at 1080p first, so the geometry is exactly 2x at
  3840x2160). A 2x crop of a 4K render is the same part of the picture as at 1080p, its pixels
  4K's. The lettering is Quake's conchars from `quaketool filmtext` at the scale times its own;
  the gamepad and keycaps are drawn with their line widths times the scale.
- **The proof's frames** are id's 320x200, the same at any size; S21m enlarges them with nearest
  neighbour into its panels.
- A pass that only measures (S46a's track of the grunt) runs at its own size whatever the
  film's, so a crop follows the same path at every size.

## Encoding

- **`--hw vaapi`:** HEVC on the GPU (`hevc_vaapi` on `/dev/dri/renderD128`) at a low constant
  QP, 4:2:0 in BT.709. On Classic's hard-edged pixels it stays close to a lossless 4:2:0 encode.
- **`--hw none`:** the production's H.264 in software: libx264, CRF 16, preset medium. At
  1920x1080 with the tools that made the cut, every composite and every file checked comes out
  the same bytes as the cut's own footage, sound and event logs included: the layouts here are
  the production's, operation for operation.

Every VAAPI file is decoded in full by ffmpeg's software decoder before it is kept; one that
does not decode cleanly is made again once, then the job fails. (The GPU's decoder does not
report a broken HEVC stream. The files keep HEVC's default `hev1` tag: with `hvc1` an mp4 keeps
only the encoder's global header, whose initial QP is not the one its slices were coded
against.)

Both convert the frames to 4:2:0 the same way (ffmpeg's scaler, BT.709, tv range) before the
encoder; only the codec differs.

## Made again only when something changed

Each file's stamp (`DIR/.stamps/NAME.json`) records what decides its bytes: the shot files it
reads, its sidecar, the `quaketool` binary, the film's scale, the encoder, and its own layout
code (the recipe and the helpers it names). A file whose stamp matches, with all its outputs on
disk, is skipped; a change to the shared kit (`kit.py`) is not tracked, so make them again with
`--force` after one. The proof's frames are stamped by the binary, the views and the oracle's
code, not by the film's size. Files are written under a `.part` name and renamed when complete.

## Jobs and memory

Each file is made by its own process, which runs its `quaketool film` renders side by side and
pipes the frames through the layout into ffmpeg. That process, its renders and its encoder run
in a systemd user scope capped at `--mem-max` with no swap, so a job that outgrows its share is
stopped alone (and reported) instead of the machine running out; where there is no systemd user
session the jobs run uncapped. The layout is one Python thread a job, so at 4K `--jobs 2` or more
finishes sooner when the machine has the memory for that many caps.

## The files

| | |
|---|---|
| [`make_footage.py`](make_footage.py) | the command: which files, whether they changed, the jobs |
| [`recipes.py`](recipes.py) | every footage file the cut reads and how it is made: `PLAIN`, one render straight to the file, then each composite |
| [`proof.py`](proof.py) | the proof: `oracle/compare.py --sse` on the four views, then S21m |
| [`kit.py`](kit.py) | `quaketool film` streams, the encoders, event logs and sidecars, and the drawing |
| [`check.py`](check.py) | `diff`: each file against a reference folder, frame by frame, with its sound and events; `sheet`: contact sheets and a 1:1 crop |

## Against the cut's own footage

`check.py diff REF/game NEW/game NAMES...` decodes both files and counts the pixels that differ by
more than an encoder's noise. With today's `quaketool`, a file differs from the cut's where the
tool's pictures changed after the cut was made, not where the layouts are laid:

- shots seen through the player's eye (`camera walk`, `camera player`, a demo's camera) draw id's
  eye nudged 1/32 unit (id's V_CalcRefdef), so their edges shift by a fraction of a pixel;
- the film's own cameras differ in single pixels at texel edges and on liquids; on one floor in
  e1m1's start room a light style's step lands a frame from where it did (S10b, and S62's Classic
  end);
- S13's `segments` x-ray has changed since the first film tool rendered the cut's S13.

The game sound, the sounds in the event logs and the proof's frames are unchanged.

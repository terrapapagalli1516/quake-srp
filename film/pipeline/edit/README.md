# film/pipeline/edit: the clock and the cut

The edit's two steps, and the cards it draws. `film/make.py` runs them as its clock and edit
stages (and the subtitles after them); each also runs alone, from the repository's root.

```sh
uv run film/pipeline/edit/timeline.py --dry          # the clock, printed
uv run film/pipeline/edit/timeline.py                # FILM_ROOT/edit/timeline.json, clock.json, clock.txt,
                                                     # ladder-events.json
uv run film/pipeline/edit/s26_lines.py [--scale 2]   # S26's check lines, in Quake's lettering (edit/v5/art/)
uv run film/pipeline/edit/build.py --publish cut --no-burn-in --preview
uv run film/pipeline/edit/build.py --scale 2 --hw vaapi --root OUT --publish cut --publish-to OUT --no-burn-in --preview
uv run film/pipeline/edit/build.py --range 113.5-116.5   # one stretch, as edit/build-NNN/range.mp4
uv run film/pipeline/edit/subs/make_subs.py --cut OUT/cut.mp4 --voice voice/voice.json --clock edit/clock.json
```

| file | what it does |
|---|---|
| [`timeline.py`](timeline.py) | the clock: `film/edit.toml` laid over the narration (`voice/voice.json`, each line's clip and word times) and the media on disk; writes the edit decision list and the named moments the score, the effects and the ladder are cued on |
| [`build.py`](build.py) | the cut: the art, one segment per shot, the mix, the encode, the 1080p copy, the phone files, the reports |
| [`cards.py`](cards.py), [`qglyph.py`](qglyph.py) | the edit's own pictures (title, terminal, labels, captions, the proof's panels, the end card), drawn with the diagrams' kit, `qkit` |
| [`s26_lines.py`](s26_lines.py) | S26's lines, drawn by `quaketool filmtext` |
| [`subs/make_subs.py`](subs/make_subs.py) | SRT, VTT and ASS subtitles on the clock |

**Scale.** `build.py --scale N` renders the film at N times 1920x1080. `edit.toml`'s
positions and sizes stay in 1080's units and are multiplied by N; the cards are drawn by cairo
with an N x transform (lines and Inter at full resolution, id's 8x8 glyphs nearest-neighbour,
exactly N x); the 4:3 box is N x 1440x1080 with N x 240 px bars; an overlay movie made at
1080 is scaled up nearest-neighbour, one made at the film's size is laid as it is. The clock
is the same at any scale. With N above 1 the build also writes a 1080p copy, area-averaged
from the same segments, and makes the phone files from it.

**Encoding.** `--hw vaapi` encodes on the GPU through VAAPI (`VAAPI_DEVICE`, default
`/dev/dri/renderD128`): the segments in HEVC at a constant QP (`SEG_QP`, high quality but not
lossless, so a 4K film's segments fit a disk), the master in HEVC (`MASTER_QP`), the 1080p
copy in H.264. `--hw none` (the default) encodes in software: lossless H.264 segments at
1080 (above it, CRF 10) and an x264 master. The phone files (`--preview`: 480p, 30 fps,
under 10 MB, HEVC and an H.264 fallback) are always software x265 and x264, two-pass, since
at that size quality per bit decides.

**Caches.** The art and the segments are cached by content under `FILM_SCRATCH/edit/cache/`
(the scale and the codec are part of a segment's key), so a build after a small change
re-renders only the shots it touched. Each build is a fresh `edit/build-NNN/` with its
timeline, mix report, stems, loudness report, contact sheet and `inputs/` (a copy of every
file it read, reflinks where the disk has them, with `MD5SUMS`).

**Needs.** ffmpeg and ffprobe (libx264, libx265, prores decoding; VAAPI for `--hw vaapi`),
cairo, the font Inter, a monospace font for the review builds' timecode, and `quaketool`
(built from `quake-rs/`) with id's shareware pak for S26's lines.

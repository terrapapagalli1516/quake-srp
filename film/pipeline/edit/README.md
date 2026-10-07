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
is the same at any scale. With N above 1 the build also writes a 1080p copy, scaled down
from the same segments, and makes the phone files from it.

**Encoding.** `--hw vaapi` uses the GPU through VAAPI (`VAAPI_DEVICE`, default
`/dev/dri/renderD128`): each segment decodes its H.264 or HEVC footage on the GPU, composites
on the CPU and encodes HEVC at a constant QP (`SEG_QP`, high quality but not lossless, so a 4K
film's segments fit a disk). The master is the segments stream-copied (they are already the
master's codec: no decode, no second generation; a review build's burned-in timecode is the
exception, encoded again at `MASTER_QP`). The 1080p copy is decoded, scaled (`scale_vaapi`'s
high-quality mode) and encoded in H.264 on the GPU. hevc_vaapi drops the BT.709 tags from its
stream, so they are written back (`hevc_metadata`, `h264_metadata`). Every VAAPI file is
checked by [`../vaapi.py`](../vaapi.py) before it is cached or published: HEVC tagged `hev1`
(never `hvc1`, which decodes hevc_vaapi's slices against the wrong PPS) and a clean software
decode, with one re-encode on a failure. `--hw none` (the default) encodes in software:
lossless H.264 segments at 1080 (above it, CRF 10) and an x264 master. The phone files
(`--preview`: 480p, 30 fps, under 10 MB, HEVC tagged `hvc1` for iPhones and an H.264 fallback)
are always software x265 and x264, two-pass, since at that size quality per bit decides.

**Overlays.** A still overlay (a label, S26's lines) or a sequence (a caption, the bumper) is
drawn full-frame, then cut to the area where it is not transparent and laid there: ffmpeg
decodes a looped PNG on every frame and queues the frames, and a full-frame one costs
gigabytes at 4K. The picture is the same. `--jobs` (segments at once) defaults to 4 at 1080 and
2 above it.

**Caches.** The art and the segments are cached by content under `FILM_SCRATCH/edit/cache/`
(the scale and the codec are part of a segment's key), so a build after a small change
re-renders only the shots it touched. Each build is a fresh `edit/build-NNN/` with its
timeline, mix report, stems, loudness report, contact sheet and `inputs/`: `MD5SUMS` of
every file it read, and reflinked copies of them where the disk has reflinks (whole copies of a
4K film's inputs would be tens of gigabytes a build).

**Needs.** ffmpeg and ffprobe (libx264, libx265, prores decoding; VAAPI for `--hw vaapi`),
cairo, the font Inter, a monospace font for the review builds' timecode, and `quaketool`
(built from `quake-rs/`) with id's shareware pak for S26's lines.

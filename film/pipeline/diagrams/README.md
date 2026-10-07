# film/pipeline/diagrams: the film's diagrams and its ladder

```sh
uv run film/pipeline/diagrams/render_all.py --scale 2 --clock OUT/edit --out OUT/diagrams   # 3840x2160
uv run film/pipeline/diagrams/render_all.py --scale 1 --clock OUT/edit --out OUT/diagrams   # 1920x1080: v7's
uv run film/pipeline/diagrams/render_all.py ... --only D13 ladder                          # some, by name
uv run film/pipeline/diagrams/render_all.py ... --list                                     # the commands
uv run film/pipeline/diagrams/render_v7.py                  # the same at scale 1, into FILM_ROOT
```

`render_all.py` renders every diagram the cut uses, and the slop-options ladder from the
clock's `ladder-events.json` (`--clock`: the folder the clock stage writes), under the names
the edit reads: `OUT/diagrams/v7/ladder_ST0.mov` (one per span of the events),
`v7/D03-oracle-band_alpha.mov`, `v6/D11-jump_alpha.mov`, `final/D01-fixed-point.mp4`, and so on
(`JOBS` in the script). Each single diagram gets a contact sheet beside it. An output whose
`.key` matches what would make it (this folder's code and cue files, the arguments, the
scale, the clock's events) is skipped; `--force` renders anyway, and is needed after a change
outside this folder (the pak, the repository's Rust sources, quaketool, the merge history).

**Codecs.** The overlays are ProRes 4444 with alpha (ffmpeg's `prores_ks`, profile 4444,
written from RGBA PNG frames as yuva444p10le, read back as yuva444p12le): VAAPI encodes no
alpha, so they stay in software. At scale 2 they are about four times v7's sizes. D01 and D02, the two whole-frame shots, are H.264
(`libx264`, CRF 16, yuv420p, BT.709), a few seconds each.

**The scale.** Every diagram is drawn in 1920x1080's coordinates. At `--scale N` the kit's
canvases are N times that size, with the scale in their cairo transform: lines, curves and
the Inter labels are drawn sharp at the new resolution, and id's 8x8 glyphs (conchars, the
status bar's digits) at N times their own scale, nearest neighbour, on whole pixels, so a
4K frame's letters are the 1080 frame's doubled pixel for pixel. Picture data inside a
diagram (the engine's pixel row in D06a, the 320x200 frame in D02) is the same data drawn
N times larger. At `--scale 1` the outputs are v7's byte for byte, but for D02 (below).

**What they need.** id's shareware pak (`quake-data/ID1/PAK0.PAK`), the repository as checked
out (torch.rs, lightstyle.rs and menu.rs are quoted; D03 names Rust files; D15 draws the merge
history from `git log --all`), the quaketool release build (D02 and D06a render frames, D14
mixes a sound: `cd quake-rs && cargo build --release --bin quaketool`), cairo, ffmpeg with
libx264 and prores_ks, and the font Inter (a caption or two). Frames go
to `FILM_SCRATCH` and are removed once each movie is made (`--keep-frames` keeps them).

**D02 and the engine.** D02's picture is quaketool's own e1m1 frame at 320x200. Since the
pixel-exact work, the engine draws a few hundred of that frame's pixels differently (texel
edges), so D02 at scale 1 is not v7's file; rendered from v7's cached frame it is, byte for
byte. Every other diagram and every ladder span is v7's file at scale 1.

## The kit, qkit

`qkit/` is the drawing kit: `look` (the frame size, the scale, id's palette by role), `anim`
(time: ramp, fade, keys, Cues), `text` (conchars, the status bar's digits, Inter, a small
equation layout), `canvas` (the primitives), `graph` (axes and plots), `quake` (id's pak, a
BSP reader, frames from quaketool), `render` (frames, video, stills, contact sheets). A
diagram is a script with its facts, its cues, `frame(c, t)` and `run(...)`, which gives it
its command line (`--scale`, `--duration`, `--cues`, `--alpha`, `--still`, `--contact`, `--out`).

For other code that draws with the kit (the edit's cards): `qkit.set_scale(N)` before the
first canvas, then `new_canvas(w=W, h=H, alpha=False)` gives a canvas of w x h in 1080's
coordinates at that scale, and `canvas_image(c)` its pixels at the device size. A canvas built
directly, `Canvas(surface, transparent, scale=1)`, keeps its surface's own size unless given a
scale, as before.

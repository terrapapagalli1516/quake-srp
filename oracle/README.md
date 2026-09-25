# The oracle — id's own renderer, headless, as a fidelity instrument

"Faithful to WinQuake" used to be judged by reading C. This directory makes it
measurable: id's 1996 software renderer, compiled from id's source with the null
drivers, renders the **same view, clock and entities** as the port, and a script
diffs the two frames pixel for pixel.

```sh
oracle/build.sh               # once (~20 s, docker); again after editing oracle/c/*
uv run oracle/compare.py      # e1m1/2/3/7 x world/ents, 320x200: table + side-by-side PNGs
oracle/characterise.sh        # re-derive every number and crop in this README (~10 s)
```

Needs docker (for the build only), uv, cargo, and the shareware pak at
`quake-data/ID1/PAK0.PAK` (or `--pak`). The id source is read from
`quake-c/WinQuake`
(`QUAKE_C_SRC` overrides). `compare.py --help` lists every option. The ones you will use most:

| option | what it does |
|---|---|
| `--maps e1m1,e1m3` `--modes world,ents` `--res 640x480` | the matrix to run |
| `--view=x,y,z,pitch,yaw,roll` | pin the camera (Quake convention: pitch + looks **down**). Use the `=` form when the first number is negative |
| `--time T` | pin `cl.time` (light styles, sky, turb, texture and alias animation) |
| `--settle N` | shoot N frames after signon instead of the first |
| `--crop name:x,y,w,h` | extra 6x C / port / diff PNG of a region |
| `--spans 8\|16\|1` | id's span routine: 8 = `D_DrawSpans8`, id's portable C (default); 16 = the 16-pixel segments of the x86 asm `D_DrawSpans16` (what DOS/Win players saw, `d_subdiv16 1`); 1 = exact per-pixel perspective (an experiment, not id) |
| `--c-cmd "d_mipscale 0"` | any console command for id's side before the map loads (repeatable) |
| `--bench N` | also time N warm re-renders of the view in both renderers |
| `--viewmodel` | draw the weapon too (the port is handed id's `cl.viewent` origin and angles, `quaketool view --viewent`) |
| `--c-only --full --viewsize 100 --settle 10` | id's composited screen (sbar etc.) alone — the port's `view` cannot draw the HUD |
| `--quaketool PATH` / `--oracle PATH` | A/B a different build of either side |

### Reading the output

One row per case: **exact%** = pixels whose palette index equals id's (the port's
RGB is mapped back to the palette; duplicate palette colours count as equal),
**\|dIdx\|** = mean absolute palette-index difference, **\|dRGB\|** = mean absolute
channel difference, **nonpal%** = port pixels whose colour is in no palette entry
(id's renderer can never produce one), **ents** = entities on id's draw list, then
a histogram of the max-channel \|dRGB\| per pixel. In `ents` mode a second table
gives the match rate over the pixels an entity touches (in either renderer). The
`side.png` per case is C | port | diff (diff = 4 x max-channel \|dRGB\|).

## How it works

**C side.** `build.sh` copies id's WinQuake tree (read-only reference, never
edited in place), makes one edit on the copy — `quakedef.h`'s `id386` switch, so
the portable C paths are used (the `nonintel.c` route, no assembly) — and
compiles `Makefile.linuxi386`'s C files with `cd_null`/`in_null`/`snd_null`,
loopback-only `net_none`, and three files of ours (`c/`, GPL like id's):

- `vid_oracle.c` — `vid_null.c` at any resolution (`-width`/`-height`, up to id's
  1280x1024 `MAXWIDTH`/`MAXHEIGHT`), buffers sized with `D_SurfaceCacheForRes` as
  the real drivers do, `vid.aspect` 1.0 (square pixels, as `vid_null`; id's DOS/Win
  320x200 used 0.8333 — `--aspect`).
- `sys_oracle.c` — `sys_null.c`'s file IO plus a deterministic clock: every
  `Host_Frame` is exactly 0.1 s and `Sys_FloatTime` is that virtual clock
  (`-oracle_realtime` switches to the wall clock for `timedemo`). `Sys_Quit` never
  writes `config.cfg`, so runs cannot leak cvars into each other.
- `oracle.c` — console commands (`oracle_view`, `oracle_time`, `oracle_shot`,
  `oracle_settle`, `oracle_stage`, `oracle_exit`; cvars `oracle_spans`,
  `oracle_bench`; see the file header). The link wraps `R_RenderView` and
  `D_DrawSpans8` (`-Wl,--wrap`), so a shot can pin the view/clock for one frame and
  dump `vid.buffer` the instant the 3-D view is done — before the sbar, console,
  notify text or centerprint touch it — as `.pgm` (raw palette indices, the real
  output), `.ppm`, `.json` (vieworg/angles, `cl.time`, vrect, fov, the 64
  `d_lightstylevalue`s, viewleaf contents, ...) and `.ents` (every entity on the
  frame's draw list, statics included: model, origin, angles, frame, skin,
  syncbase).

It is built as a static 32-bit i386 binary (1996 code assumes 32-bit pointers) in
a digest-pinned `i386/debian` container and runs directly on the x86_64 host.

**Port side.** `quaketool view <pak> <map> <out.ppm> [--res] [--origin] [--angles]
[--time] [--fov] [--ents FILE] [--viewmodel M:F] [--bench N]` renders one exactly
specified view through the same `render_scene_ext_sprited` the game uses. It is a
new subcommand; no existing output changed (goldens `fb14bd65`/`a6f98d8a`/`0211e6d4`).

**Matching inputs.** By default the camera and clock are id's own first frame after
signon (`V_CalcRefdef`'s eye, `cl.time` = 1.6 on these maps), handed verbatim to
the port. In `ents` mode the port draws **id's entity list** (the `.ents` file),
so the diff measures rendering, not the simulation. Note: that first-frame eye is
12 units below the standing eye height — `V_CalcRefdef`'s stair smoothing starts
from `static float oldz = 0` and clamps to 12 below the origin; `--settle 2` gives
the steady eye. (The port's live path starts `oldz` at the origin, so its first
0.15 s differs from id's here — cosmetic.)

## Results (320x200 unless stated, 2026-09-25)

After the Session 7 fixes (branch `quake/fid1`: classes 2, 3, 4, 5, 8 and 9 below,
see `AUDIT.md`; the numbers before them are in the git history of this file).

Headline — id's C as written (`D_DrawSpans8`, its own mip levels) vs the port:

| map | world exact% | with entities | entity pixels exact% | 640x480 world |
|---|---:|---:|---:|---:|
| e1m1 | 84.76 | 84.75 | 0.0 (16 px) | 90.79 |
| e1m2 | 64.07 | 64.34 | 97.8 (363 px) | 78.19 |
| e1m3 | 65.83 | 65.83 | 44.8 (221 px) | 90.74 |
| e1m7 | 75.65 | 75.67 | 58.2 (55 px) | 94.55 |

The entity-pixel column counts the world pixels around and behind an entity too,
so it is dominated by class 1 here; with id's mip 0 + exact perspective it reads
93.75 / 99.72 / 99.53 / 98.15 (was 93.75 / 15.92 / 70.97 / 45.76). nonpal% is 0 in
every case (was up to 0.45).

An entity-heavy view (e1m2 altar: ogre + two torches, `--view
1432.386,1397.978,233.254,9.344,-103.449,0`) scores 73.95% world / 74.50% with
entities, its entity pixels 100.00% (was 71.5 / 71.0 / 21.8). With the viewmodel
drawn (`--viewmodel --settle 3`), e1m1 scores 84.34%, the same as the world-only
frame at that settle (84.33; was 79.7 with the gun). At a settle of 3 or more e1m1's
far arch differs for a harness reason: id's light styles in that frame (its
`.json`) are the ones the port derives for 0.1 s earlier (measured) — probably id flooring a double
`cl.time` that the harness hands over rounded to a float. Passing id's `d_lightstylevalue` to
the port would remove it (not done).

**Attribution ladder** — id's renderer made to drop one known difference at a time
(world only; `characterise.sh` prints it):

| id's renderer configured as | e1m1 | e1m2 | e1m3 | e1m7 |
|---|---:|---:|---:|---:|
| as written (`--spans 8`) | 84.76 | 64.07 | 65.83 | 75.65 |
| 16-px segments (`--spans 16`, the x86 asm) | 80.39 | 63.79 | 65.33 | 67.98 |
| exact per-pixel perspective (`--spans 1`) | 86.98 | 64.27 | 66.00 | 80.39 |
| mip 0 forced (`--c-cmd "d_mipscale 0"`) | 93.13 | 93.59 | 97.13 | 93.35 |
| mip 0 + exact perspective | 95.63 | 97.41 | 98.39 | 98.68 |
| ... and the port given id's lightmap stepping (temporary patch, not committed; measured BEFORE the Session 7 fixes) | 99.96 | 95.98 | 99.87 | 99.74 |

With that last step at 640x480: 99.91 / 94.45 / 99.75 / 99.80, and a pitched and
rolled view (e1m1, pitch -15 yaw 100 roll 12) 99.96. The e1m2 remainder then was
the sky and two water pools (classes 3, 4, 8 below, since fixed: the e1m2 sky
region now matches 99.8%; the pools' scattered residue, ~2%, is probably class 7 —
id steps turb s/t over 16-pixel segments — not verified). What is left elsewhere (0.04-0.26%) is the
size of id's own floating-point noise: the oracle built with SSE2 float math
instead of x87 (`ORACLE_FPMATH=sse oracle/build.sh`) differs from the x87 build on
0.003-0.031% of pixels. So **projection, fov, pixel centres, edge rules, near
clipping, texture alignment, PVS and the camera convention are faithful**; every
larger difference is one of the classes below.

## Discrepancy classes, ranked

Crops are C | port | diff at 6x, in `crops/`, made by `characterise.sh` with the
classes a crop is not about removed on id's side where possible.

| # | class | size | verdict |
|---|---|---|---|
| 1 | mip level selection | 5-37 pts of exact% | departure |
| 2 | alias-model lighting / shading | 80-90% of entity pixels | bug — **fixed** (Session 7) |
| 3 | liquids and sky drawn overbright | ~75% of liquid/sky pixels | bug — **fixed** |
| 4 | sky front layer not offset | the whole cloud layer | bug — **fixed** |
| 5 | weapon viewmodel placement | ~5 pts when drawn | bug — **fixed** |
| 6 | surface-cache lightmap stepping | 1.5-4.4% | departure |
| 7 | affine span segments | 1-5% | expected so far (design) |
| 8 | turb warp rounding | ~20-25% of liquid pixels | departure — **fixed** |
| 9 | sample-less faces | the e1m1 golden view shows one | departure — **fixed** |

1. **Mip levels** (`crops/mip.png`). The port always samples mip 0; id picks a mip
   per surface (`D_MipLevelForScale` on `nearzi * scale_for_mip * mipadjust`,
   thresholds `d_mipscale`, floor `d_mipcap`) and builds the surface cache at that
   level (`R_DrawSurfaceBlock8_mip1..3`). Distant walls in id are blurrier and
   blotchier; in the port they sparkle. Forcing id to mip 0 moves e1m2 from 60% to
   90%. Largest class by far; also a speed lever (smaller cache blocks).
2. **Alias models** (`crops/alias.png`, `crops/fullbright.png`). The port lights
   them with a heuristic (`0.25 + ambient/200`, a fixed-direction Lambert) and a
   linear RGB multiply with no colormap — hence the non-palette colours in the
   `nonpal%` column — and so darkens fullbright texels (torch flames, the e1m7
   rune). id: `R_LightPoint` ambient/shade clamped to 128/192 in
   `R_DrawEntitiesOnList`, `R_AliasSetupLighting` (`LIGHT_MIN`, the fixed light
   vector `{-1,0,0}` rotated into the model frame), a per-vertex `lightcos` in
   `R_AliasTransformFinalVert`, then Gouraud colormap rows in `D_PolysetDraw` —
   fullbright texels untouched. Only 8-19% of entity pixels match. id
   also draws alias triangles affine (the port: perspective-correct) — minor,
   masked by the lighting. **Fixed:** the port now runs a port of that whole
   pipeline (`R_AliasCheckBBox`, `R_AliasClipTriangle`, `D_PolysetDraw`'s
   fixed-point edge walker, affine, recursive subdivision for far models) — the
   altar view's entity pixels match 100%.
3. **Liquids and sky overbright** (`crops/liquid.png`, `crops/pools.png`). The port
   maps turb and sky texels through colormap **row 0**, the brightest row (about
   2x); id writes the raw texel (`D_DrawTurbulent8Span`, `D_DrawSkyScans8` — no
   colormap; the identity is around row 31/32). Looking down at e1m1's water: 21%
   match, and 72-80% of liquid/sky pixels equal `palette[colormap[0][id's texel]]`.
   Lava survives because its texels are fullbright indices. **Fixed** (raw texel):
   e1m1 water from above 21% -> 99.45% with class 8.
4. **Sky layers** (`crops/sky.png`). id's `R_MakeSky` composites the front layer
   shifted by `(int)(skytime*skyspeed)` texels in both axes over the unshifted back
   layer, then `D_Sky_uv_To_st` adds `skytime*skyspeed` to both — front scrolls at
   16 texels/s, back at 8. The port scrolls both at 8 (`sky_texel(.., 0.0)`), so at
   `cl.time` 1.6 the clouds sit 12 texels off; at `--time 0.1` the shapes line up
   (only the class-3 brightness remains). Also expected-minor: id samples the sky
   exactly only every 32 pixels (`SKY_SPAN_SHIFT`) and uses the integer screen
   centre. (AUDIT's LOW "sky foreground drift" is this, and it is not small.)
   **Fixed**, the 32-pixel spans included (the world pass defers its sky pixels
   and redraws each visible run of a sky face as one span): the e1m2 sky region
   matches 99.8%. Open: at a viewsize below 120 id's sky centre is the SCREEN's
   (`vid.width>>1`), not the view rectangle's; the port uses the view's.
5. **Viewmodel** (`crops/viewmodel.png`). The port hangs the gun with invented
   offsets (`OFS_FORWARD 7`, `OFS_RIGHT 1.5`, `OFS_UP 3.5`) and its own depth buffer;
   id puts `cl.viewent` at the eye (+ bob, + the `scr_viewsize` fudge), angles from
   `CalcGunAngle`, drawn by `R_AliasDrawModel` like any alias model. The port's
   shotgun is several times larger and in a different place. **Fixed** (the
   origin with the options branch; then the alias pipeline, the camera's 1/32
   epsilon, full-pitch bob, CalcGunAngle's pre-punch angles, the tripled 1/z):
   the frame with the gun matches as well as the frame without it.
6. **Lightmap stepping** (`crops/lightmap.png`). id's `R_DrawSurfaceBlock8_mip0`
   interpolates the already inverted, clamped light (`t = (255*256 - bl) >> 2`,
   `>= 64`) with `>> 4` integer steps, and horizontally walks from the **right**
   luxel (texel 15 of a block gets the right luxel exactly, texel 0 gets
   `right + 15*step`); the port takes a float bilinear sample at texel `i/16` and
   then picks the row. Result: +-1 colormap row on light gradients. Proven by the
   ladder's last row (a temporary port patch doing id's integer stepping; reverted).
   The comment on `face_surf_block` claims exactness; it is one texel and one
   rounding off. (id's x86 `surf8.s` steps with deltas; not checked against it.)
7. **Span subdivision** (`crops/spans.png`: vertical stripes where the affine error
   crosses a texel). id divides every 8 pixels (portable C) or 16 (x86 asm) and
   steps s/t linearly between; the port divides per pixel. Documented as a perf
   item in STATUS.md; a faithful port would subdivide (16 to match what players
   saw, 8 to match id's C).
8. **Turb warp.** The port rounds `sintable` to whole texels and floors s/t before
   adding; id adds the 16.16 table value to the fixed-point coordinate and then
   takes `>> 16`, over 16-pixel segments. About a fifth of liquid pixels land one
   texel off once class 3 is factored out. **Fixed** (per pixel; id's 16-pixel
   linear segments are class 7).
9. **Sample-less faces** (`lightofs == -1`, ~375 world faces in e1m1, ~430 in e1m2 —
   ordinary textures, not the sky/turb faces, which are counted apart).
   id's `R_BuildLightMap` leaves them at ambient 0, i.e. colormap row 63 (black);
   the port drew them at normal brightness. Not visible in the views above, but
   the e1m1 golden camera shows one (a recessed panel edge). **Fixed**; still
   open: a map with no lighting lump (id: fullbright) renders Lambert.
   AUDIT's "lightless/test maps only" is wrong about where such faces exist.

Brush entities (doors, plats, `b_*.bsp` boxes) matched about as well as the world
in the views tried, but were not examined closely.

## Speed

id's portable C (1 core, gcc -O2, x87), `timedemo demo1`, 969 frames, sound and
input null: **1480 fps at 320x200, 535 at 640x480, 192 at 1280x1024**
(`oracle/build/quake-oracle -basedir <dir with id1/pak0.pak> -oracle_realtime
-width W -height H +timedemo demo1`). id's renderer cannot go above 1280x1024.

Same view, warm, world only (`compare.py --modes world --bench 100`): the port
takes **1.7-2.2x** id's time at 320x200, **2.3-2.7x** at 640x480 and **2.7-3.6x** at
1280x1024 — while also doing more work per pixel (mip 0 everywhere, a divide per
pixel). Timings are noisy: compare within one sitting.

## Caveats — what was not verified

- id's C, not id's asm. The shipped x86 binaries used `d_draw16.s`, `surf8.s`,
  `d_polysa.s` and friends; the oracle runs the portable C. `--spans 16`
  reproduces the 16-pixel segment algorithm in C, not the asm's exact x87
  rounding. Other asm-vs-C differences are unmeasured.
- 32-bit build with modern gcc 12 (`-O2 -fwrapv -fno-strict-aliasing`), not MSVC
  1996. The x87-vs-SSE check bounds the float noise at ~0.03%.
- Only e1m1/2/3/7, a handful of views, 320x200-1280x1024. No particles, dynamic
  lights, underwater warp, sprites or intermission were compared (the oracle can
  render them; nobody looked yet).
- The entity mode tests rendering of id's entity list; it says nothing about
  whether the port's simulation produces the same list.
- `viewsize` below 120: the C side renders it (`--c-only --full`), the port's
  `view` does not draw the HUD or shrink the 3-D view, so there is no diff yet.

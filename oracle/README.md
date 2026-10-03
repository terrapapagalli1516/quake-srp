# The oracle — id's own renderer, headless, as a fidelity instrument

"Faithful to WinQuake" used to be judged by reading C. This directory makes it
measurable: id's 1996 software renderer, compiled from id's source with the null
drivers, renders the **same view, clock and entities** as the port, and a script
diffs the two frames pixel for pixel.

```sh
oracle/build.sh               # ~20 s, docker (the tools run it themselves when the binary is missing or older than oracle/c/*)
uv run oracle/compare.py      # e1m1/2/3/7 x world/ents, 320x200: table + side-by-side PNGs
oracle/characterise.sh        # re-derive every number and crop in this README (~10 s)
uv run oracle/screen2d.py     # the 2-D layer (status bar, menus, console, ...): see its section
uv run oracle/sound.py        # id's mixer against the port's, sample for sample: see "Sound"
uv run oracle/sound_walk.py   # a walk through id's game and the port's, every sound call compared
uv run oracle/rotate_check.py --id1-pak1 … --hipnotic …   # a rotating brush model (Scourge's start door) through its swing
uv run oracle/classic_check.py  # all of Classic's proof in one run: see "Classic"
```

Needs docker (for the build only), uv, cargo, and the shareware pak at
`quake-data/ID1/PAK0.PAK` (or `--pak`). The id source is read from
`quake-c/WinQuake` at the repository's root (`QUAKE_C_SRC` overrides).
`compare.py --help` lists every option. The ones you will use most:

| option | what it does |
|---|---|
| `--maps e1m1,e1m3` `--modes world,ents` `--res 640x480` | the matrix to run |
| `--view=x,y,z,pitch,yaw,roll` | pin the camera (Quake convention: pitch + looks **down**). Use the `=` form when the first number is negative |
| `--time T` | pin `cl.time` (light styles, sky, turb, texture and alias animation) |
| `--settle N` | shoot N frames after signon instead of the first |
| `--crop name:x,y,w,h` | extra 6x C / port / diff PNG of a region |
| `--spans 8\|16\|1` | id's span routine: 8 = `D_DrawSpans8`, id's portable C (default); 16 = the x86 asm's `D_DrawSpans16` in C, its integer steps included (what DOS/Win players saw, `d_subdiv16 1` — and what the port draws); 1 = exact per-pixel perspective (an experiment, not id) |
| `--exactpersp` | the port's exact per-pixel perspective extra (`quaketool view --exactpersp 1`) instead of its default 16-pixel spans; pair it with `--spans 1` |
| `--perspspan 16\|8\|4\|1` | the port's perspective span (`quaketool view --perspspan`, `r_perspspan`): 8 is id's C `D_DrawSpans8`, so `--spans 8 --perspspan 8` puts the port's 8 against id's portable C |
| `--aspect A` | `vid.aspect` for both renderers (`-oracle_aspect` / `quaketool view --aspect`). Default 1.0, square pixels; `0.8333333` is id's DOS/Win 16:10 modes on a 4:3 monitor — and what the browser page shows (every preset is 16:10, presented at 4:3) |
| `--c-cmd "d_mipscale 0"` | any console command for id's side before the map loads (repeatable); `d_mipscale` and `d_mipcap` are handed to the port too (`quaketool view --d-mipscale/--d-mipcap`). `--c-cmd +attack --settle 3` gives a frame lit by the shotgun's muzzle flash: id's live `cl_dlights` are written to the `.json` and handed to the port (`quaketool view --dlight`) |
| `--bench N` | also time N warm re-renders of the view in both renderers |
| `--viewmodel` | draw the weapon too (the port is handed id's `cl.viewent` origin and angles, `quaketool view --viewent`) |
| `--viewsize N` | id's `scr_viewsize` (default 120, the whole screen). Below 120 the 3-D view rectangle (`r_refdef.vrect` from id's `.json`) is compared: the port renders it placed on the screen (`quaketool view --vrect`) |
| `--c-only --full --viewsize 100 --settle 10` | id's composited screen (sbar etc.) alone — the port's `view` cannot draw the HUD; `screen2d.py` compares the 2-D layer |
| `--quaketool PATH` / `--oracle PATH` | A/B a different build of either side |
| `--c-post CMD` | a console command for id's side once the map is loaded (repeatable; each `--c-post wait` holds what follows one frame): six waits then `--c-post "impulse 9"` with `--c-cmd +attack` fires rockets, `--settle 14`..`19` on e1m1 catches the explosion sprite on the far wall |
| `--demo NAME` | id's side plays the demo (`playdemo NAME`) instead of loading a map, and the shot is frame `--settle` of its playback; `--maps` names the demo's map for the port (`--maps e1m3 --demo demo1 --settle 52`: a grenade explosion among gibs) |
| `--oracle-dt DT` | id's host frame step in seconds (default 0.1, the oracle's own); `0.01388888899236917` is the port's 1/72 s, the step `quaketool play` and `demo_lerp.py` run at, so `--demo demo1 --settle K` is frame K of `quaketool play demo1 --trace` |
| `--dlights none\|id\|demoN` | the lights the port's frame is lit with: id's own `cl_dlights` of the frame (default), none (what a demo's playback drew before `fleet/demolights`), or the lights the port's own playback of demo N makes at id's `cl.time` (`quaketool play demoN --trace`; needs `--oracle-dt` as above) |
| `--id-lightstyles` | hand the port id's `d_lightstylevalue[]` of the frame (`quaketool view --style-values`) instead of the styles it derives from the clock: a demo frame's are the recording's, which the map's own animation does not know |
| `--game-dir NAME DIR` `--pak1 PAK` | a mission pack: `DIR`'s `pak0.pak`/`pak1.pak` layered over id1 as id's `-NAME` does (`-hipnotic`, `-rogue`), with id1's registered `pak1.pak` (id's own `-hipnotic` refuses the shareware id1); the port's `quaketool view` takes the same paks as a comma list |

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
loopback-only `net_none`, and four files of ours (`c/`, GPL like id's):

- `vid_oracle.c` — `vid_null.c` at any resolution (`-width`/`-height`, up to id's
  1280x1024 `MAXWIDTH`/`MAXHEIGHT`), buffers sized with `D_SurfaceCacheForRes` as
  the real drivers do, `vid.aspect` 1.0 (square pixels, as `vid_null`; id's DOS/Win
  320x200 used 0.8333 — `--aspect`, which also hands the port the same value).
- `sys_oracle.c` — `sys_null.c`'s file IO plus a deterministic clock: every
  `Host_Frame` is exactly 0.1 s and `Sys_FloatTime` is that virtual clock
  (`-oracle_realtime` switches to the wall clock for `timedemo`; `-oracle_dt`
  sets the step; `-oracle_loadtime T` makes a level load take T seconds of it, as
  loads took seconds on id's machines, so the frame after one runs
  `Host_FilterTime`'s 0.1 s clamp). `Sys_Quit` never writes `config.cfg`, so runs
  cannot leak cvars into each other.
- `oracle.c` — console commands (`oracle_view`, `oracle_time`, `oracle_shot`,
  `oracle_settle`, `oracle_stage`, `oracle_exit`, `oracle_entfield num field v…` to
  pose any edict for a shot, as `rotate_check.py` swings a door; cvars `oracle_spans`,
  `oracle_bench`; see the file header). The link wraps `R_RenderView` and
  `D_DrawSpans8` (`-Wl,--wrap`), so a shot can pin the view/clock for one frame and
  dump `vid.buffer` the instant the 3-D view is done — before the sbar, console,
  notify text or centerprint touch it — as `.pgm` (raw palette indices, the real
  output), `.ppm`, `.json` (vieworg/angles, `cl.time`, vrect, fov, the 64
  `d_lightstylevalue`s, viewleaf contents, ...), `.ents` (every entity on the
  frame's draw list, statics included: model, origin, angles, frame, skin,
  syncbase) and `.parts` (the particles `R_DrawParticles` is about to draw, in
  its order: origin and colour, written before the render moves them).
- `walk_oracle.c` — a scripted walk (`oracle_walk`, driven from a wrapped
  `CL_SendCmd`) and a log of every call into the sound layer (`oracle_sndlog`:
  `snd_null`'s entry points and the server's `SV_StartSound`, wrapped at link
  time); `sound_walk.py` drives both (see "Sound").

It is built as a static 32-bit i386 binary (1996 code assumes 32-bit pointers) in
a digest-pinned `i386/debian` container and runs directly on the x86_64 host.

**Port side.** `quaketool view <pak> <map> <out.ppm> [--res] [--origin] [--angles]
[--time] [--fov] [--aspect] [--exactpersp] [--ents FILE] [--particles FILE] [--viewmodel M:F] [--bench N]` renders one exactly
specified view through the same renderer the game uses (`render::Renderer` drawing a
`render::Scene`; `quaketool --help` lists the rest of its options). It is a
new subcommand; no existing output changed (goldens `fb14bd65`/`a6f98d8a`/`0211e6d4`
when it was added; the renderer fixes since moved them to `4807aaa1`/`9ae2b478`/`c65b7046`
at `3ba835f`).

**Matching inputs.** By default the camera and clock are id's own first frame after
signon (`V_CalcRefdef`'s eye, `cl.time` = 1.6 on these maps), handed verbatim to
the port. In `ents` mode the port draws **id's entity list** (the `.ents` file),
so the diff measures rendering, not the simulation; in both modes it draws id's
particles (the `.parts` file). Note: that first-frame eye is
12 units below the standing eye height — `V_CalcRefdef`'s stair smoothing starts
from `static float oldz = 0` and clamps to 12 below the origin; `--settle 2` gives
the steady eye. (The port's live path starts `oldz` at the origin, so its first
0.15 s differs from id's here — cosmetic.)

## Results (320x200 unless stated, 2026-09-25)

After the Session 7 fixes (branch `quake/fid1`: classes 2, 3, 4, 5, 8 and 9 below),
the mip levels and lightmap stepping (branch `quake/w2a`: classes 1 and 6), and the
pixel aspect, id's 16-pixel spans and the screen-centred sky (branch `quake/w2b`:
class 7, class 4's open note), and id's edge-sorted span renderer for the world and
brush models (branch `quake/edge`: class 1's open note, PERF_PLAN A3); see
`AUDIT.md`. The numbers before them are in the git history of this file.

The port draws what DOS/Windows players saw: the x86 build's `D_DrawSpans16`. So the
headline compares it with id's renderer set to the same (`--spans 16`); the oracle's
default is still id's portable C as written (`D_DrawSpans8`), last column.

| map | `--spans 16` world exact% | with entities | entity pixels exact% | 640x480 world | 640x400 at the page's 4:3 aspect | `--spans 8` (default) world |
|---|---:|---:|---:|---:|---:|---:|
| e1m1 | 99.96 | 99.96 | 100.0 (15 px) | 99.97 | 100.00 | 94.57 |
| e1m2 | 99.94 | 99.94 | 100.0 (357 px) | 99.97 | 100.00 | 96.82 |
| e1m3 | 99.98 | 99.98 | 100.0 (239 px) | 99.99 | 100.00 | 97.27 |
| e1m7 | 99.91 | 99.91 | 100.0 (54 px) | 99.94 | 100.00 | 91.72 |

(Before the 16-pixel spans, on the same base: 92.84 / 94.36 / 96.56 / 87.20 against
`--spans 16`, 97.44 / 97.12 / 99.16 / 94.96 against `--spans 8`. Before classes 1
and 6: 84.76 / 64.07 / 65.83 / 75.65 against `--spans 8`.) Against `--spans 8` what
remains is id's portable C's 8-pixel segments against the port's 16 (class 7).
(Before the edge renderer e1m2 read 99.21 and 96.09: one face at a finer mip in id
than its geometry gives, class 1's open note, which id's edge cache produces and the
port now does too.) What is left in the `--spans 16` rows is single pixels on
texel boundaries along 45-degree lines of floor texture and a few sky pixels —
float noise of the texel arithmetic (carrying the edge arithmetic in f64 instead of
the C's floats moves nothing). The entity-pixel column counts the world pixels
around and behind an entity too. nonpal% is 0 in every case (was up to 0.45).

**Pixel aspect** (`--aspect 0.8333333`: id's 16:10 modes on a 4:3 monitor, which
is how the browser page shows every preset). 320x200 against `--spans 16`: 100.00 /
100.00 / 100.00 / 100.00 (e1m7 99.997), entity pixels 100% everywhere; at viewsize
100 (the view above the status bar, `--viewsize 100`) 100.00 / 100.00 / 100.00 /
100.00 (e1m2 was 98.73 and 99.46 before the edge renderer). Before
the port's projection took the aspect it scored 31.35 / 26.90 / 20.23 / 14.23
against id's 4:3 frame (then with id at mip 0 and exact perspective).

**Underwater** (e1m1's pool, `--view=750,898,-332,0,90,0 --time 1.6`: id's
`r_dowarp` view, rendered into the 320x200-at-most warp buffer and stretched
over the screen by `D_WarpScreen`), `--spans 16`: 99.89 / 99.89 / 99.90 / 99.89
at 320x200 / 640x400 / 960x600 / 1280x800, 99.93 at all four at the page's
aspect. `compare.py` places the view by the `.json`'s `scr_vrect`: above
320x200 an underwater frame's `vrect` is the warp buffer's rectangle, not the
screen's. Below viewsize 120 an underwater view is not comparable (the port's
`view --vrect` draws it unwarped; `compare.py` warns).

**Particles** (the shotgun's puffs on e1m1's first wall: `--c-cmd +attack
--settle 3`, 120 particles; the port draws id's own list, the `.parts` file):
over the pixels a particle touches in either renderer, 100% at 320x200 /
640x400 / 960x600 (39 / 151 / 393 px) since `quake/polish2`'s port of
`D_DrawParticle`; before, 12.8 / 12.6 / 15.0% (the walls' `xscale` instead of
`xscaleshrink`, a centred square of `focal/z` pixels instead of `izi >>
d_pix_shift` from `(u, v)`, clipping instead of dropping at the edge, and a
float depth test where id's quantized 1/z lets the later particle win a tie).
The rest of that frame is the settle-3 arch below.

An entity-heavy view (e1m2 altar: ogre + two torches, `--view
1432.386,1397.978,233.254,9.344,-103.449,0`, `--spans 16`) scores 99.99% world and
with entities, its entity pixels 100.00% (788 px; 664 px, 100%, at the page's
aspect). With the viewmodel drawn (`--viewmodel --settle 3`), e1m1 scores 98.07%,
the same as the world-only frame at that settle. At a settle of 3 or more e1m1's
far arch differs for a harness reason: id's light styles in that frame (its
`.json`) are the ones the port derives for 0.1 s earlier (measured) — probably id flooring a double
`cl.time` that the harness hands over rounded to a float. Passing id's `d_lightstylevalue` to
the port would remove it (not done).

**Attribution ladder** — id's renderer made to drop one known difference at a time
against the port as it ships (16-pixel spans; world only; `characterise.sh` prints it):

| id's renderer configured as | e1m1 | e1m2 | e1m3 | e1m7 |
|---|---:|---:|---:|---:|
| as written (`--spans 8`) | 94.57 | 96.82 | 97.27 | 91.72 |
| 16-px segments (`--spans 16`, the x86 asm) | 99.96 | 99.94 | 99.98 | 99.91 |
| exact per-pixel perspective (`--spans 1`) | 92.85 | 95.12 | 96.58 | 87.19 |
| mip 0 forced (`--c-cmd "d_mipscale 0"`, both renderers) | 94.57 | 94.26 | 96.05 | 91.70 |
| mip 0 + 16-px segments | 99.95 | 99.93 | 99.98 | 99.91 |
| mip 0 + exact perspective, the port's too (`--exactpersp`) | 99.93 | 99.88 | 99.98 | 99.91 |

`d_mipscale` and `d_mipcap` are the port's cvars too, and `compare.py` hands them
to both sides, so the "mip 0" rows put both renderers at mip 0. With the port's
exact-perspective extra, id's exact rows are the pre-`quake/w2b` port's to the
pixel: 99.94 / 99.91 / 99.98 / 99.91 at `--spans 1` (e1m2 was 99.18 before the edge
renderer). Mip 0 + exact at 640x480:
99.96 / 99.96 / 99.99 / 99.98; a pitched and rolled view (e1m1,
`--view=544,288,32,-15,100,12`) 100.00. Over 72 more views
(the four start positions, 6 yaws x 3 pitches) against id's exact perspective and
its own mip levels, the mean is 99.99% and the worst 99.83 (before the 16-pixel
spans, measured with the exact extra's arithmetic). What is left elsewhere is the size of id's own floating-point noise: the oracle built with SSE2 float math
instead of x87 (`ORACLE_FPMATH=sse oracle/build.sh`) differs from the x87 build on
0.003-0.031% of pixels. So **projection, fov, pixel centres, edge rules, near
clipping, texture alignment, PVS and the camera convention are faithful**; every
larger difference is one of the classes below.

## Discrepancy classes, ranked

Crops are C | port | diff at 6x, in `crops/`, made by `characterise.sh` with the
classes a crop is not about removed on id's side where possible.

| # | class | size | verdict |
|---|---|---|---|
| 1 | mip level selection | 5-37 pts of exact% | departure — **fixed** |
| 2 | alias-model lighting / shading | 80-90% of entity pixels | bug — **fixed** (Session 7) |
| 3 | liquids and sky drawn overbright | ~75% of liquid/sky pixels | bug — **fixed** |
| 4 | sky front layer not offset | the whole cloud layer | bug — **fixed** |
| 5 | weapon viewmodel placement | ~5 pts when drawn | bug — **fixed** |
| 6 | surface-cache lightmap stepping | 1.5-4.4% | departure — **fixed** |
| 7 | affine span segments | 1-5% | the port draws the x86's 16 — **fixed** against `--spans 16` |
| 8 | turb warp rounding | ~20-25% of liquid pixels | departure — **fixed** |
| 9 | sample-less faces | the e1m1 golden view shows one | departure — **fixed** |

1. **Mip levels** (`crops/mip.png`). The port always samples mip 0; id picks a mip
   per surface (`D_MipLevelForScale` on `nearzi * scale_for_mip * mipadjust`,
   thresholds `d_mipscale`, floor `d_mipcap`) and builds the surface cache at that
   level (`R_DrawSurfaceBlock8_mip1..3`). Distant walls in id are blurrier and
   blotchier; in the port they sparkle. Forcing id to mip 0 moves e1m2 from 60% to
   90%. Largest class by far; also a speed lever (smaller cache blocks).
   **Fixed** (`surf.rs` `MipView`, `face_surf_block`): the port picks the level as
   `D_DrawSurfaces` does, `nearzi` over the outline clipped to the frustum's sides,
   and bakes one `extents >> miplevel` block per face per level from the BSP's own
   levels. Open: `R_RenderFace` can take a stale `r_rightexit`/`r_leftexit` from an
   earlier face when the edge that should set it was cached as fully clipped — the
   face then gets the `1/z` of another face's point, a finer level (e1m2's first
   frame: face 733, id mip 0 from a point at z 96, the port mip 1). Reproducing it
   takes id's edge cache and `R_RecursiveWorldNode` order. **Fixed** with the edge
   renderer (branch `quake/edge`, PERF_PLAN A3): the port runs id's edge list, so a
   surface's `nearzi` comes from its own edges, stale exits included — e1m2 99.21 ->
   99.94.
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
   matches 99.8%. Below viewsize 120 id's sky centre is the SCREEN's
   (`vid.width>>1`), not the view rectangle's; **fixed** on `quake/w2b` (the port
   used the view's: 24 rows off at viewsize 100): the e1m2 sky region at viewsize
   100 / 110 / 70 matches 75.1 / 46.4 / 43.4% before, 100% after.
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
   **Fixed** for all four levels (`LightMap::blocklights_into`,
   `surf::draw_surface_block`): with both renderers at mip 0 and exact perspective
   the rows went 95.61 / 97.40 / 98.50 / 98.66 -> 99.93 / 99.88 / 99.98 / 99.91.
7. **Span subdivision** (`crops/spans.png`: vertical stripes where the affine error
   crosses a texel). id divides every 8 pixels (portable C) or 16 (x86 asm) and
   steps s/t linearly between; the port divided per pixel. **Fixed** (branch
   `quake/w2b`): the port draws what players saw, `d_draw16.s`'s `D_DrawSpans16`
   (its integer steps) on the surface cache and `Turbulent8`'s 16-pixel segments
   on liquids, over id's spans (the 16-pixel grid restarts where a surface
   comes out from behind a nearer one). Against `--spans 16` the rows match as
   well as exact against exact; against id's portable C (`--spans 8`, the
   default) this class is now the 8-vs-16 difference. The old per-pixel
   perspective is the port's opt-in extra (`--exactpersp`).
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

Brush entities (doors, plats, `b_*.bsp` boxes): since the edge renderer they sort
with the world as id's do — in the world's edge list, clipped into the leaves they
span, keyed like those leaves, and sorted on 1/z against the other brush models in
the same leaf. 144 views facing the first twelve brush models of each map from
two or four sides (`--modes ents --spans 16`, 320x200): mean 94.35 -> 99.37%, 57
views better and none worse than the polygon walker. The lowest that remain (57 to
94%) are all cameras inside solid (leaf 0: no PVS), where id's frame shows the
background on floors and walls that both of the port's renderers draw; not
understood, and not a place a player's eye can be.

## Speed

id's portable C (1 core, gcc -O2, x87), `timedemo demo1`, 969 frames, sound and
input null: **1480 fps at 320x200, 535 at 640x480, 192 at 1280x1024**
(`oracle/build/quake-oracle -basedir <dir with id1/pak0.pak> -oracle_realtime
-width W -height H +timedemo demo1`). id's renderer cannot go above 1280x1024.
The port runs the same `timedemo` (`quaketool timedemo pak0.pak demo1 --res WxH`, or
the browser console): the same 969 frames, its rate next to id's in `PERF_PLAN.md` §10.

Same view, warm, world only (`compare.py --modes world --spans 16 --bench 100`):
the port takes **0.30-0.34x** id's time at 320x200, **0.37-0.45x** at 640x480 and
**0.43-0.51x** at 1280x1024 with id's edge renderer (PERF_PLAN A3); the polygon
walker it replaced took 0.48-0.81x, 0.75-1.32x and 1.06-1.94x in the same sitting
(and 1.7-3.6x before the polygon span walker of PERF_PLAN A1; the mip levels leave
a warm frame's cost where it was — they cut the rebakes, which a warm static view
has none of). Timings are noisy: compare within one sitting.

## Caveats — what was not verified

- id's C, not id's asm. The shipped x86 binaries used `d_draw16.s`, `surf8.s`,
  `d_polysa.s` and friends; the oracle runs the portable C. `--spans 16`
  reproduces `D_DrawSpans16` in C with the asm's integer steps (since
  `quake/w2b`; before, `D_DrawSpans8`'s — the two differ on 0-40 pixels of a
  320x200 frame), not the asm's x87 single-precision chop
  rounding. Other asm-vs-C differences are unmeasured.
- 32-bit build with modern gcc 12 (`-O2 -fwrapv -fno-strict-aliasing`), not MSVC
  1996. The x87-vs-SSE check bounds the float noise at ~0.03%.
- Only e1m1/2/3/7, a handful of views, 320x200-1280x1024. No sprites or
  intermission were compared (the oracle can render them; nobody looked yet);
  the underwater warp in one view and particles in one burst (above). Dynamic
  lights: the muzzle-flash frames above, since PERF_PLAN A2 (`AUDIT.md`).
- The entity mode tests rendering of id's entity list; it says nothing about
  whether the port's simulation produces the same list.
- `viewsize` below 120: the 3-D view rectangle is compared (`--viewsize N`);
  the HUD, the border around it and the rest of the 2-D layer: see the next section.

## The 2-D layer (`screen2d.py`)

```sh
uv run oracle/screen2d.py                                   # every scenario, 320x200 + 640x400
uv run oracle/screen2d.py --res 960x600 --only hud,console  # one mode, some scenarios
uv run oracle/screen2d.py --list                            # the scenarios and their shots
```

The status bar, inventory, face, numbers, scoreboards, intermission and finale
overlays, centerprints, notify lines, console, menus, fade and backtile, measured
against id's own composited screen. Each scenario is a list of steps (set a stat,
press a key, open the console, centerprint, start an intermission, let N frames
pass, shoot) played through both programs from the same e1m1 start, 30 frames
after the map loads:

- **id's side** is the oracle run from a console script: `oracle_stage 1` dumps
  the screen at `VID_Update`, and the new commands in `c/oracle.c` set the state:
  `oracle_blank idx` (the 3-D view one flat palette index after every
  `R_RenderView`), `oracle_field`/`oracle_global` (player fields and QuakeC
  globals through `ED_ParseEpair`), `oracle_centerprint`, `oracle_intermission n t
  [text]` (what `svc_intermission`/`svc_finale`/`svc_cutscene` do on the client),
  `oracle_faceanim` (the pain face), `oracle_key` (`Key_Event`, so menus are
  driven by keys as a player drives them), `oracle_quitmsg` (the quit prompt's
  `rand()&7`). A shot's `.json` now carries `realtime`, `host_time`,
  `scr_centertime_start`, `scr_con_current`, `key_dest`, `sb_lines`, `cl.stats`
  and so on. `vid_oracle.c` registers `snd_null`'s two volume cvars (the Options
  sliders read 0 without) and draws the video menu's title, so Options shows its
  "Video Options" row as the DOS and Windows drivers do.
- **The port's side** is the live `App` (the browser's own code, compiled
  natively) driven through the host's own functions, as the page's calls reach them,
  by an ignored test, `quake-wasm/src/oracle_screen.rs` (`QUAKE_SCREEN_SCRIPT=script
  cargo test --release --bin quake oracle_screen -- --ignored`; the script commands are in its
  header). The client's view hook (`quake_rs::client::set_view_hook`) paints the view the same flat colour,
  and each shot is handed the C frame's clocks (`realtime` for the flashing
  cursors, `host_time` for the menu's spinning dot, the finale's reveal time).
- The port runs its Classic profile (every engine departure off) with id's own
  controls, `default.cfg`'s bindings and id's cvars (the harness runs the console's
  `idcontrols` after booting: the controls are the same in both profiles by
  default now, `quake_rs::settings`), id's side its own defaults, so the Options
  and Customize screens compare values and bindings too.
  (Before the profiles both sides were given the port's two input defaults
  then, Always Run and the WASD binds.)

With the 3-D view one colour, what differs is the 2-D layer. `exact%` is over the
whole screen; `2d exact%` over the pixels that are not the blank colour in either
screen. Colours are compared as presented, after `V_UpdatePalette`, so the
powerup tints count. Per shot: `<scenario>.<shot>.side.png` (C | port | white
where they differ). Bulky: write `--out` to disk, not `/tmp`.

**Results** (2026-09-25, branch `quake/fid2d`): the lowest `2d exact%` of each
scenario's shots, before the branch -> after.

| scenario (shots) | 320x200 | 640x400 | 960x600 |
|---|---:|---:|---:|
| hud: viewsize 100/110/120/50/30 (5) | 98.4 -> 100 | 4.1 -> 100 | 2.9 -> 100 |
| full inventory, keys, 4 runes (1) | 97.9 -> 100 | 3.7 -> 100 | 2.7 -> 100 |
| each weapon selected (8) | 96.9 -> 100 | 3.6 -> 100 | 2.5 -> 100 |
| faces by health, pain, dead (9) | 98.4 -> 100 | 4.1 -> 100 | 2.7 -> 100 |
| quad, ring, pentagram, suit, ring+pent (5) | 98.4 -> 100 | 3.3 -> 100 | 2.4 -> 100 |
| armour types (3) | 98.4 -> 100 | 3.6 -> 100 | 2.5 -> 100 |
| new-weapon flash (1) | 96.7 -> 98.3 -> 100 | 4.1 -> 99.2 -> 100 | 2.9 -> 99.4 -> 100 |
| Tab scoreboard at viewsize 100/110/120/50 (4) | 98.4 -> 100 | 4.2 -> 100 | 2.0 -> 100 |
| centerprint 1/3/5 lines, expired (4) | 91.6 -> 100 | 3.8 -> 100 | 2.7 -> 100 |
| notify lines (1) | 98.6 -> 100 | 3.8 -> 100 | 2.6 -> 100 |
| console sliding, down, typing (3) | 36.7 -> 99.1 | 4.8 -> 98.2 | 2.8 -> 97.3 |
| console scrolled back by PgUp/PgDn, 4 and 2 lines (2; `quake/polish4b`, before it no backscroll) | 98.4 -> 99.2 | 98.5 -> 99.0 | 98.6 -> 98.9 |
| intermission, also at viewsize 50 (2) | 100 -> 100 | 0.1 -> 100 | 0.0 -> 100 |
| finale mid-reveal, later (2) | 85.7 -> 100 | 0.6 -> 100 | 0.0 -> 100 |
| menus: main (2), single player / load (3), save, multiplayer | 98.3 -> 99.6 -> 100 | 43.8 -> 99.9 -> 100 | 47.1 -> 99.95 -> 100 |
| menus: options, customize, video (3) | 95.4 -> 95.5 | 41.5 -> 98.8 | 44.7 -> 99.5 |
| menus: Multiplayer > Setup — on Accept, colours stepped, typing the host name, the player's name (4; `quake/polish4b`, before it no Setup) | 77.3 -> 100 | 93.6 -> 100 | 97.1 -> 100 |
| menus: help pages (2) | 100 -> 100 | 3.6 -> 100 | 1.0 -> 100 |
| quit prompt (2 messages) and No (3) | 65.9 -> 100 | 43.8 -> 100 | 47.4 -> 100 |
| pause: the plaque and "player paused the game", unpaused (2; `quake/timedemo`, before it no `pause`) | 100 | 100 | 100 |
| menu over the disconnected console: main, options (2; `quake/polish4b`) | 41.5 -> 99.4 / 98.9 | 29.8 -> 99.4 / 99.3 | 27.7 -> 99.4 / 99.3 |

The before column is `bbfc6bc` with the same harness. The fixes, one commit
each (`AUDIT.md`, "The 2-D layer"): the ammo counts 4 px left; a "quake-rs" label
under the main menu; a note line under Multiplayer; centerprints one row low;
the whole 2-D layer blown up from 320x200 in every larger mode (id draws it 1:1:
that is now the default and the blow-up an opt-in extra); the console (a fixed
60% panel, no slide, the conback's top rows, its own text layout); the quit
prompt (an invented box and question); the console's line width; the notify
lines surviving a console toggle; the console lingering after `map`/`load`.

**What is left, and why**

- *Menu over the disconnected console* (`quake/polish4b`, scenario
  `menu_disconnected`: a `playdemo` that cannot open its file disconnects,
  the console is forced up, Escape brings up the menu): `M_Draw` draws the
  menu over `Draw_ConsoleBackground (vid.height)` while `scr_con_current` is
  non-zero, not over the faded screen; the port faded the console text
  (41.5% before). What is left is the console's version stamp (below) and,
  on Options, its "Classic / 2026" row. `oracle.c` shoots a composited frame that
  renders no view (disconnected) as the screen stands.
- *Console* (and `console_scroll`, the menu over the disconnected console:
  400 px at 320x200, 1592 at 640x400, 3582 at 960x600): the version
  string stamped on the conback. id's Linux build (the oracle) writes "(Linux
  Quake 1.30) 1.09"; the port writes what the DOS build writes, "1.09", at the
  same place — it matches the tail of the oracle's string pixel for pixel. The
  Windows build wrote "(WinQuake) 1.09".
- *Video Options* (2422 px): the mode list is the video driver's (`VID_MenuDraw`
  in `vid_win.c`/`vid_dos.c`), and the port's is its own; only the title is
  id's in both.
- ~~*The new-weapon flash* (256 px)~~: not the 2-D layer. The port's `sv.time`
  added up in f32, id's is a double; after 60 frames of 0.1 s the port's clock
  was 7.2999954, so `(int)((cl.time - item_gettime)*10)` landed on 2 where id's
  gives 3 — the flash showed the frame before. `sv.time` is a double since
  `quake/polish2`: 100% in all three modes.
- ~~*Returning to Single Player from Load* (206 px)~~: id keeps each menu's
  cursor (`m_singleplayer_cursor`, `m_main_cursor`, `options_cursor`, ...:
  Escape from Options lands on "Options"); the port's one cursor started every
  screen at its first row. Per-menu cursors since `quake/polish2`: 100%.
- *Options* (531 px; 291 while it read "Web extras"): the port's 14th row,
  "Classic / 2026" with the profile printed at x=220 (in the slot of the
  `_WIN32` build's "Use Mouse"), which id's DOS/Linux list does not have. The
  `menu_options` scenario reaches Video Options with twelve DOWNs, not one UP,
  since UP from row 0 wraps to that 14th row in the port (it had been
  comparing id's Video Modes with the port's Web extras page since the extras
  merge).
- Not in the matrix: the loading plaque (the port loads within a frame and draws
  none; the pause plaque is in it since `quake/timedemo`, the `pause` row above), `SCR_ModalMessage`'s New
  Game question (it blocks in a key loop the null input driver never ends; its
  text goes through the fixed `center_string_top`), the attract demo's HUD (the
  same drawing code as the live one), the crosshair (off by default).

## Sound (`sound.py`)

id's mixer — `snd_dma.c`, `snd_mix.c`, `snd_mem.c` and `mathlib.c`'s
`VectorNormalize`, unedited apart from the `id386` switch — built headless
around `c/snd_oracle.c`, and the port's (`quake_rs::snd::Mixer`) run the
same scripted calls; their PCM is compared sample for sample.

```sh
oracle/build_sound.sh              # once (~5 s, docker); sound.py also rebuilds when snd_oracle.c changes
uv run oracle/sound.py             # 7 scenarios x 11025/22050/44100/48000: a table, exit 1 on any difference
uv run oracle/sound.py --fixes     # plus how far the 2026 mixer (every snd::Fixes) departs from id's
uv run oracle/sound.py --sse       # the C built with SSE2 floats (ORACLE_FPMATH=sse) instead of x87
```

**How.** `snd_oracle.c` supplies what the mixer calls: a fake DMA driver
(a 32768-pair 16-bit stereo ring whose play position the script moves;
`SNDDMA_Submit` appends every newly painted pair to the output), cvars
(id's `Q_atof`), the pak, the cache, `cl.viewentity`, a listener leaf the
script sets (`Mod_PointInLeaf`), and `rand` linked as `__wrap_rand`: the MSVC
runtime's generator WinQuake.exe used (seed 1), which the port's mixer draws
from too. A script (the commands are listed in `snd_oracle.c`'s header) is
sound calls as `CL_ParseStartSoundPacket`/`CL_ParseStaticSound` make them
(volume and attenuation as their wire bytes), the listener, its leaf, cvars,
`update` (`S_Update`, then `S_Update_` mixing `_snd_mixahead` ahead of the
play position) and `advance N` (the play position moves N pairs).
`quaketool sndscript` runs the same script through the port. Both also write
a trace: every sounding channel after each `update` (volumes, position, end).

**Scenarios.** `oneshots` (sounds all around a turning listener: distance,
pan, volume and attenuation bytes, the view entity, channel 0 and
same-channel overrides, more sounds than the 8 dynamic channels, one sample
twice in a frame, the 16-bit samples), `statics` (two levels of placed loops,
combined, volumes past 255, a refused one-shot and a missing sample),
`ambients` (leaf levels, `ambient_level`/`ambient_fade`, 72 and 60 fps, no
leaf), `loops` (door and lift hums over many laps, stopped by their stop
sounds; a 0.19 s stall that the play position overtakes), `stops`
(`S_StopSound` over every dynamic channel, `stopall`, `_snd_mixahead`),
`cvars` (`volume` past the clamp, `loadas8bit`, `nosound`), `soak` (a minute
of everything at random).

**Result (2026-09-26, x87 build).** Every scenario at every rate is
identical: PCM and trace, 28 of 28 cases (20.8 M sample pairs). Against the
SSE build the port stays identical at 11025/22050/44100 and at 48000 except
where `ResampleSfx`'s `stepscale` rounds differently as a `float` (the x87
build holds it in an 80-bit register; the port follows x87 with `f64`):
there the traces of three 48 kHz cases differ, and the PCM of one. With `--fixes`, the 2026 mixer differs from
id's where the fixes act: every sample at 48 kHz (exact resampling), and at
id's rates on loop laps (the seam), in the ambient ramp at 60 fps, and in the
`stops` scenario (`S_StopSound`'s range).

**What the port has to do to match.** Mostly nothing beyond porting the C
literally: every mixing step is integer. The float steps are the volume byte
(`fvol*255`, an exact product on the x87, then truncated), `SND_Spatialize`
(the x87 keeps the length, the dot product and the `(1 - dist) * (1 ± dot)`
scale in extended precision and stores the scale as a `float` before its
multiply; the port uses `f64` for the first and `f32` for the second, which
the traces confirm), `S_UpdateAmbientSounds`' ramp (`host_frametime *
ambient_fade` in extended precision; the scripts use frame times whose step
is not within a hair of a whole number, where 64 and 80 bits could part),
and `ResampleSfx`'s `stepscale` (above). The output stream skips a stretch
the play position overtook (`S_Update_`'s "overshot" reset), on both sides.

### The game's calls (`sound_walk.py`)

`sound.py` hands both mixers the same calls; `sound_walk.py` checks that the
game makes the same calls. One scripted walk runs through id's whole game
(this oracle, its sound log on: `walk_oracle.c`) and through the port's Classic
client (`quaketool sndwalk`), and every call each makes into the sound layer is
compared, walk frame by walk frame.

```sh
uv run oracle/sound_walk.py                  # every case: a timeline each, exit 1 on a difference
uv run oracle/sound_walk.py --instant-load   # id's loads take no time (the oracle's own clock)
```

**How.** Both sides start from a fresh `map`, run a host frame every 1/72 s,
run at id's Always Run speed, and take the script's next frame in each host
frame that sends a move (`CL_SendCmd` with `cls.signon == SIGNONS` on id's
side), so a level change's signon frames, which send none, keep the walk in
step. id's level loads take a second of its clock (`-oracle_loadtime 1`), as
they took seconds on id's machines. A script turns only in steps of 45
degrees: a yaw crosses id's wire as a byte, and the port's server takes it
unrounded. Compared exactly: the player's and the teleport fog's sounds
(class, channel, sample, the volume and attenuation bytes, the position as the
client has it; any `misc/r_tele1..5` matches any, `play_teleport` picks one
with `random()`), `S_StopAllSounds`, the levels' `S_StaticSound` loops,
`S_StopSound`, `S_LocalSound`, and the player's path. Other sounds are the
world's: monsters, and what they set off, on `random()` (id's `rand()` is
stirred every host frame; the port draws from its own streams), listed and not
compared. The log also lists the sounds id's server started that its client
never got.

**`telegate`** walks the start map from its spawn down the NORMAL skill hall
into its teleporter, across the hub, west down the first episode's hall and
into its slipgate (`trigger_changelevel` *14, `spawnflags` 1), then stands a
second in e1m1. **Result (2026-10-02): every call identical** (11 compared:
the land thud, four `misc/talk.wav` from the halls' message triggers, the two
teleport fogs 0.2 s after the teleport, both loads' `S_StopAllSounds` and
loops), and the path identical but for the first 8 frames of each level.
What it shows:

- **The slipgate makes no sound, in id's game or the port's.**
  `changelevel_touch` starts none (`SUB_UseTargets` on a trigger with no
  target and no message; `GotoNextMap`; `changelevel`), and the arrival's
  teleport fog is deathmatch and coop only (`PutClientInServer`:
  `if (deathmatch || coop) spawn_tfog`). The teleport sound on this walk is the
  skill hall's: `teleport_touch`'s two fogs, where the player stood and at the
  hub, 0.2 s after.
- **The cut.** id's: the frame after the touch runs the `changelevel` the
  touch queued (`Cbuf_Execute`), and `Host_Reconnect_f`'s
  `SCR_BeginLoadingPlaque` calls `S_StopAllSounds (true)`: every channel goes,
  the loops and ambients with them, and `S_ClearBuffer` erases the 0.1 s
  already mixed ahead. The load is silent; the server's stuffed `reconnect`
  stops everything again in the frame after it; e1m1's 14 loops start one
  frame later still (signon 1), heard from the world's origin until signon 4
  (`S_Update`'s zero listener: of e1m1's loops only `ambience/comp1.wav`, 325
  units off, is faintly audible from there), and the walk's next move comes 3
  frames after them. The port's load fits in the touch
  frame: it stops everything and starts e1m1's loops there. So the start map's
  sound ends one frame (1/72 s) sooner than in id's game, and e1m1's loops
  follow at once, where id's left the load's silence between. A sound started
  in the touch frame itself would play for that one frame in id's game and not
  at all in the port's: there is none on this walk.
- **Fixed on the way:** a sound's position now reaches the mixer as id's client
  had it, through `MSG_ReadCoord` (1/8 unit, toward zero: the fog where the
  player stood is at y 1352.375, not 1352.469), for `S_StartSound`, the
  temp entities' sounds and `S_StaticSound` (`server::wire_coord`).
- **Not sound, measured on the way.** Walk frame 0 is at `cl.time` 1.4278 in
  id's game and 1.4000 in the port's, and e1m1's first at 1.4417 and 1.4000
  (with `--instant-load`, id's 1.3417 and 1.3556): the port connects the player
  at `sv.time` 1.2 and runs two 0.1 s signon frames after it, where id's
  connects it at about 1.4 and its signon frames are the host's (AUDIT.md's
  open list); so id's player is still falling from its spawn spot for the
  first 8 frames. e1m1's sliding door (`doors/hydro1.wav`) opens
  when the soldier patrolling past it walks into its trigger field, and the
  soldier sets off `random()`*0.5 s into the level (`walkmonster_start`): in
  id's game the door opened at `sv.time` 1.1, 1.3 or 1.54 as the stand before
  the walk changed (the first two in the signon, never heard), so it is the
  world's.

## Classic (`classic_check.py`)

```sh
uv run oracle/classic_check.py                        # about a minute once built; exit 0 = Classic is id's
uv run oracle/classic_check.py --only goldens,play    # some of it
uv run oracle/classic_check.py --record --note "..."  # re-record, saying why
```

The port's Classic profile (`quake_rs::settings`: every engine departure off; the
controls are the player's, shared with 2026, and the harnesses pin id's own by name:
`Settings::id`, `idcontrols`) must stay WinQuake. One command runs every check of
that and writes a report (`oracle/build/classic-check/classic_check.txt`, next
to each tool's own output):

| check | what | against |
|---|---|---|
| `goldens` | `quaketool scene` of e1m1/e1m2/e1m3 | the sha256 prefixes `4807aaa1` / `9ae2b478` / `c65b7046` |
| `play` | `quaketool play`: the browser's client frames natively, id's three demos and four scripted walks at 320x200, 640x400 and 960x600, a hash every 30 frames and the sound-call tallies | the recorded list |
| `timedemo` | id's `timedemo` of demo1..3 at 320x200 and 640x400: the frame counts (969 for demo1, as id's C) | the recorded list |
| `census` | `quaketool census`: all nine maps through the real QuakeC (the report, by hash) | the recorded list |
| `edicts` | id's server edicts (this oracle) diffed against the port's, nine maps at t = 1.7 / 4.7 / 10.7 s (`census/`): the diff report, by hash. What it still shows: each matched entity's number one below id's (the player is the port's last edict, CENSUS L25); monsters' random idle frames and wandering; a door pair on e1m6 caught at another point of its slide at 1.7 s; the fireballs and bubbles random numbers start. The statics' rows are gone since `fleet/makestatic` (1,185 rows to 606) | the recorded list |
| `oracle` | `compare.py --aspect 0.8333333 --spans 16`: the eight standard rows | id's C: none below its recorded match (100.00%; e1m7 99.9969%, two pixels) |
| `screen2d` | `screen2d.py`, 320x200 and 640x400, the port in its Classic profile | id's C: no shot below its recorded `2d exact%` (the residues above) |
| `demolerp` | `demo_lerp.py`: id's client against the port's over the attract loop, frame by frame — the camera, the entities and the dynamic lights (below) | id's C: every demo MATCH |
| `sound` | `sound.py`: id's mixer against the engine's `Fixes::NONE`; `sound_walk.py`: a walk through id's game and the port's | id's C: every case sample-identical; every call the walk makes identical |

The recorded list is `oracle/classic_expected.txt`, with a note for each
recording (first on `a50d8d7`, the settings branch's base). A change that
moves an identity value on purpose is re-recorded with `--record --note`,
and says so where the fidelity change is recorded (AUDIT.md).

**Last run** (branch `q26/docs` on `244bcd5`, the end of the 2026 push,
2026-09-26; about a minute with everything built):

```
PASS  goldens      0.1 s  3 values match
PASS  play        12.5 s  42 values match
PASS  timedemo     3.9 s  3 values match
PASS  census       1.2 s  1 values match
PASS  edicts       2.1 s  9 values match
PASS  oracle       0.9 s  8 values match
PASS  screen2d    10.4 s  146 values match
PASS  demolerp    20.0 s  0 values match
PASS  sound        3.3 s  0 values match
ALL PASS
```

"Values match" counts the values compared with `classic_expected.txt`. The
`demolerp` and `sound` rows record nothing: they compare the port with id's C
live, in the same run (every demo MATCH, every case sample-identical), so their
count is 0 and their PASS is the live comparison's. The `oracle` and `screen2d`
rows do both: the live comparison with id's C, checked against the recorded match
of each row.

## Demo playback (`demo_lerp.py`)

What id's client draws between two recorded messages (`CL_LerpPoint`,
`CL_RelinkEntities`), frame by frame. The oracle plays the attract loop from
boot with every host frame exactly the port's 1/72 s step (`-oracle_dt`),
and `oracle_trace path [frames]` writes one record per rendered frame as
`R_RenderView` starts: `cl.time`, `cl.oldtime`, `cl.mtime[0..1]` as
`CL_LerpPoint` left them, `cl.viewangles`, the view entity's origin,
`cl.velocity`, every entity on `cl_visedicts` (number, model, origin,
angles, frame) and every dynamic light `R_PushDlights` marks (pool slot, key,
origin, radius, `die`, decay, minlight). `quaketool play demo1 N --trace PATH` writes the same from
the port's client; the script runs both, splits the traces at each demo's
first frame and compares them.

```sh
uv run oracle/demo_lerp.py                  # demo1, demo2, demo3, demo1 again: 17,500 frames
uv run oracle/demo_lerp.py --frames 2000 --keep DIR
```

Result (2026-09-26, `q26/lerp`): 17,500 frames over the whole loop, the
same frame count per demo, `cl.time` identical in every frame, camera,
velocity and every entity within 2.5e-4 (the oracle's x87 floats), the same
entities everywhere.

**Dynamic lights** (2026-10-03, `fleet/demolights`). The trace also carries the
lights `R_PushDlights` marks (`D` lines: slot in `cl_dlights`, key, origin,
radius, `die`, decay, minlight), and `demo_lerp.py` compares them: in every one
of the 17,500 frames the same slots hold the same lights — key, origin (within
the position tolerance), decay, minlight and `die` (to 1e-4: a float made from a
double clock) — 618 + 73 + 1,055 + 399 explosion light-frames, 1,467 + 1,174 +
589 + 180 muzzle-flash ones and 8,184 rocket ones, the view entity's flashes
included. The radius is exact for explosions (350, decaying 300 a second) and
rockets (200); a flash's `200 + (rand()&31)` is held to its window, which is all
two generators can share (id's `rand()` is stirred every host frame, so id's own
draws differ from run to run).

The first run differed in 36 frames, all one cause: `dl->die` is a float and
`cl.time` a double, and in a demo `cl.time` is the host's running sum, so an
explosion's last frame (0.5 s on, a whole number of 1/72 s steps) turns on bits
a float clock does not have; the pool takes `cl.time` as an `f64` now.

### Demo lights in pixels (`demo_lights.py`)

```sh
uv run oracle/demo_lights.py                  # twelve standard frames: a table and 4-up images
uv run oracle/demo_lights.py --sample 40      # + 40 lit frames spread over the loop
uv run oracle/demo_lights.py --frames demo1:323,demo3:371
```

id's side plays the demo at 1/72 s and shoots frame K of it (`compare.py --demo
demoN --settle K --oracle-dt 0.01388888899236917`, the 3-D view at 320x200 and the
page's aspect, the weapon drawn, id's light styles handed over); the port draws the
same view from id's camera, clock, entities and particles, lit three ways
(`--dlights`): by none, by the lights its own playback makes at that clock, and by
id's own `cl_dlights`. Result, 51 frames (the 12 standard and 39 spread over the
three demos; in 39 of them the lights change more than 1% of the frame), exact% of the 3-D view:

| frames | no lights (before) | the port's playback lights (after) | id's lights |
|---|---:|---:|---:|
| 26 whose lights have exact radii (explosions, rockets) | min 3.72, median 95.55 | min 99.96, median 100.00 | min 99.96, median 100.00 |
| 25 with a muzzle flash (`rand()&31`) | min 18.17, median 65.76 | min 44.53, median 90.54 | min 99.83, median 99.99 |

With exact radii the port's playback frame is id's frame to the pixel the renderer
reproduces (the 0.04% left is the renderer's: the same in both columns, where
nothing about a light is involved). A flash's radius is the port's own draw, and a
different radius moves the colormap rows of every lit pixel: the match falls with
the distance between the two draws (6 frames within 2 units: median 99.63, worst
92.20; 11 frames 9 to 16 apart: median 84.41), and with id's own radius handed over
(the last column) it is 99.83 to 100.00 every time. Single frames, before / after,
saved by the run: `demo1:323` (the player's shotgun flash) 75.65 -> 91.34 (id's
radius 218, the port's 211) -> 100.00; `demo1:358` (a grenade explosion on the frame
it is made) 14.42 -> 100.00; `demo1:376` (the same, fading, radius 275) 52.13 ->
99.99; `demo1:601` (two explosions at once) 46.23 -> 100.00.

# film/: the film's game footage

The explainer film about quake-srp shows the game through the engine's own camera: its
game footage is `quaketool film` running id's shareware episode, a map or a demo played
live, the camera placed by a shot file. This folder holds the shot files of the film's
current cut and one command that renders them all again, so anyone with the repository
and the shareware pak can make the footage themselves.

## Render it

You need what [Build and run it](../README.md#build-and-run-it) asks for (the pak at
`quake-data/ID1/PAK0.PAK`, Rust, `uv`) and ffmpeg with libx264.

```sh
uv run film/render.py                  # every shot, into film/out/
uv run film/render.py S01 ST1a-on      # only these
uv run film/render.py --png S10        # numbered PNG frames instead (no ffmpeg needed)
uv run film/render.py --list           # each shot and what it shows
```

The script builds `quaketool`, then runs `quaketool film` on each shot file in turn and
pipes the frames into ffmpeg. `film/out/NAME.mp4` is H.264 at the shot's own size and
frame rate (CRF 16, 4:2:0, BT.709), the film's master format. Beside it go the shot's
game sound, `NAME.wav` (the engine's own mixer: id's at 11025 Hz in Classic, 48000 Hz in
slop), and `NAME.events.json`: each sound started, muzzle flash and monster pose change,
with its frame, for the edit to cut on. One shot by hand:

```sh
quake-rs/target/release/quaketool film quake-data/ID1/PAK0.PAK film/shots/S10.shot out/S10
```

That writes `out/S10/00000.png` on (`--raw` writes raw frames to stdout instead, for
ffmpeg), and any shot line can be given last as an override (`--preset classic`, `--size
960x540`).

A shot gives the same frames on every run and on any number of threads (except `xray
bands`, whose colours are the threads). Until the renderer itself changes, a shot
rendered again gives the very frames the film was cut from (`S13-segments` aside: its
x-ray has changed since; the file says how).

## The shot language

A shot file says what to film, one setting a line; a `#` that starts a word starts a
comment. [`S07.shot`](shots/S07.shot), the cold open's wireframe dissolving into e1m1's
big room, is this under its comments:

```
map e1m1

duration 7.5
fps 60
size 1920x1080

preset classic
mode 320x200
hud off
cvar gamma 0.7

camera path
key 0    -208,2736,192  20,229  ease linear
key 7.5  -208,2736,192  20,199

xray black
wire all
mix 0    1
mix 2.5  1
mix 3.5  0

events on
```

`quaketool film --help` prints the whole format
([shot_format.txt](../quake-rs/src/bin/quaketool/film/shot_format.txt)). In short:

- **The world.** `map e1m1` runs the map live: its monsters, doors, lifts, lights and
  torches, through the real QuakeC. `demo demo1 from 12.8` plays id's recording instead.
  `map gen:grazing` is a map made for the film where id's own cannot show a point
  cleanly: a long hall of id's base, its walls for seeing at a grazing angle, written as
  a BSP when the shot starts ([`mapgen.rs`](../quake-rs/src/bin/quaketool/film/mapgen.rs))
  and loaded like any other; `quaketool mapgen` writes it to a file.
  `warmup` runs the game before the first frame; `wake X,Y,Z at T` wakes the nearest
  monster, its enemy the player.
- **What the player does,** at film seconds: `impulse 9 at 0`, `attack on at 0.5`,
  `cmd +jump at 2.0`, `fire t4 at 1.9` (a trigger's targets used, as the game uses them).
- **The film.** `duration`, `fps`, `size`, and `frames A..B`: a window of a longer shot,
  its clock unchanged, so two renders of one move stay in step.
- **The game's settings.** `preset slop` or `preset classic`, then any console variable,
  `cvar r_perspspan 16`, from a film second on if it says so: `cvar r_torchflicker 1 at
  7.8`. `mode` and `display` set the picture's pixels and shape; `hud`, `gun` and
  `crosshair` take `at T` too.
- **The clock and the screen.** `display 60` films a 60 Hz screen: on each refresh the
  game's own frame gate decides, as the browser's does, whether a game frame runs, and the
  screen shows the last picture drawn. `clock id` is id's gate, its 72 fps cap (Classic's);
  `clock free` is slop's, a frame every refresh. On a 60 Hz screen both draw every refresh;
  on a 240 Hz one id's gate draws every 4th. `speed 0.25` is slow motion and slows the
  screen with the world: `display 240` and `speed 0.25` in a 60 fps film is a 240 Hz
  screen at quarter speed. Without `display` the film is the screen: `clock free` is a
  game frame for every film frame (at `speed 0.125` and 30 film frames a second, the game
  runs at 240 Hz), and `clock id` is id's 72 Hz ticks, each picture held to the film
  frames after it, so a 60 fps film drops one tick in six, which no screen shows. `speed
  0` freezes the world while the camera goes on.
- **The camera.** The player's eye or the demo's; `camera fixed`; `camera path` through
  `key` lines (a smooth curve through the keys, each segment eased as its key says);
  `camera follow` and `camera orbit` an entity (the game is rehearsed once, undrawn, so
  the camera knows where it goes); `aim` turns a path, fixed or orbit camera to one.
- **X-ray.** The renderer's machinery, drawn from what it decided for the frame: `xray z`
  (the z-buffer), `spans`, `segments` (each span's affine runs and its perspective
  divides), `mip`, `cache` (the surface cache's blocks), `leaves` (the BSP leaf of each
  visible point), `lightmaps`, `error`, `bands` (each thread's rows), and more; `wire`
  draws polygon edges, `vis` another point's potentially visible set, `divides` a mark at
  every perspective divide. `mix` fades an x-ray in and out of the picture.
- **On and off in one pass.** `ab cvar r_torchflicker 0 | cvar r_torchflicker 1` renders
  the shot twice, stepped together, and `split` lays both in one frame: a wipe that can
  move, side by side, stacked, or a diff.
- **Marks.** `mark NAME X,Y,Z` or `mark NAME entity ENTITY` writes where a point lands on
  each frame, and whether it is seen, into `events.json`, for an edit's callouts.
- **The sound.** `sound on`: each sound starts with the frame that shows its cause and
  plays at its own speed, in slow motion too. `sound game`: the mixer runs on the game's
  clock, slowed with it.

The code is in [`quaketool/film/`](../quake-rs/src/bin/quaketool/film/):
[`mod.rs`](../quake-rs/src/bin/quaketool/film/mod.rs) (how the game's clock meets the
film's), [`screen.rs`](../quake-rs/src/bin/quaketool/film/screen.rs) (the screen
`display` watches, the game's gate on each refresh),
[`shot.rs`](../quake-rs/src/bin/quaketool/film/shot.rs) (the format),
[`camera.rs`](../quake-rs/src/bin/quaketool/film/camera.rs) (paths, follow, orbit),
[`xray.rs`](../quake-rs/src/bin/quaketool/film/xray.rs) and the renderer's side,
[`render/xray.rs`](../quake-rs/src/render/xray.rs),
[`events.rs`](../quake-rs/src/bin/quaketool/film/events.rs),
[`mapgen.rs`](../quake-rs/src/bin/quaketool/film/mapgen.rs) (the maps made for the film) and
[`marks.rs`](../quake-rs/src/bin/quaketool/film/marks.rs). The world is the browser's own
client, run natively, with only the camera the film's.

## The shots

Each file in [`shots/`](shots/) is one render. Its first lines say what it shows (most
quote the narration it plays under) and, where the film lays several renders together,
how ("Assembly"). The conventions:

- Most shots run a second longer at each end than the film uses: handles for the edit.
  "File second" counts from a render's first frame.
- id's Brightness slider (`cvar gamma 0.7` or `0.8`) lifts the darker shots, as a player
  would, rather than a colour grade in the edit.
- A name with a suffix is one render of several: `ST1a-off` and `ST1a-on` are the
  torches as id baked them and flickering, wiped together in the film.

| In the film | Shot files | What they show |
|---|---|---|
| The cold open | `S01` | e1m1's first corridor, as 1996 drew it |
| | `S02`, `S04`, `S05` | demo1, demo2 and demo3 in Classic: a grenade, a Scrag, the rocket launcher |
| | `S03`, `S06` | e1m8's ziggurat in its lava; the Quad Damage turning |
| | `S07`, `S08`, `S09`, `S10` | x-rays: the wireframe, the BSP leaves, the potentially visible set, the surface cache |
| | `S13-game`, `S13-segments` | id's 16-pixel perspective segments, sweeping in |
| The idea | `S15-classic`, `S15-slop` | the 4:3 box opening to 16:9 |
| | `S16-S17-classic`, `S16-S17-slop` | the start map: Classic wiped to slop, and Classic alone |
| The proof | `S19` | e1m7 frozen, settling on the oracle's view |
| | `S26`, `N1` | demo1 in Classic, its sound id's mixer |
| The torches | `ST0-TQ1`, `ST1c` | e1m3's flame, steady, then flickering |
| | `ST1a-off`, `ST1a-on`, `ST1b-off`, `ST1b-on` | torches steady against flickering |
| | `LAB1-off-light`, `LAB1-on-light`, `LAB1-off`, `LAB1-on` | the same, the light alone, then textured |
| Gliding lights | `ST2`, `S48-id`, `S48-slop` | a flickering light style, in real time and at 1/16 |
| Smooth motion | `S45-id`, `S45-slop` | a grunt at 240 Hz: id's steps against slop's glide |
| | `S46a-held`, `S46a-blended`, `S46a-track` | its poses held against blended |
| | `ST3` | demo1's zombies, the options so far on |
| No frame cap | `LAB4a-id`, `LAB4a-uncapped` | id's 72 Hz ticks on a 60 fps film (no `display`: a judder no screen shows) against a frame every refresh |
| | `LAB4a2` | a 240 Hz screen at quarter speed: id's gate draws every 4th refresh, against every refresh |
| | `N3c-turn`, `N4` | a run at 480 frames a second; jumps and grenades |
| Fluid sky | `LAB5-id`, `LAB5-slop` | id's clouds against slop's |
| Native pixels, horizontal plus | `BURST-a`, `BURST-b` | the Classic box bursting to native 16:9 |
| | `S38-S41-4x3`, `S38-S41-id`, `S38-S41-horplus` | 4:3, id's 16:9, horizontal plus |
| The status bar, the crosshair | `ST7` | the bar scaling, the world beside it, the crosshair |
| Nails from the barrels | `N8a`, `N8b`, `F13-id`, `F13-barrels` | id's nails, then nails from the barrels |
| The mixer | `N9` | the computer corridor: the picture (for the sound, see below) |
| And the rest | `LAB10a`, `F03` | each thread's bands; the z-buffer |
| | `N3c-turn`, `N7a`, `F06`, `N7b`, `N7b-2026` | speed, mouse-look, a gamepad's hit, the WASD keys of 1996 and 2026 |
| The perspective | `N2a`, `N2b`, `N2c`, `PER4` | a turning wall: id's span, its divides, slop's, the span flipping |
| | `S33-S34-64`, `S33-S34-16`, `S33-S34-8`, `S33-S34-exact` | four spans side by side |
| | `N2e` | exact at every pixel |
| All of it, on | `HERO` | demo3 in full slop |
| How it was built | `N10`, `S59`, `S60` | the start map's hall; the Shambler |
| The close | `S61` | Chthon rises from the lava |
| | `S62-0` to `S62-8` | the options switched off one by one, back to Classic |
| | `S63` | the 1996 corridor again |

## What is not here

- **The assembly.** Side by side, wipes, crops that follow a monster, dissolves, labels,
  the keycaps and the gamepad were laid out by the film's own editing scripts, not by
  the engine. Each shot file says in words how its renders were laid together, enough
  to do it again; the tool's `ab` does an on and off comparison in one render, though
  not these exact layouts.
- **The browser.** The Slop Options menu, the page loading and the phone's touch
  controls were filmed from the browser build, not with the film tool.
- **The proof's frames.** id's C beside the port on four maps, and the difference
  images, are the oracle's: `uv run oracle/compare.py --maps e1m1,e1m2,e1m3,e1m7 --modes
  world --aspect 0.8333333 --spans 16` makes them. It builds id's C, which needs id's
  source and docker ([oracle/README.md](../oracle/README.md)).
- **The mixer's click.** That beat's sound is one looping hum mixed by id's mixer and by
  slop's, outside the film tool; `N9` is only the picture under it.
- **The bands' colours.** In `LAB10a` which thread takes which band is the scheduler's,
  so the colours come out in another order on another run.
- The diagrams, the narration, the music and the edit itself.

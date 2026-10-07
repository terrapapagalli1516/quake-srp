# The cut's shots

Every shot of the film, in cut order, and what makes it. A shot file's header says what it
shows, the narration it plays under and, where the film lays several renders together, how.
`uv run film/render.py NAME` renders `NAME.shot` as `NAME.mp4`, the name the edit reads,
wherever one render is one footage file. Labels, captions, counters, diagrams and the options
ladder over the shots are the edit's, not the shot files'.

[`sidecars/`](sidecars/) holds what the edit and the sound effects read beside each footage
file `NAME.mp4`: `NAME.json`, with its handles (`head_s`, `tail_s`), its length (`shot_s`),
its game sound, its events log, and the times of what happens in it, in shot seconds
(`*_shot_s`: a grunt's steps, a light's letter changes, a nail's sounds). Copy them beside
the renders.

| Cut | Made from | What it shows |
|---|---|---|
| S01 | `S01` | e1m1's first corridor as 1996 drew it |
| S02 | `S02` | demo1 in Classic: a grenade |
| S03 | `S03` | e1m8's ziggurat in its lava |
| S04 | `S04` | demo2: a Scrag overhead |
| S05 | `S05` | demo3: the rocket launcher |
| S06 | `S06` | the Quad Damage turning |
| S07 | `S07` | a wireframe dissolving to the textured room |
| S08 | `S08` | each visible point's BSP leaf |
| S09 | `S09` | the potentially visible set, from above |
| S10 | `S10b` | the light alone, then light times texture |
| S11, S12 | — | diagrams |
| S13 | `S13-game`, `S13-segments` | id's 16-pixel segments sweeping in (see below) |
| S14 | — | card: the title |
| S15 | `S15s` | the game's first view, slop |
| S18a, S18b | `capture/s18.py` | the Slop Options menu, in the browser |
| S16 | `S15s`, `S15c` | the same view, the box closing: Classic |
| S20, S21 | `oracle/compare.py`, below | id's C beside the port on four maps, monsters in view |
| BD1–BD3 | — | cards made from the same frames (BD2 is black) |
| S26 | `S26` | demo1, id's mixer; its five lines are the edit's |
| S27 | `N1b` | demo1 in Classic, under the ten checks |
| ST0, TQ1 | `STG` | the gable flame, every option off; the torch wakes |
| ST1a | `ST1a2` | torches baked against flickering, a wipe |
| ST1b | `ST1b-off`, `ST1b-on` | e1m2's arch torches, side by side |
| LAB1 | `LAB1-off-light`, `LAB1-on-light`, `LAB1-off`, `LAB1-on` | the light alone, then textured |
| ST1c | `ST1cG` | the gable flame close, flickering |
| ST2 | `ST2` | light style 10 in real time |
| LAB2 | `S48-id`, `S48-slop` | style 10 at 1/16: steps against glide |
| LAB3a | `S45-id`, `S45-slop` | a grunt at 240 Hz: steps against glide |
| LAB3b | `S46a-held`, `S46a-blended`, `S46a-track` | poses held against blended |
| ST3 | `ST3.g065` | demo1's zombies, three settings on |
| LAB4a | `LAB4a3` | id's 72 fps cap on a 240 Hz screen |
| LAB4b | `N3w-box` | a run at 480 Hz, eight frames to a frame |
| LAB4c | `N4-box` | jumps and grenades, no frame cap |
| LAB5 | `LAB5-id`, `LAB5-slop` | clouds that jump against clouds that drift |
| BURST | `BURST2r-a`, `BURST2r-b` | the 4:3 box bursting to 16:9 |
| LAB6a, LAB6b | `S38-S41-4x3`, `S38-S41-id`, `S38-S41-horplus` | 4:3, id's 16:9, horizontal plus |
| ST7 | `ST7` | the status bar scaling, the crosshair |
| LAB8a, LAB8b | `N8a`, `N8b` | id's nails, at 1/4 and at real speed |
| LAB8c | `F13-id`, `F13-barrels` | nails from the barrels |
| LAB9 | `LAB9b`; sound below | e1m8's rune gate; the mixer's click |
| PER1 | `N2a` | a turning wall at id's span |
| PER2 | `N2b` | every divide marked |
| PER3 | `N2c` | the span at 8 |
| PER4 | `PER4` | the span flipping, 64 to exact |
| PER5 | `S33-S34-64`, `S33-S34-16`, `S33-S34-8`, `S33-S34-exact` | four spans side by side |
| PER6 | `N2e.clean` | exact at every pixel |
| HERO | `HERO7` | the fight at e1m3's end, every option on |
| LAB10m | `capture/s18.py` | the Options page |
| LAB10a | `LAB10a2` | each thread's bands (see below) |
| LAB10c | `capture/f01.py` | the page loading |
| LAB10d | `LAB10d` | a live run on e1m2 |
| LAB10e | `N7a` | mouse-look |
| LAB10f | `F06` | a chainsaw's hit and a gamepad |
| LAB10g | `capture/f05b.py` | a phone's touch controls |
| LAB10h | `N7b`, `N7b-2026` | WASD in 1996 and in 2026 |
| S57, S58 | `N10` | the start map's hall |
| S59 | `S59b` | the start map's wireframe, orbiting |
| S60 | `S60` | the Shambler |
| S61 | `S61` | Chthon rising |
| S62 | `S62-0` … `S62-8` | the options switched off one by one |
| S63 | `S63` | the 1996 corridor again |
| S64 | — | card: the end |

`capture/*.py` are the browser captures' scripts: Playwright recording the page build. They
and their recordings are not in the repository.

**S20, S21: the proof frames.** Not a film shot: `oracle/compare.py` in its `ents` mode, one
view a map at id's own clock, monsters awake. Once the player is in the game, id's console
moves him to the camera, so the monsters near it wake, turn and attack; the frame is
`--settle` frames after signon, and the port draws id's entity list, particles and dynamic
lights of that frame. For e1m1:

```sh
uv run oracle/compare.py --maps e1m1 --modes world,ents --aspect 0.8333333 --spans 16 --sse \
  --view=596.7,2808,-34,4,180,0 --settle 15 \
  --c-post wait --c-post wait --c-post wait --c-post wait --c-post wait --c-post wait \
  --c-post noclip --c-post god --c-post 'oracle_field origin 596.7 2808 -56' --out DIR
```

The others change only the map, `--view=`, `--settle` and the origin: e1m2
`1543,1385,226,4,270,0`, 19, `1543 1385 204`; e1m3 `1283.8,912.2,582,4,315,0`, 11,
`1283.8 912.2 560`; e1m5 `160.7,2549.3,446,4,0,0`, 15, `160.7 2549.3 424`. All four come out
at 0 pixels differing. Every proof command needs `--sse`: the pixel checks run id's C in its
SSE2 build. The film's S21m lays each map's `e1mN_ents_320x200.c.ppm` and `.port.ppm`
side by side at 880x660 (nearest), "id's C" and "the port" above them and the map's name
below, for 1.95 s each from shot second 0 (e1m1, e1m2, e1m3, e1m5); under them, 0.5 s after
each map appears, the difference (the largest channel's difference times 4, 360x270, in a rust
frame) fades in over 0.4 s. A second of handle each end, the head e1m1 without its difference.
BD1 and BD3 draw the same frames.

**LAB9's sound: the mixer's click.** Not a film shot: one `ambience/hum1.wav` loop beside a
still listener, run through id's mixer and through slop's by `quaketool sndscript`. The script
for a rate, written one line each:

```python
lines = ["viewent 1", "leaf 0 0 0 0", "listener 0 0 0 0.000000000 1.000000000 0 1.000000000 -0.000000000 0",
         "frametime 0.013888888888888889", "start 30 2 ambience/hum1.wav 0 64 0 255 64"]
clock, pairs = 0.0, 0
for f in range(300):            # 300 frames at 72 Hz
    clock += 1 / 72
    target = int(clock * rate)
    lines += ["update", f"advance {target - pairs}"]
    pairs = target
```

Then:

```sh
quaketool sndscript quake-data/ID1/PAK0.PAK hum1-11025.txt id.raw --rate 11025            # N9-click-id
quaketool sndscript quake-data/ID1/PAK0.PAK hum1-48000.txt slop.raw --rate 48000 --fixes  # N9-click-slop
```

Each `.raw` is 16-bit stereo at its rate, 4.25 s, kept as a WAV. In id's, the channel falls
silent for 65 samples at 3.62 s: the click. The edit lays slop's under the voice, id's alone for
two laps, then slop's again.

**The game's own eye.** Since the pixel-exact work, `camera walk`, `camera player` and a
demo's camera draw id's eye nudged 1/32 unit, as id's V_CalcRefdef does. The cut was rendered
before that, so these shot files reproduce their shots with slightly different frames (about
5.5% of the pixels in HERO7 and the walks): F06, F13-barrels, F13-id, HERO7, LAB10d, N1b,
N3w-box, N4-box, N8a, N8b, S02, S04, S05, S15c, S15s, S26, ST3.g065 and ST7. Shots with the
film's own cameras (`path`, `fixed`, `follow`, `orbit`) differ from the cut only in single
pixels at texel edges and on liquids (at most 0.13% of a shot's pixels in those checked at
the merge: the pixel-exact work), and their x-rays line up as filmed.

**S13** was rendered by the first version of the film tool. Its `segments` x-ray has changed
since, so `S13-segments` rendered today differs from the film's S13 once the sweep begins; the
film keeps its own file. **LAB10a2**'s band colours follow the thread scheduler, so they come
out in another order on every run; the pixels under them do not change.

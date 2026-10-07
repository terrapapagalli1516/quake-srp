# film/: how the explainer film is made

quake-srp has an explainer film: a narrated tour of the port that opens on id's 1996
corridor, proves the port against id's own C, switches the slop options on one at a time
until the full slop picture, says how the code was built, and switches everything off again
on the way out. Everything in it comes from this repository and id's shareware pak:

- the game footage is the engine's own: `quaketool film` running the shareware episode,
  a camera placed by a shot file;
- the proof's frames are the oracle's: id's C beside the port;
- the diagrams and the edit's cards are drawn in Quake's palette and lettering;
- the score is synthesized from code (no samples);
- the sound effects are id's own sounds, and a few designed in code.

Only the voice came from outside: ElevenLabs' text to speech, the voice "Frederick".

This folder is the film's source: what it says, what it shows, and how the pieces become the
film, as text and code. The media are not here ([The media](#the-media)).

| | |
|---|---|
| [`script.md`](script.md) | the narration, line by line, each with where the repository says it |
| [`shots/`](shots/) | every game shot of the cut as a `quaketool film` shot file; [`shots/INDEX.md`](shots/INDEX.md) maps the cut's shots to them |
| [`render.py`](render.py) | renders shot files to footage |
| [`edit.toml`](edit.toml) | the edit: the shot list, every shot's source, in-point, overlays and sound, the voice's pauses, the levels, the moments the score and the sound effects hit |
| [`pipeline/`](pipeline/) | the code that makes the rest: [`edit/`](pipeline/edit/), [`music/`](pipeline/music/), [`sound/`](pipeline/sound/), [`diagrams/`](pipeline/diagrams/), [`review/`](pipeline/review/) |

## Render it

```sh
uv run film/make.py --res 3840x2160 --voice VOICE_DIR --out OUT_DIR
```

[`make.py`](make.py) renders the whole film from this folder into `OUT_DIR`, in one command.
`VOICE_DIR` is the narration's clips, the one input that is not in the repository: the files
[`voice.json`](voice.json) names ([2. The voice](#2-the-voice)). Everything else is rendered,
stage by stage, by the scripts the sections below describe:

| stage | what it runs | what it writes under `OUT_DIR` |
|---|---|---|
| voice | copies the clips `voice.json` names | `voice/` |
| clock | [`timeline.py`](pipeline/edit/timeline.py) | `edit/timeline.json`, `clock.json`, `clock-score.json`, `ladder-events.json` |
| footage | [`footage/make_footage.py`](pipeline/footage/) | `footage/game/`, `footage/proof/` |
| web | [`web/capture.py`](pipeline/web/) | `footage/web/` |
| diagrams | [`diagrams/render_all.py`](pipeline/diagrams/render_all.py) | `diagrams/` |
| score | the score fitted to the clock and synthesized ([5](#5-the-score)) | `music/score.wav`, its stems and MIDI |
| sfx | [`sound/render.py`](pipeline/sound/render.py) on the clock | `sound/sfx.wav` |
| edit | [`build.py`](pipeline/edit/build.py) | `cut.mp4` and the files below, `edit/build-NNN/` |
| subs | [`make_subs.py`](pipeline/edit/subs/make_subs.py) | `cut.srt`, `cut.vtt` |

The film comes out as:

- `cut.mp4`, the master: 60 fps at `--res`, HEVC at 3840x2160 (made for uploading);
- `cut-1080p.mp4`, the same picture scaled to 1920x1080 (beside a 4K master);
- `cut-480p30-hevc.mp4`, the phone file, under 10 MB, and `cut-480p30.mp4`, its H.264
  fallback for players without HEVC;
- `cut-loudness.md` and `cut-contact.png` (the loudness report, a frame every 5 s), and
  `cut.srt`, `cut.vtt`;
- `edit/build-NNN/`, the build's record: its timeline, mix report, stems, and `inputs/`,
  every file it read with its MD5 (reflinked copies, where the disk has reflinks).

**At 4K.** The slop shots render at 3840x2160 natively. Classic's shots keep their own
modes (320x200, 960x600) and are scaled up as at 1080, in a 4:3 box of 2880x2160. Every
card, label, overlay and diagram is drawn at exactly twice its 1080 geometry: `edit.toml`'s
positions stay in 1080's units, cairo draws with a 2x transform, and id's 8x8 glyphs are
doubled nearest-neighbour, as are the proof's frames. The browser captures are taken at a
device pixel ratio of 2. `--res 1920x1080` renders the 1080 film: the same timeline, clock
and mix, and the same picture except where a re-rendered shot looks through the player's eye
([1. The footage](#1-the-footage)).

**Encoding.** With `--hw vaapi` (the default), the GPU does the video work through VAAPI
(`VAAPI_DEVICE`, default `/dev/dri/renderD128`). The footage and the edit's segments are HEVC
at a low constant QP (not lossless: a 4K film's lossless segments would not fit most disks),
and the segments decode their footage on the GPU too. The master is the segments themselves,
joined without decoding them again (no second generation); the 1080p copy is decoded, scaled
and encoded (H.264) without leaving the GPU. The phone files are always software x265 and
x264: at under 10 MB, quality per bit decides, and there the GPU's encoders lose. The diagram
overlays are ProRes 4444 in software, since VAAPI has no alpha. Every file a VAAPI encoder
writes is checked before it is used ([`pipeline/vaapi.py`](pipeline/vaapi.py)): its HEVC is
tagged `hev1`, never `hvc1` (hevc_vaapi's global and in-band headers disagree, and an `hvc1`
file decodes to garbage), and all of it decodes clean in software. `--hw none` encodes
everything in software, for a machine without VAAPI: the segments lossless at 1080 (as the
v7 cut was built), the master x264.

**Memory.** Each stage runs in a memory cap (`--mem-cap`, a systemd user scope with no swap),
so a job that runs away is stopped alone rather than the system's services. At 4K the
edit renders two segments at a time (`--jobs`): one 4K segment's ffmpeg holds a few
gigabytes.

**Incremental.** Each stage records what it was made from in `OUT_DIR/.make/`, and a stage
whose inputs did not change is skipped, so the command can be run again after any change and
does only what the change needs. The clock's times come from the narration and `edit.toml`
alone: a change that moves a word or a shot makes the diagrams' ladder, the score and the
sound effects again; a changed shot file re-renders its footage and the edit. Caches and
intermediates go to `FILM_SCRATCH` (default `OUT_DIR/scratch/`), and the stages' temporary
files under it, on disk: `/tmp` is often a tmpfs, held in RAM, and a 4K render's scratch would
fill it. Stopping the command (Ctrl-C, or a SIGTERM) stops the running stage the same way, and
its temporary files are removed; run it again to carry on. The scripts run alone default to
`/var/tmp/quake-srp-film/` for their scratch.

```sh
uv run film/make.py --res 1920x1080 --voice VOICE_DIR --out OUT_DIR   # the 1080 film
uv run film/make.py ... --stages footage,edit      # only these stages (each still skips when up to date)
uv run film/make.py ... --force sfx                # remake a stage even if it is up to date
uv run film/make.py ... --range 113.5-116.5        # the edit renders one stretch, as OUT_DIR/range.mp4
uv run film/make.py ... --burn-in                  # a review build: the timecode and the shot ids burned in
uv run film/make.py ... --media footage=DIR        # a stage's output taken from elsewhere (footage, web,
                                                   # diagrams: a folder; score, sfx: a WAV), copied in
```

It needs what [How it is made](#how-it-is-made) lists, VAAPI for `--hw vaapi`, and a lot of
disk at 4K: the footage, the diagrams' ProRes overlays and the edit's segments are large.
It builds `quaketool` itself (`cargo build --release`) unless `--quaketool BIN` names one.

## How it is made

```
id's pak + this repository
  ├─ footage ─────── shots/*.shot ──► render.py ──────────────────────────┐
  ├─ voice ───────── script.md ──► ElevenLabs takes ──► clips + word times │
  │                    └─► the clock: timeline.py + edit.toml             │
  │                          ├─► diagrams (the slop-options ladder, D01…) ├─► build.py ──► the cut
  │                          ├─► score (fit to the clock, synthesized)    │      └─► subtitles,
  │                          └─► sound effects (cued on the clock)        │          the viewing kit
  └─ the edit's cards, drawn inside the build ────────────────────────────┘
```

The narration sets the clock: each shot is cut on the words it plays under, so the voice
comes before everything timed to the cut. The diagrams, the score and the sound effects are
made from the clock, so a change that moves a word or a shot means making them again before
the build.

Every script below reads and writes under one folder, `FILM_ROOT` (default: this folder;
`make.py` sets it to `OUT_DIR`), and keeps caches in `FILM_SCRATCH` (default:
`/var/tmp/quake-srp-film/`, on disk). Paths in `edit.toml` are `FILM_ROOT`'s; the
media keep the production's folder names, version numbers included (`diagrams/v6/`,
`footage/game/`), and the edit's own are plain (`edit/`, `music/score.wav`,
`sound/sfx.wav`). Commands run from the repository's root.

**What it needs.** Rust (to build `quaketool`: `cargo build --release --bin quaketool` in
`quake-rs/`), id's shareware pak at `quake-data/ID1/PAK0.PAK` ([Build and run
it](../README.md#build-and-run-it) fetches it), `uv` (every script carries its Python
dependencies), ffmpeg and ffprobe with libx264 and prores_ks (and libx265 for the phone
file), cairo, the font Inter (the cards, a diagram's caption, the subtitles) and a monospace
font, JetBrains Mono if it is there (the timecode burned into review builds), and the
repository's whole git history (one diagram draws its merges). The proof's frames need
docker and id's WinQuake source ([oracle/README.md](../oracle/README.md)). The voice needs an
ElevenLabs account; the word times and the checks that listen to a cut use ElevenLabs'
speech to text, Scribe, with its key in `ELEVENLABS_API_KEY`.

### 1. The footage

```sh
uv run film/render.py --out film/footage/game                # every shot (this takes a while)
uv run film/render.py --out film/footage/game S01 STG        # only these
uv run film/render.py --list                                 # each shot and what it shows
cp film/shots/sidecars/*.json film/footage/game/             # what the edit reads beside each file
```

Each shot file goes through `quaketool film` and ffmpeg into `NAME.mp4` (H.264, CRF 16, at
the shot's own size and frame rate), with its game sound `NAME.wav` and its events
`NAME.events.json` where the shot asks for them. A shot renders the same frames on every run
and on any number of threads, and the shot files reproduce the cut's shots, with two known
differences, both from the pixel-exact work. A camera that is the game's own eye (`camera walk`,
`camera player`, a demo's camera) draws id's eye nudged 1/32 unit, as id's V_CalcRefdef
does, so those shots render slightly differently from the cut: their edges shift by a
fraction of a pixel. Shots with the film's own cameras (`path`, `fixed`, `follow`, `orbit`)
differ from the cut only in single pixels at texel edges and on liquids, and their x-rays
and marks line up as filmed. [`shots/INDEX.md`](shots/INDEX.md) lists
the shots the eye touches and the other exceptions. Beside each file the edit reads a sidecar,
[`shots/sidecars/NAME.json`](shots/sidecars/): its handles, its sound, and the times of what
happens in it, which the clock, the score and the sound effects are cued on.

Some of the cut's footage is several renders laid together: a wipe, two halves side by
side, a cross-fade, crops that follow a monster. Those are one shot file per render, and
each file's header says how they were laid together; the scripts that did it are not here.
The browser footage (the Options page, the page loading, the phone's touch controls) was
captured from the page itself with Playwright, not with the film tool. The proof's frames,
monsters in view, are `oracle/compare.py`'s. The index lists every shot with what made it,
and gives the proof's commands; each needs `--sse`, since the pixel checks run id's C in its
SSE2 build.

### 2. The voice

The narration is [`script.md`](script.md), spoken by "Frederick" on ElevenLabs
(`eleven_v4`, seed 7). Each line's chosen take was trimmed to its speech and levelled to
−16 LUFS, and Scribe heard it back to give every word's time. The result is the clips and
[`voice.json`](voice.json), the line map: each line's clip (a path in the clips' folder), its
text, its pause and every word's time. [`pipeline/voice/voice_map.py`](pipeline/voice/voice_map.py)
wrote the map from the takes' folder; the edit reads it as `FILM_ROOT/voice/voice.json`,
beside the clips (`make.py` puts it there).

The scripts that did this are not here: a take costs money, and the same text gives a
different take each time, so the takes are the film's source and are kept, not re-made.

### 3. The clock

```sh
uv run film/pipeline/edit/timeline.py --dry      # print it
uv run film/pipeline/edit/timeline.py            # write edit/timeline.json, clock.json, clock.txt, ladder-events.json
```

[`timeline.py`](pipeline/edit/timeline.py) lays the clips end to end with the script's
pauses, cuts each shot on its first word (`over` in `edit.toml`), gives every shot without
narration its own length, and resolves every source and overlay from what is on disk. The
result is `timeline.json`, the edit decision list, and `clock.json`, the named moments the
score and the sound effects hit (`[events]` in `edit.toml`). The score is composed on a
copy of the clock, `edit/clock-score.json`, renewed only when the clock's times change; the
build fits the score to the cut shot by shot.

### 4. The diagrams

```sh
uv run film/pipeline/diagrams/render_v7.py            # every diagram the cut uses (this takes minutes)
uv run film/pipeline/diagrams/render_v7.py D01 ladder # some, by name
uv run film/pipeline/diagrams/render_v7.py --list     # the commands, without running them
uv run film/pipeline/diagrams/render_all.py --scale 2 --clock OUT/edit --out OUT/diagrams   # at 3840x2160
```

[`pipeline/diagrams/README.md`](pipeline/diagrams/README.md) has the outputs, the codecs and
the scale.

The diagrams are drawn with [`qkit`](pipeline/diagrams/qkit/), a small kit on cairo that
draws in id's palette and lettering, read from the pak (palette, conchars, gfx.wad): the
slop-options ladder (the scoreboard that lights each option as the film switches it on,
timed by the clock's `ladder-events.json`), the fixed-point and palette diagrams of the
cold open, the oracle's band, one luxel's light wandering about id's line, a light style's
string, a jump's arc at 72 and 480 Hz, the mixer, the perspective divide, the fleet's merges.
Each overlay is a ProRes 4444 movie with alpha; the two whole-frame ones are H.264. Their
timings are cue files in [`cues/`](pipeline/diagrams/cues/), on the cut's words. Some read
the repository as it is checked out: the torch and light-style code they quote, the menu's
rows, the merge history; D02 and D06a render frames with `quaketool`, and D14 mixes a sound
with `quaketool sndscript`.

### 5. The score

```sh
uv run film/pipeline/music/v7/fit_events.py -o film/pipeline/music/v7/events-v7.inc   # the cut's moments, from the clock
uv run film/pipeline/music/v7/make_scores.py           # score-v10.score, from its template
uv run film/pipeline/music/v7/kills.py                 # the climax's kills, on the score's grid
uv run film/pipeline/music/synth/render.py film/pipeline/music/score-v10.score -o film/music/score.wav --stems --midi
```

The score is synthesized: [`music/synth/`](pipeline/music/synth/) is a synthesizer in numpy
and scipy, and a `.score` file is a text score for it: tracks, instruments, patterns and
notes placed at named moments, tuned so that D2 is 72 Hz, the game's tick rate
([`score.py`](pipeline/music/synth/score.py) describes the format; `render.py --patches`
lists the instruments). [`score-v10.score`](pipeline/music/score-v10.score) includes the
[`v7/`](pipeline/music/v7/) files, one per part of the film, and `events-v7.inc`, the cut's
moments on the clock (`edit/clock.json`, read by `fit_events.py`), so the score's hits
land on the cut: the torches, the burst, the climax. The first three commands are needed only
when the clock moves (their outputs are here, fitted to the v7 cut's clock; `make.py` runs
them on a copy of this folder in `OUT_DIR/music/src/`, so the repository's stay as they are);
the render writes `music/score.wav`, its stems and a MIDI file, takes minutes, needs nothing
but the score's text, and gives the same bytes on every run (each note's noise is seeded from
the note).

### 6. The sound effects

```sh
uv run film/pipeline/sound/extract_id.py      # id's sounds out of the pak: sound/id/
uv run film/pipeline/sound/design.py          # the designed sounds: sound/designed/
uv run film/pipeline/sound/render.py          # the cue list on the cut's clock: sound/sfx.wav
```

The sound effects are one stem the edit mixes under the voice. Most are id's own sounds,
straight out of the pak; the rest ([`design.py`](pipeline/sound/design.py): whooshes, hits,
risers, the torches' roar, the two-pixel glitch) are made from the score's instruments, in
its tuning. [`cues-v7.md`](pipeline/sound/cues-v7.md) places each one on the cut: on a shot's
cut, on a word of the narration, on a named moment of the clock, or on an event the film tool
logged in the footage (a sound started, a muzzle flash), and
[`render.py`](pipeline/sound/render.py) renders them, as long as the film, from the clock,
the footage's sidecars and event logs. It takes minutes, and gives the same bytes on every
run.

### 7. The edit

```sh
uv run film/pipeline/edit/s26_lines.py                       # S26's check lines (Quake's lettering)
uv run film/pipeline/edit/build.py --publish cut             # edit/build-NNN/, then edit/cut.mp4
uv run film/pipeline/edit/build.py --publish cut-clean --no-burn-in --preview   # no timecode; phone files
uv run film/pipeline/edit/build.py --range 113.5-116.5       # one stretch, as build-NNN/range.mp4
uv run film/pipeline/edit/s26_lines.py --scale 2             # at 3840x2160: the lines at 2x, then
uv run film/pipeline/edit/build.py --scale 2 --hw vaapi --publish cut --no-burn-in --preview
```

[`build.py`](pipeline/edit/build.py) makes the clock again, draws the edit's own pictures
(the title, the proof's panels, labels, captions, the end card:
[`cards.py`](pipeline/edit/cards.py)), renders one segment per shot (the footage at its
in-point, the overlays and diagrams composited, fades; lossless in software, HEVC at a low
QP with `--hw vaapi`), mixes the voice, the score
(fitted to the cut, ducked under the voice), the game's own sound and the effects stem,
masters to −14 LUFS under −1 dBTP, and encodes the film. It takes minutes. Each build is a
fresh `build-NNN/` with its `timeline.json`, its mix report, its stems, a loudness report, a
contact sheet, and `inputs/`, every file it read with its MD5 (reflinked copies where the
disk has reflinks); nothing earlier is overwritten.

### 8. Subtitles and the viewing kit

```sh
uv run film/pipeline/edit/subs/make_subs.py --cut edit/cut.mp4 --voice voice/voice.json --clock edit/clock.json
uv run film/pipeline/review/watch.py edit/build-NNN/film.mp4
```

[`make_subs.py`](pipeline/edit/subs/make_subs.py) writes SRT, VTT and ASS subtitles timed by
the clock, placed clear of the film's own text, checks them against what Scribe hears in the
cut, and burns them into the phone files. [`watch.py`](pipeline/review/watch.py) watches a
cut and writes a report of what to fix or look at: contact sheets, loudness and the buses,
sync, the words against the script, the cut against its own sources
([`review/kit/README.md`](pipeline/review/kit/README.md)). Both take minutes.

## The media

The media are not in the repository: the footage, the voice takes and clips, the diagram
movies, the score and the effects stem, the builds and the cut. They run to many
gigabytes, and most of them can be made again from what is here: every picture from its
shot file (with the eye's 1/32-unit difference above) or diagram script, the score and the
effects from their sources. The voice
cannot: the takes were paid for, and a take cannot be regenerated byte for byte.
`film/.gitignore` keeps the media folders out of git when they are rendered here.

**Left out, and why.** The code below made media the film uses, but it writes into the
production's own tree, drives services or hardware outside this repository, or only checked
the work; each part's text above says what replaces it.

- The footage scripts that laid several renders together (the wipes, side-by-side halves,
  crops that follow a monster, cross-fades, the mosaics), graded two shots and wrote the
  sidecars. Each shot file's header says how its renders were laid together, and
  [`shots/sidecars/`](shots/sidecars/) keeps what the edit and the sound read from each
  footage file (its handles, its sound, the times of its events).
- The browser captures' scripts: Playwright recording the page, served from a deployed copy,
  on a desktop and in a phone's profile. A recapture is close, not identical (the page's boot
  is a real-time screen recording).
- The voice's scripts: the ElevenLabs takes, choosing and trimming them, the levelling, and
  Scribe's word times. The takes are the source; see [2. The voice](#2-the-voice).
- The score's and the sound's analysis and check tools, the earlier cuts' scores, cue lists
  and configs, and the trailer and the poster.
- The production's own notes: the plans, the reviews, the versions in between.

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

One shot by hand:

```sh
quake-rs/target/release/quaketool film quake-data/ID1/PAK0.PAK film/shots/S07.shot out/S07
```

That writes `out/S07/00000.png` on (`--raw` writes raw frames to stdout instead, for
ffmpeg), and any shot line can be given last as an override (`--preset classic`, `--size
960x540`). `quaketool film --help` prints the whole format
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
  the camera knows where it goes); `camera walk`, the player walking a route, the camera
  its own eye; `aim` turns a camera to an entity, and `aim monsters` makes a walking player
  aim at what it fights.
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
[`walk.rs`](../quake-rs/src/bin/quaketool/film/walk.rs) (the walking player),
[`xray.rs`](../quake-rs/src/bin/quaketool/film/xray.rs) and the renderer's side,
[`render/xray.rs`](../quake-rs/src/render/xray.rs),
[`events.rs`](../quake-rs/src/bin/quaketool/film/events.rs),
[`mapgen.rs`](../quake-rs/src/bin/quaketool/film/mapgen.rs) (the maps made for the film) and
[`marks.rs`](../quake-rs/src/bin/quaketool/film/marks.rs). The world is the browser's own
client, run natively, with only the camera the film's.

The conventions of the shot files:

- Most shots run a second longer at each end than the film uses: handles for the edit.
  "File second" counts from a render's first frame.
- id's Brightness slider (`cvar gamma`) lifts the darker shots, as a player would, rather
  than a colour grade in the edit.
- A name with a suffix is one render of several: `ST1b-off` and `ST1b-on` are laid
  together in the film, as the files' headers say.

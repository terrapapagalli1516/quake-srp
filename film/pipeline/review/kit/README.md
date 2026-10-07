# The viewing kit

Watch a cut the way a careful editor would and get one folder with an `index.md`. A whole run takes minutes.

```
uv run film/pipeline/review/watch.py edit/v7/build-004/film.mp4                # everything, into a fresh folder
uv run film/pipeline/review/watch.py CUT --range 1:43.0-1:45.0                 # plus every frame of a range
uv run film/pipeline/review/watch.py CUT --out DIR --only words                # re-run one part into the same folder
```

**Where it reads and writes** (`film/pipeline/filmroot.py`): a relative path (the cut, `--build`, `--script`…) is
looked up in the current folder, then under `FILM_ROOT`, which holds the film's media in its own layout
(`edit/vN/build-NNN/`, `voice/`, `music/`, `sound/`, `footage/`, `diagrams/`). The report goes to a fresh
`FILM_SCRATCH/watch/runs/<cut>-NNN/` unless `--out` names a folder (an existing one is reused, its decoded frames
kept). The only other thing it writes is its cache, `FILM_SCRATCH/watch/cache/` (Scribe's transcripts, re-made
buses); `WATCH_CACHE` moves it.

**It needs** ffmpeg and ffprobe, and for the words part an ElevenLabs API key in `ELEVENLABS_API_KEY` (Scribe, their
speech-to-text). Without the key, and with no transcript of this cut's sound cached, the words part lists no words and
the sound and sync parts run without them. It decodes on the GPU through VA-API when there is one.

`index.md` lists every finding marked **fix** or *look*, with a time and the page that shows it.

**Parts** (`--only` / `--skip`, comma lists):
- `shots`: a contact sheet per section, a row per shot: first, middle and last frame, frame numbers, film times, the planned words.
- `overview`: a frame every 0.5 s (`--overview-every`), a red bar after each cut.
- `range`: `--range A-B` (repeatable), every frame (`--every N`, `--tile PX`), the sound of each page drawn above it per bus.
- `words`: Scribe on the cut's own audio, cached by its bytes; the words in each shot; the transcript against the script
  (`--script`, default the edit's; a voice map `.json` works too): missing, extra, misheard, a cut-off word heard whole;
  each word's time against the timeline's; lines with a cut mark.
- `sound`: EBU R128 loudness over time with the shots, per bus; the voice against its bed per voiced passage; clipping,
  dropouts, unmarked silences (`--silence`), sound inside a marked silence (a mute, the SFX stem's `silence` lines), a cue
  a mute cuts in two, clicks and steps at the cuts (and what explains them: a cue, a score hit, a mute's edge).
- `picture`: every captioned moment at phone size (640 wide, 1:1) with the 90% safe area and a table of text heights;
  black, frozen, flat-colour and flashing frames, each checked against the shot's own source (a still source is not a
  frozen film; a source that ran out is the build holding a frame); text the shot list plans that the cut lacks.
- `sync`: the edit's named moments, the score's hits, the sound designer's cues (and whether `sound/render.py --list`
  would still put them there on this cut), the footage's logged sounds: where each should land, where a rise in the
  right bus shows it landed, and whether a short hit placed on a word is masked by it.

**The checks** (on by default; each writes its page, its flags into the index, and its evidence into `evidence/`).
The text checks compare each moment of the cut with **its own source at the same moment** (`layers.py`): the build's
frozen copy, decoded the way the edit's `build.py` places it (in-point, speed, fit or 4:3 box, held last frame, dim).
Where the two differ, the edit drew something; letters there are the overlay's, letters in the source are the
footage's. A review build's burned-in timecode and shot ids are found and left out.
- `ghosts`: text burned into the footage that still reads beside or through a band laid over it. Three moments of each
  overlay's span, full size. A burned-in label is a source line of Quake's letters, one height, one baseline, on a dark
  plain band, holding still; a ghost is one the overlay reaches (some letters hidden, dimmed, or under its own letters)
  while two or more still read (7+ luma levels of contrast). The evidence marks each letter: red still reads, green hidden.
- `overtext`: the overlay's own letters drawn where three or more footage letters still read. Same frame pairs. A ghost
  under a translucent band usually shows in both.
- `shape`: in act 4 the frame's shape follows the ladder: the game's picture is the 4:3 box until NATIVE PIXELS lights
  (`edit/vN/ladder-events.json`, checked against this cut's shots; else the clock's `burst`), full width after.
  Measured on each act-4 game shot's source pillars, so the ladder and diagrams over them don't count; web captures and
  diagrams are left out; a side-by-side comparison of one view (halves alike) is a lab layout, listed, not flagged; the
  burst itself must open the box.
- `stops`: a music, game or SFX bus falling from audible (-50 dBFS) to digital silence in 10 ms or less, mid-note. Read
  on the buses (the build's saved `stems/`, else the re-made ones). **fix** at a cut where the clock marks no silence and
  the whole sound falls 10 dB or more; *look* where the other buses carry on; listed only where they cover it or a mute
  or the clock marks it. A silence the score's sidecar names is said so: the silence is meant, the dead stop may not be.
- `readtime`: each text's fully visible time against its reading time, 0.3 s a word and 1.5 s at least. Labels,
  captions and bumpers from the timeline (their words, their fades and typing, carried across a cut when the next shot
  has the same text). Cards, cards.py overlays, diagram movies and PNGs measured (`texts.py`): ten samples a second
  (five for diagram movies), lines found at their brightest, a line fully up while its letters keep 60% of their best
  contrast, lines that come up together read as one block, words estimated from a line's width.
- `textsize`: each text's capital height at phone size, 640 wide, against 8 px (**fix** under 6). Quake's capital is 6
  of its 8 rows (18 px at 3x, so a 3x label is 6.0 px, a 4x caption 8.0): from the scale for labels, captions and
  bumpers; measured for the rest; the ladder on one frame of each of its movies. Like items share a flag.

**What it finds by itself:** the build that made the file (by its bytes), that build's timeline, `mix.json` and config,
the clock if it is this edit's. Override with `--build`, `--timeline`, `--clock`, `--config`.

**The buses.** The edit's `build.py` mixes voice, music, game and SFX and saves them in
`BUILD/stems/{voice,music,game,sfx}.flac`; the kit reads those first. For a build without them it re-makes them with
`derive_stems.py`: `build.py`'s own `mix_audio()` on the build's own timeline and frozen sources, cached per build. It
checks them against the cut's sound, window by window; a source rewritten after the build makes the re-make differ,
and the check flags where.

**It cannot judge** taste: whether a joke lands, the pacing, whether the picture shows what the words say, whether a
comparison reads, whether the film delights. It checks no claim against the repository. It measures text size and
time, not contrast or clutter. It finds burned-in text only as Quake's letters on a dark band. Text that slides across
the frame reads as up for less time than it is; a word count from a line's width is an estimate. It does not know
which silences, freezes, stops and hits are meant: it says what it saw and why it might matter.

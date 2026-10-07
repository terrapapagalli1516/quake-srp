# film/pipeline/web: the page, filmed

The film shows quake-srp's own web page three times: the page loading in a browser window
(F01), touch play on a phone (F05b), and the game's own Slop Options menu (S18), which the film
tool cannot open. This stage films them from the real page, running in a headless Chromium
driven by Playwright.

```sh
uv run film/pipeline/web/capture.py --scale 2 --out OUT/footage/web            # every web shot, 3840x2160
uv run film/pipeline/web/capture.py --scale 1 --out OUT/footage/web --only S18 # one, at v7's 1920x1080
uv run film/pipeline/web/capture.py --list                                     # the shots
```

It writes, for each shot, the names the edit reads:

| | |
|---|---|
| `NAME.mp4` | the clip, 60 fps, at 1920s x 1080s |
| `sheets/NAME.png` | a contact sheet |
| `sheets/NAME.events.json` | what happens on which frame (the click, each FIRE and its blast, each menu key) |

A clip whose inputs are unchanged since it was made (the page's program and files, id's pak,
this stage's code, the scale and the encoder: `.stamps/NAME.json`) is skipped; `--force`
makes it again.

## What it does

1. **Builds the page's program** from this repository: the threads build, `cargo build
   --release --target wasm32-wasip1-threads` in `quake-wasm/` (`--wasm FILE` uses a built
   one instead).
2. **Lays out a deploy dir** as [`web/PLATFORM.md`](../../../web/PLATFORM.md) describes: the
   page's files (`web/isolated.py`'s list), `quake.wasm`, and id's shareware pak as
   `id1/pak0.pak`. It lives in `FILM_SCRATCH/web/` for the run and is removed after it.
3. **Serves it** with `web/isolated.py`'s handler (the isolation headers the threads need) on
   the first free port in 9300–9309, from a thread of the command itself. F01's server is
   capped at 10 MB/s, so the download shows.
4. **Films each shot.** From the moment a shot's action starts, every frame is one 1/60 s tick
   of the page's own driver (`quake.pause()`, `quake.tick()`), with the keys, clicks and touches
   sent between ticks, then a screenshot at the page's device pixels. So the motion is exactly
   60 fps whatever the machine's load, and a run gives the same frames each time. The one
   exception is F01's boot (the download and the program starting), which is filmed in real time
   by the browser's screencast and laid on a fixed number of frames, so everything after it keeps
   its times.
5. **Draws what a screenshot lacks**, on a black frame: a plain browser window (one tab, the
   page's favicon and title, the address `localhost:PORT`) around the desktop page, a plain
   bronze phone outline around the phone, the mouse pointer (the CSS cursor the page shows
   at that point), a click ring, and soft circles where fingers touch. Nothing is a real
   browser's or device's: no branding, no real address.
6. **Encodes** each frame as it is drawn, piped into ffmpeg (no frame files): `--hw vaapi`
   is HEVC on the GPU (VAAPI) at a constant QP (`--qp`), `--hw none` is software H.264 at CRF
   16, the encode v7's clips had; `auto` (the default) takes VAAPI when a test encode works.
   A VAAPI clip's header is built from the stream's own parameter sets, and every clip passes
   [`vaapi.py`](../vaapi.py)'s checks before it is kept: HEVC tagged hev1, the header's
   parameter sets the stream's, and all of it decoding clean in software (vaapi.py says why).
   A clip that does not decode clean is captured again (a run gives the same frames each time),
   and a second bad one, or a header that disagrees, stops the command with an error.

The whole command runs inside a systemd user scope with a memory cap (`--mem-max`, default
`FILM_MEM_MAX` or 6G, no swap), the browser and the encoder included, where `systemd-run`
is there: Chromium at a high devicePixelRatio and a 4K encoder are heavy, and a machine out
of memory kills whatever it picks.

At `--scale s` everything is s times v7's: the page is the same size in CSS pixels at s times
the devicePixelRatio (the desktop window 920x472 CSS px at 2s, S18's canvas 1920x1080 CSS px at
s, the phone profile 1012x412 CSS px at 2.6s), and the window, the phone, the pointer and the touch
marks are drawn at s times their geometry. So at scale 2 the desktop page draws the game
natively at its 4K size, the menu's lettering at twice the scale, and the text crisp. The phone
is the exception: its game keeps the phone profile's 1315x535 frame (the game's pixel size goes
from the touch preset's 2 to 2s), so the game looks as it does at scale 1, while its touch
controls are drawn at the finer ratio.

## The shots

- **F01-page-load.** A fresh browser profile opens the page (the default preset, slop). The
  boot panel and the download; the start prompt over the attract demo (demo1 from its first
  frame); the pointer glides to it and clicks; the demo plays on.
- **F05b-phone-touch.** web/verify_touch.py's phone profile, PHONE_26 (landscape,
  `isMobile`, `hasTouch`). e1m1's skylight hall at skill 2, which the game sets the way it
  does for a player (start's Hard hall, then its episode 1 portal: the port has no `skill`
  command, and `map` starts at skill 1), checked in a save. Brightness 0.8, god mode, the
  rocket launcher; the player walks in through the doorway, the five grunts turn, a rocket
  into them, a jump, rockets down the hall, a look up at the skylight. The setup is the page's
  own calls (`quake.callLine`); the console is toggled down and up so no notify line shows.
- **S18-slop-options-menu.** The canvas alone, full frame: the start map, the player at S17's
  end (`setpos`, `look`), then the main menu, Options, up past the end to Reset to slop and
  Reset to Classic, Slop Options, Picture and sound, and down its rows. The page's "click to
  capture mouse" chip is hidden: it is page chrome, not the game's picture.

## What it needs

- Rust with the `wasm32-wasip1-threads` target, and id's shareware pak at
  `quake-data/ID1/PAK0.PAK` (as the rest of the repository expects).
- `uv`; Playwright's Chromium, installed once with
  `uv run --with playwright==1.63.0 playwright install chromium`.
- ffmpeg and ffprobe with libx264 (and, for `--hw vaapi`, a VAAPI device: `VAAPI_DEVICE`,
  default `/dev/dri/renderD128`); fontconfig with Noto Sans and Noto Sans Mono (the window's
  title and the contact sheets' times).
- `FILM_SCRATCH` (see [`filmroot.py`](../filmroot.py)) for the run's deploy dir.

## Checking it

```sh
uv run film/pipeline/web/compare.py NEW.mp4 OLD.mp4 --frames 0,120,300 --strip cmp.png
```

prints the PSNR of every frame of a re-capture against another clip and draws chosen frames
side by side with their difference; a 2x clip is scaled down to the 1x one's size first. A
re-capture at scale 1 matches v7's clips except where the world moves on its own clock (the
torches, the sky, the light styles: S18's ticks start from a fixed point now, v7's from wherever
the real-time setup left them), F01's boot (real time), and the player's-eye pixels that id's
1/32-unit eye nudge moves (film/README.md).

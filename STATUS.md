# Quake-RS — status and hand-off

Last updated 2026-10-07, after the merge of Classic's every pixel, the monsters and the film tool. The first section is
where things stand. Then the rounds, newest first, branch by branch; how to work here; and
the older history, kept as evidence, with superseded items marked.

---

## Where things stand

- **What it is.** A Rust port of id's WinQuake. It runs in the browser, as a WASI program
  in a Web Worker, and natively through `quaketool`. It plays the shareware episode in
  single player: attract demos, New Game, E1M1–E1M8 with Chthon, death and respawn,
  intermission and finale, save and load, the menus, the console. With the player's own
  `pak1.pak` it plays the registered game. `README.md` says how to build, serve and play
  it.
- **The rules** (the user's, 2026-09-26, in priority order):
  1. zero dependencies;
  2. no `unsafe`;
  3. Classic = id's, proven for anything touched;
  4. the default is the best "software-rendered Quake, in 2026": palette-true, crisp
     texels, never filtered, and playing well from 60 to 480 Hz with no 72 fps cap;
  5. showcase-quality code.

  Where the rules stand today:
  - Every crate and binary is `#![forbid(unsafe_code)]`, with only `std` and no
    dependencies. The page is plain JS/HTML.
  - Every departure from id's game is a setting. The **Classic** profile turns them all
    off; **2026**, the default, turns most on (`AUDIT.md`, "The profiles and the
    departures").
- **What works in 2026, beyond id's game.** Each of these is off in Classic:
  - no frame cap, with the game stepped to play as at 72 Hz from 60 to 480 Hz;
  - native resolution in whole pixels (Auto pixel size), Hor+ widescreen, and a 2-D
    layer blown up by a whole number;
  - smooth monster movement and the crosshair;
  - WASD, mouse look and Always Run;
  - id's mixer at the device's rate with four bugs fixed;
  - a twin-stick gamepad with rumble, and touch controls on phones.

  **In every profile:**
  - the renderer draws on every core, byte-identical at any thread count;
  - 8-bit frames, presented through WebGL2 or a 2-D canvas;
  - id's mixer in an AudioWorklet;
  - saves and `config.cfg` as files in IndexedDB;
  - install to the home screen and play offline;
  - a player's own `pak1.pak` and CD tracks;
  - QuakeC errors end the game as id's `Host_Error` does.
- **Measured** (2026-10-07, at the merge of `fleet/pixel-ents`, `fleet/film-hero` and `fleet/filmdocs`):
  - **Classic** (since `fleet/pixelexact`, 2026-10-05): `uv run oracle/classic_check.py`
    prints ALL PASS, ten checks. They cover the goldens `790c53d3` / `3684efc6` /
    `e18bb516`, 42 play hashes and tallies, the timedemo counts, the census and id's
    edicts on nine maps, the eight 3-D oracle rows at 100.0000% and the `exact` sweep's
    676 frames with not one pixel off, 262 of them with the player among each map's
    monsters, awake (both against id's C built with SSE2 floats, the port's target:
    AUDIT, "Bit for bit" and "Every view tried, with monsters"), 146 2-D shots at their recorded match, demo playback against
    id's client over 17,500 frames, and the mixer against id's C in 28 of 28 cases.
  - **Tests:** `cargo test --release` gives 975 in quake-rs (plus a few ignored ones that
    need the mission packs' data, `QUAKE_*_DIR`/`QUAKE_HIP1M1_PAK`) and 224 in quake-wasm (1
    ignored: the 2-D oracle's harness).
  - **Clippy:** 0 warnings in both crates.
  - **Browser checks:** the `web/verify_*.py` checks pass in headless Chromium
    (2026-10-07). `verify_threads` builds its own program and
    `verify_audio_resilience` needs a `--features bench` build; the rest run on a
    deploy dir of either build. Earlier branches also ran most checks in headless
    Firefox.
  - **Frame rate:** `quaketool framerate --check` passes: 22 scenarios, uncapped at
    60–480 Hz against 72.
  - **Speed:** `timedemo demo1` in Classic natively is 1.35–1.44x id's portable C. In
    2026 video on 8 threads it runs 689 / 455 / 210 fps at 1080p / 1440p / 4K. In the
    browser a 1440p frame takes 3.3 ms on the threads build on the GPU (`PERF_PLAN.md`
    §11; these predate `fleet/opt-pixels`, §15).
- **Not verified:**
  - a real browser on a real display: every browser check ran headless, on a
    desktop GPU at best;
  - Safari and iOS (Playwright's WebKit would not start here);
  - a real phone beyond one: an Android phone was measured through Chrome's
    remote debugging (2026-10-01, below); nothing else, and no other browser;
  - a real 120–480 Hz display (the phone's Chrome caps the page's main thread at 60 Hz
    unless a finger moves; see below);
  - real pointer lock (headless Chromium's lock jumps the pitch to −70);
  - a real gamepad (emulated);
  - ~~Esc under the Keyboard Lock API~~ (verified 2026-10-02 in a headed Chromium: a tap is
    the menu, a hold leaves fullscreen);
  - sound by ear (only its counters, samples and the C oracle).
- **Deployed.** The threads build of `main` is served over https on a private network:
  the shareware page, and a registered one that offers `pak1.pak` and the CD tracks
  through `files.json` (`web/PLATFORM.md`, "A server's own files"). No game data is in
  the repo. `README.md` describes serving generically.
- **What is left.** `AUDIT.md`, "Open, as of 2026-09-26", is the one list. The closing
  review's ranked next steps:
  1. old-era names (`wasm_*` cvars, `MenuScreen::Extras`, `EXTRAS_*`; renaming needs
     `config.cfg` aliases);
  2. rustfmt and edition 2024 for quake-rs (CODE_PLAN W0a);
  3. 12 rustdoc warnings and dead public functions in `server/` and the VM;
  4. 16 thread-locals left;
  5. ~~on a phone, the touch buttons overlap the ammo count~~ (fixed 2026-09-30);
  6. ~~in 2026, Video Options shows 960x600 as current and a pick silently turns Native
     resolution off~~ (fixed 2026-09-30; the fade dither was already fixed);
  7. ~~no `version` command, and `disconnect` does not end the game~~ (fixed 2026-10-02,
     with id's F-key binds).

  The structural work is `CODE_PLAN.md`'s menu:
  W0a/W0b, R1, R6, R8, R9, R11, and the engine-owned `Host` session (§7).

---

## 2026-10-05 to 2026-10-07: every pixel, the monsters, and the film tool

Merged together (the details are in the branches' commits, and for the pixels in AUDIT):

- **Classic is id's C to the pixel** (`fleet/pixelexact`, `fleet/pixeldocs`). Classic's
  renderer computes in id's C types (`AngleVectors`, `R_ViewChanged`, `D_CalcGradients`, the
  spans' float accumulators, the alias models, sky and warp; `R_AnimateLight`'s clock in
  double), and matches id's C built with SSE2 floats on every pixel of every view tried.
  The goldens moved to `790c53d3` / `3684efc6` / `e18bb516` (texel-edge pixels); the
  x87 build and 1996's FPU mode are not the target (AUDIT, "Bit for bit").
- **With the monsters awake** (`fleet/pixel-ents`). 18 views of 1,967 searched among the
  monsters differed; all are id's now: the view leaf is `Mod_PointInLeaf`'s at
  `V_CalcRefdef`'s eye, nudged 1/32 off node lines; `D_DrawZSpans` stores in pairs as id's
  C does (the C, not the 1996 asm: the user's call); `compare.py` hands the port id's
  light-style strings. `exact_sweep.py`'s `monsters` and `cases` views are in
  `classic_check` (AUDIT, "Every view tried, with monsters").
- **`quaketool film`** (`fleet/filmtool` to `fleet/film-hero`, merged as one stack). A
  camera inside the engine for the port's explainer film, driven by shot files: fixed,
  path, follow, orbit and walking cameras; x-rays of the renderer (spans, surface cache,
  lightmaps, segments, threads' bands, the exact pass); marks that stick to the world;
  `display HZ`; settings that change mid-shot; a map made for the film (`mapgen`); a
  walking player that fights (`aim monsters`). Its hooks in the engine are idle unless a
  film turns them on. A film's own camera is drawn where its shot puts it (no bob, no
  nudge); its views of the player get id's eye. `film/` holds the shots and the command
  that renders them.
- **The docs say "the user"** (`fleet/filmdocs`), and segments ahead is id's own x86 FDIV
  overlap (PERF_PLAN §15).

## 2026-10-02, later: what the user found playing, and the packs' own paths

Played on a phone and a desktop the same day, with Opus and Sonnet agents on `fleet/*`
branches, the renderer changes reviewed, every merge full-checked and deployed:

- **The packs on a phone** (`fleet/picker`). The start overlay's own click took a tap on a
  pack as "tap to start"; the choices were grey 12-px text with no state and gone after the
  first tap. Now one `gameList()` draws them as buttons with the running game marked, on
  the start overlay, in a "game" menu in the desktop bar, as a GAME pill beside BACK in a
  phone's menu, and under the quit screen; `?game=` stays the only state.
- **The Scourge start-room door** (it did not move, yet the player walked through it): two bugs.
  `fleet/rotate` ported `R_RotateBmodel` and the clipped submodel path (`edge.rs`,
  `world.rs`): rotating brush models draw rotated, byte-identical in Classic (id1 has none),
  matched against id's C on that door through its swing. `fleet/startdoor` (round 2) found
  the collision half: `sv_move` took a `SOLID_BSP` hull from the entity's live `model`
  string, which Hipnotic's QC blanks after `setmodel` — id's `SV_HullForEntity` goes by
  `modelindex`; now `Host::model_name` does, and id's C and the port block the walk at the
  same wall. (Round 1 had proven hip1m1's five-piece door right; the user meant Scourge's
  own `start.bsp`.)
- **No sound at the episode gates** (`fleet/telesound`): proven with a scripted walk through
  id's C (`oracle/sound_walk.py`, `walk_oracle.c`, `quaketool sndwalk`; in `classic_check`'s
  sound row): the skill halls' teleporters sound, the `trigger_changelevel` gates don't.
  Fixed on the way: sound positions as the wire rounds them (`server::wire_coord`).
- **The Level Complete screen** (`fleet/intermission`): id's `Sbar_IntermissionOverlay`
  draws at coordinates laid out for 320 columns while the bar, the menus and the finale
  centre; in 2026's wide 2-D screen it sat left. `Screen2d::centred_320_x` serves all of
  them; Classic keeps id's placement; the crosshair stays off the stats.
- **Fullscreen** (`fleet/fullscreen`): in a headed Chromium, F was honoured only while the
  game had the keyboard, and holding Esc hands the game one Esc first (the menu), so F died
  after every exit. Alt+Enter toggles from `document.fullscreenElement` in the capture
  phase whatever has the keyboard; F is unbound as in id's `default.cfg`; Classic has no key
  (its strafe-jump); `vid_fkey` is `vid_altenter`, old names accepted; refusals reported.
- **The packs' paths** (`fleet/packclass`, `AUDIT.md` "The mission packs' paths", P1–P19):
  what id's engine does that the port skipped because id1 never reaches it, against id's C
  on all 35 pack levels (`census/packs.py`, `screen2d.py --game`, `oracle_move`). Fixed:
  `items2` (the packs' wetsuit, empathy shields, armour type, shield and belt never reached
  the status bar; HUDs 100% now), slanted clip-plane sums in f64 (three items spawn as
  id's), the re-release's `svc_achievement` skipped by name. Its follow-ups:
  `fleet/makestatic` (`makestatic` frees its edict as `PF_makestatic` does; edict numbers
  near id's — the `edicts` check's differences fell from 1,185 rows to 606; `r2m6` loads
  in Classic under 600, so the edicts round's "632" was the port's own count, and
  `sv_max_edicts` is just room for maps past 600; old saves migrated on load),
  `fleet/sprites` (id's `r_sprite.c`/`d_sprite.c`, every type, in id's list order: bullet
  holes lie on the wall, explosions' z-ties as id's; `play.demo1` re-recorded),
  `fleet/packend` (builtin #79 `finaleFinished` and `menu_credits`: a pack's ending reaches
  the end screen), and `fleet/strings` (the re-release's `$qc_` keys with `{0}` arguments read as
  English from the pack's own `loc_english.txt`, finale text included; both profiles).
- **Found, left open** (`AUDIT.md` Open): `angle_vectors` in f64 where id's is float (a
  perpendicular facing test can flip), movement angles unrounded, particle origins
  unrounded, `cvar()` of client cvars (Hipnotic's footsteps), `sprint` to a non-client.

---

## 2026-10-02: the polish round

The user asked for a look at what would improve
this project, the one complaint being Options on a phone. Four items were chosen and
done, each by a Sonnet agent on a `fleet/*` branch, the two engine ones reviewed by a second
agent before the chair merged them; `main` and both pages were updated after each.

- **The menus on a phone** (`fleet/menupad`). A text row of id's menus is 8 lines — about
  12 CSS px on a phone, a third of a fingertip — so taps missed and sliders took
  a tap a notch. While the menu is up the touch layer now shows a key pad (▲ ▼ ◀ ▶ OK) in
  the free margin right of the menu's centred layout; held arrows repeat at a keyboard's
  cadence; it hides while the menu asks y/n or waits for a key to bind. Keys only, so the
  engine is untouched. `verify_touch` has 81 checks.
- **id's function keys** (`fleet/fkeys`). `default.cfg`'s F1–F4, F6, F9, F10 and F12 are
  bound as id's are (`keys.rs`), with the commands behind them: `menu_save`/`menu_load`/
  `menu_options`, `save quick`/`load quick` through `wait` (the rest of a bound line runs
  next host frame, where `Cbuf_Execute` would), `quit`, `version`, `disconnect` (ends the
  game), and `screenshot` (id's PCX into the game directory; not a download yet). The page
  keeps those keys from the browser and locks F12 with Escape in fullscreen.
  `verify_fkeys.py`.
- **Animation frames blended** (`fleet/lerpframes`, reviewed). `r_lerpmodels`, on in 2026:
  an alias model is drawn between its previous and current frame over id1's 0.1 s tick
  (`client/lerpmodels.rs`, after `lerpmove.rs`), light averaged from both poses, the view
  weapon too; group frames and the usual snaps excepted. "Smooth animations" on the
  settings page, whose help lines now adapt to the row count. Cost: noise-level on 8
  threads. The review's two fixes: the view weapon's identity is its precache index
  (`Host::find_model`, `SV_ModelIndex`'s lookup), and a change away from a group frame
  snaps as the doc said.
- **The mission packs** (`fleet/mission`, reviewed; `fleet/mission-page`). Scourge of
  Armagon and Dissolution of Eternity as WinQuake plays them with `-hipnotic`/`-rogue`
  (`-game <dir>` too): a game directory layered over `id1`, the progs' unknown builtins
  failing at the call as id's do, `GameMode` read from the loaded progs, each pack's own
  status bar (`sbar.rs`'s `if (hipnotic)`/`if (rogue)` arms; pixel-exact against id's C on
  hip1m2 and r1m1 in the review). On the page: `?game=hipnotic|rogue`, a server's
  `files.json` may offer `<game>/pak0.pak` and `<game>/music/`, fetched only for the game
  asked; dropped packs; CD tracks keyed per game; a start-screen picker when a pack is
  there. The registered deploy serves both packs (built from the 2021 re-release with
  English messages; `maps/b_exbox2.bsp`, which the re-release moved into `id1`, put back
  in hipnotic's pak) and a `pak1.pak` without e4m5's stray teleporter (id's own map bug:
  "couldn't find target" ended the game).
- **The edict pool** (`fleet/edicts`). `sv_max_edicts` (console only): 600 in Classic,
  8192 in 2026, sized where `SV_SpawnServer` sizes `sv.edicts`, carried across
  changelevel and restart, saves load under the live ceiling, at most 32000. With it off,
  id's "ED_Alloc: no free edicts". *Corrected 2026-10-02* (`fleet/makestatic`): the 632
  edicts Rogue's `r2m6` seemed to need were the port's own count, its 91 statics keeping
  edicts id's `makestatic` frees. It spawns under 600 in Classic, as in id's C. No map of
  id1 or the packs needs the extra; it stays as room for bigger maps.
- **Found on the way:** the e4m5 teleporter above (verified reachable: open space; the
  e4m8 one is inside solid); `verify_loops`'s audio-underrun count fails when run
  under load 7–15 on any build (it passes in a quiet window); Chrome's main-thread rAF
  cap (last round) is unchanged.
- **Not done, noted:** the user's side note that the sky could be more fluid in places
  for 2026 — likely `R_MakeSky`'s whole-texel scroll steps; a `screenshot` download;
  mission-pack cases in `oracle/screen2d.py` (the review's comparison was ad hoc); ~~whether
  Rogue's own engine raised `MAX_EDICTS` (its source was never released, and id's C can't
  run `-rogue` on r2m6 for a missing `campaign` cvar)~~ (moot: id's C runs it, at 542-546
  edicts; `fleet/packclass`, `fleet/makestatic`).

---

## 2026-09-30 to 2026-10-02: its own repo, and an Android phone

The project moved out of the repository it came from into this one, with its whole history
(`git filter-repo --subdirectory-filter`, 583 commits). Then a round of Sonnet agents, each
on a `fleet/*` branch, merged by the chair, from the user's notes after playing on a
phone.

- **Standalone** (chair). id's C source is read from `quake-c/` beside `quake-data/` (both
  ignored links); `classic_check` builds the oracle itself from a fresh checkout; check
  screenshots and unreferenced early renders left the tree.
- **Quit** (`fleet/quit`). Menu > Quit > Y and the console's `quit` end the program as
  `Sys_Quit` did: a `Quit` record (kind 18) carries `end1.bin`/`end2.bin`, the page leaves
  fullscreen, releases the pointer and keyboard locks, and draws DOS Quake's text-mode end
  screen (`web/endscreen.js`); a tap or key reloads into a fresh game.
- **Touch layout** (`fleet/touch`). The buttons sit above the status bar at any phone size
  and Screen size (`sbar_height`, from `calc_refdef`'s own arithmetic).
- **A server's own files** (`fleet/serverfiles`). An optional `files.json` lets a deploy
  offer `id1/pak1.pak` and `id1/music/trackNN.ogg`; the player's dropped files still win.
- **Video Options** (`fleet/video`). In 2026 it lists Native Auto / 1x-4x below id's
  modes, marks what is really showing, and switches either way.
- **Speed at phone resolutions** (`fleet/water`, `fleet/sched`). Liquid texels wrap with a
  mask, the warp's row multiply is in its tables, and `band::for_rows` hands out several
  runs a thread like the bands do; frames byte-identical (PERF_PLAN.md §12).
- **An Android phone, measured** (chair; adb + Chrome's remote debugging over USB). At pixel size
  1 (2640x1080, 8 threads) frames took 13 ms dry and 19 ms underwater against 60 Hz's
  16.7, with the phone throttled at ~40 C and its cores at ~40% of their clocks, and the
  audio ring ran dry. At 2x2: 8 and 9 ms, clean. So Auto now starts a phone (devicePixelRatio
  2 or more, shorter side at most 540 CSS pixels) at 2x2; the `Window` record carries the
  ratio.
- **Late frames and sound** (`fleet/audio`). In 2026 the mixer's lead grows to 2.5x a late
  host frame (at most 0.55 s) and eases back within about a second; the ring doubled.
  Classic's fixed lead is untouched. `verify_audio_resilience.py` stalls frames on purpose
  (`stall_ms`, bench builds only).
- **Parked, not merged: `fleet/present120`.** The presenter and the tick loop in a worker on
  an `OffscreenCanvas`. On an Android phone with "Force peak refresh rate", Chrome gave the
  page's main thread 60 animation frames a second and a worker canvas 120. With the
  phone's default adaptive refresh the display idles at 60 anyway, so the change (two
  writers on the input ring, asynchronous readback) buys nothing today. Its `?ratecheck`
  measures the two rates.
- **Not changed:** exact perspective stays off in 2026 (id's 16-pixel spans; it changes
  about 1% of pixels at a phone's 2x2 and costs a divide a pixel; its cost was not
  measured on a quiet machine).

---

## 2026-09-26: the 2026 push

The user's brief:
- **Extremely important:** zero dependencies, no `unsafe`, and identical to id with
  every feature off.
- **Beyond that:** "the most amazing experience ever of an idealized version of
  software-rendered Quake (but in 2026)" by default, textures never smoothed.
- **The ideas list:** do every idea on it but a fixed 72 Hz simulation with interpolated
  rendering ("sounds like a degradation").
- **Frame rate:** the game should run well independently of it, up to a 480 Hz
  monitor.
- **The code:** it matters, as a showcase.

The chair ran 17 agents, at most 4 at a time, one branch each. It merged them into
`quake/2026` (from `quake/overnight` @ `3866e1b`) after a full check each time. Its
ledger (a `PLAN.md`) is kept outside this repository; each
merge message summarises its branch. In merge order:

- **rustcheck** (`638571c`): `CODE_PLAN.md`, a measured plan for showcase-quality Rust
  (edition 2024 is a one-line change; W0a/W0b mechanical windows; refactors R1–R11).
- **hires** (`4ecb848`): the renderer at 2026 sizes. Edge `u` in 44.20 so nothing wraps
  past 2048 columns; particles and the underwater warp in proportion; Hor+; `quaketool
  shot`. Classic byte-identical.
- **audio, part 1** (`7bb709f`): id's `snd_dma.c`/`snd_mix.c`/`snd_mem.c` in the engine
  (`snd::Mixer`, no globals), sample-exact against id's C built headless
  (`oracle/sound.py`, 28/28). The 2026 mixer fixes four faults.
- **framerate** (`5360411`): `Stepping::Uncapped`. Jumps, flashes, trails and clocks
  land on id's 72 Hz values at 60–480 Hz; `quaketool framerate --check`, 22 scenarios;
  `FRAMERATE.md`. It found the Classic demo bug that `lerp` fixed.
- **platform** (`a50d8d7`): the browser build as a plain Rust program. `fn main` runs in
  a Web Worker (`wasm32-wasip1`, edition 2024, `forbid(unsafe_code)`, no exports),
  reading events on stdin and writing frames and sound on stdout. The pak and saves go
  through `std::fs` over IndexedDB. The host can run threads. The nine checks pass in
  Chromium and Firefox.
- **multicore** (`a2c2944`): the renderer owns its state (CODE_PLAN R3: `Renderer`,
  `Scene`, `begin_map`, no thread-locals) and draws a frame in row bands on N threads,
  byte-identical at any N. The browser gets threads through pooled workers; `r_threads`.
- **server** (`d5db64a`): server state out of thread-locals (R2: `Outbox`,
  `ServerCvars`, `QRand`). QuakeC errors end the game as id's `Host_Error` does
  (`PR_RunError`'s report, `error`/`objerror`; CENSUS L16).
- **settings** (`efa3bc7`, seam `207eee1`): one typed settings value (`cvar.rs`,
  `settings.rs`), a command table (`cmd.rs`) and `Bindings` (R4). The Classic and 2026
  profiles; `config.cfg` as id's writes it (a profile line plus the diffs); `?classic` /
  `?2026`; `oracle/classic_check.py`.
- **lerp** (`61008e1`, merged into settings): Classic demo playback is id's
  `CL_LerpPoint`, frame for frame (`oracle/demo_lerp.py`, 17,500 frames against id's
  client). The port had played pre-cut 60 Hz sub-frames. `r_lerpmove` smooths monsters
  in 2026.
- **audio, part 2** (`9e86bf7`): the browser plays the engine's mixer. The worker paints
  into a shared ring that an AudioWorklet plays; the page's Web Audio mixing is gone.
  `snd_modern` is the 2026 departure.
- **mobile** (`d24ff57`): touch controls (`web/touch.js`, `in_touch`), menus that answer
  taps (`Menu::tap`), pause when hidden. The PWA manifest and service worker give
  offline play and isolation headers on hosts without them; `verify_touch`.
- **content** (`2b0e970`): id's search path (`common.rs`: `pak0`..`pakN` over loose
  files) and `COM_CheckRegistered`. The player can drop in their own `pak1.pak`; the
  2021 re-release is refused with a reason. CD music plays from the player's tracks
  (`cd_audio.rs`); `verify_content`.
- **tool** (`4f43321`): `quaketool` as one directory with one `Command` table driving
  dispatch and `--help` (R7), and `Box<dyn Error>`; output byte-identical.
- **input** (`877a339`): id's `in_win.c` joystick in Classic, the 2026 twin-stick pad
  with rumble, raw mouse (`unadjustedMovement`), WASD by physical key (AZERTY works);
  `web/latency.py`.
- **present** (`1139ab1`): PERF_PLAN B5 in full. Frames are palette indices with the
  palette applied at presentation. WebGL2 is the DAC (an `R8UI` texture and a 256x1
  palette), with a 2-D canvas fallback. The threads build's frames are read where they
  lie in shared memory. Page frame at 1440p on 8 threads: 10.6 → 3.4 ms.
- **vm** (`ea13e24`): R5, typed entity fields (`MoveType`, `Solid`, `EntFlags`), with
  every engine read through resolved handles and `Option` for sentinels. R10: opcodes
  decoded at load, `Vm` private, `intern` de-duplicated, the output log drained.
  simbench −7%.
- **review** (`244bcd5`): the closing review on a frozen tree. It found no rule breaks
  and made five small fixes (the status line, dead `Pak::from_static`, the profiler's
  clock hook, stale docs and rustdoc, the crate's front page), and left the ranked list
  above.
- **docs** (this branch): README, STATUS, AUDIT (one open list, the departures table),
  quake-rs/README, CODE_PLAN, PERF_PLAN and the oracle's README made current.

**Start → end.**
- The goldens did not move: `4807aaa1` / `9ae2b478` / `c65b7046`.
- Tests: 601 + 2 + 8 → 707 + 6 + 8 (+1 doctest) in quake-rs; 135 → 172 in quake-wasm.
- Source: quake-rs 60.3k → 70.6k lines; quake-wasm 10.3k → 12.8k; the page's JS and
  HTML 1.6k → 3.9k.
- The two decisions the overnight push left for the user are settled by the profiles:
  - the four control departures are 2026 settings, and Classic has `default.cfg`'s
    bindings;
  - the 72 fps cap is Classic's alone.

## How to work here

- **The rules come first.** The five above; where a rule and a nice idea clash, the rule
  wins.
- **Prove Classic.** After any change, run `uv run oracle/classic_check.py`. A change
  that moves an identity value on purpose is a fidelity change: re-record with
  `--record --note "..."` and record the move in `AUDIT.md`, with its evidence.
- **Faithful first, in Classic.** Read id's C for the thing you are changing (the
  WinQuake tree is at `quake-c/WinQuake`)
  and name the C function in the doc comment. A change that departs from id is a new
  setting: a `departure` in `quake_rs::cvar::CVARS`, off in `Cvars::classic`, and on in
  `Cvars::modern` if it belongs in 2026. Add a row to AUDIT's departures table.
- **New code** follows `CODE_PLAN.md` §4: no new `thread_local!` or `static` state; at
  most 7 parameters; settings as typed fields; entity fields through `vm.fo()`;
  `#[expect(…, reason)]`, not `#[allow]`.
- **Benchmarks:** A/B two builds in one sitting and note the load; absolute milliseconds
  swing 2–3x with what else is running.
- **The web scripts need their `--with` flags:** `uv run --with playwright
  web/verify_walk.py DEPLOYDIR` (`verify_settings.py` also `--with pillow`). Plain `uv
  run web/x.py` ignores the shebang's flags and fails. Parallel runs take
  `QUAKE_VERIFY_PORT` so they don't collide. The scripts write screenshots into the
  directory they serve, so point them at a scratch deploy dir.
- **Commits:** never commit game data or a built `quake.wasm`.

## Quick commands

From the repository's root:

```bash
# tests and lints
(cd quake-rs && cargo test --release && cargo clippy --release --all-targets)
(cd quake-wasm && cargo test --release && cargo clippy --release --all-targets)

# Classic's proof, ten checks (goldens, play hashes, timedemo, census; and id's C: edicts, 3-D, the exact sweep with the monsters awake, 2-D, demos, sound)
uv run oracle/classic_check.py

# the uncapped game against 72 Hz
quake-rs/target/release/quaketool framerate quake-data/ID1/PAK0.PAK --check

# the browser build, a deploy dir, a check, and a server
(cd quake-wasm && cargo build --release --target wasm32-wasip1-threads)
D=$HOME/quake-web
(cd web && uv run python -c "import isolated; isolated.copy_page('$D')")
cp quake-wasm/target/wasm32-wasip1-threads/release/quake.wasm "$D/"
mkdir -p "$D/id1" && cp quake-data/ID1/PAK0.PAK "$D/id1/pak0.pak"
QUAKE_VERIFY_PORT=8561 uv run --with playwright web/verify_walk.py "$D"
miniserve -C -p 8080 --index index.html --header "Cross-Origin-Opener-Policy:same-origin" --header "Cross-Origin-Embedder-Policy:require-corp" "$D"

# benchmark: the page in headless Chromium plus a native twin (bench.py --help)
uv run --with playwright web/bench.py --build --native
```

---

## 2026-09-25: the overnight push

*As it was on 2026-09-25. Since 2026-09-26 the rule is Classic and 2026 profiles, not
"faithful by default plus opt-in Web extras". The Web extras are settings in the 2026
profile. The browser build is a WASI program: the pak is a file, not embedded; the
engine mixes the sound, not Web Audio; saves are files, not localStorage. The top of
this file is current.*

The user's brief: faithful by default (only Always Run departs; anything
else becomes an opt-in extra), three bugs they had noticed (Chthon has no electricity; the
Options cursor blinks too fast; Screen size does the wrong thing), performance, well
structured code. The chair split it into branches, one agent each, merged into
`quake/overnight` in order (`git log --first-parent 5af4fa1..31775f5`; each merge message
summarises its branch). The chair's ledger
(a `PLAN.md`) is kept outside this repository.

**The three reported bugs.**
- *Chthon's lightning* (`ce2dbf8`): boss.qc writes `TE_LIGHTNING3` to `MSG_ALL`, and the
  port read temp entities only from the broadcast buffer, so the bolts were never drawn.
  The server now parses each message buffer the way the client would. An end-to-end test
  fights Chthon on e1m7 and kills him with the lightning.
- *The Options cursor* (`962c82b`): it blinked off a 10 Hz frame counter; id's is
  `(int)(realtime*4)&1`, 4 Hz. The console cursor was 2 Hz; also 4 now.
- *Screen size* (`1e9a299`): the row had been turned into a resolution picker. It is id's
  `viewsize` again (30–120, the view shrinks inside a tiled border, 110 drops the
  inventory, 120 the status bar), with the view rendered above the status bar as
  `SCR_CalcRefdef` does: before, the view filled the screen and the bar was pasted over
  its bottom 24%, so the horizon sat too low. Resolution moved to Video Options.
  AUDIT.md: "Options menu + screen framing".

**Instruments, so "faithful" is measured instead of argued.**
- *The oracle* (`oracle/`): id's WinQuake built from the C with null drivers renders the
  same view, clock and entities as the port; `compare.py` counts matching palette
  indices. `screen2d.py` does the same for the whole composited 2-D layer.
- *The census* (`CENSUS.md`): `quaketool census` plays all nine maps headless through the
  real QuakeC, and id's own server edicts are dumped and diffed against the port's. It
  found 4 HIGH, 14 MED and 25 LOW differences.
- *The benchmark* (`web/bench.py`): the real page in headless Chromium, fixed workloads,
  per-phase timers, a native twin; `PERF_PLAN.md` holds the baseline and the plan.

**The 3-D renderer, now id's.** In four steps, each measured by the oracle
(AUDIT.md sections of the same names): id's alias-model pipeline, raw-texel liquids and
sky, the sky's layer offset, the gun's placement ("Session 7"); id's mip levels and
integer lightmap stepping ("mip levels, lightmap stepping"); the pixel aspect of a 16:10
mode on a 4:3 screen — the world had been drawn 1.2x too tall — and the x86 build's
16-pixel perspective spans ("Projection and spans"); and finally id's edge-sorted span
renderer for the world and brush models, which replaced the port's own polygon walker
("World pass: id's edge renderer"). Outcome, world pixels matching id's x86 renderer at
320x200 in the four standard views (e1m1 / e1m2 / e1m3 / e1m7): **80.39 / 59.87 / 65.33 /
67.80% when the oracle first ran → 99.96 / 99.94 / 99.98 / 99.91%**, and 100.00% at the
page's aspect; entity pixels 0.0 / 14.6 / 18.6 / 8.5% → 100%.

**The 2-D layer, now id's** ("The 2-D layer against id's composited screen"). WinQuake
draws the status bar, menus and console 1:1 in every mode; the port blew up a 320x200
screen. Now 1:1 by default (the old look is the "Scaled 2-D layer" extra), with id's
sliding console, the DOS quit prompt, and a handful of pixel offsets. `screen2d.py` at
640x400: 1–55% of 2-D pixels matched before, 100% on every shot after except three
explained residues. To keep the 1:1 bar a sensible size, the page's canvas is now the
largest 4:3 box the window fits (976x732 at 1440x900, was a fixed 640x480).

**Gameplay** ("Census client/host fixes", "Census fixes, server side"). All 18 HIGH and
MED census findings are fixed: teleporters turn the view; single player pauses behind the
menu and console; e1m8 has its low gravity; a weapon switch pressed during a cooldown is
kept; two health boxes no longer fall out of e1m1 and e1m6; the gold pickup flash; the
player is named ("player was shot by a Grunt"); level-start doors open as in id; door and
lift sounds loop until they stop; runes show on the status bar; Tab shows the scores;
weapon keys work on any keyboard layout; damage flashes in god mode; new weapons flash;
demos get trails, skins and the demo1 → demo2 → demo3 cycle. Plus 16 of the 25 LOWs, and
two in part.

**Web extras** ("Web extras: the opt-in departures"). Options > Web extras, in the slot
id's Windows build uses for its 14th row: uncapped framerate, show FPS, exact
perspective, scaled 2-D layer. Off by default, persisted by the page, each a `wasm_*`
console variable. Also: `viewsize` persists across reloads, and Esc works in fullscreen
through the Keyboard Lock API (not verified in a real browser).

**id's demo commands, `timedemo` and `pause`** ("Demo commands, timedemo, pause"; merged
last). `playdemo`, `stopdemo`, `startdemos` and `demos` as `cl_demo.c` has them, with the
attract loop running through them and id's disconnected state (the console forced up);
`timedemo` plays a demo one message per frame, uncapped, and prints id's line — the same
969 / 985 / 1090 frames as id's C for demo1–3; `pause` with `SCR_DrawPause`'s plaque,
pixel-exact against id's screen. No loading plaque, on purpose: a level loads in 6–23 ms,
inside a frame. Natively `quaketool timedemo`; in the browser, `web/verify_timedemo.py`.

**Performance** (`PERF_PLAN.md`). Mostly by doing what Quake did, which also made it more
faithful: the 72 fps `Host_FilterTime` cap; the embedded pak as one static slice (memory
81 → 53 MB after boot, 171 → 62 MB after four map loads); the palette shift as
`V_UpdatePalette`'s 256-entry ramps (a Quad frame at 1280x800: 32.8 → 17.1 ms, before the
later world-pass gains); dynamically lit walls through the surface cache; mip levels;
id's edge renderer (wasm frame −23 to −33%, native −43 to −57%); only the entities the
server would send (e1m3's 98 alias models per frame → 3.8; it also stopped muzzle flashes
lighting walls from behind); entity fields resolved once per progs instead of hashed on
every access (the e1m3 game tick 2.64 → 0.29 ms). demo1 at 1280x800 in wasm: 22.6 → 4.5
ms a frame.

**Structure.** `render.rs` (15,000 lines), `server.rs` (9,900) and quake-wasm's `lib.rs`
(8,000) were split along id's own files, byte-identical, one agent per file. Then the game
client moved from quake-wasm into `quake_rs::client` (quake-wasm's code 6.3k → 3.0k lines),
so `quaketool play` runs the browser's client natively and prints the same frame hashes.
The polygon walker was deleted once the edge renderer won (−2.5k lines). The source grew
from 52,000 to 67,000 lines.

**Reviews.** Two adversarial reviews of the merged tree; their findings were fixed on
`quake/polish` ("Review fixes"), `quake/polish2` and `quake/polish3` ("Second review
fixes", both sides). The fixes a player could see: loading a save no longer resets the
options; the underwater view is rendered in id's 320x200 warp buffer; the client clock is
`cl.time`; `sv.time` is a double (the new-weapon flash was a frame early); particles are
drawn as `D_DrawParticle`; every menu keeps its own cursor; carried weapons no longer flash
at each level start; a busy frame can no longer leave a door hum looping forever.

**Goldens.** They moved with each deliberate fidelity fix, every move recorded in
AUDIT.md with its pixel count: `fb14bd65` / `a6f98d8a` / `0211e6d4` at the start →
`4807aaa1` / `9ae2b478` / `c65b7046` at `3ba835f` and still at `31775f5`.

**Tests.** 475 library + 8 integration and 64 quake-wasm tests at the start; 587 + 1 + 8
and 126 at `31775f5`.


---

# History before 2026-09-25

Kept as evidence. Numbers and file names below are as they were then; where
something has since been superseded it says so in place.

## Demo playback parity (2026-06-11, branch `ship/demo-parity`)

User (twice): the attract demo must match the real game — sound, status bar,
everything. In the C the demo IS the game client rendering a recorded stream;
this port's demo path decoded the stream but discarded most of it. Closed in
4 commits (`6b4417c`/`bd806d1`/`e58997a`/`8306aed`):

- **Decode side** (`quake-rs/src/demo.rs`, vs cl_parse.c/view.c): svc_sound per
  CL_ParseStartSoundPacket (SND_VOLUME default 255, atten byte/64 default 1.0,
  ent=ch>>3 / chan=ch&7, precache-resolved sample — id's demo1 carried **595
  recorded one-shots that were silently discarded**); svc_stopsound;
  svc_lightstyle into a per-demo copy-on-write `cl_lightstyle` table;
  svc_clientdata per CL_ParseClientdata's EXACT bit order (viewheight/
  idealpitch, punch char + velocity char*16 interleaved, items, ONGROUND/
  INWATER, weaponframe/armor/weapon + the fixed health/ammo/active-weapon
  trailer); svc_damage (V_ParseDamage); svc_print/centerprint; svc_stufftext
  consumed-inert with the C's would-exec documented.
- **Frame emission gates on signon completion** (the first entity fast-update
  is "the final signon stage", cl_parse.c:340) — the ~1.2 s of void-camera
  frames at demo start AND loop wrap are gone at the source; the wrap also
  resets the damage/kick/notify/oldz POV state.
- **Playback side** (`quake-wasm` step_demo): recorded one-shots +
  CL_ParseTEnt impact sounds queue through the SAME `queue_sounds` spatialized
  path as live (listener = recorded camera); recorded-stats `render::Hud`
  sbar; SU_WEAPON/SU_WEAPONFRAME weapon viewmodel with the R_DrawViewModel
  hide gates; recorded lightstyle flicker via the shared
  `server::lightstyle_scales_at` (literal R_AnimateLight math, proven
  byte-identical to the live path); V_ParseDamage flash + directional kick
  (v_kickroll/v_kickpitch 0.6, v_kicktime 0.5) decayed in V_CalcViewRoll;
  notify/centerprint overlays (Con_Print '\n' accumulation, same gating as
  live); full V_CalcRefdef camera (V_CalcBob from recorded SU_VELOCITY, oldz
  stair smoothing on recorded SU_ONGROUND, strafe lean, dead-view roll=80
  assignment semantics, punchangle added LAST); underwater D_WarpScreen +
  content/damage/powerup blends deferred to the dispatcher like live.
- **Sound-channel override CLOSED for live + demo** (was a known-deferred
  LOW): the page-side (entity,channel) playing-source registry in
  `web/index.html` implements SND_PickChannel's cross-frame "always override
  sound from same entity" + S_StopSound, fed by new `sound_entity`/
  `sound_channel`/`poll_stop_sound` exports. id's demo1/2/3 send **zero**
  svc_stopsound (engine-asserted census) — the stop path is protocol
  completeness; the override fix is live behaviour.
- **Evidence:** `web/verify_demo.py` (new permanent headless-Chromium harness,
  9/9: first rendered frame in-world, one-shots actually play via the page's
  `__sndStats` counter, sbar band drawn + lit, stop/override drain clean, zero
  console errors) + verify_walk/verify_ambient green on the same assembly;
  **458 lib + 48 wasm tests** (real-demo1 decode census, zero-stopsound
  census, loop-wrap seam, damage math, sbar A/B pixel diff); clippy 0/0;
  goldens byte-identical `fb14bd65`/`a6f98d8a`/`0211e6d4` (scene renders no
  demos).

---

## ⭐ SHIP PUSH (2026-06-10) — six features landed in one coordinated push

A 6-branch parallel implementation (each adversarially reviewed by 2–3
independent lenses, blocking findings fixed on-branch, then merged serially
with tests + goldens verified after every merge). **449 lib + 42 wasm tests
pass; goldens byte-identical throughout (`fb14bd65`/`a6f98d8a`/`0211e6d4`).**

1. **Death→respawn CLOSED and PROVEN** (was the one gameplay-breaking gap).
   The full QuakeC chain runs on the real progs.dat: T_Damage → Killed →
   PlayerDie (deadflag=DYING, movetype=TOSS) → death-anim thinks while dead →
   DEAD_DEAD → PlayerDeathThink (DEAD_RESPAWNABLE on button release) → +attack
   → respawn() → localcmd("restart") → try_restart reloads with level-ENTRY
   parms. e2e wasm tests cover a self-rocket kill AND an environment (slime)
   kill. Two fidelity fixes en route: console `kill` was a health hack → now
   ports Host_Kill_f via `Server::client_kill` (runs QC ClientKill); and
   SV_Physics_Client's TOSS/BOUNCE arm was missing (dead corpse froze mid-air)
   → routes to physics_toss per sv_phys.c.
2. **Intermission + finale screens** (QC-driven, faithful svc flow). Root
   causes: WriteByte/WriteString builtins dropped every MSG_ALL write (engine
   never saw svc_intermission/svc_finale), AND the port never did
   SV_SpawnServer's world-edict setup — `world.model`/`mapname` were empty so
   QC's episode-end check could never match (new `Server::set_map_name`).
   V_CalcIntermissionRefdef camera, Sbar_IntermissionOverlay (complete/inter
   plaques + big numbers: Time min:sec, Secrets, Kills), e1m7 finale text at
   8 chars/sec + CONGRATULATIONS plaque, svc_sellscreen → Help menu. Demo path
   carries cl.stats + intermission state. Review fix: completed_time latches
   the QC `time` global (epoch 1.0, = cl.time) not w.clock (epoch 0) — the
   displayed Time was 1s low; co-fixed the death-scoreboard clock.
3. **Lightning/beam temp entities render** (TE_LIGHTNING1/2/3 + TE_BEAM; was
   the deferred HIGH). New `quake-rs/src/tent.rs` ports cl_tent.c: CL_ParseBeam
   slot store (24 slots, same-entity replacement so a held thunderbolt is ONE
   refreshing beam), CL_UpdateTEnts expansion (bolt piece per 30 units, exact
   integer vectoangles, rand()%360 roll), view-entity re-anchoring. Live +
   demo paths. Bonus fix: PF_WriteEntity read its parm as float instead of an
   int global (would have collapsed all beams onto one slot).
4. **Ambient sounds (H11 — the LAST audit HIGH) closed.** PF_ambientsound →
   StaticSound registry (vol/atten byte-quantized exactly like
   svc_spawnstaticsound) + `Demo::static_sounds`; new `quake-rs/src/snd.rs`
   ports S_UpdateAmbientSounds (water1/wind2 only, like the C) + GetWavinfo
   (cue-chunk loop gate). Web: per-static looping sources through the same
   spatialGain law as one-shots; `sound_generation()` lifecycle stops every
   loop on boot/New Game/map/changelevel/restart/demo↔walk. Review fix: the
   ambient ramp runs the C's literal INTEGER master_vol math on a fixed 1/72s
   accumulator (Host_FilterTime's cap) — faithful asymmetric fade at any
   display refresh rate, no high-Hz stall. Also: ambience/* one-shots
   un-silenced (E1M6 wind tunnels), PF_ambientsound precache gate per the C.
   Ground truth: e1m1 = 14-loop base soundscape (fl_hum1/comp1 — no torches),
   e1m2 = 24 fire1.wav torches.
5. **First-frames texture/lighting "pop" root-caused and fixed** — caveat 5's
   cold-cache guess was WRONG (the surf cache bakes synchronously; can't pop).
   Two real causes: (a) frame 0 rendered with ZERO physics after spawn, so the
   player's ~5u spawn-settle fall played on camera — `Server::run_signon_frames`
   ports the C's two signon SV_Physics ticks (0.1s each), called by every wasm
   walk-building path (boot/New Game/map/changelevel/restart; quaketool CLI
   deliberately unchanged); (b) `any_dlight_reaches` was plane-distance-only,
   so ANY dlight anywhere kicked all near-coplanar in-view faces off the baked
   surface cache onto the per-pixel path (13.3% whole-view shimmer ~0.55s
   after New Game from a distant lavaball trail). Headless before/after:
   17.8% + 13.3% pops → max 0.86% (sky scroll + flicker only).
6. **R_MarkLights BSP dlight gating** (deferred-twice MED): per-face u32
   dlightbits via faithful sphere-vs-plane node recursion (world: headnode[0];
   inline submodels: own headnode, entity-local origins (world-space since `quake/polish4a`, as id's) per
   R_DrawBEntitiesOnList). A/B on e1m1 with an injected light: 90,525 affected
   px → 10,411 (80,114 bleed px removed, 0 added — strict subset). Composed
   at merge with (5): mask (C-faithful "may contribute") → plane test → luxel-
   extent test (port-specific CACHE-PATH tightening only — the C keys rebuilds
   on marking alone but always renders through its surface cache; we'd flip
   baked→per-pixel and shimmer for zero pixel change). New debug knob:
   `QUAKE_DLIGHT=x,y,z,r|eye[:r]` on quaketool scene injects a light for A/B.

**Also landed the same session:** an idiomatic-Rust pass (cargo clippy
--all-targets = **0 warnings in both crates**, proven byte-identical: goldens
+ simbench VM-stmt counts unchanged), the **HTML shell redesign** (product
presentation, streaming download progress, structured help, favicon,
touch-screen notice, audio-state button — single dependency-free file), the
**deployed `web/quake_wasm.wasm` rebuilt at HEAD**, and a 5-lens final
whole-diff review (integration / faithfulness / regression+determinism /
docs / real-browser playtest — the playtest proved all six features live in
headless Chromium with zero console errors, including flying to e1m1's real
exit trigger for a genuine intermission). Verdicts: ship. Remaining LOWs
recorded below.

---

## TL;DR (pre-push state, 2026-05-31)

*Superseded: the renderer described here (and every timing below) was replaced on
2026-09-25; the surface-cache fix itself still stands (`surf.rs`).*

- The shareware episode (E1) is playable in the browser; **401 lib + 25 wasm tests pass**.
- **⭐ THE render bottleneck is FOUND AND FIXED.** The prior session's "fixed ~60 ms
  per-face cost, NOT per-pixel" lead was exactly right. Root cause: the lit-surface
  cache was a single thread-local slot **shared between the world pass and the external
  brush-model pass** (each `b_*.bsp` item box is a distinct `Bsp` with its own
  fingerprint + face count), so every external box reset the 5000-entry world cache and
  the world **re-baked every face every frame**. Fixed by making external models
  **bypass** the surface cache (they re-clone their `Bsp` every frame, so they could never
  hit it) and keeping a **single world-model slot** for the world + its inline submodels
  (commit `30c6866` + adversarial-review follow-up). Result on an **idle** host, e1m1:

  | res | before | after | speedup |
  |-----|--------|-------|---------|
  | 64×48 | 65.3 ms | **1.14 ms** | 57× |
  | 320×200 | 65.2 ms | **2.28 ms** | 29× |
  | 640×400 | ~68 ms | **4.97 ms** | ~14× |
  | 1920×1080 | 89.5 ms (11 fps) | **27.97 ms (36 fps)** | **3.2×** |

  **Byte-identical** output (e1m1/e1m2/e1m3 golden sha256 unchanged at
  `fb14bd65`/`a6f98d8a`/`0211e6d4`). The frame now scales with resolution (per-pixel
  bound) as a software renderer should. A regression test
  (`external_models_bypass_and_dont_evict_world_surf_cache`, which renders 26 distinct
  external models — past where the first-attempt LRU broke) guards it.
- **New benchmarks** cover both halves of the engine: the render `QUAKE_BENCH` harness
  gained world-pass **sub-phase timers** (pvs/sort/setup/lightmap/surf + cache hit/rebake
  counters — these localised the bug), and a new **`quaketool simbench`** measures the
  **game-logic** tick (physics/VM/AI/collision) with VM-statement + BSP-trace counts
  (commit `0591b70`).
- The deployed `web/quake_wasm.wasm` is **rebuilt + current** (headless-verified booting
  e1m1, no errors).

---

## Resolution: higher default + it now PERSISTS (2026-06-09)

*Superseded in part (2026-09-25): the Options row called "Screen size" here is id's
`viewsize` again, and the resolution list lives in Options > Video Options. The 960x600
default, the 16:10 presets and the localStorage persistence stand.*

User report: picking a resolution in Options and then starting the game reverted it
to the default. Root cause: `boot()` / `boot_demo()` / `boot_attract()` and the
**New Game** branch of `menu_select()` each force-called
`set_render_size(DEFAULT_W, DEFAULT_H)` (and `Menu::new()`), throwing away the
menu-picked size every mode transition. (The framebuffer is the documented source of
truth and `step()` already syncs the menu label to it each frame — so the *only*
thing reverting the size was those four explicit resets.)

**Fix:**
- Those four resets now **preserve** the live framebuffer and instead call
  `a.menu.sync_resolution(render_w, render_h)` so the fresh menu's "Screen size"
  preset tracks the preserved size. `set_resolution` (export) also syncs the label
  now, so a programmatic set can't leave it stale.
- **Default bumped 320×200 → 960×600** (`DEFAULT_W/DEFAULT_H`; must stay a
  `RESOLUTION_PRESETS` member so the label can sync). Per the user's pick.
- `RESOLUTION_PRESETS` extended to a consistent 16:10 ladder up to the **1280×800**
  clamp cap: `320×200 … 960×600, 1120×700, 1280×800` (7 presets).
- **Cross-reload persistence** (`web/index.html`): the chosen size is saved to
  `localStorage['quake-rs.resolution']` as `"WxH"` in `syncCanvasSize()` (the single
  choke point for size changes) and restored after `boot_attract()` via
  `restoreResolution()` (hands the raw value to `set_resolution`, which clamps — a
  stale/garbage value can never break boot).
- The `map <name>` console path (`run_map_command`) already preserved the framebuffer;
  it now also eagerly `sync_resolution`s the fresh menu's label (was the one
  `Menu::new()` site relying on `step()`'s per-frame sync — surfaced by an adversarial
  multi-lens review of the diff; the other 3 lenses found nothing).

**Verified:** 401 lib + 26 wasm tests pass (added
`chosen_resolution_persists_across_reboot`; updated the preset-cycle + clamp tests).
End-to-end headless Chromium check (a one-off
`verify_resolution.py`, 9/9 pass): fresh boot = 960×600, persists to localStorage,
reload restores a picked 640×400, **walk/boot + New Game both preserve it** (the bug),
oversized saved value clamps to 1280×800, garbage falls back to default, no console
errors. `web/quake_wasm.wasm` rebuilt + deployed.

---

## ⭐ PERFORMANCE — render bottleneck RESOLVED (was the "key lead")

*2026-05-31. The triangle rasteriser these numbers measure was replaced on 2026-09-25
(polygon spans, then id's edge renderer); current numbers are in `PERF_PLAN.md`.*

The prior session's lead was correct: **a FIXED per-face cost independent of resolution**
dominated the frame. With the host **idle** (load 0.4/16 — the prior session's "throttling"
was likely partly this same fixed cost making all resolutions look uniformly slow), a
resolution sweep + new world-pass **sub-phase timers** pinned it exactly:

```
e1m1 @ 64×48, BEFORE:  world 61.1 ms  (surf phase 59.4 ms!)  — 0 cache true-hits, 942 REBAKES/frame
e1m1 @ 64×48, AFTER:   world  0.76 ms (surf phase  0.03 ms)  — 942 true-hits, 0 rebakes
```

**Root cause:** `face_surf_block`'s `SURF_CACHE` was a single thread-local slot keyed by
one `WorldFingerprint`. It is shared by the world pass **and** the external brush-model
pass — and every external `b_*.bsp` item box is a *separate* `Bsp` (distinct pointer
**and** `faces.len()=6` vs the world's `5516`). So each box `needs_reset` wiped the whole
world cache to a 6-entry one, the next frame's world pass wiped it back, and **every world
face re-baked its texture×lightmap×colormap block every frame** — the entire ~60 ms.
(The geom + lightmap caches were spared only because the world pass alone touches them.)

**Fix (`30c6866` + review follow-up):** A first attempt keyed a small per-model LRU set by
fingerprint; an adversarial review then found that the game **re-clones each external item
box's `Bsp` every frame**, so externals get a fresh fingerprint every frame and instance —
they can *never* hit the cache and, 24+ at once, would still evict the world cache (the
regression returns). The shipped fix: external models **bypass** the surface cache (bake
fresh each frame — a 6-face box is ~free), and the world + its inline submodels (which
share the one world `Bsp`) use a **single slot** that self-invalidates on a changelevel.
So exactly one model is ever cached and it can never be evicted. Byte-identical (goldens
unchanged). Regression test: `external_models_bypass_and_dont_evict_world_surf_cache`. The
bench's surf line now splits `cached-rebakes` (should be ~0 warm) from `external-bypass-bakes`
(expected, one per visible item-box face).

## Performance scorecard (2026-05-31, idle host, e1m1; superseded by `PERF_PLAN.md`)

| res | before | after | speedup | phase split (after) |
|-----|--------|-------|---------|---------------------|
| 64×48 | 65.3 ms | **1.14 ms** | 57× | world 0.76 (sort .15, setup .46, surf .03) |
| 320×200 | 65.2 ms | **2.28 ms** | 29× | world 1.65 |
| 640×400 | ~68 ms | **4.97 ms** | ~14× | world 3.90 |
| 1920×1080 | 89.5 ms (11 fps) | **27.97 ms (36 fps)** | **3.2×** | world 23.6, submodel 2.1, alias 1.8 |

The 2× target is **beaten** (3.2× @1080p, much more at low res). The frame is now genuinely
**per-pixel bound** — the remaining ~23.6 ms @1080p world is real shading work (2.5M px ×
texel-read + lightmap + palette/colormap + z-test), and overdraw is ~1.0× (culling optimal).

### ⭐ Clone/alloc hunt (byte-identical wins) — the SIM nearly DOUBLED

An adversarial per-frame/per-tick clone+alloc hunt (15 confirmed findings) landed the
following byte-identical removals (render goldens unchanged; sim VM-stmt/trace/think counts
IDENTICAL run-to-run, proving behaviour is unchanged):

| sim ms/frame | before | after |
|--------------|--------|-------|
| e1m1 | 1.42 | **0.82** (1.7×) |
| e1m3 | 5.00 | **2.44** (2.05×) |
| start | 0.44 | **0.29** |

- **🔥 `build_hull` rebuilt the ENTIRE clip-node table on every trace** (`world.rs`) — thousands
  of entries, hundreds of times per tick. Now cached per-world (`HULL_CACHE`, fingerprint-keyed
  like the render caches); `Hull.clipnodes` is an `Rc<Vec<ClipNode>>`. This is the bulk of the
  sim win. (`commit 321eff7`)
- `FaceGeom.poly` → `Rc<Vec<Vec3>>` (was deep-cloned ~2× per face/frame; ~5% at browser res).
- `clip_poly_near` writes a reused scratch buffer (no per-face `Vec`); VM `EqS/NeS/NotS` compare
  borrowed `&str` (no per-op `String`); `sv_move` borrows the `*N` model name; `fly_move` uses a
  fixed `[Vec3;5]`. (`commit 2216cc2`)

Render @ browser res now: 320×200 **2.81 ms (355 fps)**, 640×400 **5.49 ms (182 fps)**,
1280×800 (the wasm cap) **15.5 ms (65 fps)**.

### Next perf ideas (2026-05-31) — superseded by `PERF_PLAN.md`

The list (clone-hunt leftovers, caching inline submodels' lightmaps, `simd128`, 16-pixel
spans, mip selection) was re-measured on 2026-09-25: `simd128` gave nothing (it does since the
span loops got tight: the browser builds use it, PERF_PLAN.md §15), the 16-pixel
spans and the mip levels are done (as fidelity fixes), and `draw_submodel` and
`compute_visible_faces` no longer exist (id's edge renderer draws the brush models).

### Profiler / benchmarks

(Current tools; the numbers in this bullet list are 2026-05-31's.)

- **Browser (since 2026-09-25): `uv run --with playwright web/bench.py --build --native`** —
  the real page in headless Chromium, deterministic workloads at dt=1/72, per-phase
  median/p95 for wasm and its native twin (see its docstring). `PERF_PLAN.md` has the
  measurements.
- **Render:** `render/stats.rs` is an opt-in `RenderStats` (per-phase ns timers and face,
  pixel, surface-cache and alias counters), zero-cost when off, surfaced by
  `QUAKE_BENCH=<iters> QUAKE_RES=WxH quaketool scene <pak> <map> <out>` and by `bench.py`.
  Wrap any new per-face work in it to keep the cost honest.
- **Sim:** `quaketool simbench <pak> <map> [frames]` benchmarks the game-logic tick (no
  rendering): per-frame ms + VM statements + BSP traces + thinks. Deterministic
  (same counts run-to-run). After the hull-cache fix: e1m1 ≈ 0.82 ms/frame; e1m3 ≈ 2.44 ms/frame
  (522 traces/frame). Since the 2026-09-25 field-offset work (PERF_PLAN D2): e1m1 0.11,
  e1m3 0.29 ms per tick.

---

## ⚠️ Caveats / debt

1. *Superseded: the goldens are `790c53d3` / `3684efc6` / `e18bb516` since 2026-10-05,
   `4807aaa1` / `9ae2b478` / `c65b7046` from 2026-09-25 (top of this file).*
   **Golden baseline is `fb14bd65` / `a6f98d8a` / `0211e6d4`**
   (e1m1/e1m2/e1m3, post submodel cache). The submodel surface cache (`d2fc0d6`) was re-verified
   **byte-identical** to its parent (0 px diff on e1m1), so its "byte-identical"
   message is correct. (Note: each *world*-cache texel-baking step earlier in the
   history legitimately re-baselined the golden; that's expected and faithful —
   texel-resolution lighting is what Quake's software renderer actually does.)
2. **`ae3ba68` (linear-step perspective) really is NOT byte-identical** — it shifts
   ~57 px (sub-pixel edge ULP drift); corrected by `3fce66c`. That one stands. *(Moot
   since 2026-09-25: that rasteriser is deleted.)*
3. **Benchmark absolute numbers swing with host load.** This session's numbers were
   taken on an **idle** host (load ~0.4/16) and are trustworthy; the prior session's
   "throttling" was likely partly the fixed surf-rebake cost (now fixed) making every
   resolution look uniformly slow. Still: re-check `uptime` before trusting ms, and A/B
   in one sitting.
4. The `quaketool` bench's submodel profiler line label may lag the actual counters
   (cosmetic only; the numbers are right). *(Not re-checked since the edge renderer
   replaced the submodel pass.)*
5. ~~**Texture "pop" on first frames**~~ — RESOLVED in the ship push (see top).
   The cold-cache guess was wrong; the real causes were the unsettled spawn
   rendering on camera + plane-only dlight gating. Lesson: the surf cache
   bakes synchronously on first visibility — it cannot pop by itself.

---

## ~~WIP — single-player respawn~~ — ✅ CLOSED AND PROVEN (ship push, see top)

The plumbing (`bi_localcmd` → `RESTART_REQUEST` → `take_pending_restart` →
`try_restart`) was sound; the e2e chain is now driven and asserted by wasm
tests (`real_death_chain_respawns_via_restart`,
`environment_slime_kill_enters_the_same_death_chain`) with a per-frame
deadflag trace. The two real gaps were the `kill` health-hack and the missing
client TOSS physics arm — both fixed (details in the ship-push section).

---

## Known-deferred items (LOW severity, documented in AUDIT.md)

*As of 2026-06-11. The current list is `AUDIT.md`, "Open, as of 2026-09-26"; the
marks added below say what happened to each since.*

All previous HIGH/MED deferred items (lightning beams, R_MarkLights gating,
intermission/finale, ambient sounds, texture pop) shipped in the 2026-06-10
push. What remains is the LOW tail, all reviewer-vetted as non-blocking:

- Demo explosion dlight (demo path emits no dynamic lights). *Still open.*
- quaketool CLI walk paths skip the signon settle (deliberate — keeps CLI
  artifacts byte-stable; unify later with a walk-golden re-baseline).
- Console `kill` drains only a pending restart, not a same-frame changelevel
  (unreachable conflict with vanilla progs).
- The C's CL_UpdateTEnts MAX_VISEDICTS half-cap and its outer-loop index
  clobber (UB in the C) are deliberately not modeled.
- ~~Sound-channel override only dedups within a frame~~ — ✅ CLOSED
  (demo-parity branch, 2026-06-11): the page-side (entity,channel) registry
  implements SND_PickChannel's cross-frame override + S_StopSound, live + demo.
- ~~Per-ammo sbar nits; pain-frame face anim~~ — ✅ the status bar matches id's
  composited screen pixel for pixel since 2026-09-25 (`oracle/screen2d.py`), the pain
  face with census F16; assorted Round-2 LOW list items (in AUDIT.md's open list).
- From the final whole-diff review (all vetted non-blocking): the demo loop
  wrap keeps the ambient ramp warm (deliberate seamless loop; the C's restart
  re-ramps from 0); ~~the intermission idle-sway phase uses w.clock (constant,
  invisible phase offset vs cl.time)~~ — ✅ 2026-09-25 (`quake/polish`: the live
  client clock is `cl.time`); ~~demo1 playback shows ~1.2 s of
  void-camera frames at start/loop-wrap~~ — ✅ CLOSED (demo-parity branch:
  frame emission now gates on signon completion, so the void frames are never
  emitted; loop wrap equally clean, seam-tested); ~~submodel dlight marking uses
  entity-local light origins where the C used world-space~~ — ✅ CLOSED
  (`quake/polish4a`: world-space lights, as id's; a moved door is lit as if
  it had not moved, AUDIT.md "Final review fixes, engine side").


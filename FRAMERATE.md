# Frame rate: the same game from 60 to 480 Hz

id's WinQuake runs at most 72 host frames a second (`Host_FilterTime`), and the
game was tuned there. The port's "Uncapped framerate" extra runs one host frame
per display refresh — server and client, at the display's rate, no fixed tick —
and until this branch it ran id's per-frame code at that rate. This page
measures what that changes, says what was fixed and what is left, and how to
rerun the measurement. (Branch `q26/framerate`, 2026-09-26.)

## In short

- **Measured:** `quaketool framerate` plays 22 scripted scenarios on the
  shareware maps through the browser's own client frame, at 72 Hz with id's
  code (the reference) and at 60, 144, 240, 480 Hz and a jittery 144 Hz
  display, each with id's per-frame code and with the fixed uncapped step. The
  tables are below.
- **What drifted with the frame rate** (id's code, uncapped, at 480 Hz against
  72): jumps peaked 1.6 units higher (45.3 against 43.7; a 44-unit ledge
  becomes reachable), everything that falls flew higher and further (grenades
  landed 23 units further), bonus flashes lasted a third as long and damage
  flashes half, a slow gib trailed 7 times as much blood and a grenade twice
  the smoke, riders bounced off lifts, and clocks that add a frame at a time
  in `f32` ran 5.5% fast an hour in (they stop at 18 hours). (Demos moved
  their camera 58 times a second on a 480 Hz display: the port's Classic
  playback, not id's, whose client interpolates at any rate — fixed in
  Classic on `q26/lerp`.)
- **Fixed, in the uncapped path only** (`Stepping::Uncapped`; Classic, the
  72 fps gate, is byte-identical): all of the above. Every quantity the
  harness measures now matches 72 Hz within a stated tolerance, and every
  tolerance wider than a frame's sampling says what id's own game varies by
  (the phase of its 72 Hz frames) or which small drift is accepted.
- **Left, and why** — see "What is left": the ground acceleration and
  friction (a fixed 2–3 units over a run, 5% of a slide), swimming (2–3% over
  a second, QuakeC's drag), QuakeC timers that a frame rounds up (lava burns
  4% faster at 480 Hz than at 72, a fall-damage threshold 4.5 units lower),
  pusher think transitions (up to a 72 Hz frame each in id's game), air
  control (not measured).
- **Budget:** natively, one core, at 1280x800 a live frame costs 1.6 ms
  (median; p95 2.4) — inside 480 Hz's 2.08 ms at the median — and a demo
  frame 2.2 ms. 1280x1024 and the extrapolated 1920x1080 (3.2 ms) fit 240 Hz
  but not 480. Details under "Budget".
- **Not yet in the browser:** the platform owns `quake-wasm`; the few
  lines that switch the page's uncapped extra onto this path are under
  "Wiring". Flipping the uncapped default is a later decision.

## How the uncapped step works

The simulation still steps at the display rate. `Stepping` (`quake-rs/src/stepping.rs`)
tells the server and client how a frame of any length `dt` steps each
per-frame integrator that drifts, so that it does what a run of 1/72 s frames
covering the same time does in id's game:

| what | id's per-frame code | uncapped | where |
|---|---|---|---|
| gravity (player, monsters' leaps, grenades, gibs, corpses) | semi-implicit Euler: a 72 Hz trajectory runs `g·t/144` below the parabola, a 480 Hz one `g·t/960` | the move leads the vertical velocity by `g·(dt − 1/72)/2`, which lands on id's 72 Hz curve at every rate; a bounce clips the led velocity, as a 72 Hz frame bounces with the speed its frame ends at | `server::sv_phys` (`gravity_lead`, `move_with_lead`) |
| damage and bonus flashes | `int` percents lose a truncation every frame: at least 1 a frame | id's drop per whole 1/72 s tick (`Tick72`) | `client::view::fade_cshifts` |
| trails (rockets, grenades, gibs, tracers) | `R_RocketTrail` drops at least one particle a frame | the particles one 72 Hz frame would drop at the entity's speed, spread evenly, the spacing carried from frame to frame | `particles::ParticleSystem::spawn_trail` |
| `host_time` (notify, centre prints), pushers' `ltime` | `f32 += dt` | kept beside a double (`advance_clock`) | `client::cl_main`, `server::sv_phys` |
| the gate | `Host_FilterTime` without its cap clamps a frame under 1 ms up to 1 ms, so past 1000 Hz the game outran the clock | `host_filter_time_display`: id's gate with the cap at 1000 fps; a closer refresh skips | `client::host` |

What needed nothing: QuakeC thinks (`SV_RunThink` runs each at its own
`nextthink` time, so monster AI, animation and weapon cadence are the same at
every rate — the grunt's fight is identical), pushers' paths (`SV_Physics_Pusher`
moves them exactly to their think times), everything drawn from `cl.time`
(view bob and roll, light styles, sky and water, the intermission sway),
linear fades (the view kick, dlights, stair smoothing, the punch angle),
the ambient sounds, which already step in 1/72 s ticks (`snd.rs`), and demo
playback: id's `CL_LerpPoint` draws every frame between the two newest
recorded messages at any rate (`client::cl_demo::demo_frame`; the port's
Classic playback did not until `q26/lerp`).

Tried and dropped (return on complexity): the exponential particle
velocities (`pt_explode`'s `vel += vel·dvel`) and the ground and water
friction stepped exactly. They moved an explosion cloud 1%, a slide 1.3 of
its 3 units and a swim nothing measurable; "What is left" says why the rest
can stay.

## What is left

All small; each is a per-frame rounding in id's own game that is finer at a
higher rate, or a fixed offset under the size of a frame's travel.

- **Ground acceleration and friction.** id's frame moves at the speed it
  ends with, so a 72 Hz start is about half a frame ahead: 2.6 units behind
  at 480 Hz after a second's run (301 against 304), and the explicit friction
  decay lets a 480 Hz slide go 3 units (5%) further. Fixed offsets under a
  frame's travel at 72 Hz (4.4 units); not something a player can see.
- **Swimming.** QuakeC's `WaterMove` drag (`velocity -= 0.8·waterlevel·
  frametime·velocity`) and the engine's water friction are explicit decays:
  2–3% further in a second at 480 Hz. Stepping the friction exactly changed
  nothing measurable, and the drag is QuakeC's.
- **QuakeC timers a frame rounds up.** Lava burns when `time` passes
  `dmgtime = time + 0.2`, which a frame's end rounds up: every 15th 72 Hz
  frame (0.208 s), every 97th at 480 Hz (0.202 s) — 25 burns in 5 s instead
  of 24. At 480 Hz the timer is closer to QuakeC's 0.2 s than id's 72 Hz was.
- **Landings.** QuakeC judges a landing by `jump_flag`, the speed at the end
  of the last frame in the air — up to a frame of gravity (11 u/s at 72 Hz)
  short of the impact. At 480 Hz it is closer to the impact speed, so the
  fall-damage threshold is 4.5 units lower (268.6 against 273.1) and the
  landing sound's the same (58.3). id's own threshold moves by up to 9 units
  with the fall's phase against the 72 Hz frames.
- **Pusher think transitions.** A pusher's think ends its frame's move, so
  each think (a door opening, its wait over, closed) delays what follows by
  up to a frame: up to 14 ms each at 72 Hz, 2 ms at 480. A door closes 20–30 ms
  sooner at 480 Hz.
- **Bounce and landing phases.** Where a bouncing grenade stops depends on
  the speed each bounce leaves the floor with, which at 72 Hz varies with the
  impact's phase by up to half a frame of gravity: the grenade stops 5 units
  short of 72 Hz's (tolerance 8.5, id's own spread).
- **Flash phase.** An uncapped flash drops on the next 1/72 s tick, not in
  the frame of the hit: its first frames show 3 percent more and it ends up
  to one tick (14 ms) later.
- **Not measured.** Air control: `SV_AirAccelerate` caps the gain per frame
  along the wish direction at 30 u/s, so strafe-jumping gains more at higher
  rates (a QuakeWorld-era phenomenon); plain air control (30 u/s sideways) is
  the same. A blocked door with `wait -1` calls its `blocked` damage every
  frame, so it crushes faster at a higher rate (no shareware door found that
  does this to a player). Sound start times are quantized to the frame
  (finer at higher rates).
- **Numerics, still `f32`.** QuakeC's `time` is a float, as in id's: at
  16384 s (4.5 hours) into one level its resolution is 2 ms, a 480 Hz frame;
  thinks still fire (they compare against the double `sv.time`), one frame
  early or late. The page's menu spinner clock (`quake-wasm`, `a.clock += dt`
  in `f32`) freezes after 18 hours at 480 Hz; the platform's to move to a
  double.
- **Classic, found on the way:** id's client interpolates demo playback at
  any rate (`CL_LerpPoint`); the port's Classic path showed each recorded
  message until the next (58 camera moves a second where id's show one a
  frame). Fixed on `q26/lerp`, frame for frame against id's client
  (`oracle/demo_lerp.py`).

## The tables

`quaketool framerate <pak> --markdown`, native, 2026-09-26. Each cell is id's
per-frame code at that rate → the uncapped step (its difference from 72 Hz);
the last column is `--check`'s tolerance. 72 Hz is the reference (id's code at
72 is WinQuake). `jitter` is a 144 Hz display whose refresh intervals wander
±40% and drop one in 50.

**jump** — a standing jump on e1m1's flat floor

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| apex (u) | 43.70 | 43.33 → 43.70 (0) | 44.63 → 43.70 (0) | 45.00 → 43.71 (+0.002) | 45.28 → 43.71 (+0.003) | 44.55 → 43.71 (+0.003) | ±0.100 |
| air time (s) | 0.667 | 0.667 → 0.667 (0) | 0.674 → 0.667 (0) | 0.675 → 0.662 (−0.004) | 0.673 → 0.663 (−0.004) | 0.673 → 0.665 (−0.002) | ±0.014 |
| last airborne speed (u/s) | 252.2 | 250.0 → 250.0 (−2.22) | 263.3 → 257.8 (+5.56) | 266.7 → 256.7 (+4.44) | 266.7 → 258.3 (+6.11) | 262.1 → 257.2 (+4.97) | ±12.0 |

**runjump** — a running jump (full speed) on e1m1's flat floor

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| apex (u) | 43.67 | 43.33 → 43.70 (+0.029) | 44.63 → 43.70 (+0.027) | 45.00 → 43.71 (+0.031) | 45.28 → 43.71 (+0.032) | 44.58 → 43.70 (+0.030) | ±0.100 |
| jump length (u) | 213.3 | 213.3 → 213.3 (0) | 215.6 → 213.3 (0) | 216.0 → 212.0 (−1.33) | 216.0 → 212.0 (−1.34) | 215.3 → 212.4 (−0.941) | ±6.0 |

**accel** — from rest, forward held, on flat floor; the view bob while running

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| time to 200 u/s (s) | 0.071 | 0.071 → 0.071 (−0.001) | 0.073 → 0.073 (+0.001) | 0.073 → 0.073 (+0.002) | 0.074 → 0.074 (+0.002) | 0.073 → 0.073 (+0.001) | ±0.014 |
| time to 319 u/s (s) | 0.125 | 0.132 → 0.132 (+0.007) | 0.131 → 0.131 (+0.006) | 0.129 → 0.129 (+0.004) | 0.129 → 0.129 (+0.004) | 0.128 → 0.128 (+0.003) | ±0.014 |
| distance at 0.25 s (u) | 63.68 | 64.24 → 64.24 (+0.558) | 62.17 → 62.17 (−1.51) | 61.56 → 61.56 (−2.12) | 61.10 → 61.10 (−2.58) | 62.26 → 62.26 (−1.42) | ±3.0 |
| distance at 1 s (u) | 303.7 | 304.2 → 304.2 (+0.558) | 302.2 → 302.2 (−1.51) | 301.6 → 301.6 (−2.12) | 301.1 → 301.1 (−2.58) | 302.3 → 302.3 (−1.42) | ±3.0 |
| top speed (u/s) | 320.0 | 320.0 → 320.0 (0) | 320.0 → 320.0 (0) | 320.0 → 320.0 (0) | 320.0 → 320.0 (0) | 320.0 → 320.0 (0) | ±0.100 |
| view bob, peak to peak (u) | 6.55 | 6.56 → 6.56 (+0.008) | 6.56 → 6.56 (+0.007) | 6.56 → 6.56 (+0.008) | 6.56 → 6.56 (+0.008) | 6.56 → 6.56 (+0.007) | ±0.100 |

**friction** — at full speed, forward let go: the slide to a stop

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| time to 100 u/s (s) | 0.283 | 0.281 → 0.281 (−0.002) | 0.287 → 0.287 (+0.004) | 0.288 → 0.288 (+0.006) | 0.290 → 0.290 (+0.007) | 0.286 → 0.286 (+0.004) | ±0.014 |
| time to stop (s) | 0.542 | 0.533 → 0.533 (−0.008) | 0.542 → 0.542 (0) | 0.542 → 0.542 (0) | 0.540 → 0.540 (−0.002) | 0.536 → 0.536 (−0.005) | ±0.014 |
| slide (u) | 63.75 | 63.00 → 63.00 (−0.750) | 65.63 → 65.63 (+1.88) | 66.38 → 66.38 (+2.62) | 66.94 → 66.94 (+3.19) | 65.36 → 65.36 (+1.61) | ±4.0 |

**stairs** — running up and down e1m1's first stairs (6 steps of 8 units, then 16)

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| up 400 units (s) | 1.26 | 1.26 → 1.26 (+0.001) | 1.25 → 1.26 (−0.002) | 1.25 → 1.25 (−0.003) | 1.25 → 1.25 (−0.004) | 1.25 → 1.26 (−0.002) | ±0.030 |
| view lag on the steps, max (u) | 12.0 | 12.0 → 12.0 (0) | 12.0 → 12.0 (0) | 12.0 → 12.0 (0) | 12.0 → 12.0 (0) | 12.0 → 12.0 (0) | ±1.0 |
| down 400 units (s) | 1.28 | 1.28 → 1.28 (−0.004) | 1.30 → 1.30 (+0.012) | 1.30 → 1.30 (+0.016) | 1.31 → 1.30 (+0.020) | 1.30 → 1.29 (+0.010) | ±0.030 |
| fastest fall going down (u/s) | 144.4 | 146.7 → 146.7 (+2.22) | 155.6 → 150.0 (+5.56) | 156.7 → 146.7 (+2.22) | 158.3 → 150.0 (+5.56) | 151.9 → 151.9 (+7.50) | ±12.0 |

**fall** — the drop height (feet above floor) that makes the landing sound, and fall damage

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| landing sound from (u) | 58.33 | 61.31 → 60.92 (+2.59) | 59.40 → 60.48 (+2.15) | 58.13 → 59.60 (+1.27) | 56.57 → 58.33 (0) | 57.50 → 59.45 (+1.12) | ±4.50 |
| fall damage from (u) | 273.1 | 272.2 → 271.3 (−1.83) | 270.9 → 273.1 (0) | 268.1 → 271.3 (−1.83) | 264.7 → 268.6 (−4.57) | 268.8 → 271.0 (−2.18) | ±9.0 |

**swim** — e1m4's deep water: sinking idle, swimming down, swimming up

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| idle sink in 0.9 s (u) | 33.69 | 33.43 → 33.43 (−0.264) | 34.35 → 34.35 (+0.657) | 34.62 → 34.62 (+0.925) | 34.80 → 34.80 (+1.11) | 34.29 → 34.29 (+0.590) | ±1.50 |
| swim down in 0.25 s (u) | 41.32 | 41.47 → 41.47 (+0.145) | 40.94 → 40.94 (−0.388) | 40.76 → 40.76 (−0.559) | 40.63 → 40.63 (−0.694) | 40.96 → 40.96 (−0.366) | ±1.50 |
| swim down in 0.9 s (u) | 179.9 | 179.1 → 179.1 (−0.812) | 181.9 → 181.9 (+2.00) | 182.7 → 182.7 (+2.79) | 183.3 → 183.3 (+3.37) | 181.6 → 181.6 (+1.65) | ±4.0 |
| swim up in 0.9 s (u) | 90.00 | 90.00 → 90.00 (−0.001) | 90.00 → 90.00 (0) | 90.00 → 90.00 (+0.003) | 89.99 → 89.99 (−0.011) | 90.00 → 90.00 (−0.002) | ±1.50 |

**lava** — standing waist-deep in e1m7's lava for 5 s

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| damage in 5 s (hp) | 480.0 | 460.0 → 460.0 (−20.0) | 500.0 → 500.0 (+20.0) | 480.0 → 480.0 (0) | 500.0 → 500.0 (+20.0) | 480.0 → 480.0 (0) | ±20.0 |
| burns in 5 s | 24.0 | 23.0 → 23.0 (−1.0) | 25.0 → 25.0 (+1.0) | 24.0 → 24.0 (0) | 25.0 → 25.0 (+1.0) | 24.0 → 24.0 (0) | ±1.0 |

**plat** — riding e1m1's lift up (func_plat, 150 u/s)

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| lift reaches the top (s) | 1.03 | 1.03 → 1.03 (+0.006) | 1.02 → 1.02 (−0.007) | 1.02 → 1.02 (−0.007) | 1.02 → 1.02 (−0.011) | 1.03 → 1.03 (−0.002) | ±0.014 |
| rider off the floor, max (u) | 0.000 | 0.0 → 0.0 (0) | 0.000 → 0.000 (0) | 0.028 → 0.030 (+0.030) | 0.964 → 0.030 (+0.030) | 0.022 → 0.030 (+0.030) | ±0.500 |
| rider frames airborne | 0.0 | 0.0 → 0.0 (0) | 0.0 → 0.0 (0) | 2.0 → 1.0 (+1.0) | 33.0 → 2.0 (+2.0) | 1.0 → 1.0 (+1.0) | — |

**door** — e1m1's first door: opens, waits 3 s, closes

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| open (s) | 0.250 | 0.250 → 0.250 (0) | 0.243 → 0.243 (−0.007) | 0.242 → 0.242 (−0.008) | 0.242 → 0.242 (−0.008) | 0.244 → 0.244 (−0.006) | ±0.014 |
| starts closing (s) | 3.26 | 3.27 → 3.25 (−0.014) | 3.25 → 3.24 (−0.021) | 3.25 → 3.24 (−0.022) | 3.24 → 3.24 (−0.022) | 3.25 → 3.25 (−0.017) | ±0.028 |
| closed (s) | 3.51 | 3.52 → 3.50 (−0.014) | 3.49 → 3.49 (−0.028) | 3.49 → 3.48 (−0.031) | 3.49 → 3.48 (−0.031) | 3.49 → 3.49 (−0.027) | ±0.042 |

**toss** — a bouncing projectile (MOVETYPE_BOUNCE, the grenade's) thrown up the runway

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| apex (u) | 74.15 | 73.67 → 74.15 (−0.003) | 75.35 → 74.15 (0) | 75.83 → 74.15 (−0.001) | 76.20 → 74.15 (0) | 75.24 → 74.15 (−0.005) | ±0.200 |
| first landing (s) | 0.970 | 0.965 → 0.965 (−0.004) | 0.971 → 0.964 (−0.006) | 0.973 → 0.965 (−0.005) | 0.974 → 0.964 (−0.006) | 0.969 → 0.964 (−0.006) | ±0.014 |
| height at 0.5 s (u) | 112.2 | 111.7 → 112.2 (0) | 113.6 → 112.2 (0) | 114.2 → 112.2 (0) | 114.6 → 112.2 (0) | 113.5 → 112.2 (−0.004) | ±0.300 |
| bounces | 2.0 | 2.0 → 2.0 (0) | 2.0 → 2.0 (0) | 2.0 → 2.0 (0) | 2.0 → 2.0 (0) | 2.0 → 2.0 (0) | ±0.0 |
| comes to rest (s) | 1.75 | 1.73 → 1.73 (−0.017) | 1.77 → 1.74 (−0.014) | 1.78 → 1.73 (−0.017) | 1.79 → 1.74 (−0.012) | 1.78 → 1.73 (−0.024) | ±0.028 |
| rests at (u) | 259.8 | 256.5 → 257.4 (−2.40) | 263.9 → 259.0 (−0.747) | 265.7 → 259.4 (−0.324) | 267.1 → 260.0 (+0.225) | 264.8 → 258.6 (−1.19) | ±3.0 |

**leap** — a monster-sized MOVETYPE_STEP leap (a dog's jump)

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| apex (u) | 23.61 | 23.33 → 23.61 (0) | 24.31 → 23.63 (+0.019) | 24.58 → 23.63 (+0.019) | 24.79 → 23.63 (+0.019) | 24.21 → 23.63 (+0.018) | ±0.100 |
| air time (s) | 0.500 | 0.500 → 0.500 (0) | 0.500 → 0.493 (−0.007) | 0.504 → 0.492 (−0.008) | 0.504 → 0.492 (−0.008) | 0.501 → 0.492 (−0.008) | ±0.014 |
| leap length (u) | 150.0 | 150.0 → 150.0 (0) | 150.0 → 147.9 (−2.08) | 151.2 → 147.5 (−2.50) | 151.2 → 147.5 (−2.50) | 150.3 → 147.5 (−2.54) | ±4.50 |

**grenade** — a grenade fired level up the runway

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| height at 0.25 s (u) | 47.64 | 47.36 → 47.64 (0) | 48.34 → 47.64 (0) | 48.59 → 47.61 (−0.030) | 48.79 → 47.61 (−0.030) | 48.21 → 47.61 (−0.034) | ±0.500 |
| distance at 0.5 s (u) | 300.0 | 300.0 → 300.0 (0) | 300.0 → 300.0 (0) | 300.0 → 300.0 (0) | 300.0 → 300.0 (0) | 300.0 → 300.0 (0) | ±1.0 |
| bounces | 3.0 | 3.0 → 3.0 (0) | 3.0 → 3.0 (0) | 3.0 → 3.0 (0) | 3.0 → 3.0 (0) | 3.0 → 3.0 (0) | ±0.0 |
| explodes after (s) | 2.51 | 2.52 → 2.52 (+0.003) | 2.51 → 2.51 (−0.007) | 2.50 → 2.50 (−0.010) | 2.50 → 2.50 (−0.012) | 2.50 → 2.50 (−0.009) | ±0.014 |
| explodes at y (u) | 648.5 | 646.2 → 649.6 (+1.13) | 660.6 → 641.4 (−7.14) | 667.8 → 643.0 (−5.49) | 671.4 → 643.3 (−5.16) | 654.8 → 640.8 (−7.67) | ±8.50 |
| explodes at z (u) | 0.031 | 0.031 → 0.031 (0) | 0.031 → 0.031 (0) | 0.031 → 0.031 (0) | 0.031 → 0.031 (0) | 0.031 → 0.031 (0) | ±2.0 |

**rocket** — a rocket fired into the wall at the runway's end: flight, explosion, light, particles

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| hits the wall after (s) | 0.458 | 0.450 → 0.450 (−0.008) | 0.458 → 0.458 (0) | 0.458 → 0.458 (0) | 0.458 → 0.458 (0) | 0.455 → 0.455 (−0.003) | ±0.014 |
| explodes at y (u) | 760.0 | 760.0 → 760.0 (0) | 760.0 → 760.0 (0) | 760.0 → 760.0 (0) | 760.0 → 760.0 (0) | 760.0 → 760.0 (0) | ±0.500 |
| light radius at 0.2 s (u) | 285.8 | 285.0 → 285.0 (−0.833) | 287.9 → 287.9 (+2.08) | 288.7 → 288.7 (+2.92) | 289.4 → 289.4 (+3.54) | 287.5 → 287.5 (+1.70) | ±5.0 |
| light gone after (s) | 0.514 | 0.517 → 0.517 (+0.003) | 0.507 → 0.507 (−0.007) | 0.504 → 0.504 (−0.010) | 0.502 → 0.502 (−0.012) | 0.502 → 0.502 (−0.012) | ±0.014 |
| explosion particles at 0.3 s | 1024.0 | 1024.0 → 1024.0 (0) | 1024.0 → 1024.0 (0) | 1024.0 → 1024.0 (0) | 1024.0 → 1024.0 (0) | 1024.0 → 1024.0 (0) | ±20.0 |
| their mean distance (u) | 103.9 | 101.9 → 100.5 (−3.34) | 105.3 → 103.9 (+0.004) | 105.8 → 103.1 (−0.750) | 104.8 → 106.9 (+3.07) | 104.8 → 103.9 (+0.028) | ±6.0 |

**rocketjump** — a rocket at the feet (pitch 80) fired with a jump

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| apex (u) | 135.9 | 135.9 → 135.9 (0) | 135.9 → 135.9 (0) | 136.0 → 136.0 (+0.030) | 136.0 → 136.0 (+0.030) | 136.0 → 136.0 (+0.030) | ±2.0 |

**weapons** — fire held: shotgun for 3 s, nailgun for 2 s

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| shotgun shots in 3 s | 6.0 | 6.0 → 6.0 (0) | 6.0 → 6.0 (0) | 6.0 → 6.0 (0) | 6.0 → 6.0 (0) | 6.0 → 6.0 (0) | ±0.0 |
| shotgun: time between shots (s) | 0.500 | 0.500 → 0.500 (0) | 0.500 → 0.500 (0) | 0.500 → 0.500 (0) | 0.500 → 0.500 (0) | 0.501 → 0.501 (+0.001) | ±0.014 |
| nails in 2 s | 20.0 | 20.0 → 20.0 (0) | 20.0 → 20.0 (0) | 20.0 → 20.0 (0) | 20.0 → 20.0 (0) | 21.0 → 21.0 (+1.0) | ±1.0 |

**trails** — trail particles per 100 units behind a gib, a grenade and a rocket

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| gib blood (150 u/s) (/100u) | 46.34 | 38.33 → 46.67 (+0.322) | 94.34 → 48.00 (+1.66) | 158.3 → 48.33 (+1.99) | 318.3 → 48.33 (+1.99) | 94.73 → 48.20 (+1.85) | ±3.0 |
| grenade smoke (600 u/s) (/100u) | 34.76 | 38.33 → 34.58 (−0.175) | 47.17 → 35.59 (+0.828) | 39.58 → 35.83 (+1.07) | 79.58 → 35.83 (+1.07) | 42.95 → 35.51 (+0.755) | ±3.0 |
| rocket fire (1000 u/s) (/100u) | 34.76 | 34.50 → 34.75 (−0.009) | 42.46 → 35.50 (+0.745) | 47.50 → 35.75 (+0.991) | 47.75 → 36.00 (+1.24) | 39.28 → 35.60 (+0.841) | ±3.0 |

**grunt** — an e1m1 grunt woken 192 units away: its first shot, shots and damage in 8 s

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| first shot, mean (s) | 1.50 | 1.50 → 1.50 (0) | 1.50 → 1.50 (0) | 1.50 → 1.50 (0) | 1.50 → 1.50 (0) | 1.50 → 1.50 (+0.001) | — |
| shots in 8 s, mean | 4.80 | 4.80 → 4.80 (0) | 4.80 → 4.80 (0) | 4.80 → 4.80 (0) | 4.80 → 4.80 (0) | 4.60 → 4.60 (−0.200) | — |
| damage in 8 s, mean (hp) | 76.80 | 76.80 → 76.80 (0) | 76.80 → 76.80 (0) | 76.80 → 76.80 (0) | 76.80 → 76.80 (0) | 73.60 → 73.60 (−3.20) | — |
| animation frames in 8 s, mean | 79.0 | 79.0 → 79.0 (0) | 79.0 → 79.0 (0) | 79.0 → 79.0 (0) | 79.0 → 79.0 (0) | 80.0 → 80.0 (+1.0) | ±1.0 |

**quad** — impulse 255: how long the Quad Damage lasts

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| quad lasts (s) | 30.01 | 30.02 → 30.02 (+0.003) | 30.01 → 30.01 (−0.007) | 30.00 → 30.00 (−0.010) | 30.00 → 30.00 (−0.012) | 30.00 → 30.00 (−0.009) | ±0.014 |

**flash** — a hit's damage flash and view kick, and a pickup's bonus flash

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| damage flash, first frame (%) | 34.0 | 34.0 → 34.0 (0) | 35.0 → 37.0 (+3.0) | 36.0 → 37.0 (+3.0) | 36.0 → 37.0 (+3.0) | 35.0 → 37.0 (+3.0) | ±3.0 |
| damage flash gone after (s) | 0.160 | 0.192 → 0.158 (−0.001) | 0.122 → 0.170 (+0.010) | 0.148 → 0.177 (+0.017) | 0.074 → 0.178 (+0.018) | 0.156 → 0.172 (+0.012) | ±0.020 |
| view kick gone after (s) | 0.486 | 0.483 → 0.483 (−0.003) | 0.493 → 0.493 (+0.007) | 0.496 → 0.496 (+0.010) | 0.498 → 0.498 (+0.012) | 0.493 → 0.493 (+0.007) | ±0.014 |
| bonus flash gone after (s) | 0.330 | 0.396 → 0.329 (−0.001) | 0.337 → 0.332 (+0.002) | 0.202 → 0.341 (+0.011) | 0.101 → 0.343 (+0.013) | 0.372 → 0.335 (+0.005) | ±0.020 |

**clocks** — long sessions: the client's host clock and a door's ltime, one hour in

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| host clock, 10 s at 1 h (s) | 10.02 | 9.96 → 10.00 (−0.020) | 9.84 → 10.00 (−0.020) | 9.96 → 10.00 (−0.020) | 10.55 → 10.00 (−0.020) | 10.00 → 10.00 (−0.019) | ±0.020 |
| door opens in, at 1 h (s) | 0.250 | 0.250 → 0.250 (0) | 0.243 → 0.243 (−0.007) | 0.242 → 0.242 (−0.008) | 0.229 → 0.242 (−0.008) | 0.245 → 0.245 (−0.005) | ±0.014 |
| door's snap at the end, at 1 h (u) | 0.000 | 0.000 → 0.000 (0) | 0.000 → 0.000 (0) | 0.000 → 0.000 (0) | 4.33 → 0.000 (0) | 0.000 → 0.000 (0) | ±0.500 |

**demo** — demo1's first 20 s: how often the recorded view moves on screen

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| camera moves a second (/s) | 70.35 | 58.65 → 58.65 (−11.70) | 140.6 → 140.6 (+70.30) | 234.3 → 234.3 (+164.0) | 468.4 → 468.4 (+398.1) | 138.0 → 138.0 (+67.70) | — |
| demo message at 20 s | 242.0 | 242.0 → 242.0 (0) | 242.0 → 242.0 (0) | 242.0 → 242.0 (0) | 242.0 → 242.0 (0) | 242.0 → 242.0 (0) | ±2.0 |

(Since `q26/lerp` the demo plays as id's client does, one recorded message
at a time, drawn between the two newest at every rate: the camera moves in
every frame the recorded player moves, and a message is a real message, not
one of the port's old 60 Hz sub-frames.)

## Monsters between their steps (`r_lerpmove`, `q26/lerp`)

id's server moves a monster (`MOVETYPE_STEP`) only in its think, every
0.1 s, and the client draws it where the last step put it: at 240 Hz it
stands still for 23 frames and jumps in the 24th, beside a camera that moves
every frame. The 2026 extra `client::lerpmove::LerpMove::Smooth` (off in
Classic; QuakeSpasm's `r_lerpmove`) draws it gliding from step to step, over
0.1 s from where it is drawn when the step comes (one frame for a mover the
server moves every frame); the module doc says why that rule and not
QuakeSpasm's own. Animation frames are not blended.

`quaketool framerate <pak> --lerpmove`, native, 2026-09-26: over the frames
in which a monster was walking (it moved within 0.1 s before and after),
Classic → `r_lerpmove`. Demos are id's relink in Classic (its `U_NOLERP`
jump a message ahead and back: the 25-unit moves).

| workload | quantity | 72 | 60 | 144 | 240 | 480 |
|---|---|---|---|---|---|---|
| patrol (e1m1's grunt on its path) | frames drawn moving (%) | 13.7 → 99.2 | 16.5 → 99.1 | 6.8 → 99.7 | 4.2 → 99.8 | 2.1 → 99.9 |
| | spread of the per-frame move (sd/mean) | 2.87 → 0.56 | 2.58 → 0.53 | 4.20 → 0.56 | 5.42 → 0.52 | 7.72 → 0.51 |
| | largest move in a frame (u) | 4.11 → 0.59 | 4.11 → 0.69 | 4.11 → 0.29 | 4.11 → 0.17 | 4.11 → 0.09 |
| | behind the server, mean (u) | 0 → 1.13 | 0 → 1.15 | 0 → 1.05 | 0 → 1.04 | 0 → 1.02 |
| charge (the first-room grunt woken) | frames drawn moving (%) | 13.3 → 100 | 16.0 → 100 | 6.6 → 100 | 3.9 → 100 | 2.0 → 100 |
| | spread of the per-frame move | 2.63 → 0.26 | 2.37 → 0.25 | 3.88 → 0.25 | 5.08 → 0.23 | 7.26 → 0.23 |
| | largest move in a frame (u) | 15.0 → 2.13 | 15.0 → 2.81 | 15.0 → 1.06 | 15.0 → 0.64 | 15.0 → 0.32 |
| | behind the server, mean (u) | 0 → 6.27 | 0 → 6.41 | 0 → 5.86 | 0 → 5.68 | 0 → 5.56 |
| knock (thrown: moved every frame) | behind the server, mean (u) | 0 → 2.06 | 0 → 2.54 | 0 → 1.07 | 0 → 0.64 | 0 → 0.32 |
| demo1 (every recorded monster) | spread of the per-frame move | 1.62 → 0.97 | 1.49 → 0.96 | 2.32 → 0.94 | 3.03 → 0.93 | 4.32 → 0.92 |
| | largest move in a frame (u) | 24.9 → 3.38 | 26.0 → 4.36 | 24.9 → 1.75 | 24.5 → 1.02 | 24.3 → 0.52 |
| | behind the newest message, mean (u) | 2.11 → 3.54 | 1.92 → 3.64 | 2.48 → 3.37 | 2.65 → 3.30 | 2.76 → 3.24 |

The price is the glide itself: a monster is drawn on average half a step
behind where the server has it (1 unit walking, 6 running), for at most
0.1 s; its box, its shots and everything else are the server's. The spread
left is the monsters' own: their steps are of different lengths (a patrol's
1–4 units, a run's 8–15). `--strip DIR` writes a 240 Hz step of the charging
grunt, Classic and with the extra, as frames.

## Budget

`quaketool framerate <pak> --budget --res 640x400,1280x800,1280x1024`, native
release build, one core of a 16-core desktop (load about 1), 2026-09-26.
The uncapped client at 480 Hz for 2400 frames, timed per phase with the
client's lap hook: the live game on e1m1 (`bench.py`'s walk — a turn in
place, runs, about-faces — with the level's monsters awake) and demo1's
playback. Median / p95 ms per frame; the last column is the median's
headroom against 480, 240 and 144 Hz (2.08, 4.17, 6.94 ms). The platform's
own work is not in it (packing the frame to RGBA, the page).

| screen, workload | sim | 3-D view | post + 2-D | total | headroom 480 / 240 / 144 Hz |
|---|---|---|---|---|---|
| 640x400, e1m1 live | 0.04 / 0.05 | 0.36 / 0.62 | 0.08 / 0.08 | 0.47 / 0.75 | +1.61 / +3.69 / +6.47 |
| 640x400, demo1 | 0.00 / 0.00 | 0.54 / 0.88 | 0.08 / 0.09 | 0.62 / 0.97 | +1.46 / +3.54 / +6.32 |
| 1280x800, e1m1 live | 0.04 / 0.08 | 1.38 / 2.05 | 0.17 / 0.29 | 1.59 / 2.42 | +0.49 / +2.58 / +5.35 |
| 1280x800, demo1 | 0.00 / 0.01 | 1.89 / 2.93 | 0.23 / 0.43 | 2.15 / 3.30 | −0.07 / +2.02 / +4.79 |
| 1280x1024, e1m1 live | 0.08 / 0.11 | 2.12 / 2.88 | 0.29 / 0.44 | 2.54 / 3.30 | −0.45 / +1.63 / +4.41 |
| 1280x1024, demo1 | 0.01 / 0.01 | 2.64 / 3.60 | 0.30 / 0.45 | 2.97 / 3.98 | −0.89 / +1.19 / +3.97 |

- **1920x1080 is not measured:** the renderer stops at id's largest view,
  1280x1024 (`render::MAXWIDTH`, `MAXHEIGHT`). Scaled by the pixel count
  (2.03x 1280x800's), a live frame would take about 3.2 ms and a demo frame
  4.3 ms: 240 Hz with a little room, not 480.
- **The simulation is cheap:** the server frame plus the client's side of
  its messages is 0.04–0.08 ms at 480 Hz; the frame is the renderer's.
- id's own measure agrees: `quaketool timedemo <pak> demo1 --res
  1280x800,1280x1024` (every message as fast as the client draws it, packed
  to RGBA as the page presents it) runs 318–348 and 247–269 fps in the same
  sitting (3.0 and 3.8 ms a frame, means).
- In the browser the wasm build runs about 0.7x native (`PERF_PLAN.md` §10),
  so 480 Hz there needs a smaller screen or more than one core.

## Rerun it

```sh
cd quake-rs && cargo build --release
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK             # every table, text (2.5 min)
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK --markdown  # the tables above
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK --check     # fail if an uncapped value leaves its tolerance
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK --only jump,flash --rates 144,480
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK --budget --res 1280x800
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK --lerpmove  # monsters between steps (5 s)
```

Each scenario restarts the process's random sequences (`server::reset_random`)
before it loads its map, so two rates differ only by their frame times.

Without the game data, `cargo test` checks the same things on synthetic
worlds, at 60, 144 and 480 Hz against 72, with the tolerances stated in each
test: `server::sv_phys::tests::uncapped_frames_jump_and_bounce_like_72_hz`
(a jump and a bounce: apex to 0.05 units, landing to a 72 Hz frame),
`client::view::tests::uncapped_flashes_fade_like_72_hz`,
`particles::tests::uncapped_trails_are_as_dense_as_72_hz`,
`client::host::tests::host_filter_time_display_runs_every_refresh_and_never_outruns_the_clock`
and `stepping::tests`. Each also shows id's per-frame code failing the
comparison, so the tests have teeth.

## Wiring (quake-wasm, for the chair)

`quake-wasm` is the platform branch's, so the page still runs its uncapped
extra through id's per-frame code. Switching it over is three changes in
`quake-wasm/src/host.rs` `step`: the uncapped extra (not a timedemo, which
keeps id's uncapped `Host_FilterTime` and one message a frame) takes
`host_filter_time_display` for its gate and hands `Stepping::Uncapped` to
the walk and the demo each frame, as `viewsize` is handed:

```rust
use quake_rs::client::host::{host_filter_time, host_filter_time_display, host_filter_time_uncapped};
use quake_rs::stepping::Stepping;

let display = a.menu.extras().uncapped && !a.cls.timedemo;
let stepping = if display { Stepping::Uncapped } else { Stepping::Classic };
// the gate: host_filter_time_display(a.realtime, &mut a.oldrealtime) when `display`,
// host_frame_time(..) as now otherwise
wk.stepping = stepping; // beside wk.key_move / wk.viewsize
d.stepping = stepping;  // beside d.viewsize
```

Tried on this branch without committing: `quake-wasm` builds, its 135 tests
pass, `verify_extras` (42/42) and `verify_demo` (11/11) pass, and in the real
page a second of 1/480 s steps of the attract demo with `wasm_uncapped 1`
presents 480 frames, all different (102 before: the camera moved only on the
demo's 60 Hz messages), the live walk 480 of 480, with no console errors.

`r_lerpmove` (`q26/lerp`) is wired in the shell: the `r_lerpmove 0|1`
console cvar (`App::lerpmove`, 0 — Classic — by default) is handed to the
walk and the demo each frame in `host::step`, beside `viewsize` and the
renderer's threads (a timedemo ignores it). The settings work folds it into
the 2026 profile:

```rust
wk.lerpmove = a.lerpmove; // quake_rs::client::lerpmove::LerpMove::{Classic, Smooth}
d.lerpmove = a.lerpmove;
```

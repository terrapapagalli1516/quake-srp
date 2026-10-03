# Frame rate: the same game from 60 to 480 Hz

id's WinQuake runs at most 72 host frames a second (`Host_FilterTime`), and the
game was tuned there. The port's "Uncapped framerate" extra runs one host frame
per display refresh — server and client, at the display's rate, no fixed tick —
and until this branch it ran id's per-frame code at that rate. This page
measures what that changes, says what was fixed and what is left, and how to
rerun the measurement. (Branch `q26/framerate`, 2026-09-26.)

## In short

- **Measured:** `quaketool framerate` plays 23 scripted scenarios on the
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
- **Stairs, found later** (`fleet/stairs`, 2026-10-02): above about 233 Hz
  the uncapped game showed every stair step as a snap — the view rose 4 of a
  16-unit step's units with the body, the rest one frame later, where id's
  glides it up at 80 u/s over 0.15 s (the user found climbing stairs strange, the
  view rising too fast); a lift's rider's view could jump the same way. Not the
  smoothing: the server's `FL_ONGROUND` dropped for the frame after every step
  up, because a frame that short falls less than the trace's 1/32-unit
  standoff and so no longer touches the floor; the client's smoothing lets go
  of the view whenever the flag is off, as id's does. Fixed in the uncapped
  path ("Ground contact" below; the `step` table).
- **Left, and why** — see "What is left": the ground acceleration and
  friction (a fixed 2–3 units over a run, 5% of a slide), swimming (2–3% over
  a second, QuakeC's drag), QuakeC timers that a frame rounds up (lava burns
  4% faster at 480 Hz than at 72, a fall-damage threshold 4.5 units lower),
  pusher think transitions (up to a 72 Hz frame each in id's game), air
  control (not measured).
- **Budget:** natively, one core, at 1280x800 a live frame costs 1.6 ms
  (median; p95 2.4) — inside 480 Hz's 2.08 ms at the median — and a demo
  frame 2.2 ms. 1280x1024 and the extrapolated 1920x1080 (3.2 ms) fit 240 Hz
  but not 480. Details under "Budget". On one thread, that is; the renderer
  now draws on every core (`PERF_PLAN.md` §11).
- **In the browser** since `q26/settings` (`02a32ad`): `wasm_uncapped` runs
  this path in the page ("Wiring" below). The 2026 profile, the default, has
  it on; Classic keeps id's 72 fps gate.

## How the uncapped step works

The simulation still steps at the display rate. `Stepping` (`quake-rs/src/stepping.rs`)
tells the server and client how a frame of any length `dt` steps each
per-frame integrator that drifts, so that it does what a run of 1/72 s frames
covering the same time does in id's game:

| what | id's per-frame code | uncapped | where |
|---|---|---|---|
| gravity (player, monsters' leaps, grenades, gibs, corpses) | semi-implicit Euler: a 72 Hz trajectory runs `g·t/144` below the parabola, a 480 Hz one `g·t/960` | the move leads the vertical velocity by `g·(dt − 1/72)/2`, which lands on id's 72 Hz curve at every rate; a bounce clips the led velocity, as a 72 Hz frame bounces with the speed its frame ends at | `server::sv_phys` (`gravity_lead`, `move_with_lead`) |
| ground contact (the player standing on a floor) | `FL_ONGROUND` holds only while each frame's move touches the floor, which a step up or a landing leaves 1/32 unit below (the trace's standoff): a 72 Hz frame falls 0.15 units and does; past 160 Hz (233 with the gravity lead) the frame after a step up falls short and the flag drops for it | a walker that stood on the ground and whose move touched no floor looks 0.045 unit below — the standoff measured straight down on the steepest floor one can stand on (1/32 ÷ 0.7) — and stands on the floor it finds there | `server::sv_phys` (`keep_ground`), `Stepping::ground_probe` |
| damage and bonus flashes | `int` percents lose a truncation every frame: at least 1 a frame | id's drop per whole 1/72 s tick (`Tick72`) | `client::view::fade_cshifts` |
| trails (rockets, grenades, gibs, tracers) | `R_RocketTrail` drops at least one particle a frame | the particles one 72 Hz frame would drop at the entity's speed, spread evenly, the spacing carried from frame to frame | `particles::ParticleSystem::spawn_trail` |
| `host_time` (notify, centre prints), pushers' `ltime` | `f32 += dt` | kept beside a double (`advance_clock`) | `client::cl_main`, `server::sv_phys` |
| the gate | `Host_FilterTime` without its cap clamps a frame under 1 ms up to 1 ms, so past 1000 Hz the game outran the clock | `host_filter_time_display`: id's gate with the cap at 1000 fps; a closer refresh skips | `client::host` |

What needed nothing: QuakeC thinks (`SV_RunThink` runs each at its own
`nextthink` time, so monster AI, animation and weapon cadence are the same at
every rate — the grunt's fight is identical), pushers' paths (`SV_Physics_Pusher`
moves them exactly to their think times), everything drawn from `cl.time`
(view bob and roll, light styles, sky and water, the intermission sway — the
light styles still step ten times a second at every rate, as id's do, and
the 2026 profile glides between the steps: "Light styles between their
letters", below), linear fades (the view kick, dlights, the punch angle,
and the stair smoothing — 80 u/s times the frame's time — once its ground
flag holds), the ambient sounds, which already step in 1/72 s ticks
(`snd.rs`), and demo playback: id's `CL_LerpPoint` draws every frame
between the two newest recorded messages at any rate
(`client::cl_demo::demo_frame`; the port's Classic playback did not until
`q26/lerp`).

Tried and dropped (return on complexity): the exponential particle
velocities (`pt_explode`'s `vel += vel·dvel`) and the ground and water
friction stepped exactly. They moved an explosion cloud 1%, a slide 1.3 of
its 3 units and a swim nothing measurable; "What is left" says why the rest
can stay.

## Ground contact: stairs (`fleet/stairs`)

id's server steps the player up a stair at once (`SV_WalkMove`), and the
client glides the view after it (`V_CalcRefdef`, "smooth out stair step
ups"): while `cl.onground` and the body is above the view, the view rises at
80 u/s, at most 12 units behind; when the flag is off it snaps to the body.
The port's smoothing (`client::cl_main::walk_frame`) is id's, stepped by the
frame's own time, and needed nothing. What broke it was the flag. A step up
ends with a trace down onto the step, which stops the box 1/32 unit above it
(`DIST_EPSILON`). The next frame's move falls `g·dt·(dt + 1/72)/2` (with the
gravity lead): 0.15 units at 72 Hz, which crosses the standoff, touches the
step and keeps `FL_ONGROUND`; 0.022 at 309 Hz, which does not. `SV_WalkMove`
clears the flag before the move, so for that one frame the player is
airborne, the client resets the smoothing, and the view jumps the 12 units
it was still behind in one frame (3.2 ms at 309 Hz) — every step of every
staircase, and on a lift whenever its rider is set down on it again (a
jittery display's long frame does that). id's per-frame code at those
rates drops the flag for two or three frames.

The uncapped walker now feels for its floor: when it stood on the ground
and its move touched none, a trace 0.045 unit down — the standoff measured
straight down on the steepest floor one can stand on (`normal.z > 0.7`), so
it finds a floor the walker was set down on at any frame length and no
floor the move left further behind — sets it down there as a touch would,
`FL_ONGROUND` and all (`Server::keep_ground`). Its touch function waits for
the next frame, whose move starts inside the standoff and touches it.
Classic is id's.

The `step` table measures the view over e1m1's 16-unit step at the top of
the first stairs. Without the probe the view was level with the body
2–4 ms after the step at 240 Hz and up, rising at 2,900–5,800 u/s (12,000
at 1000 Hz), uncapped and with id's code alike; with it the view is 8 units
below at 0.05 s and 4 at 0.1 s, level at 0.15 s, rising at 80 u/s, as at
72 Hz. Going down, id's smooths nothing, and neither does the port at any
rate. The lift: the rider's view rises at the lift's 150 u/s at every rate
(at most 12 units under it), where the jittery 144 Hz display's had jumped
at 1575 u/s. The probe moved nothing else the harness measures (the stairs'
"down 400 units" at 240 Hz by a millisecond). The lift's rider is still
airborne for a frame or two at 240–480 Hz: the first frames, dropped onto
the lift from the standoff, before it stood on anything.
`server::sv_phys::tests::uncapped_walker_stays_on_the_ground_after_a_step_up`
checks the flag on a synthetic step at 60 to 1000 Hz.

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
- **Ground contact on gentle down slopes.** On a floor that falls away
  under a running walker, the uncapped walker keeps touching it every frame
  while it drops 10–13 u/s (speed × slope, 60 to 480 Hz; without the probe
  6–7 above 233 Hz), id's 72 Hz walker while it drops 9 u/s (at a full run
  a 1.8–2.3° slope against 1.6°); beyond that both touch on some frames.
  Friction works on the frames that touch. Not measured (no slope in the
  harness is that gentle); the numbers are the arithmetic of the fall and
  the standoff.
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

`quaketool framerate <pak> --markdown`, native, 2026-10-02 (first 2026-09-26;
`step` and the lift rider's eye added on `fleet/stairs`). Each cell is id's
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
| down 400 units (s) | 1.28 | 1.28 → 1.28 (−0.004) | 1.30 → 1.30 (+0.012) | 1.30 → 1.30 (+0.017) | 1.31 → 1.30 (+0.020) | 1.30 → 1.29 (+0.010) | ±0.030 |
| fastest fall going down (u/s) | 144.4 | 146.7 → 146.7 (+2.22) | 155.6 → 150.0 (+5.56) | 156.7 → 146.7 (+2.22) | 158.3 → 150.0 (+5.56) | 151.9 → 151.9 (+7.50) | ±12.0 |

**step** — the view over a 16-unit step: running up it (id's stair smoothing, 80 u/s) and off it

| quantity | 72 (id) | 60 | 144 | 240 | 480 | jitter | tolerance |
|---|---|---|---|---|---|---|---|
| eye below the body as it steps (u) | 12.0 | 12.0 → 12.0 (0) | 12.0 → 12.0 (0) | 12.0 → 12.0 (0) | 12.0 → 12.0 (0) | 12.0 → 12.0 (0) | ±0.100 |
| eye below the body at 0.05 s (u) | 8.00 | 8.00 → 8.00 (0) | 8.00 → 8.00 (0) | 0.0 → 7.97 (−0.030) | 0.0 → 7.97 (−0.027) | 0.0 → 7.97 (−0.030) | ±1.11 |
| eye below the body at 0.1 s (u) | 4.00 | 4.00 → 4.00 (0) | 4.00 → 4.00 (0) | 0.0 → 3.97 (−0.030) | 0.0 → 3.97 (−0.026) | 0.0 → 3.97 (−0.030) | ±1.11 |
| eye level again after (s) | 0.153 | 0.150 → 0.150 (−0.003) | 0.153 → 0.153 (0) | 0.004 → 0.150 (−0.003) | 0.002 → 0.150 (−0.003) | 0.004 → 0.155 (+0.002) | ±0.014 |
| eye's fastest rise (u/s) | 80.00 | 80.00 → 80.00 (0) | 80.00 → 80.00 (0) | 2876.7 → 80.00 (0) | 5758.3 → 80.00 (−0.001) | 2863.9 → 80.00 (+0.001) | ±0.500 |
| going down: eye off the body, max (u) | 0.0 | 0.0 → 0.0 (0) | 0.0 → 0.0 (0) | 0.0 → 0.0 (0) | 0.0 → 0.0 (0) | 0.0 → 0.0 (0) | ±0.010 |

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
| rider frames airborne | 0.0 | 0.0 → 0.0 (0) | 0.0 → 0.0 (0) | 2.0 → 1.0 (+1.0) | 33.0 → 2.0 (+2.0) | 1.0 → 0.0 (0) | — |
| rider's eye: fastest rise (u/s) | 150.0 | 150.0 → 150.0 (0) | 150.0 → 150.0 (+0.001) | 216.7 → 150.0 (0) | 6297.9 → 150.0 (+0.001) | 357.8 → 150.0 (+0.001) | ±1.0 |

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
| camera moves a second (/s) | 70.35 | 58.65 → 58.65 (−11.70) | 140.6 → 140.6 (+70.25) | 234.3 → 234.3 (+163.9) | 468.4 → 468.4 (+398.1) | 138.0 → 138.0 (+67.64) | — |
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
QuakeSpasm's own. Animation frames are a separate extra ("Animation frames
blended" below).

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

## Animation frames blended (`r_lerpmodels`, `q26/lerpframes`)

id steps an animated model's `frame` field at 10 Hz (a monster's walk cycle,
the view weapon's fire animation) — on a 1996 display that was most of a
frame's worth of pose change anyway; at 144–480 Hz, beside a camera that
moves every frame, the model visibly holds a pose for several frames and
jumps to the next. The 2026 extra `client::lerpmodels::LerpModels::Smooth`
(off in Classic; QuakeSpasm's `r_lerpmodels`) blends the alias pipeline's
vertex pass between the current pose and the one before it, by how far
through 0.1 s (`GLIDE_FRAME`, the same reasoning as `r_lerpmove`'s own
`GLIDE`) the clock is; the module doc says why a frame change does not chain
the way a position glide does. A `Frame::Group` pose (a torch's flicker, a
flame) never blends — it is not a motion between two named poses — and
neither does the view weapon across a model change (a weapon switch).

A firing model's frames are not one motion either. QuakeC raises
`EF_MUZZLEFLASH` for the frame a weapon discharges, and the fire frames carry
the flare as geometry: `v_nail.mdl` alternates barrels, each pose with its
own flare, so a blend slid the flare from one to the other and half-recoiled
both; the shotguns' and a grunt's flare is full in the first fire pose, so a
blend from the pose before grew it over a tenth of a second (it reached full
size when the next pose began to take it away again, a 0.1 s late pulse
instead of a flash). FitzQuake and QuakeSpasm have the rule the port left
out: an entity with the flash this frame draws its pose as it is
(`LERP_RESETANIM`, r_alias.c:430) and its next pose change snaps too
(`LERP_RESETANIM2`, :439: "no lerping for two frames"), the player's flash
being the view weapon's (`cl.viewent`, cl_main.c:531). The 2026 port does the
same, in the live walk and in demo playback (`FrameLerps::muzzle_flash`);
nothing else changes — a monster's walk, the axe's swing and the recoil
frames after the flash blend as before. Not measured, and not worth it: a
flash is a set insert and a snap, which draws one pose instead of two. What
the rule costs a model that flashes without a flare of its own is two snapped
changes (the ogre's grenade toss), as in QuakeSpasm.

Stepping is unchanged: `quaketool framerate --check` passes with the extra
on or off, because nothing about *when* a frame changes moves — only how it
is drawn between the changes.

The cost is the vertex pass reading two frames instead of one (positions
and, for the light, both vertices' normals) for every alias model, every
frame. `quaketool timedemo <pak> demo1 --video modern --res 1280x800
--lerpframe 1` against the same without `--lerpframe` (`r_lerpmodels`
otherwise always reads Classic in a timedemo — `cl_demo::timedemo_frame`'s
own doc — so the flag is the only way to measure it), interleaved, 11
repetitions, medians, native release build, 2026-10-02, under other load
(load varied a lot across the runs — one 1-thread rep's total frame time was
4.7x another's — so the `fps` column below is noisy):

| threads | Classic (median fps) | `r_lerpmodels` (median fps) | cost by total fps |
|---|---|---|---|
| 1 | 243.4 | 180.3 | 35% (dominated by machine noise, see below) |
| 8 | 537.9 | 531.4 | 1.2% |

The `fps` numbers fold in everything a frame does (world surfaces, the edge
scan, particles — not just alias models), so a slow unrelated phase in one
rep swings the total as much as the extra itself would; `--profile 1`'s own
`alias` + `gun` milliseconds isolate just the phases `r_lerpmodels` touches,
9 reps each, medians:

| threads | Classic `alias+gun` (ms) | `r_lerpmodels` `alias+gun` (ms) | cost |
|---|---|---|---|
| 1 | 0.627 | 0.626 | −0.2% (within noise) |
| 8 | 0.788 | 0.788 | 0.0% (within noise) |

Demo1 never has more than a few alias models on screen at once (a couple of
grunts, the view weapon), so doubling their vertex reads is not measurable
against the timings' noise floor — "a few percent at most" turned
out to be an upper bound, not the real number. A level with many more
visible monsters at once would show more; not measured here.

## Light styles between their letters (`r_lerplightstyles`, `fleet/lerplight`)

id's `R_AnimateLight` (`r_light.c`) gives each light style the letter of
the current tenth of a second, `(map[(int)(cl.time*10) % len] - 'a') * 22`:
a flickering torch, a fluorescent tube or a pulsing light holds a brightness
for a tenth and jumps to the next. On a 1996 display that was seven frames
a step; at 240 Hz it is 24, and the jump moves a third of e2m2's torch-lit
start in one frame. The 2026 extra `server::LerpLightStyles::Smooth` (off in
Classic; DarkPlaces' `r_lerplightstyles`) moves from letter `k` to letter
`k+1` across the tenth, by `frac(cl.time*10)`, in steps of two of id's light
units (`server::GLIDE_STEP`; 256 is the white point, a letter 22). At every
whole tenth the value is id's letter, so those frames are id's
(`server::lightstyle::tests`); DarkPlaces glides from `k-1` to `k` instead,
the same shape a tenth later. One function serves the live walk and demo
playback (`server::lightstyle_scales_at`), so demos glide too — though of
the attract loop only demo3's views hold an animated light (demo1's and
demo2's frames are the same either way; the steady torches' flicker, below,
is what moves their light). The glide reads its fraction off the client's
`double` clock (`sv.time`, the demo's `cl.time`), Classic its letter off the
`float` id's renderer reads, as before: on a `float` clock two 480 Hz frames
would share a time after about 10 hours in one level.
A one-letter style — a steady light, a switched one — never moves, and a
pattern QuakeC replaces (`lightstyle()`) still changes at once.

What it does to worldspawn's twelve patterns (`world.qc`): the flickers
(styles 1, 6) and candles (3, 7, 8) keep their rhythm and holds, but each
change becomes a tenth-long slope; the pulses (2, 5, 11) were staircases of
22-unit steps and become smooth triangles; the strobes become ramps — the
fast strobe (4, "mamama") a 5 Hz triangle that never holds still, the slow
strobe (9) two 0.1 s fades between 0.7 s of dark and of bright — and e1m1's
fluorescent flicker (10) a run of quick dips. Style 0 ("m") does not move.
In the shareware episode the torches are steady (style 0); its animated
lights are e1m1's fluorescent flicker, e1m5's and e1m6's slow pulse. The
flickering torches are the registered episode 2's (e2m2–e2m5, styles 1 and
6).

At 240 Hz (`quaketool view --lightstyles classic|smooth` at 25 clock times
1/240 s apart, across one tenth: `cl.time` 5.0–5.1 at e2m2's start, 5.1–5.2
in e1m1's corridor, where style 10 goes from 'm' to 'a'; the two modes'
frames at the whole tenth byte-identical). The numbers depend on the tenth:
how far its letters are apart.

| wall, 640x400 | id's: steps that change, the jump | the glide: steps that change, the largest |
|---|---|---|
| e2m2's start, torch-lit (styles 1, 6), 5.0–5.1 | 1 of 24, 10.8% of the view | 22 of 24, 0.65% |
| e1m1's fluorescent corridor (style 10, 'm' to 'a'), 5.1–5.2 | 1 of 24, 51% | 24 of 24, 15% |

**The cost.** The lit-surface caches (`D_CacheSurface`'s, the port's
`render::surf`) are keyed on the style's value: every new value rebakes each
block the style lights. id's rebake once a tenth; the glide, while the
value moves, once a frame — at most once a step. Hence the step: a one-letter
change is 11 steps of 2, not 22 of 1, and at 480 Hz a torch's blocks rebake
on fewer frames (e2m2's start: 9.0 blocks a frame instead of 14.1; e2m5's:
17.9 instead of 30.7). The look does not change: a step of two units is half
a colormap row at the brightest luxel (one row is 1024 of `luxel * value`),
and on e2m2's wall at 480 Hz the most any frame changes is 1.3% of the view
with steps of 2, 1.2% with 1; with 4 (a whole row) it is 2.4%, the changes
bunched onto every other frame. Where a style jumps the whole range in a
tenth (`'m'` to `'a'`, 264 units) the value moves more than a step a frame
at any rate, so its blocks rebake every frame whatever the step.

`quaketool framerate <pak0>,<pak1> --lightstyles --rates 72,480 --res
1920x1080 --threads 1|8 --reps 3`: the live game standing at each view, the
2026 video settings but the light styles, 4.6 s a run (the counters from one
run of each mode, the times from three of each, interleaved, the counters
off), native release build, 2026-10-03, load average 1–3, with the frame's
bakes on the render threads (PERF_PLAN.md §13). "Styled" is the surfaces
drawn whose lightmap has a style past 0; the time is the 3-D view's mean per
frame:

| view | styled | blocks rebaked a frame, 72 Hz / 480 Hz | 1 thread, ms, 72 / 480 Hz | 8 threads, ms, 72 / 480 Hz |
|---|---|---|---|---|
| e1m1 start | 14 | 1.27 → 9.64 / 0.19 → 9.13 | 3.51 → 3.57 / 3.48 → 3.52 | 1.07 → 1.12 / 1.07 → 1.12 |
| e1m1 fluorescent corridor | 26 | 2.36 → 17.9 / 0.35 → 17.0 | 3.37 → 3.44 / 3.35 → 3.41 | 1.01 → 1.06 / 1.01 → 1.06 |
| e1m5 slow pulse | 7 | 0.95 → 6.85 / 0.14 → 1.57 | 3.04 → 3.14 / 3.02 → 3.04 | 0.72 → 0.79 / 0.70 → 0.72 |
| e2m2 start, torches | 18 | 2.50 → 18.0 / 0.38 → 9.01 | 3.17 → 3.28 / 3.15 → 3.22 | 0.81 → 0.89 / 0.80 → 0.85 |
| e2m5 by the start, torches | 46 | 5.74 → 41.4 / 0.86 → 17.9 | 3.27 → 3.49 / 3.22 → 3.34 | 0.88 → 1.00 / 0.85 → 0.93 |

At most 0.23 ms a frame on one thread (e2m5 at 72 Hz, where nearly every
frame rebakes every torch-lit block), 0.02–0.11 elsewhere; on eight, 0.05–0.12
ms (before the bakes went to the threads, the same 0.22 ms as on one: up to
25% of the frame at 72 Hz, now 13%). A frame bakes 0.08–0.38M texels here,
two to eleven threads' worth at `BAKE_TEXELS_PER_THREAD`, so the thread
starts are a good part of what is left. The worst frame stays well inside
480 Hz's 2.08 ms.

## Steady torches that flicker (`r_torchflicker`, `fleet/torchlight`)

id's mappers gave a torch an animated light style only now and then —
`start`'s, a few of episode 2's (world.qc's flickers, styles 1 and 6) — and
left the rest steady, style 0: all 143 torches and flames of e1m2–e1m7, 109
on e2m6, 48 on e4m5. LIGHT.EXE baked a steady torch into each face's style-0
lightmap with the room's other lights, so at run time it is just light. The
2026 extra (`render::torch`; off in Classic) finds each one again: from the
entity lump its origin and `light` (any key starting `light`, or `_light`;
LIGHT.EXE's 300 without one), and for each face in front of it and within
reach its share of each luxel as the tool computed it. That is the tool's
code ported: `SingleLightFace`'s `(light - dist) * (0.5 + 0.5 cos)`, halved
by `rangescale`, at `CalcPoints`' sample points (pulled toward the face's
middle where the middle cannot see them), four samples a luxel averaged, as
`-extra` lights — and nothing where `TestLine`, the tool's trace through the
world's nodes, finds a wall between the torch and the sample. id lit the
shareware maps with `-extra`: re-derived so, every style-0 light traced,
`start`'s style-0 lightmaps come out to the byte and e1m2–e1m6's on 85–97%
of their luxels (one sample a luxel: a quarter). The registered maps in the
pak tested here are not this tool's bake (3% of their luxels; held over
re-derived spreads 0.4–1.4): their shares are the tool's light, not theirs.
A luxel's shares are still never more than it holds, which only bites where
the tool clamped a luxel at 255. Each frame adds

```text
strength · depth · Σ_k (s_k(t)/s̄_k - 1)/√2 · share · d_lightstylevalue[0]/256
```

to the luxel: `s_k` world.qc's two flicker strings through the light-style
glide (stepped as id's with `r_lerplightstyles 0`), `s̄_k` their means, so the
change is zero-mean and every luxel's light averages to id's over the
strings' common period (`render::torch::tests`: to a hundredth of a luxel
unit). The picture's mean brightness follows within about 2%, not exactly:
the colormap's rows are not even steps (toward its dark end a rise
brightens a texel more than a dip darkens it: +2.1% of luma on e1m3's
flames close up), and a rise past the brightest row is clamped there while
the dip is not (−1.1% on e1m2's torch wall), both in proportion to the
strength. Two
strings, not one: either alone
comes round every 1.7 or 2.3 s, its one bright `q` a beat the eye finds; two
at rates whose periods do not divide wander. The kinds differ, as fire does:
the wall torch quick and shallow (style 6 at its own rate, style 1 at 0.75,
depth 0.8), the big brazier flame slow and deep (both at about half speed,
depth 1.3), the small flame between. Each torch starts at its own phases, a
hash of its origin, so a row of torches never pulses as one; the light is a
function of `cl.time` and the torch alone, the same in the live game and in
a demo — demo1 (e1m3) and demo2 (e1m4), whose frames the light-style glide
leaves as they were, come alive. `strength` is the cvar, 0 to 2, 1 the
flicker style's own swing. A model takes the change of the luxel under it
(`R_LightPoint`'s), as the floor does.

At 240 Hz on e1m2's arch (two small flames on the wall ahead, 640x400, 4 s;
the pixels id's own frames change — the flames' models, an ogre, the
animated floor — left out): id's light never moves; at strength 1, 946 of
959 frames change, 0.87% of the view on average and 2.4% at most (0.43% and
1.2% at 0.5), and a frame differs from id's in 9% of its pixels, each by a
colormap row or two.

**The cost.** As with the light styles it is the bakes: a block a torch
lights rebakes when the torch's value moves. But a torch's light reaches 300
units, so in a torch-lit room most of the surfaces drawn are torch-lit, and
with two strings and several torches a face, the value moves nearly every
frame: at 72 Hz every torch-lit block rebakes every frame, at 480 Hz about
half. The shadows take 12–23% of those surfaces away (the faces the torch
cannot see; across whole maps 20–41% of the torch-lit faces).
`quaketool framerate <pak0>,<pak1> --torchflicker 1 --res WxH --threads 1|8
--reps 3 --secs 3`: the live game standing at each view (`framerate.rs`'
`TORCH_VIEWS`: on each map, of standing 160 or 288 units from each torch
facing it, where the most drawn surfaces are torch-lit), the 2026 video
settings in both, the counters from one run of each, the 3-D view's median
ms a frame from three of each, interleaved, the counters off, native release
build, 2026-10-03, load 0–5. Each cell is without the shadows (the first
build, `b22ba40`) → with them, run in the same sitting (→ with the bakes on
the threads, on eight); the time is what the flicker adds to id's view (1080p: 3.9–5.0 ms on one thread, 1.4–1.7 on
eight; 1315x535: 1.6–2.0 and 0.8–1.1):

1920x1080:

| view | torch-lit surfaces | blocks rebaked a frame, 72 / 480 Hz | 1 thread, ms added, 72 / 480 Hz | 8 threads, ms added, 72 / 480 Hz |
|---|---|---|---|---|
| e1m2's start | 130 → 111 | 129 → 110 / 79 → 61 | 0.55 → 0.46 / 0.36 → 0.29 | 0.85 → 0.85 → 0.20 / 0.57 → 0.33 → 0.15 |
| e1m3's flames | 154 → 135 | 154 → 134 / 107 → 85 | 1.15 → 1.03 / 0.98 → 0.84 | 1.58 → 1.45 → 0.48 / 1.28 → 0.97 → 0.29 |
| e1m4's torches | 141 → 109 | 140 → 108 / 74 → 52 | 0.82 → 0.64 / 0.48 → 0.35 | 1.19 → 0.74 → 0.26 / 0.60 → 0.50 → 0.16 |
| e2m6's torches | 142 → 121 | 142 → 121 / 114 → 82 | 0.60 → 0.50 / 0.51 → 0.39 | 1.02 → 0.71 → 0.53 / 0.63 → 0.51 → 0.23 |
| e4m5's flames | 221 → 192 | 217 → 188 / 88 → 76 | 1.30 → 1.25 / 0.71 → 0.55 | 1.76 → 1.76 → 0.57 / 0.80 → 0.68 → 0.39 |

1315x535 (a wide frame):

| view | torch-lit surfaces | blocks rebaked a frame, 72 / 480 Hz | 1 thread, ms added, 72 / 480 Hz | 8 threads, ms added, 72 / 480 Hz |
|---|---|---|---|---|
| e1m2's start | 127 → 110 | 126 → 109 / 77 → 61 | 0.30 → 0.21 / 0.19 → 0.13 | 0.34 → 0.34 → 0.14 / 0.20 → 0.18 → 0.10 |
| e1m3's flames | 174 → 154 | 174 → 154 / 120 → 98 | 0.70 → 0.62 / 0.52 → 0.44 | 0.79 → 0.62 → 0.22 / 0.59 → 0.50 → 0.18 |
| e1m4's torches | 147 → 113 | 146 → 112 / 78 → 54 | 0.58 → 0.45 / 0.38 → 0.26 | 0.62 → 0.49 → 0.18 / 0.40 → 0.28 → 0.14 |
| e2m6's torches | 144 → 121 | 144 → 121 / 115 → 82 | 0.37 → 0.32 / 0.33 → 0.22 | 0.41 → 0.34 → 0.17 / 0.39 → 0.25 → 0.13 |
| e4m5's flames | 237 → 208 | 233 → 204 / 96 → 84 | 0.82 → 1.02 / 0.39 → 0.52 | 1.07 → 1.06 → 0.26 / 0.46 → 0.38 → 0.18 |

The third value on eight threads is with the frame's bakes on the render
threads (PERF_PLAN.md §13), measured the same way in a later sitting; on
one thread nothing changed (within ±0.1 ms of the second value). So the
flicker adds 0.3–1.3 ms a frame at 1080p on one thread, and on eight, once
the bakes went to the threads, 0.2–0.6 (it was 0.3–1.8: the bakes ran in
`D_DrawSurfaces`' setup, before the bands, on one thread). The shadows took
about a sixth off before that (summed over the rows, 15% on one thread, 17%
on eight; one row, e4m5 at 1315x535 on one thread, came out the
other way in this noise). The rebakes are about 0.7 ns a texel, and a
torch-lit room bakes about a texel a pixel of what it shows. demo1's
timedemo (`quaketool timedemo demo1 --video modern --display 16:9 --res
1920x1080`, every message a frame, so 72 Hz's case) runs 199 fps on one
thread and 567 on eight (494 with the bakes on one thread; PERF_PLAN.md §13).
What is left serial on eight is mostly the edge scan. (A cheaper flicker —
each face's torches sampled at most 120 times a second, at the face's own
phase — would cut the 480 Hz rebakes by four at the price of a second clock
per face; not built.)

The torch set is found the first frame the extra is on, a map's load: the
tool's traces, four samples a luxel and the samples pulled in, are most of
it — 58–260 ms on the shareware maps on one thread, 431 on e2m6 — so it is
built on the renderer's threads, 9–39 ms on eight (66 on e2m6), the same
set for any count; 0.1–1.1 MB. In the page (a 16-thread desktop) the
`map` command and its first frame take 40–80 ms with the flicker on and
15–20 without.

The flicker runs on the scene's `float` `cl.time`: after about 10 hours in
one level it holds a value for two 480 Hz frames, which a flame's flicker
cannot show (the light-style glide reads the `double` clock).

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
  4.3 ms: 240 Hz with a little room, not 480. *(Since `q26/hires` the renderer
  goes past 1280x1024, and since `q26/multicore` it draws on every core.
  `timedemo demo1` at 1920x1080 with the 2026 video settings takes 4.8 ms a
  frame on one thread and 1.45 ms on eight, which is inside 480 Hz's 2.08 ms
  (`PERF_PLAN.md` §11).)*
- **The simulation is cheap:** the server frame plus the client's side of
  its messages is 0.04–0.08 ms at 480 Hz; the frame is the renderer's.
- id's own measure agrees: `quaketool timedemo <pak> demo1 --res
  1280x800,1280x1024` (every message as fast as the client draws it, packed
  to RGBA as the page presents it) runs 318–348 and 247–269 fps in the same
  sitting (3.0 and 3.8 ms a frame, means).
- In the browser the wasm build runs about 0.7x native (`PERF_PLAN.md` §10),
  so 480 Hz there needs a smaller screen or more than one core.

## The perspective span (`r_perspspan`, `fleet/exactpersp`, `fleet/perspspan`)

id's x86 renderer finds a wall's texel exactly every 16 pixels and steps
affinely in between (`D_DrawSpans16`; `Turbulent8` on liquids). The user
turned exact perspective on in the 2026 profile (`fleet/exactpersp`: at 1080p
and above they see the affine steps as a wobble along a wall seen at a grazing
angle), and then asked for the steps between: `r_perspspan 16|8|4|1`, the
Picture and sound page's Perspective span row (`id's 16`, `8`, `4`, `exact`,
left and right). Classic is 16; 2026 stays exact (1) until they have tried the
four. The old `wasm_exactpersp` is a view onto it: `1` sets exact, `0` id's
16, and it reads 1 only while the span is 1; nothing writes it any more, so
a saved `wasm_exactpersp "1"` (a Classic player who switched it on) draws
exact perspective as before and the next save writes `r_perspspan "1"`.

**The four.** 16 is `D_DrawSpans16` (d_draw16.s), what 1996 players saw,
Classic's pixels unchanged. 8 is id's own portable C, `D_DrawSpans8`
(d_scan.c), ported as written; beyond the count it differs from the asm in
1/65536ths of a texel: a full segment's step is floored to 16.16 (`>> 3`)
where the asm keeps 20 fractional bits, the last segment divides (toward
zero) where the asm multiplies by `reciprocal_table_16`, and the segment ends
are clamped to 8/65536 where the asm clamps to 1/16 texel. Against id's C
(`compare.py --spans 8 --perspspan 8`, the oracle running `D_DrawSpans8`) the
eight standard rows are 100.00% at the page's aspect (the port's 16 against
them: 91.97-97.43%) and 99.97-99.99% at 640x480 and 1280x1024, the float
residue 16 has against `--spans 16`. 4 is the same arithmetic at 4 (its
clamp, 4/65536, still keeps every position inside the surface). 1 is the
exact path, its pixels unchanged. Liquids take `Turbulent8`'s arithmetic at
the same length (at 8 and 4 not id's: `Turbulent8` is 16 in both id builds).
16 and 1 are proven the same pixels as before by 66 hashes (eight maps'
views at 1920x1080 and 1315x535 in Classic and in 2026 at 16 and 1; `shot`s
with the status bar overlay, under water, slime and lava; `play` frames of
demo1 and the e1m1 walk).

**What changes.** The affine error of a run grows as the square of its
length: on a steep test wall (`raster`'s
`the_error_against_exact_shrinks_with_the_square_of_the_span`) the texel is
off by 2.97 texels on average at 16, 0.77 at 8, 0.19 at 4. In a frame that
is a share of pixels a texel off, and the share falls a little slower than
the error (a pixel only changes where the error crosses a texel's edge): at
1920x1080 of the frame's pixels differ from exact at 16 / 8 / 4

| view (`quaketool view ... --res 1920x1080 --video modern --perspspan N`) | 16 | 8 | 4 |
|---|---|---|---|
| e1m6's long corridor, `--origin 504,500,242 --angles 0,100,0` | 3.69% | 1.27% | 0.40% |
| the same at 1315x535 | 6.58% | 2.52% | 0.86% |
| e1m6's courtyard, `--origin 204,-100,220 --angles 0,100,0` | 4.94% | 1.54% | 0.44% |
| e1m1's start, `--origin 480,-352,110 --angles 0,90,0` | 0.78% | 0.46% | 0.21% |
| e1m4's lake from above, `--origin 320,1284,950 --angles 35,0,0` | 0.19% | 0.05% | 0.02% |

Floors and ceilings hardly change at all: with the view level (no roll) a
row of the screen crosses a floor at one depth, so an affine run along it is
exact (e1m6's corridor floor, 0.40 / 0.11 / 0.02% of a crop of it, the same
with the strafe's 2-degree roll). It is the walls, whose depth changes along
the row, and most where they are seen at a grazing angle. A lake seen from
above is a floor too: it costs the most to draw exactly and changes least.

**What it looks like.** Stills, each pixel 2x2 or 4x4, the four as a grid
(16, 8 / 4, exact): `screenshots/perspspan-wall-2x.png` (e1m6's corridor,
the left wall near the far end, 1080p crop at (400, 100)),
`perspspan-wall-grazing-4x.png` (the courtyard's tower, its grazing right
face, crop at (1220, 200)) and `perspspan-floor-2x.png` (the corridor floor);
`exactpersp-spans-above-exact-below.png` is `fleet/exactpersp`'s, 16 above
exact below in the courtyard.
At 16 the mortar lines along the grazing wall are broken into straight
pieces with a jog every 16 pixels; at 8 they are nearly straight with a
one-pixel jog here and there; at 4 I cannot tell them from exact at 2x or
4x. The floor looks the same at all four.

And in motion, a clip: `quaketool framerate <pak> --perspspan --dump DIR
--rates 60 --res 1920x1080 --secs 10 --view corridor=e1m6:504,500,220:100
--turn -1.5` (a slow turn to the right in e1m6's corridor, 60 fps, the torches
flickering as in 2026), the four 960x540 crops of the left wall as a 2x2
grid (16, 8 / 4, exact) through ffmpeg's `xstack`. Every span's run is the
same game frame for frame, so the four differ only by the span. I could not
watch it at speed; I looked at its frames, at runs of consecutive frames and
at a space-time slice of one row, and measured how much of the error moves
from one frame to the next. At 16, 4.3% of the wall's pixels are off exact
in a frame and 7.7% change between right and wrong from one frame to the
next: the error does not stay put, it slides through the texture with the
turn, so the mortar lines' jogs crawl along them, the wobble the user
describes. At 8 that is 1.2% and 2.2% (a few short jogs crawling, a quarter
as many); at 4, 0.4% and 0.6%, scattered single pixels I did not find in the
frames I looked at without the difference map. Whether 4 or 8 is still
perceptible at speed I do not know; the user's eye decides.

**The cost.** `quaketool framerate <pak> --perspspan --rates 240 --res WxH
--threads 1|8 --reps 5 --secs 3`: the live game with the camera held at each
view (`PERSP_VIEWS`, the player floating), the 2026 video settings with the
torches flickering, the four spans interleaved, the 3-D view's median ms a
frame, native release build, 2026-10-03, load 1-3:

| view | 1920x1080, 1 thread: 16 / 8 / 4 / 1 | 1920x1080, 8 threads | 1315x535, 1 thread | 1315x535, 8 threads |
|---|---|---|---|---|
| e1m1's start | 2.94 / 3.31 (+13%) / 3.86 (+31%) / 5.18 (+76%) | 1.01 / 1.08 (+7%) / 1.17 (+16%) / 1.35 (+33%) | 1.13 / 1.26 (+11%) / 1.43 (+27%) / 1.85 (+63%) | 0.56 / 0.58 (+3%) / 0.62 (+11%) / 0.69 (+22%) |
| e1m1's corridor | 2.76 / 3.11 (+13%) / 3.65 (+32%) / 4.95 (+79%) | 0.96 / 1.01 (+6%) / 1.09 (+14%) / 1.28 (+33%) | 1.10 / 1.21 (+10%) / 1.38 (+26%) / 1.80 (+64%) | 0.53 / 0.54 (+3%) / 0.59 (+12%) / 0.65 (+24%) |
| e1m6's courtyard walls | 2.80 / 3.15 (+13%) / 3.68 (+31%) / 4.98 (+78%) | 0.96 / 1.01 (+5%) / 1.08 (+13%) / 1.27 (+32%) | 1.06 / 1.18 (+11%) / 1.34 (+27%) / 1.77 (+67%) | 0.48 / 0.51 (+5%) / 0.56 (+17%) / 0.62 (+30%) |
| e1m4's lake, from above | 4.02 / 4.37 (+9%) / 5.10 (+27%) / 7.78 (+93%) | 1.14 / 1.18 (+4%) / 1.29 (+13%) / 1.68 (+48%) | 1.56 / 1.67 (+7%) / 1.91 (+23%) / 2.72 (+75%) | 0.67 / 0.69 (+3%) / 0.73 (+9%) / 0.85 (+28%) |

`timedemo demo1` (`quaketool timedemo <pak> demo1 --video modern --display
square --res WxH --threads N --perspspan S --profile 1`: the whole host frame
with its RGBA pack, five runs of each interleaved, the median) and the same
in the browser (`timedemo demo1` from the console of a `?2026` page with a
1920x1080 frame, `vid_pixelsize 1`, its status bar corners drawn; headless
Chromium, the threads build, `r_threads 8`, three runs of each interleaved):

| demo1, ms a frame (fps) | 16 | 8 | 4 | 1 (exact) |
|---|---|---|---|---|
| native 1920x1080, 1 thread | 4.70 (213) | 5.05 (198), +8% | 5.57 (180), +18% | 7.01 (143), +49% |
| native 1920x1080, 8 threads | 1.57 (637) | 1.63 (614), +4% | 1.72 (581), +10% | 1.96 (509), +25% |
| native 1315x535, 1 thread | 2.10 (476) | 2.22 (450), +6% | 2.39 (418), +14% | 2.83 (353), +35% |
| native 1315x535, 8 threads | 1.02 (981) | 1.04 (961), +2% | 1.07 (935), +5% | 1.19 (841), +17% |
| browser 1920x1080, 8 threads | 2.44 (410) | 2.56 (391), +5% | 2.66 (376), +9% | 3.00 (333), +23% |

In plain words: on the walls 8 costs about an eighth more than id's 16 on one
thread, 4 about a third more, exact three quarters more; so 8 buys back
five sixths of exact's extra cost and 4 about three fifths. A whole demo1
frame at 1080p on one thread: +8%, +18%, +49%; on eight threads and in the
browser about half of that. Liquids are where exact costs most (+93%
on the lake) and 4 saves most (+27%).

These are a faster 16 than `fleet/exactpersp` measured (its tables are in
this file's history: 16 → exact +45-49% for the walls): the span
loops' full segments are now loops of a constant length the compiler
unrolls (`raster::span16_cached`, `span_c_cached`, `turb_span`; the last
segment's C division a match of constant divisors). That made 16's walls
16% faster for the 3-D view (main against this branch in one sitting, 1080p,
one thread: 3.29-3.42 → 2.75-2.89 ms; demo1 5.21 → 4.64 ms) and left exact's
where it was (4.89-5.03 → 4.92-5.08 ms; demo1 6.96 → 6.93), so exact's
relative cost reads higher here (+76-79%). It
mattered most for 8 and 4: written as id's loops, with a variable trip count,
8 cost nearly what exact did (on a 1900-pixel oblique span, ns a pixel: 16
1.25 → 0.91, 8 1.92 → 1.04, 4 1.96 → 1.37, exact 1.87; on a liquid 16 1.76 →
1.71, 8 2.30 → 1.85, 4 3.37 → 2.09, exact 3.7). What is left is the
arithmetic: a double-precision divide every N pixels, two saturating float
to integer conversions, and the pixel loop.

**What moved besides.** `--video modern` (`quaketool`) and
`set_video("modern")` (the page's checks, `bench.py --video modern`) are the
2026 set and draw exact perspective; `--perspspan 16|8|4|1` (and the older
`--exactpersp 0|1`, its ends) is a video option of `shot`, `view`, `play` and
`timedemo`; `compare.py --perspspan N` hands it to the port. A number taken
with `--video modern` before 2026-10-03 (PERF_PLAN.md §11, PLATFORM.md's
measurements, the tables above for light styles and torches, whose harness
draws `VideoCvars::MODERN` with id's spans in both columns) has id's spans
and the slower 16. The page's settings checks (`verify_settings`,
`verify_extras`, `verify_save`) expect the 2026 extras to be 13 (uncapped,
exact perspective, scaled 2-D) or 15 with Show FPS; `extras()`'s bit 4 is
"the span is 1". And the Auto pixel size (`vid::AUTO_PIXEL_BUDGET`, "about
6 ms" a 1080p frame a thread) was set against id's spans; with exact
perspective a 1080p frame on one thread is 7 ms, at 4 5.6 ms. Not changed.

## Rerun it

```sh
cd quake-rs && cargo build --release
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK             # every table, text (2.5 min)
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK --markdown  # the tables above
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK --check     # fail if an uncapped value leaves its tolerance
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK --only jump,flash --rates 144,480
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK --budget --res 1280x800
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK --lerpmove  # monsters between steps (5 s)
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK,PAK1.PAK --lightstyles --res 1920x1080  # gliding lights' cost (8 min)
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK,PAK1.PAK --torchflicker 1 --res 1920x1080 --secs 3  # flickering torches' cost (7 min)
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK,PAK1.PAK --bake --threads 1,2,4,8,16 --res 1920x1080  # the bakes on 1-16 threads (10 min)
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK --torchflicker 1 --dump DIR --strengths 0,1 --rates 60 --res 960x540 --secs 10 --view arch=e1m2:1488,1240,296:270  # raw frames for a clip
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK --perspspan --rates 240 --res 1920x1080 --threads 1 --reps 5 --secs 3  # the perspective spans' cost (6 min)
./target/release/quaketool framerate ../quake-data/ID1/PAK0.PAK --perspspan --dump DIR --rates 60 --res 1920x1080 --secs 10 --view corridor=e1m6:504,500,220:100 --turn -1.5  # the spans' clip, raw (15 GB)
```

Each scenario restarts the process's random sequences (`server::reset_random`)
before it loads its map, so two rates differ only by their frame times.

Without the game data, `cargo test` checks the same things on synthetic
worlds, at 60, 144 and 480 Hz against 72, with the tolerances stated in each
test: `server::sv_phys::tests::uncapped_frames_jump_and_bounce_like_72_hz`
(a jump and a bounce: apex to 0.05 units, landing to a 72 Hz frame),
`server::sv_phys::tests::uncapped_walker_stays_on_the_ground_after_a_step_up`,
`client::view::tests::uncapped_flashes_fade_like_72_hz`,
`particles::tests::uncapped_trails_are_as_dense_as_72_hz`,
`client::host::tests::host_filter_time_display_runs_every_refresh_and_never_outruns_the_clock`
and `stepping::tests`. Each also shows id's per-frame code failing the
comparison, so the tests have teeth.

## Wiring (quake-wasm)

*Done: `q26/settings` wired it as described below (`02a32ad`).
`quake-wasm/src/host.rs` has a `FrameGate` that picks the gate and the
`Stepping` from `wasm_uncapped`, and a timedemo keeps id's uncapped gate. The
rest of this section is the note as the framerate branch left it.*

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

`r_lerpmove` is a setting (`quake_rs::cvar::Cvars::lerpmove`, the console's
`r_lerpmove 0|1`, Options > Classic / 2026 > "Smooth monsters"): off in the
Classic profile, on in 2026, handed to the walk and the demo each frame in
`host::step` beside `viewsize` and the renderer's threads (a timedemo ignores
it). `r_lerpmodels` ("Smooth animations") is wired the same way, beside it;
a timedemo ignores it too — `cl_demo::timedemo_frame` always reads
`LerpModels::Classic`, not `d.lerpmodels` (`timedemo_frame_lerpmodels`
exists only for `quaketool timedemo --lerpframe`'s own measurement, above).

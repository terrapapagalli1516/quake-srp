# Faithfulness Audit — Rust port vs id's original C

The ledger of every difference found between the port and id's WinQuake C, and what was
done about it, with the evidence. Newest work is at the bottom. This top part is the way
in:

1. the two profiles, and every departure the 2026 profile makes, as one table;
2. an index of the 2026-09-26 and 2026-09-25 sections;
3. everything still open, in one list.

## The profiles and the departures

Since 2026-09-26 every departure from id's game is a setting. `quake_rs::cvar::CVARS`
marks each one `departure`, and two profiles switch them all at once
(`quake_rs::settings`):

- **Classic** has every departure off and `default.cfg`'s bindings. It is WinQuake:
  frames, game state and timing, proven by `uv run oracle/classic_check.py`
  (`oracle/README.md`, "Classic").
- **2026** is the default: an idealized software-rendered Quake on a 2026 machine.

To switch, use Options > "Classic / 2026" (←/→; Enter lists every row), the console's
`profile classic|2026`, or the page's `?classic` / `?2026`. Each row can also be set
alone, on that settings page or as its console variable. Switching profile resets the
departures and the bindings, and keeps id's own settings (Screen size, Brightness,
the volumes, the mouse).

| departure | setting (settings page row) | 2026 | why |
|---|---|---|---|
| No 72 fps cap: a host frame on every display refresh, with the game stepped as id's 72 Hz frames step it (`Stepping::Uncapped`) | `wasm_uncapped` (Uncapped framerate) | on | The game must play the same from 60 to 480 Hz. id's gate caps the game at 72 fps, so a 120 Hz display runs at 60. The stepping keeps jumps, flashes, trails and clocks on id's 72 Hz values (`FRAMERATE.md`). |
| The picture fills the window at the window's aspect, at its device pixels divided by a whole pixel size, with square pixels; the renderer's `hires` (views past 1280x1024, particles and the underwater warp in proportion). Video Options (2026 only) lists Native — Auto, then 1x..4x pixel size — above `RESOLUTION_PRESETS`, honestly marking whichever is actually live (printing its real size) and letting Enter switch back to it after a fixed mode; off (Classic, or the host never says `modern`), the screen is `VID_MenuDraw`'s alone | `vid_native` (Native resolution), `vid_pixelsize` (Pixel size) | on, Auto | id's modes stop at 1280x1024 and are shown in a 4:3 box. Whole pixels keep the chunky software look. Auto picks the smallest size that keeps a frame within 1920x1080 pixels times the whole square root of the render threads. ("High resolutions and Hor+") |
| Hor+: `fov` spans a 4:3 screen, and a wider screen sees more at the sides | `fov_adapt` (Widescreen FOV) | on | id spreads `fov` over any width, so a wide screen loses the top and bottom. |
| The 2-D layer (status bar, menus, console) at the largest whole multiple of 320x200 that fits; on its screen, wider than 320 on most frames (384 at 16:9), the level-complete screen centred as the status bar is (`Sbar_DrawPic`'s `(vid.width - 320)>>1`, `Screen2d::centred_320_x`) | `wasm_scaled2d` (Scaled 2-D layer) | on | id draws it 1:1, so at 1440p the status bar is a 24-pixel strip. `Sbar_IntermissionOverlay` draws at fixed coordinates laid out for 320 columns, so on a wider screen it sits left of the centred bar, menus and finale ("The 2-D layer on a wide screen", below). |
| Monsters glide between their 0.1 s steps (QuakeSpasm's `r_lerpmove`) | `r_lerpmove` (Smooth monsters) | on | At high refresh rates a stepping monster visibly jumps ten times a second. ("Demo playback between messages") |
| An alias model's animation blends between its frames (QuakeSpasm's `r_lerpmodels`) | `r_lerpmodels` (Smooth animations) | on | id steps `frame` ten times a second; at high refresh rates a monster's walk cycle (and the view weapon's) visibly holds a pose for several frames, then jumps. (`client::lerpmodels`, FRAMERATE.md "Animation frames blended") |
| id's crosshair (`V_RenderView`'s `+`), left off the intermission and finale screens as id's GLQuake leaves it (`gl_screen.c`'s `SCR_UpdateScreen`; WinQuake draws it over the stats too) | `crosshair` (Crosshair) | on (id: 0) | Mouse aiming; a level's stats have nothing to aim at. |
| Mouse look without holding `+mlook` | `freelook` (Mouse look) | on | How mouse play works today; `+mlook` still works in both profiles. |
| Always Run | `cl_forwardspeed`, `cl_backspeed` 400 (Options > Always Run) | on (id: 200) | |
| WASD: `w`/`s` forward and back, `a`/`d` strafe, over `default.cfg`'s `a` `+lookup` and `d` `+moveup` | the profile's bindings (`Bindings::with_wasd`) | on | |
| The wheel switches weapons: a notch up `impulse 10` (next), down `impulse 12` (previous) | the profile's bindings (`Bindings::with_wheel`) | on | `default.cfg` predates the wheel (`in_win.c`'s `WM_MOUSEWHEEL` already turns it into `MWHEELUP`/`MWHEELDOWN` key presses), so id's players bound it themselves. |
| Jump also swims up (`upmove`), on top of QuakeC's own swim | `cl_jumpswim` (Space swims up) | on | With WASD, `d` no longer swims up. |
| Alt+Enter toggles fullscreen, whatever has the keyboard (the page takes the chord before the game) | `vid_altenter` (Fullscreen key; `vid_fkey`, its name when the key was F, still sets it) | on | `default.cfg` binds ALT `+strafe` and ENTER `+jump`, so in WinQuake the chord is a strafe-jump. A letter could not work in the menu and the console (`web/PLATFORM.md`, "Fullscreen"). |
| id's mixer at the device's rate with four of its faults fixed: the click at a loop's restart, 48 kHz pitch 1.4% flat, ambient fades stalling above 100 fps, `S_StopSound`'s channel range | `snd_modern` (Full-rate sound) | on | Classic is id's mixer as written, at 11025 Hz. ("The engine's own mixer") |
| Touch controls on a touch screen (stick, look by dragging, fire, jump, weapon), and the live game pauses when the page is hidden | `in_touch` (Touch controls) | on | Phones. Classic on a touch screen keeps a MENU button and the tappable menu. ("Touch, install and offline") |
| A gamepad as a twin-stick pad: in_win.c's advanced joystick layout (`joystick 1`, `joyadvanced 1`, the axis maps and sensitivities), a round dead zone, a look curve, the pad in the menus, and the 2026 pad bindings | `joystick` (Gamepad), `joyadv*`, `joy*sensitivity`, `joy*threshold`, `joy_deadzone`, `joy_exponent`, `joy_menukeys` | on | id's `joystick 0` reads no pad. ("Input") |
| Rumble on damage and on the big guns (the pad, or a phone's vibration) | `joy_rumble` (Rumble) | on | |
| QuakeWorld's frame-rate readout | `wasm_showfps` (Show FPS) | off | Clutter. |
| Exact perspective at every pixel | `wasm_exactpersp` (Exact perspective) | off | id's 16-pixel spans are part of the look. |
| The `ED_Alloc` edict ceiling past id's 600 (`Vm::max_edicts`) | `sv_max_edicts` (console only, no settings row — nothing to choose until a map needs it) | on, 8192 | id's own number, kept for Classic. No map of id1 or the mission packs needs more: Rogue's `r2m6`, which seemed to, overflowed only while the port kept its statics' edicts ("`makestatic` frees its edict"). Room for bigger maps. |

**The 2-D layer on a wide screen.** Every 2-D draw that WinQuake places by screen
coordinates, and where it sits on a 2-D screen wider than 320 (any mode past 320x200 in
Classic; the scaled layer's own screen in 2026):
- Centred by id, so in both profiles: the status bar, inventory and solo scoreboard
  (`Sbar_DrawPic`/`Sbar_DrawString`'s `(vid.width - 320)>>1`), the menus (`M_DrawPic`,
  `M_DrawCharacter`), the finale plaque (`Sbar_FinaleOverlay`, `(vid.width -
  pic->width)/2`), centre prints and the finale text (`SCR_DrawCenterString`) and the New
  Game prompt (`SCR_DrawNotifyString`), line by line `(vid.width - l*8)/2`, and the pause
  plaque (`SCR_DrawPause`).
- Laid out for a 320-wide screen at fixed coordinates: `Sbar_IntermissionOverlay` alone.
  Classic keeps it in the top-left corner, as id; 2026 centres it (the scaled 2-D row).
- Anchored by design, left where id puts them: the notify lines (`Con_DrawNotify`, at the
  console's own left margin, wrapped to its width), the console, the crosshair (the view's
  centre), and Show FPS (QuakeWorld's bottom-right corner).
- Not ported: `SCR_DrawLoading` (centred by id; the port draws no loading plaque),
  `SCR_DrawRam`/`SCR_DrawTurtle`/`SCR_DrawNet` (id's surface-cache, slow-frame and
  lost-connection icons, at the view's top-left corner), `Draw_BeginDisc` (the screen's
  top-right corner), and deathmatch's `Sbar_DeathmatchOverlay` (centred by id) and
  `Sbar_MiniDeathmatchOverlay` (at a fixed x = 324: to look at again if deathmatch is
  ever ported).

**The same in both profiles** (id's behaviour, or the platform's, not a departure):
- Raw mouse (`unadjustedMovement`): id's `IN_StartupMouse` switched pointer acceleration
  off.
- Keys by their physical place: id's scancodes.
- `r_threads`: the pixels are the same for any thread count.
- On a touch screen the menus answer taps (a tap becomes a key id's menu takes).
- QuakeC errors end the game: id's `Host_Error`.
- The CD plays the player's own tracks (`cd_win.c`; with none there is no drive, as
  `cd_null.c`).
- A player's own `pak1.pak` goes through id's search path.
- Classic's joystick has a few small departures of its own ("Input").

The names still carry the old era: `wasm_*` for four of the cvars, `MenuScreen::Extras`
and `EXTRAS_*` in the menu code. Renaming them needs `config.cfg` aliases (Open, "Code").

## The 2026-09-26 sections

The 2026 push, in merge order. Each section names its branch; the merge message on
`quake/2026` summarises it too (`STATUS.md`, "2026-09-26: the 2026 push").

- **The engine's own mixer** (`q26/audio`): `snd_dma.c`, `snd_mix.c` and `snd_mem.c`
  in the engine, sample-exact against id's C; the 2026 mixer's four fixes; the browser
  plays it through an AudioWorklet.
- **The browser as a WASI program** (`q26/platform`): saves and `config.cfg` as files;
  `play`; `S_StopAllSounds` in call order; each level's placed loops.
- **High resolutions and Hor+** (`q26/hires`): 44.20 edge `u` past 2048 columns;
  particles and the underwater warp in proportion; Hor+.
- **Demo playback between messages** (`q26/lerp`): Classic demos drawn as id's
  `CL_LerpPoint` draws them (a Classic fix); `r_lerpmove`.
- **QuakeC errors end the game** (`q26/server`): `PR_RunError`'s report, `Host_Error`,
  `error`/`objerror` (CENSUS L16).
- **Settings and profiles** (`q26/settings`): Classic's controls are id's; `+mlook`; the
  crosshair; `config.cfg` as id's; one command table; the profiles.
- **Input** (`q26/input`): id's joystick; the 2026 pad; keys by place; raw mouse;
  latency.
- **Touch, install and offline** (`q26/mobile`): `in_touch`; tappable menus; pause when
  hidden.
- **The player's own Quake** (`q26/content`): the search path; `COM_CheckRegistered`;
  CD audio.

Without a section here, because they did not change what Classic draws or does (each was
checked against the goldens, the play hashes and the census):
- `q26/framerate`: the uncapped stepping; `FRAMERATE.md`.
- `q26/multicore`: the renderer owns its state and draws on N threads, byte-identical
  at any N; `web/PLATFORM.md` "Threads", `PERF_PLAN.md` §11.
- `q26/present`: 8-bit frames with the palette applied at presentation; `PERF_PLAN.md`
  B5, `web/PLATFORM.md` "Presentation".
- `q26/vm`: typed entity fields, a private VM, opcodes decoded at load; it also closed
  two open items, the string heap growing and the output log never drained;
  `CODE_PLAN.md` R5 and R10.
- `q26/tool`: quaketool as one directory, with byte-identical output.
- `q26/rustcheck`: `CODE_PLAN.md`.
- `q26/review`: five small fixes; `STATUS.md`.

## The 2026-09-25 sections

In file order (roughly merge order). Each names its branch; the merge message on
`quake/overnight` summarises it too.

- **Options menu + screen framing** (`quake/options`) — 4 Hz menu and console cursors;
  Screen size is `viewsize` again, the view framed by `SCR_CalcRefdef` above the status
  bar; the gun at `V_CalcRefdef`'s origin; menus fade and print bronze; Reset = `default.cfg`.
- **Session 7 — oracle-measured render fixes** (`quake/fid1`) — liquids and sky from the
  raw texel with `Turbulent8`'s math; `R_MakeSky`'s layers; id's alias pipeline
  (`D_PolysetDraw`, colormapped, affine); the gun's angles; faces without light samples black.
- **Host loop: Host_FilterTime's 72 fps cap** (`quake/host`) — the 72 fps gate, with a
  1 ms tolerance.
- **Frame composition** (`quake/perf-b`) — palette shifts as the software
  `V_UpdatePalette`'s integer ramps.
- **World pass: polygon spans, dlit surface cache** (`quake/w1`) — id's fill rule and
  `D_CalcGradients`; dynamically lit walls through the surface cache; animated wall
  textures no longer frozen; `R_AddDynamicLights` in integers.
- **World pass: mip levels, lightmap stepping** (`quake/w2a`) — `D_MipLevelForScale` and a
  cache block per mip level; `R_DrawSurfaceBlock8`'s integer lightmap stepping.
- **Census client/host fixes** (`quake/fix-client`) — CENSUS F1 F2 F4 F6 F10 F11 F13–F18,
  L1 L2 L8 L9 L11 L12 (part) L14 L24.
- **Census fixes, server side** (`quake/fix-server`) — Chthon's lightning (`MSG_ALL` temp
  entities); CENSUS F3 F5 F7 F8 F9, L4–L7 L18 L20 L25 (half).
- **Review fixes** (`quake/polish`) — loading keeps the options; `D_PolysetDraw`'s int
  wrap; the underwater view in id's 320x200 warp buffer; the unwrapped `intsintable`; the
  client clock is `cl.time`; `quaketool playtest` framing; prints reach the console.
- **The 2-D layer against id's composited screen** (`quake/fid2d`) — the 2-D layer 1:1 as
  WinQuake draws it; id's console; the DOS quit prompt; status-bar and centre-print offsets.
- **Projection and spans** (`quake/w2b`) — the pixel aspect in the projection (the world
  was 1.2x tall); `D_DrawSpans16`; the sky centred on the screen; `wasm_exactpersp`.
- **Web extras: the opt-in departures** (`quake/extras`) — the Web extras page (since
  2026-09-26 the "Classic / 2026" settings page); `viewsize` persists; Esc in fullscreen.
- **Entity culling and resolved fields** (`quake/sim`) — `SV_WriteEntitiesToClient`'s PVS
  test and efrag statics (CENSUS L22: flashes lit walls through walls); fields resolved
  once per progs.
- **Second review fixes** (`quake/polish2`) — the oracle's underwater rectangle;
  `D_DrawParticle`; `sv.time` a double; `sv_gravity` outlives the map; per-menu cursors;
  the Load menu after a reload; a hum that could outlive its stop.
- **World pass: id's edge renderer** (`quake/edge`) — `r_edge.c`/`d_edge.c` for the world
  and brush models; entities against the 16-bit z-buffer.
- **Second review fixes, client side** (`quake/polish3`) — no weapon flash at level start;
  a mover's stop sound survives the sound cap; console prints reach the notify lines;
  menu and console key routing; old saves' player name; the warp keeps its tables; demo
  particles fall by `sv_gravity`; the canvas is the largest 4:3 box.
- **Demo commands, timedemo, pause** (`quake/timedemo`) — `playdemo`, `stopdemo`,
  `startdemos`, `demos` and the disconnected state; `timedemo` with id's frame counts;
  `pause` and `SCR_DrawPause`; why there is no loading plaque.
- **Final review fixes, UI side** (`quake/polish4b`) — every key through `Key_Event`; boot
  into the attract demos, the menu stops the loop (`M_Menu_Main_f`); `help` is the Help
  screen; console history, completion and backscroll; Multiplayer > Setup.
- **Final review fixes, engine side** (`quake/polish4a`) — dynamic lights on moved brush
  models in world space; views clamped to `MAXWIDTH`x`MAXHEIGHT` (since 2026-09-26 only
  in Classic); `setmodel`'s alias and sprite boxes (CENSUS L10); malformed-map hardening.

Without a section here: the census itself (`CENSUS.md`), the oracle (`oracle/README.md`),
the performance plan (`PERF_PLAN.md`), and the structure-only branches — the render,
server and quake-wasm splits and the move of the game client into `quake_rs::client` —
which were byte-identical (STATUS.md).

## Open, as of 2026-09-26

Everything known to differ from id's WinQuake in Classic, or not yet checked, gathered
from the sections below, `CENSUS.md`, `oracle/README.md`, `FRAMERATE.md`, `PERF_PLAN.md`
and the closing review of 2026-09-26. One line each, with where it came from. Items
marked *(2026-06)* were not re-checked since. Struck items were closed on 2026-09-25 or
2026-09-26, and say where.

**Closed since the 2026-09-25 list**
- ~~Four control departures on by default (mouse look held while the pointer is locked,
  WASD, `f` for fullscreen, Space adding swim-up speed)~~: Classic has `default.cfg`'s
  bindings and none of them; 2026 has each as a named setting (settings).
- ~~The main menu does not stop the attract loop~~: it does, as `M_Menu_Main_f`
  (polish4b; the 2026-09-25 list missed it).
- ~~`setmodel` gives alias and sprite models a zero box~~: id's box (polish4a, CENSUS L10).
- ~~`objerror`/`error` do not end the game~~: they do, through `Host_Error`, and so does
  any QuakeC runtime error (server, CENSUS L16).
- ~~The browser console has no `d_mipscale`/`d_mipcap`~~ (settings).
- ~~The framebuffer is RGB, not 8-bit (PERF_PLAN B5)~~: 8-bit, with the palette applied
  at presentation (present).
- ~~`Vm::intern` never de-duplicates and `vm.output` is never drained~~ (vm, `d63a0b4`,
  `267fb8c`).
- ~~`quake-rs/src/lib.rs`'s crate doc describes only the file loaders~~ (review,
  `5af262e`); ~~`render/surf.rs`'s `mipadjust` comment is backwards~~ (docs).
- ~~Static sounds of one sample are separate Web Audio sources; the page plays at most
  16 one-shots a frame~~: the page no longer mixes; id's mixer runs in the program
  (audio).
- ~~CD audio is not modelled~~: `cd_win.c` with the player's own tracks (content).

**Game and client**
- `angle_vectors` (`math.rs`) keeps the angle in f64 where id's `AngleVectors` works in float:
  facing exactly perpendicular to a trigger, the port's facing test can flip (the start map's
  "Walk into the Slipgate" message showed 9 frames before id's at yaw 180; the sound walk
  approaches diagonally) (telesound, 2026-10-02).
- Movement angles reach the server unrounded (`sv_user.rs`); id's client sends them as a byte
  in 1.40625° steps, so 180 arrives as −180 (telesound).
- Explosion and impact particles start at the server's exact positions; id's client gets them
  rounded to 1/8 unit (sounds are rounded since telesound's `wire_coord`) (telesound).
- ~~Missing `default.cfg` binds: F1–F4, F6, F9, F10, F12~~ — ✅ fkeys: `keys.rs`'s
  `default_cfg` binds all eight to id's console lines (F5/F7/F8/F11 stay
  unbound, as id's own file leaves them). `t` `messagemode` and the
  `zoom_in` alias (CENSUS L12; its `pause` half is done) remain open. No
  loading plaque, on purpose: loads finish inside a frame (timedemo).
- ~~Console commands id has and the port does not: `version`, `disconnect`
  (it does not end the game)~~ — ✅ fkeys: `version` prints `CON_VERSION` and
  the profile; `disconnect` runs `CL_Disconnect` (`cl_disconnect`, already
  shared with `quit`/`Host_Error`). `wait` is ✅ too (`Cmd_Wait_f`: the rest
  of a bound key's line, scoped to that one call rather than id's single
  shared buffer, waits for the next host frame — `host::step`, where
  `Cbuf_Execute` would run it). Among others `alias`, `playvol`, `soundlist`
  are still open (review; the port's 42 are in `wasm_help`).
- Multiplayer > Setup's name does not reach the player's edict: the server connects
  "player" (polish4b).
- `pause`: the `pausable` and `showpause` cvars (id's defaults, both 1, are what the port
  does) and `VID_HandlePause` are not modelled; `timedemo` sets `cls.timedemo` only when
  the demo opens, where id's sets it anyway (timedemo).
- `give` is not `Host_Give_f`: it clamps, has an armour case, fills a missing amount, and
  selects the weapon (CENSUS L13).
- No pitch drift on slopes: `cl.idealpitch` is fixed at 0 (CENSUS L3).
- ~~`makestatic` keeps the edict (id frees it into the signon) (CENSUS L17)~~ — ✅
  makestatic: `PF_makestatic` writes the static into the server's signon list and frees
  the edict, the client draws the list; the edict numbers and live counts are id's, and
  `r2m6` spawns in Classic ("`makestatic` frees its edict", below).
- `checkclient` traces a line of sight instead of using the 0.1 s client PVS (CENSUS L19).
- The gibbed player's head leaves no blood trail: the client skips the player's edict
  before trails, where id skips only drawing it (CENSUS L23).
- The player is the last edict, not edict 1: physics runs it after the map's entities,
  and edict numbers are one off against id's (CENSUS L25; fix-server).
- The player connects at `sv.time` 1.2, id's signon at about 1.4 (fix-server, F8).
- A stuffed `bf` runs in the same host frame; id's `Cbuf_Execute` runs it one frame later
  (fix-client, F6; accepted).
- A QuakeC error while a NEW game comes up (`build_walk_map`, `build_walk_savegame`)
  returns without id's report; no shareware map raises one (server).
- `setmodel` copies the model's name into the string heap, where id keeps the QuakeC
  string; nothing reads the difference (vm).
- The 72 fps gate gives an 85–100 Hz display half its rate, as id's would (host;
  Classic only since 2026-09-26).
- Console `kill` drains only a pending restart, not a same-frame changelevel (2026-06).
- `quaketool`'s walk paths skip the signon settle frames, to keep their output stable
  (2026-06, deliberate).

**Input**
- Classic's joystick departs from in_win.c in small ways: `IN_Commands` keys the newest
  reading (id's, the frame before); a pad that goes away lets its held keys go; the pad's
  turn is gated behind the menu and console; "joystick detected" prints when a pad first
  shows itself (input).
- With `+strafe` held, the 2026 pad's right stick strafes the wrong way (id's one
  `joysidesensitivity` serves both axes; input, not fixed).

**Demo playback**
- No dynamic lights in demo playback: explosions and rockets light nothing (Round 4;
  fix-client F13).
- Demo statics are drawn without the efrag test (same pixels, more work) (sim).
- The loop wrap keeps the ambient ramp warm where id restarts it from 0 (2026-06,
  deliberate; not re-checked since the loop moved to `CL_NextDemo` on `quake/timedemo`
  and the mixer into the engine on `q26/audio`).

**Renderer**
- A map with no lighting lump renders lit; id draws it fullbright. Test maps only (fid1).
- Edge renderer: id's fixed pools (`r_maxedges`, `r_maxsurfs`, `MAXSPANS`) are growable
  buffers, so where id would drop far faces the port draws them (id's demos never come
  close); brush entities join the
  edge list in a fixed order, not `cl_visedicts`' (exact ties only); `r_clearcolor` is
  fixed at 2; a camera inside solid shows floors id leaves as background (not understood,
  not reachable in play) (edge).
- Statics skip the frustum test on their efrag leaves; the packet-overflow cutoff of
  `SV_WriteEntitiesToClient` is not modelled (sim). (~~Statics use the edict's float
  origin and angles, not `svc_spawnstatic`'s bytes~~: the bytes, since makestatic.)
- `SV_ClipToLinks` uses `maxs - mins` where id reads `v.size` (differs only if QuakeC sets
  `mins`/`maxs` without `setsize`) (sim).
- Sprites other than `SPR_VP_PARALLEL` are drawn as facing billboards (`render/sprite.rs`):
  Hipnotic's bullet holes (`SPR_ORIENTED`) stand out of the walls ("The mission packs'
  paths", P5).
- For a mode taller than 16:10 (not a preset) the underwater warp buffer is narrowed
  instead of squeezed through id's aspect (polish; w2b).
- `CL_UpdateTEnts`' `MAX_VISEDICTS` half-cap and its index clobber (undefined in the C)
  are not modelled (ship push, deliberate).
- The surface cache has no fixed-size pool; external `b_*.bsp` boxes bypass it
  (PERF_PLAN C4). Architecture, not pixels.
- Kept, id's: at high resolutions the maps' sub-pixel gaps (T-junctions) show the
  background on about one pixel every 40 frames at 4K (hires).

**The 2026 profile** (not Classic; what the port's own departures still leave)
- Uncapped, a few per-frame roundings in id's code still drift with the frame rate:
  ground friction and acceleration (2–3 units over a run, 5% of a slide), swimming (2–3%
  over a second), QuakeC timers a frame rounds up (lava burns 4% faster at 480 Hz, a
  fall-damage threshold 4.5 units lower), pusher think transitions; air control is not
  measured (`FRAMERATE.md`, "What is left").
- On a phone the JUMP and FIRE buttons overlap the status bar's ammo count (review).

**Menus**
- Video Options lists the port's modes in one column, not `VID_MenuDraw`'s grid with its
  test/default keys (options).

**Sound**
- The client hands `S_StartSound` the QuakeC volume in live play, not the wire byte over
  255 id's client sees (a volume can differ by one step of 255); temp-entity sounds use
  entity 0 where `CL_ParseTEnt` uses -1 (nothing audible) (audio).
- The mixer does not model `GetSoundtime`'s chop after 2^30 pairs (`paintedtime` is
  64-bit), `snd_show`, `soundlist`/`soundinfo` or `playvol` (audio).

**Files and the command line**
- ~~`-rogue`/`-hipnotic`/`-game`, and a `progs.dat` with builtins id's engine never had
  refused at startup instead of at the call~~: `-rogue`/`-hipnotic`/`-game <dir>` each
  layer a game directory over `id1`, in id's order, and `com_gamedir` follows the last
  one added; the eager builtins scan is gone, so a progs that only *declares* a foreign
  builtin (the mission packs' `finaleFinished`/`localsound`) loads, and only an actual
  call to one fails, lazily, as id's own `PR_RunError` does (mission, "The mission packs'
  own file layout and progs", below).
- Not modelled: `-path` (fully replaces the generated search path), `-cachedir` (a
  CD-ROM cache), `proghack`, `cmdline`, the CD's eject and `MCI_NOTIFY_FAILURE`.
- Mission packs: `Host_Give_f`'s hipnotic/rogue arms (new weapon letters, Rogue's split
  ammo fields) are not ported — the port's own `give` was already a simplified stand-in
  for id's letter+digit scheme, not a literal port of it, even for id1; items are still
  obtainable by picking them up in-level. `menu.c`'s ~75 hipnotic/rogue references are
  all the multiplayer game-options screen (episode/level lists, team-colour border),
  which the port has no menu for; checked against the C, nothing applies.
- ~~Rogue's `r2m6` overflows the port's `MAX_EDICTS` (600, id's own number too) during
  plain entity spawn~~ (`fleet/edicts`): the ceiling is `Vm::max_edicts`, a field
  defaulting to id's 600 ([`MAX_EDICTS`]), raised by the console-only `sv_max_edicts`
  cvar (a departure: 600 in Classic, 8192 in 2026 — QuakeSpasm's own `max_edicts`
  default; no settings-page row, see `menu.rs`'s
  `the_settings_page_lists_every_departure_once_in_the_page_idiom` test, since there is
  nothing to choose until a map needs it). Every edict array in both crates was already
  a growing `Vec` (not a fixed `[T; 600]`); the 600 figure was only ever this one
  ceiling check in `Vm::spawn_checked` (`PF_Spawn`) and a matching bound in savegame
  loading — grepped every other `MAX_EDICTS`/`600` in `quake-rs/src` and
  `quake-wasm/src` and none of the rest meant an edict count (test fixture screen
  sizes, an unrelated frame-count default, the *demo file's* own independent and far
  larger entity cap). With the extra off, `ED_Alloc`/`ED_Free`, the numbering and the
  `"ED_Alloc: no free edicts"` wording are untouched — `classic_check` stays ALL PASS —
  and `r2m6` fails exactly as before (`plat2_spawn_inside_trigger`,
  `pr_edict::tests::r2m6_needs_more_than_ids_600_edicts`, `#[ignore]`d: needs the
  mission pack's own `progs.dat`/`r2m6.bsp`, not in this repo). With it on, `r2m6`
  spawns clean at 632 live edicts (`889` entity blocks, `821` spawned) — same test,
  `QUAKE_R2M6_DIR=<dir> cargo test --release r2m6 -- --ignored`. Memory: an edict is
  `entityfields` (per-progs; 195 for id1, 257 for Rogue's) `u32` cells plus ~40 bytes of
  the port's own per-edict bookkeeping (free flag, freetime, touched-leaf list, static
  flag) — about 0.8–1.1 KB each — so 600→8192 costs a few more MB, against the threads
  build's already-fixed 1 GiB shared memory reservation (`web/PLATFORM.md` "Memory");
  the page does not notice either build. Rogue's real,
  never-open-sourced engine's own number is unknown either way (below): this port's
  600→8192 is QuakeSpasm's convention, not a recovered Rogue constant. Still not
  checked against id's C: `-rogue` hangs the oracle on this map (never terminates in
  60s, 20,000+ `Cvar_Set: variable campaign not found` lines), because Rogue's real
  engine registered a `campaign` cvar this GPL WinQuake tree never did — so whether the
  real Rogue engine also needed more than 600 edicts for this map is still open.
  *Corrected 2026-10-02* ("The mission packs' paths", P4): id's C does run `r2m6`
  (`census/packs.py`: the `campaign` lines are one a frame, not a hang) and peaks at 546
  edicts; the port's 632 are 541 plus 91 `makestatic` edicts id frees (CENSUS L17). The
  overflow is the port's, not the map's. *Fixed* (`fleet/makestatic`): `r2m6` spawns
  under id's 600 in Classic, 541 live edicts after its entities load, and 542 at
  t = 4.7 and 10.7 s with the player, as id's C (`census/packs.py`).
- Rogue's demo wire format for a weapon past the standard 7 (the `1<<i`
  re-expansion `demo.rs` already documents as not modelled, for any non-standard progs)
  applies to the mission packs too, if a demo of theirs is ever added — none is in scope
  here.

**Mission packs** ("The mission packs' paths", 2026-10-02; the re-release's progs were
written for its own engine)
- Its strings are localization keys: pickups, obituaries, centerprints and the finale
  texts print as `$qc_got_item$qc_double_shotgun`, `$qc_finale_hip1` (P6).
- Its end-of-pack flow calls `finaleFinished` (#79), which id's engine and the port
  refuse with a QuakeC error, then `localcmd("menu_credits")` (P7).
- `PF_cvar` returns 0 for every cvar outside the server's own list: Hipnotic's footsteps
  (`cvar("crosshair") == 2`) never play (P9).

**Older LOW tail** *(2026-05/06, not re-checked)*: `PF_particle`'s byte count and
direction quantising; `clip_box`'s inopen/plane-distance coordinates; `SV_NewChaseDir`'s
integer abs; `OP_ADDRESS`'s world guard; `AngleVectors` in f64; `ST_RAND` syncbase;
tracer parity; sky-name case; `push_entity`'s trigger order against `SV_Impact`; sprite
group syncbase (Round 2, Round 5).

**Code** (the closing review's ranked list; `CODE_PLAN.md` has the plan)
- Old-era names: the `wasm_*` cvars (`wasm_uncapped`, `wasm_showfps`,
  `wasm_exactpersp`, `wasm_scaled2d`, `wasm_help`), `MenuScreen::Extras`, `EXTRAS_*`
  and the `extras` automation calls. Renaming needs `config.cfg` aliases.
- rustfmt and edition: 117 files are not rustfmt-clean, and quake-rs is still on edition
  2021 (CODE_PLAN W0a).
- 12 rustdoc warnings in `server/` and the VM, and dead public functions (`Vm::global_ofs`,
  `Vm::ret_int`, `sound_names` in `server/mod.rs`).
- 16 `thread_local!` blocks remain (from 39), among them `FILES`/`GAMEDIR` in
  `quake-wasm/src/common.rs` and the scaled 2-D layer's `SCALED_2D` in `draw.rs`.

**Tooling**
- `quaketool view --vrect` draws an underwater view unwarped, so the warp below viewsize
  120 is not compared (polish2).
- The oracle harness hands the port light styles 0.1 s off at settle ≥ 3 on e1m1
  (oracle README); sprites and the intermission were never compared.

**Not verified**
- A real browser on a real display: every browser check ran headless (Chromium, and
  Firefox for most), on a desktop GPU at best. Not tried: Safari and iOS (WebKit
  would not start here), a real phone, a real 120–480 Hz display, real pointer-lock
  behaviour (headless Chromium's lock jumps the pitch), a real gamepad (the checks emulate
  the Gamepad API), and Esc under the Keyboard Lock API in fullscreen.
- Sound was checked by its counters, samples and the C oracle, never by ear.

---

## The original audit (2026-05-31)

Generated 2026-05-31 by a 17-subsystem map→adversarial-verify pass comparing
`quake-rs`/`quake-wasm` against id Software's GPL Quake C (`WinQuake/`). **66
confirmed discrepancies**: 15 high, 24 medium, 27 low (the 27 low are mostly
cosmetic edge cases; see the workflow result if needed). This is the working
roadmap toward a fully faithful single-player port. Out of scope (intentional):
multiplayer/netcode, save/load (since ported: `quake-rs/src/save.rs`, id's `.sav`
format), audio mixing internals (we use Web Audio), video/platform init.

Per-subsystem confirmed/rejected: physics 7/0 · user-move 5/1 · builtins 5/2 ·
sbar 10/0 · renderer 6/1 · particles 6/0 · demo 6/0 · sound 4/0 · sv_main 4/1 ·
mdl 4/1 · world/move 3/1 · lighting 3/2 · bsp 2/0 · vm 0/1 · progs 1/7 · pak/wad
0/5 · mathlib 0/4. (The VM, edict loader, asset loaders and mathlib audited
essentially clean — most of their "findings" were rejected as faithful.)

## Session 2 (2026-05-31, extended push)

Beyond the 66-finding audit, this round fixed the user's playtest issues + ongoing refinements:
- ✅ **e1m3 performance** (`ca15409`) — lightmap surface cache + frustum culling + per-face geometry cache. Warm frame ~433ms → ~4ms (~100x). Pixel-identical (golden-verified).
- ✅ **Button/switch alternate textures** (`67f490b`) — BModelInstance.frame threaded so func_button/door select the +a..+j cycle when activated (pressed button turns green).
- ✅ **On-screen messages** (`67f490b`) — centerprint (centered) + sprint/bprint (notify) routed + drawn with Quake timeouts.
- ✅ **Sound choppiness** (`web`) — decoded-WAV cache (content hash) kills per-play decode latency.
- ✅ **Stereo pan law** + **svc_particle** + **demo interpolation activation** (earlier this round).
- A review agent re-verified the prior fixes faithful (SV_WaterMove, colormap, HUD coords, push physics, demo interp) and flagged: Options menu is a 3-row reduction vs id's 14-row M_Options_Draw (sliders/checkboxes), main-menu Help/Quit-confirm missing, rocket-trail if/else order (cosmetic, mutually-exclusive flags), invuln 666/disc (cosmetic).

**Remaining:** Options-menu faithfulness (sliders/checkboxes + functional cvars), Help screen + Quit confirm, ambient sounds (last HIGH — looping audio), invuln 666/disc.

## Session 2 visual-fidelity pass (review-agent driven)

A second review agent hunted rendering diffs vs WinQuake; fixed:
- ✅ **Liquid warp** (`171c01a`) — replaced the GLQuake water warp with software `Turbulent8` (integer-texel-driven 128-cycle table, SPEED=20). Correct ripple wavelength + speed on every water/lava/slime.
- ✅ **Faithful Options menu** (`26df181`) — 13 rows with real sliders (128-131) + checkboxes per `M_Options_Draw`; + 6-page Help screen + Quit confirm.
- ✅ **Powerup/content view tints** (`88eee9a`) — `powerup_cshift` (Quad=blue, Biosuit=green, Ring=gray, Pentagram=yellow) in id's blend order.
- ✅ **Weapon-fire punchangle** kick on the view (`88eee9a`).
- Verified faithful (no fix needed): sky DOME projection (active path), alias frame snap (correct for software), liquid opacity, particle color ramps, EF muzzle/bright/dim dlights, particle size, backface cull.

**Remaining polish (subtle / diminishing returns):** view-roll (strafe lean + damage kick — needs a Camera.roll field across all call sites), weapon viewmodel bob/sway/world-lighting, `.spr` sprite entities (glowing light orbs / bubbles — rare in shareware), the bonus-pickup gold flash (`bf` stuffcmd), invuln "666"/disc, EF_BRIGHTFIELD halo, alias-model colormap LUT. **Ambient sounds** (last HIGH) remains — looping web audio + per-leaf ambient_level; audio, not pixels.

## Session 3 — codebase-wide code reviews (≥3 rounds)

Goal (the user's): a faithful port of Quake I to Rust, with at least 3
codebase-wide code reviews, fixing issues as they are found. Each round
spawns parallel review agents over the whole tree, then the findings are fixed,
tested (396 lib tests), golden-verified (e1m1/e1m2/e1m3 scene sha256 unchanged at
`8eea4f9c`/`1edb0642`/`2009e041`), and committed.

### Round 1 — fixed
- ✅ **clip_box freeze (HIGH)** — `world.rs`: a move that merely *starts* inside a
  box hull returned `fraction=0/allsolid` and froze the mover; now matches the C
  box hull (startsolid set, but allsolid only when the move never exits the box,
  fraction=1, endpos=end) so movers slide out instead of locking when AABBs touch.
- ✅ **sv_move allsolid early-out (MED)** — `server.rs SV_ClipToLinks`: once the
  move is wholly trapped, break the per-edict loop (C `if (trace.allsolid) return;`)
  so a later entity's clip can't clobber `allsolid` back to false.
- ✅ **SV_WalkMove ground gate (MED)** — `server.rs`: the step-down only latches
  FL_ONGROUND/groundentity when `ent.solid == SOLID_BSP` (the player gets ground
  from the regular SV_FlyMove slide), matching sv_phys.c; previously unconditional.
- ✅ **powerup_cshift priority (MED)** — `render.rs`: first-match `else if` chain
  QUAD > SUIT > INVISIBILITY > INVULNERABILITY (was last-wins, so Quad+Pent showed
  the wrong tint).
- ✅ **Options "Screen size" Enter resize (MED)** — `render.rs`/`lib.rs`: Enter on
  the resolution row now propagates a `MenuAction::ResolutionChanged` so the host
  reallocates the framebuffer (was cycling the preset but snapping back).
- ✅ **Help nav (MED)** — `render.rs`: up=next/down=prev (M_Help_Key) and
  left/right also page the Help screen (were inverted / inert).
- ✅ **Rocket-trail stale origin (MED)** — `lib.rs`: prune `trail_org` of freed
  edicts each frame so a recycled slot doesn't draw a spurious streak.
- ✅ **PF_random range (LOW)** — `builtins.rs`: divide by `0x7fff` (closed [0,1],
  matches id) not `0x8000`.
- ✅ **ED_Alloc ceiling (LOW)** — `vm.rs`: `MAX_EDICTS=600`; PF_Spawn surfaces a
  `run_error` instead of growing memory unbounded on a runaway spawn loop.
- ✅ **`+A..+J` anim slot (LOW)** — `bsp.rs`: uppercase animated-texture frames map
  to the ALTERNATE cycle (the C upper-cases first, so `+a..+j` and `+A..+J` are the
  same alternate branch); was mapped to primary.
- ✅ **centerprint position (LOW)** — `render.rs`: y = 200·0.35 (≤4 lines) else 48,
  per SCR_DrawCenterString (was dead-centre).

### Round 2 — 15-subsystem workflow (Rust vs WinQuake C) + adversarial verify

60 agents, 45 findings, **37 confirmed** (4 HIGH, 14 MED, 19 LOW), 8 refuted. Fixed
across commits `081beda` (2a), `6d39591` (2b), `c4b4751` (2c). Golden hashes held
byte-identical throughout (`8eea4f9c`/`1edb0642`/`2009e041`).

Fixed:
- ✅ **EF_ROTATE spin** (HIGH) — bonus pickups spin (`anglemod(100*t)`); was frozen.
- ✅ **View roll** (HIGH) — Camera.roll: strafe lean + 80° dead-view + punch roll;
  demo cams replay recorded `viewangles[ROLL]`. (Damage-kick roll needs svc_damage
  — since landed for demo playback, Session 6 2026-06-11: V_ParseDamage's
  directional v_dmg kick from the recorded svc_damage. Live play infers the damage
  *flash* from stat deltas and has no `from` direction, so the live kick stays open.)
- ✅ **EF_MUZZLEFLASH clear** (HIGH) — SV_CleanupEnts at frame top; muzzle light was
  latching forever after the first shot.
- ✅ **Dead/Tab scoreboard** (HIGH) — scorebar + Monsters/Secrets/Time/level on death.
- ✅ **Per-entity skin** (MED) — armor.mdl now green/yellow/red, not always green.
- ✅ **Alias pitch/roll** (MED) — projectiles point along flight path (R_AliasSetUp-
  Transform); zero-orientation fast path keeps golden bit-identical.
- ✅ **SV_TryUnstick + SV_WallFriction** (MED) — wedge escape at BSP seams + into-wall
  tangential friction.
- ✅ **clip_box entry-axis pullback** (MED) — diagonal box-vs-box stops at correct depth.
- ✅ **Damage flash** (MED) — blood/armour split, min-10 floor, 3*count (was 2×), tint.
- ✅ **Stair-step oldz smoothing** (MED) — stairs glide instead of jolting.
- ✅ **Underwater warp** (MED) — D_WarpScreen sine wobble when the eye is submerged.
- ✅ **Weapon icon inv2_*** (MED) — active weapon settles to the bright icon.
- ✅ **Invuln 666 + disc** (MED) — Pentagram-of-Protection armour field.
- ✅ **spawn_burst count==1024** (LOW) — fiery explosion, not SlowGrav dust.
- ✅ **SV_WalkMove waterlevel jump gate** (LOW) — swimmers can step up.

Deferred (documented, lower priority / higher risk):
- ✅ **R_MarkLights BSP dlight gating** (MED) — fixed in the ship push (2026-06-10).
- ✅/⬜ **No-lightmap face fullbright/black** (MED) — sample-less faces (`lightofs
  -1`) now black like id (Session 7); a map with no lighting lump at all still
  renders Lambert instead of id's row 0 (lightless/test maps only).
- ✅ **Intermission view** (MED) — fixed in the ship push (2026-06-10).
- ⬜ LOWs: ~~client_think pre/post-think order~~ (✅ CENSUS L5, `quake/fix-server`); PF_particle byte count/dir quantize;
  clip_box inopen/plane_dist coords; SV_NewChaseDir integer abs; OP_ADDRESS world
  guard; AngleVectors f64-vs-float (golden-sensitive); ~~sky foreground drift~~ (✅ Session 7); ~~particle
  on-screen size ramp~~ (✅ `quake/polish2`, `D_DrawParticle`); ST_RAND syncbase; ~~alias triangle near-clip~~
  (✅ Session 7, `R_AliasClipTriangle`); tracer parity;
  ~~lightstyle /264-vs-/256~~ (✅ Round 5, normalised by 256); sky-name case sensitivity.

### Round 3 — verify-the-fixes + fresh passes (12 dims) + adversarial verify

23 agents, 12 findings, **6 confirmed** (1 MED, 5 LOW), 6 refuted. Crucially the
skeptics REFUTED several "regression-in-new-fix" alarms — re-deriving the math
confirmed the Round-2 view-roll/punchangle, listener pose, and select_interval
truncation were already correct. All 6 confirmed fixed; golden held byte-identical.

- ✅ **Polyblend whole-screen tint** (MED) — software V_UpdatePalette shifts the whole
  palette LAST in SCR_UpdateScreen, so damage/water/powerup tints now cover the HUD,
  centerprint, menu and console too (was 3-D-viewport-only = the GL look). The blend
  is deferred out of step_walk/step_demo and applied to the composited frame.
- ✅ **Monster relink trigger time** (LOW, regression vs an earlier fix's invariant) —
  sv_movestep/sv_step_direction relink touches now use `vm.sv_time` (frame-start
  sv.time) like world.c SV_TouchLinks, not the clamped per-think `time` global.
- ✅ **apply_warp sine table** (LOW) — use id's truncated `3.14159` literal (table tops
  at 5, not 6) for bit-identical D_WarpScreen.
- ✅ **spawn_explosion even/odd** (LOW) — odd→pt_explode / even→pt_explode2, matching
  R_RunParticleEffect/R_ParticleExplosion.
- ✅ **Console/menu mutual exclusion** (LOW) — the menu is suppressed while the console
  is down (Quake key_dest), so the console no longer renders under the menu.
- ✅ **select_interval doc** (LOW) — corrected the false "C loads intervals as a running
  sum" claim (the loader stores them verbatim).

Refuted (no change — verified faithful): dead-view punchangle roll, centerprint-over-
HUD order, listener-pose bob exclusion, PF_makestatic ED_Free, select_interval floor().

**3 codebase-wide reviews complete.** Remaining deferred items (R_MarkLights dlight
gating, no-lightmap shading, intermission view, + the cosmetic LOWs listed under
Round 2) are tracked above for a future pass.

### Session 4 — user playtest fixes + perf + rounds 4-6

User reported: (1) demo brush models not rendering, (2) multi-line pickup messages,
(3) invisible barrel explosion, (4) e1m3 live-play perf terrible (demo fine).

Fixed (commits 6de1c65, 2904090):
- ✅ **Demo brush submodels** — step_demo passed empty bmodels; now resolves "*N"
  precache names to world submodels (doors/elevators render behind the boot demo).
- ✅ **Multi-line pickups** — notify text now accumulates and breaks only on '\n'
  (Con_Print model); "You receive 25 health" was 3 sprint() calls = 3 lines.
- ✅ **Invisible barrel explosion** — `particle(...,255)` is the explosion sentinel
  the C maps to 1024; the live bi_particle recorded it raw. Now mapped (byte-exact).
- ✅ **e1m3 live-play perf (~25×)** — root cause was `find_field`/`find_global`
  LINEAR-SCANNING every def with a strcmp on every ent_get/set (thousands/frame).
  Cached name->offset in Progs (O(1)); + SV_ClipToLinks abs-box broadphase reject.
  600-frame sim: e1m3 22380ms->898ms, e1m1 5708->301ms. PROVEN result-identical
  (sim output byte-identical before/after; golden scene renders unchanged).

### Round 4 — 27 agents, 16 confirmed of 17 (verify-bugfix + fresh passes)

Fixed (commits 6bd546a, 03d91b7):
- ✅ **SV_Impact on world hits** (HIGH) — `if (trace.ent)` includes the world edict;
  the port's `ent > 0` SKIPPED world hits, so rockets/nails never detonated on walls
  and grenades bounced silently. push_entity + fly_move_core now use `>= 0`.
- ✅ **Sprite-model rendering** (HIGH) — facing-sprite pass (draw_sprites) so the
  barrel's s_explod.spr flash + other .spr effects render (live + demo).
- ✅ **TE impact sounds** (MED) — tink/ric (spike/super-spike), wizard/hit, hknight/hit.
- ✅ **Clear notify/centerprint on changelevel** (MED) — stale text no longer lingers.
- ✅ **Viewmodel hidden when dead / invisible** (MED/LOW) — R_DrawViewModel guard.
- ✅ **bi_particle byte-exact** (LOW) — (count & 0xFF)==255 -> 1024, matches MSG_WriteByte.
- ✅ **Centerprint/notify gated on menu/console** (LOW) — key_dest suppression.

Deferred (confirmed, with concrete plans, lower frequency / higher effort):
- ✅ **Lightning/beam temp entities** (HIGH) — fixed in the ship push (2026-06-10):
  `tent.rs` ports cl_tent.c (slot store + CL_UpdateTEnts), live + demo paths.
- ✅ **R_MarkLights dlight BSP gating** (MED) — fixed in the ship push: per-face
  dlightbits via the faithful node recursion (+ a port-specific luxel-extent
  cache-path gate; see the session entry).
- ⬜ **Demo explosion dlight** (LOW) — the demo path emits no dynamic lights.

### Render-perf pass + Rounds 5-6

**Render perf (commit ff4b6ab).** User: higher resolution slows a lot even in simple
maps. Added a render benchmark (`QUAKE_BENCH`/`QUAKE_RES`); render is per-pixel bound
(e1m1 18.4ms@640x400 -> 129ms@1080p). Optimised the textured-triangle inner loop like
Quake's D_DrawSpans: incremental barycentric stepping (3 edge() -> 3 adds) + s/t reuse
the depth reciprocal. ~13-18% faster; near-identical (32-66 of 256k px differ, sub-pixel
edge picks). Bigger remaining lever = `factor_at` bilinear lightmap (~32%) -> a Quake
surface cache (documented follow-up). Golden re-baselined.

**Round 5 (14 agents, 5 confirmed).** Fixed:
- ✅ **World ~1 colormap row too dark** (MED) — lightstyle_scales normalised by 'm'
  (264); id's worldspawn lightstyle(0,"m") -> style 0 = 264 vs the 255*256 white point
  = 1.03125x. Normalise by 256 instead. (commit 12e0550)
- ✅ **Demo bypassed the colormap LUT** (MED) — overbright boot demo; thread gfx/colormap.
- ✅ **R_LightPoint integer truncation** (LOW) — match C RecursiveLightPoint int math.
- Deferred (LOW, documented): push_entity trigger order vs SV_Impact; sprite group syncbase.

**Round 6 — final (13 agents, 5 confirmed).** Fixed (commit d6bb3f1):
- ✅ **physics_step relink** (MED, regression in the sim-perf broadphase) — the freefall
  branch never recomputed absmin/absmax, so a fast-falling MOVETYPE_STEP monster's stale
  abs box could be wrongly broadphase-rejected (missed collision). Relink (SV_LinkEdict)
  before the trigger pass, as the C does.
- ✅ **Demo world too dark** (MED) — seed the demo style 0 = 264/256 (the Round-5 fix
  hadn't reached the demo path).
- ✅ **point_in_leaf strict `d > 0`** (LOW); **megahealth-rot damage flash** suppressed
  (LOW); stale lightstyle doc comment (LOW).
- Refuted: centerprint/notify-after-HUD ordering (verified faithful).

**Golden baseline (as of Round 6; later superseded — see STATUS.md for the
current `fb14bd65`/`a6f98d8a`/`0211e6d4` baseline):** e1m1 `81bca4da`, e1m2 `b93d088a`, e1m3 `df856aeb`
(perf + lightstyle + colormap fidelity applied). 399 lib + 25 wasm tests pass.

**Six codebase-wide reviews complete** (3 + 3). Outstanding deferred work, all documented
above with plans: lightning/beam temp entities (HIGH), R_MarkLights dlight gating (MED),
intermission view (MED), no-lightmap-face shading (MED), the render surface cache (perf),
animated demo lightstyles, + assorted cosmetic LOWs. *(Historical list — since closed:
beams/MarkLights/intermission in the Session-5 ship push, the surface cache in the
render-perf pass, animated demo lightstyles in Session 6's demo-parity work. Of these
only no-lightmap-face shading remains, tracked under "Still open".)*

### Texture lighting cache (lit surface cache, commit 6e95f8a)

Implemented Quake's `d_surf.c` surface cache (mip 0): each lightmapped world surface
is baked ONCE into a per-surface block of final palette indices (texture × lightmap
× colormap), keyed by world + style scales like the lightmap cache (`Rc` blocks,
persistent, rebuilt when a style ticks; gated off dynamically-lit and colormap-less
faces, and blocks over 1<<20 texels). The rasteriser (`raster_triangle_cached`) then
reads ONE byte per pixel + a palette lookup, instead of a texture sample + bilinear
lightmap + colormap-row + colormap-index every pixel.

- **~2.1× faster** warm frames (e1m1 640×400 15.1→7.5ms, 1080p 110→49ms), ~2.4× vs
  the original 18.4ms (with the earlier incremental-edge rasteriser). Deterministic.
- **Fidelity:** the cache samples the lightmap at TEXEL centres (then nearest per
  pixel), which is exactly what Quake's `R_BuildLightMap` + `D_DrawSurfaceBlock8`
  do — so it is MORE faithful to id's software renderer than the port's previous
  per-pixel bilinear lighting (which was smoother than Quake). The two differ on
  ~39% of pixels (avg Δ≈11, max 56 — i.e. about one colormap step, slightly blockier
  lighting gradients), so the world's lighting is now Quake-accurate, not bit-
  identical to the prior render. (NOTE: commit 84e4eda's message wrongly claimed
  "bit-identical / golden unchanged" — that came from corrupted shell output during a
  sandbox fault; this is the corrected record.)
- **Golden re-baselined** (this batch also fixed quaketool's `scene`/`QUAKE_BENCH` to
  read the colormap from the PAK so it exercises the colormap-LUT path the live game
  uses, vs the old failed filesystem read → linear fallback): e1m1 `87c3ff09`,
  e1m2 `d0f48469`, e1m3 `ac2e03dc`. 399 lib + 25 wasm tests pass. The live game
  (always loads the colormap) gets the full 2× in-game.
- Follow-up: mip-level selection would let very large distant surfaces (currently
  over the per-face cap, staying on the per-pixel path) also use the cache and shrink
  block memory.

## HIGH (15)

| # | Finding | Status |
|---|---------|--------|
| H1 | `aim` (#44) never auto-targets — returned `v_forward` unconditionally | ✅ fixed `1146148` (full PF_aim port) |
| H2 | `SV_WaterMove` entirely missing — no swimming physics underwater | ✅ fixed `114ba66` |
| H3 | `SV_WaterJump` (auto climb-out-of-water push) missing | ✅ fixed `1b29e97` |
| H4 | Animated `+texture` sequencing (`Mod_LoadTextures`) + per-frame `R_TextureAnimation` missing — water/teleporters/switches/lights don't animate | ✅ fixed `8f13cb1` (R_TextureAnimation 10 Hz cycles) |
| H5 | ALIAS_GROUP frames never animate — only the first sub-pose is drawn (e.g. flames) | ✅ fixed `8f13cb1` (group-frame + skin-group anim by time) |
| H6 | `pixelAspect` — frame presented 16:10 square-pixel instead of authored 4:3 | ✅ fixed `9876cb4` (present at 4:3); the projection's `pixelAspect` ✅ `quake/w2b` |
| H7 | `R_LavaSplash` not implemented — TE_LAVASPLASH faked as a 20-particle burst | ✅ fixed `8bda8cb` (TE→R_LavaSplash/R_TeleportSplash) |
| H8 | `R_TeleportSplash` not implemented — TE_TELEPORT faked, wrong color | ✅ fixed `8bda8cb` (TE→R_LavaSplash/R_TeleportSplash) |
| H9 | `R_RocketTrail` entirely missing — no rocket/grenade/gib/tracer/voor trails | ✅ fixed `8bda8cb` (trails wired per model flags) |
| H10 | Stale entities never removed — missing the per-message msgtime/relink cull (demo) | ✅ fixed `8f13cb1` (msgtime cull) |
| H11 | Ambient sounds missing — placed `ambientsound()` loops + the 4 automatic leaf ambients | ✅ fixed (ship push 2026-06-10) |
| H12 | Inventory bar (ibar) with weapon icons + current-weapon flash | ✅ fixed `7cbc202` |
| H13 | Animated player face (health/powerup frames; pain-anim is minor TODO) | ✅ fixed `7cbc202` |
| H14 | Item, key, and sigil icons + ammo/armor-type icons | ✅ fixed `7cbc202` |
| H15 | Ammo number always shows shells instead of the active weapon's ammo | ✅ fixed `10ec5bc` (use currentammo) |

Also fixed this session (was a separate reported bug, not in the audit): the
**explosive box** is now shootable — external `b_*.bsp` collision bounds (`ef2c7b5`).

## MEDIUM (24)

**Fixed in wave 1 (`8f13cb1`)** unless noted: skill selection now applies (cvar/cvar_set + spawn filter + carried across changelevel) · `localcmd` inert · `SV_CheckWaterTransition` (splash + watertype/waterlevel) · `SV_Physics_Step` landing thud · `SV_SpawnServer` settle frames · brush-submodel pixel-spread · per-entity `skinnum` · sky dome · alias world-light + dlights · `svc_setangle` no-override · demo inter-frame interpolation (implemented; lib/quaketool still call `parse_demo` — *activation* pending) · EF_ROTATE spin (mechanism; needs model-flag feed) · `R_BlobExplosion` distinct (function ported; TE-wiring pending). HUD big-number x-positions + gold-when-low color fixed earlier (`9876cb4` era). **Still open:** colormap-LUT lighting (needs colormap.lmp input), sound pan law + channel override, and the remaining sbar elements (below).

- builtins: difficulty hard-locked to skill 1 (`cvar("skill")==1`, `cvar_set` no-op, spawn filter only checks NOT_MEDIUM) — **skill selection portals set difficulty but it never takes effect**
- builtins: `localcmd` (#46) faults instead of being a benign no-op
- physics: `SV_CheckWaterTransition` missing — no water-entry splash sound; watertype/waterlevel never set for toss/step entities
- physics: `SV_Physics_Step` missing the landing `hitsound` (dland2.wav)
- sv_main: `SV_SpawnServer`'s two settle frames never run — monsters/items don't droptofloor / settle before play
- world: brush-submodel bounds not pixel-spread for setmodel (world doors/plats 1u too tight)
- mdl: per-entity `skinnum` ignored (always skin 0)
- renderer: sky maps wall (s,t) onto the poly instead of projecting the view-direction dome
- renderer: alias models lit by fixed directional Lambert instead of world light sample + dlights
- lighting: linear 0..4x multiply instead of the colormap LUT (overbright where software Quake has none)
- particles: `R_BlobExplosion` (TE_TAREXPLOSION) routed through the rocket explosion
- demo: `svc_setangle` overrides recorded view angles instead of being ignored
- demo: no inter-frame interpolation (choppy 10 Hz replay)
- demo: EF_ROTATE pickups don't spin
- demo: `svc_particle` count==255 sentinel rendered as fiery explosion instead of `R_RunParticleEffect`
- sound: stereo pan law diverges (C linear 1±dot w/ up-to-2x near gain; JS equal-power)
- sound: channel overriding only dedups within one frame, never cuts a playing sound
- sbar: per-ammo small-digit counts on the ibar missing
- sbar: armor icon by type (+ invuln 666 + disc) missing
- sbar: active-weapon ammo-type icon on the main bar missing
- sbar: low-value gold (anum) digit color not honored
- sbar: health/armor/ammo big-number x positions diverge from sbar.c
- sbar: scorebar / solo scoreboard on death + intermission/finale overlays not drawn

## LOW (27)

Tracked but deferred (cosmetic/edge). A few already landed in wave 1: SV_SetIdealPitch, SV_CheckStuck, groundentity-on-landed-entity, perspective-correct z-buffer (1/z), continuous 1/z particle size, debug builtins inert, light-style default, frame-index reset-to-0. Remaining low items (~~SV_TryUnstick/WallFriction~~ (✅ Round 2), ~~force_retouch~~ (✅ CENSUS F8, `quake/fix-server`), sky case-sensitivity, ~~affine span subdivision~~ (✅ `quake/w2b`, 16-pixel spans), TE color-ramp edge cases, audio cull threshold, etc.) are low-value and unscheduled. The current list is "Open, as of 2026-09-26" at the top.

## Wave 2 — DONE

1. ✅ **Particle/TE spawn-wiring** (`8bda8cb`) — rocket/grenade/gib/tracer trails per model flags; TE_LAVASPLASH/TELEPORT/TAREXPLOSION/EXPLOSION2 routed (TAREXPLOSION split out of the dlight group).
2. ✅ **HUD completeness** (`7cbc202`) — ibar + weapon strip + active flash + ammo counts/icons + keys/sigils + face + armor/ammo-type icons (pain-frame face anim + invuln 666/disc are minor TODOs).
3. ✅ **Colormap-LUT lighting** (`604b2ce`) — `gfx/colormap.lmp` threaded through `render_scene_ext`; no more overbright; visually verified e1m1/e1m3.
4. ✅ **Stereo pan law** (`web`) — linear 1±dot with full near-side gain.
5. ✅ **`svc_particle`** → R_RunParticleEffect (not the rocket explosion).

## Still open (as of 2026-06; superseded by "Open, as of 2026-09-26" at the top)

All HIGHs and the actionable MEDs are closed as of the 2026-06-10 ship push
(see the session entry below). The remaining tail, all LOW / niche:

- **Lightless maps** (no lighting lump): Lambert instead of id's fullbright row 0
  (narrow; test maps only). Sample-less faces in lit maps: ✅ Session 7.
- Demo explosion dlight. (~~Sound channel override only dedups within a
  frame~~ — ✅ closed in Session 6: cross-frame (entity,channel) override +
  S_StopSound in the page registry, live + demo.)
- ~~Minor sbar polish (pain-frame face anim)~~ — ✅ census F16 below; the
  Round-2 LOW list (sky case-sensitivity, AngleVectors f64, lightstyle /264,
  etc. — all cosmetic).
- ~~Live-play damage-kick roll~~ — ✅ census F16 below (live play reads
  `dmg_take`/`dmg_save`/`dmg_inflictor` like SV_WriteClientdataToMessage).

## Session 5 — the ship push (2026-06-10)

Six parallel implementation branches (one agent each, isolated worktrees),
every branch adversarially reviewed by 2–3 independent lenses (faithfulness
vs the C, engineering, integration), blocking findings fixed on-branch, then
merged serially with the full suite + golden renders verified after every
merge. **449 lib + 42 wasm tests; goldens `fb14bd65`/`a6f98d8a`/`0211e6d4`
byte-identical throughout.** Full narrative in STATUS.md; the faithfulness
ledger:

- ✅ **Death→respawn e2e** — `Server::client_kill` (Host_Kill_f port; console
  `kill` was a health hack bypassing QC), physics_client TOSS/BOUNCE arm
  (SV_Physics_Client; dead corpse froze mid-air). The QC death chain proven
  on real progs.dat with a per-frame deadflag trace, incl. environment kills.
- ✅ **Intermission/finale (MED ×2)** — MSG_ALL svc recognizer (WriteByte/
  WriteString dropped everything before), `Server::set_map_name`
  (SV_SpawnServer's world-edict setup was missing entirely — `mapname` and
  `world.model` were empty for ALL QC consumers: samelevel/noexit/runes/
  episode-end), V_CalcIntermissionRefdef, Sbar_Intermission/FinaleOverlay,
  8-chars/sec finale reveal, svc_sellscreen→Help. Review fix: completed_time
  latches the QC `time` global (cl.time epoch 1.0), not the render clock.
- ✅ **TE_LIGHTNING1/2/3 + TE_BEAM (HIGH)** — `tent.rs` (cl_tent.c port:
  24-slot store, same-entity replacement, 30-unit pieces, integer
  vectoangles, rand()%360 roll, view-entity re-anchor), live + demo. Bonus:
  PF_WriteEntity read its parm as float, not an int global (G_EDICTNUM).
  Documented deviations: missing beam.mdl skips pieces (no Sys_Error); the
  C's MAX_VISEDICTS half-cap and outer-loop index clobber (UB) not modeled.
- ✅ **Ambient sounds (H11, last HIGH)** — PF_ambientsound → StaticSound
  registry (wire-byte-exact vol/atten), `snd.rs` (S_UpdateAmbientSounds +
  GetWavinfo cue-loop gate; water1/wind2 only, like the C), demo
  static_sounds, Web Audio looping with the one-shot spatialGain law +
  sound_generation lifecycle. Documented deviations: Web Audio mixing
  internals (scope note); the ramp runs the C's integer master_vol math on a
  fixed 1/72 s accumulator (Host_FilterTime's cap) so the faithful asymmetric
  fade holds at any display Hz (literal per-frame trunc would stall >100fps);
  one looping source per static (no same-sfx combine pass).
- ✅ **First-frames "pop"** — `Server::run_signon_frames` (the C's two signon
  SV_Physics ticks; frame 0 previously rendered the spawn-settle fall), and
  the `any_dlight_reaches` luxel-extent gate (below). Headless A/B: 17.8% +
  13.3% px pops → max 0.86%.
- ✅ **R_MarkLights BSP dlight gating (MED)** — faithful per-face dlightbits
  node recursion (R_PushDlights/R_MarkLights); submodels marked via their own
  headnode (R_DrawBEntitiesOnList) — with entity-local origins until the final
  review, now with the world-space lights as id's (below, "Final review fixes,
  engine side"). e1m1 A/B with an
  injected light: 90,525 affected px → 10,411 (strict subset). Merge
  composition: mask (C-faithful "may contribute") → plane test → luxel-extent
  test — the extent test is a PORT-SPECIFIC tightening of the cache-path
  decision only (the C keys rebuilds on marking alone but always renders
  through its surface cache; this port would flip baked→per-pixel and shimmer
  for zero pixel change). `QUAKE_DLIGHT` knob on quaketool scene for A/B.

**Performance:** resolved — see STATUS.md's scorecard (the surface-cache fix +
clone/alloc hunt landed; 36 fps @1080p idle, per-pixel bound; SIMD remains the
only further ~2× lever and is unscheduled). *(Superseded by `PERF_PLAN.md`,
2026-09-25: the larger levers were Quake's own techniques; `simd128` measured
no gain.)*

## Session 6 — demo playback parity (2026-06-11, branch `ship/demo-parity`)

User: the attract demo must match the real game. In the C the demo IS the
client rendering a recorded stream (full sound, sbar, viewmodel, flashes,
text, recorded lightstyles); the port's demo path discarded most of it.
Faithfulness ledger (ground truth cl_parse.c / view.c / sbar.c / snd_dma.c;
full narrative in STATUS.md):

- ✅ **svc_sound** — CL_ParseStartSoundPacket decode (SND_VOLUME default 255,
  atten byte/64 default 1.0, ent=ch>>3/chan=ch&7, precache-resolved); queued
  through the SAME spatialized path as live, listener = recorded camera. id's
  demo1 carried 595 silently-discarded one-shots.
- ✅ **svc_stopsound + channel override** — S_StopSound + SND_PickChannel's
  cross-frame "always override sound from same entity" via a page-side
  (entity,channel) source registry (closes the audit MED's remaining half;
  channel 0 never keyed). demo1/2/3 send zero stops (engine-asserted census).
- ✅ **svc_lightstyle** — recorded style strings drive demo lighting through
  the shared literal R_AnimateLight math (`server::lightstyle_scales_at`,
  delegation proven byte-identical); the seeded style-0='m' default remains
  only as the synthetic-demo fallback.
- ✅ **svc_clientdata** — CL_ParseClientdata's EXACT bit order (viewheight/
  idealpitch chars, punch char + velocity char*16 interleaved per axis, items
  long, SU_ONGROUND/INWATER, weaponframe/armor/weapon, fixed health/ammo/
  active-weapon trailer); mvelocity shift + CL_RelinkEntities velocity lerp.
- ✅ **Sbar during demos** — the same `render::Hud` as live, fed from recorded
  cl.stats (Sbar_Draw runs during playback in the C).
- ✅ **Weapon viewmodel during demos** — SU_WEAPON via the demo precache +
  SU_WEAPONFRAME, R_DrawViewModel hide gates (dead/invisible/intermission).
- ✅ **svc_damage** — V_ParseDamage: count=(blood+armor)/2 min 10, percent
  += 3*count clamp 150 fading dt*150, blood/armour tint, directional
  v_dmg_roll/pitch kick (v_kickroll/v_kickpitch 0.6, v_kicktime 0.5) decayed
  in V_CalcViewRoll.
- ✅ **svc_print/centerprint** — the live notify (Con_Print '\n' accumulation)
  + centerprint overlays, same gating; svc_stufftext consumed-inert with the
  C's would-exec documented.
- ✅ **Void-camera start/wrap** — frame emission gates on signon completion
  (first entity fast-update = "the final signon stage", cl_parse.c:340 /
  SCR_EndLoadingPlaque); wrap resets the POV state.
- ✅ **V_CalcRefdef parity** — V_CalcBob from recorded velocity, oldz stair
  smoothing on recorded onground, strafe lean, dead-view roll=80 assignment
  semantics, punchangle added LAST; underwater warp + blends deferred to the
  dispatcher like live.

Evidence: `web/verify_demo.py` (new permanent harness) 9/9 + verify_walk/
verify_ambient green; 458 lib + 48 wasm tests; clippy 0/0; goldens
byte-identical (scene renders no demos).

## Options menu + screen framing (2026-09-25, branch `quake/options`)

The user found the Options menu's cursor blinking too fast, and screen size
seeming not to do the right thing. Both real, plus what they exposed:

- ✅ **Cursor blink** — every 12/13 menu cursor blinked off the menudot frame
  parity (10 Hz, host_time); the C is `12+((int)(realtime*4)&1)` (4 Hz, real
  time). The console input cursor was 2 Hz; Con_DrawInput is
  `10+((int)(realtime*con_cursorspeed)&1)`, speed 4. `step(dt)` now splits
  the raw dt like Host_FilterTime (realtime += dt; host_time += min(dt, 0.1)).
- ✅ **View framing (HIGH-visibility)** — the 3-D view was rendered full-screen
  under a pasted-on status bar: horizon at y=100 instead of 76 at 320x200, 24%
  of pixels drawn only to be covered. Now SCR_CalcRefdef + R_SetVrect
  (`render::calc_refdef`): vrect above sb_lines, projection centred on it,
  backtile border (`draw_tile_clear`, Draw_TileClear), sb_lines 48/24/0 gating
  Sbar_DrawInventory / the status strip (death scoreboard still at 0),
  intermission full screen, D_WarpScreen on the vrect only. Proven on e1m1:
  the 100/110 views are the 120 view shifted up 24/12 rows, pixel for pixel.
- ✅ **Screen size = viewsize** — the row had been repurposed to cycle render
  resolutions. Now scr_viewsize (±10, 30..120, slider (v-30)/90), `sizeup` /
  `sizedown` / `viewsize [n]` console commands, default.cfg's `+`/`=`/`-`
  binds, `viewsize 100` on Reset. Resolution lives only in Video Options
  (M_Video), where the preset list already was; localStorage restore intact.
- ✅ **Viewmodel placement** — the gun hung at a hand-tuned offset (7 fwd, 1.5
  right, 3.5 up, near clip 1) calibrated under the old framing; with the fixed
  framing it showed ~3.6x id's gun. Now V_CalcRefdef's origin (forward bob +
  the viewsize fudge +2/+1/+1/+0.5 at 100/110/90/80) and r_aclip.c's
  ALIAS_Z_CLIP_PLANE 5: e1m1 shotgun IoU 0.92 vs an independent projection of
  v_shot.mdl through R_ViewChanged's math.
- ✅ **M_Draw / M_Print** — menus now Draw_FadeScreen the frame under them
  (3-in-4 dither) and print in the bronze conchars half (c+128); M_PrintWhite
  only where the C uses it (current video mode, "No Communications").
- ✅ **Reset to defaults** — execs exactly default.cfg (binds + viewsize,
  gamma, volume, sensitivity); no longer resets bgmvolume / Always Run /
  m_pitch / lookspring / lookstrafe.
- Open: the Video list is one column (the C's 3-wide grid with "Windowed" /
  "Fullscreen" headers and T/D test/default keys is not modelled); the port's
  2-D layer stays a scaled 320x200 screen (WinQuake draws it 1:1 at higher
  modes, with a tiled strip beside a 320-wide sbar) — fixed on `quake/fid2d`; pixel aspect stays square
  (id's 320x200 uses pixelAspect 0.8333 for 4:3 CRTs) — fixed on `quake/w2b`.

Evidence: 489 lib + 67 wasm tests (blink rates, refdef at 100/110/120/90/70/
50/30 + intermission + scaled modes, tile/compose, sb_lines HUD gating, slider,
binds, console commands, Video mode, e1m1 framing, fudge); clippy 0/0; all six
verify_*.py green (verify_menu 60/60 incl. reload persistence); goldens
byte-identical `fb14bd65`/`a6f98d8a`/`0211e6d4` (the scene camera has no
status bar or viewmodel).

## Session 7 — oracle-measured render fixes (2026-09-25, branch `quake/fid1`)

Each fix measured with `oracle/compare.py` (id's own renderer, headless); the
numbers are exact-palette-index match %. Classes refer to `oracle/README.md`.

- ✅ **Liquids and sky overbright** (class 3) — turb/sky went through colormap
  row 0 (~2x); id's `D_DrawTurbulent8Span`/`D_DrawSkyScans8` store the raw
  texel. **Turb warp** (class 8) — now `Turbulent8`'s 16.16 math: `sintable`
  in fixed point (id's `3.14159`, 256 entries, not periodic), added before the
  `>>16`, on `(s+8192)<<16` (`Mod_LoadFaces`' turb `texturemins`). e1m1 water
  from above 21.1% → 99.45%. Goldens: e1m2 `a6f98d8a` → `76905e15`.
- ✅ **Sky layers + sampling** (class 4; was the LOW "sky foreground drift") —
  the front layer is now `R_MakeSky`'s composite, shifted `(int)(skytime*8)`
  texels over the back (so it scrolls at 2x), `skytime` wrapped at 512 s
  (`R_SetSkyFrame`), `D_Sky_uv_To_st` at the integer pixel and screen centre.
  And `D_DrawSkyScans8`'s spans: the world pass records its sky pixels and
  `resolve_sky_spans` redraws each visible run of one sky face exactly every 32
  pixels, stepped between. e1m2 sky region (130,0,65,32, mip 0 + exact
  perspective on id's side): 15.6% at the start, 23.5% after the class-3 fix,
  93.2% with the layers, 99.8% with the spans. e1m2 world 61.25 → 64.07.
  Goldens unchanged (no sky in them).
- ✅ **Alias models** (class 2; the old "alias-model colormap LUT" item) — the
  port lit them with its own heuristic and an RGB multiply (off-palette
  colours, fullbright flames darkened) and rasterised them perspective-correct.
  Now every alias model and the gun go through a port of id's pipeline:
  `R_DrawEntitiesOnList`/`R_DrawViewModel` light (R_LightPoint + dlights,
  128/192 clamps, gun >= 24), `R_AliasSetupLighting` (`LIGHT_MIN`, the
  `{-1,0,0}` light vector in the model frame), `R_AliasCheckBBox`
  (trivial accept, subdivision beyond `r_aliastransition`, `size/11`),
  per-vertex `r_avertexnormals` light, `R_AliasClipTriangle`, and
  `D_PolysetDraw` — affine, Gouraud, integer vertices, `acolormap[texel +
  (light & 0xFF00)]`, 16-bit-z semantics against the shared z-buffer, the
  gun's 1/z tripled. Entity pixels (id with mip 0 + exact perspective):
  e1m2 15.9% → 99.7%, e1m3 71.0% → 99.5%, e1m7 45.8% → 98.2%; the e1m2 altar
  view (ogre + two flames) 21.8% → 100.0% with id as shipped. nonpal% is 0
  everywhere. Goldens: `fb14bd65` → `d103ba3f`, `76905e15` → `a833cbac`,
  `0211e6d4` → `ed42c092` (monsters/items in all three).
- ✅ **Viewmodel** (class 5; the placement itself landed with the options
  branch) — what still differed from id: the gun is drawn by the alias
  pipeline above (lighting, affine, tripled 1/z in the shared z-buffer
  instead of a private depth buffer, no gun at fov > 90); its origin lacks
  the camera's 1/32 node-line epsilon (-1/32 relative, ~0.5 px at 320x200);
  the bob moves it along the full view pitch (V_CalcRefdef has just set the
  entity angles to the view's, not the server's third); and its angles are
  CalcGunAngle's — the view before `cl.punchangle`, without the view roll
  (`Viewmodel::angles`, `viewmodel_angles`). `quaketool view --viewent` takes
  the C's `cl.viewent`. e1m1/e1m2/e1m3 at `--settle 3`, id with mip 0 + exact
  perspective: the frame with the gun matches as well as the world-only
  frame (93.96/96.88/98.52 vs 93.95/96.85/98.51); the gun region 99.9%.
  Goldens unchanged (no gun in them).
- ✅ **Sample-less faces** (class 9) — they are NOT (only) sky/turb: e1m1 has
  375 world + 211 submodel faces with ordinary textures and `lightofs -1`
  (e1m2 431/241, e1m3 431/490 — the light tool found no light reaching them),
  besides its 304 turb + 63 sky faces. The port drew them with its Lambert
  fallback; `R_BuildLightMap` clears to the ambient (0), has no samples to
  add, adds dlights and inverts — black. Now the same. None is visible in the
  oracle's standard views; the e1m1 golden view shows one (a recessed panel
  edge): that view's region 94.6% → 97.7% against id. Goldens: e1m1
  `d103ba3f` → `5b29abb8`, e1m3 `ed42c092` → `e3d873f0` (3 px), e1m2
  unchanged. Still open: a map with no lighting lump renders Lambert, id
  fullbright.

## Host loop: Host_FilterTime's 72 fps cap (2026-09-25, branch `quake/host`)

- ✅ **Frame cap** — the port stepped and rendered once per rAF, so a 144 Hz
  display ran twice Quake's frames. `step()` now gates like Host_FilterTime:
  `realtime += dt` every call; a frame runs only once 1/72 s has passed since
  the last (`oldrealtime = realtime`, overshoot dropped) and advances the game
  by that time clamped to [0.001, 0.1]; `step` returns 0 on a skipped call and
  the page presents nothing. `dt = 0` remains the tests' frozen frame.
- **Deviation (documented at `HOST_FRAME_TOLERANCE`):** a frame may run up to
  1 ms early. A rAF host sees whole vsyncs; at 144 Hz two are 13.889 ms, on
  the knife edge of 1/72 s, and the strict test judders 72/48 fps. With 1 ms:
  60/75/90/100/120/144/165/240/360 Hz give 60/75/45/50/60/72/55/60/72 fps,
  each at a fixed vsync count per frame (unit-tested). Only 75 Hz exceeds 72.
  Accepted gap: 85–100 Hz displays get half their rate (the C gate needs two
  vsyncs there).

## Frame composition (PERF_PLAN B, 2026-09-25, branch `quake/perf-b`)

- ✅ **Cshifts are the software `V_UpdatePalette`'s integer ramps** (B2). The
  port blended the finished frame per pixel in f32 through `V_CalcBlend`'s
  combined alpha, rounding — but `V_CalcBlend` is GLQuake's (`#ifdef
  GLQUAKE`); the software build walks `cl.cshifts` (CONTENTS, DAMAGE, BONUS,
  POWERUP) over every palette level with `v += (percent*(destcolor-v)) >> 8`
  (`int` percent, arithmetic shift: fractions round toward minus infinity) and
  then `gammatable[v]`. `render::cshift_ramps` is that, per channel, and the
  host packs the finished frame through the three ramps (a palette colour's
  channels looked up in them = the C's shifted palette entry). Unit-tested
  against hand-worked C values, including the order dependence (water then
  damage: level 100 → 197; the other way round 160).
  **Re-baseline:** only frames with a shift on move; the goldens have none and
  are unchanged (`5b29abb8`/`a833cbac`/`e3d873f0`). Over id's palette the new
  ramps differ from the old blend on 180–249 of 256 colours per shift, by 1
  level (2 at most, lava and the 150% damage flash). On real frames at 320×200
  (native dumps, old vs new): the Quad walk — 30 of 30 sampled frames differ,
  76–94% of pixels each, every channel by exactly 1, always darker; id's demo1
  — the 28 damage-flash frames (of 720) differ, 80–100% of pixels, |d| 1
  (2 on 11% of changed pixels), darker except 102 channels. The other 692
  frames are byte-identical.
- **Seen, not changed (the cshift *state*, another branch's):** the C keeps
  `percent` as an `int`, so `cl.cshifts[CSHIFT_DAMAGE].percent -=
  host_frametime*150` truncates every frame (150 → 147 at 72 fps, not
  147.9); the port decays a float and truncates only in the ramp. And
  `V_SetContentsColor`'s `default:` is water — any contents other than
  empty/solid/lava/slime, sky included — where `content_cshift` returns none.

## World pass: polygon spans, dlit surface cache (2026-09-25, branch `quake/w1`, PERF_PLAN A0–A2)

- ✅ **A1: faces scan-converted as polygons.** Each clipped world, submodel
  and external-box face was fan-triangulated and each triangle's bounding box
  walked with an inside test, its s/z and t/z interpolated per triangle from
  the vertices (absolute s, thousands of texels, in f32). Now every face is one
  polygon walked row by row (`raster.rs`: `scan_poly`, `raster_poly_cached`,
  `raster_poly_tex`, `raster_poly_flat`):
  - **Fill rule = id's:** pixel centres, left and top edges inclusive, right
    and bottom exclusive (`R_EmitEdge` takes rows `ceil(v0)..ceil(v1)-1`,
    `R_GenerateSpans` columns `ceil(u_l)..ceil(u_r)-1`). Every edge's
    crossing is computed from its top endpoint, and a near-clipped edge's new
    vertex from its inside endpoint, so two faces sharing an edge get the
    same bits.
  - **Gradients = `D_CalcGradients`:** 1/z from the face plane over the eye's
    distance to it (`R_RenderFace`), s/z and t/z from the view-space texinfo
    axes, with s and t relative to the eye's (`sadjust`/`tadjust`); f64
    accumulators; the cached span loop takes the texel as `D_DrawSpans8` does
    (`(int)(sdivz*z) + sadjust`, `>> 16`).
  - **Goldens:** e1m1 `5b29abb8` → `bb64996e` (151 px, 0.059%), e1m2
    `a833cbac` → `8186a64c` (709 px, 0.28%), e1m3 `e3d873f0` → `f41e8b59`
    (729 px, 0.28%). All are interior texel-boundary pixels: none sits next to
    a colour edge, and there are 0 background pixels before and after. Against
    an exact reference (f64 perspective per pixel) the old renderer was wrong
    on 149 / 731 / 722 of those pixels and the new one on 6 / 34 / 13, so the
    moves remove the old f32 rounding.
  - **Cracks:** over 144 oracle views (8 maps × 6 yaws × 3 pitches), the old
    renderer left 5 background pixels at 320×200 and 30 at 640×480; the new one
    leaves 0 at both.
  - **Oracle,** mean exact% over those 144 views, old → new: 320×200
    79.873 → 79.907 (118 views better, 23 worse), with id at mip 0 + exact
    perspective 97.682 → 97.760 (139 better, 5 worse); 640×480 90.636 →
    90.774 (138 / 6), and 97.611 → 97.801 (144 / 0).
  - **Open** *(superseded: after the mip levels, the 16-pixel spans and the edge
    renderer these rows read 99.91–99.98 against id's x86 spans; see "World pass:
    id's edge renderer")*: the 16 standard rows (`compare.py` world, 320×200 and 640×480,
    id as shipped and mip 0 + exact) rise on 9 rows and fall by 0.01–0.03
    points on 7 (e1m1 84.76 → 84.74, e1m7 75.65 → 75.62, …; PERF_PLAN A1
    has the table). The lost pixels are single pixels at texel boundaries
    (some exactly on one, at these axis-aligned start views) and face edges,
    where the port's and id's float rounding fall on different sides. The old
    port's rounding agreed with id's more often on these views, but not on
    average. Tried without effect: floor versus 16.16 texel arithmetic, vertex
    versus plane gradients, f32 versus f64 divides, an id-style float camera
    basis.
- ✅ **A2: dynamically lit walls through the surface cache**
  (`D_CacheSurface` + `R_AddDynamicLights`). A wall a dynamic light reached
  left the surface cache for a per-pixel path, with bilinear lightmap and
  colormap on every screen pixel. Now `face_surf_block` bakes it with the
  light at texel resolution, and marks the entry `dlight` (`cache->dlight`):
  it is never a hit, so the first frame without the light rebuilds it.
  Two fixes come with the C's hit test:
  - **The texture is part of the key** (`cache->texture`). Animated wall
    textures (`+0…`) were frozen on whichever frame was baked first, on every
    cached wall; a unit test now fails on the old code.
  - **The submodel `ent_frame == 0` gate is gone.** An activated button's
    alternate texture is just another texture.

  Goldens unchanged. Oracle muzzle-flash frames (new: `compare.py --c-cmd
  +attack --settle 3`, with id's `cl_dlights` handed to `quaketool view
  --dlight`), mip 0 + exact, 320×200, exact% before → after: e1m1 64.12 →
  90.01, e1m2 88.46 → 96.85, e1m3 76.47 → 97.77; as shipped, 55.96 → 80.30,
  52.28 → 60.66, 40.72 → 60.95.
  - **Pixels the flash lights in id** (e1m3): the port matches 96.1% of them,
    was 23.1%. Of the rest, about 1% are the gunshot's particles (`view`
    draws none) and about 3% are one colormap row off. That is the class-6
    lightmap interpolation (oracle README), which a dlight's steep gradient
    brings out more than static light does. So dlit lighting is per texel
    now, but not id's texel for texel until class 6 is ported. (Ported on
    `quake/w2a`, "Dynamic lights at the chosen level" below.)
  - **e1m1's lit frames** also carry the settle ≥ 3 light-style offset of the
    harness (oracle README).
- ✅ **`R_AddDynamicLights` in the C's integers.** The per-luxel distance
  took float `sd`/`td` and `min/2`. The C truncates the offsets to `int`,
  halves with `>> 1` and truncates `(rad - dist)*256` into the 8.8
  `blocklights`. Now the same. `any_dlight_reaches` keeps a 2-unit margin so
  it stays conservative. Flash-lit pixels (mip 0 + exact, 320×200) now match
  83.4% on e1m1 (was 81.8%) and 96.1% on e1m3 (was 95.9%); no row fell.
  Goldens unchanged.

## World pass: mip levels, lightmap stepping (2026-09-25, branch `quake/w2a`, PERF_PLAN A5)

- ✅ **Mip levels** (oracle class 1, the largest departure). The port baked every
  surface from the full-resolution texture; id draws each surface at the mip
  level `D_DrawSurfaces` picks per frame, `D_MipLevelForScale(nearzi *
  scale_for_mip * mipadjust)`, and caches one block per level. Now the same
  (`surf.rs`: `MipView`, `face_surf_block`):
  - **`nearzi`** is the largest `1/z` over the face's outline clipped to the
    frustum's four side planes, `z` clamped to `NEAR_CLIP` 0.01 — what
    `R_RenderFace` gathers from `R_EmitEdge` (the left clip edge and the
    right one's `1/z` included). `scale_for_mip` is the larger focal length
    (`D_ViewChanged`); `mipadjust` is `Mod_LoadTexinfo`'s 1/2/3/4 from the
    mean texture-axis length; the thresholds are `basemip` {1, 0.4, 0.2} times
    `d_mipscale`, floored at `d_mipcap` (`D_SetupFrame`; id sets no higher
    floor at high resolutions). Both cvars are settable
    (`render::set_mip_cvars`, `quaketool view --d-mipscale/--d-mipcap`;
    `compare.py` hands a `--c-cmd "d_mipscale 0"` to both renderers).
  - **The block** is `D_CacheSurface`'s: `extents >> miplevel` texels a side
    (it was `extents + 1` at mip 0 — one column and row id never reads:
    `bbextents` clamps s to `extents - 1`), from the level's texels
    (`bsp.rs` now keeps levels 1..3, `MipTex::mip`), tiled from
    `texturemins >> miplevel`; one cache slot per face per level
    (`cachespots[miplevel]`). The span walker reads it through gradients
    scaled by `1 / (1 << miplevel)` (`PolyGrads::mip_scaled`, `D_CalcGradients`'
    `mipscale`). Submodels and the external `b_*.bsp` boxes pick their levels
    the same way. A texture without levels 1..3 (synthetic tests) stays at
    mip 0; a face whose texinfo maps it to a line (zero extent; id's
    `D_SCAlloc` would `Sys_Error`) keeps the per-pixel path.
  - **Oracle,** exact% world-only 320×200 (id as shipped): e1m1 84.74 →
    92.20, e1m2 64.06 → 91.03, e1m3 65.88 → 96.68, e1m7 75.62 → 92.58;
    640×480: 90.84 → 94.78, 78.28 → 95.40, 90.73 → 97.56, 94.58 → 97.39;
    against id's exact per-pixel perspective (`--spans 1`) 86.95 → 94.55,
    64.26 → 92.64, 66.11 → 97.47, 80.37 → 97.46. With both renderers at mip
    0 (`d_mipscale 0`) every row is unchanged — the mip-0 bake is the old one.
    Muzzle-flash frames (`--c-cmd +attack --settle 3`, as shipped): e1m1 80.64
    → 87.02, e1m2 60.66 → 89.76, e1m3 61.00 → 95.80.
  - **Goldens:** e1m1 `bb64996e` unchanged (every face in that view is at mip
    0), e1m2 `8186a64c` → `3d47c70d` (38,742 px, 15.1%), e1m3 `f41e8b59` →
    `c7e5b50e` (31,730 px, 12.4%): the distant walls, now drawn from id's
    coarser levels.
  - **Not done:** a brush model spanning several BSP leaves is split by
    `R_DrawSolidClippedSubmodelPolygons` into fragments, each with its own
    `nearzi` and so possibly its own level; the port picks one level per face.
    (Done since, by the edge renderer: "World pass: id's edge renderer" below.)
    The wasm console has no `d_mipscale`/`d_mipcap` commands yet.

- ✅ **Lightmap stepping** (oracle class 6, ±1 colormap row on 1.5–4.4% of
  pixels). The bake took a float bilinear lightmap factor at each texel's corner
  and then picked the row. id builds `blocklights` in 8.8 integers
  (`R_BuildLightMap`: each style's luxel times its `d_lightstylevalue`, plus
  `R_AddDynamicLights`' truncated `(rad - dist)*256`), inverts and clamps them
  (`(255*256 - bl) >> 2`, at least `1 << 6`), and `R_DrawSurfaceBlock8_mip0..3`
  walks each `16 >> miplevel` cell down its left and right edges in integer
  steps `(bottom - top) >> (4 - miplevel)` and along each row from the RIGHT
  edge's value by `(left - right) >> (4 - miplevel)`, indexing
  `colormap[(light & 0xFF00) + texel]`. Now exactly that
  (`LightMap::blocklights_into`, `surf::draw_surface_block`); the port's f32
  luxels are the C's sum over 256, each term exact, so `luxel * 256` is id's
  integer. The oracle agent's temporary patch of earlier tonight, made
  permanent for all four levels.
  - **Oracle,** exact% world-only 320×200, item 1 → this: as shipped 92.20 →
    97.44, 91.03 → 97.12, 96.68 → 99.16, 92.58 → 94.96 (e1m1/2/3/7);
    640×480 94.78 → 99.20, 95.40 → 98.77, 97.56 → 99.30, 97.39 → 98.77;
    against id's exact perspective (`--spans 1`, its own mips) 94.55 →
    99.94, 92.64 → 99.18, 97.47 → 99.98, 97.46 → 99.91; both at mip 0 with
    exact perspective 95.61 → 99.93, 97.40 → 99.88, 98.50 → 99.98, 98.66 →
    99.91 (640×480: 99.96 / 99.96 / 99.99 / 99.98). What is left as shipped
    is id's 8-pixel affine span segments (class 7, branch `quake/w2b`).
    Over 72 more views (4 maps × 6 yaws × 3 pitches, `--spans 1`) the mean is
    99.99%, the worst 99.83.
  - **Goldens:** e1m1 `bb64996e` → `959d0221` (9,953 px, 3.9%), e1m2
    `3d47c70d` → `0cd18471` (8,041 px, 3.1%), e1m3 `c7e5b50e` → `b63ae8b7`
    (7,555 px, 3.0%): single colormap rows on light gradients. From before
    item 1: 9,953 / 42,613 / 36,634 px.
- ✅ **Dynamic lights at the chosen level** (w1's A2 path): a dlit face is baked
  by the same `face_surf_block`, so it gets the level and the integer stepping
  with the light's `blocklights` folded in (`cache->dlight` as before).
  Muzzle-flash frames (`--c-cmd +attack --settle 3`), 320×200, base → now: as
  shipped e1m1 80.64 → 95.36, e1m2 60.66 → 96.16, e1m3 61.00 → 98.98; mip 0
  + exact 90.35 → 98.11, 96.85 → 99.46, 97.82 → 99.89. On the pixels id's
  flash changes (id's lit frame against id's frame at the same view and
  clock without the shot): e1m1 83.4% → 99.8% (14,296 px), e1m2 90.3% →
  94.9% (825 px; the other 42 are the shot's blood particles, which `view`
  does not draw), e1m3 96.1% → 99.5% (13,211 px). e1m1 keeps the harness's
  settle ≥ 3 light-style offset (oracle README).
- **Speed and memory** (items 1 and 2), native twin of `web/bench.py`, a fresh
  process per resolution, medians of two runs; base → mip levels → + stepping:
  - Texels baked per frame (`surf_texels`, the world cache plus the external
    boxes' uncached bakes): demo1 4,348 → 2,847 / 3,511 / 3,993 (320×200 /
    640×400 / 1280×800); walk_e1m1 35,185 → 3,963 / 6,983 / 11,482;
    fire_e1m1 48,660 → 16,322 / 19,594 / 23,880; walk_e1m3 68,628 → 2,902 /
    5,683 / 10,767.
  - Time in `face_surf_block` (mean ms per frame): fire_e1m1 0.45–0.46 →
    0.34–0.45 → 0.026–0.047; walk_e1m1 0.17–0.18 → 0.09–0.19 → 0.016–0.033.
    Whole step, fire_e1m1 median / p95: 320×200 1.48 / 3.79 → 0.68 / 1.35
    ms, 640×400 2.17 / 4.92 → 1.55 / 2.75, 1280×800 5.37 / 8.91 → 4.97 /
    7.40; walk_e1m3 320×200 2.80 → 1.28, 640×400 3.78 → 2.32, 1280×800 7.09
    → 5.78. demo1 moves within noise.
  - Wasm (`web/bench.py --build`, two runs each, step median / p95 at 640×400,
    median at 1280×800): walk_e1m3 3.42–3.60 / 4.60–4.86 → 2.62–2.78 /
    3.49–3.63 ms, 7.50–7.74 → 6.58–6.85 (the external boxes' uncached bakes
    0.81 → 0.08 ms); fire_e1m1 2.09–2.27 / 4.05–4.18 → 1.72–2.07 / 2.73–3.17,
    5.67–5.94 → 5.37–5.75; walk_e1m1 p95 3.41–3.59 → 2.70–2.94; demo1 within
    noise.
  - `QUAKE_DLIGHT=eye` on e1m1 at 1280×800 (w1 left it at ~14 ms): radius 350
    14.1–15.1 → 10.0–13.9 → 7.4–9.1 ms, radius 200 12.1–12.7 → 8.0–8.5 —
    the unlit frame is 8.2–8.6.
  - Surface cache resident (`surfcache_kb`, the most over a run): demo1 3,489
    KB → 2,166 / 2,985 / 3,359; walk_e1m1 1,973 → 975 / 1,367 / 1,785;
    walk_e1m3 3,350 → 1,071 / 1,757 / 1,998. (id's fixed pool is 600 KB at
    320×200 and ~3.4 MB at 1280×800, `D_SurfaceCacheForRes`.)
- **Still open:** `nearzi` is the clipped outline's, which is id's in all but
  one quirk: `R_RenderFace` re-uses `r_rightexit`/`r_leftexit` when the edge
  that should set them was cached as fully clipped by an earlier face this
  frame, so id can give a face the `1/z` of a stale point from another face —
  a finer level than its geometry (seen: face 733 at e1m2's first frame, id
  mip 0 from a point at z 96, the port mip 1; 0.7% of that frame, 0.17% of
  one of the 72 sweep views, nothing elsewhere). Reproducing it means
  running id's edge clipping and edge cache in `R_RecursiveWorldNode`'s order.
  (Closed by the edge renderer, which does exactly that: "World pass: id's edge
  renderer" below.)

## Census client/host fixes (2026-09-25, branch `quake/fix-client`)

One line per CENSUS.md finding fixed; the evidence and the C are in CENSUS.md
and the commit messages.

- ✅ **F2 single player pauses behind the menu/console** (`Host_ServerFrame`/`SV_RunClients`): `step_walk` runs no server frame while `key_dest != key_game`; `cl.time` (`w.clock`) freezes with it, host-time fades/countdowns keep going (new `Walk::host_time`); the attract demo keeps playing.
- ✅ **F1 teleporters (and spawns) turn the view** (`SV_WriteClientdataToMessage` fixangle → `svc_setangle`; `Host_Spawn_f`'s setangle): after the server frame `step_walk` copies the player's `angles` into the view through `MSG_WriteAngle`/`ReadAngle` quantisation and clears `fixangle`; every walk builder starts facing the spawned player's `angles` (SelectSpawnPoint's spot, `info_player_start2` included) instead of parsing `info_player_start`.
- ✅ **F4 an impulse pressed during the weapon cooldown is kept** (`SV_ReadClientMove` only sets a non-zero impulse; QC `ImpulseCommands` clears it after `W_WeaponFrame`'s cooldown return): `Server::physics_client` no longer zeroes `impulse` after PlayerPostThink (server.rs, a few lines + its two tests).
- ✅ **F10 runes on the status bar** (`SV_WriteClientdataToMessage`: `items | serverflags << 28`): the live HUD's `items` is `client_items(w)`, so `Sbar_DrawInventory`'s sigil cells light up.
- ✅ **F11 Tab = `+showscores`** (default.cfg `bind TAB +showscores`, `Sbar_Draw`'s `sb_showscores`): `BIND_SHOWSCORES` in the bindings table (TAB by default), `KeyMove::showscores` feeds `Hud::show_scores` live and in demo playback; the page no longer opens the menu on Tab (Esc still does).
- ✅ **F17 weapon keys by key number** (default.cfg `bind 1 "impulse 1"`..`bind 8`, `bind 0 "impulse 0"`; Key_Event works on key numbers): `"impulse N"` commands in the bindings table, bound to the digit row by default; the page sends digits through `quakeKey` (e.code `Digit*`), so Shift+digit and AZERTY select weapons; the `e.key` digit path is gone.
- ✅ **F16 damage flash, kick and pain face in god mode / with the Pentagram** (QC `T_Damage` accumulates `dmg_take`/`dmg_save` before its god/invulnerable returns; `SV_WriteClientdataToMessage` sends svc_damage and zeroes them; `V_ParseDamage`): `step_walk` reads and zeroes the three fields after the server frame instead of inferring the hit from health/armour deltas (the megahealth-rot guard is moot); the live view now has V_CalcViewRoll's directional kick and `cl.faceanimtime`'s pain face (`Hud::face_pain`, `face_p*`), demo playback too. `V_ParseDamage` lives in quake-wasm `view.rs`, shared.
- ✅ **F6 gold bonus flash** (QC `stuffcmd(other, "bf\n")` at 16 pickup/powerup sites → `svc_stufftext` → `V_BonusFlash_f`, dropped `host_frametime*100` by V_UpdatePalette): `PF_stuffcmd` queues `(entity, text)` (builtins.rs; server.rs installs it at #21); the live walk runs `bf` for its player, demo playback runs recorded `svc_stufftext`; the bonus cshift sits between damage and powerup. Accepted gap: Cbuf_Execute would run it one host frame later.
- ✅ **F18 new-weapon icon flash** (`CL_ParseClientdata` stamps `cl.item_gettime[j]` for newly set bits; `Sbar_DrawInventory`'s `flashon` picks `inva1..5` for a second): `Hud::item_gettime`, stamped by the live walk and the demo (zeroed with the level, so carried weapons flash at level start); keys/powerups/runes never visibly flash in the C (their `flashon` is 0).
- ✅ **F13 demo trails** (`CL_RelinkEntities` → `R_RocketTrail` for model flags, live or demo): `EntSnapshot::num` + `DemoPlay::trail_org`; step_demo trails like step_walk (no EF_ROCKET dlight: demo dlights are still the open gap).
- ✅ **F14 demo skins** (`CL_ParseUpdate` U_SKIN / baseline skin, `CL_ParseStatic`): demo.rs keeps the skin, `EntSnapshot::skin` → `ModelInstance::skinnum`; yellow armour is yellow in demo2/demo3.
- ✅ **F15 attract loop cycles demo1 → demo2 → demo3** (quake.rc `startdemos`, `svc_disconnect` → `Host_EndGame` → `CL_NextDemo`): `DEMOS` + `build_demo_n`; the dispatcher starts the next demo when one has shown its last frame (same-demo wrap kept as the fallback).
- ✅ **L2 cshift percents are ints** (client.h `cshift_t.percent` is `int`: V_ParseDamage's `+=` and V_UpdatePalette's drops truncate every frame — 150 → 147 at 72 fps): view.rs `cshift_add`/`cshift_drop` for the damage and bonus shifts, live and demo. Closes perf-b's first "seen, not changed" note above.
- ✅ **L24 contents tint defaults to water** (`V_SetContentsColor`'s `default:` — sky included): `content_cshift` returns none only for empty/solid. Closes perf-b's second note.
- ✅ **L1 live punchangle in whole degrees** (`MSG_WriteChar(punchangle[i])`): `client_punchangle` truncates to signed chars for the camera and the gun.
- ✅ **L8 particles drawn before they move** (`R_DrawParticles`: free `die < cl.time`, draw, then move/ramp): `ParticleSystem::retire` + `integrate`, called around the draw list in step_walk/step_demo.
- ✅ **L9 dlights drawn before they decay** (Host_Frame: `CL_DecayLights` after `SCR_UpdateScreen`; R_PushDlights skips `die < cl.time`): step_walk renders `pushed_dlights` and decays after the 3-D view.
- ✅ **L11 notify lines** (`Con_Print` 38-column word-wrapped lines stamped at their start; `Con_DrawNotify` last 4 from `v = 0`): quake-wasm `ConNotify`, live + demo; `draw_notify` from y = 0. ~~Still open: prints never reach the drop-down console's scrollback~~ (✅ `quake/polish`, c71989f).
- ✅ **L12 default.cfg binds** — ENTER `+jump`, MOUSE2 `+forward`, `\` and MOUSE3 `+mlook`, INS `+klook` seeded; the page sends MOUSE2/MOUSE3 while locked. The F-key commands are ✅ too (fkeys: F1–F4, F6, F9, F10, F12, "Game and client" above). Not done: `t` messagemode, the `zoom_in` alias.
- ✅ **L14 New Game asks first while a game runs** (`M_SinglePlayer_Key` → `SCR_ModalMessage`, y/n/Escape, faded screen + `SCR_DrawNotifyString`): `Menu::new_game_confirm`, raised when the host-set `server_active`; y (`menu_quit_yes`) starts the game.

## Census fixes, server side (2026-09-25, branch `quake/fix-server`)

One line per fix; evidence and tests in the commit, the rows in `CENSUS.md`.

- ✅ **Chthon's electricity** — boss.qc `lightning_fire` writes TE_LIGHTNING3 to MSG_ALL (`sv.reliable_datagram`), which `CL_ParseServerMessage` parses like the datagram; the port decoded temp entities only from MSG_BROADCAST, so Chthon died with no bolt drawn. `server/msg.rs` now runs one svc parser per buffer (datagram, reliable), each reading temp entities and commands alike (fix first found by the `chthon` agent, salvaged `00bf4a7`). Test `cl_tent::tests::chthon_lightning_reaches_the_client_and_kills_him_on_e1m7`.
- ✅ **F3 e1m8 low gravity** — `sv_gravity` is a live cvar (`server/host.rs`): world.qc `worldspawn`'s `cvar_set("sv_gravity", "100"|"800")` lands, `cvar("sv_gravity")` reads it, `SV_AddGravity`, `SV_Physics_Step`'s landing-sound threshold and the live `R_DrawParticles` gravity use it; a fresh server starts at 800 (since `quake/polish2` the cvar outlives the map, as id's: the next worldspawn sets it). Test `census_e1m8_has_low_gravity` (+ `sv_gravity_cvar_drives_add_gravity`). The demo path keeps 800 (no server runs during playback).
- ✅ **F5 b_*.bsp item boxes** — two causes. (1) `WorldModel::precache_model` spread the external model's bounds a second time (`Bsp::parse` already applies `Mod_LoadSubmodels`' pixel): the explosive box was 34 wide (hull2) and floated 2 units. (2) `world::clip_box` treated touching boxes as overlapping; the C box hull (`SV_InitBoxHull`) is half-open, `mins <= p < maxs`, so e1m1's 10-health box beside a grunt and e1m6's 25-health box beside an ogre no longer "fall out of the level" in PlaceItem's droptofloor. Oracle edict diff: both boxes present, explosive box at id's z -207.969; nothing else moved on any map. Tests `census_bmodel_item_bounds_are_spread_once`, `clip_box_is_half_open_like_the_c_box_hull`.
- ✅ **F8 force_retouch** — `SV_Physics` relinks every live edict with `SV_LinkEdict(ent, true)` while the QC global is set (`spawn_tdeath`, `teleport_use` set 2) and decrements it after the loop; now both `run_frame` and `client_frame` do (world skipped, SOLID_NOT relinked without touching). e1m6 doors *31/*76 and e1m8 *6 open at the level start as in id (oracle: open by t 4.7; at t 1.7 the port is 0.2 s ahead, the signon-timing gap below). Tests `census_force_retouch_opens_e1m6_start_door`, `force_retouch_relinks_stationary_edicts_for_two_frames`. Open: the port connects the player at sv.time 1.2, id's signon at ~1.4.
- ✅ **F9 looping mover sounds** — `queue_sounds` carries each sample's `cue ` loop window (`GetWavinfo`) through `poll_sound` (`sound_loop_start` -1 = one-shot) and no longer drops `misc/null.wav`; the page loops such a source (`SND_PaintChannels`) until a later sound on its (entity, channel) overrides it, re-spatializes it every frame from its fixed origin like the C channel, lets an inaudible sound still end its key's loop (`S_StartSound` picks the channel before the audibility test), guards the async first decode against an override that landed meanwhile, and stops every dynamic source on a level change (`S_StopAllSounds`). Tests `queue_sounds_carries_the_cue_loop_so_movers_hum_until_their_stop_sound`, `web/verify_loops.py` (demo1's first door hum loops, then its stop sound ends it).
- ✅ **L4 `time` before PlayerPreThink** — `SV_Physics_Client` sets `pr_global_struct->time = sv.time` before `PlayerPreThink` (and `SV_Physics` before `StartFrame`); the port left the thinktime of the last think. Test `player_prethink_sees_sv_time_not_a_preceding_thinktime`.
- ✅ **L20 StartFrame in every frame** — `run_frame` (the spawn settle frames) now runs `StartFrame` at `time = sv.time` like every `SV_Physics`. Test `run_frame_starts_with_startframe_like_sv_physics`.
- ✅ **L25 (half) edict loop bound** — `SV_Physics` re-reads `sv.num_edicts` every iteration, so an edict a think spawns (a missile, a gib) gets physics on its spawn frame; `run_frame`/`client_frame` fixed the bound at frame start. Test `an_edict_spawned_by_a_think_moves_on_its_spawn_frame`. Not done: player-first order (the C reserves edict 1 for the client; the port allocates the player after the map, which also offsets every edict number by one against id's) — a numbering change across spawn, connect and savegames for a sub-frame ordering effect.
- ✅ **L7 `restart` keeps only the level-entry runes** — `Server` keeps `svs.serverflags` (set with the QC global before spawn); `try_restart` respawns with it instead of the live global, as `Host_Restart_f` -> `SV_SpawnServer` does (only `SV_SaveSpawnparms`, at a changelevel, reads the global back). Test `restart_respawns_with_the_level_entry_serverflags`. As in id, a loaded save starts from a fresh `svs.serverflags` (0), since `Host_Loadgame_f` never sets it.
- ✅ **L18 world edict** — `set_map_name` (the port's `SV_SpawnServer` world setup) now also sets edict 0's `modelindex` 1, `SOLID_BSP`, `MOVETYPE_PUSH`, replacing the documented deviation; collision and `SV_PushMove` already skip edict 0 as the C does, and its pusher pass is inert (`nextthink` 0). The oracle edict diff loses its `worldspawn movetype 7/0 solid 4/0` line; the census tool's pusher list skips the world. Test `set_map_name_sets_up_the_world_edict_like_sv_spawnserver`.
- ✅ **L5 client think order** — `Host_ServerFrame` runs `SV_RunClients` (`SV_ReadClientMove` + `SV_ClientThink`) before `SV_Physics`; `client_frame` now does too, so `PlayerPreThink`'s `PlayerJump`/`WaterMove` act on the accelerated velocity (it also gives a dead player `DropPunchAngle`, which only ran for WALK/FLY/NOCLIP). A standing forward jump on e1m1 now follows id's oracle frame for frame (10 u forward per frame; it made 3). Test `census_client_think_runs_before_player_prethink`.
- ✅ **L6 ED_Alloc / ED_Free** — `Vm::spawn` reuses a free slot only if it was freed in the first two seconds of server time or more than 0.5 s ago (`freetime`), so a missile spawned the frame another is removed never inherits its slot (no stray trail); `Vm::free_edict` clears only `ED_Free`'s fields (model, takedamage, modelindex, colormap, skin, frame, origin, angles, solid; nextthink -1) and keeps the rest, as id does. Tests `ed_alloc_waits_half_a_second_before_reusing_a_freed_slot`, `ed_free_clears_only_the_fields_the_c_clears`.
- ✅ **F7 the player's name** — `connect_client_inner` sets up the client edict as `Host_Spawn_f` does before `ClientConnect`: `netname` "player" (cl_name), `team` 1 ((cl_color & 15) + 1), `colormap` = its edict number. Obituaries read "player was shot by a Grunt". Test `census_player_netname_is_player`.

## Review fixes (2026-09-25, branch `quake/polish`)

Findings of the adversarial review of the overnight merge, one line each; the
C followed and the test are in the commit message.

- ✅ **Loading a save kept no options** (MED) — `load_game` rebuilt the Menu
  (`Menu::new()`), so Screen size, Brightness, Always Run, every rebind and the
  slot listings snapped to defaults. `Host_Loadgame_f` never touches a cvar or
  `keybindings[]`; now the same `reset_nav()` as New Game. Test
  `load_keeps_every_option_and_binding` (the `viewsize 60`, `save t`, `load t`
  repro).
- ✅ **Debug-build overflow on sliver alias triangles** (LOW) — after
  `R_AliasClipTriangle` a sliver (d_xdenom of a few units under a long edge)
  gets 1/z and light steps far out of `int` range. id's `(int)` gives
  0x80000000 there (Rust's `as` saturated to 0x7FFFFFFF for positive ones) and
  its `int` sums wrap; a debug build panicked in `scan_left_edge` (`d_zi +=`,
  `d_light +=`). `polyse.rs` now converts with `c_ftoi` and wraps every step
  sum and the subdivision midpoints like the C. Tests
  `sliver_triangles_wrap_like_the_c_ints`, `float_to_int_is_x86s_not_rusts_saturation`;
  the review's stress harness (4000 random gun/entity renders with overflow
  checks) panicked at iteration 3068 before, none now. Goldens unchanged.
- ✅ **`intsintable` is not wrapped** (LOW) — `D_WarpScreen` reads
  `turb = intsintable + phase` at `turb[u]`/`turb[v]` over the whole screen, and
  `R_InitTurb`'s `3.14159` makes the table non-periodic: at i = 128, 256, ...
  the entry is 2 where a wrapped cycle gives 3. `apply_warp` indexed `& 127`;
  now it reads the unwrapped table (`intsintable`). Tests
  `intsintable_is_r_initturbs_unwrapped_table`,
  `warp_reads_intsintable_past_the_first_cycle`. (Turbulent8's `sintable` was
  already unwrapped, fid1.)
- ✅ **Underwater view at the warp buffer's resolution** — with the eye in
  water/slime/lava, id's `R_SetupFrame` renders the view into `r_warpbuffer`
  (at most `WARP_WIDTH` x `WARP_HEIGHT`, 320x200: the mode scaled to 320 wide,
  capped at 200 high, `R_SetVrect` on that with `sb_lines * h/vid.height`)
  and `D_WarpScreen` stretches it over `scr_vrect` (`wratio`/`hratio`, the
  C's float row/column tables). The port rendered and warped at full
  resolution. Now `screen::warp_vrect` + `apply_warp(view, out_w, out_h,
  clock)` (a new signature, so `render/mod.rs` is untouched), live, demo and
  `quaketool view` (which now warps an underwater eye like id's
  `R_RenderView`). Oracle, e1m1's pool (`--view=750,898,-332,0,90,0 --time
  1.6`), exact%: 320x200 97.58; 640x400 48.22 -> 97.59; 960x600 44.14 ->
  97.60; 1280x800 42.65 -> 97.63 (before = the full-resolution warp; id's
  `--spans 8` against the port's then exact perspective). *Re-measured on
  `quake/polish2`* after `compare.py` stopped taking the warp buffer's
  `r_refdef.vrect` for the screen's (it had scored this view 6.6% at 960x600
  since `quake/w2b`): against `--spans 16` 99.89 / 99.89 / 99.90 / 99.89,
  at the page's aspect (`--aspect 0.8333333`) 99.93 at all four; against
  `--spans 8` 98.53 / 98.57 / 98.54 / 98.58. Cheaper
  too: an underwater frame renders 320x152 at every preset. At every 16:10
  mode the C's pixel aspect for the warp buffer equals `vid.aspect`, so the
  square-pixel projection is exact; for a mode taller than 16:10 id squeezes
  320x200 through that aspect and the port narrows the buffer instead
  (`vid.width*200/vid.height` wide: same picture, e.g. 266 columns at 4:3).
  Whoever lands the pixel-aspect projection: the warp buffer's aspect is
  `vid.aspect * (h/w) * (vid.width/vid.height)` (R_ViewChanged). Tests
  `warp_vrect_is_r_setupframes_warp_buffer_view`,
  `warp_stretches_the_warp_buffer_over_the_screen_view`,
  `underwater_view_renders_into_the_warp_buffer`. Goldens unchanged.
- ✅ **The client clock is `cl.time`** — the live walk's `w.clock` started at
  0 and counted `dt`, so the sky, liquids, underwater warp, texture/alias
  animation and `R_AnimateLight`'s `(int)(cl.time*10)` ran about 1.2 s behind
  id's (and reset to 0 at every changelevel, restart and load). On a local
  server `CL_LerpPoint` snaps `cl.time` to the message time, `sv.time` after
  the frame's physics: `w.clock` is now `server.time()` after every server
  frame and at every walk build (spawn, changelevel, restart, load = the
  save's time), and still stops behind the menu. Particles, dlights, beams,
  the bob, rotating pickups and the intermission sway run on it too, as they
  do on `cl.time` in the C. Tests `client_clock_is_the_server_clock`, and the
  load in `save_load_round_trips_the_world_digest`. Demo playback already ran
  on the recorded `cl.time`.
- ✅ **Damage flash percent is an `int`** (review item) — already fixed on
  main by census L2 (`3ec96f4`, `cshift_add`/`cshift_drop`); the demo test
  asserts id's 22 (30 - 7.5 truncated), the ramps take the int percent.
  Nothing further to change.
- ✅ **`quaketool playtest` framing** — the POV shot drew a full-screen view
  (viewsize 120's framing) with the viewsize-100 gun fudge and pasted the
  48-line bar over it, so the gun sat under the bar. Now it is the game's
  default screen: `calc_refdef` at viewsize 100, the view above the bar,
  `compose_view` with the backtile, the gun offset for the same viewsize.
  Test `playtest_frames_the_view_for_the_guns_viewsize`.
- ✅ **Stale comments** — `render_scene_ext`'s doc (the gun "uses its own
  depth buffer", "a plain `[f32; 256]`" sine table, "walls, alias models and
  the viewmodel ignore `time`"), `Viewmodel`'s ("no world origin", "view
  space", frame "clamped"), and the `V_CalcBlend` mentions in `cl_walk.rs`,
  `cl_demo.rs` and the README (the shifts are the software `V_UpdatePalette`
  ramps). Comments only (`render/mod.rs` touched for its doc comment alone).
- ✅ **Census L11: prints reach the console scrollback** — `Con_Print`
  writes the console's text buffer, and the notify lines are its last
  lines; the port showed `svc_print` text only in the notify overlay. Now
  `quake_rs::console::ConCursor` is Con_Print's layout (`con_x`, the word
  wrap at `con_linewidth` 38 — a word longer than a line runs on until its
  remainder fits — `\n`, `\r`), shared by the notify lines and
  `Console::print`; `println` is `Con_Printf("%s\n")` through it, so
  console output wraps too and continues a line a print left open. The
  dispatcher hands each frame's printed text (live or demo) to the console.
  Tests `console_print_is_con_print`, `game_prints_reach_the_console_scrollback`
  (e1m1's shells pickup). ~~Still open: console command output does not reach
  the notify lines~~ — moot: `Con_ToggleConsole_f` zeroes `con_times`, so what
  was printed while the console was down never shows as a notify line in the
  C either (branch `quake/fid2d`, below).

## The 2-D layer against id's composited screen (2026-09-25, branch `quake/fid2d`)

`oracle/screen2d.py` (see `oracle/README.md`, "The 2-D layer") plays the same
scenario through id's WinQuake and the port's live App with the 3-D view one
flat colour on both sides, and diffs the screens: 63 shots x 3 modes. Before
the branch the shots' 2-D pixels matched 37-100% at 320x200 and 0-100% (mostly
under 55%) at 640x400 and 960x600; after, 57 of 63 shots in each mode are
exact and the other six are four explained residues (95.5-99.9%). Goldens unchanged
(`959d0221`/`0cd18471`/`b63ae8b7`, 3-D only). (Two of the four residues, the
new-weapon flash and Single Player after Load, were closed on `quake/polish2`; the
other two, the conback's version string and the Video mode list, plus the Options
page's Web extras row, are what `oracle/README.md` lists as left.)

- ✅ **Ammo counts 4 px left** (`8b960c2`) — `Sbar_DrawCharacter` draws at
  `x + ((vid.width - 320)>>1) + 4` (sbar.c:293); the port's inventory counts
  used the bare x. Test: the shells count starts at x = 8 - 2 + 4.
- ✅ **"quake-rs" under the Main / Single Player menus** (`77a0664`) — not in
  `M_Main_Draw` (menu.c:291). Removed.
- ✅ **A note line under Multiplayer** (`f20d28b`) — `M_MultiPlayer_Draw`
  (menu.c:635) prints only "No Communications Available". Removed.
- ✅ **Centerprints one row low** (`4a92962`) — `y = vid.height*0.35`
  (screen.c:142, and `SCR_DrawNotifyString`'s) is `(int)` of a product the x87
  computes in extended precision, where the double 0.35 leaves it a hair under
  70: row 69 (the oracle's x87 build draws 69, its SSE build 70). The port took
  70. `center_string_top` is the exact integer floor; the plain centerprint now
  goes through the finale's `SCR_DrawCenterString` port (its 40-column scan).
- ✅ **The 2-D layer blown up in every larger mode** (`cdfd57f`) — a default
  departure: WinQuake draws the 2-D layer 1:1 in every video mode — the bar 320
  wide at the bottom centre with backtile either side (`Sbar_Draw`,
  sbar.c:938), menus across the top centre (`M_DrawPic`'s
  `(vid.width-320)>>1`), the intermission at fixed screen coordinates, a 48-row
  bar the view clears, `(int)(48*h/vid.height)` rows of the underwater warp
  buffer, a console `vid.width/8-2` characters wide. The port scaled id's
  320x200 screen to fill the framebuffer. `draw::screen_2d` is now the screen
  id's code lays out on: the framebuffer at scale 1 by default, or with the new
  **"scaled 2-D" extra** (`draw::set_scaled_2d`, wasm export `set_scaled_2d(1)`)
  the old 320x200 blow-up. 640x400: 1-55% of 2-D pixels -> 99-100%. ~~**Needs
  wiring** into the page's extras (not done here: the Extras menu and the
  page's persistence are another branch's); until then the browser's default
  960x600 shows id's small bar and menus.~~ Wired since (`dafa2c7`): "Scaled 2-D
  layer" on Options > Web extras, `wasm_scaled2d`, persisted; and the page's canvas
  grew to the largest 4:3 box the window fits (`quake/polish3`), so the 1:1 bar is
  not tiny.
- ✅ **The console** (`d27eebb`) — a fixed 60% panel that snapped on and off,
  the conback's top rows squeezed into it, its own text layout. Now
  `SCR_SetUpToDrawConsole` (screen.c:458: `scr_con_current` slides 300 rows a
  second to `vid.height/2` and back), `Draw_ConsoleBackground` (draw.c:539: the
  conback's BOTTOM rows, the version stamped in with `0x60 + texel` — the DOS
  build's "1.09"), `Con_DrawConsole` (console.c:580: `(lines-16)>>3` rows from
  `lines-16-rows*8`, x = `(col+1)*8`) and `Con_DrawInput` (the prompt line at
  `lines-16`, prestepped past `con_linewidth`); closing clears the typing
  (`Con_ToggleConsole_f`). `Con_CheckResize`: the console and notify text are
  laid out `con_linewidth = (vid.width>>3)-2` wide (was 38 in every mode).
  320x200: 37% -> 99.1% (the rest: the oracle's Linux version string).
- ✅ **The Quit prompt** (`d6c0fe1`) — an invented black box asking "Are you sure
  you want to quit?". Now `M_Quit_Draw` (menu.c:1674, the non-Windows builds,
  i.e. DOS Quake's): the menu it rose over redrawn without a second fade
  (`wasInMenus`, `m_recursiveDraw`), `M_DrawTextBox (56, 76, 24, 4)` from the
  `gfx/box_*.lmp` pieces (menu.c:181), one of the eight `quitMessage` taunts
  (`msgNumber = rand()&7`) in `M_Print`'s bronze; No/Escape return to that
  menu, or to the game. 66% -> 100%. (WinQuake on Windows showed a credits box.)
- ✅ **The console lingered after `map` / `load`** (`2b024a2`) —
  `SCR_BeginLoadingPlaque` zeroes `scr_con_current`: it goes at once.
- ✅ **Notify lines survived a console toggle** (`0cdcb1a`) —
  `Con_ToggleConsole_f` zeroes `con_times`. Test
  `toggling_the_console_clears_the_notify_lines`.

Found, not fixed (outside the 2-D drawing, or another branch's file):
- ✅ (`quake/polish2`, "Second review fixes") **Menu cursors are not remembered** — id keeps one per menu
  (`m_main_cursor`, `m_singleplayer_cursor`, `options_cursor`, `load_cursor`
  shared by Load and Save, ...): Escape from Options lands on "Options", the
  port's single cursor on "Single Player". `menu.rs` behaviour, left for after
  the Extras-menu work (screen2d `menu_sp.sp_again`, 206 px).
- ✅ (`quake/polish2`, "Second review fixes") **`sv.time` adds up in f32** (`sv_phys.rs`: `gset_float("time", start_time +
  dt)`); id's `sv.time` is a double copied into the QuakeC float each frame.
  After 60 frames of 0.1 s the port reads 7.2999954, so the new-weapon flash
  (`(int)((cl.time - item_gettime)*10)`) is a frame behind (screen2d
  `flash.rl_new`), and the error grows with the session (at an hour, f32 steps
  are ~0.24 ms of a 13.9 ms frame).
- **Default key binds are WASD** (`keys.rs`: w/s/a/d/c over `default.cfg`'s
  a = +lookup, d = +moveup) — a default departure besides Always Run.
- ✅ (`quake/timedemo`, "Demo commands, timedemo, pause") **No pause** —
  `pause` (default.cfg binds PAUSE) and `SCR_DrawPause`'s plaque;
  **no loading plaque** (`SCR_DrawLoading`: not needed, see that section).
- **`give` is not `Host_Give_f`** — it clamps, has an `a` (armour) case,
  defaults a missing amount to full, and selects the weapon it gives; id sets
  the field to `atoi(argv[2])` (0 when missing) and only ORs the weapon bit.

## Projection and spans (2026-09-25, branch `quake/w2b`)

**On the merged base** (`quake/overnight` `bbfc6bc`: the mip levels, the census and review fixes), all
four items: goldens e1m1 `959d0221` → `4807aaa1` (4983 px, 1.95%), e1m2
`0cd18471` → `8ce25660` (5808 px, 2.27%), e1m3 `b63ae8b7` → `3531e9cd` (4077 px,
1.59%) — all from the 16-pixel spans (the aspect and the sky centre leave the
square-pixel full-screen scene alone). Oracle world, 320x200, against id's x86
spans (`--spans 16`): 92.84 / 94.36 / 96.56 / 87.20 → **99.96 / 99.21 / 99.98 /
99.91**; at the page's aspect (`--aspect 0.8333333`) 100.00 / 98.73 / 100.00 /
100.00 and 100.00 at 640x400; against id's portable C (`--spans 8`, the
default) 97.44 / 97.12 / 99.16 / 94.96 → 94.57 / 96.09 / 97.27 / 91.72 (8 against
16 now). The exact extra reproduces the merged base byte for byte. The
paragraphs below give each item's own before/after as measured on the old
base.

Measured with `oracle/compare.py` (exact-palette-index match %, 320x200,
world, e1m1/e1m2/e1m3/e1m7 unless stated).

- ✅ **Pixel aspect** (the second half of H6, PERF_PLAN A4). Every render
  preset is 16:10 and the page always shows the canvas at 4:3, as DOS Quake's
  320x200 filled a 4:3 monitor; the port projected square pixels, so the whole
  3-D view was shown 1.2x too tall. id folds the display into the projection:
  `vid.aspect = (h/w)*(320/240)` (vid_win.c) is `R_ViewChanged`'s
  `pixelAspect`, `yscale = xscale*pixelAspect`, and the frustum, the alias
  scales, sprites and particles all take it. Now `render::RenderOptions::
  pixel_aspect`, one `Projection` for every pass (world, submodels, external
  boxes, alias models and the gun, sprites, particles, the frustum); the wasm
  shell passes `vid_aspect(w, h, 4/3)` — 0.8333 at every preset. The sky
  keeps its screen-pixel mapping (`D_Sky_uv_To_st` has no aspect), and a
  particle stays a pixel square (`d_y_aspect_shift` is 0 below 1.4).
  - **Oracle** (`--aspect 0.8333333` now reaches both renderers; id at mip 0
    and exact perspective): 31.35 / 26.90 / 20.23 / 14.23 (the port square)
    → 95.89 / 97.60 / 98.50 / 98.79, against 95.61 / 97.40 / 98.50 / 98.66
    for both square. Entity pixels 100 / 100 / 98.8 / 100; the e1m2 altar
    view (id as shipped) 100% of 684 entity pixels; with the gun
    (`--viewmodel --settle 3`) 94.44 / 97.21 / 98.62.
  - **Goldens unchanged** (`quaketool scene` writes a square-pixel PPM:
    aspect 1). At aspect 1 every pass is bit-identical to before.
  - Page screenshots before/after at 320x200 and 960x600 (e1m1 spawn): the
    rivet grid on the walls, square in the texture, was 1.17:1 tall and is
    1:1; the view shows 1.2x more vertically, and the gun is DOS Quake's.
- ✅ **16-pixel perspective spans** (oracle class 7; the LOW "affine span
  subdivision"). The shipped x86 WinQuake drew the surface cache with
  `D_DrawSpans16` (`d_draw16.s`, `d_subdiv16` 1): exact perspective every 16
  pixels, affine in between; the port divided at every pixel. Now
  `raster.rs`'s `span16_cached` is the asm's integer algorithm: the 16.16
  coordinates exact at a span's first pixel (clamped to `[0, bbextents]`) and
  at each full segment's end (clamped to `[4096, bbextents]`), stepped by
  `(snext - s)/16` with the 20 fractional bits the asm carries; the last
  segment lands on the span's last pixel with `reciprocal_table_16`'s
  `floor(ds * R[n] / 2^31)`. Liquids follow `Turbulent8` (C in both builds):
  16-pixel segments, `>> 4` steps, the C division on the last one, the
  `(CYCLE << 16) - 1` mask per segment. And the spans are id's: `R_ScanEdges`
  cuts a surface's row at every nearer surface's edge, so the 16-pixel grid
  restarts where a surface becomes visible; the port draws front to back
  against its z-buffer, so a span is a run of pixels passing the z test. Sky
  stays on its 32-pixel `D_DrawSkyScans8` runs, found the same way.
  - **Oracle, id at mip 0** (the mip class removed), `--spans 16` on id's
    side: 88.74 / 89.99 / 93.57 / 85.62 → **95.61 / 97.43 / 98.51 / 98.68**,
    the same as exact against exact (95.61 / 97.40 / 98.50 / 98.66); 640x480
    93.69 / 93.85 / 96.57 / 95.01 → 95.93 / 97.60 / 98.50 / 98.75. Without the
    z-test runs (the 16-pixel grid anchored at the polygon's left edge) e1m1 /
    e1m2 read 94.40 / 95.20: the remainder was stripes on partly hidden walls.
    Liquid pixels of an oblique teleporter (e1m1, `--view=1260,1050,-368,0,60,0
    --time 3.25`): 92.15% → 100.00%. Floors and pools, whose 1/z is constant
    along a row, were already exact.
  - **Id as shipped on x86** (`--spans 16`, id's own mips): 80.37 / 63.78 /
    65.41 / 67.94 → 86.95 / 64.29 / 66.14 / 80.35; against id's portable C
    (`--spans 8`, the oracle's default) it falls, as it must: 84.74 / 64.06 /
    65.88 / 75.62 → 82.04 / 63.79 / 65.56 / 72.23.
  - **The oracle's `--spans 16`** was a C re-creation with `D_DrawSpans8`'s
    integer steps (`>> 4`, clamp 16, division); it now has the asm's (the
    exact 1/16 steps, clamp 4096, `reciprocal_table_16`). The two differ on 0 /
    6 / 40 / 40 pixels of the four standard frames; the port with either
    arithmetic matches id within ±0.05 points, the size of id's float noise.
  - **Goldens** (640x400): e1m1 `bb64996e` → `2023d7d9` (5025 px, 1.96%),
    e1m2 `8186a64c` → `1ac070d0` (7612 px, 2.97%), e1m3 `f41e8b59` →
    `cc121e29` (5218 px, 2.04%) — texel steps inside 16-pixel segments.
  - **Brush entities before the world.** id's bmodel faces are in the
    world's edge list, so an item box or a door cuts the spans of the wall
    behind it; the port now draws brush entities first, so they cut the
    world's z-test runs too. e1m3's standard entity frame (the shells box):
    99.94 → 99.98% against `--spans 16` (the wall right of the box had
    16-pixel stripes); 61 views facing doors, plats and buttons on e1m1-e1m3
    unchanged; the exact extra unchanged. Goldens (on the merged base with
    the mip levels): e1m1 `74522852` → `4807aaa1` (288 px), e1m2 `8ce25660`
    unchanged, e1m3 `ce0f5c89` → `3531e9cd` (13 px).
  - **Speed** (on the merged base): wasm world −16 to −23% (demo1 at 1280x800
    4.11 → 3.30 ms, walk_e1m3 3.35 → 2.73), step −5 to −13%; native world
    +30-40%. PERF_PLAN §6 has the table.
  - The old per-pixel perspective stays as an opt-in extra
    (`RenderOptions::exact_perspective`, `quaketool view --exactpersp 1`,
    `compare.py --exactpersp`): byte-identical to before (the oracle's exact
    rows are unchanged). The uncached per-pixel wall path (faces over the
    surface-cache size cap, or no colormap — never in id's maps) stays exact.
- ✅ **`wasm_exactpersp 0|1`** (an extra, not id; default 0). The browser
  reaches the exact-perspective renderer option through a console variable.
  All of the port's opt-in extras live in one place, `quake-wasm/src/
  extras.rs`: a table of `wasm_*` cvars that behave like id's (`wasm_x` prints
  `"wasm_x" is "0"`, `wasm_x 1` sets it, `Q_atof` semantics), process state like
  id's cvars (not saved), read by `vid::render_options` each frame; `help`
  lists them. An extras menu can drive the same table. Always Run, the one
  default departure, stays with the menu options (it is id's own setting).
  (Since the merge with `quake/extras`: the values live in the menu, on
  Options > Web extras, and the page persists them; `extras.rs` keeps the
  cvar table and hands the renderer each frame's copy. See "Web extras".)
- ✅ **Sky centre below viewsize 120** (the open item of oracle class 4).
  `D_Sky_uv_To_st` centres the sky on the SCREEN (`u - (vid.width>>1)`,
  `(vid.height>>1) - v` in screen pixels); the port centred it on the view
  rectangle — 24 rows off at the default viewsize 100, 12 at 110, both axes
  inside a border. `RenderOptions::screen` (the vrect's corner on the
  `vid_w x vid_h` screen) now gives `SkyView` the screen's centre in the
  view's pixels; the page passes it every frame. The oracle compares views
  below 120 now (`compare.py --viewsize N` hands id's vrect to `quaketool
  view --vrect` and crops id's frame to it). e1m2 at `--spans 16`: the sky
  region matches 75.1 / 46.4 / 43.4% at viewsize 100 / 110 / 70 before, 100%
  after; the whole view 98.90 → 99.97, 97.56 → 99.56, 95.98 → 99.03 (what is
  left at 110 and 70, and at 120 too — 99.21 — is a ceiling face at the top
  right (a face at a finer mip in id than its geometry gives, oracle class
  1's open note) and one floor edge, the same with exact perspective on the merged
  base). Goldens unchanged (the scene view is the screen).
- **Underwater** (with `quake/polish`'s warp buffer): an underwater view is
  rendered at `warp_vrect`'s size with the screen's pixel aspect (for a
  16:10 mode id's `vid.aspect*(h/w)*(vid.width/vid.height)` is `vid.aspect`)
  and the sky placed as id places it — `D_Sky_uv_To_st` measures the warp
  buffer's pixels from the SCREEN's centre, so above 320x200 an underwater
  view of the sky is off-centre in WinQuake too, and here. Open: for modes
  taller than 16:10 (not a preset) `warp_vrect` narrows the buffer because
  the port had square pixels; with `pixel_aspect` id's 320-wide buffer and
  its aspect are now possible (not done).

## Web extras: the opt-in departures (2026-09-25, branch `quake/extras`)

The rule: faithful by default, Always Run the only default departure; other
departures live behind an explicit opt-in. Their home is **Options > Web
extras**, a 14th Options row in the slot id's `_WIN32` build gives its own
14th row ("Use Mouse", y=136). It opens a page of Options drawn in
`M_Options_Draw`'s idiom (qplaque + OPTIONS title, a white "Web extras: not
in id's Quake" header, `M_Print` labels, on/off at x=220, the 4 Hz cursor,
menu1/2/3, Esc back to the same Options row) with help lines for the
highlighted row. Each extra is also a console variable. One table lists them
(`render::WEB_EXTRAS`: the value, its `wasm_*` name, its row label and help);
one place holds their values (the menu's `Extras`, what the page draws and
persists); `quake-wasm/src/extras.rs` (from `quake/w2b`) is their console
side and hands the renderer each frame's copy. None is a default.cfg cvar,
so Reset to defaults and re-boots keep them; the page persists them in
localStorage. With every extra off the port is unchanged (goldens, all seven
earlier verify scripts).

| extra | switch | what | status |
|---|---|---|---|
| Uncapped framerate | `wasm_uncapped 0\|1` | `Host_FilterTime` without its 72 fps gate (same [0.001, 0.1] clamps): a host frame per display refresh (120/144 Hz run 120/144 fps). The gate itself is unchanged. | departure, opt-in via Web extras, default off |
| Show FPS | `wasm_showfps 0\|1` | QuakeWorld's `SCR_DrawFPS`: `"%3d FPS"` in white conchars at `vid.width - len*8 - 8`, `vid.height - sb_lines - 8`, not on intermission screens. The rate is presented frames over a window of at least 1 s of `realtime` (QW shows the raw count; count/window reads a steady 60 instead of 60/61). | departure, opt-in via Web extras, default off |
| Exact perspective | `wasm_exactpersp 0\|1` | exact perspective at every pixel of the textured walls and liquids instead of id's 16-pixel spans (`RenderOptions::exact_perspective`, `quake/w2b`'s). | departure, opt-in via Web extras, default off |
| Scaled 2-D layer | `wasm_scaled2d 0\|1` | the status bar, menus, console and text blown up from a 320x200 screen, the port's old layout, instead of WinQuake's 1:1 2-D layer (`draw::set_scaled_2d`, `quake/fid2d`'s). Added by the chair after this branch (`dafa2c7`, extras bit 8). | departure, opt-in via Web extras, default off |

Faithful, same branch:
- ✅ **viewsize persists across reloads** — id's `scr_viewsize` is archived
  (config.cfg); since Screen size stopped being the resolution it reset on
  every reload. The page stores it like the resolution (`set_viewsize`).
- ✅ **Esc ignores autorepeat** — `Key_Event` ignores repeats of every key
  but backspace and pause; the page's Esc toggled the menu on each repeat.
- ✅ **Esc in fullscreen** — the page locks Escape with the Keyboard Lock API
  while fullscreen (Chromium), so a tapped Esc toggles the menu like id's
  `togglemenu` with the mouse still captured, and a held Esc leaves
  fullscreen; elsewhere the browser's two-step Esc stands. Unverified: a real
  browser's handling of a locked Esc (headless has none); the page logic is
  tested.

Evidence: quake-rs lib tests for the Extras screen (defaults, toggles, sounds,
Esc row, layout and colours, bits) and `draw_fps` placement; quake-wasm tests
for `host_frame_time` (every refresh at 60..240 Hz, clamps), 72 vs 144 frames
a second at 144 Hz through `step`, the FPS window, the readout confined to its
box and byte-identical when off, the `wasm_*` commands and the exports;
`web/verify_extras.py` 36/36 (40/40 at `3ba835f`, with the fourth extra).

## Entity culling and resolved fields (PERF_PLAN C1, D2; 2026-09-25, branch `quake/sim`)

- ✅ **C1: the live client relinks only what the server sends** (`SV_WriteEntitiesToClient`,
  `CL_RelinkEntities`). The port used to relink and draw every edict with a model, every frame.
  That lit walls from behind (CENSUS L22), trailed and spun what nobody could see, and drew 98
  alias models per frame on e1m3 for about 3 visible.
  - Now `SV_LinkEdict` records the edict's leaves (`SV_FindTouchedLeafs`, at most
    `MAX_ENT_LEAFS` = 16, front child first). `SV_FatPVS` unions the PVS of every leaf within 8
    units of the player's `origin + view_ofs`. An entity is sent when it has a `modelindex`, a
    non-empty `model` and a leaf in that set; the player is always sent. `step_walk` gives
    `EF_*` lights, trails, `EF_ROTATE` and drawing only to those. An entity that drops out
    loses its trail history, as `CL_ParseUpdate`'s forcelink restarts it.
  - `PF_makestatic` marks the edict a static. id frees the edict into the signon; the port keeps
    it, because edict numbering and savegames follow it. A static is never relinked. It is drawn
    when a leaf of its `R_AddEfrags` box is in the PVS of the leaf holding the view origin
    (`R_MarkLeaves`, not fattened), after the relinked entities, as `R_StoreEfrags` appends it.
    The box is ±16 for alias models (`Mod_LoadAliasModel`'s "FIXME"), ±maxwidth/2 and
    ±maxheight/2 for sprites, and the model's bounds for brush models.
  - Evidence: frame hashes are identical on four bench workloads (PERF_PLAN C1). The goldens,
    the simbench counts and the census output are unchanged. Tests:
    - `an_entity_outside_the_fat_pvs_is_not_drawn_and_its_flash_lights_nothing`: L22, with a
      control that the light would have lit the wall.
    - `an_entity_in_view_is_drawn_and_its_flash_made`.
    - `statics_draw_through_efrags_in_the_view_pvs`.
    - `touched_leafs_*`, `edict_leafs_cap_at_max_ent_leafs`, `leaf_pvs_*`,
      `fat_pvs_unions_the_leaves_within_8_units`.
  - Accepted gaps:
    - Statics use the edict's float origin and angles, not `svc_spawnstatic`'s coord and angle
      bytes.
    - `R_RecursiveWorldNode`'s frustum test on an efrag's leaf is left out.
      `R_AliasCheckBBox` and the z-buffer hide the same pixels, unless a mesh reaches past its
      efrag box.
    - `SV_WriteEntitiesToClient`'s "packet overflow" cutoff (`MAX_DATAGRAM`) is not modelled.
    - Demo statics are still drawn without the efrag test. The output is the same; only the cost
      differs.
    - The `quaketool scene` tool view draws every entity.
  - Seen, not changed: the port's `SV_ClipToLinks` "points never interact" test uses
    `maxs - mins` where the C reads `v.size`. The two differ only if QuakeC writes
    `mins`/`maxs` without `setsize`.
- ✅ **D2: entity fields are resolved once per progs, not hashed per access.** This is
  byte-identical.
  - id reads `entvars_t` members at fixed offsets. The port looked every field up by name,
    SipHash into a `HashMap<String>`, per access: 29,300 lookups per frame on walk_e1m3. 79% of
    them were the fields `SV_Move`'s per-trace scan reads for every edict.
  - `Vm::fo` (`FieldOfs`: every `entvars_t` field, and `gravity`) and `Vm::go` (the per-frame
    globals) now hold `Fld`/`Glb` handles, resolved by name in `Vm::new`, so any progs works.
    The by-name accessors resolve the name and then run the same code, so the semantics cannot
    drift. A field the progs lacks reads 0 and drops writes.
  - The server's per-frame paths, the edict-scanning builtins and the client gather use the
    handles. The model-cache loop borrows names instead of allocating a `String` per edict per
    frame, and `PF_find` compares borrowed strings. 90 lookups per frame remain: once-per-frame
    reads of the player and the HUD.
  - Proof: the simbench counts, the census output, the `census-edicts` dumps of nine maps at
    five times, the goldens, and 720 frame hashes across five workloads and two resolutions are
    all unchanged. Test `resolved_fields_match_the_by_name_accessors`.
  - Speed: simbench e1m3 2.64 → 0.29 ms per tick. The wasm sim phase is 68–76% lower, and the
    walk_e1m3 frame goes 1.39 → 0.77 ms at 320×200 (PERF_PLAN D2).

## Second review fixes (2026-09-25, branch `quake/polish2`)

Findings of the second adversarial review that live outside `quake-wasm/src/`
(another branch is moving the client there); one line each, the C and the
test in the commit message.

- ✅ **Oracle: underwater views above 320x200 compared the wrong rectangle**
  (MED, tooling) — `compare.py` took the `.json`'s `vrect` (`r_refdef.vrect`)
  for the view's place on the screen. With the eye in a liquid above 320x200
  that is the rectangle in `R_SetupFrame`'s warp buffer (0,0,320,200), not
  the screen's, so it cropped id's frame to its top-left corner and asked the
  port for an unwarped 320x200 view: e1m1's pool scored 6.6% at 960x600. It
  now takes `scr_vrect` (already in the `.json` since `quake/fid2d`), which
  `D_WarpScreen` stretches the buffer over, and warns for an underwater view
  below viewsize 120 (`quaketool view --vrect` draws it unwarped). The pool,
  `--spans 16`: 99.89 / 99.89 / 99.90 / 99.89 at 320x200 / 640x400 / 960x600 /
  1280x800; 99.93 at all four at the page's aspect. The stale 97.6 numbers in
  "Review fixes" are corrected there.
- ✅ **Particles drawn as `D_DrawParticle`** (LOW) — `draw_particles` projected
  with the walls' `xscale`/`yscale` where `R_DrawParticles` scales `vright`/
  `vup` by `R_ViewChanged`'s `xscaleshrink = (vrect.width-6)/
  horizontalFieldOfView` (3 px nearer the centre at a 320-wide view's edge);
  it also sized a centred square by `round(focal/z)`, clipped it at the edge
  and z-tested float depth. Now `render/part.rs` is `D_DrawParticle`
  (`d_part.c`) with `D_ViewChanged`'s constants (`d_modech.c`): `u =
  (int)(xcenter + zi*x + 0.5)`, `pix = (int)(zi*0x8000) >> d_pix_shift`
  clamped to `[d_pix_min, d_pix_max]`, drawn from `(u, v)` right and down,
  dropped whole past `d_vrectright_particle`/`d_vrectbottom_particle`,
  `PARTICLE_Z_CLIP` 8, and id's `pz <= izi` on the quantized 1/z (particles
  of one burst tie often; the later wins). Measured: the oracle now dumps
  the particles it draws (`.parts`) and `quaketool view --particles` draws
  them; the shotgun's puffs on e1m1 match 100% of particle pixels at 320x200
  / 640x400 / 960x600 (were 12.8 / 12.6 / 15.0%). Tests
  `particles_project_with_r_main_c_xscaleshrink`,
  `particle_size_is_d_part_cs_izi_shift`,
  `particles_obey_d_part_cs_clip_and_edges`,
  `particles_tie_on_d_part_cs_quantized_1_over_z`. Goldens unchanged (no
  particles in them).
- ✅ **`sv.time` is a double** (fid2d's leftover) — id's `server_t.time` is a
  `double` (server.h), advanced by `SV_Physics`' `sv.time += host_frametime`
  (a double too), and QuakeC sees its float through each
  `pr_global_struct->time = sv.time`. The port kept the clock in the QC float
  global and added `dt` in f32: 7.2999954 after 63 frames of 0.1 s from 1.0,
  so the new-weapon flash's `(int)((cl.time - item_gettime)*10)` showed the
  frame before (screen2d `flash.rl_new`), and the error grows with the session.
  Now `Vm::sv_time` is an f64 (`Server::sv_time`), `Server::time()` its float
  (the QC global's and `svc_time`'s value), and the double comparisons are
  double: `SV_RunThink`'s `thinktime > sv.time + host_frametime` and
  `thinktime = sv.time`, `SV_Physics_Pusher`'s `ltime + host_frametime`,
  `SV_ClientThink`/`SV_WaterJump`'s `teleport_time`, `ED_Alloc`/`ED_Free`'s
  `freetime`. New entry points `run_frame_f64`/`client_frame_f64` take id's
  `double host_frametime`; the f32 ones widen theirs (quake-wasm's, for now).
  The settle frames and the quaketool drivers (census, census-edicts,
  simbench, sim, playtest, changelevel) run id's exact 0.1. Savegames: written
  `%f` from the double (`Host_Savegame_f`), read into a float and restarted
  from it (`Host_Loadgame_f`'s `float time`). Tests
  `sv_time_is_a_double_and_the_qc_time_global_its_float`,
  `a_think_is_due_by_sv_time_plus_host_frametime_in_double` (a think at
  `time + 0.1` = 7.4f waits a frame at sv.time 7.3, as in the C; the f32 sum
  ran it early), the save tests (`1234.567890` written, the float read back).
  Evidence: screen2d `flash.rl_new` 98.3 / 99.2 / 99.4% -> 100% at 320x200 /
  640x400 / 960x600. id's edicts (the oracle's `oracle_edicts`, nine maps at
  t = 1.7 ... 120.7) against `census-edicts`: `nextthink` mismatches over
  every non-monster edict 310 -> 252, none worse (every map's player idle
  think now on id's phase, e1m1's two start doors at 4.040/4.035, e1m4,
  e1m6, e1m8 movers); the dump at 120.7 no longer reads 120.699. simbench
  and the census change as a one-frame think phase shift propagates through
  random-driven fights (e1m1, e1m3, e1m5-e1m7 simbench counts identical;
  census: no new fault, error or never-moved mover). **Goldens re-baselined**
  (e1m1 unchanged): e1m2 `8ce25660` -> `9ae2b478`, e1m3 `3531e9cd` ->
  `2ca0f916`. The scene is taken after `SV_SpawnServer`'s two settle frames,
  at sv.time 1.2; an item's `PlaceItem` think at `time + 0.2` = 1.2f =
  1.2000000477 is later than 1.1 + 0.1 in double, so it has not run yet and
  the shells box (e1m2) and an ammo box (e1m3) stand where the map put them,
  as in id's (its edict dump at t = 1.2: every item's nextthink 1.200, not yet
  dropped). The f32 sum 1.1f + 0.1f = 1.2f ran it a frame early.
- ✅ **`sv_gravity` outlives the map; the gravity checks can fail** (LOW) —
  `Server::new` reset the cvar to 800 before worldspawn ran, so the census
  test's "the next map's worldspawn sets it back to 800" (e1m5 after e1m8,
  `quake-wasm/src/census_tests.rs`) and the unit test's "a fresh server
  starts at the default" held whatever worldspawn did. id's cvar outlives the
  map (`SV_SpawnServer` never touches it; id1's worldspawn sets it on every
  map). The reset is gone; the unit test (`sv_gravity_cvar_drives_add_gravity`)
  now checks the next server keeps 100 until `cvar_set("sv_gravity", "800")`,
  and the census test fails if that `cvar_set` is dropped (checked by breaking
  it: 100 != 800). Savegame loads run worldspawn (`Host_Loadgame_f` ->
  `SV_SpawnServer`), so an e1m8 save still loads at 100.
- ✅ **Stale docs** — README and quake-rs/README said save/load was out of
  scope and counted 458 + 48 tests; the verify-script list, CENSUS L11's moot
  "still open", L15 (partly fixed by F9, not marked; the per-side clamp after
  the master volume and the one-shots' per-frame spatialisation are still
  open, `web/index.html` `playRouted`), the census tests described as
  `#[ignore]`d, and the fix-client L11 / fix-server F3 lines. Not changed,
  another agent's files: `render/surf.rs`'s `mipadjust` comment is backwards
  (it says a texture scaled up in the editor "drops to a coarser mip sooner";
  its short axes give `mipadjust` 4, which raises `nearzi * scale_for_mip *
  mipadjust`, so it keeps a FINER level longer — and it counts world units
  per texel, not texels per unit); `quake-wasm/src/census_tests.rs`'s module
  doc still says `#[ignore]`.
- ✅ **Menu cursors kept per menu** (fid2d's leftover) — `menu.c` keeps one
  static cursor per menu and no `M_Menu_*_f` resets it (only `M_Menu_Help_f`
  sets `help_page = 0`): Escape from Options lands on "Options", Load after
  a load opens on that slot, the Quit prompt returns to its screen's row.
  The port had one cursor that every screen change put back on row 0.
  `menu.rs` now keeps `m_main_cursor`, `m_singleplayer_cursor`,
  `load_cursor` (Load and Save share it), `m_multiplayer_cursor`,
  `options_cursor`, `keys_cursor`, `vid_line` and the Web extras page's own;
  Help and Quit have none. `vid_line` is a static in `vid_dos.c` too; id's
  starts on the list's first line (the live mode in a default DOS setup),
  the port's list is its own, so the first visit opens on the live mode and
  later ones where the player left it. `reset_nav` (the host's boot) puts
  them all at 0, a program start. Tests
  `each_menu_keeps_its_cursor_like_menu_cs_statics`,
  `a_closed_menu_reopens_on_m_main_cursor`,
  `help_starts_on_page_0_and_quit_returns_to_the_screens_cursor`,
  `video_opens_on_the_live_mode_then_keeps_vid_line`,
  `reset_nav_is_a_program_start_for_the_cursors`. screen2d
  `menu_sp.sp_again` 99.62 / 99.90 / 99.95% -> 100%; every menu shot now
  matches in all three modes except the two explained ones (Options' Web
  extras row, Video's mode list). `web/verify_menu.py`
  follows id's cursors (61/61). **Two quake-wasm tests fail with this
  commit** (they re-navigate assuming the reset; quake-wasm is off limits
  here): see the list below. The commit is the branch's last.

Left for the quake-wasm pass (found here, not changed: `quake-wasm/src/` is
being moved into `quake-rs/src/client/`) — all done on `quake/polish3`
("Second review fixes, client side", below), the `--vrect` tooling item
aside:
- The live host hands `client_frame` an f32 `dt`, which the server widens;
  call `client_frame_f64` with `Host_FilterTime`'s double `host_frametime`, so
  `sv.time` adds exactly id's frame times.
- `census_tests.rs`'s module doc says the tests are `#[ignore]`d; all ten
  (this line said twenty; the file had ten at `3ba835f`) run in the normal suite.
- The page's sound law (not quake-wasm, but the same pass): `playRouted` /
  `spatializeDynLoop` clamp each side after the master volume (id clamps
  `leftvol`/`rightvol` at 255, `snd_mix.c`, then scales by `volume`), and
  non-looping one-shots are not re-spatialised each frame (`S_Update` runs
  `SND_Spatialize` on every channel) — CENSUS L15's open half.
- Tooling: `quaketool view --vrect` draws an underwater view unwarped
  (`compare.py` warns); the warp at viewsize below 120 is not compared.
- Menu cursors (below): two quake-wasm tests navigate assuming the old reset
  (`host::tests::gamma_changes_the_presented_frame_and_one_is_byte_identity`,
  `input::tests::invert_mouse_flips_pitch_and_lookspring_recentres_on_unlock`:
  after reopening the menu they press DOWN twice for "Options", which from
  the kept "Options" is Quit); each needs its re-navigation shortened to
  `menu_cancel(); menu_select();` (+ one DOWN for Lookspring). And
  `Menu::reset_nav` is shared by `boot()` (a program start, where menu.c's
  statics are 0) and New Game / load / `map` (where id keeps every cursor):
  split it so only the boot resets the cursors (app.rs's re-boot test pins
  cursor 0 after `boot()`).
- ✅ **Census: e1m8's `*6` reported "never moved"** (LOW, tooling) — the mover
  baselines were taken after the signon frames, by which time
  `PutClientInServer`'s `force_retouch` had opened the door (an ogre stands
  in its trigger field, CENSUS F8). Baselines are now taken right after the
  spawn, before the player connects: e1m8 "19 total, 17 moved, never moved:
  func_wall#111(*16) func_wall#159(*27)", `*6` among the movers already
  moving at the idle; no other line of the census changes.
- ✅ **The Load menu was empty after a page reload** (LOW, pre-existing) —
  `refreshSaveComments()` (the page's `M_ScanSaves`: localStorage's
  `s0..s11.sav` into the menu's slot comments) ran only after a save,
  though its comment said "at boot"; a reloaded page listed every slot as
  `--- UNUSED SLOT ---` and refused to load them. id rescans each time the
  screen opens (`M_ScanSaves` in `M_Menu_Load_f` / `M_Menu_Save_f`). The page
  now scans at boot and whenever the menu enters Load or Save
  (`menu_visible` / `menu_screen_id`, checked before the frame that draws
  it); existing exports only. `web/verify_save.py` also saves to slot 0,
  reloads, forgets the boot scan and opens Load: slot 0's row is the save's
  comment and Enter loads it (the old page fails both).
- ✅ **A hum could start after its own stop and loop** (LOW) —
  `drainGameSounds`: an inaudible sound (gain <= 0.02) called `stopKey`
  without bumping `keySeq`, so a looping sample whose first decode was still
  pending when an inaudible sound on its (entity, channel) arrived (a far
  door's stop sound during its hum's first decode) started afterwards and
  looped forever. `S_StartSound` picks the channel, and so overrides the
  old sound, before the audibility test (snd_dma.c); the page now bumps
  `keySeq` first, so the pending decode sees the override. `web/verify_loops.py`
  feeds the two sounds through `drainGameSounds` from a stand-in `exp` (a
  fresh 8-bit WAV at the listener on entity 900 channel 2, then a sound
  10000 units away on the same key): the hum no longer starts (the old page
  fails), and alone it does (the control).

## World pass: id's edge renderer (2026-09-25, branch `quake/edge`, PERF_PLAN A3)

- ✅ **The world and the brush entities are drawn as WinQuake draws them**
  (`render/edge.rs`). Before, each
  clipped face was a polygon walked row by row, front to back by centroid,
  against an f32 z-buffer cleared every frame, with the 16-pixel grid
  restarted wherever a run of passing z tests began. Now the frame is
  `R_EdgeDrawing`'s:
  - `R_RecursiveWorldNode` walks the BSP front to back from the view leaf's
    PVS (`R_MarkLeaves`, parents as `Mod_SetParent`), culls nodes against the
    four sides of the view (`R_TransformFrustum`, `R_SetUpFrustumIndexes`),
    marks faces through the leaves' marksurfaces and gives every face, node
    and leaf its key (smaller is nearer).
  - `R_RenderFace` clips each face's edges to the view's sides (`R_ClipEdge`,
    no near plane: `R_EmitEdge` clamps z to `NEAR_CLIP` and u/v to the view)
    and puts them in per-scanline lists in the C's floats and 12.20 fixed
    point, sharing edges through the `medge_t` edge cache (owner test,
    "fully clipped" frame stamps), adding the left-side edge and the right
    side's 1/z from `r_leftexit`/`r_rightexit`, which persist across faces as
    the C's statics do.
  - Brush entities join the same list (`R_DrawBEntitiesOnList`):
    `R_BmodelCheckBBox`, `R_SplitEntityOnNode2`, then either
    `R_DrawSubmodelPolygons` (one leaf: whole faces keyed as the leaf, sorted
    on 1/z against the leaf's other brush models in `R_LeadingEdge`) or
    `R_DrawSolidClippedSubmodelPolygons` (`R_RecursiveClipBPoly` cuts the
    faces into fragments along the world's planes, each keyed as its leaf,
    with its own `nearzi`). Inline submodels and external `b_*.bsp` boxes
    alike.
  - `R_ScanEdges` walks the scanlines with the active edge table
    (`R_InsertNewEdges`, `R_StepActiveU`, `R_RemoveEdges`) and the surface
    stack (`R_LeadingEdge`, `R_TrailingEdge`, `R_CleanupSpan`), emitting a span
    wherever the nearest surface changes; the background surface
    (`r_clearcolor` 2) owns what nothing covers.
  - `D_DrawSurfaces` draws each surface's spans once: sky through
    `D_DrawSkyScans8` (no more deferred sky pixels), liquids through
    `Turbulent8`, walls at the mip level of their own `nearzi` through the
    surface cache and `D_DrawSpans16`, then `D_DrawZSpans` writes the 16-bit
    1/z. Every pixel of the view is written exactly once and nothing is
    cleared, image or z-buffer. The texel arithmetic of a span is the
    polygon walker's (the gradients at the span's first pixel), so a
    surface drawn over the same run gets the same texels.
- ✅ **The entities test id's 16-bit `d_pzbuffer`** (`render::ZBUF`, an
  `[i16]` the world's spans fill; the entity passes take it as `zbuf`): alias
  models and the gun `D_PolysetDraw`'s `(lzi >> 16) >= *lpz`, particles
  `D_DrawParticle`'s `izi = (int)(zi * 0x8000)`, sprites the sprite spans'
  `izi >> 16` against `D_DrawZSpans`' values; the gun's tripled 1/z as before.
- **What it reproduces that the polygon walker could not:**
  - e1m2's face 733 (the w2a open note above): a stale `r_rightexit` gives it
    another face's 1/z and a finer mip, as in id. e1m2's first frame 99.21 →
    99.94% (`--spans 16`), 98.73 → 100.00 at the page's aspect, 99.46 →
    100.00 at viewsize 100.
  - Brush models cut into leaf fragments with their own mip levels (the w2a
    "not done"), sorted with the world by leaf key instead of per-pixel depth;
    world faces in front of a brush model now cut its spans too (w2b noted
    they did not).
  - Entity pixels match 100% in every oracle case (12 sweep views were
    99.30–99.96, the entity cut into world spans differently).
- **Oracle** (`compare.py`, id's x86 spans `--spans 16`, 320x200 unless
  stated; before → after). 372 cases (the standard rows world and ents, both
  span modes, 320x200 / 640x480 / 640x400 with and without the page's aspect,
  viewsize 100, exact perspective, mip 0, muzzle flash, settle 3, a second
  clock, the altar, the teleporter, the water, and 4 maps × 6 yaws × 3 pitches
  at 320x200 and at 640x400 with aspect): mean 99.798 → 99.833, 55 better, 3
  worse by one pixel each (worst −0.0016 points).

  | row | e1m1 | e1m2 | e1m3 | e1m7 |
  |---|---|---|---|---|
  | world `--spans 16` | 99.959 → 99.961 | 99.206 → 99.941 | 99.981 → 99.981 | 99.911 → 99.911 |
  | `--aspect 0.8333333` | 100.00 → 100.00 | 98.73 → 100.00 | 100.00 → 100.00 | 99.997 → 99.997 |
  | viewsize 100, aspect | 100.00 → 100.00 | 99.46 → 100.00 | 100.00 → 100.00 | 99.996 → 99.996 |
  | 640x480 | 99.971 → 99.971 | 99.968 → 99.968 | 99.992 → 99.992 | 99.943 → 99.944 |
  | `--spans 8` (id's portable C) | 94.57 → 94.57 | 96.09 → 96.82 | 97.27 → 97.27 | 91.72 → 91.72 |
  | exact extra vs `--spans 1` | 99.94 → 99.94 | 99.18 → 99.91 | 99.98 → 99.98 | 99.91 → 99.91 |
  | muzzle flash (`+attack --settle 3`) | 98.02 → 98.02 | 98.34 → 99.46 | 99.89 → 99.89 | 99.69 → 99.69 |

  Sweeps: 72 views at 320x200 (world and ents) 99.982 → 99.993 mean, 16
  better, none worse; at 640x400 with aspect 99.9970 → 99.9972. Brush
  entities: 144 views facing the first twelve brush models of each map from
  two or four sides (ents mode) 94.35 → 99.37, 57 better, none worse; the
  lowest left (57–94%) are cameras inside solid (leaf 0, no PVS), where id
  shows the background on floors and walls both port renderers draw — not
  understood, and not a place a player's eye can be. What is left in the
  standard rows is texel-boundary pixels on 45-degree lines of floor texture
  and a few sky pixels: float noise of the texel arithmetic (the edge
  arithmetic in f64 instead of the C's floats moves nothing).
- **Goldens:** e1m1 `4807aaa1` and e1m2 `8ce25660` unchanged; e1m3
  `3531e9cd` → `1867f5a7` (150 px, 0.06%: texels of the shells box, now
  drawn over id's spans and at its fragments' mip levels).
- **Speed** (PERF_PLAN A3 has the tables): wasm whole frame −23 to −33% at
  the median and −21 to −36% at p95 on demo1, walk_e1m1, fire_e1m1 and
  walk_e1m3 at 640x400 and 1280x800; the brush pass (world + submodel +
  external) −22 to −39%; native −43 to −57% per frame (the w2b native
  regression is gone with its per-pixel z test).
- **Accepted gaps and notes:**
  - id's pools (`r_maxedges` 2400, `r_maxsurfs` 800, `MAXSPANS` 3000 with its
    mid-scan flush) are growable buffers. Running out of spans only changes
    when id draws; running out of edges or surfaces drops the farthest faces
    and the brush models. id's demos never come close (at most 971 edges and
    390 surfaces in any frame of demo1–3); `quaketool scene`'s camera, which
    can stand in solid (no PVS), makes up to 3542 edges and 1400 surfaces,
    where id would drop faces. `RenderStats` counts them.
  - Brush models do not rotate (`BModelInstance` has no angles; the
    shareware has no rotating brush models), so `R_RotateBmodel` is the
    identity and only moves `modelorg` and the clip planes.
  - id adds brush entities to the edge list in `cl_visedicts` order; the port
    adds the inline submodels, then the external boxes. The order only breaks
    exact ties (two edges at the same u, two same-leaf brush models at the
    same 1/z).
  - `r_clearcolor` is fixed at its default, 2.
  - The synthetic test rooms are now wound clockwise seen from the front,
    as qbsp winds every face: the edge renderer, like id's, takes leading and
    trailing edges from the winding, and a face wound the other way makes only
    inverted spans. A bsp without nodes or leaves (test fixtures only) goes in
    as one leaf's brush model, sorted on 1/z.
  - The polygon walker is deleted (`draw_world_textured`, `draw_submodel`,
    `draw_brush_bsp`, the `raster_poly_*` fillers, the deferred sky, the
    face-AABB frustum and near-plane clip, the f32 z-buffer): the last commit
    of the branch, byte-identical (goldens, the 372 oracle cases, 104 wasm
    frame hashes over four workloads at 320x200 and 640x400). Its fill-rule and
    crack tests went with it; the edge renderer's coverage is id's by
    construction (the spans partition every scanline).

## Second review fixes, client side (2026-09-25, branch `quake/polish3`)

The second review's client/platform findings, polish2's quake-wasm
follow-ups, and one presentation fix. One commit each; the C followed and
the evidence are in the commit messages. On `quake/overnight` after
`quake/polish2` and `quake/edge`: goldens `4807aaa1` / `9ae2b478` /
`c65b7046` unchanged.

- ✅ **Every carried weapon flashed at each level start** (MED) — restart,
  load and demo start too. The port stamped `cl.item_gettime` against the
  zeroed `cl.items` at its own `cl.time` (~1.2), so the icons flashed for a
  second. In the C, `CL_ClearState` zeroes `cl.time` too, and the signon's
  clientdata is parsed in `CL_ReadFromServer` after `cl.time +=
  host_frametime` and before `CL_LerpPoint` snaps `cl.time` to the
  server's: the owned bits are stamped at about `host_frametime`, and the
  flash is over by the first drawn frame (`cl.time` >= 1.2). Demo playback
  reads the signon and frame 0's block in one `CL_ReadFromServer`, the
  same. Every `CL_ClearState` site (New Game/map/load via `assemble_walk`,
  changelevel, restart, `DemoPlay::new`, the demo's wrap) seeds the spawn
  items with the get-times at 0. Tests
  `only_items_got_in_play_flash_not_what_a_level_starts_with` (replaces the
  test that locked the bug in), `demo_start_does_not_flash_the_recorded_weapons`.
  Oracle (screen2d's harness without its 30-frame settle, e1m1): the
  status-bar rows at `cl.time` 1.9 / 2.3 differed from id's by 264 / 245 px,
  now 0 / 0 at 320x200 and 960x600. CENSUS F18's premise corrected.
  `quaketool play` frame hashes change at frames 0, 30 and 60 only (inside
  the old flash).
- ✅ **A door/lift/train hum could loop forever** (MED) — the 12-sound
  cap ran before the (entity, channel) override, so a mover's stop sound
  (CHAN_VOICE) in a busy frame was dropped. `SND_PickChannel`'s same-key
  override comes first and always wins: past the cap a non-zero channel's
  sound still goes out, replacing an undrained entry of its key (bounded:
  the cap plus one per key). Test `queue_cap_never_drops_a_channel_override`.
- ✅ **Console-only prints never reached the notify lines** (LOW) —
  `Con_Print` stamps `con_times` for every console line, so "Saving game to
  s1.sav..." / "done." after a menu save show over the game. The console
  keeps what the host prints for the active mode's notify lines
  (`Console::take_unnotified`, handed over after every `ensure_app`); a
  console toggle (`Con_ToggleConsole_f` zeroes `con_times`) or a level load
  (`SCR_EndLoadingPlaque`'s `Con_ClearNotify`) drops it; `map`'s own
  "loading" line stays console-only. Test `console_prints_reach_the_notify_lines`.
  Options > "Go to console" is `Con_ToggleConsole_f` too (it set the
  console open without zeroing `con_times`); test
  `go_to_console_is_con_toggleconsole_f`.
- ✅ **Tab pressed in the menu became +showscores when the menu closed**
  (LOW) — `Key_Event` hands a key down to its binding only when
  `key_dest == key_menu && menubound[key]`, `key_dest == key_console &&
  !consolekeys[key]`, or in the game; `key_down` now routes with those
  tables (Tab in the menu is `M_Keydown`'s). Test
  `keys_pressed_in_the_menu_or_console_do_not_hold_their_binding`.
- ✅ **Old saves loaded with an empty netname** (LOW) — "  was shot by a
  Grunt" until the next level: a load keeps the save's fields
  (`Host_Spawn_f` skips the edict setup when `sv.loadgame`), and saves from
  before tonight had none. An empty one loads as "player". Test
  `old_saves_load_with_the_player_named`.
- ✅ **The underwater warp allocated every frame** (LOW) — `rowptr`,
  `column` and ~1000 f64-sin entries of `intsintable`. They live in a
  thread-local table now (R_InitTurb fills `intsintable` once), rebuilt only
  when a size changes. Byte-identical: `quaketool view` of e1m1's pool at
  five modes x two times, `cmp`-equal; test
  `warp_tables_kept_across_frames_change_nothing`.
- ✅ **Demo particles fell at a constant 800** (LOW) — `R_DrawParticles`
  reads the client's `sv_gravity` cvar in playback too (e1m8's worldspawn
  leaves it at 100, and it outlives the map); `Server::sv_gravity_cvar()`.
  Test `demo_particles_fall_by_the_sv_gravity_cvar`. The census tests'
  module doc no longer says they are `#[ignore]`d.
- ✅ **polish2's follow-ups**: (a) `Menu::reset_nav` keeps menu.c's
  cursors (a `map`, New Game, load or demo keeps them); only a program
  start (`boot`, `boot_attract`: `Menu::reset_boot`) zeroes them. Tests
  `only_a_program_start_resets_the_cursors`,
  `only_a_boot_resets_the_menu_cursors`. (b) The live host drives the
  server with `Host_FilterTime`'s double (`client_frame_f64`): `sv.time`
  adds exactly `host_frametime`; the client's own timing takes the same f32
  as before. Tests `host_frametime_is_the_double`,
  `the_server_advances_by_the_hosts_double`; `quaketool play` hashes
  identical. (c) CENSUS L15's open half, in the page: each side clamped at
  full before the master volume (`snd_mix.c` clamps `leftvol`/`rightvol`
  at 255, then `S_TransferPaintBuffer` scales by `volume`; the page
  clamped after it, up to 1.43x louder), and one-shots re-spatialised every
  frame (`S_Update`). `web/verify_loops.py` section 4.
- ✅ **The canvas box fits the window** (presentation) — the page showed
  the framebuffer in a fixed 640x480 box; with the 2-D layer 1:1, the
  default 960x600's 320-wide status bar and menus came out 213 px wide. The
  canvas is now the largest 4:3 box the window fits under the header with
  the status line in view, never under 640x480; the narrow-screen and
  fullscreen rules are unchanged. 1440x900: 640x480 -> 976x732; 1920x1080:
  -> 1216x912. The engine's rendering and resolutions are untouched.

Found, not fixed:
- `S_StaticSound` combines static channels of the same sample into one
  (`S_Update`'s "combine static sounds", whose summed volumes then clamp at
  255); the page plays each torch on its own source, so several near
  torches of one sample are louder than id's.
- The page drains at most 16 one-shots a frame (`drainGameSounds`' guard);
  past the cap the queue can now hold a few more keyed entries, which then
  play a frame later.

## Demo commands, timedemo, pause (2026-09-25, branch `quake/timedemo`)

Quake's own demo and pause commands (CENSUS L12's pause half), against
`cl_demo.c`, `cl_main.c`, `host.c` and `host_cmd.c`.

- ✅ **`playdemo`, `stopdemo`, `startdemos`, `demos`, and the attract loop
  through them.** `cls`'s demo half is host state (`quake-wasm` `App::cls`:
  `demos[8]`, `demonum`) and the commands are id's: `CL_PlayDemo_f`
  (`CL_Disconnect`, `COM_DefaultExtension` ".dem", "Playing demo from %s.",
  "ERROR: couldn't open." + `demonum = -1`; the usage line "play <demoname> :
  plays a demo" is the C's), `Host_Stopdemo_f`, `Host_Startdemos_f` ("%i
  demo(s) in loop", "Max %i demos in demoloop", 15-character names, starts the
  loop only with nothing running and the loop not off, else switches it off),
  `Host_Demos_f` (off: back at the second slot). The boots run quake.rc's
  `startdemos demo1 demo2 demo3`, and a demo's end is `Host_EndGame`:
  `CL_NextDemo` (wrap after the last listed slot) or, outside the loop,
  `CL_Disconnect`. `map`, `load` and New Game switch the loop off
  (`cls.demonum = -1` in `Host_Map_f`/`Host_Loadgame_f`) and disconnect the
  demo. The attract loop cycles demo1 → demo2 → demo3 as before (F15), now
  printing id's console lines as it goes.
- ✅ **Disconnected** (`cls.state == ca_disconnected`: after `stopdemo`, a demo
  ending outside the loop, a `playdemo` that cannot open its file): nothing
  plays and the console covers the screen (`con_forcedup`: full height at
  once, its input line drawn, typing goes to it — `Key_Event`'s `key_game &&
  con_forcedup`), the menu over it; the console toggle brings up the main
  menu (`Con_ToggleConsole_f` with no connection), and leaving the main menu
  resumes the loop (`M_Main_Key`'s `CL_NextDemo`). Before this there was no
  such state: the port always had a level or a demo.
- ✅ **`timedemo`** (`CL_TimeDemo_f`, `CL_FinishTimeDemo`): the demo one
  recorded message per host frame, no 72 fps cap (`Host_FilterTime`'s
  `!cls.timedemo`), each drawn at its message's time (`CL_LerpPoint` in a
  timedemo: no interpolation; the parser's keyframes, EF_ROTATE spin
  included); the first frame reads through the message after the one that
  completed the signon, as `CL_GetMessage` does; the frame that reads the
  closing `svc_disconnect` draws nothing and ends it (`Host_EndGame`: the loop's
  next demo, or disconnected), and `CL_StopPlayback` (a `stopdemo`,
  `playdemo`, `map` mid-run) ends it early; `"%i frames %5.1f seconds %5.1f
  fps"` from `host_framecount - td_startframe - 1` and `realtime -
  td_starttime` (a float, as `cls.td_starttime`), `time = 1` if 0. The
  particles move by the time between messages (`cl.time - cl.oldtime`), the
  view kick, fades and stair smoothing by `host_frametime`. demo1/2/3 draw
  969/985/1090 frames, id's counts (oracle). The browser runs host frames
  back to back in ~12 ms slices per animation frame, presenting the last;
  each `step` is handed the previous one's duration, so `realtime` adds up
  the frames' own time and not the page's pauses between slices (what the
  number includes: `PERF_PLAN.md` §10). One departure: id's sets
  `cls.timedemo` even when the file does not open, which only leaves the host
  uncapped until the next disconnect; the port sets it when the demo plays.
  Natively: `quaketool timedemo`.
- ✅ **`pause`** (`Host_Pause_f`, CENSUS L12's pause half): default.cfg's
  `bind PAUSE "pause"` (also with the console down: PAUSE is no console key;
  not in the menu), forwarded to the server (`Cmd_ForwardToServer`: nothing
  during demo playback, `Can't "pause", not connected` with nothing running),
  which toggles `sv.paused` and broadcasts "player paused the game" /
  "player unpaused the game" (`SV_BroadcastPrintf`, the notify line and the
  console). While paused neither `SV_ClientThink` nor `SV_Physics` runs, so
  `sv.time` and with it `cl.time` stand — particles, dlights, light styles,
  sky, liquids, animations; `cl.paused` (the local client's `svc_setpause`,
  the same frame) keeps V_CalcRefdef off, so the view keeps its kick and
  stair smoothing (`steptime = cl.time - cl.oldtime`: nothing while the
  server stands, behind the menu too); the palette-shift fades, the notify
  and centerprint timers and the ambient ramps run on host time, playing
  sounds and loops carry on, as in the C. `SCR_DrawPause` draws
  `gfx/pause.lmp` at `((w - 128)/2, (h - 48 - 24)/2)` outside an
  intermission, under the menu; a recorded `svc_setpause` shows it during
  demo playback. Against id's composited screen (`oracle/screen2d.py`,
  scenario `pause`): 100% at 320x200, 640x400 and 960x600, paused and
  unpaused. Not modelled: the `pausable` and `showpause` cvars (both 1, id's
  defaults, and the port has no cvar registry) and `VID_HandlePause` (the
  Windows build frees a windowed mode's mouse while paused).
- **No loading plaque, and none needed** (`SCR_BeginLoadingPlaque` /
  `SCR_DrawLoading`): id's draws `gfx/loading.lmp` over the last frame while a
  changelevel, restart, a load or the next attract demo loads, because
  loading took seconds. The port loads within the frame that starts it —
  measured in the browser (headless Chromium, wasm): `map e1m2` 8–23 ms,
  `map e1m1` 6–10 ms, `playdemo demo2` 7–9 ms — so there is no loading
  moment to show it in.
- ~~Kept, not id's: **the menu does not stop the demo loop.**~~ Fixed on
  `quake/polish4b` ("Final review fixes, UI side"): the page boots into the
  demos with no menu, and the menu stops the loop as `M_Menu_Main_f` does.

## Final review fixes, UI side (2026-09-25, branch `quake/polish4b`)

The final review's menu, console, keyboard and page findings, against
`keys.c`, `menu.c`, `console.c`, `cmd.c`/`cvar.c` and `host.c`. One commit
each; the C followed and the evidence are in the commit messages.

- ✅ **Keys went where the page sent them, not where `Key_Event` sends
  them** (MED). The page routed the keyboard itself: the console key opened
  the console over the menu and over a key grab, Enter answered the Quit
  prompt "yes", and the mouse buttons could not be bound. Every key now goes
  through one export, `key_event(keynum, down, ch)` — keys.c's `Key_Event`
  in `quake-wasm` `input.rs`, in id's order: New Game's `SCR_ModalMessage`
  takes every key first (`y` / `n` / Escape by key number); autorepeat is
  ignored but for Backspace and Pause; an unbound mouse button prints "MOUSE2
  is unbound, hit F4 to set."; Escape is the menu's own Escape or
  `M_ToggleMenu_f`; a key up releases its `+` binding wherever the keyboard
  is; a key down runs its binding in the game, with the console down only
  for the keys it does not keep (`consolekeys`: so Home/End reach
  `centerview`, as in id), with the menu up only for Escape and F1–F12
  (`menubound`); every other key, Shift applied (`keyshift[]`), goes to
  `M_Keydown` (the engine's `Menu::keydown`, each screen's `M_*_Key`) or
  `Key_Console` (`Console::key`). The console key is default.cfg's `bind `` ` ``
  "toggleconsole"` (and `~`), a binding like any other (`BIND_TOGGLECONSOLE`):
  over the menu it is `M_Keydown`'s, which ignores it, and during a key grab
  `M_Keys_Key` refuses it and ends the grab. The Quit prompt is `M_Quit_Key`:
  only y/Y quit, n/N/Escape go back, Enter and the rest do nothing. The page
  forwards the mouse buttons (K_MOUSE1..3) while a key is being grabbed, so
  Customize controls binds them as id's does. Losing the window's focus runs
  vid_win.c's `ClearAllStates` (`key_clear_states`: nothing stays held). The
  menu/console exports the tests and automation use (`menu_up`,
  `menu_select`, `console_enter`, ...) are one key each through the same
  path, and so is the 2-D oracle's `key` (the C's `oracle_key` is
  `Key_Event` too). One platform choice: the page hands `Key_Event` the
  character the player's keyboard layout typed (`ch`), which the console and
  the Setup name fields insert; id's inserts the key number through its US
  `keyshift[]` table, which the port still uses when the page gives none —
  the same on a US layout, and a French or German player types what the
  keys say. Tests `the_quit_prompt_takes_only_y_and_n`,
  `the_console_key_over_the_menu_is_the_menus`,
  `mouse_buttons_bind_on_customize_controls`,
  `autorepeat_is_ignored_but_for_backspace`, `keydown_is_each_screens_m_key`,
  `key_init_tables_are_ids`, `key_console_enter_submits_and_echoes_the_line`
  (an empty Enter echoes `]` as id's); `web/verify_menu.py` (Enter and `` ` ``
  leave the Quit prompt up; a right click is the key a grab binds). The 2-D
  oracle is unchanged (every menu scenario 100%, as before).
- ✅ **The page booted into the main menu over the attract demo, and the
  demos cycled behind the menu** (MED). Quake starts with `key_dest =
  key_game`: quake.rc's `startdemos demo1 demo2 demo3` plays with no menu,
  and during demo playback a console key brings up the main menu
  (`Key_Event`: `cls.demoplayback && down && consolekeys[key] && key_dest ==
  key_game` → `M_ToggleMenu_f`; Escape too, the mouse buttons and F-keys
  not). `M_Menu_Main_f` from outside the menu stops the loop
  (`m_save_demonum = cls.demonum; cls.demonum = -1`): the demo playing goes
  on to its end, then `Host_EndGame` disconnects instead of playing the next
  — the console forced up, the menu over it — and `M_Main_Key`'s Escape puts
  the loop back and, with nothing playing, starts its next demo
  (`CL_NextDemo`). The port now does all of it (`App::m_menu_main`,
  `MenuAction::Resume`, `boot_attract` leaves the menu closed); the page
  keeps its click-to-start overlay (browsers need a gesture before they play
  sound), which now says "then press any key for the menu", and the status
  line and the help say so too. With the console out, `M_Draw` puts the menu
  over `Draw_ConsoleBackground (vid.height)` instead of the fade
  (`scr_con_current`), which the disconnected screen now shows
  (`render::draw_menu_over_console`): the 2-D oracle's new
  `menu_disconnected` scenario went 41.5 → 99.4% (320x200), 29.8 → 99.4
  (640x400), 27.7 → 99.4 (960x600) — what is left is the console's version
  stamp, as in the `console` scenario (`oracle.c` now shoots a frame that
  renders no view). Tests `boot_attract_plays_the_demo_and_a_key_brings_up_the_menu`,
  `the_menu_stops_the_attract_loop_until_escape`,
  `attract_loop_cycles_demo1_demo2_demo3` (no menu over it); the verify
  scripts that opened with "the attract menu" (`verify_demo`, `_input`,
  `_menu`, `_extras`, `_timedemo`) now check that no menu is up at boot and
  that a key brings it.
- ✅ **`help` printed the port's command list** (MED). menu.c registers
  `help` as `M_Menu_Help_f`: the Help/Ordering screen on its first page,
  the menu taking the keyboard from the console (`App::m_menu_help`, which
  `svc_sellscreen` runs too). The port's list moved to `wasm_help`, a
  `wasm_` name like the extras' (not id's); `cmdlist`, which id never had,
  is gone. The page's console drawer and README list both. Test
  `help_is_the_help_screen_and_wasm_help_the_ports_list`.
- ✅ **The console had no history, no Tab completion, no scrollback, and
  typed any Unicode** (MED). `Key_Console` (keys.c) is now whole in the
  engine's `Console::key`: `key_lines[32]` with `edit_line` / `history_line`
  (Up walks back over the non-empty lines — at the oldest it stays, the slot
  after `edit_line`; Down forward, past the newest to an empty line; Enter
  echoes the line, even an empty one, and keeps it), Tab
  (`Cmd_CompleteCommand` then `Cvar_CompleteVariable` on the whole line,
  case-sensitive prefix, the name and a space: `"map "`) over this console's
  commands in the order id's `cmd_functions` list meets them (registered
  last, found first: `timedemo`, `playdemo`, `impulse`, `sizedown`, ... `echo`;
  then `wasm_help`) and its cvars (`viewsize`, the `wasm_*` extras), PgUp/PgDn
  (and the wheel keys) moving `con_backscroll` by 2 within `con_totallines -
  (vid.height>>3) - 1`, any print (`Con_Print`) and a new console width
  (`Con_CheckResize`) resetting it, `Con_DrawConsole` drawing that many lines
  up; only ASCII 32..126 types (`key >= 32 && key <= 127`), 254 characters at
  most (`MAXCMDLINE`). The scrollback keeps `con_totallines = CON_TEXTSIZE /
  con_linewidth` lines as id's ring does (431 at 320 wide, 138 at 960; was a
  flat 200). Home/End are Key_Console's too, but no console keys, so
  `Key_Event` runs their bindings (End: `centerview`) as in id. Tests
  `up_and_down_walk_the_32_line_history`, `tab_completes_a_command_then_a_cvar`,
  `pgup_and_pgdn_scroll_the_text_back`,
  `console_history_completion_and_backscroll_through_key_event`,
  `every_completion_is_a_command_the_console_knows`; `web/verify_extras.py`
  (Up Up and Tab through the page's keys). 2-D oracle, new scenario
  `console_scroll` (PgUp twice, then PgDn): 98.4 → 99.2% at 320x200 (738 →
  398 px, the version stamp left), 98.5 → 99.0 at 640x400, 98.6 → 98.9 at
  960x600.
- ✅ **Multiplayer > Setup did nothing** (MED). `M_Menu_Setup_f`,
  `M_Setup_Draw` and `M_Setup_Key` are ported (`MenuScreen::Setup`): the
  screen fills from the cvars when it opens (`hostname` "UNNAMED", `_cl_name`
  "player", `_cl_color` 0) on `setup_cursor` (a static starting at 4, Accept
  Changes); "Hostname" and "Your name" in 16-column text boxes with the text
  cursor (10/11) after the name on its row, "Shirt color", "Pants color",
  "Accept Changes" in its box, `gfx/bigbox.lmp` around `gfx/menuplyr.lmp`
  drawn through `M_BuildTranslationTable(top*16, bottom*16)` (the shirt rows
  16..31 and pants rows 96..111 taken from the chosen colour rows, backwards
  from row 128 on — `M_DrawTransPicTranslate`); Up/Down (menu1), Left/Right
  and Enter step the colours on their rows (menu3, wrapping 0..13), do
  nothing on the name rows, Backspace and any key 32..127 edit the names
  (15 characters), Accept sets what changed and returns to Multiplayer
  (m_entersound), Escape returns without. The cvars live with the menu's;
  the console reads and sets them as id's does: `name` / `color` (the
  client halves of `Host_Name_f` / `Host_Color_f`: print, or set — 15
  characters; each colour `& 15`, at most 13) and `hostname`, `_cl_name`,
  `_cl_color` (`Cvar_Command`), all in Tab completion. Not done: the name
  reaching the player's edict (`netname`, "player entered the game") — the
  port's server connects the player as "player" (`Server::connect_client_inner`,
  `server/`, another branch's), and `colormap` is never visible in single
  player (CENSUS: no chase camera; bodies are coop-only). 2-D oracle, new
  scenario `menu_setup` (on Accept Changes, the colours stepped, typing the
  host name, then the name): 100% at 320x200, 640x400 and 960x600, the
  translated preview included (before: Enter on Setup stayed on Multiplayer,
  77.3 / 93.6 / 97.1%). Tests `setup_is_m_setup_key`,
  `translation_table_is_m_buildtranslationtable`,
  `setup_draws_the_translated_player`,
  `setup_sets_the_name_and_colours_the_console_reads`; `web/verify_menu.py`
  (Setup opens, Escape returns).
- ✅ **The Web extras page's text ran under the plaque** (LOW; the port's
  own page). Its white header and help lines were centred across the 320
  columns, so they crossed `qplaque` (x 16..47). The page is now laid out as
  `M_Options_Draw` lays out Options: the rows from y=32, 8 px apart, labels
  right-justified from x=16, "on"/"off" at x=220, the cursor at x=200; under
  them, starting at x=64 (clear of the plaque, as Setup's labels), the white
  "Not in id's Quake" and the highlighted row's two help lines and its
  console variable, each at most 32 columns (two help lines shortened).
  Test `web_extras_screen_draws_in_the_options_idiom` (nothing but the plaque
  in its columns). Screenshots, 1440x900, the scaled 2-D extra on and off:
  `extras_{before,after}_scaled2d{1,0}.png` (not committed).
- ✅ **The canvas box banded the pixelated picture** (LOW; the page). The
  largest 4:3 box a 1440x900 window fits is 976 wide for the default 960
  columns: with `image-rendering: pixelated` one column in ~60 was doubled,
  which striped the menu's checkerboard fade and the 1:1 text; in windows
  narrower than the framebuffer whole columns of 1:1 text were dropped. The
  page's `fitCanvas()` now snaps the fitted box, in device pixels per
  framebuffer column `s`: at most 1/8 past a whole number `k`, the box is
  exactly `k` (960x720 at 1440x900; the picture gives up at most a ninth of
  its width); below 1, the fitted box drawn smooth (`image-rendering:
  auto`); otherwise the fitted box, pixelated as before (1216x912 at
  1920x1080: 1.27 per column spreads its doubled columns evenly, one in
  four, and smoothing would blur the whole 3-D view to hide it). It runs on
  load, resize (so zoom and devicePixelRatio), fullscreen and every
  resolution change. The rows cannot be made even: the 16:10 framebuffer
  is shown at 4:3, 1.2 rows per column's width, every fifth row a pixel
  taller. Measured over the Quit prompt's fade (screen columns that repeat
  their neighbour): 1440x900, 16 of 977 → 0; 1920x1080, 256 of 1217 (as
  before); 1024x768 no longer drops columns (smoothed). `web/verify_input.py`
  checks the three boxes. Screenshots `box_{before,after}_{1440x900,1920x1080}.png`
  and their zoomed crops (not committed).

## Final review fixes, engine side (2026-09-25, branch `quake/polish4a`)

The final review's engine findings (renderer, server, hardening). One commit
each; the C followed and the evidence are in the commit messages. Checks at
the end of the branch: goldens `4807aaa1` / `9ae2b478` / `c65b7046`
unchanged; the standard oracle rows (`--aspect 0.8333333 --spans 16`, world
and ents) 100.00; census, simbench (nine maps) and the `quaketool play`
hashes unchanged; 592 lib + 2 bin + 8 integration tests and 126 wasm, clippy
clean in both crates (and with `--features bench`); the nine `web/verify_*.py`
pass on the default wasm.

- ✅ **Dynamic lights on moved brush models are id's** (MED). The edge
  renderer moved each light into a brush model's frame (`origin - bm.origin`)
  before marking and lighting its faces. id does not: `R_DrawBEntitiesOnList`
  calls `R_MarkLights (&cl_dlights[k], 1<<k, clmodel->nodes +
  clmodel->hulls[0].firstclipnode)` with the light as it is, and
  `R_AddDynamicLights` measures `cl_dlights[lnum].origin` against the face's
  own plane and texinfo — the model's, where the map put it. So a lowered lift
  or an opened door is lit as if it had not moved. The old note called the
  shift deliberate ("arguably fixes a C quirk that mis-lights moved doors");
  under the rule the C wins. Oracle (`--c-cmd "impulse 9" --c-cmd +attack
  --modes ents --spans 16`, id's `cl_dlights` handed over): e1m6's start lift
  (lowered 184, the flash above it) looking down the shaft, `--settle 3
  --view=-64,672,100,89,270,0` 82.56 → 100.00 and from `-64,672,180` 94.87 →
  100.00; e1m8's door `*6` (opened 72 units) beside a grunt's flash,
  `--settle 20 --view=272,100,-60,30,270,0`, 99.72 → 99.98. The standard
  muzzle-flash rows (`+attack --settle 3`, world and ents, e1m1/2/3/7) and the
  goldens are unchanged (no moved model in reach). Test
  `a_moved_brush_model_is_lit_by_the_lights_where_they_are`.
- ✅ **No view larger than id's `MAXWIDTH` x `MAXHEIGHT`** (MED). A view
  2048 or more pixels wide panicked in release: the edge renderer's 12.20
  fixed-point u of the view's right edge, `(w << 20) + 0xFFFFF`, wraps an i32
  there, and `R_StepActiveU`'s push-back walked off the edge list
  (`quaketool view … --res 2048x400`, index out of bounds). id never has such
  a view: `r_shared.h` has `MAXWIDTH` 1280 and `MAXHEIGHT` 1024, which size
  its tables (`newedges[MAXHEIGHT]`, `d_scantable`), and `vid_win.c` /
  `vid_ext.c` list no larger mode. Now `render::MAXWIDTH`/`MAXHEIGHT` and
  `clamp_to_max`: `render_scene_ext_sprited` draws at most that size (the
  returned image says what it drew; a screen composed around a smaller view
  gets the backtile, no panic), `render_edges` refuses anything larger, and
  quaketool's `--res` (view, play, timedemo, `QUAKE_RES`) clamps with a note
  on stderr. The page already capped its modes at 1280x800. Tests
  `no_view_is_larger_than_id_maxwidth_by_maxheight`,
  `res_is_at_most_id_largest_mode`.
- ✅ **`setmodel` gives alias models and sprites id's box** (LOW, CENSUS
  L10). The port gave every `.mdl`/`.spr` a zero box (its comment said id
  did). `PF_setmodel` is `SetMinMaxSize (e, mod->mins, mod->maxs, true)` on
  the model `PF_precache_model` loaded (`sv.models[i] = Mod_ForName (s,
  true)`), and `Mod_LoadModel` gives an alias model ±16
  (`Mod_LoadAliasModel`, "FIXME: do this right") and a sprite
  `±maxwidth/2` across, `±maxheight/2` up (`Mod_LoadSpriteModel`). Precache
  now resolves every model file's bounds by its magic (IDPO, IDSP, else a
  brush model's submodel 0, as before for the b_*.bsp boxes). What QuakeC
  `setsize`s afterwards is unchanged; what it does not — an explosion's
  `s_explod.spr` (56x56: ±28, id's rocket explosion in `oracle_edicts` is
  ±28 too) — now links with id's box, so `SV_WriteEntitiesToClient`'s leaf
  test sends it where id would. Evidence: the edict dumps gained `mins`/`maxs`
  (`oracle_edicts`, `quaketool census-edicts`, `edict_diff.py --fields
  mins,maxs`): over nine maps × five times the port's boxes change only for
  the flames and torches L17 keeps as live edicts (±16, their static
  entity's box in id), and on e1m1–e1m3 every entity both sides have
  matches id's box. Census and simbench output unchanged. Test
  `alias_and_sprite_models_get_mod_load_model_bounds`.
- ✅ **Hardening against malformed maps** (no shareware map reaches these;
  id's C would crash or loop too). `SV_AddToFatPVS`'s one-sided descent is a
  loop, and only its recursion was depth-bounded, so a node tree that loops
  (a child pointing back up) never returned: `add_to_fat_pvs` now spends a
  `nodes + leafs` visit budget (a real tree reaches each once) in the loop
  too, as `touched_leafs` does (`fat_pvs_unions_the_leaves_within_8_units`
  gained a one-sided and a two-sided cycle). A world face whose plane index
  is past the plane lump had its edges emitted and then no surface posted,
  so `R_LeadingEdge` read a surface that does not exist (misdrawn, or an
  index panic when it was the frame's last): `R_RenderFace` now skips such a
  face whole (`a_face_with_a_bad_plane_index_is_skipped`: the frame equals
  one without the face). A span whose 1/z is exactly 0 at a clipped edge
  saturated `(sdivz * z) as i64` and the `+ sadjust` overflowed in a debug
  build; the adds wrap as the C's `int`s (release output unchanged: release
  already wrapped), and the clamps keep the texel in the block
  (`a_span_at_zero_1_over_z_wraps_like_the_c_int`).
- ✅ **Stale docs made true.** `draw_particles` said the port's buffer
  "holds depth" and recomputed `izi` from it; it is id's 16-bit
  `d_pzbuffer`, compared and stored as `D_DrawParticle` does.
  `render_scene_ext`'s doc described the polygon walker's order (brush
  entities first "to cut the world's spans", a depth buffer that makes the
  order irrelevant, the external boxes drawn "after the world") and a
  design note about a task option; it now describes `R_RenderView`'s
  passes over the edge renderer. The edge section above named a `ZBuf::Izi`
  type that does not exist (the buffer is `render::ZBUF`, `[i16]`). The
  `with_pak` docs (server and world model) now say what the pak resolves
  since the `setmodel` fix above.

## The engine's own mixer (2026-09-26, branch `q26/audio`)

id's `snd_dma.c`, `snd_mix.c` and `snd_mem.c` are ported into the engine as
`quake_rs::snd::Mixer` (`snd/dma.rs`, `mix.rs`, `mem.rs`): the channel table,
`S_StartSound` with `SND_PickChannel` and `SND_Spatialize`, `S_StaticSound`
and the combining of one sample's statics in `S_Update`, `S_StopSound`,
`S_StopAllSounds`, `S_LocalSound`, `S_UpdateAmbientSounds`, `S_Update_`'s
mix-ahead, `S_PaintChannels` with `SND_PaintChannelFrom8`/`16` and the scale
table, `S_TransferStereo16`, `GetWavinfo`, `S_LoadSound` + `ResampleSfx`, and
the cvars `volume`, `nosound`, `loadas8bit`, `ambient_level`, `ambient_fade`,
`_snd_mixahead`. It takes the client's `SoundCall`s and paints 16-bit stereo
PCM at the caller's rate. **The browser plays it**: the worker runs the mixer
and paints into a shared ring an AudioWorklet plays; the page's own Web Audio
mixing is gone (`web/PLATFORM.md`, "Sound"). Classic (`snd_modern 0`, `snd::SoundMode::Classic`)
is id's mixer at 11025 Hz, reconstructed at the device's rate by the worklet
as a sound card's DAC did; the 2026 default runs `Fixes::ALL` at the device's
rate. What the page's mixing got wrong goes with it: one-sample statics are
combined and their sum clamped as in `S_Update`, no cap on a frame's sounds,
`SND_PickChannel`'s 8 channels (a ninth sound takes the one nearest its end,
never the player's), the menu's clicks through `S_LocalSound`, and the level's
sound playing on under the menu and over `pause` as id's `S_Update` does.

- ✅ **Classic is id's, sample for sample.** `oracle/sound.py` runs id's C
  mixer (built headless, `oracle/build_sound.sh`) and the port on the same
  scripted calls at 11025/22050/44100/48000 Hz: 28 of 28 cases identical,
  PCM and per-update channel trace (oracle/README.md, "Sound").
- **Default-on departures** (`snd::Fixes::ALL`, the 2026 mixer; `Fixes::NONE`
  is Classic). Each repairs a fault in id's code and leaves the character (8-bit
  samples, point resampling, id's spatialization and attenuation):
  - *loop seam*: `SND_PaintChannelFrom8` always paints from `paintbuffer[0]`,
    so after a loop restart inside a paint pass the samples land over the
    pass's start and the rest of the pass gets nothing from that channel — a
    click on every lap of an ambient, torch or mover hum. Fixed: painted at
    their offset (Quake II's fix).
  - *exact resampling*: `ResampleSfx` steps in 8.8 fixed point; at 48 kHz that
    is 58/256 for 58.8/256, every sound 1.4% flat with its last 1.4% cut.
    Fixed: exact point-sampling steps. (Exact at 11025/22050/44100 either way.)
  - *ambient steps*: the int `master_vol` moves by `host_frametime *
    ambient_fade`, truncated: above ~100 fps the step is under 1 and the water
    and wind never fade in. Fixed: the ramp runs in 1/72 s steps (id's at its
    cap) at any frame rate.
  - *stop range*: `S_StopSound` searches channels 0..7 (four of them ambients)
    and misses the last four dynamic channels. Fixed: the eight dynamic ones.
- Kept as id's: the paint passes of 512 pairs, `rand()` offsetting a sample
  started twice in a frame (the MSVC runtime's generator, the mixer's own
  sequence), `S_StartSound` comparing that offset with `end` (a time), clipping
  at 16 bits after `volume`. Not modelled: `S_ClearBuffer` (the platform owns the
  output buffer), `GetSoundtime`'s chop after 2^30 pairs (`paintedtime` is 64-bit),
  `snd_show`, `soundlist`/`soundinfo`, `play`/`playvol`, CD audio.
  *Since, on this branch: `play` is `S_Play` (`Mixer::play`), and `S_ClearBuffer`
  reaches the platform (`Mixer::take_clear`, the PCM record's clear flag). CD
  audio came with `q26/content` (below).*
- Port-side notes: the client's temp-entity sounds use entity 0 where
  `CL_ParseTEnt` uses -1 (no audible difference: neither is the view entity
  and channel 0 never overrides); the live path hands `S_StartSound` the
  QuakeC volume, not the wire byte over 255 the C client sees.
- `quaketool sound <pak> <demo> <out.wav> [--classic] [--rate HZ]` renders a
  demo's sound through the mixer natively.

## The browser as a WASI program (2026-09-26, branch `q26/platform`)

The browser build became a plain program (`fn main`, stdin/stdout, `std::fs`)
in a Web Worker under `web/wasi.js`; `web/PLATFORM.md` has the design and
the measurements. What that moved that id's game has an opinion on:

- ✅ **Saves are files, as `Host_Savegame_f` writes them.** `save` writes
  `id1/<name>.sav` and prints "done." after the write, or the C's "ERROR:
  couldn't open." when it fails (the page's asynchronous "couldn't store"
  message is gone; a storage failure after the fact is `echo`ed by the page).
  `load` reads the file (`Host_Loadgame_f`). The Load and Save menus list the
  slots from `s0.sav`..`s11.sav` when they open (`M_ScanSaves` in
  `M_Menu_Load_f`/`M_Menu_Save_f`), in the program now. Tests in
  `savegame.rs`; `web/verify_save.py`.
- ✅ **`config.cfg`** (`Host_WriteConfiguration`, quake.rc's `exec
  config.cfg`). It archives what the page kept before: the video mode (the
  port's `_vid_resolution WxH`, where id archives a mode number), `viewsize`
  and the Web extras. Written when a value changes (a page is never told it
  quits), exec'd at startup before the attract loop, values unquoted (this
  console has no `COM_Parse`). Key bindings and the other Options cvars are
  not archived yet, as the page never kept them. Tests in `config.rs`.
  *Since `q26/settings` (below): `bind` lines and every archived cvar, read
  through `COM_Parse`, after a `profile` line.*
- ✅ **`play`** (`S_Play`): each named sample (`.wav` added without an
  extension) as a local sound; the page's sound button runs `play
  items/r_item1.wav`. Test `play_queues_each_named_sample_as_a_local_sound`.
- ✅ **`S_StopAllSounds` stops what the frame started before it, and only
  that.** A level change's stop now drops the one-shots and stops queued
  earlier in the same frame, and the page hears it before the new level's
  sounds, which play. The old page drained one-shots first and stopped every
  source at the generation change after them, so a sound the new level
  started in its first frame was cut. Test
  `stop_all_drops_the_sounds_queued_before_it_and_keeps_those_after`.
- ✅ **A level's placed loops are that level's.** With audio unlocked after
  the walk had booted, the old page started the attract demo's 66 loops over
  e1m1 (`verify_ambient.py`: "e1m1 static loops started 66"; e1m1 has 14);
  the new page keeps each level's loop records and starts those, once each
  (Firefox found them started twice).

## High resolutions and Hor+ (2026-09-26, branch `q26/hires`)

Two video cvars, `render::VideoCvars` (per-thread, like `d_mipscale`; `render::set_video_cvars`). *Since `q26/multicore` (R3) both are per frame, in the frame's `RenderOptions::video` and the client's `Vid::video`, and the settings set them from `vid_native` and `fov_adapt`.*
Both off is **Classic**, id's: views clamped to `MAXWIDTH`x`MAXHEIGHT`, `fov` across the view.

| cvar | what it changes when on | status |
|---|---|---|
| `hires` | views up to 7680x4320 (`HIRES_MAXWIDTH`/`HEIGHT`); particle size `izi * xscale / 20480` clamped to `[xscale/160, xscale/40]` instead of `izi >> d_pix_shift`; the underwater view rendered at the view rectangle, not the 320x200 warp buffer, with `D_WarpScreen`'s sine scaled by `sqrt(w*h/(320*200))` | departure, off here; meant to be on by default in the page |
| `fov_mode` `HorPlus` | `fov` is the horizontal field of view of a 4:3 screen of the same height; a wider SCREEN adds columns at the sides (16:9: 106.26 degrees for fov 90); 4:3 and narrower are Classic | departure, off here; meant to be on by default in the page |

- ✅ **Classic unchanged.** Goldens `4807aaa1` / `9ae2b478` / `c65b7046`; oracle
  (`--aspect 0.8333333 --spans 16`) 100.00% on the eight standard rows, entity pixels
  100%; `quaketool play demo1,demo2,walk_e1m1,fire_e1m1,quad_e1m1,walk_e1m3 --res
  320x200,640x400,1280x800 --hash-every 30` byte-identical to `3866e1b` (18 runs).
- ✅ **Past 2048 wide.** id's edge `u` is 12.20 in an `int`; the right edge `(w << 20) +
  0xFFFFF` wraps from 2048 wide. The port's `Edge::u`/`u_step` are 44.20 in an `i64`
  with id's values wherever the `int` holds them (an edge that is stepped spans two or more
  rows, so `|u_step| < w`). Every other table the C sizes by `MAXWIDTH`/`MAXHEIGHT` was
  already a run-time `Vec` (`newedges`, `removeedges`, `DPS_MAXSPANS`, the warp's `rowptr`
  and `column`, `intsintable`); `r_maxedges`/`r_maxsurfs`/`MAXSPANS` are growable, and demo1
  at 3840x2160 peaks at 1112 edges and 392 surfaces (id's pools: 2400, 800), about what it
  needs at 640x400 (950, 378). Texture, lightmap, sky and z fixed point are in texel or
  1/z units, not screen columns.
- ✅ **Hor+ details.** The screen, not the view rectangle, decides (id's own 320x152 view
  above the status bar is wider than 4:3). `r_fov_greater_than_90` tests the cvar, so the
  gun stays; `D_Sky_uv_To_st`'s `temp` and `r_aliastransition`'s `res_scale` use the 4:3
  view that Hor+ widens, so the sky does not slide against the walls and models change
  drawing path at the same distance. 1920x1080 Hor+ against 1440x1080 Classic on e1m2's start
  (sky in view): the middle 1440 columns match on 99.94% of the view's pixels (the rest are
  single pixels along edges, where the two views clip differently).
- **Particles.** id's `d_pix_shift = 8 - (int)(width/320 + 0.5)` halves the size once per
  320 columns while the width only grows by 320, so the size is right only at 320 and 640
  wide: 2x at 1280, 5x at 1920, and a negative (undefined) shift from 2720. With hires the size
  is id's at 320 and 640 and in proportion elsewhere; Classic keeps id's formula.
- **Underwater.** id's sine is 3 screen pixels over a 128-pixel cycle at every resolution,
  and the view comes from a buffer of at most 320x200. At 4K that is a 12x blow-up with a
  faint shimmer; with hires it is 320x200's wobble at full resolution (id's to the pixel at
  320x200).
- **Kept as they are (the look, or already fine at 4K).** The 16-pixel perspective spans:
  they differ from exact perspective on 8.84 / 1.20 / 0.20% of the pixels of an e1m1 view at
  320x200 / 1280x800 / 3840x2400, so affine swim fades as the resolution grows.
  Mip levels follow `xscale`, so 4K draws finer mips further out, as id's formula
  intends. The 16-texel lightmap blocks, dlights (per luxel), the 128x128 sky layers, the
  gun's affine texturing and palette banding are resolution-free and kept. The scaled 2-D
  layer's non-integer scale (5.4x at 1080p) makes 2-D pixels 5 or 6 wide: barely visible,
  left alone. Cracks and sparkles: the background (`r_clearcolor`) shows through on 0 / 2 /
  6 / 16 / 24 pixels in all 969 frames of demo1 at 640x400 / 1280x800 / 1920x1080 /
  2560x1600 / 3840x2160 (`quaketool timedemo --profile 1`), Hor+ or not: sub-pixel gaps in
  the maps (T-junctions) that a finer pixel grid samples more often, about one pixel every
  40 frames at 4K. id's; left alone.
- **Not measured:** the page (quake-wasm and web/ do not set the cvars yet), GPU browsers,
  phones.
- *Since `q26/settings`: the 2026 profile turns both on in the page (`vid_native` sets
  `hires`, `fov_adapt` sets Hor+); Classic keeps both off. The page's speed at these sizes
  is in `web/PLATFORM.md` ("Threads", "Presentation") and `PERF_PLAN.md` §11.*

## Demo playback between messages: id's CL_LerpPoint in Classic (2026-09-26, branch `q26/lerp`)

id's client draws demo playback between the two newest recorded messages in
every frame, at any frame rate: `CL_ReadFromServer` advances `cl.time` by the
host frame time, `CL_GetMessage` reads a message whenever `cl.time` has
passed the newest (`cl.time <= cl.mtime[0]`: "don't need another message
yet"), and `CL_RelinkEntities` puts the view entity, every entity,
`cl.velocity` and (in `cls.demoplayback`) `cl.viewangles` at
`CL_LerpPoint`'s fraction between `cl.mtime[1]` and `cl.mtime[0]`. The port
pre-interpolated each message interval into 60 Hz sub-frames and showed the
latest sub-frame due — 58 camera moves a second at 72 Hz, a sub-frame late,
and the monsters without id's `U_NOLERP` behaviour.

- ✅ **Fixed: Classic is id's, frame for frame.** The parser
  (`demo::parse_demo`) keeps one frame per message with the client state id's
  relink reads — each entity's `msg_origins[0..1]`, `msg_angles[0..1]` and
  the `forcelink` its update set, `cl.mtime[0..1]`, `cl.mviewangles[0..1]`,
  `cl.mvelocity[0..1]`, `cl.viewheight`, and whether the block ends the demo
  (`svc_disconnect`). `client::cl_demo::demo_frame` runs `CL_ReadFromServer`
  on them: the double clock, the read-ahead, `CL_LerpPoint` (the 0.1 s cap,
  the pull back to the interval past 1%, which also starts a demo 0.1 s
  before its first message) and `CL_RelinkEntities` (the >100-unit teleport
  test, which goes straight to the new place; angles the short way;
  `EF_ROTATE` at `anglemod(100*cl.time)`; `ent->forcelink` set by any update
  the frame read, cleared once drawn). Effects spawn when their message is
  read, at that `cl.time`; particles and the stair smoothing step by
  `cl.time - cl.oldtime` (the smoothing stepped by the host frame time). `oracle/demo_lerp.py` plays the attract loop from
  boot in id's client (the oracle, new `oracle_trace`) and the port at 72 Hz:
  demo1, demo2, demo3 and demo1 again, 17,500 frames — the same number of
  frames per demo, `cl.time` identical in every frame, the camera angles and
  origin, `cl.velocity` and every entity within 2.5e-4 (x87 float noise), the
  same entities in every frame.
- **Two id quirks this brings, kept:** a message's `U_NOLERP` entities (the
  `MOVETYPE_STEP` monsters) are drawn where it put them in the frame that
  reads it and lerp from the message before in the frames after, so a
  stepping monster jumps a message ahead for one frame and falls back
  (`CL_ParseUpdate` sets `ent->forcelink` for `U_NOLERP` without copying the
  history, and the relink clears it); and each demo opens with the camera
  turning from the previous block's recorded angles (the signon's, 0) over
  its first 0.1 s. Both are id's at 72 Hz.
- The frame that reads a demo's closing `svc_disconnect` draws the last
  frame's view at the new clock, as id's does before `Host_EndGame` leaves
  the frame; id draws it under the loading plaque (`CL_NextDemo`'s
  `SCR_BeginLoadingPlaque`), which the port does not draw (no loading plaque,
  on purpose: the Open list).
- Departure, invisible: id's first frame of a demo starts `cl.time` at 0
  (`CL_ClearState` runs after the frame's increment), the port's at the
  frame time; `CL_LerpPoint` moves either to 0.1 s before the first message.
- **Timedemo:** one message a frame at frac 1, as before (frame counts
  969 / 985 / 1090), through the same relink. Hashed with a fixed host frame
  time (the tool's is the wall clock, so its frames vary run to run), every
  frame of the three is identical to before except where the stair smoothing
  moves: id's `V_CalcRefdef` steps it by `steptime = cl.time - cl.oldtime`,
  in a timedemo a message interval, where the port stepped it by the host
  frame time. Fixed with the demo path; the view kick and the palette fades
  stay on `host_frametime`, as id's.
- `quaketool play demo1..3` hashes change (the frames are id's now); goldens
  and the walk workloads' hashes are unchanged, the sound tallies are the same.
  The uncapped path (`Stepping::Uncapped`) needs nothing of its own for
  demos any more: `quaketool framerate` measures 70.4 camera moves a second
  at 72 Hz (id's: every frame the recorded player moves) and 468 at 480 Hz.

**The 2026 extra on the same branch: `r_lerpmove`** (`client::lerpmove`,
`LerpMove::Classic` by default and in Classic; the settings work turns it on
in the 2026 profile). id's monsters step every 0.1 s (their thinks) and are
drawn where each step put them; with `LerpMove::Smooth` a step mover
(`MOVETYPE_STEP` live, `U_NOLERP` in a demo) glides from where it is drawn
to its new place over 0.1 s, or over one frame when the server moves it
every frame (airborne, pushed), turning the short way, and snaps on a new
sighting, a new model, a move over 100 units on an axis and a clock that goes
back. In a demo it is relinked where its message put it (QuakeSpasm's `f = 1`
for its step movers), so id's `U_NOLERP` jump goes too. Only where the model
is drawn changes (lights, sound and the box are the server's; no id1 monster
model has a trail flag), and animation frames are not blended. Departure, off in Classic; the goldens, the walk and
demo hashes and timedemo are unchanged by it. Measured in `FRAMERATE.md`
("Monsters between their steps"): at 240 Hz a walking grunt is drawn moving
in 99.8% of frames (Classic 4.2%), its largest move in a frame 0.17 units
(4.1), half a step behind the server on average.

## QuakeC errors end the game (2026-09-26, branch `q26/server`)

The client dropped the server frame's `Result` (`client/cl_main.rs:300`), and the server
isolated a failing think, touch, `blocked` or spawn function and carried on: a QuakeC
runtime error vanished and the game went on. id's `PR_RunError` (pr_exec.c) prints the
failing statement (`PR_PrintStatement`), a stack trace (`PR_StackTrace`) and the message,
then `Host_Error ("Program error")` (host.c) shuts the server down, disconnects, stops the
demo loop and drops to the console. Every profile now does that; it is id's, not an extra.

- ✅ **The report, to the column.** `vm/print.rs` ports `PR_PrintStatement` (id's
  `pr_opnames`, where the disassembler says `DIV_F`/`LOAD_F` id says `DIV`/`INDIRECT`),
  `PR_GlobalString`, `PR_ValueString`, `PR_StackTrace` and `ED_Print`, padding included;
  `a_program_error_is_pr_run_errors_report_and_halts_the_vm` pins the text.
- ✅ **The longjmp.** The first error halts the VM: the QuakeC running, and whatever
  called it through a builtin (a touch a `walkmove` fired), stops; the server frame or
  level load returns the error; the server runs no more QuakeC. The census, a harness,
  resumes the VM (`reset_execution`) and carries on, as before.
- ✅ **Host_Error in the client.** `client::host::host_error` prints the report and
  `Host_Error: Program error` to the console text and stops every sound
  (`CL_Disconnect`'s `S_StopAllSounds`); `walk_frame` shows the disconnected screen from
  then on and sets `Walk::host_error` for the host, which drops the walk, sets
  `cls.demonum = -1` and brings the console down (the shell's side:
  `finish_host_error`, wired at the merge, `d5db64a`). A changelevel, restart or `kill` whose QuakeC fails ends the game the
  same way; a missing or corrupt map still leaves the level running (the port's degrade).
- ✅ **`error` and `objerror` (CENSUS L16).** id prints `======SERVER ERROR in <function>:`
  (or `OBJECT ERROR`) and the text, dumps `self` (`ED_Print`), and calls `Host_Error`
  directly (no statement or trace); `objerror` frees `self` first. Both were `PR_RunError`s
  with a banner of the port's own that left `self` alive. The census's forced touch of
  start.bsp's unreachable teleporter now frees it: that map's census has 1 fault where it
  had 3 (two fewer touches, frames, teleport sounds and splashes); every other map's
  census is unchanged when run alone.
- **Not id's:** a QuakeC error while `build_walk_map` / `build_walk_savegame` bring up a
  NEW game returns `None` / the error text without id's report (the host prints "map not
  found" or the text); no shareware map raises one (census: 0 spawn errors).

## Settings and profiles: Classic and 2026 (2026-09-26, branch `q26/settings`)

Every departure from id's game is a setting (`quake_rs::cvar::CVARS`, marked
`departure`), and two profiles switch them: **Classic** (all off, id's
`default.cfg` bindings) and **2026** (the default in the page). Options' 14th
row is "Classic / 2026" (left/right switch it; Enter lists every setting),
the console has `profile classic|2026`, the page `?classic` / `?2026`. The
settings live in one typed value the host owns (`quake_rs::settings`), and
`config.cfg` keeps them the id way. What that changed against id's WinQuake:

- ✅ **Classic's controls are id's** (closes "Decisions, not work": the four
  control departures, and Always Run). `default.cfg`'s bindings: `a`
  `+lookup`, `d` `+moveup`, `c` `+movedown`, `w`/`s` unbound; `cl_forwardspeed`
  / `cl_backspeed` 200; `f` unbound (the page's fullscreen key is
  `vid_fkey`); `+jump` sets only `button2` (`cl_jumpswim` adds `upmove`); no
  mouse look without `+mlook` (`freelook`). 2026 turns all six on.
- ✅ **`+mlook` works** (`in_mlook`, cl_input.c/in_win.c): held (`\`, MOUSE3),
  mouse Y looks and stops the pitch drift, else it walks (`m_forward`);
  `lookstrafe` strafes only in mouse look; letting `+mlook` go with
  `lookspring` re-levels the view (`IN_MLookUp`). Before, mouse look was
  always on and `+mlook` did nothing. Test `ids_mouse_walks_and_mlook_held_looks`.
- ✅ **`crosshair`** (view.c): `V_RenderView`'s conchars `+`, its cell's corner
  at the view's centre, over the view and under the 2-D layer. Not drawn
  before; off in Classic as id's default, on in 2026.
- ✅ **`config.cfg` as `Host_WriteConfiguration` writes it**: `bind "KEY"
  "command"` lines (`Key_WriteBindings`) and `name "value"` archived cvars
  (`Cvar_WriteVariables`), read back through `Cmd_TokenizeString`/`COM_Parse`
  (quotes, `//` comments) and `Cbuf_Execute`'s `;` split. It keeps a
  `profile` line and only what differs from that profile's defaults (id's
  lists every value; its defaults never changed after release — the port's
  2026 defaults will, and a returning player should get them). A file from
  before the profiles runs without the lines that only restate that page's
  defaults. Tests in `config.rs`.
- ✅ **The console's commands are one table** (`Cmd_AddCommand`): dispatch, Tab
  completion and `wasm_help` read it. New from id: `bind` (`Key_Bind_f`: a
  key's binding, or `bind KEY "command"`), `unbind`, `unbindall`, `exec`,
  `togglemenu`, `toggleconsole`; a binding to any console line runs it on the
  key down (`+` lines run their `-` half on the key up); the cvars
  `cl_forwardspeed`, `cl_backspeed`, `m_pitch`, `lookspring`, `lookstrafe`,
  `sensitivity`, `gamma`, `volume`, `bgmvolume`, `crosshair`, and
  `d_mipscale` / `d_mipcap` (closes the Open item "The browser console has no
  `d_mipscale`/`d_mipcap`"). An unknown command prints id's `Unknown command
  "x"`. `stuffcmds`: the command line's `+` commands run after `config.cfg`.
- ✅ **`host_time` is a double** in the shell, as host.c's: the menu's
  spinning dot counts `(int)(host_time*10)` in double. The 2-D oracle
  harness hands the port the C's `realtime` and `host_time` as doubles too.
- **Options' port row** reads "Classic / 2026" with the profile at x=220 where
  it read "Web extras": the `screen2d` residue on Options is 531 px (was 291)
  at 320x200 and 640x400 (oracle/README.md). The C side of `screen2d.py` no
  longer gets the port's Always Run and WASD: the port runs Classic.
- **The scaled 2-D layer is at a whole scale** (`draw::screen_2d`): the
  largest whole multiple of 320x200 that fits (3.5x at 1120x700 is 3x, 1.5x
  at 480x300 1x), so every 2-D pixel is the same size. Only with
  `wasm_scaled2d`, off in Classic.
- **2026's picture** (`vid_native`): the page's box in device pixels over a
  whole pixel size (Auto: the smallest that keeps a frame within 1920x1080
  pixels), square pixels, the renderer's `hires` and Hor+ (`fov_adapt`), so
  the window is filled at its own aspect; `wasm_uncapped` runs the frame
  gate every refresh and steps the game with `Stepping::Uncapped`
  (FRAMERATE.md).
- **Proof of Classic:** `oracle/classic_check.py` (oracle/README.md,
  "Classic"): goldens `4807aaa1` / `9ae2b478` / `c65b7046`, `quaketool play`
  hashes and sound tallies (7 workloads x 3 sizes), timedemo frame counts,
  the census report and id's edicts diffed on nine maps all identical to
  `a50d8d7`; the oracle's eight rows as before; screen2d as before but the
  Options row above; the sound oracle 28/28.
- **`r_lerpmove`** (`lerp`'s smooth step movement) is a departure in
  `Cvars` (`LerpMove::Smooth` in 2026, `Classic` in Classic), on the settings
  page as "Smooth monsters". The merge of `q26/lerp` moved Classic's demo
  playback to id's `CL_LerpPoint`: `classic_expected.txt`'s demo frame hashes
  are re-recorded (the walks' and the demos' sound tallies unchanged), and
  `classic_check.py` runs `demo_lerp.py` (id's client, frame by frame, the
  whole attract loop: MATCH).
- **`snd_modern`** (`audio`'s 2026 mixer: `snd::Fixes::ALL` at the device's
  rate) is a departure in `Cvars` (`Cvars::sound`: `SoundMode::Modern` in
  2026, `Classic` — id's mixer at 11025 Hz — in Classic), on the settings page
  as "Full-rate sound"; the sound device reads it at every mix.
- **Not done / slots:** `input`'s raw mouse and gamepad become a departure in
  `Cvars` (on in `Cvars::modern`) when they land. *(They landed: the 2026 pad is
  departures, "Input" below; the raw mouse is id's feel, in both profiles.)*
  *(id's F-key shortcuts (F1–F12) landed too: fkeys, CENSUS L12 above.)*
  `messagemode` and the `zoom_in` alias are still not bound.

## Input: id's joystick, the 2026 gamepad, raw mouse, keys by place (2026-09-26, branch `q26/input`)

What changed against id's WinQuake, and what the 2026 profile adds. The
joystick is `quake-rs/src/client/in_win.rs` (the joystick half of in_win.c),
its host side `quake-wasm/src/input.rs`; the page polls the Gamepad API
(`web/PLATFORM.md`, "Input").

- ✅ **id's joystick** (in_win.c): `IN_StartupJoystick` (detection, `-nojoy`),
  `Joy_AdvancedUpdate_f` (`joyadvancedupdate`), `IN_Commands` (buttons as
  `JOY1`–`JOY4` then `AUX5`.., the hat as `AUX29`–`AUX32`) and `IN_JoyMove`,
  with every cvar at id's defaults: `joystick` (0: no pad is read, as id's),
  `joyname`, `joyadvanced`, `joyadvaxisx`..`v`, the four thresholds and
  sensitivities, `joywwhack1`/`2`. keys.c's `JOY`/`AUX` names bind. Before,
  a gamepad did nothing. Tests `in_win.rs`, `classic_reads_the_pad_only_with_joystick_1_as_ids_joystick`.
- **The pad as winmm saw it** is the port's choice (id's code saw whatever the
  driver reported): a standard Gamepad API pad reads as an Xbox 360 pad
  through winmm — X/Y the left stick, Z the triggers, R/U the right stick,
  the D-pad the hat — except that the triggers are also buttons 7 and 8 (the
  Gamepad API's order), so they can be bound. Table in `in_win.rs`.
- **Departures in Classic's joystick, kept small on purpose:**
  - `IN_Commands` keys the newest reading. id's keyed what `IN_JoyMove` read
    the host frame before (a frame later); with `joystick 0` both key nothing.
  - A pad that goes away, or `joystick 0`, lets every held pad key go (id's
    kept the last read: a held button stayed held).
  - The pad's turn and look are gated behind the menu and the console, as the
    port gates the mouse; id's `IN_JoyMove` turned the view behind the menu.
  - "joystick detected" prints when a pad first shows itself (a browser shows
    none until a button is pressed), not at startup; "joystick not found"
    never prints.
  - id archives only `joystick`; the port also keeps the layout cvars in
    `config.cfg` (departures are kept), so a player's layout lasts.
- **Kept, id's:** a diagonal on the hat keys nothing (`dwPOV` is compared
  with the four straight directions); an idle look axis with `lookspring 0`
  stops the pitch drift every frame ("the lookspring bug" workaround), so
  with a pad read, `centerview` is cancelled at once — in 2026 too, which is
  why the 2026 pad does not bind `centerview`.
- ✅ **The 2026 pad** (on by default): id's own advanced configuration as the
  layout — `joystick 1`, `joyadvanced 1`, X side, Y forward, R look, U turn,
  no thresholds, `joysidesensitivity 1` (strafe right is right),
  `joyyawsensitivity -1.75` (245°/s at full tilt) — plus the port's
  `joy_deadzone 0.2` (each stick's dead zone round, and rescaled from its
  edge), `joy_exponent 2` (the look curve), `joy_menukeys 1` (A Enter — `y` on
  a yes/no prompt — B and Start Escape, D-pad arrows, while the menu has the
  keyboard and no key is being bound; a key comes up as what it went down
  as), `joy_rumble 1` (damage: `V_ParseDamage`'s count; firing the super
  shotgun, grenade or rocket launcher, the lightning gun per bolt: the
  player's `punchangle` kick; on a phone without a pad in use,
  `navigator.vibrate` through the touch controls). Bindings (`Bindings::with_gamepad`): A and LT
  `+jump`, RT `+attack`, B and D-pad down `+movedown`, D-pad up `+moveup`,
  Y, RB and D-pad right `impulse 10`, LB and D-pad left `impulse 12`, Back
  `+showscores`, Start `togglemenu`, L3 `+speed`. Options' settings page:
  Gamepad (`joystick`) and Rumble (`joy_rumble`); the layout's cvars are
  the console's, as id's were. With `+strafe` held the right stick strafes
  the wrong way (id's one `joysidesensitivity` serves both a side axis and a
  turn axis that strafes); not fixed.
- ✅ **Keys by their place** (the page): letters are `KeyA`..`KeyZ` like the
  digits and punctuation already were — id's scancodes (`scantokey`, a US
  table). Before, letters followed the layout, so AZERTY's WASD was ZQSD in
  2026 and `default.cfg`'s `a`/`d`/`z`/`c` moved. What the layout types
  still goes to the console and the name fields (and AltGr now types there).
  Firefox's quick-find no longer opens on `/` (`impulse 10`) or `'`.
- ✅ **Raw mouse**: pointer lock asks for `unadjustedMovement` (Chromium's raw
  input), falling back to the plain lock where refused (Linux). id's
  `IN_StartupMouse` switched Windows' pointer acceleration off
  (`newmouseparms {0, 0, 1}`), so this is id's feel, in both profiles. Every
  count arrives (a coalesced event carries the sum of its samples) and the
  turn per count is the same at any frame rate
  (`mouse_turns_the_same_at_60_and_480_hz`).
- **Latency** (`web/latency.py`, PLATFORM.md "Input"): from an event's
  `timeStamp` to the canvas, 12–13 ms median at 60 Hz for a key, the mouse
  or the pad (half a refresh's wait, then a 4 ms frame); at an emulated
  240 Hz the pad, now polled just before each tick, 4.2 ms. `?lowlatency`
  stays opt-in (unverifiable headless, may tear).
- **Proof:** `oracle/classic_check.py` ALL PASS; `web/verify_gamepad.py`
  20/20 and the other page checks in headless Chromium and Firefox; no real
  pad or real 240/480 Hz display was tried.

## Touch, install and offline (2026-09-26, branch `q26/mobile`)

A phone plays the page with touch controls (`web/touch.js`), installs it to
the home screen and plays it offline (`web/sw.js`); `web/PLATFORM.md`,
"Touch" and "Offline and install", has the design. What that changed
against id's WinQuake:

- **`in_touch`** (`Cvars::touch`) is a departure, on in 2026, off in
  Classic: the touch controls for play. The settings page's last row,
  "Touch controls". `in_touchaccel` (look acceleration, console only,
  default 0) reads nothing in the game, so it is no departure. Classic on a
  touch screen keeps only a MENU button and the tappable menu, so a phone
  is never stranded; Classic on a desktop is untouched (touch.js is not
  even loaded).
- **The menu answers taps** (`Menu::tap` / `point` / `item_at`, quake-rs
  `menu.rs`, "Taps"): the port's input path, not id's. A tap becomes the
  key id's menu already takes (`M_Keydown` through `Key_Event`), so no
  screen does anything a key could not. Its rows are the draw code's: the
  list origins became named constants (`PIC_ROW_Y0`, `SLOT_ROW_Y0`,
  `KEYS_ROW_Y0`, `VIDEO_ROW_Y0`, ...) and the Options sliders one
  `options_slider` function, which the drawing now uses too — pixels
  unchanged (`screen2d` 146 values match; `classic_check.py` ALL PASS). A
  test draws every list and checks the cursor is where a tap finds it.
- **The State record** gains three flags: 128 `in_touch`, 256 the menu
  asks y/n (Quit, New Game's question), 512 the live game is paused.
- **Hidden page, 2026 with touch:** the live game pauses (`pause`, id's
  plaque and its "paused the game" line) under the menu, and unpauses when
  the player is back in the game. Classic only stops getting ticks, as
  every browser page does when hidden.
- `player_field NAME` (automation): a read-only call the checks read the
  player's edict through.

## The player's own Quake: search path, registered game, CD music (2026-09-26, branch `q26/content`)

The engine reads through id's search path (`quake_rs::common`, `pak.rs`), the
registered game works from a player's own `pak1.pak`, and the CD plays the
player's track files (`web/PLATFORM.md`, "Your files" and "CD music").

- ✅ **The search path** (`COM_InitFilesystem`, `COM_AddGameDirectory`,
  `COM_FindFile`): the game directory's loose files, then `pak0.pak`,
  `pak1.pak`, … in front until one is missing, the last searched first; a
  shareware game reads no loose file below the directory (`static_registered`).
  `COM_LoadPackFile`'s "not a packfile" and `com_modified` (count and CRC against
  339 / 32981), `COM_Path_f` (`path`). A `Pak` is one element with the rest of
  the path behind it, so no engine signature changed.
- ✅ **`COM_CheckRegistered`**: `gfx/pop.lmp` against `pop[]` (big-endian),
  "Playing shareware/registered version." (and "Added packfile …") on the
  console at startup, "Corrupted data file." and "You must have the registered
  version to use modified games" as the program's exit. `cvar("registered")` is
  the path's answer on every server (it was always 0): `trigger_onlyregistered`
  opens start's episode gates, and e1m7's `ExitIntermission` takes the
  registered finale text and goes on instead of the sell screen.
- ✅ **CD audio** (was "not modelled"): `cd_win.c`'s `CDAudio_Play`/`Stop`/
  `Pause`/`Resume`, `CD_f` (`cd`) and the end-of-track notify
  (`quake_rs::cd_audio`), asked for where id's client asked: `svc_cdtrack` at every
  signon (`sv.edicts->v.sounds`), the QuakeC's `SVC_CDTRACK` (now parsed, not
  skipped), a demo's `cls.forcetrack` from its header line (demo1: 2), a demo's
  and `pause`'s `svc_setpause`. Without the player's music there is no drive
  (`cd_null.c`, as the C oracle): nothing changes, and `oracle/classic_check.py`
  passes unchanged (the play tally and census report leave the CD's calls out).
- **Decisions.** The level is DOS `cd_audio.c`'s (`(int)(bgmvolume*255)`,
  `bgmvolume` held to 0..1 while there is a drive), not WinQuake's: MCI could not
  set a CD's level, so `cd_win.c`'s `CDAudio_Update` snapped `bgmvolume` to 0 or
  1 on any change (pausing or resuming the disc). The port's `registered` is
  read-only on the console (id's could be set: in shareware that only opened
  gates to maps it lacks). The port refuses at startup a `progs.dat` whose
  builtins id's engine never had (past `pr_builtin[]`, or `#0` by name as the
  2021 re-release's), where id's would run until the first call.
  `PR_LoadProgs`'s `PROGHEADER_CRC` check (5927) is made once at startup.
- **Checked against the C, not found:** the brief's "id hides the ordering screen
  when registered" — WinQuake's (and QuakeWorld's) `M_Menu_Help_f` always pages
  through `gfx/help0..5.lmp` (`NUM_HELP_PAGES` 6); only the DOS/Linux quit
  screens differ by `registered`. What a registered pak changes there it changes
  through the path (a `pak1.pak` lump overrides `pak0.pak`'s). `menu.c`'s other
  `registered` use is the multiplayer game options' episode count (7 vs 2),
  a menu the port does not have.
- **Not modelled:** `-path`, `-cachedir`, `proghack`; `cmdline` (set to
  `com_cmdline` when registered); the CD's `MCI_NOTIFY_FAILURE`/eject door;
  `CDAudio_Play`'s "Bad track number" (a developer print). A track the player
  has no file for, within the disc's range, is to the game a data track
  ("CDAudio: track N is not audio"). `-game`/`-rogue`/`-hipnotic` are now
  ported — see "The mission packs' own file layout and progs" below.

## Video Options honesty, and the menu fade dither at a big 2-D scale (branch `fleet/video`)

Two "look" problems the closing review found playing the 2026 default in a browser
(the "Open" list above, 2026-09-26): Video Options lying about the picture, and a
"screen-door" fade dither.

- ✅ **Video Options is honest about native resolution** (quake-rs `menu.rs`). The
  bug: in 2026, native resolution's actual size rarely matches any of
  `RESOLUTION_PRESETS` exactly, so `Menu::sync_resolution`'s `(w, h) ==
  preset` match never fired and the list kept showing whatever preset it
  last matched — in practice the boot default, 960x600 — as "current" even
  though the screen was something else entirely; and Enter on any mode
  silently set `vid_native 0` with no way back short of leaving the screen
  for Options > Classic / 2026. Fixed, in the 2026 profile only (`Menu::
  native_rows_shown`, the host's `modern = profile == Modern`; off in
  Classic, or if `Menu::sync_resolution` is never told `modern` at all —
  `VID_MenuDraw`'s plain grid, untouched): `NATIVE_ROWS` (Auto, then pixel
  sizes 1..=4) are **appended after** `RESOLUTION_PRESETS`, not prepended —
  every existing preset keeps its row index, so this is purely additive.
  While the picture genuinely is native (`Menu::actual_native`, the host's
  `vid::native` — a window must be known too, not just `vid_native`'s cvar),
  no preset is marked current; the matching native row is, and it prints
  the actual render size (`Menu::actual_size`) instead of a guess. Enter on
  a preset still turns native off exactly as id's `VID_SetMode` always did
  (visibly now: the screen was already showing no preset as current, so one
  lighting up is the visible change); Enter on a native row turns it back on
  at that pixel size. Same list, same Up/Down/Enter — the way back is never
  more than an arrow away. `MenuAction::ResolutionChanged` now branches on
  `cvars.native` (quake-wasm `menu.rs`/`vid.rs`): a native pick recomputes
  the picture from the window and pixel size (`vid::apply_settings`) instead
  of reallocating to a stored mode that doesn't exist for it.
  Tests: `quake-rs/src/menu.rs` — `video_rows_are_the_presets_alone_until_
  2026_native_rows_sync`, `video_options_opens_honest_when_the_picture_is_
  native`, `picking_a_fixed_mode_is_reversible_back_to_native`,
  `a_native_row_sets_its_own_pixel_size`, `classic_video_options_ignores_
  native_rows_even_if_native_is_on`, `video_options_marks_the_live_native_
  row_white_with_its_real_size`. `web/verify_settings.py` opens Video
  Options in 2026, checks the native row is current and prints the real
  size, picks a fixed mode, and picks native back. Classic: `screen2d`'s
  `menu_options.video` shot is unchanged (native rows never draw there;
  `oracle/classic_expected.txt`'s 95.49/98.80 — the port's own mode list vs
  id's grid, a pre-existing, documented difference — holds exactly).
- **The fade dither at a big "scaled 2-D" scale: already fixed, not a
  regression.** Checked directly (not just read): `fade_screen` (`draw.rs`)
  already dithers on `screen_2d`'s own scale, not the framebuffer's raw
  pixels — it has since `bf35315` (2026-09-25, "the scaled layout becomes an
  opt-in extra"), a day before the review that still listed this. A probe
  at 1920x1080 (scale 5, squarely in the review's "4-6x") confirmed whole
  5x5 blocks, no 1-pixel screen-door; a new test pins it down exactly:
  `fade_screen_is_whole_blocks_not_a_screen_door_at_a_native_4_to_6x_scale`
  asserts every dither cell is one whole `scale x scale` block at that
  scene (alongside the existing `fade_screen_matches_the_per_pixel_dither_
  at_any_scale`, which already covered the general formula across sizes
  `bf35315` never exercised at exactly review-sized windows). Likeliest
  explanation: the review played a deployed build that predated that day's
  later merges. No production code changed for this half of the brief.

Screenshots (this branch's scratch dir, named in its report): Video Options
in 2026 and Classic, before (native 1108c3e) and after this branch; a menu
over the game at a 4-6x 2-D scale, before and after (unchanged, proving the
dither was never touched).

## The mission packs' own file layout and progs (2026-10-02, branch `fleet/mission`)

Scourge of Armagon (`hipnotic`) and Dissolution of Eternity (`rogue`) playable the way
WinQuake plays them with `-hipnotic`/`-rogue`: their own game directory beside `id1`,
their own re-release `progs.dat` allowed to load, and the status bar drawing their own
weapons and items. `mission_paks.py` (in a work directory) builds each pack's
own `pak0.pak` from the 2021 re-release with English map messages (`unlocalize_bsp`, the
same trick `make_paks.py` used for id1's registered `pak1.pak`).

- ✅ **The search path** (`quake-rs` `common.rs`): `init_filesystem` takes `mod_dirs`
  (id's order — `-rogue`'s `rogue`, then `-hipnotic`'s `hipnotic`, then `-game`'s own
  directory) and `force_modified` (`-game`'s own `com_modified = true`, unconditional);
  each directory layers over the ones before it with the same `Pak::over` chain id1
  alone always used, so `com_gamedir` (saves, `config.cfg`) becomes whichever was added
  last. `quake-wasm`'s `main.rs` parses `-rogue`/`-hipnotic`/`-game <dir>` from `argv`
  exactly where it already parsed `-basedir`; `index.html`'s `commandLine()` turns
  `?game=hipnotic`/`?game=rogue` into them. Tested in `quake-rs/src/common.rs`
  (`mod_dirs_layer_over_id1_in_ids_order_and_the_last_becomes_com_gamedir`, a synthetic
  id1 + rogue + a plain `-game` directory, checked against `path_lines`).
- ✅ **The eager builtins refusal is gone; the lazy one (already there) is now the
  only one.** `check_progs` no longer scans every declared function for a builtin
  number past the port's table — id's own engine never did either; it only notices at
  the actual call (`PR_RunError`), which `vm.rs`'s `OP_CALLn` dispatch already
  implements correctly and unchanged. The mission packs' re-release `progs.dat` declare
  `finaleFinished` (#79) and `localsound` (#80), past the port's 79-entry table, but
  never call either (`quaketool dis`, grepped for both names outside their own
  declaration line) — that eager scan was refusing a game id's own engine would have
  run without incident. Tested: `common.rs`'s
  `a_progs_that_only_declares_foreign_builtins_is_not_refused_at_startup`; `vm.rs`'s
  `calling_an_unknown_builtin_errors_at_the_call_not_at_load` proves the lazy path a
  progs that *does* call one would hit.
- ✅ **The status bar** (`sbar.rs`): `Server::mode` (`GameMode::Id1`/`Hipnotic`/`Rogue`)
  is detected once at construction from the loaded `progs.dat` itself — Rogue declares
  the field `ammo_lava_nails` (its `give` cheat), Hipnotic the function
  `EmpathyShieldsCheat` (dead in the shipped game, but still declared) — rather than
  threaded from the command line through every `map`/`changelevel`/`restart`, so every
  caller that builds a `Server` (the browser, `quaketool`, the tests) gets it for free.
  `Hud::mode` draws what `sbar.c` draws under `hipnotic`/`rogue`: Hipnotic's four
  weapons (laser cannon, mjolnir, the grenade-launcher/proximity-gun combo slot) and two
  items (wetsuit, empathy shields, which displace its two keys to the main strip);
  Rogue's inventory-bar background swap, five tier-2 weapon icons (drawn over the
  standard loop's slot when active), two items (shield, anti-grav belt, at the same slot
  Hipnotic's use and id1's sigils otherwise occupy), and its remapped armour-type and
  ammo-type bits (standard's armour-bit positions are Rogue's own tier-2-weapon bits).
  One faithfully-kept oddity: `sbar.c`'s wetsuit/shields check (`1<<(24+i)`) does not
  match `quakedef.h`'s own `HIT_WETSUIT`/`HIT_EMPATHY_SHIELDS` (`1<<25`/`1<<26`, one bit
  higher) — id's own mismatch between the `#define` and the code that was supposed to
  use it, ported as the engine (what this module ports) actually checks it. Tested:
  `sbar.rs`'s `mission_pack_item_bits_match_quakedef_h`,
  `flashon_for_cycles_like_weapon_flashon_at_a_different_bit_and_slot`,
  `hipnotic_weapons_and_items_draw_only_in_hipnotic_mode`,
  `rogue_remaps_the_armour_bits_and_draws_its_tier2_weapon_icon`.
- **`menu.c`'s ~75 hipnotic/rogue references**: read every one. All are the
  multiplayer game-options screen (`M_GameOptions_Draw`/`M_NetStart_Change`: episode and
  level lists, the teamplay mode count, a CTF team-colour border) — a menu the port does
  not have (no multiplayer). Nothing applies; nothing ported.
- **`host_cmd.c`'s nine references (`Host_Give_f`)**: not ported. The port's own `give`
  console command was already a simplified stand-in for id's letter+digit scheme even
  for id1 (a plain digit 1-8 selects a weapon, not id's letters for ammo plus a digit for
  weapons) — extending it to Rogue's per-letter field remapping (new fields
  `ammo_shells1`/`ammo_nails1`/`ammo_lava_nails`/`ammo_rockets1`/`ammo_multi_rockets`/
  `ammo_cells1`/`ammo_plasma`) and Hipnotic's digit branch risked more than a debug cheat
  is worth under this round's time. Every new item is still obtainable by picking it up
  in-level.
- **Not compared against id's C**: the status bar's hipnotic/rogue draws, and whether
  `r2m6`'s edict overflow (below) also happens in the real engine. `oracle/build.sh`'s
  WinQuake (the same tree the port is ported from, with `-hipnotic`/`-rogue` support
  bolted on by id) is missing at least one cvar the real Rogue engine had: a `map r2m6`
  run under `-rogue` prints `Cvar_Set: variable campaign not found` over 20,000 times in
  60 seconds and never finishes (no `grep -rn campaign quake-c/WinQuake` hit at all) —
  the real, never-open-sourced Rogue Entertainment engine evidently registered a
  `campaign` cvar (likely its deathmatch-tournament bookkeeping) this public source
  never got. Hipnotic's oracle run (`-hipnotic +map hip1m1`, full composited screen) DID
  complete cleanly (one benign `'fog' is not a field` warning, matching id's own
  tolerance of a QuakeC field it doesn't have), but a quick, unsynchronized capture
  against `quaketool shot` landed on two different camera positions (the oracle's own
  frame was taken before its client-side signon settled: `vieworg [0,0,0]`,
  `con_current` still the full screen height) — a real pixel comparison needs
  `screen2d.py`'s own scripted multi-frame sequence extended to accept `-hipnotic`/a
  custom pak, which this round did not do.
- **`r2m6` (Rogue) cannot be spawned by this port**: `plat2_spawn_inside_trigger():
  ED_Alloc: no free edicts` — `quaketool sim`/`census` both hit it on bare entity spawn,
  reproduced identically with the shareware id1 and with the full 2021 re-release id1
  merged in (so not a missing-registered-asset artifact). `MAX_EDICTS` is 600 in this
  source for id1 too (`quakedef.h`'s own "FIXME: ouch! ouch! ouch!"); whether the real
  Rogue engine raised it, or this map's `plat2` count is a genuine overflow nobody hit at
  a lower skill, is open — the oracle can't settle it (previous bullet). Every other map
  of both packs spawns clean (`quaketool census`, 18 hipnotic + 15 of 16 rogue single-
  player maps — `rogue`'s own deathmatch-only maps, `ctf1`/`b_lnail*`/`b_mrock*`/
  `b_plas*`, spawn too, untested for actual deathmatch play, which the port has none
  of); `hip1m1`→`hip1m2` and `r1m1`→`r1m2` play start to exit with inventory carried
  across (`quaketool changelevel`). *Since `fleet/makestatic` it spawns, under id's 600:
  the overflow was the port's own kept statics ("`makestatic` frees its edict").*
- **The browser**: `?game=hipnotic`/`?game=rogue` is wired through to `-hipnotic`/
  `-rogue` and, checked live (headless Chromium, no hipnotic data present), fails
  exactly as id's own engine would — silently plays plain shareware, since an empty
  `hipnotic/` game directory contributes nothing to the search path — not a crash or a
  hang. ~~The page's own file layer is unchanged and still id1-only~~ — done, branch
  `fleet/mission-page`, below.
- ✅ **The page's file layer, the mission packs' own game directory, and the game
  picker** (branch `fleet/mission-page`): `index.html`'s drop handler recognises a
  dropped `hipnotic/pak0.pak`/`rogue/pak0.pak` (and their own `music/trackNN.ogg`) by
  the nearest `GAME_DIRS` name in the dropped path itself (`dropGameDir` — a bare file,
  or id1's own un-prefixed `music/…` the way Steam and GOG actually ship it, still means
  `id1`, exactly as before); `isPakPath` and `planServerFiles` generalise the same way
  for `files.json`. A mission pack's own files are fetched — from a server's manifest or
  kept in IndexedDB — only for the game `?game=` is actually starting: never for id1,
  and never the other pack's (AUDIT and the brief's own wording: "and never requests
  them for id1"); id1's own extra paks (`pak1`…) still layer under every game, since the
  search path always does. `files.json`'s manifest and `web/isolated.py`'s
  `write_manifest` generalise past `id1/pak1.pak` the same way (`GAME_DIRS`), excepting
  only `id1/pak0.pak` itself (always the deploy's own baseline fetch). The CD's kept
  tracks move from a bare `<track>` IndexedDB key to `"<game dir>:<track>"`
  (`storage.music*`) — id1, hipnotic and rogue each shipped their own soundtrack, so
  without this a mission pack's track 2 would silently overwrite id1's kept one;
  `storage.migrateMusicKeys()` moves an existing bare-numbered key under `"id1:"` once, so
  a kept CD rip from before this round still plays. The start overlay's picker
  (`renderGamePicker`, `#gamePicker`) offers id1 plus every mission pack a manifest or
  the player's own files actually have (`packsAvailable`) as plain `?game=` links (a
  reload, not a live switch — "the dumb way"), plus whichever game is already selected
  even if its data turns out missing, so there is always a way back to id1.
  Tested: live, against the real 2021 re-release packs (`mission_paks.py`, a scratch
  deploy) — `?game=hipnotic`/`?game=rogue` each fetch only their own pak (plus id1's
  pak1) and no 404s, the network log shows zero requests touching the other two
  directories while on the third, and `hip1m1`/`r1m1` load and render (screenshots in the
  branch's report) with each pack's own status bar; a dropped (not server-offered)
  `hipnotic/pak0.pak` on a plain id1 deploy is found by path, listed, offered by the
  picker, and reached by the engine on the next `?game=hipnotic` load with no network
  request for it at all. `web/verify_content.py` gained a synthesized mission-pack case
  (a fake `hipnotic/pak0.pak` holding `maps/hip1m1.bsp` — the existing `e1m1.bsp`-as-
  `e2m1.bsp` trick, not real mission-pack data) for permanent regression coverage; the
  existing id1-only checks in it and `verify_touch`'s offline step were re-run unchanged
  and still pass. Not verified: the picker's visuals on a phone-size viewport, and
  `verify_touch`'s touch/menu steps with a mission pack selected (only its unchanged
  id1-only path was re-run).

## hip1m1's start door: not reproduced (2026-10-02, branch `fleet/startdoor`)

The user reported, playing Scourge of Armagon on a phone, that in the room the player
starts the door did not move, yet they could walk through it. hip1m1's start room has a
`trigger_once` (x −289..−111, y 367..433, z −105..−39, found at edict 226) targeting a
`trigger_relay` (1 s delay) that opens five `func_door` pieces (`t7`/`t2` at once, `t4`
+1.5 s, `t6`/`t8` +2.5 s; `spawnflags` 2052 = `DOOR_DONT_LINK` + not-in-deathmatch, `wait
-1`). This round tried hard to reproduce it and could not, on real hipnotic data
(a local deploy dir, `deploy/{id1,hipnotic}`),
three independent ways:

- **A native Rust harness** (`Server::with_pak` directly, no client/renderer): teleported
  the player into the trigger (the real `setorigin` builtin, #2, movetype left alone —
  unlike a noclip "fly to" cheat, id's `SV_Physics_Noclip` links with
  `touch_triggers=false`, so a noclip teleport alone never fires a touch trigger), then
  stepped `client_frame_f64` at a fixed 0.1 s. All five doors move on the brief's own
  schedule, `solid` stays `SOLID_BSP` (4) throughout (collision follows the moved
  entity), and `Server::entities_sent_to_client()` keeps every door in the client's
  visible-entity set the whole time — so `cl_main.rs`'s brush-entity loop (reads `origin`
  fresh from the edict every frame, gated only on that `is_relinked`/`sent` flag) has
  correct, live input every frame. Kept as `quake-rs/tests/hip1m1_start_door.rs`
  (`#[ignore]`d, `QUAKE_HIP1M1_PAK` — mirrors `pr_edict.rs`'s `QUAKE_R2M6_DIR`): closed
  doors block the player, the first two pieces open on the relay's own ~1 s delay, all
  five are open and still solid by t=6 s, and the player is free to proceed once they are.
- **A browser/wasm harness** (Playwright driving the real production automation calls —
  `quake-wasm/src/automation.rs`'s `exec`, `setpos`-style `setorigin`, `step`,
  `frame_hash`, plus throwaway `dbg_*` calls added and then reverted for this
  investigation) against the real `wasm32-wasip1-threads` build, `?game=hipnotic`,
  `map hip1m1`: with the camera actually facing the doors (the first attempt stared at a
  blank wall and wrongly looked like a frozen frame — a methodology trap worth naming
  since it is easy to fall into again), `frame_hash()` — the program's own last-rendered
  frame, independent of canvas/rAF — changes every tick the door's `origin` changes, in
  both the 2026 and Classic profiles; screenshots at t=1 s and t=5 s show the door
  genuinely closed, then genuinely open onto the room beyond. A forced run straight at
  the still-closed cluster gets physically blocked (position pinned for half a second)
  before any piece has moved, then proceeds once they open. True natural play (the real
  `info_player_start`, no teleport) also reaches the trigger and the same sequence plays
  out.
- **id's own C oracle**, run on the real data for the first time with its own game
  directory: `census/oracle_run.py` gained `--hipnotic-pak`/`--rogue-pak` (sets up
  `<base>/hipnotic/pak0.pak` or `rogue/pak0.pak` and passes `-hipnotic`/`-rogue` — stock
  `COM_InitFilesystem` already knows those flags, nothing in `oracle/c` needed to
  change) and `--id1-pak1` (`COM_CheckRegistered` gates `-hipnotic`/`-rogue` on
  `gfx/pop.lmp`, which only the registered pak carries). An idle census at t=0/2/4/6 s
  confirms id's C spawns the same five closed doors at the same spawn geometry the port
  computes (absmin/absmax match to the hull-expansion rounding already documented
  elsewhere in this file); a full triggered run needs a scripted walk through hip1m1's
  actual (unmapped-by-this-round) corridor, which this round did not build — the static
  baseline is as far as this extension went. The `--hipnotic-pak`/`--rogue-pak`/
  `--id1-pak1` flags themselves are worth keeping either way: AUDIT's own "mission packs"
  section above named this exact gap ("extend the harness to take a game directory if it
  doesn't yet").

**Not explained.** Every suspect the brief named was checked and came back faithful:
the client's visible-entity list (no stale cache — reads live), the search path (one
bsp, one set of `*N` indices, server and client alike), `DOOR_DONT_LINK` (each piece
triggers independently, matching the relay's five separate `use` targets), the 2026
extras (`r_lerpmove`/`r_lerpmodels` are client-side monster/alias-model smoothing,
untouched by brush-submodel rendering, which reads `origin` raw every frame; the
Classic profile reproduces the same open sequence). A persistent-renderer regression
(`render::world::tests::a_moved_submodel_redraws_on_a_renderer_reused_across_frames`,
quake-rs/src/render/world.rs) pins down the one structural risk this round worried about
most — the real client reuses one `Renderer` every frame, unlike `render_once`'s
fresh-renderer-per-call tests above it — and confirms a submodel's moved origin redraws
correctly on a stationary camera, at any thread count. This leaves the report
unreproduced on this build: either it predates a fix already on `main` (the mission-pack
round landed hours before this one), or it needs the exact phone/touch
conditions this round's keyboard-driven automation does not cover. If it recurs, the
`hip1m1_start_door.rs` test and the oracle flags above are the fastest way back in.

**Resolved (chair, the same day).** The user's door was not hip1m1's: Scourge of Armagon
has its own `maps/start.bsp`, whose start room faces a *rotating* door (`rotate_object`
`*65` at (816 −256 160), swung by `func_rotate_door` "damndoor", its collision 22
`func_movewall`s, opened by the floor plate `*77`). The server swings the movewalls away
(the player walks through) while the renderer drew `*65` at its spawn angles — the
"brush models do not rotate" line of the Renderer list, closed by `fleet/rotate` below.
The investigation above stands for hip1m1, and its oracle flags and tests are kept.
The second half — the door's collision walls were never solid in the port — is Round 2, next.

## Round 2: the real map, found and fixed — `SV_ClipToLinks` re-read a brush entity's own `model` string instead of `modelindex` (2026-10-02, branch `fleet/startdoor`)

The user meant Scourge of Armagon's actual `maps/start.bsp` (hipnotic's own, not
hip1m1): the exit is a rotating door — a `rotate_object` (`*65`, purely visual, always
`SOLID_NOT`) swung by a `func_rotate_door` controller (targetname `damndoor`), whose
REAL collision is 30 `func_movewall` entities sharing targetname `t2`, opened by a
floor-plate `func_button`. `review-rotate`'s own harness found the plate registers but
`*65`'s angles never move, in id's C or the port — true, and faithful (below); it is not
what the user hit.

- ✅ **Root cause, found by disassembling hipnotic's own QC** (`quaketool dis`'s raw
  output needed a quick annotator — offsets resolved to field/global names and their
  actual stored float, since a disassembly's bare operand numbers collide constantly
  with unrelated immediates at the same global cell; not kept, it did its job). The
  chain: `func_movewall`'s spawn (`hiprot.qc`) calls `setmodel(self, self.model)` —
  attaching the real hull and bounds — then, unconditionally for these movewalls
  (`spawnflags` 2048), does `self.model = "";`: a common, legitimate QuakeC idiom (don't
  leak the submodel name once `setmodel` has used it). `self.modelindex`, a SEPARATE
  field `setmodel` also sets, is untouched. id's real `SV_HullForEntity` (`world.c`)
  resolves a `SOLID_BSP` entity's hull as `sv.models[(int)ent->v.modelindex]` — an
  integer, set once, never the live `model` string. `quake-rs/src/server/sv_world.rs`'s
  `sv_move` (`SV_ClipToLinks`) instead re-parsed the entity's *current* `model` field
  (`"*N"` → submodel `N`) on every single clip. The instant `func_movewall` blanked it —
  at spawn, before the player ever approaches — the entity became silently uncollidable:
  `solid` stayed `SOLID_BSP` (4), `absmin`/`absmax` stayed correct (from the real
  `setmodel` bounds), but `sv_move`'s `None => continue` on a model string with no `*N`
  dropped it from every trace. A closed, "solid"-looking door the player walked straight
  through — reproduced natively: a real, untouched walk from `start.bsp`'s own
  `info_player_start` (816 −704 216, +Y) sails to y=+559.97 with the movewalls
  (y −353..−235) never slowing it at all.
- ✅ **Fixed**: `Host` (`quake-rs/src/vm.rs`) gains `model_name(idx) -> Option<&str>`,
  the reverse of `precache_model`/`find_model` — the precached name at a model index, an
  immutable table QuakeC cannot touch (`server/mod.rs`'s `WorldModel` backs it with the
  same `precache_models` table `model_names()` already exposed, concretely, for callers
  that aren't behind the `Host` trait object). `sv_world.rs`'s `Solid::Bsp` branch now
  resolves the submodel from `host.model_name(ent.modelindex)`, never `ent.model`. No
  other collision path read the live field this way (checked every `fo().model` and
  by-name `"model"` read in `server/`); the client's own renderer (`cl_main.rs`) reads
  `model` by DESIGN each frame for a completely different reason (routing an entity to
  the inline-submodel vs. external-bsp vs. alias-model draw path, not resolving a hull)
  and was never part of this bug.
- ✅ **Proven against id's own C oracle.** `census/oracle_run.py`'s new
  `--hipnotic-pak`/`--id1-pak1` (round 1, above) plus its existing `oracle_walk`
  (`oracle/c/walk_oracle.c`, newly landed on `main` from `fleet/telesound` this same day)
  script a REAL walk — `cl.viewangles`, `+forward`, the ordinary `SV_Physics_Client` path
  — through id's actual WinQuake. One caveat worth recording: `oracle_run.py` always
  appends `oracle_quit` to the end of its script, which fires before `oracle_walk`'s own
  queued frames ever run (the walk only drives through the engine's NORMAL per-frame
  loop after the command buffer empties, same as any other script's trailing `wait`s) —
  this round drove the oracle binary directly for the walk, bypassing that; the
  `oracle_run.py` wrapper itself needs a small change (skip the auto-quit when a script's
  last command is `oracle_walk`) to make this combination usable from it directly, not
  done here. A real, untouched walk from `start.bsp`'s own spawn blocks at **y=−337.83**
  in id's C (`oracle_sndlog`'s per-frame origin log, 30+ identical consecutive values)
  and at **y=−368.03** in this port, post-fix (`server::client_frame_f64`, fixed 0.1 s
  steps) — both well short of the movewalls' own span, both agreeing the door blocks.
  Before the fix the port's own walk reached y=+559.97, clean through. The ~30-unit gap
  between the two blocked positions is not resolved — likely the id C walk's real
  wall-clock `host_frametime` vs. this harness's fixed 0.1 s steps giving a different
  exact approach, not re-examined.
- **Tested**: `quake-rs/src/server/sv_world.rs`'s
  `sv_move_stops_at_solid_bsp_entity_even_once_its_model_field_is_cleared` — synthetic,
  fast, no data dependency: a `SOLID_BSP` edict whose `model` field is blanked right
  after a real `setmodel("*1")` (so `modelindex` resolves through the precache table
  exactly as it would in play) must still block a trace; reverting the fix makes it fail
  with `fraction=1` (checked). `quake-rs/tests/start_bsp_rotating_door.rs`
  (`#[ignore]`d, `QUAKE_HIP1M1_PAK`) is the full real-data regression: every movewall
  starts closed, solid, and with a blanked `model` field (so the synthetic test's setup
  is not a strawman); the real player, walking from the real spawn, is blocked well short
  of the door; triggering the controller's own `use` (the plate's effect) moves at least
  one movewall off its closed origin, still solid throughout.
- **Not explained, and faithful either way**: `*65`'s own angles never changing from a
  single plate-touch — `RotateTargets`' rotate_object branch does copy the controller's
  live angles onto it every tick it's called, so this is NOT a dead link; it matches id's
  C too (`review-rotate`'s finding, not re-litigated here), so whatever makes a single
  touch insufficient to visibly swing `*65` is either the map's own design (`wait 4`, a
  button spawnflag, a second required trigger) or a genuinely separate QC question, out
  of this round's scope — the collision bug was the one the user actually hit
  (walking through the door), and it is fixed.
- **Not verified**: whether the fixed movewalls' own group-reversal physics
  (`movewall_blocked` → `rotate_door_group_reversedirection`, now reachable at all since
  these entities finally collide with each other and the world) plays out identically to
  id's C over the FULL open sequence — this round's native test only confirms at least
  one movewall moves and stays solid by t≈5s post-trigger, not an exact id-C-matching
  trajectory for the whole group; scripting the oracle walk all the way to the plate
  itself (behind the now-correctly-blocking door, reached by a side path this round did
  not map) would close that gap.

## The mission packs' paths (2026-10-02, branch `fleet/packclass`)

The question: rotating brush models were never ported because nothing in id1 rotates
(`rotate` ported `R_RotateBmodel` this round). What else did id's engine do that the port
skipped, stubbed or simplified because the shareware and registered id1 never reach it,
and that Scourge of Armagon (`hipnotic`) or Dissolution of Eternity (`rogue`) do reach?

**How it was looked for.** Statically: every "not modelled / id1 never / none do / stub /
no-op / simplification" in both crates and the Open list, crossed with what the packs'
`progs.dat` and data use that id1's do not (`census/qcsym.py` over the three progs:
builtins and their arities, `Write*` sequences per function, the values stored into
`effects`, `solid`, `movetype`, `weapon`, `items2`, `avelocity`; `cvar`/`cvar_set`/
`localcmd`/`stuffcmd` arguments; every model's flags, sync type, skin and frame groups,
every sprite's type; the maps' special entities). Dynamically, against id's C:
`census/packs.py` (new: id's server under `-hipnotic`/`-rogue` against the port's, live
edicts at t = 1.7 / 4.7 / 10.7 s, on all 35 levels of both packs), `oracle_move` (new: one
`SV_Move` in id's hull code), the 3-D oracle on 64 views (each level's spawn view and its
first `info_intermission` camera, entities, particles and dynamic lights handed over),
`screen2d.py --game` (new: each pack's status bar, 21 shots at each of 320x200 and
640x400), and `quaketool census` (the full scripted playthrough) on every level. One
caveat runs
through all of it: the packs exist here only as the 2021 re-release's files, whose
`progs.dat` was rebuilt for the re-release's own engine, and id's WinQuake cannot really
run it either (P3, P6, P7).

Each item: the feature, where the port diverges, what a player sees, how sure, and what
was done. Struck items are fixed on this branch.

- **P1 ~~`items2`~~** (fixed, `4f7237f`). `SV_WriteClientdataToMessage` sends `items |
  items2 << 23` for a progs that declares `items2` (`GetEdictFieldValue`) and `items |
  serverflags << 28` (the runes) only for one that does not; both packs declare it.
  `client/cl_main.rs` `server_items` always mixed in the runes: Hipnotic's wetsuit and
  empathy shields and Rogue's armour type, ammo-type highlight, power shield and
  anti-grav belt never reached the bar, and in Rogue a held rune lit the multi-rocket,
  shield, belt and superhealth bits. Certain: `screen2d.py --game` shows the bars 96-98%
  (the wrong icons) before and 100.00% after, every shot but the Tab bar's clock (27 px:
  id's signon of a bigger map takes one more 0.1 s frame, the Open list's connect-time
  item). id1's progs has no `items2`; nothing changes there.
- **P2 ~~A point on a slanted clip plane~~** (fixed, `ca2ab37`). `world.rs`
  `plane_distance`: id's x87 registers evaluate `DotProduct (normal, p) - dist` almost
  exactly; the port's `f32` rounding put points lying on 45-degree planes on the other
  side. Three items differ in the packs: hip1m1's shells (1184 -160 -176) and hip2m6's
  health (1032 8 176) stay where id's `droptofloor` finds solid and removes them ("Bonus
  item fell out of level"), hip3m1's rockets (-224 16 -448) go where id's keeps them.
  Certain (`oracle_move` at each point; Python id-style descents of the clip hulls); all
  three match after. id1's identity values move only in `play.fire_e1m1` (re-recorded,
  `oracle/classic_expected.txt` says why); every id-anchored check is unchanged.
- **P3 `svc_achievement` (52)** (kept, named, `b8a16a6`). The re-release progs write
  `WriteByte 52` + a string when a monster kills another (`Killed`, `MSG_ALL`), at every
  secret (`multi_trigger`, `MSG_ONE`), at a pack's end. id's client Host_Errors on it
  ("Illegible server message"): the C oracle ends hip1m1 the moment its player walks into
  the first `trigger_secret`. The port skips the command and its string. A deliberate
  departure from id's C (whose engine cannot play these progs), in both profiles.
- **P4 ~~`makestatic` keeps its edict~~** (CENSUS L17; fixed on `fleet/makestatic`:
  "`makestatic` frees its edict", below). `server/pr_cmds.rs:325`, `vm.rs:1093`. id's `PF_makestatic` writes `svc_spawnstatic` into the signon and frees
  the edict, which the next spawn reuses at once (`freetime` < 2). The port keeps every
  static alive: 0-106 more live edicts per pack level (torches, flames, candles,
  lanterns, `func_illusionary`), and Rogue's `r2m6` overflows 600 in Classic ("ED_Alloc:
  no free edicts") where id's C spawns it at 546 — the port's 632 are 541 + 91 statics.
  `sv_max_edicts` (2026) hid this; the fleet/edicts finding that `r2m6` needs more than
  id's 600 was the port's own count. Also: `quaketool census`'s stress run (every
  monster gibbed at once) hits ED_Alloc on seven pack levels (hip1m4, hip2m2, r1m4,
  r1m7, r2m4, r2m5, r2m7) with the kept statics eating the margin; whether id's would
  overflow too under that stress is not known. Certain about the statics.
- **P5 ~~Oriented sprites~~** (fixed, `fleet/sprites` `d049e21`: id's `r_sprite.c`/`d_sprite.c` ported, every type; was: open, brief B2). `render/sprite.rs:30` draws every sprite as a
  camera-facing billboard. Hipnotic's bullet holes are `progs/s_bullet.spr`, an
  `SPR_ORIENTED` sprite (type 3, never in id1) placed on the wall by `placebullethole`
  for every shotgun or super-shotgun pellet that hits the world (up to 10, for 300 s):
  id's draws them lying on the wall, the port as squares facing the player that stand
  out of it and turn with the view. Certain: the 3-D oracle on hip1m1 after `+attack`,
  seen along the wall (id's thin slivers; the port's billboards, cut by the wall).
  `s_blood1.spr` (type 3, `wallsprite`) is placed by no map.
- **P6 ~~The re-release's strings are localization keys~~** (fixed, `fleet/strings` `a64a0a9`: `localization.rs`, the pack's `loc_english.txt` in its pak; was: open, brief B3). 136 of
  Hipnotic's string immediates and 190 of Rogue's in `sprint`/`bprint`/`centerprint`/
  `dprint`/`WriteString` are `$qc_...` keys, and the formatted ones take arguments the
  re-release's engine substitutes (`sprint(other, "$qc_got_item", self.netname)` with
  `qc_got_item = "You got {0}\n"`, `qc_double_shotgun = "the Double-barrelled
  Shotgun"`). id's `PF_VarString` and the port's (`builtins.rs:57`) concatenate: the
  player reads
  `$qc_got_item$qc_double_shotgun`, `$qc_enteredplayer`, and `$qc_finale_hip1` as the
  episode's closing text. Same in id's C. `mission_paks.py` put the maps' `$map_` keys
  back in English; the progs' cannot be, because of `{0}`.
- **P7 The end of each pack** (open: brief B4). Hipnotic's `ExitIntermission` on hipend
  and Rogue's `finale_5`/`finale_check` poll `finaleFinished()` (builtin #79) before
  `finale_transition` runs `localcmd("menu_credits\n")` and `"disconnect\n"`. id's
  engine and the port have no #79: `PR_RunError` ("bad builtin call number",
  `vm.rs:1735`), the game ends with a QuakeC error at each pack's very end. By reading
  the QuakeC and the dispatch; not reached dynamically.
- **P8 `cvar_set("campaign")` every frame** (kept). Both packs' `StartFrame` set a cvar
  the re-release's engine has; id's `Cvar_Set` prints "Cvar_Set: variable campaign not
  found" every frame (its console and notify lines fill with it: the "hang" an earlier
  run reported). The port's `cvar_set` (`server/pr_cmds.rs:256`) ignores names it does not
  keep. A harmless departure; `screen2d.py --game` hides id's notify lines for it.
- **P9 `cvar()` of a client cvar** (open, small). `server/pr_cmds.rs:239` `cvar_value`
  answers only the server's own few; everything else is 0. Hipnotic's `worldspawn`
  turns its footstep sounds on when `cvar("crosshair") == 2` (`footsteps`): in id's a
  player who sets `crosshair 2` hears them, in the port never. `sv_cheats`, `campaign`,
  `gamecfg` are 0 in both (id's has no such cvars, or a 0 default).
- **P10 `MSG_ONE`** (covered). `server/msg.rs:354` models only `MSG_BROADCAST` and
  `MSG_ALL` ("the id1 progs never write" the others). The packs write `MSG_ONE` for
  achievements (P3) and Hipnotic's hipend camera (`UpdateCamera`: a hand-built
  `svc_updateentity` origin update for the player); the port's client reads the edicts
  directly, so nothing is lost.
- **P11 A QuakeC `svc_updatestat`** (covered). Hipnotic's spawners (`spawn_use`,
  `Gremlin_Split`) and Rogue's (`tbaby_checknew`, `morph_wake1`) write `WriteByte 3,
  STAT_TOTALMONSTERS, WriteLong total_monsters`; the port's parser drops it, and its
  status bar reads `total_monsters` live (`client/cl_main.rs:1223`): the same number.
- **P12 The active weapon under `-hipnotic`/`-rogue`** (noted). With `standard_quake`
  off id's server sends the index of `weapon`'s lowest set bit and the client puts `1<<i`
  back; the port uses `weapon` itself. Equal for every single-bit weapon; a `weapon` of 0
  writes no byte at all in id's (its message goes out of step), which no pack code was
  seen to do.
- **P13 Temp entities id1 never sends** (noted). `TE_EXPLOSION2` (Rogue's multi-grenades,
  plasma, lava balls) and `TE_BEAM` (Rogue's grappling hook, deathmatch) are parsed and
  drawn; `beam.mdl` is in Rogue's pak. `particles.rs:427` draws the explosion's six
  random numbers per particle in a different order from id's (positions, then
  velocities; id's interleaves them per axis), but the client's particle generator is the
  port's own sequence anyway, so nothing visible follows. Not compared with id's C (no
  pack demo).
- **P14 What `-hipnotic`/`-rogue` change in id's engine** (checked). `standard_quake`
  (P12, and `MINIMUM_MEMORY_LEVELPAK`), `sbar.c`, `menu.c` and `Host_Give_f` (Open list),
  nothing else; `SV_PushMove` does not turn a pusher by `avelocity` in WinQuake and the
  port does not either (Hipnotic's rotations are think-driven). Both packs call `precache_*`
  only at spawn, pass `sound` attenuations in range, and use id1's movetypes.
- **P15 The 3-D views** (checked). 64 views over 33 levels (`r2m6` excepted: P4 stops the
  port's `view` spawning it): every one at 98.45% or better at `--spans 16`. What is
  below 99.9% is explained: exact axis-aligned views put a texel boundary or
  `D_MipLevelForScale`'s threshold on a knife edge between id's x87 and the port's `f32`
  (start, r1m7: one degree of yaw gives 99.98-99.997%); a random `func_counter` flashing
  hip2m2's lightning style 32 (the view harness cannot share id's `rand()`); the flames of
  wall torches and fires on r1m2/hip2m1 one frame apart (~0.1%, the Open list's
  `ST_RAND` syncbase). Rogue's skin-group models (`sphere.mdl`, `p_shield.mdl`,
  `timecore.mdl`) were in none of the views: not compared.
- **P16 Hipnotic's start map** (the `rotate` agent's 93.3-93.5%). Two causes, neither in a
  pack path: `oracle/rotate_check.py` runs id's renderer with its default
  `D_DrawSpans8`, where the port draws `D_DrawSpans16` (`oracle_spans 16`: 97.85%); and
  the spawn view is the knife edge of P15 — the side walls 160 units away at fov 90 put
  their nearest visible point exactly at mip 0's threshold (`nearzi * xscale` = 1.0), and
  yaw exactly 90 lines the floor's seams up with the rays (`d_mipscale 0` on both: the
  walls match; yaw 91: 99.997%).
- **P17 The edicts sweep** (checked). With P1-P2 in, what still differs is: the statics
  (P4), monsters' random idle frames and wandering, things monsters start at random
  (doors in r1m4/r1m7/r2m7 opened by a monster in their field, Hipnotic's scourge
  triggers, random `func_counter`s, bubbles, lava balls), and hip2m4's rotating door
  starting 0.2 s early (its trigger is under the player, who connects at 1.2 s, id's at
  1.4: the Open list's connect-time item). id's console under the packs prints only
  P8's lines and "'fog' / 'alpha' / 'property 1' is not a field" (the re-release's
  worldspawn and entity keys; the port ignores them silently, as it ignores the
  re-release's `.lit` files: same picture).
- **P18 `sprint`/`centerprint` to a non-client** (noted, not pack-specific).
  `server/msg.rs:233`/`250` print to the player whoever the QuakeC names; id's prints
  "tried to sprint to a non-client" and nothing else. Hipnotic's `counter_use`
  centerprints to its `activator`, which a monster can be.
- **P19 `checkclient`** (noted): CENSUS L19's line-of-sight stand-in for the client PVS
  serves 9 call sites in Hipnotic (1 in id1): its monsters wake by it more often.

**For follow-up agents** (each Classic-relevant unless said; prove against id's C):

- ✅ **B1 (P4): `makestatic` frees its edict** (done, `fleet/makestatic`). Snapshot what
  `svc_spawnstatic` carries (model, frame, colormap, skin, origin, angles) into a
  server-side signon list, `ED_Free` the edict, and draw statics from that list in
  `client/cl_main.rs` (today the static
  path keys off `Vm::is_static_edict` in the edict loop, ~628-720) and in
  `quake-wasm/src/cl_walk.rs` (837, 922). Keep the floats or take id's wire bytes (the
  Open list's statics item) — say which. Savegames: id's saves no statics (the map's
  respawn rebuilds them); check the port's load does the same. Expect every edict number
  after the first static to move, so the Classic identities (`census`, `edicts`, maybe
  `play`) move toward id's numbering: re-record with notes. Proof: `census/packs.py`'s
  "only in port" rows vanish; `r2m6` spawns in Classic at id's 546; the `edicts` check's
  diffs shrink on id1. `sv_max_edicts` then stays as a 2026 extra for maps past 600.
- **B2 (P5): `R_DrawSprite` for every sprite type.** Port `r_sprite.c`
  (`R_SetupAndDrawSprite`: the four orientations, `R_ClipSpriteFace` against the
  frustum) and `d_sprite.c` (`D_SpriteDrawSpans`, the scan-edge walkers, the
  gradients), so a sprite is a projected, perspective-textured, z-tested polygon, not a
  rectangle. It changes `SPR_VP_PARALLEL` (id1's explosions and bubbles) too: compare
  with the oracle (`compare.py --modes ents` on a view with `s_explod.spr`; the `.ents`
  carry sprites), re-record `play`'s demo hashes if they move. Hipnotic's bullet holes
  (`+attack` on hip1m1, as P5) are the type-3 proof.
- **B3 (P6): the re-release's strings.** A small table loaded from
  `localization/loc_english.txt` on the search path (let `mission_paks.py` put the
  re-release's file into each pack's `pak0.pak`); `var_string` substitutes `{0}`, `{1}`
  in a `$key`'s text with the following arguments (each looked up when it is itself a
  key) and otherwise concatenates as now; `svc_finale`/`svc_cutscene` texts and
  centerprints look their key up at the client. Decide and say whether it is a 2026
  extra or both profiles (id1's progs has no `$` strings, so Classic's proof does not
  move either way); Ironwail's `LOC_Format` is the usual reading of the format.
- **B4 (P7): the end of a pack.** Builtin #79 `finaleFinished` (true once the finale
  text is fully out and the player presses a key, so `finale_check` moves on) and the
  two `localcmd`s it leads to (`menu_credits`: the port's own end screen or the main
  menu; `disconnect`: `cl_disconnect`). Prove by playing hipend's and r2m8's endings
  through `quaketool census` or a scripted walk to their `ExitIntermission`.

**Tools added** (committed): `census/packs.py`; `quaketool census`/`census-edicts` take a
layered `a.pak,b.pak,c.pak`; `oracle_move` in `oracle/c/oracle.c`; `screen2d.py --game
hipnotic|rogue --data DIR` with each pack's status-bar scenarios (the port's harness
boots `$QUAKE_SCREEN_BASEDIR`/`$QUAKE_SCREEN_GAME`).

**Not verified**: the end-of-pack flow (P7) dynamically; Rogue's skin groups; Hipnotic's
`func_clock`, `effect_finale` camera and hipend cutscene against id's C; sound; any pack
level in a browser. The 3-D sweep's views are static ones: entities in motion, monsters'
attacks and Rogue's own monsters' models in action were not compared.

## `makestatic` frees its edict (2026-10-02, branch `fleet/makestatic`)

AUDIT P4, CENSUS L17. id's `PF_makestatic` writes `svc_spawnstatic` (model, frame,
colormap, skin, origin, angles) into `sv.signon` and `ED_Free`s the edict; the next
`ED_Alloc` takes the slot at once (`freetime` < 2 during the load). The port kept every
static alive behind a flag on the VM, so every edict number after a map's first static
was off from id's, a mission-pack level ran up to 106 more live edicts than id's, and
Rogue's `r2m6` overflowed id's 600 in Classic.

**What changed.**
- `bi_makestatic` (`server/pr_cmds.rs`) records a `StaticEntity` (`server/msg.rs`) and
  frees the edict. The record is what id's client read back: the frame and skin bytes,
  and the origin and angles through `MSG_WriteCoord`/`MSG_WriteAngle` (`wire_coord`,
  `wire_angle`; the client's old `net_angle` moved beside them). No colormap:
  `CL_ParseStatic` gives every static `vid.colormap`.
- The server owns the list for the level (`Server::statics`), filled from the outbox
  after each QuakeC window, the way the light styles are. The client draws statics from
  it (`cl_main.rs` `static_desc`: `CL_ParseStatic` and `R_AddEfrags`), not from the edict
  loop. `Vm`'s `edict_static`, `is_static_edict` and `make_static` are gone.
- `quaketool scene` draws the statics after the live edicts: the goldens are unchanged.
- **The wire bytes, not the floats.** Classic is id's, and id's client drew the bytes;
  demo playback already did. On the shareware maps it changes nothing: every static has
  angles 0 and whole-unit origins. The packs' statics have whole-unit origins, and a
  few have a yaw that is no multiple of 1.40625 degrees (150, 250, -2: torches and small
  flames), which now draws as id's did, up to 1.4 degrees off the map's. 2026 gains
  nothing from the floats, so both profiles share one path.
- **Savegames.** A save writes the freed slots as empty blocks, so it holds no statics,
  as id's. A load re-runs the map's spawn functions (`Host_Loadgame_f`'s
  `SV_SpawnServer`), which rebuild the list before the save's edicts are read
  (`a_save_holds_no_statics_and_its_load_rebuilds_them`, which also checks that a new
  save's load leaves every slot as saved).
- **The port's own older saves** (the user's, in IndexedDB) hold each static as a live
  edict, which would draw on top of the respawned static. The load migrates them
  (`save.rs` `free_statics_an_old_save_kept`): a loaded edict whose `svc_spawnstatic`
  would be exactly one the spawn wrote, `SOLID_NOT` and with no `think`, is freed as a
  newer save's `{}` block. No save id's engine or this port now writes holds one.
  `an_old_save_s_kept_statics_are_freed_on_load` crafts such a save on the start map:
  its load has the new save's live edicts and frame, 9 alias models drawn (14 without
  the migration).

**Proof.**
- `classic_check`: ALL PASS. `census` and `edicts` re-recorded with a note: the census
  report changes only in edict numbers and its makestatic line's wording. The edict diffs
  of the nine maps shrink from 1,185 rows to 606. Of the entities they match, 469 were
  numbered other than id's minus one (CENSUS L25's player offset); 30 still are, all
  random fireballs and bubbles. Every map's live count equals id's, random spawns aside.
  goldens, play (all 42), timedemo, oracle, screen2d, demolerp and sound are unchanged.
  The play hashes do cover statics: with them hidden, 9 of `walk_e1m3`'s 22 change.
- `census/packs.py`, all 35 pack levels: no "only in port" statics left. The rows that
  remain (two classless thinkers on hip3m1 at 10.7 s, a fireball on hip3m2) were there
  before too, things monsters and random numbers start. The live counts at
  t = 10.7 s equal id's on 31 levels; the 4 others (hip1m1, hip2m4, hip3m1, hip3m2) differ
  by things monsters or random numbers start (P17). `r2m6` now compares: C 545 / 542 /
  542 live edicts at t = 1.7 / 4.7 / 10.7 s, the port 542 / 542 / 542 (id's three extra at
  1.7 are zombie gibs).
- `r2m6_spawns_under_ids_600_edicts` (ignored, real data): Classic's ceiling, 91 statics,
  541 live edicts after the entities load.
- `makestatic_records_the_wire_static_and_frees_the_edict`: the bytes, the wire origin
  and angles, the edict free, and the next spawn takes its slot.

**`sv_max_edicts`** stays a 2026 extra: 600 in Classic, 8192 in 2026, as room for maps
past 600. No map of id1 or either pack needs it. Its docs and console help say so.

**Left open.**
- A `makestatic` after the client's signon would draw at once. id's client only sees it
  at its next signon. No progs does this: every `makestatic` in id1, Hipnotic and Rogue
  is in a spawn function (`census/qcsym.py ... calls makestatic`).
- `CL_ParseStatic`'s `MAX_STATIC_ENTITIES` (128, "Too many static entities") is not
  modelled. No map checked reaches it: `r1m1`'s 106 statics are the packs' most, and
  the shareware maps have at most 44 (`e1m3`).

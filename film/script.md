# The narration

The film's narration as the cut speaks it, in order. Each line has its id (the voice
clip's name), its words, and under it, in an HTML comment, where the repository says what
it says. Lines without a comment are the film's own: questions, jokes, links between
beats.

The edit reads this file too (`film/edit.toml`, `paths.script`): a line's `## N.` section
heading says which section of the shot list it belongs to. Section 4.10 is spoken after
4.12; the numbers are the shot list's.

The voice is "Frederick" on ElevenLabs (`eleven_v4`, seed 7), in British spelling. In the
text he was given, id is spelled "Idd", so that it is said as a word, not as two letters.
Line ids carry the script version that wrote them: `V3-` lines are the third script's;
the `V4-`, `V6-` and `V7-` lines were rewritten or added in later versions.

## 1. Cold open

*(no voice: e1m1's first corridor, as 1996 drew it, with NARRATED BY AI at the foot of the screen)*

**V3-01** · In nineteen ninety-six, id Software released Quake.
<!-- README.md: "id Software's *Quake* (1996)" -->

**V3-02** · It changed how a generation saw three-dimensional space.

**V3-03** · It went online. People rebuilt it, raced through it, and competed in it.

**V3-04** · Underneath ran a monument of applied mathematics.

**V3-05** · A software renderer: binary space partitions, a potentially visible set, a surface cache, fixed point.
<!-- README.md: "the edge-sorted software renderer" -->

**V3-06** · Every pixel, one of two hundred and fifty-six colours.
<!-- README.md: "8-bit pixels and a 256-colour palette" -->

**V3-07** · And one division, rationed.
<!-- FRAMERATE.md, "The perspective span" -->

*(no voice: the title)*

## 2. The idea

**V3-08** · This is quake-srp: the slop rust port. Quake, ported to Rust by AI agents.
<!-- README.md: the title; "How it was built" -->

**V3-09** · By default, it is the same renderer, given a twenty twenty-six machine.
<!-- README.md, opening -->

**V3-10** · Each departure from id is a named setting: a slop option.
<!-- AUDIT.md, "The slop options and the presets" -->

**V4-11** · Switch them all off, and you get Classic: id's game.
<!-- README.md, opening: "With every extra switched off it is id's game, checked against id's own C ... What is known to differ still is a list (AUDIT.md, "Open")". AUDIT.md: "**Classic** has every slop option off ... It is WinQuake". -->
<!-- Not "id's game, exactly": AUDIT.md's Open list still holds Classic behaviours that differ (cl.idealpitch, Host_Give_f, ...). -->

## 3. The proof

**V3-12** · How do you prove a port is faithful?

**V3-13** · Build id's own C to run without a screen: the oracle.
<!-- oracle/README.md, opening -->

**V3-14** · Give both the same camera, the same clock, the same monsters, and compare the frames, pixel by pixel.
<!-- oracle/README.md, opening -->

**V3-15** · Subtract one frame from the other, and this is what's left.
<!-- compare.py's diff panel (oracle/README.md, "Reading the output"), full screen: black wherever the two frames agree. -->

*(silence: the screen is black, and so is the sound)*

**V7-16** · No, your video hasn't frozen. That's the difference: on these views, monsters and all, every pixel matches.
<!-- README.md: "every pixel of the 3-D view in every view tried, monsters awake". oracle/README.md: compare.py's `ents` mode, the port drawing id's entity list. The proof frames under this line are four such views at 320x200 (e1m1, e1m2, e1m3, e1m5), with their monsters: 0 pixels differ. -->

**V3-17a** · The mixer matches id's, sample for sample.
<!-- README.md: "the mixer's output, sample for sample" -->

**V3-17b** · Demo playback matches frame by frame: the camera, every entity, every dynamic light.
<!-- oracle/README.md, "Demo playback" -->

**V6-18** · One command runs ten checks, six of them against id's own C. All ten pass.
<!-- oracle/classic_check.py's CHECKS: goldens, play, timedemo, census, edicts, oracle, exact, screen2d, demolerp, sound. README.md, "Proof": "This runs the Classic preset through ten checks. Four compare the port with values recorded from a tree known to be right. Six run id's C next to the port". -->
<!-- Not "and prints: all pass": a program printing a verdict is not evidence; the count is. -->

## 4. The slop, switched on

### 4.0 From Classic

**V3-19** · Now, the slop. We start from Classic, and switch it on one setting at a time.
<!-- AUDIT.md: each departure is "a setting"; README.md: "every setting can be changed alone" -->

### 4.1 TORCHES

*(no voice: the bumper, TORCHES!, and the torch's own roar)*

**V3-20** · First: torches.

**V3-21** · id's mappers baked most of them into the walls, perfectly still: a hundred and forty-three, from the second map to the seventh.
<!-- FRAMERATE.md, "Steady torches that flicker": "id's mappers gave a torch an animated light style only now and then ... left the rest steady, style 0: all 143 torches and flames of e1m2–e1m7" -->

**V3-22** · The port finds each one with the light tool's own code, and lets it flicker, averaging to id's light.
<!-- FRAMERATE.md: "That is the tool's code ported"; "every luxel's light averages to id's" -->

**V3-24** · The user asked for that: a warm feeling.
<!-- AUDIT.md, `r_torchflicker`: "torches that flickered a bit, it gave a nice warm feeling" -->

### 4.2 Gliding lights

**V3-26** · The lights id did animate are strings of letters, from a for dark to zee for bright, a new letter every tenth of a second.
<!-- FRAMERATE.md, "Light styles between their letters": "`(map[(int)(cl.time*10) % len] - 'a') * 22`... holds a brightness for a tenth and jumps to the next" -->

**V7-27** · On a sharp, fast screen they hold, then jump: a stutter in a smooth picture. Slop glides between the letters.
<!-- AUDIT.md, `r_lerplightstyles`: "at 240 Hz a flickering torch holds each brightness for 24 frames"; "moves from each letter of its pattern to the next across its tenth of a second" -->
<!-- It says why the steps are bad: on a screen that shows every other motion smoothly, a light that holds and jumps reads as a stutter. -->

### 4.3 Smooth motion

**V3-28** · Monsters still think ten times a second. At two hundred and forty hertz, id's grunt stands still for twenty-three frames, then jumps.
<!-- FRAMERATE.md, "Monsters between their steps" -->

**V3-29** · Slop glides him, and blends his poses. Without the blend, he ice-skates.
<!-- AUDIT.md: `r_lerpmove`, `r_lerpmodels` -->

### 4.4 No frame cap, 480 Hz

**V3-30** · id capped the game at seventy-two frames a second, and tuned it there.
<!-- FRAMERATE.md, opening -->

**V3-31** · Run id's code faster, and the physics drifts: at four hundred and eighty hertz, jumps peak higher, and grenades land further.
<!-- FRAMERATE.md, "In short" -->

**V3-32a** · The slop preset draws a frame on every refresh, and steps gravity so that every jump lands on id's seventy-two hertz curve.
<!-- AUDIT.md, frame rate; FRAMERATE.md, "How the uncapped step works" -->

### 4.5 Fluid sky

**V3-32** · The clouds used to jump eight times a second. Now they drift.
<!-- AUDIT.md, `r_fluidsky`: "eight one-texel jumps a second"; "The sky's clouds glide" -->

### 4.6 Native pixels, horizontal plus

**V3-33** · Now, the picture.

*(the 4:3 box bursts out to the whole screen)*

**V3-34** · Your screen's own pixels: square, and never smoothed.
<!-- AUDIT.md, `vid_native`: "with square pixels"; README.md: "textures are never filtered" -->

**V3-35a** · id spread its ninety degrees across any width. On a wide screen, that cuts off the top and the bottom.
<!-- AUDIT.md, Hor+: "id spreads `fov` over any width, so a wide screen loses the top and bottom" -->

**V3-35b** · Horizontal plus keeps the height, and adds world at the sides.
<!-- AUDIT.md, Hor+: "`fov` spans a 4:3 screen, and a wider screen sees more at the sides" -->

### 4.7 The status bar, the crosshair

**V3-36** · The status bar scales up in whole pixels, and the brown strips beside it become more world.
<!-- AUDIT.md: the scaled 2-D layer; the status bar overlay, "id's layout leaves a brown strip either side of it" -->

*(no voice: the crosshair pops on, captioned CROSSHAIR)*

### 4.8 Nails from the barrels

**V3-37** · QuakeC fires each nail six point seven units above the barrel. At ten-eighty, you can tell. So now they leave the barrels.
<!-- AUDIT.md, `r_nailbarrels` -->

### 4.9 The mixer

**V3-38** · id's mixer runs at your device's rate now, with four of its bugs fixed. Like that click.
<!-- AUDIT.md, `snd_modern` -->

### 4.11 The perspective, last and short

**V3-42** · One last switch, and it's subtle.

**V3-44a** · id's renderer divided once every sixteen pixels, and drew straight lines in between.
<!-- FRAMERATE.md, "The perspective span" -->

**V3-44b** · Here is every divide.

**V3-45** · The slop preset divides every eight pixels, as id's own portable C did. Why eight? Honestly, it felt right. A tenth more work, a quarter of the error: a good trade.
<!-- AUDIT.md: span 8; FRAMERATE.md: "8 is id's own portable C"; "8 costs a tenth more than 16"; the steep test wall, "2.97 at 16, 0.77 at 8" -->

### 4.12 All of it, on

**V4-47** · And that's the slop preset, switched on.
<!-- AUDIT.md: "slop is the default: an idealized software-rendered Quake on a 2026 machine"; "two fixed presets set them all at once". -->
<!-- Not "and that's all of it": the slop preset also turns on the mouse wheel's weapon cycle and `sv_max_edicts`, which the film doesn't show. -->

*(no voice: the climax, e1m3's last hall in full slop: flickering torches, rockets, fiends)*

### 4.10 And the rest

*(over the Options page, Reset to Slop and Reset to Classic passing under the cursor)*

**V4-39a** · The rest is the same in both presets.
<!-- AUDIT.md, "The slop options and the presets": "The controls are shared. WASD, mouse look, the gamepad, ... the touch controls ... are the same in both presets"; "The same in both presets: ... `r_threads`: the pixels are the same for any thread count". WebAssembly is the build, not a setting (web/PLATFORM.md). -->

**V3-39** · Every core draws its own bands of the frame. One thread or eight, the same pixels.
<!-- README.md, "How it works": "so every thread count gives the same frame" -->

**V3-40** · It runs in a browser, as WebAssembly.
<!-- README.md: "It plays in a browser"; web/PLATFORM.md: `quake.wasm` in a Web Worker -->

**V3-41** · It's fast, and it feels fast. Mouse-look, W A S D, a gamepad, a phone: even a zoomer can play it.
<!-- FRAMERATE.md: 60 to 480 Hz; PERF_PLAN.md: the frame times, natively and in the page; AUDIT.md: the controls -->

*(no voice: the WASD keys of 1996 and of 2026)*

## 5. How it was built

**V3-48** · Claude, Anthropic's model, wrote the code, as fleets of agents, each on its own branch with a written brief.
<!-- README.md, "How it was built" -->

**V3-49** · A chair merged a branch only when every check passed.
<!-- README.md, "How it was built" -->

**V3-50** · The user set the rules: no dependencies, no unsafe code, and Classic proven.
<!-- README.md, "How it was built" -->

**V3-51** · Then played it, and said what was wrong.
<!-- README.md, "How it was built" -->

## 6. Closing

**V3-52** · id Software released Quake's source under a free licence. This port follows it, file by file.
<!-- README.md, "License"; "file by file" -->

**V3-53** · Every change has a name. Every name can be switched off.
<!-- Over the options switched off one by one: the build-up in reverse. -->

**V3-54** · And underneath, pixel for pixel, it is still nineteen ninety-six.

*(no voice: the end card, whose credits end "narrated in SRP: Slop Received Pronunciation")*

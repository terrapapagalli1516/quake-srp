# sfx v7: the sound effects of the v7 cut

The cues of the film's sound-design layer, on `edit/v7/timeline.json` (6:03.05). Only the lines inside the `sfx`
block are read; `render.py --help` describes the format. Among them: the clicks on the ladder's lights, S62's dipped
darks, the climb to ALL ON (bright enough for a phone), the switch to Classic on S16's cut, ten PASS ticks, LAB9's
room, and digital silence after "six".

- **S16** carries on under the question. Its room is S15c's own sound, in the edit's game track; nothing here sits
  on S16 or on the oracle cut (65.50). The hall's hiss under the S18 capture is gone by S16's start, and the first
  sound in section 3 is D03's lock, 2.2 s into the oracle.
- **LAB2**'s slowed-hum drone covers the whole shot. GLIDING LIGHTS' click follows the ladder (157.74).
- **S15** is slop from its first frame, so there is no switch at 48.38. The switch to Classic is on S16's cut
  (1:00.25): id's `menu2` and `whoosh-narrow-short` as the 4:3 box closes in. Both are gone by 60.75, before
  "Classic" (60.83).
- **HERO** (279.02–286.30) keeps only its downbeat on the cut, plus ALL ON's switch: HERO7 brings its own fight.

```sfx
# ---------------------------------------------------------------- 1 Cold open: X01-X11
S01@in            ambience/hum1     loop=1 dur=4.5 fadein=2.0 fadeout=0.3 lufs=-30     # e1m1's hum
S03+0             whoosh-long       rate=0.5 align=start lp=1800 lufs=-28               # X02 slow air with the crane
S06@in+0.1        shimmer           lufs=-26                                            # X03 the Quad turning
S07+2.5           shimmer-rise      lufs=-27                                            # X04 wireframe -> textured
S08@in+0.05       clack             lufs=-26                                            # X05 the leaves colour in
S09@out           whoosh-reverse    rate=0.55 lufs=-28                                  # X06 rising with the PVS crane
S10@in+0.05       stamp             lufs=-26                                            # X07 the cache tiles
S11:fill          bit-tick          lufs=-28 pan=-0.3 underwords=-5                     # X08 D01's bits (5 dB down under "point.")
S11:rshift        bit-tick          lufs=-28 pan=0.0 underwords=-5
S11:texel         bit-tick          lufs=-28 pan=0.3 underwords=-5
S12:fly           tick-scatter      lufs=-28                                            # X09 pixels into the palette
S13+0.9           ui-tick           times=16 every=0.0375 spread=-0.9>0.9 underwords=-5 lufs=-27   # X10 the divide marks, 5 dB under "rationed"
S14@in            hit-heavy         lufs=-21                                            # X11 the title

# ---------------------------------------------------------------- 2 The idea
S16@in            misc/menu2        dur=0.5 fadeout=0.1 lufs=-24                         # slop -> Classic on S16's cut: id's menu sound...
S16@in            whoosh-narrow-short lufs=-26                                          # ...under the box closing in (0.4 s); both gone before "Classic" (60.83)
S18a@in           ambience/fire1    loop=1 dur=6.5 fadein=0.3 fadeout=0.5 flutter=0.25 lufs=-30    # X13 start's hall under the silent S18 capture (S15/S16 carry their own)
S18a+3.17         misc/menu2        lufs=-27                                            # X15 Enter: Slop Options (on the cut)
S18a+3.77         misc/menu2        lufs=-27                                            #     Enter: Picture and sound
S18a+4.37         misc/menu1        times=7 every=0.333 lufs=-27                        #     a row every 0.33 s
S18b+2.13,2.43,2.73 misc/menu1      lufs=-27                                            #     up past the top (S18b)
# S18b's last Enter (file 3.17, 0.2 s before S16's cut) is dropped: the switch's own menu2 on the cut stands for it, and its 1 s ring sat on "Classic"

# ---------------------------------------------------------------- 3 The proof
S20@in+2.2        lock              lufs=-25                                            # X16 D03's lanes meet
S21+0             click             times=4 every=1.95 lufs=-26                         # X17 each map change
BD1@in+2.917      slide             lufs=-28                                            # X65 the two frames slide together...
BD1@in+2.917      clunk             lufs=-22                                            #     ...and merge (cards.py: max(minus+0.8, len-1.2))
silence           diff_black        video_not_frozen                                    # BD2: nothing at all until "No,"
BD3:grid          ui-tick-low       times=4 every=0.12 lufs=-28                         # X67 the four black tiles
S27:pass_lines    ui-type           lufs=-27                                            # X24 a tick per PASS line: ten (`exact` is the seventh)
all_pass_end      stinger-reveal    lufs=-24                                            # X25 the chord, on the end of "pass"

# ---------------------------------------------------------------- 4 The ladder (X10): one fixed level for every switch
ladder[do=light,item!=TORCH_FLICKER]+0.005 misc/menu2 lufs=-26 underwords=-4 name=switch   # the 14 lights: id's menu "select"...
ladder[do=light,item!=TORCH_FLICKER]+0.005 switch     lufs=-23 underwords=-4 name=switch   # ...and the switch's bright click, on the light
ladder[do=light,item=TORCH_FLICKER]+0.005  misc/menu2 lufs=-30 name=switch                # TORCH FLICKER sits under its roar
ladder[do=light,item=TORCH_FLICKER]+0.005  switch     lufs=-27 name=switch
ladder[do=all-flash]+0.005 misc/menu2 lufs=-26 name=switch                              # ALL ON
ladder[do=all-flash]+0.005 switch lufs=-23 name=switch
ladder[do=dark]+0.06  misc/menu1  lufs=-26 underwords=-8 avoid=S62~off:0.3 name=switch  # S62's 14 darks (X63), on each dim's start
ladder[do=dark]+0.06  switch      lufs=-23 underwords=-8 avoid=S62~off:0.3 name=switch

# ---------------------------------------------------------------- 4.1 TORCHES
torches_flicker_on+0.017 torch-whoosh-v4 lufs=-26                                      # TORCHES! the hit on the lit frame, 3 dB down...
torches_flicker_on+0.017 ambience/fire1 loop=1 dur=2.18 fadein=0.01 fadeout=0.4 force=1 crest=8 lufs=-26.5   # X44 ...the torch's own hiss, up loud...
torches_flicker_on+0.017 ambience/fire1 loop=1 rate=0.5 dur=2.18 fadein=0.01 fadeout=0.4 force=1 crest=8 lufs=-29     # ...an octave down
LAB1+0            breath            lufs=-30                                            # X68 with the lightmap's swing (to shot 4.5)

# ---------------------------------------------------------------- 4.2 Gliding lights
# X42: ST2's camera is 405 units from e1m1's nearest fl_hum1 (id's static ambients fall silent past
# 333), so ST2.wav cannot carry it: force=1 adds, it does not double. A tik on each change of style 10.
ST2@in            ambience/fl_hum1  loop=1 dur=9.42 fadein=0.3 fadeout=0.3 force=1 lufs=-30
ST2+0.4,0.7,0.9,1.1,1.5,1.9,2.2,2.4,2.9,3.2,3.4,3.6,4.0,4.4,4.7,4.9,5.4,5.7,5.9,6.1,6.5,6.9,7.2,7.4,7.9,8.2,8.4,8.6,9.0,9.4 elec-tik lufs=-31
ST2+0.0,0.5,0.8,1.0,1.4,1.6,2.0,2.3,2.5,3.0,3.3,3.5,3.9,4.1,4.5,4.8,5.0,5.5,5.8,6.0,6.4,6.6,7.0,7.3,7.5,8.0,8.3,8.5,8.9,9.1 elec-tik pitch=5 lufs=-34
LAB2@in           ambience/fl_hum1  rate=0.0625 dur=9.58 fadein=1.0 fadeout=1.0 lufs=-30   # X43 the hum at 1/16, a drone, the whole (longer) LAB2

# ---------------------------------------------------------------- 4.3 Smooth motion
LAB3a@in+0.25     soldier/sight1    lufs=-28                                            # X39 the grunt wakes
LAB3a:id_steps    thud              lufs=-28                                            # X40 each of id's steps
LAB3b:held_pose_changes click       pan=-0.5 lufs=-26                                   # X41 the held pose snaps...
LAB3b:held_pose_changes skate       pan=-0.5 lufs=-31                                   #     ...and slides

# ---------------------------------------------------------------- 4.5-4.6 Fluid sky, the burst, Hor+
LAB5@in           ambience/wind2    loop=1 dur=5.02 fadein=0.3 fadeout=0.5 lufs=-30    # X58 the sky
burst             burst-v6          dur=1.71 fadeout=0.5 lufs=-14.5                        # X69 the burst, +2.5 dB in 300 Hz-4 kHz, gone before "Your"
all_on            hero-hit          lufs=-14                                            # ALL ON: the downbeat, on HERO's first frame
LAB6a+3.5         squeeze           lufs=-26                                            # X36 id's 16:9 crops (shot 3.5-5.0)
LAB6b+0           whoosh-widen      align=start rate=1.6 lufs=-25                       # X37 Hor+ opens the sides...
events LAB6b      only=wizard/ lufs=-30                                                 #     ...the Scrag's own sounds, from its log

# ---------------------------------------------------------------- 4.7-4.8 HUD, crosshair, nails
LAB8a:freeze      weapons/rocket1i  rate=0.25 dur=0.25 tapestop=0.25 force=1 lufs=-22   # X57 the tape-stop on the freeze
LAB8a~barrel.end  room-tone         dur=0.36 fadein=0.05 fadeout=0.10 lufs=-30          # room tone across the hole (232.48-232.74: LAB8a's game is silent there) into LAB8b's ambience

# ---------------------------------------------------------------- 4.9 The mixer: the edit plays the hums and clicks; this adds the room
LAB9@in           room-tone         dur=4.76 fadein=0.3 fadeout=0.3 lufs=-34             # under V3-38a (ducked 6 dB: ~10 dB under the edit's -30 hum), to id's hum crossing in
silence           lab9_id_hum       clean_mixer                                          # the gap: id's hum and its clicks, alone
clean_mixer       room-tone         dur=1.67 fadein=0.2 fadeout=0.3 lufs=-40             # the clean hold (FULL-RATE SOUND lights in it), ~10 dB under the hum

# ---------------------------------------------------------------- 4.10 And the rest
LAB10a@in+0.1     ui-tick           times=8 every=0.125 lufs=-28                        # X55 a tick per band
LAB10c+3.27       misc/menu2        lufs=-25                                            # X59 "click to start"
LAB10e+0.3        whoosh-short      pan=-0.5>0.6 lufs=-25                               # X50 the mouse-look flick
LAB10g+3.0        weapons/guncock   lufs=-26                                            # X51 the tap on FIRE
silence           wasd_gag          LAB10h@out                                          # the WASD gag: only the keys and the water
LAB10h:surface_break misc/outwater  lufs=-20                                            # X52 the 1996 view breaks the surface
LAB10h+0.3,1.2    key-click         lufs=-24                                            # X53 A, then D

# ---------------------------------------------------------------- 4.11 The perspective
PER1@in+3.0       click             lufs=-25                                            # X28 D06a's divide (its 'divide' cue)
PER2@in           glass-tick        times=16 every=0.033 spread=-0.9>0.9 lufs=-28       # X29 every divide snaps on...
PER2@in+0.5       grain             dur=1.4 fadein=0.4 fadeout=0.3 lufs=-38             #     ...a faint crawl
span8+0.08        zap               lufs=-24                                            # X30 the marks double, just after the switch
PER4:flips        misc/menu3        lufs=-26                                            # X31 each flip...
felt_right+0.35   thunk             lufs=-24                                            #     ...landing on 8, after "right"

# ---------------------------------------------------------------- 5 How it was built
S57@in+0.6        ui-tick           times=11 every=0.6 spread=-0.6>0.6 lufs=-30         # X60 a tick per branch
S58:d15_merges    misc/menu1        lufs=-28                                            # X61 each merge (the clock's d15_merges)
S59:rule1         strike-low        lufs=-24                                            # X62 the rules
S59:rule2         strike-low        lufs=-24
S59:rule3         strike-low        lufs=-24

# ---------------------------------------------------------------- 6 Closing
S63@in            ambience/fl_hum1  loop=1 dur=5.21 fadein=1.0 fadeout=0.8 lufs=-34     # X01 the bookend's air, faint, gone by the end of "six"
silence           six_end           S64@out                                             # after "six": the score's arrival alone
```

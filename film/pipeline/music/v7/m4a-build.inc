# M4a The slop, switched on, from Classic to the mixer: one layer per switch, each switch's bell a
# step higher than the last, each in its breath. Every entry is a named event (events.tsv).
anchor auto $CLASSIC tempo=120 grid=1/2
anchor auto $TORCHES_HIT tempo=192 grid=1
anchor auto $TORCHES_END tempo=120 grid=1/2
anchor auto $GLIDE_START
anchor auto $GLIDE_ON
anchor auto $STEP1
anchor auto $STEP1+9.6 grid=0.4
anchor auto $SNAP1
anchor auto $SMOOTH_ON
anchor auto $FRAMECAP_START
anchor auto $STRIP480
anchor auto $FRAMECAP_ON
anchor auto $JUMPS
anchor auto $SKY_ON
anchor auto $BURST
anchor auto $HUD
anchor auto $SCALED_HUD grid=1/2
anchor auto $HUD_OVERLAY grid=1/2
anchor auto $CROSSHAIR grid=1/2
anchor auto $NAILS
anchor auto $NAILS_REAL
anchor auto $NAIL_ON grid=1/2
anchor auto $MIXER
anchor auto $MIXER_CLEAN tempo=120

# ----------------------------------------------------------------------- Classic: everything off
@$CLASSIC     sub     D1 >@$BUILT 0.55 attack=2 release=1                 # a bare sub on D: a held breath
arp @$CLASSIC lowbell D3 >@$LAYERS_END step=8 order=0 vel=0.45                  # one clean, low bell every 4 s
auto master width @$CLASSIC-0.5 1 @$CLASSIC 0.35 @$BURST 0.35 @$BURST_END 1

# ----------------------------------------------------------------------- 1 TORCHES!
@$TORCHES_HIT gong  - 1 0.8
@$TORCHES_HIT gongl - 1 0.7
@$TORCHES_HIT crash - 1 1.0 decay=1.3
play chant    @$TORCHES_HIT vel=0.7                                       # TOR-CHES!
play chant    @$TORCHES_HIT+1.25 vel=0.7                                  # TOR-CHES! (four beats on)
play schord2  @$TORCHES_HIT until=@$TORCHES_END times=2                   # the D chord rung, then chugs
play drumsD   @$TORCHES_HIT until=@$TORCHES_END times=2                   # the double kick
play motif4   @$TORCHES_HIT track=dbass transpose=-12 vel=1.0             # the theme's first four notes
play motif4   @$TORCHES_HIT track=hibell transpose=24 vel=0.55            # ...and an octave up, on the bells
@$TORCHES_ON  swbell D4 1 0.6                                              # TORCH FLICKER
# the warm pad stays all act, its filter on two slow LFOs (the torch's two voices)
play warmprog @$TORCHES_END track=padwarm until=@$BURST times=6
play warmrise @$BURST track=padwarm until=@$WARM_END times=6         # the burst turns the loop to a rising i-III-VI-VII

@$WARM_BREATH+0.1 padt D4,F4,A4 1.6s 0.3 attack=0.5 release=1.2 cutoff=1400   # a warm swell in the breath
@$WARM_BREATH+0.1 bell D5 1 0.35 decay=3

# ----------------------------------------------------------------------- 2 GLIDING LIGHTS
# a tremolo on the pad: stepped while id's letters show, gliding once slop's do
lightstyle padwarm @$GLIDE_START mmamammmmammamamaaamammma rate=10 dur=$GLIDE_ON-$GLIDE_START depth=0.8 scale=264 outside=1 trail=0
lightstyle padwarm @$GLIDE_ON    mmamammmmammamamaaamammma rate=10 mode=ramp dur=$HERO-$GLIDE_ON depth=0.45 scale=264 lead=0 outside=1
@$GLIDE_ON    glass   A5,E6 1 0.6                                          # a glassy bell on the switch
@$GLIDE_ON    swbell  E4 1 0.65                                           # GLIDING LIGHTS

# ----------------------------------------------------------------------- 3 SMOOTH MOTION
play gruntsteps @$STEP1                                                   # stepped notes on the grunt's steps
play poses    @$SNAP1 track=bassl stretch=1.6 vel=0.8                     # the poses: held on the left...
play poses    @$SNAP1 track=gbass stretch=1.6 vel=0.6                     # ...blended on the right
auto gbass pan @$SNAP1 1
@$SMOOTH_MON  swbell  F4 1 0.65                                           # SMOOTH MONSTERS
@$SMOOTH_MON  thud    - 1 0.4
@$SMOOTH_ON   swbell  G4 1 0.65                                           # SMOOTH ANIMATIONS
@$SMOOTH_ON   gbass   D1 0.3s 0.8                                         # the switch: the bass slides up
@$SMOOTH_ON+0.3 gbass D2 0.9s 0.8
auto gbass pan @$SMOOTH_ON 1 @$SMOOTH_ON+0.2 0
play glideprog @$TORCHES_END until=@$BURST times=6
play gliderise @$BURST until=@$LAYERS_END times=6                         # then the legato line, gated in
auto gbass2 level @0 -90 @$SMOOTH_ON+0.6 -90 @$SMOOTH_ON+1.4 0 @$LAYERS_END 0 @$LAYERS_END+0.01 -90 @$HERO-0.01 -90 @$HERO -3 @$HERO+2 -3 @$HERO+2.5 0

# ----------------------------------------------------------------------- 4 NO FRAME CAP
# the layer is the sixteenth grid: eighths at id's gate, thirty-seconds on the 480 Hz strip, eighths
# for the jumps, then sixteenths from the switch on, staying
arp @$FRAMECAP_START hatl 9000hz >@$STRIP480 step=1/2 order=0,0 vel=0.42 accent=0 accent_vel=0.6
arp @$STRIP480       hatl 9000hz >@$JUMPS step=1/8 order=0,0,0,0,0,0,0,0 vel=0.4 accent=0 accent_vel=0.6
arp @$JUMPS          hatl 9000hz >@$FRAMECAP_ON step=1/2 order=0,0 vel=0.42 accent=0 accent_vel=0.6
arp @$FRAMECAP_ON    hatl 9000hz 1 step=1/16 order=0 vel=0.5 vel_end=0.85                        # the switch: a burst
@$FRAMECAP_ON        hatl - 1 0.7 open=1
@$FRAMECAP_ON        swbell A4 1 0.65                                    # FRAME RATE CAP: NONE
auto hatl level @$BURST-0.01 0 @$BURST -5                                 # the layer stays, under the burst's own (it ends at LAYERS_END)
arp @$FRAMECAP_ON+0.5 hatl 9000hz >@$LAYERS_END step=1/4 order=0,0,0,0 vel=0.5 accent=0 accent_vel=0.7
# the jumps: a glissando follows each; the 480 Hz arc higher; then fused on id's curve
play arc    @$JUMP1 stretch=1.13
play arc480 @$JUMP2 stretch=1.13
play arc    @$JUMP3 stretch=1.13
play arc480 @$JUMP3 stretch=1.13 transpose=-1 cents=50

# ----------------------------------------------------------------------- 5 FLUID SKY
arp @$SKY_ON  wash 9000hz >@$LAYERS_END step=2 order=0 vel=0.5                   # a cymbal wash, airy
@$SKY_ON      swell A6 >@$SKY_ON+2 0.35 source=noise bright=3
@$SKY_ON      swbell  Bb4 1 0.65                                         # FLUID SKY
arp @$SKY_ON  spark D6,A6,F6,E6 >@$BURST step=1/2 order=0,1,2,3 vel=0.18

# ----------------------------------------------------------------------- 6, 7 THE BURST: native pixels, horizontal plus
@$BURST-1.5   swell D4 >@$BURST 0.55
@$BURST       crash - 1 1.0 decay=1.6
@$BURST       gong  - 1 0.5
@$BURST       stab  D3,A3,D4,F4 0.4 0.9
@$BURST       boom  - 1 0.75 freq=42 decay=1.5
@$BURST       swbell C5 1 0.75                                           # NATIVE PIXELS
@$BURST_END   swbell D5 1 0.7                                            # WIDESCREEN FOV
play motif4   @$BURST track=hibell transpose=36 vel=0.7                   # the theme an octave up (the phone)
play burstband @$BURST until=@$HUD                                         # the synth chug and the bass enter, and stay
play kicks    @$BURST until=@$HUD times=8                                  # the kick with it

# ----------------------------------------------------------------------- 8, 9, 10 THE HUD: the gallop
play sgallopx @$HUD until=@$NAILS times=5
play drums    @$HUD until=@$NAILS times=5
@$SCALED_HUD  snare - 1 1.0                                                # a hit on each HUD change
@$SCALED_HUD  swbell E5 1 0.65
@$SCALED_HUD  clank - 1 0.55 freq=800
@$HUD_OVERLAY snare - 1 1.0
@$HUD_OVERLAY swbell F5 1 0.65
@$HUD_OVERLAY clank - 1 0.6 freq=1000
@$CROSSHAIR   tick  - 0.1 0.9 tone=5200                                    # a sharp tick on the crosshair
@$CROSSHAIR   snare - 1 0.85                                               # ...and a hit, out of the hole, as on the other two
@$CROSSHAIR   swbell G5 1 0.65

# stop-time holes: chugs, kick and snare out for the eighth before each HUD hit and NAILS FROM BARRELS
auto chugs level @$SCALED_HUD-0.27 0 @$SCALED_HUD-0.25 -90 @$SCALED_HUD-0.005 -90 @$SCALED_HUD 0
auto kick  level @$SCALED_HUD-0.27 0 @$SCALED_HUD-0.25 -90 @$SCALED_HUD-0.005 -90 @$SCALED_HUD 0
auto snare level @$SCALED_HUD-0.27 0 @$SCALED_HUD-0.25 -90 @$SCALED_HUD-0.005 -90 @$SCALED_HUD 0
auto chugs level @$HUD_OVERLAY-0.27 0 @$HUD_OVERLAY-0.25 -90 @$HUD_OVERLAY-0.005 -90 @$HUD_OVERLAY 0
auto kick  level @$HUD_OVERLAY-0.27 0 @$HUD_OVERLAY-0.25 -90 @$HUD_OVERLAY-0.005 -90 @$HUD_OVERLAY 0
auto snare level @$HUD_OVERLAY-0.27 0 @$HUD_OVERLAY-0.25 -90 @$HUD_OVERLAY-0.005 -90 @$HUD_OVERLAY 0
auto chugs level @$CROSSHAIR-0.27 0 @$CROSSHAIR-0.25 -90 @$CROSSHAIR-0.005 -90 @$CROSSHAIR 0
auto kick  level @$CROSSHAIR-0.27 0 @$CROSSHAIR-0.25 -90 @$CROSSHAIR-0.005 -90 @$CROSSHAIR 0
auto snare level @$CROSSHAIR-0.27 0 @$CROSSHAIR-0.25 -90 @$CROSSHAIR-0.005 -90 @$CROSSHAIR 0
auto chugs level @$NAIL_ON-0.27 0 @$NAIL_ON-0.25 -90 @$NAIL_ON-0.005 -90 @$NAIL_ON 0
auto kick  level @$NAIL_ON-0.27 0 @$NAIL_ON-0.25 -90 @$NAIL_ON-0.005 -90 @$NAIL_ON 0
auto snare level @$NAIL_ON-0.27 0 @$NAIL_ON-0.25 -90 @$NAIL_ON-0.005 -90 @$NAIL_ON 0

# ----------------------------------------------------------------------- 11 NAIL BARRELS: a tape stop on the freeze
play sgallopx @$NAILS until=@$NAILS_REAL times=3
play drums    @$NAILS until=@$NAILS_REAL times=3
varispeed @$NAIL_FREEZE-0.3 @$NAILS_REAL @$NAIL_FREEZE-0.3 1 @$NAIL_FREEZE 0
@$NAILS_REAL  crash - 1 1.0 decay=1.5                                      # back at tempo
@$NAILS_REAL  stab  D3,A3,D4,F4 0.3 0.85
@$NAIL_ON     swbell A5 1 0.65                                           # NAILS FROM BARRELS
@$NAIL_ON     anvil  A4 1 0.5 decay=1.2
@$NAIL_ON     snare - 1 0.9                                                # out of the hole
play sriff    @$NAILS_REAL until=@$MIXER times=3
play drums    @$NAILS_REAL until=@$MIXER times=3

# ----------------------------------------------------------------------- 13 FULL-RATE SOUND: the score silent through LAB9
auto master dry  @$MIXER-0.003 1 @$MIXER 0 @$MIXER_CLEAN-0.01 0 @$MIXER_CLEAN 1        # the stop rings 0.3 s (its reverb)
auto master gain @$MIXER+0.25 1 @$MIXER+0.3 0 @$MIXER_CLEAN-0.002 0 @$MIXER_CLEAN 1   # then digital silence

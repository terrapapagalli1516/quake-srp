# M5 How it was built (the fleet) and M6 the closing: the build undone.
# The Shambler's cut-out is on S60's cut.

# ======================================================================= M5 THE FLEET (100, D Dorian)
anchor auto $BUILT tempo=100 grid=1
@$BUILT      pad   D3,A3,C4,E4 >@$SHAMBLER_OUT 0.4 cutoff=500 cutoff_end=1200 attack=2
arp @$BUILT     fleet D3,A3,C4,E4  >@$MERGE1  step=1   vel=0.3  pan=0
arp @$BUILT+0.6 fleet F4,A4,D5     >@$MERGE2  step=1/2 vel=0.25 pan=-0.5
arp @$BUILT+1.2 fleet A4,C5,E5     >@$MERGE3  step=2/3 vel=0.25 pan=0.5
arp @$BUILT+1.8 fleet D5,F5,B5     >@$MERGE4  step=3/4 vel=0.24 pan=-0.8
arp @$BUILT+2.4 fleet E5,G5,C6     >@$MERGE5  step=1/2 vel=0.24 pan=0.8
arp @$BUILT+3.0 fleet C5,D5,A5     >@$MERGE6  step=1/3 vel=0.22 pan=-0.3
arp @$BUILT+3.6 fleet G4,B4,E5     >@$MERGE7  step=3/4 vel=0.24 pan=0.3
arp @$BUILT+4.2 fleet A5,D6,E6     >@$MERGE8  step=1   vel=0.22 pan=-0.6
arp @$BUILT+4.8 fleet B4,D5,F5     >@$MERGE9  step=1/2 vel=0.22 pan=0.6
arp @$BUILT+5.4 fleet D6,A6        >@$MERGE10 step=1/4 vel=0.18 pan=-0.2
arp @$BUILT+6.0 fleet F5,C6,G6     >@$MERGE11 step=2/3 vel=0.2  pan=0.2
@$MERGE1     bell  D5 1 0.35 decay=2
@$MERGE2     bell  E5 1 0.35 decay=2
@$MERGE3     bell  F5 1 0.36 decay=2
@$MERGE4     bell  G5 1 0.37 decay=2
@$MERGE5     bell  A5 1 0.38 decay=2
@$MERGE6     bell  B5 1 0.39 decay=2
@$MERGE7     bell  C6 1 0.4 decay=2
@$MERGE8     bell  D6 1 0.41 decay=2
@$MERGE9     bell  E6 1 0.42 decay=2
@$MERGE10    bell  F6 1 0.43 decay=2
@$MERGE11    bell  A6 1 0.45 decay=2
@$MERGED     bell  D5,F5,A5,C6 1 0.5 decay=4
@$MERGED     pad   D3,F3,A3,C4,E4 >@$SHAMBLER_OUT 0.5 attack=0.3 cutoff=1500 release=0.3
@$BUILT+1    sub   D1 >@$SHAMBLER_OUT 0.5 attack=2 release=0.2
@$RULE1      boom  - 1 0.42 freq=40 decay=1.5                            # a strike per rule
@$RULE1      anvil D3 1 0.6 decay=2
@$RULE2      boom  - 1 0.45 freq=40 decay=1.5
@$RULE2      anvil D3 1 0.62 decay=2
@$RULE3      boom  - 1 0.48 freq=40 decay=1.5
@$RULE3      anvil D3 1 0.65 decay=2
auto master dry  @$SHAMBLER_OUT-0.003 1 @$SHAMBLER_OUT 0 @$FINALE-0.01 0 @$FINALE 1   # the Shambler's own sounds; the stop rings 0.3 s
auto master gain @$SHAMBLER_OUT+0.25 1 @$SHAMBLER_OUT+0.3 0 @$FINALE-0.002 0 @$FINALE 1

# ======================================================================= M6 CLOSING: the M4 peak again, then undone
anchor auto $FINALE tempo=120 grid=1/2
@$FINALE     crash - 1 0.9 decay=1.5
@$FINALE     boom  - 1 0.6 freq=40 decay=2
@$CHTHON     boom  - 1 1.0 freq=36 decay=4                              # Chthon breaks the lava
@$CHTHON     gong  - 1 0.6
@$FINALE     sub   D1 >@$END 0.6 attack=0.05 release=3
play full    @$FINALE
play motif   @$FINALE track=hibell transpose=36 vel=0.55                  # the theme an octave up (the phone)
play landing @$FINALE+8 times=3                                           # on D, driving, while the layers leave
lightstyle padt @$FINALE mmamammmmammamamaaamammma rate=10 mode=ramp dur=$OFF_GLIDE-$FINALE depth=0.45 scale=264 outside=1
auto master width @$FINALE-0.01 1 @$FINALE 1 @$OFF_WIDESCREEN 1 @$OFF_WIDESCREEN+0.3 0.35 @$S63 0.35 @$S63+1 1
# the ladder goes dark, 14 to 1, one layer with each (their reverbs ring on)
auto glass  level @$OFF_SPAN8      0 @$OFF_SPAN8+0.03      -90
auto spark  level @$OFF_SPAN8      0 @$OFF_SPAN8+0.03      -90
auto snare  level @$OFF_SOUNDFIX   0 @$OFF_SOUNDFIX+0.03   -90
auto brass  level @$OFF_NAIL       0 @$OFF_NAIL+0.03       -90
auto bell   level @$OFF_CROSSHAIR  0 @$OFF_CROSSHAIR+0.03  -90
auto hibell level @$OFF_CROSSHAIR  0 @$OFF_CROSSHAIR+0.03  -90
auto kick   level @$OFF_OVERLAY    0 @$OFF_OVERLAY+0.03    -90
auto dbass  level @$OFF_SCALEDHUD  0 @$OFF_SCALEDHUD+0.03  -90
auto chugs  level @$OFF_NATIVE     0 @$OFF_NATIVE+0.03     -90
auto wash   level @$OFF_SKY        0 @$OFF_SKY+0.03        -90
auto hat    level @$OFF_FRAMECAP   0 @$OFF_FRAMECAP+0.03   -90
auto gbass2 level @$OFF_SMOOTHANIM 0 @$OFF_SMOOTHANIM+0.03 -90
auto crash  level @$OFF_SMOOTHMON  0 @$OFF_SMOOTHMON+0.03  -90
auto padt   level @$OFF_TORCHES    0 @$OFF_TORCHES+0.03    -90
# ======================================================================= THE ENDING: the film ends on V3-54
# The film ends on "it is still nineteen ninety-six", with fuller music, ringing a little longer.
# Driven by the line's end and the end card's start.
#  - S63: the switch-offs have left the 1996 voicing: the bare sub and the low bell of the act's start.
#  - The theme comes home: M2's plain Classic statement on the low bell, an octave up, at M2's 72 bpm, timed
#    back from the line's end so that its last note, A, lands under "nineteen".
#  - The dominant swells under "nineteen ninety-six" (A with its C#, low, under the voice's band).
#  - The arrival, just after "six": the film's first chord, the title's (boom, sub, growl, brass, pad, bells),
#    the 1996 voicing in full. It rings into the end card, which adds its own bright bell, to the film's end.
track padh   patch=pad   gain=-14 hp=60 hall=0.45 group=drones-pads
track brassh patch=brass gain=-12 hall=0.4 eq_lo=-2 group=melodic
track bellh  patch=bell  gain=-15 hall=0.55 group=melodic
let HOME_IN  = $LINE54_END-4.15
let HOME_A   = $LINE54_END-0.82
let ARRIVAL  = $LINE54_END+0.15

arp @$S63    lowbell D3 >@$HOME_IN step=8 order=0 vel=0.45
play motif4  @$HOME_IN track=lowbell transpose=12 vel=0.5 stretch=1.6667          # D A Bb G ...
@$HOME_A     lowbell A4 1 0.55 decay=4                                            # ... A, under "nineteen"
@$HOME_A-0.2 padh  A2,E3,A3,C#4 >@$ARRIVAL-0.02 0.4 attack=0.9 release=0.12 cutoff=700   # V, swelling, low
@$HOME_A     timp  A1 1 0.35
auto sub level @$S63 0 @$ARRIVAL-0.01 0 @$ARRIVAL 2.5 @$END 2.5                   # the sub opens with the chord

# the arrival: the title's chord (0:37.0), now the film's last
@$ARRIVAL    boom    - 1 0.6 freq=38 decay=5
@$ARRIVAL    growl   D2 3s 0.7 release=2.5
@$ARRIVAL    brassh  D3,A3,D4 3.5s 0.75 release=2.5
@$ARRIVAL    padh    D3,A3,D4,F4 >@$END 0.55 attack=0.05 cutoff=1800 cutoff_end=600 release=3
@$ARRIVAL    pad     D2,A2,D3 >@$END 0.5 attack=0.3 cutoff=900 release=3
@$ARRIVAL    lowbell D3 1 0.55 decay=8
@$ARRIVAL    bellh   D5 1 0.65 decay=6
@$ARRIVAL+1.2 bellh  A4,D5 1 0.55 decay=6                                        # as at the title, 1.2 s on
# the end card: its bright voice, and the low bell once more, ringing to the end
@$END_CARD+0.5 cardbell D5,A5 1 0.45 decay=5                                     # on the card's first full frame (it fades up 0.5 s)
@$END_CARD+0.5 lowbell  D3 1 0.4 decay=8

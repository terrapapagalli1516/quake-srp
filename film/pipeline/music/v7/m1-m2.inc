# M1 Cold open and M2 The idea (it settles on Classic's D minor).
tempo 0 60

# ======================================================================= M1 CORRIDOR (dark)
@0              air   - >@$T_M1_DROP 0.6 attack=8 release=1.5
@$T_M1_SUB      sub   D1 >@$T_M1_SILENCE 0.7 attack=4 release=0.3
@$T_M1_SUB      drone D2 >@$T_M1_DROP 0.45 attack=6 release=1.5 evolve=0.05 f_hi=900
@$T_M1_MOTIF    growl D2 >@$T_M1_REST 0.8                     # the dark motif begins
@$T_M1_REST+0.3 swell D4 >@$T_M1_CRANE 0.6                    # the grenade's rest, then a swell into the cut
@$T_M1_CRANE    boom  -  1 0.5 freq=42 decay=3
@$T_M1_CRANE    growl A2 2s 0.75
@$T_M1_CRANE+2  growl Bb2 1s 0.8
@$T_M1_CRANE+3  growl G2 1s 0.7
@$T_M1_CRANE+4  growl A2 >@$T_M1_WIRE 0.75
@$T_M1_CRANE    pad   D3,A3 >@$T_M1_WIRE 0.5 cutoff=200 cutoff_end=500
@$T_M1_CUT1     clank Eb4 1 0.3 pan=-0.6
@$T_M1_CUT2     clank A4  1 0.3 pan=0.5
@$T_M1_CUT3     clank D5  1 0.35 pan=-0.3
@$T_M1_WIRE-1.5 swell D4 1.5s 0.7
@$T_M1_WIRE     bell  D4 1 0.6 decay=6
@$T_M1_WIRE     clank Ab4 1 0.4
@$T_M1_WIRE     pad   Eb3,Bb3 >@$T_M1_TEXT 0.5 cutoff=300 cutoff_end=600   # the flat second: dread
@$T_M1_WIRE     growl D2 >@$T_M1_LEAVES 0.7
@$T_M1_TEXT     bell  D6,A6 1 0.35 decay=3                    # the dissolve's shimmer, doubled
@$T_M1_TEXT     pad   D3,F3,A3 >@$T_M1_DROP 0.55 cutoff=350 cutoff_end=900 release=0.8
@$T_M1_LEAVES   clank - 1 0.35 freq=380 pan=-0.4
@$T_M1_PVS      clank - 1 0.38 freq=520 pan=0.4
@$T_M1_CACHE    clank - 1 0.42 freq=700
@$T_M1_LEAVES   growl D2 >@$T_M1_CACHE 0.75                   # the motif again, fitted to the hits
@$T_M1_CACHE    growl A2 >@$T_M1_FIXED 0.75
@$T_M1_FIXED    growl Bb2 1s 0.8
@$T_M1_FIXED    tick  - 0.1 0.7
@$T_M1_FIXED+0.1 tick - 0.1 0.7
@$T_M1_FIXED+0.2 tick - 0.1 0.7
@$T_M1_FIXED+1  growl G2 >@$T_M1_COLOURS 0.7
@$T_M1_COLOURS  growl A2 >@$T_M1_DROP 0.75 release=0.6
@$T_M1_COLOURS  bell  D4,A4,F5 1 0.55 decay=5                 # 256 colours
auto master gain @$T_M1_SILENCE-0.05 1 @$T_M1_SILENCE 0 @$T_TITLE-0.002 0 @$T_TITLE 1   # after "rationed" (0:36.40), a 50 ms fade; 0.63 s of true silence
# the title: no voice, full
@$T_TITLE  boom  - 1 0.8 freq=38 decay=4.5
@$T_TITLE  sub   D1 >@$T_M2+2 0.85 attack=0.05 release=3
@$T_TITLE  growl D2 3s 0.75
@$T_TITLE  brass D3,A3,D4 3s 0.85 release=1.5
@$T_TITLE  pad   D3,A3,D4,F4 >@$T_M2 0.65 attack=0.05 cutoff=1800 cutoff_end=500
@$T_TITLE  bell  D5 1 0.7 decay=6
@$T_TITLE2 bell  A4,D5 1 0.6 decay=5

# ======================================================================= M2 THE IDEA
anchor auto $T_M2 tempo=72 grid=1
anchor auto $T_M2_MOTIF grid=1
anchor auto $WIPE grid=1
@$T_M2       padm2 D3,F3,A3,E4 >@$CLASSIC_D 0.6 attack=1.2 release=1.2   # S15 is slop throughout (no switch): one pad to S16
auto padm2 width  @$T_M2 0.1 @$T_M2_OPEN 0.1 @$T_M2_OPEN+2.5 1.0
auto padm2 cutoff @$T_M2 250 @$T_M2_OPEN 250 @$T_M2_OPEN+2.5 1600
@$T_M2+1     sub   D1 >@$CLASSIC_D 0.5 attack=3 release=1
play motif4 @$T_M2_MOTIF track=bell transpose=12 vel=0.65 decay=3       # the theme's first four notes, under V3-08
auto padm2 level @$WIPE+4.5 0 @$WIPE+5.5 -5                               # light under the menu's own sounds
auto glass level @0 0 @$WIPE+4.5 0 @$WIPE+5.5 -5 @$CLASSIC_D -5 @$CLASSIC_D+0.5 0
auto spark level @0 0 @$WIPE+4.5 0 @$WIPE+5.5 -5 @$CLASSIC_D -5 @$CLASSIC_D+0.5 0
# Classic: a plain D minor chord (the colour that comes back at the act's start)
@$CLASSIC_D  pad   D3,F3,A3 >@$T_M3+1 0.5 attack=0.4 release=1.5 cutoff=500
@$CLASSIC_D  sub   D1 >@$T_M3+1 0.45 attack=0.5 release=1
@$CLASSIC_D  bell  D4 1 0.4 decay=4

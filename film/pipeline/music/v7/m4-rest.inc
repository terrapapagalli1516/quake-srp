# M4-rest "And the rest": after HERO, a coda. The button's tail is heard alone (REST_BAND = the out
# cut + 0.6 s; the voice enters at +1.0). Then the coda's own light figure, unbroken to the WASD gag at about
# -27 LUFS short-term as heard: the rising loop, the legato bass, a light groove, the glass on D-A-E (it sits on
# all four chords), the motif and its answer on the spark. A hit on each caption's cut. The gag is its only drop.
anchor auto $REST tempo=120 grid=1/2
anchor auto $WASD
play warmrise  @$REST_BAND track=padwarm until=@$WASD times=3
auto padwarm level @$REST_BAND-0.01 -90 @$REST_BAND 0
play gliderise @$REST_BAND until=@$WASD times=3
play codadrums @$REST_BAND until=@$WASD times=12
play motif     @$REST_BAND track=spark transpose=24 vel=1.0
play answer    @$REST_BAND+4.0 track=spark transpose=24 vel=1.0
arp @$REST_BAND glass D5,A5,E6,A5 >@$WASD step=1/4 order=0,1,2,3 vel=0.3
@$REST_CRASH  crash - 1 0.8 decay=1.2
@$CAP2        snare - 1 0.9
@$CAP2        clank - 1 0.5 freq=900
@$CAP3        crash - 1 0.7 decay=1.0
@$CAP4        snare - 1 0.9
@$CAP5        boom  - 1 0.6 freq=44 decay=1.0
@$CAP5        stab  D3,A3,D4,F4 0.25 0.8
@$CAP6        crash - 1 0.75 decay=1.0

# ----------------------------------------------------------------------- THE WASD GAG: silence
auto master gain @$WASD-0.003 1 @$WASD 0 @$WASD_END-0.002 0 @$WASD_END 1

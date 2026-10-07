# The run-in to ALL ON climbs instead of sagging.
# The span's bed is 3 dB up (the ride) with the legato bass back under it. From "felt right" (SHRUG) a riser
# climbs about 8 s to the downbeat (in pitch, density and brightness: the build's duck holds its level under
# the voice), built outside the voice's band: a rising sub, a timpani roll low-passed at
# 900 Hz, a hat roll and a reversed noise swell high-passed above 4.5 kHz. In the gap before V4-47 a timpani
# stroke (a score hit, so the duck lets go there). Everything stops 0.3 s before the downbeat: the suck-out.
track riser  patch=swell gain=-21 hp=4500 hall=0.15 group=fx
track timpr  patch=timp  gain=-20 lp=900 hall=0.3 group=percussion
track subr   patch=glide gain=-19 shape=sine porta=1.2 lp=220 group=drones-pads
track rollh  patch=hats  gain=-24 hp=6000 hall=0.25 pan=-0.15 group=percussion

# the span's bed: the legato bass back under it (below 500 Hz)
play gliderise @$PER1 until=@$SHRUG times=2
auto gbass2 level @$PER1-0.02 -90 @$PER1 0 @$HERO-0.32 0 @$HERO-0.3 -90

# the riser, SHRUG -> HERO-0.3
@$SHRUG       riser D6 >@$HERO-0.3 0.9 source=noise
@$SHRUG       subr  D1 >@$SHRUG+3.2 0.75
@$SHRUG+3.2   subr  A1 >@$SHRUG+5.6 0.85
@$SHRUG+5.6   subr  D2 >@$HERO-0.3 0.95
auto subr level @$SHRUG -9 @$HERO-0.45 0 @$HERO-0.32 0 @$HERO-0.3 -90
arp @$SHRUG   timpr D2 >@$SHRUG+4 step=1/4 order=0,0 vel=0.28 vel_end=0.5 accent=0 accent_vel=0.5
arp @$SHRUG+4 timpr D2 >@$HERO-0.3 step=1/8 order=0,0 vel=0.5 vel_end=0.95 accent=0 accent_vel=0.9
auto timpr level @$HERO-0.32 0 @$HERO-0.3 -90
arp @$SHRUG+3 rollh 9000hz >@$HERO-0.3 step=1/8 order=0 vel=0.15 vel_end=0.8
auto rollh level @$HERO-0.32 0 @$HERO-0.3 -90
@$RUNIN_GAP   timpr A1 1 0.8                                      # in the line gap: the duck lets go
@$RUNIN_GAP   boom  - 1 0.3 freq=40 decay=1.2

# the suck-out: the held layers out for the last 0.3 s (the ride dips too)
auto padwarm level @$HERO-0.32 0 @$HERO-0.3 -90
auto swell   level @$HERO-2.1 -90 @$HERO-0.01 -90 @$HERO 0          # a swell into HERO would sit in the voice's band: the riser replaces it
auto sub     level @$HERO-0.32 0 @$HERO-0.3 -90 @$HERO-0.02 -90
auto glass   level @$PER6 0 @$PER6+0.3 -6 @$HERO-0.32 -6 @$HERO-0.3 -90 @$HERO-0.01 -90 @$HERO 0   # its glint out of V4-47's way
hit @$RUNIN_GAP runin-gap

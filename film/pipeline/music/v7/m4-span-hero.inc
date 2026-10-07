# M4-span-hero: the last switch (perspective span, the subtle one), then ALL ON and its button.
anchor auto $PER1 tempo=120 grid=1/2
anchor auto $DIVIDES
anchor auto $SPAN8_ON
anchor auto $FLIP1
anchor auto $SHRUG
anchor auto $PER6
anchor auto $HERO tempo=120 grid=1/2
anchor auto $ALL_ON_OUT grid=4
# ----------------------------------------------------------------------- 13 SPAN 8, the subtle one
arp @$PER1     glass D4,F4,A4,E5 >@$DIVIDES $SPAN16 vel=0.3 accent=0 accent_vel=0.6     # accents: id's divides
arp @$DIVIDES  glass D4,F4,A4,E5 >@$SPAN8_ON $SPAN16 vel=0.3 accent=0 accent_vel=0.6
arp @$DIVIDES  spark D5,E5,F5,G5,A5,Bb5,C6,D6 2 step=1/8 order=0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15 vel=0.2 vel_end=0.45
arp @$SPAN8_ON glass D4,F4,A4,E5 >@$FLIP1 $SPAN16 vel=0.32 accent=0,8 accent_vel=0.62    # every eight
@$SPAN8_ON     swbell C6 1 0.65                                           # PERSPECTIVE SPAN: 8
# the flip: the metal climbs with the rate; exact glitters; the shrug lands it on 8
arp @$FLIP1    glass D4,F4,A4,E5 >@$SHRUG $SPAN16 vel=0.33
@$FLIP1       anvil D4  1 0.6 decay=0.5
@$FLIP2       anvil G4  1 0.6 decay=0.5
@$FLIP3       anvil C5  1 0.6 decay=0.5
@$FLIP4       anvil F5  1 0.62 decay=0.5
@$FLIP5       anvil Bb5 1 0.64 decay=0.5
arp @$FLIP6    spark D6,F6,A6,D7 >@$SHRUG step=1/16 order=0,1,2,3 vel=0.5
@$SHRUG       shrugb A2 0.22s 0.85                                         # "felt right": A-G, dry
@$SHRUG+0.24  shrugb G2 0.5s 0.6 mute=0.2
arp @$SHRUG    glass D4,F4,A4,E5 >@$PER6 $SPAN16 vel=0.3 accent=0,8 accent_vel=0.55
arp @$PER6     glass D4,F4,A4,E5 >@$HERO $SPAN16 vel=0.36 accent=0,8 accent_vel=0.62
@$HERO-2      swell D4 >@$HERO 0.7

# ----------------------------------------------------------------------- ALL ON: the act's loudest, then the button
@$HERO        crash - 1 1.0 decay=1.6
@$HERO        gong  - 1 0.7 decay=2.5
@$HERO        gongl - 1 0.6 decay=2.5
@$HERO        stab  D3,A3,D4,F4,A4 0.4 1.0
@$HERO        boom  - 1 0.75 freq=40 decay=2
@$HERO        swbell D6 1 0.7
play full4    @$HERO until=@$ALL_ON_OUT                                     # the theme in metal, every layer
auto padwarm level @$HERO-0.3 0 @$HERO-0.02 -90                             # the loop stops for the downbeat
auto sub    level @$HERO-0.01 0 @$HERO -4.5 @$HERO+2 -4.5 @$HERO+2.5 0          # the first bar: less below 150 Hz...
auto boom   level @$HERO-0.01 0 @$HERO -4.5 @$HERO+2 -4.5 @$HERO+2.5 0
auto gongl  level @$HERO-0.01 0 @$HERO -4.5 @$HERO+2 -4.5 @$HERO+2.5 0
auto kick   level @$HERO-0.01 0 @$HERO -4.5 @$HERO+2 -4.5 @$HERO+2.5 0
auto dbass  level @$HERO-0.01 0 @$HERO -3 @$HERO+2 -3 @$HERO+2.5 0
auto stab   level @$HERO-0.01 0 @$HERO 3 @$HERO+2 3 @$HERO+2.5 0            # ...more above it
auto hibell level @$HERO-0.01 0 @$HERO 3 @$HERO+2 3 @$HERO+2.5 0
auto brass  level @$HERO-0.01 0 @$HERO 3 @$HERO+2 3 @$HERO+2.5 0
play herogtr4 @$HERO_ACTION until=@$ALL_ON_OUT                              # guitar2, the action half only
@$HERO_SWITCH crash - 1 0.7 decay=1.2                                       # HERO's internal cut, if it has one (else its downbeat)
play button   @$ALL_ON_OUT                                                  # the button on HERO's out cut
# the button is a stop: the held layers end on the out cut, so the button's hit and its tail are heard alone
auto padt   level @$ALL_ON_OUT-0.02 0 @$ALL_ON_OUT+0.02 -90 @$REST_BAND-0.01 -90 @$REST_BAND 0
auto gbass2 level @$ALL_ON_OUT-0.02 0 @$ALL_ON_OUT+0.02 -90 @$REST_BAND-0.01 -90 @$REST_BAND 0
auto brass  level @$ALL_ON_OUT-0.02 0 @$ALL_ON_OUT+0.02 -90 @$REST_BAND-0.01 -90 @$REST_BAND 0
auto glass  level @$ALL_ON_OUT-0.02 0 @$ALL_ON_OUT+0.02 -90 @$REST_BAND-0.01 -90 @$REST_BAND 0
auto gtr    level @$ALL_ON_OUT+0.25 0 @$ALL_ON_OUT+0.4 -90

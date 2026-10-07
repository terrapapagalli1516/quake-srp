# M3 The proof: the ostinato; BD1 the two voices close in and meet in one unison note; BD2 total
# silence; the ostinato back, deadpan, on "the standard views"; ALL PASS.
anchor auto $T_M3 tempo=90 grid=1/2
@$T_M3        bell  A2 1 0.5 decay=4
@$T_M3        boom  - 1 0.35 freq=45 decay=2
@$T_M3        sub   A1 >@$BD2_BLACK 0.6 attack=1 release=0.1
@$T_M3        drone A2 >@$BD1 0.4 attack=3 release=1.5 evolve=0.06 f_hi=1100
play ost7  @$T_M3 track=bellL until=@$BD1_MERGE                          # id's C, hard left
play ost7  @$T_M3 track=bellR until=@$BD1_MERGE                          # the port, hard right: they fuse
play tick7 @$T_M3 until=@$BD1 vel=0.8
# BD1: the panels slide together; the two voices close in from the sides, a little louder,
# and meet in one unison note as the panels merge
auto bellL pan   @$BD1 0 @$BD1_MERGE 1 @$STANDARD_VIEWS-0.1 1 @$STANDARD_VIEWS 0
auto bellR pan   @$BD1 0 @$BD1_MERGE -1 @$STANDARD_VIEWS-0.1 -1 @$STANDARD_VIEWS 0
auto bellL level @$BD1 0 @$BD1_MERGE 3 @$STANDARD_VIEWS-0.1 3 @$STANDARD_VIEWS 0
auto bellR level @$BD1 0 @$BD1_MERGE 3 @$STANDARD_VIEWS-0.1 3 @$STANDARD_VIEWS 0
@$BD1_MERGE   bellL A5 >@$BD2_BLACK 0.6 decay=4
@$BD1_MERGE   bellR A5 >@$BD2_BLACK 0.6 decay=4
# BD2: hard cut to total silence, every bus, no tails, until the voice; the music stays out
auto master gain @$BD2_BLACK-0.002 1 @$BD2_BLACK 0 @$BD2_END-0.002 0 @$BD2_END 1
# the standard views: back mid-phrase, quiet and deadpan
anchor auto $STANDARD_VIEWS tempo=90 grid=1/2
play ost7r @$STANDARD_VIEWS track=bellL until=@$ALL_PASS vel=0.75
play ost7r @$STANDARD_VIEWS track=bellR until=@$ALL_PASS vel=0.75
play tick7 @$STANDARD_VIEWS until=@$ALL_PASS vel=0.6
@$STANDARD_VIEWS sub A1 >@$ALL_PASS 0.45 attack=0.3 release=0.5
play perc7 @$T_S26 until=@$ALL_PASS                                      # demo1: the percussion
# ten PASS lines type (S27's terminal, under V6-18): a tick each, high (6-8 kHz) and light, above the voice's band
@$PASS1  tick - 0.06 0.28 tone=6000
@$PASS2  tick - 0.06 0.30 tone=6220
@$PASS3  tick - 0.06 0.32 tone=6440
@$PASS4  tick - 0.06 0.34 tone=6660
@$PASS5  tick - 0.06 0.36 tone=6880
@$PASS6  tick - 0.06 0.38 tone=7100
@$PASS7  tick - 0.06 0.40 tone=7320
@$PASS8  tick - 0.06 0.42 tone=7540
@$PASS9  tick - 0.06 0.44 tone=7760
@$PASS10  tick - 0.06 0.46 tone=7980
@$ALL_PASS_CARD hibell A6 1 0.3 decay=2.5                                  # ALL PASS on the card, on "All": a glint
# ALL PASS: the A major chord on the end of "pass", ringing through the act break
@$ALL_PASS    pad   A2,E3,A3,C#4,E4 >@$CLASSIC+0.6 0.42 attack=0.05 release=2.5 cutoff=1500   # on "pass", ringing through the break
@$ALL_PASS    bell  A4,C#5,E5 1 0.45 decay=5
@$ALL_PASS    hibell A5,E6 1 0.35 decay=4
@$ALL_PASS    sub   A1 >@$CLASSIC 0.55 attack=0.1 release=1.5

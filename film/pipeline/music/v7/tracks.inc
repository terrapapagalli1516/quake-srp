# The score's instruments, by stem group. The peaks are bright enough for a phone's speaker; guitar2 plays only in HERO.
# drones-pads
track sub     patch=drone   gain=-21 hall=0.08 duck=0.3 group=drones-pads
track drone   patch=drone2  gain=-12 hall=0.4 group=drones-pads
track air     patch=air     gain=-9  hall=0.35 freq=380 group=drones-pads
track tex     patch=texture gain=-12 hall=0.5 group=drones-pads
track pad     patch=pad     gain=-17 hp=80 hall=0.45 attack=1.5 release=3 group=drones-pads
track padm2   patch=pad     gain=-17 hp=80 hall=0.45 group=drones-pads
track padw    patch=pad     gain=-17 hp=70 hall=0.5 group=drones-pads
track padbd   patch=pad     gain=-16 hp=70 hall=0.12 group=drones-pads   # BD4's held chord: little hall, so it doesn't swell
track padl    patch=pad     gain=-10 hp=80 hall=0.35 group=drones-pads
track padt    patch=pad     gain=-11 hp=80 hall=0.35 group=drones-pads
track padwarm patch=pad     gain=-12 hp=80 hall=0.35 group=drones-pads   # the warm-pad loop, alone: cut on ALL ON's downbeat
track wash    patch=hats    gain=-12 hall=0.5 pan=-0.2 open=1 decay=0.3 group=percussion
track lowbell patch=bell    gain=-17 hall=0.55 decay=6 hum=0.4 group=melodic
track harm    patch=glide   gain=-24 hall=0.4 attack=0.6 release=1.2 group=drones-pads
# melodic
track growl   patch=bass    gain=-17 hall=0.15 cutoff=90 env_amt=1.8 env_decay=1.2 res=0.4 drive=5 decay=2 sustain=0.85 release=1.0 tone=1600 sub=0.8 group=melodic
track dbass   patch=bass    gain=-11 hp=30 room=0.08 group=melodic
track chugs   patch=chug    gain=-15 room=0.1 drive=19 tone=4800 eq_mid=2.5 group=melodic   # saturated: energy above 300 Hz
track bass2   patch=bassg   gain=-10 hp=35 sub=0.28 human_t=3 human_v=0.05 group=melodic
track gtr     patch=guitar2 gain=-15 hp=90 eq_mid=-1.5 eq_lo=-1 room=0.08 group=guitar   # HERO's action half only, still under the theme; mute=1 removes it
# the old guitar, for the A/B: swap this line in for the one above
# track gtr   patch=guitar  gain=-13 hp=90 eq_mid=-1.5 eq_lo=-1 room=0.08 human_t=4 human_v=0.06 group=guitar
track organ   patch=organ   gain=-18 hall=0.45 eq_lo=-2 group=melodic
track wbass   patch=bassg   gain=-12 hp=35 bright=0.3 drive=1.2 sub=0.4 room=0.15 group=melodic
track bell    patch=bell    gain=-15 hall=0.5 group=melodic
track bellL   patch=bell    gain=-16 pan=-1 hall=0.3 decay=1.4 index=3 group=melodic
track bellR   patch=bell    gain=-16 pan=1  hall=0.3 decay=1.4 index=3 group=melodic
track glass   patch=glass   gain=-12 hall=0.35 delay=0.12 delay_time=0.375 decay=0.5 group=melodic
track spark   patch=glass   gain=-15 hall=0.4 delay=0.2 delay_time=0.25 decay=0.6 group=melodic
track fleet   patch=glass   gain=-15 hall=0.4 decay=0.45 group=melodic
track brass   patch=brass   gain=-11 hall=0.3 eq_lo=-2 group=melodic
track stab    patch=stab    gain=-19 hall=0.3 eq_lo=-2 group=melodic
track parab   patch=glide   gain=-20 hall=0.3 porta=0.03 group=melodic
track parabb  patch=glide   gain=-22 hall=0.3 porta=0.03 band=12 band_level=1 group=melodic
track gliss1  patch=glide   gain=-18 hall=0.35 shape=fm porta=0.07 pan=-0.3 group=melodic
track gliss2  patch=glide   gain=-18 hall=0.35 shape=fm porta=0.9 pan=0.3 group=melodic
track gbass   patch=glide   gain=-14 hall=0.1 shape=saw porta=0.15 cutoff=500 drive=2 group=melodic
track gbass2  patch=glide   gain=-15 hall=0.12 shape=saw porta=0.25 cutoff=420 drive=1.8 band=12 band_level=0.5 group=melodic   # the legato bass layer
track bassl   patch=bass    gain=-13 pan=-1 decay=0.2 sustain=0 release=0.08 group=melodic
track shrugb  patch=bassg   gain=-9  bright=0.25 mute=0.3 group=melodic
# percussion
track boom    patch=impact  gain=-12 hall=0.25 group=percussion
track thud    patch=impact  gain=-15 hall=0.2 decay=0.7 freq=60 noise=0.2 group=percussion
track kick    patch=kick2   gain=-11 human_t=3 human_v=0.08 group=percussion
track snare   patch=snare2  gain=-13 room=0.25 hall=0.08 human_t=4 human_v=0.07 group=percussion
track hat     patch=hats    gain=-19 pan=0.25 human_t=5 human_v=0.12 group=percussion
track crash   patch=crash   gain=-18 hall=0.15 tone=1.25 eq_hi=3 group=percussion      # brighter (the phone)
track clank   patch=clank   gain=-14 room=0.45 hall=0.15 group=percussion
track anvil   patch=strike  gain=-15 hall=0.3 group=percussion
track tick    patch=tick    gain=-9  room=0.25 tone=2400 group=percussion
track snap    patch=tick    gain=-8  pan=-1 tone=1800 decay=0.006 group=percussion
track rust    patch=ticks   gain=-6  room=0.2 tone=2400 group=percussion
track slopt   patch=ticks   gain=-10 room=0.2 tone=4600 group=percussion
track tom     patch=tom     gain=-12 room=0.2 hall=0.1 human_t=4 group=percussion
track timp    patch=timp    gain=-14 hall=0.35 human_v=0.05 group=percussion
track gong    patch=crash   gain=-19 decay=6 tone=0.5 hall=0.4 group=percussion
track gongl   patch=strike  gain=-16 freq=65 decay=6 hall=0.4 group=percussion
# fx
track swell   patch=swell   gain=-11 hall=0.2 group=fx
track clock   patch=ticks   gain=-9  hall=0.1 group=fx
track swbell  patch=bell    gain=-14 hall=0.45 ratio=3.5 index=4.5 decay=2.5 group=melodic   # a switch's bell, a step higher each time
track hibell  patch=bell    gain=-15 hall=0.5 decay=2.2 index=3 group=melodic                 # the theme an octave up at the peaks
track hatl    patch=hats    gain=-4  pan=0.25 human_t=5 human_v=0.12 group=percussion  # NO FRAME CAP's layer, audible
track cardbell patch=bell   gain=-15 hall=0.5 decay=2.2 index=3 group=melodic         # the end card's voice above 300 Hz

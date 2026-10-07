# The score's material: the theme and the metal (patterns-metal.inc), and the parts of the peaks.
include patterns-metal.inc

pattern full4 length=16         # ALL ON: the full statement, with the theme an octave up on the bells (for a phone's speaker)
  play full 0
  play motif  0 track=hibell transpose=36 vel=0.7
  play answer 8 track=hibell transpose=36 vel=0.75
end

pattern button length=1         # the button on HERO's out: one short D, and its tail
  0  stab   D3,A3,D4,F4,A4 0.3 1.0
  0  boom   - 1 0.7 freq=40 decay=1.2
  0  crash  - 1 0.8 decay=0.9
  0  hibell D5,A5,D6 1 0.7 decay=1.6
  0  chugs  D2 0.25 1.0 mute=0.3
  0  dbass  D2 0.25 1.0
  0  kick   - 1 1.0
end

pattern herogtr4 length=8       # guitar2 in HERO's action half: the gallop, then the D rung
  play gtrgallop 0
  play gtrgallop 4 transpose=-4
end

pattern warmrise length=32      # the warm loop after the burst, rising: Dm9 Fmaj7 Bbmaj7 Cadd9 (no C#, no B: the theme's Bb sits in it)
  0   padt D3,A3,E4,F4   8 0.6 attack=1.5 release=2 cutoff=700 lfo_rate=0.4 lfo_depth=0.7 lfo2_rate=0.3 lfo2_depth=0.5
  8   padt F2,C3,E3,A3   8 0.6 attack=1.5 release=2 cutoff=700 lfo_rate=0.4 lfo_depth=0.7 lfo2_rate=0.3 lfo2_depth=0.5
  16  padt Bb2,F3,A3,D4  8 0.6 attack=1.5 release=2 cutoff=750 lfo_rate=0.4 lfo_depth=0.7 lfo2_rate=0.3 lfo2_depth=0.5
  24  padt C3,G3,D4,E4   8 0.62 attack=1.5 release=2 cutoff=800 lfo_rate=0.4 lfo_depth=0.7 lfo2_rate=0.3 lfo2_depth=0.5
end

pattern gliderise length=32     # the legato bass under it
  0   gbass2 D2  6 0.75
  6   gbass2 C2  2 0.7
  8   gbass2 F1  6 0.75
  14  gbass2 A1  2 0.7
  16  gbass2 Bb1 6 0.75
  22  gbass2 A1  2 0.7
  24  gbass2 C2  8 0.75
end

pattern chugoct length=4        # the gallop an octave up, palm-muted, under the main one (the phone)
  0    chugs D3 1/2 0.6 mute=0.9
  0.5  chugs D3 1/4 0.45 mute=0.95
  0.75 chugs D3 1/4 0.46 mute=0.95
  1    chugs D3 1/2 0.55 mute=0.9
  1.5  chugs D3 1/4 0.45 mute=0.95
  1.75 chugs D3 1/4 0.46 mute=0.95
  2    chugs D3 1/2 0.58 mute=0.9
  2.5  chugs D3 1/4 0.45 mute=0.95
  2.75 chugs D3 1/4 0.46 mute=0.95
  3    chugs D3 1/2 0.55 mute=0.9
  3.5  chugs D3 1/4 0.46 mute=0.95
  3.75 chugs D3 1/4 0.5 mute=0.95
end

pattern sgallopx length=4       # the gallop, doubled an octave up
  play sgallop 0
  play chugoct 0
end

pattern codadrums length=4      # the coda's light figure: kick on 1 and 3, a soft snare on 2 and 4, hats in eighths
  0  kick 50hz 1/2 0.7
  2  kick 50hz 1/2 0.6
  1  snare - 1 0.42
  3  snare - 1 0.46
  arp 0 hat 9000hz 4 step=1/2 order=0,0 vel=0.3 accent=0 accent_vel=0.42
end

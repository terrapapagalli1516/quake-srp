# The synth metal and the build's layers, on top of the theme (patterns-theme.inc).
include patterns-theme.inc


# ---------------------------------------------------------------- the synth metal (no guitar):
# the chug voice, the distorted synth bass, the drums
pattern schord2 length=4        # a power chord rung for two beats, then palm-muted sixteenths
  0     chugs D2 2 1.0 mute=0.1
  0     dbass D2 2 1.0
  arp 2 chugs D2 2 step=1/4 order=0 vel=0.8 accent=0,4 accent_vel=0.95 mute=0.9
  arp 2 dbass D2 2 step=1/2 order=0 vel=0.85
end

pattern sriff length=4          # chugs, a minor-third stab, a flat-second stab
  0     chugs D2  1/2 1.0  mute=0.2
  0     dbass D2  1/2 1.0
  arp 0.5 chugs D2 1 step=1/4 order=0 vel=0.78 mute=0.9
  arp 0.5 dbass D2 1 step=1/2 order=0 vel=0.75
  1.5   chugs F2  1/2 0.95 mute=0.2
  1.5   dbass F2  1/2 0.95
  arp 2   chugs D2 1 step=1/4 order=0 vel=0.78 mute=0.9
  arp 2   dbass D2 1 step=1/2 order=0 vel=0.75
  3     chugs Eb2 1/2 1.0  mute=0.25
  3     dbass Eb2 1/2 1.0
  arp 3.5 chugs D2 1/2 step=1/4 order=0 vel=0.78 mute=0.9
  3.5   dbass D2  1/2 0.75
end

pattern sgallop length=4        # the gallop: an eighth and two sixteenths, on a root (play with transpose)
  0    chugs D2 1/2 1.0
  0.5  chugs D2 1/4 0.7
  0.75 chugs D2 1/4 0.72
  1    chugs D2 1/2 0.9
  1.5  chugs D2 1/4 0.7
  1.75 chugs D2 1/4 0.72
  2    chugs D2 1/2 0.95
  2.5  chugs D2 1/4 0.7
  2.75 chugs D2 1/4 0.72
  3    chugs D2 1/2 0.9
  3.5  chugs D2 1/4 0.72
  3.75 chugs D2 1/4 0.78
  0    dbass D2 1/2 0.95
  1    dbass D2 1/2 0.85
  2    dbass D2 1/2 0.9
  3    dbass D2 1/2 0.85
end

pattern gtrgallop length=4      # guitar2's gallop, low under the synth one (HERO only)
  0    gtr D2 1/2 1.0 mute=0.85
  0.5  gtr D2 1/4 0.7 mute=0.9
  0.75 gtr D2 1/4 0.72 mute=0.9
  1    gtr D2 1/2 0.9 mute=0.85
  1.5  gtr D2 1/4 0.7 mute=0.9
  1.75 gtr D2 1/4 0.72 mute=0.9
  2    gtr D2 1/2 0.95 mute=0.85
  2.5  gtr D2 1/4 0.7 mute=0.9
  2.75 gtr D2 1/4 0.72 mute=0.9
  3    gtr D2 1/2 0.9 mute=0.85
  3.5  gtr D2 1/4 0.72 mute=0.9
  3.75 gtr D2 1/4 0.78 mute=0.9
end

pattern warmprog length=32      # the warm pad, from the torches on: Dm9 Bbmaj7 Gm7 A7, two LFOs at 1 : 0.75
  0   padt D3,A3,E4,F4   8 0.6 attack=1.5 release=2 cutoff=650 lfo_rate=0.4 lfo_depth=0.7 lfo2_rate=0.3 lfo2_depth=0.5
  8   padt Bb2,F3,A3,D4  8 0.6 attack=1.5 release=2 cutoff=650 lfo_rate=0.4 lfo_depth=0.7 lfo2_rate=0.3 lfo2_depth=0.5
  16  padt G2,D3,F3,Bb3  8 0.6 attack=1.5 release=2 cutoff=650 lfo_rate=0.4 lfo_depth=0.7 lfo2_rate=0.3 lfo2_depth=0.5
  24  padt A2,E3,G3,C#4  8 0.62 attack=1.5 release=2 cutoff=700 lfo_rate=0.4 lfo_depth=0.7 lfo2_rate=0.3 lfo2_depth=0.5
end

pattern glideprog length=32     # the legato bass, under the warm pad (gated in at SMOOTH_ON)
  0   gbass2 D2  6 0.75
  6   gbass2 C2  2 0.7
  8   gbass2 Bb1 6 0.75
  14  gbass2 C2  2 0.7
  16  gbass2 G1  6 0.75
  22  gbass2 Bb1 2 0.7
  24  gbass2 A1  8 0.75
end

pattern gruntsteps length=20.8  # stepped notes on the grunt's thirteen steps, 0.8 s (1.6 beats) apart
  0.0  bass2 D2  0.4 0.8 mute=0.5
  1.6  bass2 D2  0.4 0.7 mute=0.5
  3.2  bass2 F2  0.4 0.8 mute=0.5
  4.8  bass2 D2  0.4 0.7 mute=0.5
  6.4  bass2 A1  0.4 0.8 mute=0.5
  8.0  bass2 A1  0.4 0.7 mute=0.5
  9.6  bass2 C2  0.4 0.8 mute=0.5
  11.2 bass2 A1  0.4 0.7 mute=0.5
  12.8 bass2 Bb1 0.4 0.8 mute=0.5
  14.4 bass2 Bb1 0.4 0.7 mute=0.5
  16.0 bass2 A1  0.4 0.8 mute=0.5
  17.6 bass2 G1  0.4 0.7 mute=0.5
  19.2 bass2 A1  0.4 0.8 mute=0.5
end

pattern kicks length=4          # the kick under the chugs, following their accents
  0    kick - 1 1.0
  0.5  kick - 1 0.72
  0.75 kick - 1 0.72
  2    kick - 1 0.92
  2.5  kick - 1 0.72
  3.5  kick - 1 0.75
end

pattern burstband length=32     # the burst: the D chord rung, then the riff (the synth chug and the bass)
  play schord2 0
  play sriff 4
  play sriff 8
  play sriff 12
  play sriff 16
  play sriff 20
  play sriff 24
  play sriff 28
end

pattern arc length=1            # a glissando up and down a jump's parabola
  0     gliss1 m69    0.17s 0.5
  0.12  gliss1 m73    0.17s 0.5
  0.25  gliss1 m75.5  0.17s 0.5
  0.37  gliss1 m76    0.17s 0.5
  0.5   gliss1 m75.5  0.17s 0.5
  0.62  gliss1 m73    0.17s 0.5
  0.75  gliss1 m69    0.2s  0.5
end

pattern arc480 length=1         # the same jump at 480 Hz: higher
  0     gliss2 m69    0.17s 0.5 porta=0.08
  0.12  gliss2 m74    0.17s 0.5
  0.25  gliss2 m77    0.17s 0.5
  0.37  gliss2 m78    0.17s 0.5
  0.5   gliss2 m77    0.17s 0.5
  0.62  gliss2 m74    0.17s 0.5
  0.75  gliss2 m69    0.2s  0.5
end

pattern full length=16          # ALL OF IT: the theme in metal, every layer (Dm F Bb C); no guitar

  play motif  0 track=brass transpose=12 vel=1.0
  play answer 8 track=brass transpose=12 vel=1.0
  play motif  0 track=bell transpose=24 vel=0.6 decay=2.5
  play answer 8 track=bell transpose=24 vel=0.6 decay=2.5
  play sgallop 0
  play sgallop 4 transpose=3
  play sgallop 8 transpose=-4
  play sgallop 12 transpose=-2
  play drumsD 0
  play drumsD 4
  play drumsD 8
  play drumsD 12
  0   padt   D3,A3,D4,F4   4 0.75 attack=0.1 release=1 cutoff=2200
  4   padt   F3,A3,C4,F4   4 0.75 attack=0.1 release=1 cutoff=2200
  8   padt   Bb2,F3,Bb3,D4 4 0.78 attack=0.1 release=1 cutoff=2400
  12  padt   C3,G3,C4,E4   4 0.8 attack=0.1 release=1 cutoff=2600
  0   gbass2 D2  4 0.8
  4   gbass2 F2  4 0.8
  8   gbass2 Bb1 4 0.8
  12  gbass2 C2  4 0.8
  arp 0  glass D5,F5,A5,D6  4 step=1/4 order=0,1,2,3,2,1,2,3 vel=0.32
  arp 4  glass F5,A5,C6,F6  4 step=1/4 order=0,1,2,3,2,1,2,3 vel=0.32
  arp 8  glass D5,F5,Bb5,D6 4 step=1/4 order=0,1,2,3,2,1,2,3 vel=0.34
  arp 12 glass E5,G5,C6,E6  4 step=1/4 order=0,1,2,3,2,1,2,3 vel=0.36
  arp 0  hat 9000hz 16 step=1/4 order=0 vel=0.26 accent=0,4,8,12 accent_vel=0.4
  arp 0  wash 9000hz 16 step=2 order=0 vel=0.5
end

pattern landing length=8        # after the statement: on D, the gallop driving on (the layers can leave one by one)
  play sgallop 0
  play sgallop 4
  play drumsD 0
  play drumsD 4
  0   padt   D3,A3,D4,F#4  8 0.8 attack=0.1 release=1.5 cutoff=2600
  0   gbass2 D2  8 0.8
  0   chugs  D2  0.5 1.0 mute=0
  arp 0  glass D5,F#5,A5,D6 8 step=1/4 order=0,1,2,3,2,1,2,3 vel=0.34
  arp 0  hat 9000hz 8 step=1/4 order=0 vel=0.26 accent=0,4,8,12 accent_vel=0.4
  arp 0  wash 9000hz 8 step=2 order=0 vel=0.5
end

pattern herogtr length=16       # guitar2, low, under HERO's synth gallop: the film's one guitar
  play gtrgallop 0
  play gtrgallop 4 transpose=3
  play gtrgallop 8 transpose=-4
  play gtrgallop 12 transpose=-2
end

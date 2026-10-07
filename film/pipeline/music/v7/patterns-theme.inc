# The theme and its first material: the motif and its answer, the 7/8 ostinato, the riffs, the drums.
pattern motif length=8          # the theme: D A Bb G A
  0    D3  2    0.9
  2    A3  1    0.8
  3    Bb3 1/2  0.85
  3.5  G3  1/2  0.75
  4    A3  4    0.85
end

pattern motif4 length=4         # its first four notes
  0    D3  2    0.9
  2    A3  1    0.8
  3    Bb3 1/2  0.85
  3.5  G3  1/2  0.75
end

pattern answer length=8         # the end turns upward: Bb C D
  0    D3  2    0.9
  2    A3  1    0.8
  3    Bb3 1/2  0.85
  3.5  C4  1/2  0.85
  4    D4  4    0.95
end

pattern tick7 length=3.5        # eighths in 7/8, accents on the 2+2+3 groups
  0    tick - 0.1 0.6
  0.5  tick - 0.1 0.3
  1    tick - 0.1 0.45
  1.5  tick - 0.1 0.3
  2    tick - 0.1 0.45
  2.5  tick - 0.1 0.3
  3    tick - 0.1 0.35
end

pattern ost7 length=7           # the motif in A, folded into two bars of 7/8
  0    A5  1    0.5
  1    E5  1/2  0.4
  1.5  F5  1/2  0.42
  2    D5  1/2  0.4
  2.5  E5  1    0.45
  3.5  A5  1    0.5
  4.5  E5  1/2  0.4
  5    F5  1/2  0.42
  5.5  G5  1/2  0.42
  6    E5  1/2  0.4
  6.5  C5  1/2  0.38
end

pattern ost7r length=7          # the same, from its second bar: back mid-phrase, as if nothing happened
  0    A5  1    0.5
  1    E5  1/2  0.4
  1.5  F5  1/2  0.42
  2    G5  1/2  0.42
  2.5  E5  1/2  0.4
  3    C5  1/2  0.38
  3.5  A5  1    0.5
  4.5  E5  1/2  0.4
  5    F5  1/2  0.42
  5.5  D5  1/2  0.4
  6    E5  1    0.45
end

pattern perc7 length=3.5
  0    thud  - 1   0.45
  0    hat   - 0.1 0.35
  0.5  hat   - 0.1 0.22
  1    hat   - 0.1 0.3
  1.5  hat   - 0.1 0.22
  2    clank - 1   0.3 freq=600
  2    hat   - 0.1 0.3
  2.5  hat   - 0.1 0.22
  3    hat   - 0.1 0.26
end

pattern poses length=9          # S46a: the same notes, held (left) and blended (right)
  0 A1 1 0.7
  1 C2 1 0.65
  2 D2 1 0.65
  3 E2 1 0.7
  4 G2 1 0.7
  5 E2 1 0.65
  6 D2 1 0.65
  7 C2 1 0.65
  8 A1 1 0.7
end

pattern lift length=12          # M2 after the wipe: Dorian (the raised sixth), the glass enters wide
  0      padm2 D3,F3,A3,B3   4 0.6 attack=0.3 release=2
  4      padm2 G2,D3,G3,B3   4 0.6 attack=0.5 release=2
  8      padm2 Bb2,F3,A3,D4  4 0.6 attack=0.5 release=1.5
  arp 0      glass D4,F4,A4,B4   4 step=1/2 order=0,1,2,3,4,3,2,1 vel=0.3 pan=-0.6
  arp 0.25   spark D5,A5,B5      4 step=1/2 order=0,1,2,1 vel=0.25 pan=0.6
  arp 4      glass G3,B3,D4,A4   4 step=1/2 order=0,1,2,3,4,3,2,1 vel=0.3 pan=-0.6
  arp 4.25   spark G5,B5,D6      4 step=1/2 order=0,1,2,1 vel=0.25 pan=0.6
  arp 8      glass Bb3,D4,F4,A4  4 step=1/2 order=0,1,2,3,4,3,2,1 vel=0.28 pan=-0.6
  arp 8.25   spark F5,A5,D6      4 step=1/2 order=0,1,2,1 vel=0.22 pan=0.6
end

let SPAN16 = step=1/4 order=0,1,2,3,4,5,6,7,8,7,6,5,4,3,2,1

# ---------------------------------------------------------------- metal
pattern riff length=4           # D: chugs, a minor-third stab, a flat-second stab
  0     gtr   D2  1/2 1.0  mute=0.1
  0     bass2 D1  1/2 1.0
  arp 0.5 gtr   D2 1 step=1/4 order=0 vel=0.8 mute=0.9
  arp 0.5 bass2 D1 1 step=1/4 order=0 vel=0.8 mute=0.9
  1.5   gtr   F2  1/2 0.95 mute=0.1
  1.5   bass2 F1  1/2 0.95
  arp 2   gtr   D2 1 step=1/4 order=0 vel=0.8 mute=0.9
  arp 2   bass2 D1 1 step=1/4 order=0 vel=0.8 mute=0.9
  3     gtr   Eb2 1/2 1.0  mute=0.2
  3     bass2 Eb1 1/2 1.0
  arp 3.5 gtr   D2 1/2 step=1/4 order=0 vel=0.8 mute=0.9
  arp 3.5 bass2 D1 1/2 step=1/4 order=0 vel=0.8 mute=0.9
end

pattern gallop length=4         # the gallop, on a root: play with transpose
  arp 0 gtr   D2 4 step=1/4 order=0 vel=0.75 accent=0,4,8,12 accent_vel=1.0 mute=0.8
  arp 0 bass2 D1 4 step=1/2 order=0 vel=0.85 mute=0.6
end

pattern chord2 length=4         # a power chord rung for two beats, then chugs
  0     gtr   D2 2 1.0 mute=0
  0     bass2 D1 2 1.0
  arp 2 gtr   D2 2 step=1/4 order=0 vel=0.8 mute=0.9
  arp 2 bass2 D1 2 step=1/2 order=0 vel=0.85 mute=0.6
end

pattern drums length=4          # the kick follows the chugs; snare on 2 and 4
  arp 0 hat 9000hz 3.5 step=1/2 order=0 vel=0.38 accent=0,2,4,6 accent_vel=0.55
  3.5  hat   - 1/2 0.5 open=0.6
  0    kick  - 1 1.0
  0.5  kick  - 1 0.75
  0.75 kick  - 1 0.75
  2    kick  - 1 0.9
  2.5  kick  - 1 0.75
  3.5  kick  - 1 0.75
  1    snare - 1 1.0
  3    snare - 1 1.0
end

pattern drumsD length=4         # double kick in sixteenths
  arp 0 kick 50hz 4 step=1/4 order=0 vel=0.72 accent=0,8 accent_vel=0.95
  arp 0 hat 9000hz 4 step=1/2 order=0 vel=0.36 accent=0,2,4,6 accent_vel=0.5
  1 snare - 1 1.0
  3 snare - 1 1.0
end

pattern double length=4         # double time: hats in thirty-seconds, the snare on every beat
  arp 0 kick 50hz 4 step=1/4 order=0 vel=0.75 accent=0,4,8,12 accent_vel=0.95
  arp 0 hat 9000hz 4 step=1/8 order=0 vel=0.3 accent=0,4,8,12,16,20,24,28 accent_vel=0.5
  0 snare - 1 0.9
  1 snare - 1 1.0
  2 snare - 1 0.95
  3 snare - 1 1.0
end

pattern airy length=4           # half time, the cymbals washing
  0 kick  - 1 0.85
  2 snare - 1 0.8
  0 hat   - 1 0.45 open=1
  1 hat   - 1 0.35 open=1
  2 hat   - 1 0.45 open=1
  3 hat   - 1 0.35 open=1
end

pattern chant length=1          # "TOR-CHES!"
  0    stab  D3,A3,D4,F#4,A4 0.45 1.0
  0.5  stab  D3,A3,D4,F#4,A4 0.3  0.85
  0    timp  D2 1 1.0
  0.5  timp  A1 1 0.85
end

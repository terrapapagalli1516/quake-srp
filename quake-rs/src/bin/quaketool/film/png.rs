//! PNG files without a dependency: a deflate stream of LZ77 matches in the
//! fixed Huffman codes (RFC 1951 §3.2.6), in a zlib wrapper (RFC 1950), in
//! PNG chunks with their CRCs (the PNG spec, ISO/IEC 15948). Each row takes
//! the filter whose bytes sum smallest (as libpng's heuristic). Not as small
//! as zlib's dynamic codes, but a third to a tenth of the raw pixels for a
//! frame of Quake, and a mostly empty title card is a few kilobytes.

/// An RGB (`channels` 3) or RGBA (4) image, `w x h`, row-major, as PNG bytes.
pub fn encode(w: usize, h: usize, channels: usize, pixels: &[u8]) -> Vec<u8> {
    assert!(channels == 3 || channels == 4, "RGB or RGBA");
    assert_eq!(pixels.len(), w * h * channels, "the pixels fill the image");
    let stride = w * channels;
    // The filtered rows, each led by its filter's byte.
    let mut raw = Vec::with_capacity(h * (stride + 1));
    let mut cand = vec![0u8; stride];
    let mut best = vec![0u8; stride];
    let zero = vec![0u8; stride];
    for y in 0..h {
        let row = &pixels[y * stride..(y + 1) * stride];
        let up = if y > 0 { &pixels[(y - 1) * stride..y * stride] } else { &zero[..] };
        let mut best_score = u64::MAX;
        let mut best_filter = 0u8;
        for filter in 0..5u8 {
            for i in 0..stride {
                let a = if i >= channels { row[i - channels] } else { 0 };
                let b = up[i];
                let c = if i >= channels { up[i - channels] } else { 0 };
                cand[i] = row[i].wrapping_sub(match filter {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                    _ => paeth(a, b, c),
                });
            }
            let score: u64 = cand.iter().map(|&v| u64::from((v as i8).unsigned_abs())).sum();
            if score < best_score {
                (best_score, best_filter) = (score, filter);
                best.copy_from_slice(&cand);
            }
        }
        raw.push(best_filter);
        raw.extend_from_slice(&best);
    }
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend((w as u32).to_be_bytes());
    ihdr.extend((h as u32).to_be_bytes());
    ihdr.extend([8, if channels == 4 { 6 } else { 2 }, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &zlib(&raw));
    chunk(&mut png, b"IEND", &[]);
    png
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = ((p - i16::from(a)).abs(), (p - i16::from(b)).abs(), (p - i16::from(c)).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    let at = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[at..]);
    out.extend(crc.to_be_bytes());
}

/// CRC-32 (ISO 3309, the PNG chunks').
pub fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// Adler-32 (RFC 1950, zlib's).
pub fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for block in bytes.chunks(5552) {
        for &x in block {
            a += u32::from(x);
            b += a;
        }
        (a, b) = (a % 65521, b % 65521);
    }
    (b << 16) | a
}

/// Bits out, least significant first (deflate's order).
struct Bits {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl Bits {
    fn put(&mut self, value: u32, bits: u32) {
        self.acc |= u64::from(value) << self.n;
        self.n += bits;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }

    /// A Huffman code, sent most significant bit first.
    fn code(&mut self, code: u32, bits: u32) {
        self.put(code.reverse_bits() >> (32 - bits), bits);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

/// The fixed code of literal/length symbol `sym` (RFC 1951 §3.2.6).
fn put_lit(bits: &mut Bits, sym: u32) {
    match sym {
        0..=143 => bits.code(0x30 + sym, 8),
        144..=255 => bits.code(0x190 + sym - 144, 9),
        256..=279 => bits.code(sym - 256, 7),
        _ => bits.code(0xC0 + sym - 280, 8),
    }
}

/// Lengths 3..=258: base and extra bits of symbols 257..=285.
const LEN_BASE: [u16; 29] =
    [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
/// Distances 1..=32768: base and extra bits of codes 0..=29.
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145,
    8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] =
    [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];

const WINDOW: usize = 32768;
const MAX_MATCH: usize = 258;
const HASH_BITS: u32 = 15;
/// How many earlier places with the same three bytes a match tries.
const CHAIN: usize = 8;

/// `data` as a zlib stream: one deflate block in the fixed codes.
pub fn zlib(data: &[u8]) -> Vec<u8> {
    let mut bits = Bits { out: vec![0x78, 0x01], acc: 0, n: 0 };
    bits.put(1, 1); // BFINAL
    bits.put(1, 2); // BTYPE 01: fixed Huffman codes
    let mut head = vec![usize::MAX; 1 << HASH_BITS];
    let mut prev = vec![usize::MAX; WINDOW];
    let hash = |i: usize| {
        let v = u32::from(data[i]) | u32::from(data[i + 1]) << 8 | u32::from(data[i + 2]) << 16;
        (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    };
    let insert = |i: usize, head: &mut Vec<usize>, prev: &mut Vec<usize>| {
        if i + 3 <= data.len() {
            let h = hash(i);
            prev[i % WINDOW] = head[h];
            head[h] = i;
        }
    };
    let mut i = 0;
    while i < data.len() {
        // The longest match among the last few places with these three bytes.
        let (mut best_len, mut best_dist) = (0, 0);
        if i + 3 <= data.len() {
            let mut cand = head[hash(i)];
            let limit = (data.len() - i).min(MAX_MATCH);
            for _ in 0..CHAIN {
                if cand == usize::MAX || i - cand > WINDOW - 1 || cand >= i {
                    break;
                }
                let len = data[cand..].iter().zip(&data[i..i + limit]).take_while(|(a, b)| a == b).count();
                if len > best_len {
                    (best_len, best_dist) = (len, i - cand);
                    if len == limit {
                        break;
                    }
                }
                let next = prev[cand % WINDOW];
                if next == usize::MAX || next >= cand {
                    break;
                }
                cand = next;
            }
        }
        if best_len >= 3 {
            let k = LEN_BASE.iter().rposition(|&b| usize::from(b) <= best_len).unwrap_or(0);
            put_lit(&mut bits, 257 + k as u32);
            bits.put((best_len - usize::from(LEN_BASE[k])) as u32, u32::from(LEN_EXTRA[k]));
            let d = DIST_BASE.iter().rposition(|&b| usize::from(b) <= best_dist).unwrap_or(0);
            bits.code(d as u32, 5);
            bits.put((best_dist - usize::from(DIST_BASE[d])) as u32, u32::from(DIST_EXTRA[d]));
            for j in i..i + best_len {
                insert(j, &mut head, &mut prev);
            }
            i += best_len;
        } else {
            put_lit(&mut bits, u32::from(data[i]));
            insert(i, &mut head, &mut prev);
            i += 1;
        }
    }
    put_lit(&mut bits, 256);
    let mut out = bits.finish();
    out.extend(adler32(data).to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader for what [`zlib`] writes (a fixed-code block), to check it
    /// round trips.
    fn inflate(z: &[u8]) -> Vec<u8> {
        assert_eq!(&z[..2], &[0x78, 0x01]);
        assert_eq!((u32::from(z[0]) * 256 + u32::from(z[1])) % 31, 0, "the zlib header's check");
        let data = &z[2..z.len() - 4];
        let mut pos = 0usize;
        let mut bit = |n: u32| -> u32 {
            let mut v = 0;
            for k in 0..n {
                v |= u32::from(data[pos / 8] >> (pos % 8) & 1) << k;
                pos += 1;
            }
            v
        };
        assert_eq!((bit(1), bit(2)), (1, 1));
        let mut out: Vec<u8> = Vec::new();
        loop {
            // Read a fixed literal/length code, MSB first.
            let mut code = 0u32;
            let mut len = 0;
            let sym = loop {
                code = code << 1 | bit(1);
                len += 1;
                match len {
                    7 if code <= 0x17 => break code + 256,
                    8 if (0x30..=0xBF).contains(&code) => break code - 0x30,
                    8 if (0xC0..=0xC7).contains(&code) => break code - 0xC0 + 280,
                    9 if (0x190..=0x1FF).contains(&code) => break code - 0x190 + 144,
                    9 => panic!("bad code"),
                    _ => {}
                }
            };
            match sym {
                0..=255 => out.push(sym as u8),
                256 => break,
                _ => {
                    let k = (sym - 257) as usize;
                    let length = usize::from(LEN_BASE[k]) + bit(u32::from(LEN_EXTRA[k])) as usize;
                    let mut d = 0;
                    for _ in 0..5 {
                        d = d << 1 | bit(1);
                    }
                    let dist = usize::from(DIST_BASE[d as usize]) + bit(u32::from(DIST_EXTRA[d as usize])) as usize;
                    for _ in 0..length {
                        out.push(out[out.len() - dist]);
                    }
                }
            }
        }
        assert_eq!(adler32(&out).to_be_bytes(), z[z.len() - 4..], "the stream's Adler-32");
        out
    }

    #[test]
    fn deflate_round_trips() {
        let mut data: Vec<u8> = (0..20000u32).map(|i| (i * 7 % 13) as u8).collect();
        data.extend([9u8; 1000]);
        data.extend((0..5000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8));
        data.extend_from_slice(b"abcabcabcabcabcabd");
        for d in [&data[..], &data[..2], &[][..], &b"aaa"[..]] {
            let z = zlib(d);
            assert_eq!(inflate(&z), d);
        }
        assert!(zlib(&data).len() < data.len() / 2, "repeats compress");
    }

    #[test]
    fn checksums_are_the_standard_ones() {
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn a_png_has_its_chunks() {
        let px: Vec<u8> = (0..4 * 3 * 3).map(|i| i as u8).collect();
        let png = encode(4, 3, 3, &px);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 4);
        assert_eq!(&png[png.len() - 12..], &[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]);
        // The IDAT's rows unfilter back to the pixels.
        let idat_len = u32::from_be_bytes(png[33..37].try_into().unwrap()) as usize;
        let raw = inflate(&png[41..41 + idat_len]);
        let mut rows: Vec<u8> = Vec::new();
        for y in 0..3 {
            let line = &raw[y * 13..(y + 1) * 13];
            for i in 0..12 {
                let a = if i >= 3 { rows[y * 12 + i - 3] } else { 0 };
                let b = if y > 0 { rows[(y - 1) * 12 + i] } else { 0 };
                let c = if y > 0 && i >= 3 { rows[(y - 1) * 12 + i - 3] } else { 0 };
                let pred = match line[0] {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                    _ => paeth(a, b, c),
                };
                rows.push(line[1 + i].wrapping_add(pred));
            }
        }
        assert_eq!(rows, px);
    }
}

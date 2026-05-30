//! CRC-16 (CCITT, non-reflected, polynomial 0x1021).
//!
//! Ported from Quake (GPLv2) — `WinQuake/crc.c` and `WinQuake/crc.h`,
//! Copyright (C) 1996-1997 Id Software, Inc.
//!
//! The original comment in `crc.c` describes this as the "CCITT standard CRC
//! used by XMODEM", with polynomial `0x1021`, init `0xffff`, final XOR
//! `0x0000`, and no reflection. That parameter set (init `0xffff`, xorout
//! `0x0000`) is what is commonly called **CRC-16/CCITT-FALSE**; true XMODEM
//! uses init `0x0000`. We faithfully reproduce Quake's behavior here (init
//! `0xffff`), so the standard CCITT-FALSE check value `0x29B1` applies.
//!
//! The C used `unsigned short` (16-bit) arithmetic; the per-byte update is
//!
//! ```text
//! *crc = (*crc << 8) ^ crctable[(*crc >> 8) ^ data];
//! ```
//!
//! which we reproduce with masked/wrapping `u16` operations.

/// The exact 256-entry CRC table copied verbatim from `crc.c`.
///
/// Precomputed for polynomial `0x1021`, non-reflected (MSB-first).
const CRC_TABLE: [u16; 256] = [
    0x0000, 0x1021, 0x2042, 0x3063, 0x4084, 0x50a5, 0x60c6, 0x70e7,
    0x8108, 0x9129, 0xa14a, 0xb16b, 0xc18c, 0xd1ad, 0xe1ce, 0xf1ef,
    0x1231, 0x0210, 0x3273, 0x2252, 0x52b5, 0x4294, 0x72f7, 0x62d6,
    0x9339, 0x8318, 0xb37b, 0xa35a, 0xd3bd, 0xc39c, 0xf3ff, 0xe3de,
    0x2462, 0x3443, 0x0420, 0x1401, 0x64e6, 0x74c7, 0x44a4, 0x5485,
    0xa56a, 0xb54b, 0x8528, 0x9509, 0xe5ee, 0xf5cf, 0xc5ac, 0xd58d,
    0x3653, 0x2672, 0x1611, 0x0630, 0x76d7, 0x66f6, 0x5695, 0x46b4,
    0xb75b, 0xa77a, 0x9719, 0x8738, 0xf7df, 0xe7fe, 0xd79d, 0xc7bc,
    0x48c4, 0x58e5, 0x6886, 0x78a7, 0x0840, 0x1861, 0x2802, 0x3823,
    0xc9cc, 0xd9ed, 0xe98e, 0xf9af, 0x8948, 0x9969, 0xa90a, 0xb92b,
    0x5af5, 0x4ad4, 0x7ab7, 0x6a96, 0x1a71, 0x0a50, 0x3a33, 0x2a12,
    0xdbfd, 0xcbdc, 0xfbbf, 0xeb9e, 0x9b79, 0x8b58, 0xbb3b, 0xab1a,
    0x6ca6, 0x7c87, 0x4ce4, 0x5cc5, 0x2c22, 0x3c03, 0x0c60, 0x1c41,
    0xedae, 0xfd8f, 0xcdec, 0xddcd, 0xad2a, 0xbd0b, 0x8d68, 0x9d49,
    0x7e97, 0x6eb6, 0x5ed5, 0x4ef4, 0x3e13, 0x2e32, 0x1e51, 0x0e70,
    0xff9f, 0xefbe, 0xdfdd, 0xcffc, 0xbf1b, 0xaf3a, 0x9f59, 0x8f78,
    0x9188, 0x81a9, 0xb1ca, 0xa1eb, 0xd10c, 0xc12d, 0xf14e, 0xe16f,
    0x1080, 0x00a1, 0x30c2, 0x20e3, 0x5004, 0x4025, 0x7046, 0x6067,
    0x83b9, 0x9398, 0xa3fb, 0xb3da, 0xc33d, 0xd31c, 0xe37f, 0xf35e,
    0x02b1, 0x1290, 0x22f3, 0x32d2, 0x4235, 0x5214, 0x6277, 0x7256,
    0xb5ea, 0xa5cb, 0x95a8, 0x8589, 0xf56e, 0xe54f, 0xd52c, 0xc50d,
    0x34e2, 0x24c3, 0x14a0, 0x0481, 0x7466, 0x6447, 0x5424, 0x4405,
    0xa7db, 0xb7fa, 0x8799, 0x97b8, 0xe75f, 0xf77e, 0xc71d, 0xd73c,
    0x26d3, 0x36f2, 0x0691, 0x16b0, 0x6657, 0x7676, 0x4615, 0x5634,
    0xd94c, 0xc96d, 0xf90e, 0xe92f, 0x99c8, 0x89e9, 0xb98a, 0xa9ab,
    0x5844, 0x4865, 0x7806, 0x6827, 0x18c0, 0x08e1, 0x3882, 0x28a3,
    0xcb7d, 0xdb5c, 0xeb3f, 0xfb1e, 0x8bf9, 0x9bd8, 0xabbb, 0xbb9a,
    0x4a75, 0x5a54, 0x6a37, 0x7a16, 0x0af1, 0x1ad0, 0x2ab3, 0x3a92,
    0xfd2e, 0xed0f, 0xdd6c, 0xcd4d, 0xbdaa, 0xad8b, 0x9de8, 0x8dc9,
    0x7c26, 0x6c07, 0x5c64, 0x4c45, 0x3ca2, 0x2c83, 0x1ce0, 0x0cc1,
    0xef1f, 0xff3e, 0xcf5d, 0xdf7c, 0xaf9b, 0xbfba, 0x8fd9, 0x9ff8,
    0x6e17, 0x7e36, 0x4e55, 0x5e74, 0x2e93, 0x3eb2, 0x0ed1, 0x1ef0,
];

/// Initial CRC accumulator value (`CRC_INIT_VALUE` in `crc.c`).
const CRC_INIT_VALUE: u16 = 0xffff;

/// Final XOR value applied to the accumulator (`CRC_XOR_VALUE` in `crc.c`).
const CRC_XOR_VALUE: u16 = 0x0000;

/// A running CRC-16 (CCITT, non-reflected) accumulator.
///
/// Mirrors the C `unsigned short crcvalue` that was threaded through
/// `CRC_Init` / `CRC_ProcessByte` / `CRC_Value`. All arithmetic is 16-bit,
/// matching the original `unsigned short` semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crc(u16);

impl Crc {
    /// Create a fresh accumulator initialized to `0xffff` (`CRC_Init`).
    #[inline]
    pub fn new() -> Self {
        Crc(CRC_INIT_VALUE)
    }

    /// Fold one byte into the accumulator (`CRC_ProcessByte`).
    ///
    /// C:
    /// ```text
    /// *crcvalue = (*crcvalue << 8) ^ crctable[(*crcvalue >> 8) ^ data];
    /// ```
    ///
    /// `*crcvalue >> 8` yields the high byte (0..=255) and XORing with `data`
    /// (also 0..=255) keeps the table index within 0..=255, so the index is
    /// always in range; we mask to `u8` to make that explicit and total.
    #[inline]
    pub fn process_byte(&mut self, b: u8) {
        let index = ((self.0 >> 8) ^ (b as u16)) & 0x00ff;
        self.0 = (self.0 << 8) ^ CRC_TABLE[index as usize];
    }

    /// Return the finalized CRC value (`CRC_Value`): accumulator XOR `0x0000`.
    #[inline]
    pub fn value(&self) -> u16 {
        self.0 ^ CRC_XOR_VALUE
    }
}

impl Default for Crc {
    #[inline]
    fn default() -> Self {
        Crc::new()
    }
}

/// Compute the CRC-16 of an entire byte slice in one shot.
///
/// Equivalent to: `let mut c = Crc::new(); for &b in data { c.process_byte(b); }
/// c.value()`.
pub fn crc_block(data: &[u8]) -> u16 {
    let mut crc = Crc::new();
    for &b in data {
        crc.process_byte(b);
    }
    crc.value()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Standard CRC-16/CCITT-FALSE check value: CRC of the ASCII string
    /// "123456789" is 0x29B1.
    #[test]
    fn check_value_123456789() {
        assert_eq!(crc_block(b"123456789"), 0x29B1);
    }

    /// Processing bytes one-by-one through `Crc` must equal `crc_block`.
    #[test]
    fn streaming_equals_block() {
        let data: &[&[u8]] = &[
            b"",
            b"A",
            b"123456789",
            b"The quick brown fox jumps over the lazy dog",
            &[0x00, 0xff, 0x10, 0x21, 0x80, 0x7f, 0xab, 0xcd, 0xef],
        ];
        for &buf in data {
            let mut crc = Crc::new();
            for &b in buf {
                crc.process_byte(b);
            }
            assert_eq!(crc.value(), crc_block(buf), "mismatch on {buf:?}");
        }
    }

    /// Empty input yields the init value (no bytes processed), XORed with the
    /// final XOR value (0x0000): 0xffff.
    #[test]
    fn empty_is_init_value() {
        assert_eq!(crc_block(b""), 0xffff);
        assert_eq!(Crc::new().value(), 0xffff);
        assert_eq!(Crc::default().value(), 0xffff);
    }

    /// Single-byte known step matches the direct C formula:
    /// after one byte, crc = (0xffff << 8) ^ table[(0xffff >> 8) ^ b].
    #[test]
    fn single_byte_matches_formula() {
        for b in 0u8..=255 {
            let mut crc = Crc::new();
            crc.process_byte(b);
            let index = ((0xffffu16 >> 8) ^ (b as u16)) & 0x00ff;
            let expected = (0xffffu16 << 8) ^ CRC_TABLE[index as usize];
            assert_eq!(crc.value(), expected, "byte {b}");
        }
    }

    /// The table must have exactly 256 entries and start/known anchors must
    /// match the verbatim copy from crc.c.
    #[test]
    fn table_anchors() {
        assert_eq!(CRC_TABLE.len(), 256);
        assert_eq!(CRC_TABLE[0], 0x0000);
        assert_eq!(CRC_TABLE[1], 0x1021);
        assert_eq!(CRC_TABLE[16], 0x1231);
        assert_eq!(CRC_TABLE[255], 0x1ef0);
    }
}

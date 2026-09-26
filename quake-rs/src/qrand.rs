//! `rand()` for a host session: the two random streams the server draws from —
//! QuakeC's `random()` and the monsters' choices of chase direction.
//!
//! id's C called the C library's `rand()` for both (`PF_random` in pr_cmds.c,
//! `SV_NewChaseDir` in sv_move.c): one process-global stream whose numbers
//! the platform defines, and which the host stirs once a frame to "keep the
//! random time dependent" (host.c) — no two runs of id's game draw the same.
//! The port draws from deterministic generators instead, so its runs repeat
//! (the tests, `quaketool play`'s frame hashes), and it keeps the two streams
//! it has always had: merging them into one libc-like stream would change
//! every frame hash for no fidelity gain.
//!
//! A host session owns one [`QRand`] and hands it to every server it runs
//! ([`crate::server::Server::set_rand`]), so the streams continue across level
//! loads as libc's does; the server's VM draws from it
//! ([`crate::vm::Vm::rand`]). It is shared (`Rc<QRand>`), not moved in and out,
//! so the session's streams survive whatever happens to a server — a failed
//! level load drops its server, not the streams.

use std::cell::Cell;

/// [`QRand::random`]'s state in a fresh session.
const RANDOM_SEED: u32 = 0x1337_BEEF;
/// [`QRand::chase`]'s state in a fresh session.
const CHASE_SEED: u32 = 0x1234_5678;

/// One host session's random streams (see the module doc). Draws take `&self`:
/// the session and its server share one `Rc<QRand>`.
#[derive(Debug)]
pub struct QRand {
    /// The LCG behind QuakeC's `random()`.
    random: Cell<u32>,
    /// The LCG behind `SV_NewChaseDir`'s coin flips.
    chase: Cell<u32>,
}

impl Default for QRand {
    fn default() -> Self {
        QRand::new()
    }
}

impl QRand {
    /// A fresh session's streams: every run that starts from here draws the
    /// same numbers.
    pub fn new() -> QRand {
        QRand { random: Cell::new(RANDOM_SEED), chase: Cell::new(CHASE_SEED) }
    }

    /// `PF_random`'s value, `(rand() & 0x7fff) / (float)0x7fff`: a float in
    /// the CLOSED `[0, 1]` (exactly `1.0` when the 15 bits are all set), so
    /// endpoint-sensitive QuakeC behaves as in id's. The bits are the low 15 of
    /// a Numerical-Recipes LCG step (`x = x*1664525 + 1013904223`).
    pub fn random(&self) -> f32 {
        let next = self.random.get().wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.random.set(next);
        (next & 0x7fff) as f32 / 32767.0
    }

    /// sv_move.c's `rand()`: 15 random bits (`(x >> 16) & 0x7fff` of an ANSI C
    /// LCG step, `x = x*1103515245 + 12345`), for the monsters' `rand()&1` and
    /// `rand()&3` choices.
    pub fn chase(&self) -> u32 {
        let next = self.chase.get().wrapping_mul(1_103_515_245).wrapping_add(12_345);
        self.chase.set(next);
        (next >> 16) & 0x7fff
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_session_repeats_and_the_streams_are_separate() {
        let (a, b) = (QRand::new(), QRand::new());
        let draws = |r: &QRand| (0..8).map(|_| (r.random(), r.chase())).collect::<Vec<_>>();
        assert_eq!(draws(&a), draws(&b), "two fresh sessions draw the same numbers");
        // Drawing from one stream does not move the other.
        let (c, d) = (QRand::new(), QRand::new());
        for _ in 0..5 {
            c.random();
        }
        assert_eq!(c.chase(), d.chase());
    }

    #[test]
    fn random_is_in_the_closed_unit_interval() {
        let r = QRand::new();
        for _ in 0..10_000 {
            assert!((0.0..=1.0).contains(&r.random()));
        }
        assert!(QRand::new().chase() <= 0x7fff);
    }
}

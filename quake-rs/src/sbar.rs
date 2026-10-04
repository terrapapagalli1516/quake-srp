//! The status bar, the scoreboard, and the intermission/finale overlays.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/sbar.c` — `Sbar_Draw` and its parts, `Sbar_DrawScoreboard`,
//! `Sbar_IntermissionOverlay`, `Sbar_FinaleOverlay`.

use crate::draw::{
    HUD_TRANSPARENT, HUD_VIRT_W, blit_qpic_at, blit_scaled, conchars_pic, draw_tile_clear, scaled_2d, screen_2d,
};
use crate::render::Image;
use crate::screen::{SbarLayout, draw_center_string_revealed};
use crate::server::GameMode;

// ---------------------------------------------------------------------------
// HUD / status bar (Quake's `sbar.c` `Sbar_Draw`)
// ---------------------------------------------------------------------------
//
// Quake's status bar is a 2-D overlay blitted on top of the finished 3-D
// framebuffer. `sbar.c` authored it for a fixed 320x200 virtual screen: the bar
// occupies the bottom 24 rows, with the `sbar` background pic (320x24) drawn
// across the bottom and the big white `num_*` digits stamped on top of it for
// health, current ammo, and armour. The pics live in `gfx.wad` (a WAD2).
//
// This port keeps the same virtual coordinates Quake uses. The caller hands us a
// [`Hud`] holding a borrow of the parsed `gfx.wad` and the three
// integer stats read off the player edict; [`draw_hud_into`] then blits the bar
// 320 wide at the bottom centre of the screen, backtile either side on a wider
// one, as Sbar_Draw does (or blown up with the "scaled 2-D" extra).
//
// The HUD is a *separate* `pub fn draw_hud_into(image, hud)` the client calls
// on the screen after the 3-D view is drawn into it, as `SCR_UpdateScreen`
// calls `Sbar_Draw` after `V_RenderView`: the renderer (`render::Renderer`)
// draws no HUD.
//
// Faithfulness/safety: every WAD pic is fetched with `wad.qpic(name).ok()`, so a
// missing or malformed pic simply doesn't draw (never panics, never errors out
// the frame). Pixel writes go through bounds-checked `Image::put`-style logic,
// and HUD-pic texels equal to palette index 255 are skipped (Quake's transparent
// colour for the status-bar pics).

/// The status bar's height in virtual rows (`SBAR_HEIGHT`: the bottom 24 rows
/// of the screen).
const HUD_BAR_H: f32 = 24.0;

/// Where the bar's coordinates land on the framebuffer: `Sbar_DrawPic` and
/// friends draw at `(x + ((vid.width - 320)>>1), y + vid.height - SBAR_HEIGHT)`
/// on the [`screen_2d`] screen, each of its pixels `scale` framebuffer pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
struct BarXf {
    scale: f32,
    /// Framebuffer x of the bar's x = 0 (the 320-wide bar centred).
    ox: f32,
    /// Framebuffer y of the bar's y = 0 (the top of the 24-row sbar strip).
    vy_top: f32,
}

impl BarXf {
    /// The transform on a `vid_w x vid_h` framebuffer.
    fn new(vid_w: usize, vid_h: usize) -> BarXf {
        let sc = screen_2d(vid_w, vid_h);
        BarXf { scale: sc.scale, ox: sc.centred_320_x(), vy_top: (sc.h as f32 - HUD_BAR_H) * sc.scale }
    }

    /// The framebuffer pixel of bar coordinate `(vx, vy)`.
    fn at(&self, vx: f32, vy: f32) -> (i64, i64) {
        ((self.ox + vx * self.scale).floor() as i64, (self.vy_top + vy * self.scale).floor() as i64)
    }
}

/// The Quake HUD overlay: the parsed `gfx.wad` and the player stats to
/// display. Built by the caller each frame from the player edict
/// and the loaded `gfx.wad`; consumed by [`draw_hud_into`].
///
/// The `wad` borrow carries an explicit lifetime `'a` so the caller can
/// keep one parsed [`Wad2`](crate::wad::Wad2) alive and lend it per frame without cloning.
pub struct Hud<'a> {
    /// The parsed `gfx.wad`, which holds the `sbar`/`ibar`/`num_*`/`anum_*`/face/
    /// weapon/item/ammo/armor pics and the `conchars` font.
    pub wad: &'a crate::wad::Wad2,
    /// Current player health, drawn as a big number on the left of the bar, and
    /// driving the face-frame bracket in [`Sbar_DrawFace`](draw_hud_into).
    pub health: i32,
    /// Current ammo for the active weapon, drawn on the right of the bar.
    pub ammo: i32,
    /// Current armour value, drawn just right of the armour icon.
    pub armor: i32,
    /// The QuakeC `items` bitfield (`cl.items`): weapons (`IT_SHOTGUN`..
    /// `IT_LIGHTNING`), ammo-type bits (`IT_SHELLS`..`IT_CELLS`), armour type
    /// (`IT_ARMOR1/2/3`), keys (`IT_KEY1/2`), powerups (`IT_INVISIBILITY`,
    /// `IT_INVULNERABILITY`, `IT_SUIT`, `IT_QUAD`) and sigils (`IT_SIGIL1..4`).
    pub items: i32,
    /// The active weapon's `IT_*` bit (QuakeC `weapon` / `cl.stats[STAT_ACTIVEWEAPON]`):
    /// selects which inventory icon flashes and (via its ammo type) is highlighted.
    pub weapon: i32,
    /// Shell count (QuakeC `ammo_shells`), drawn small in the ibar's first slot.
    pub ammo_shells: i32,
    /// Nail count (QuakeC `ammo_nails`), second ibar ammo slot.
    pub ammo_nails: i32,
    /// Rocket count (QuakeC `ammo_rockets`), third ibar ammo slot.
    pub ammo_rockets: i32,
    /// Cell count (QuakeC `ammo_cells`), fourth ibar ammo slot.
    pub ammo_cells: i32,
    /// The client clock in seconds (`cl.time`): the solo scoreboard's time and,
    /// against [`Hud::item_gettime`], the new-weapon icon flash.
    pub time: f32,
    /// `cl.item_gettime[32]`: the `cl.time` each `items` bit was last newly set
    /// (CL_ParseClientdata). A weapon got less than a second ago cycles its
    /// `inva1..5` icons (Sbar_DrawInventory). `None` draws every icon settled.
    pub item_gettime: Option<&'a [f32; 32]>,
    /// Killed monsters / total (`cl.stats[STAT_MONSTERS/STAT_TOTALMONSTERS]`) for the
    /// solo scoreboard shown on death or Tab.
    pub monsters: i32,
    pub total_monsters: i32,
    /// Found secrets / total (`cl.stats[STAT_SECRETS/STAT_TOTALSECRETS]`).
    pub secrets: i32,
    pub total_secrets: i32,
    /// The level name (worldspawn `message`), right-justified on the scoreboard.
    pub level_name: &'a str,
    /// Force the scorebar + solo scoreboard (Tab "show scores"); the C also shows it
    /// whenever `cl.stats[STAT_HEALTH] <= 0`, which [`draw_hud_into`] handles directly.
    pub show_scores: bool,
    /// `cl.time <= cl.faceanimtime` (V_ParseDamage sets it 0.2 s ahead on every
    /// hit): `Sbar_DrawFace` draws the pain face `face_p*` of the health bracket.
    pub face_pain: bool,
    /// `sb_lines` from [`calc_refdef`](crate::screen::calc_refdef) (the viewsize): 48 draws the inventory
    /// strip and the status bar, 24 the status bar alone, 0 neither — though
    /// the death / Tab scoreboard still shows at 0, as in `Sbar_Draw`.
    pub sb_lines: i32,
    /// How the view meets the bar ([`SbarLayout`], the frame's
    /// [`calc_refdef`](crate::screen::calc_refdef) layout): id's tile-clears
    /// the bar's sides; the slop overlay leaves them to the world drawn
    /// under the view (or the screen's own backtile, where none is: the same
    /// tile).
    pub sbar_layout: SbarLayout,
    /// `standard_quake`/`hipnotic`/`rogue` (common.c): which game this is —
    /// id1 or one of the two mission packs, detected from the `progs.dat`
    /// ([`crate::server::GameMode::detect`]). Draws the extra weapons/items
    /// and the remapped armour/ammo-type bits `Sbar_DrawInventory`/`Sbar_Draw`
    /// draw when `hipnotic`/`rogue` is set.
    pub mode: GameMode,
}

/// Blit one `Qpic` at bar position `(vx, vy)` (`Sbar_DrawPic`): its top-left
/// lands at [`BarXf::at`], each texel a `scale`-pixel block. Each destination
/// pixel samples its source
/// texel nearest-neighbour; texels equal to [`HUD_TRANSPARENT`] (255) are left
/// transparent, leaving the underlying 3-D pixel untouched. Every write is
/// clipped to the framebuffer, so a pic that overhangs an edge never panics.
fn blit_qpic(image: &mut Image, pic: &crate::wad::Qpic, vx: f32, vy: f32, xf: BarXf) {
    if pic.width <= 0 || pic.height <= 0 || xf.scale <= 0.0 {
        return;
    }
    let pw = pic.width as usize;
    let ph = pic.height as usize;
    // Guard against a truncated/short pixel buffer (never index past it).
    if pic.data.len() < pw.saturating_mul(ph) {
        return;
    }

    // Destination top-left in framebuffer pixels, and the scaled pic extent.
    let (dst_x0, dst_y0) = xf.at(vx, vy);
    let dst_w = (pw as f32 * xf.scale).round().max(1.0) as i64;
    let dst_h = (ph as f32 * xf.scale).round().max(1.0) as i64;
    let inv_scale = 1.0 / xf.scale;
    // Transparent texels leave the 3-D pixel as-is.
    blit_scaled(image, &pic.data, pw, (0, 0, pw, ph), (dst_x0, dst_y0, dst_w, dst_h), inv_scale, HUD_TRANSPARENT);
}

/// Draw a right-justified non-negative integer using the big `num_*` digit pics
/// (or the gold `anum_*` pics when `alt` is true), porting `Sbar_DrawNum`.
///
/// `(vx, vy)` is the virtual position of the number's **right edge** at its top;
/// digits are laid out leaving-to-right after right-justifying, exactly like
/// Quake (which walks the string from the right, stepping left by each pic's
/// width). Each digit pic's own width drives the spacing, so proportional digit
/// pics still align. A negative value clamps to 0 (the HUD never shows negative
/// stats); any digit whose pic is missing is simply skipped (no panic).
#[allow(clippy::too_many_arguments)]
fn draw_num(image: &mut Image, value: i32, vx: f32, vy: f32, xf: BarXf, wad: &crate::wad::Wad2, alt: bool) {
    // Render the magnitude; the HUD shows 0 for any negative stat.
    let v = if value < 0 { 0 } else { value };
    // Decompose into decimal digits, most-significant first.
    let mut digits: Vec<u32> = Vec::new();
    let mut n = v as u32;
    if n == 0 {
        digits.push(0);
    } else {
        while n > 0 {
            digits.push(n % 10);
            n /= 10;
        }
        digits.reverse();
    }

    // Walk from the rightmost digit leftward, advancing the pen left by each
    // pic's VIRTUAL width — this right-justifies the number at virtual `vx`.
    // `pen` stays in 320-virtual units the whole time; blit_qpic applies `scale`.
    // (The earlier code mixed virtual `vx` with a pixel `w*scale` step, which
    // mis-placed every digit at scale != 1 — i.e. at the real 640-wide frame.)
    let mut pen = vx;
    for &d in digits.iter().rev() {
        let name = if alt { ANUM_NAMES[d as usize] } else { NUM_NAMES[d as usize] };
        if let Ok(pic) = wad.qpic(name) {
            let w = pic.width.max(0) as f32;
            pen -= w;
            blit_qpic(image, &pic, pen, vy, xf);
        } else {
            // Missing digit pic: still advance by a default 24-virtual slot so
            // the remaining digits keep their right-justified positions.
            pen -= 24.0;
        }
    }
}

/// The white big-number digit pic names (`num_0`..`num_9`).
const NUM_NAMES: [&str; 10] =
    ["num_0", "num_1", "num_2", "num_3", "num_4", "num_5", "num_6", "num_7", "num_8", "num_9"];

/// The gold/alternate digit pic names (`anum_0`..`anum_9`), used for ammo.
const ANUM_NAMES: [&str; 10] =
    ["anum_0", "anum_1", "anum_2", "anum_3", "anum_4", "anum_5", "anum_6", "anum_7", "anum_8", "anum_9"];

// ---------------------------------------------------------------------------
// Status-bar item bits (`quakedef.h` IT_* / the QuakeC `items` bitfield) and the
// gfx.wad lump-name tables (`Sbar_Init`). These drive Sbar_DrawInventory,
// Sbar_DrawFace and the armour/ammo-type icons.
// ---------------------------------------------------------------------------

/// `items` bit for owning the shotgun — the base of the 7 weapon bits. Weapon `i`
/// (0..6) is owned when `items & (IT_SHOTGUN << i)` is set, matching
/// `Sbar_DrawInventory`'s `cl.items & (IT_SHOTGUN<<i)` loop. The 7 bits in order
/// are shotgun(1), super-shotgun(2), nailgun(4), super-nailgun(8),
/// grenade-launcher(16), rocket-launcher(32), lightning(64).
const IT_SHOTGUN: i32 = 1;
const IT_SHELLS: i32 = 256;
const IT_NAILS: i32 = 512;
const IT_ROCKETS: i32 = 1024;
const IT_CELLS: i32 = 2048;
const IT_ARMOR1: i32 = 8192;
const IT_ARMOR2: i32 = 16384;
const IT_ARMOR3: i32 = 32768;
pub(crate) const IT_INVISIBILITY: i32 = 524288; // 1<<19
pub(crate) const IT_INVULNERABILITY: i32 = 1048576; // 1<<20
pub(crate) const IT_SUIT: i32 = 2097152; // 1<<21 (Biosuit)
pub(crate) const IT_QUAD: i32 = 4194304; // 1<<22
const IT_KEY1: i32 = 131072; // 1<<17
const IT_KEY2: i32 = 262144; // 1<<18

// ---------------------------------------------------------------------------
// Hipnotic's and Rogue's own item bits (`quakedef.h`'s "hipnotic added
// defines" / "rogue changed and added defines") and `gfx.wad` lump names
// (`Sbar_Init`'s `if (hipnotic)`/`if (rogue)` arms). Drawn only when
// [`Hud::mode`] says so; id1's `Sbar_Draw`/`Sbar_DrawInventory` path (mode
// [`GameMode::Id1`]) is unchanged.
// ---------------------------------------------------------------------------

/// `HIT_MJOLNIR_BIT` (quakedef.h): reuses standard's unused bit 7
/// (`IT_SUPER_LIGHTNING`, never assigned a weapon in id1).
const HIT_MJOLNIR_BIT: usize = 7;
/// `HIT_PROXIMITY_GUN_BIT`: reuses standard's unused bit 16 (`IT_SUPERHEALTH`,
/// dead in id1's QC).
const HIT_PROXIMITY_GUN_BIT: usize = 16;
/// `HIT_LASER_CANNON_BIT`: a fresh bit past every standard one.
const HIT_LASER_CANNON_BIT: usize = 23;
/// `HIT_PROXIMITY_GUN` (used by name: the combo-slot check reads it directly,
/// unlike the other three weapon bits, which the [`HIPWEAPONS`]-indexed loop
/// reaches by shifting their `_BIT` constant instead).
const HIT_PROXIMITY_GUN: i32 = 1 << HIT_PROXIMITY_GUN_BIT;
// Wetsuit and empathy shields, Hipnotic's two inventory items, are drawn
// below straight off `1<<(24+i)` — sbar.c:699's own literal, which does NOT
// match `quakedef.h`'s `HIT_WETSUIT`/`HIT_EMPATHY_SHIELDS` (`1<<25`/`1<<26`,
// one bit higher). That mismatch is id's own (checked against the C: the
// `#define`s exist but sbar.c never uses them, using `24+i` instead); ported
// faithfully as the *engine* (sbar.c, what this module ports) checks it.

/// `hipweapons[4]` (sbar.c): the `items` bit index for each of the 4
/// inventory-bar slots — laser cannon, mjolnir, the grenade-launcher/
/// proximity-gun combo slot (bit 4, `IT_GRENADE_LAUNCHER`'s), proximity gun.
const HIPWEAPONS: [usize; 4] = [HIT_LASER_CANNON_BIT, HIT_MJOLNIR_BIT, 4, HIT_PROXIMITY_GUN_BIT];

/// `hsb_weapons[flash][0..4]` suffixes (`Sbar_Init`): laser, mjolnir, the
/// combo slot's two pics (grenade-launcher-holds-proximity-gun and
/// proximity-gun-holds-grenade), proximity gun alone.
const HIP_WEAPON_SUFFIX: [&str; 5] = ["laser", "mjolnir", "gren_prox", "prox_gren", "prox"];
/// `hsb_items[0..1]` (`Sbar_Init`): wetsuit, empathy shields.
const HIP_ITEM_NAMES: [&str; 2] = ["sb_wsuit", "sb_eshld"];

/// Rogue's weapon-tier-2 bits (`quakedef.h`): the five `rsb_weapons` icons'
/// `items` bits, each one bit above the last — `RIT_LAVA_NAILGUN << i`
/// (`Sbar_DrawInventory`/`Sbar_Draw`'s `cl.stats[STAT_ACTIVEWEAPON] >=
/// RIT_LAVA_NAILGUN` gate for the whole powered-weapon row).
const RIT_LAVA_NAILGUN: i32 = 4096;
/// Rogue's ammo-type bits — NOT the standard `IT_SHELLS`.. ones: Rogue shifts
/// the whole ammo/weapon bit range down by one from standard (freeing bit 7,
/// unused in id1, for `RIT_SHELLS`), so these are distinct constants.
const RIT_SHELLS: i32 = 128;
const RIT_NAILS: i32 = 256;
const RIT_ROCKETS: i32 = 512;
const RIT_CELLS: i32 = 1024;
/// Rogue's armour-type bits — also not the standard ones: standard's
/// `IT_ARMOR1/2/3` bits (13/14/15) are Rogue's weapon-tier-2 bits instead
/// (`RIT_LAVA_SUPER_NAILGUN`/`RIT_MULTI_GRENADE`/`RIT_MULTI_ROCKET`), so
/// armour moved to fresh bits past them.
const RIT_ARMOR1: i32 = 8_388_608;
const RIT_ARMOR2: i32 = 16_777_216;
const RIT_ARMOR3: i32 = 33_554_432;
/// The two extra ammo-icon bits past the standard four (`rsb_ammo[0..2]`
/// covers lava nails, plasma, and multi-rockets — the C has no separate icon
/// for multi-grenades).
const RIT_LAVA_NAILS: i32 = 67_108_864;
const RIT_PLASMA_AMMO: i32 = 134_217_728;
const RIT_MULTI_ROCKETS: i32 = 268_435_456;
/// Rogue's new inventory items (shield, anti-grav belt) — `rsb_items[0..1]`,
/// same bar position as Hipnotic's wetsuit/shields (mutually exclusive modes).
/// `rsb_items[0..1]`'s base bit (shield at `+0`, anti-grav belt at `+1`,
/// `RIT_SHIELD`/`RIT_ANTIGRAV` — `1<<29`/`1<<30`).
const RIT_SHIELD_BIT: usize = 29;

/// `rsb_weapons[0..4]` lump names (`Sbar_Init`): lava nailgun, lava super
/// nailgun, multi-grenade launcher, multi-rocket launcher, plasma gun.
const RSB_WEAPON_NAMES: [&str; 5] = ["r_lava", "r_superlava", "r_gren", "r_multirock", "r_plasma"];
/// `rsb_items[0..1]`: shield, anti-grav belt.
const RSB_ITEM_NAMES: [&str; 2] = ["r_shield1", "r_agrav1"];
/// `rsb_ammo[0..2]` (`Sbar_Init`) and which `items` bit shows each
/// (`Sbar_Draw`'s ammo-icon slot, Rogue's extra `else if` arm past the
/// standard four): `RIT_LAVA_NAILS` → `rsb_ammo[0]` "r_ammolava",
/// `RIT_PLASMA_AMMO` → `rsb_ammo[1]` "r_ammomulti", `RIT_MULTI_ROCKETS` →
/// `rsb_ammo[2]` "r_ammoplasma" — the last two names look swapped against
/// their bit, but that is id's/Rogue's C exactly (sbar.c:1018-1022); kept
/// faithfully, mismatch and all.
const RSB_AMMO_ICONS: [(i32, &str); 3] =
    [(RIT_LAVA_NAILS, "r_ammolava"), (RIT_PLASMA_AMMO, "r_ammomulti"), (RIT_MULTI_ROCKETS, "r_ammoplasma")];
/// `rsb_invbar[0..1]` (`Sbar_Init`): the inventory strip's background, swapped
/// to the "powered" art while a tier-2 weapon is active
/// (`cl.stats[STAT_ACTIVEWEAPON] >= RIT_LAVA_NAILGUN`).
const RSB_INVBAR_NAMES: [&str; 2] = ["r_invbar1", "r_invbar2"];

/// `inv_*` (owned, dim) weapon icon lump names, `sb_weapons[0][i]` in `Sbar_Init`.
const WEAPON_INV_NAMES: [&str; 7] =
    ["inv_shotgun", "inv_sshotgun", "inv_nailgun", "inv_snailgun", "inv_rlaunch", "inv_srlaunch", "inv_lightng"];
/// The per-weapon name suffixes (`*_shotgun` … `*_lightng`) shared by the
/// `inv_*`/`inv2_*`/`inva{1..5}_*` icon families (`Sbar_Init`). Used to build the
/// selection-flash frame names for the active weapon.
const WEAPON_SUFFIX: [&str; 7] = ["shotgun", "sshotgun", "nailgun", "snailgun", "rlaunch", "srlaunch", "lightng"];

/// `sb_ammo[type]` ammo-icon lump names (`Sbar_Init`): shells/nails/rocket/cells.
const AMMO_ICON_NAMES: [&str; 4] = ["sb_shells", "sb_nails", "sb_rocket", "sb_cells"];

/// `sb_armor[type]` armour-icon lump names (`Sbar_Init`).
const ARMOR_ICON_NAMES: [&str; 3] = ["sb_armor1", "sb_armor2", "sb_armor3"];

/// `sb_items[0..6]` (`Sbar_Init`): the keys + powerup icons drawn on the ibar.
/// In `items`-bit order from bit 17: key1, key2, invisibility(ring), invuln(pent),
/// suit, quad — matching `cl.items & (1<<(17+i))`.
const SB_ITEM_NAMES: [&str; 6] = ["sb_key1", "sb_key2", "sb_invis", "sb_invuln", "sb_suit", "sb_quad"];

/// `sb_sigil[0..3]` (`Sbar_Init`): the 4 runes, `cl.items & (1<<(28+i))`.
const SB_SIGIL_NAMES: [&str; 4] = ["sb_sigil1", "sb_sigil2", "sb_sigil3", "sb_sigil4"];

/// `sb_faces[f][0]` static-face lump names by health bracket, where bracket 0 is
/// the lowest health (`face5`) and bracket 4 (`face1`) the highest, mirroring
/// `Sbar_Init`'s `sb_faces[4]="face1" … sb_faces[0]="face5"`. Indexed `[bracket]`.
const FACE_NAMES: [&str; 5] = ["face5", "face4", "face3", "face2", "face1"];
/// `sb_faces[f][1]`: the pain faces (`face_p1` .. `face_p5`), same brackets.
const FACE_PAIN_NAMES: [&str; 5] = ["face_p5", "face_p4", "face_p3", "face_p2", "face_p1"];

/// `Sbar_DrawFace`'s powerup faces: invisibility+invulnerability, quad, invisibility,
/// invulnerability — checked in that priority order before the health face.
const FACE_INVIS_INVULN: &str = "face_inv2";
const FACE_QUAD: &str = "face_quad";
const FACE_INVIS: &str = "face_invis";
const FACE_INVULN: &str = "face_invul2";

/// Select the player-face health bracket exactly as `Sbar_DrawFace`:
/// `health >= 100 -> 4` (full-health `face1`); otherwise `health / 20` (integer
/// division). So 0..19 -> 0 (`face5`), 20..39 -> 1, 40..59 -> 2, 60..79 -> 3,
/// 80..99 -> 4, >=100 -> 4. A non-positive health (the player is dead — the C
/// shows the scorebar instead) clamps to bracket 0 so we never index out of range.
fn face_bracket(health: i32) -> usize {
    if health >= 100 {
        4
    } else if health <= 0 {
        0
    } else {
        ((health / 20) as usize).min(4)
    }
}

/// `Sbar_DrawInventory`'s `flashon` for owned weapon `i` (0..6):
/// `(int)((cl.time - item_gettime[i])*10)`; from 10 on (a second after it was
/// got) it is 1 for the active weapon (`inv2_*`) and 0 for the rest (`inv_*`),
/// before that `flashon%5 + 2`, the `inva1..5_*` cycle. With no get-times the
/// icon is settled.
fn weapon_flashon(hud: &Hud, i: usize) -> i32 {
    let settled = || i32::from(hud.weapon == IT_SHOTGUN << i);
    let Some(gettime) = hud.item_gettime else { return settled() };
    let flashon = ((hud.time - gettime[i]) * 10.0) as i32;
    if flashon >= 10 { settled() } else { flashon % 5 + 2 }
}

/// `sb_weapons[flashon][i]` (Sbar_Init): `inv_*`, `inv2_*`, then `inva1..5_*`.
fn weapon_icon_name(flashon: i32, i: usize) -> String {
    match flashon {
        0 => WEAPON_INV_NAMES[i].to_string(),
        1 => format!("inv2_{}", WEAPON_SUFFIX[i]),
        f => format!("inva{}_{}", f - 1, WEAPON_SUFFIX[i]),
    }
}

/// [`weapon_flashon`] generalized to an arbitrary `items` bit and its
/// `item_gettime` slot — Hipnotic's weapons use slots 4/7/16/23, not 0..6.
/// Same formula: `(int)((cl.time - item_gettime[idx])*10)`; from 10 on, 1 for
/// the active weapon and 0 otherwise; before that, `flashon%5 + 2`.
fn flashon_for(hud: &Hud, bit: i32, idx: usize) -> i32 {
    let settled = || i32::from(hud.weapon == bit);
    let Some(gettime) = hud.item_gettime else { return settled() };
    let Some(&t) = gettime.get(idx) else { return settled() };
    let flashon = ((hud.time - t) * 10.0) as i32;
    if flashon >= 10 { settled() } else { flashon % 5 + 2 }
}

/// `hsb_weapons[flashon]` (Sbar_Init): `inv_*`, `inv2_*`, then `inva1..5_*` —
/// Hipnotic's own suffix family, the same shape as [`weapon_icon_name`].
fn flashed_hip_name(flashon: i32, suffix: &str) -> String {
    match flashon {
        0 => format!("inv_{suffix}"),
        1 => format!("inv2_{suffix}"),
        f => format!("inva{}_{suffix}", f - 1),
    }
}

/// Try to fetch a HUD pic by name and blit it at virtual `(vx, vy)`; a missing or
/// unparseable lump is silently skipped (`wad.qpic(name).ok()`), so the bar
/// degrades gracefully exactly as the task requires.
// Mirrors Sbar_DrawPic (sbar.c); the C reads vid/draw globals passed explicitly here.
#[allow(clippy::too_many_arguments)]
fn blit_named(image: &mut Image, wad: &crate::wad::Wad2, name: &str, vx: f32, vy: f32, xf: BarXf) {
    if name.is_empty() {
        return;
    }
    if let Ok(pic) = wad.qpic(name) {
        blit_qpic(image, &pic, vx, vy, xf);
    }
}

/// Stamp one console-font glyph (`conchars` cell `ch`) at virtual `(vx, vy)` in
/// 320x200 bar space, scaled/anchored exactly like [`blit_qpic`] — the
/// `Draw_Character` under `Sbar_DrawString` and `Sbar_DrawCharacter` (whose
/// callers add its `+ 4`).
///
/// `conchars` is the raw 128x128 atlas wrapped as a [`crate::wad::Qpic`]
/// (`width = height = 128`), a 16x16 grid of 8x8 glyphs; byte `ch`'s glyph sits at
/// source `(8*(ch%16), 8*(ch/16))`. The ammo counts use the gold digit glyphs
/// `18 + digit` (cells 18..27). Glyph texels equal to palette index 0 are the
/// transparent background and are skipped; every write is clipped to the
/// framebuffer. The 8x8 glyph occupies an 8x8 *virtual* box, scaled to the frame.
// Mirrors Sbar_DrawCharacter/Draw_Character (sbar.c/draw.c); the C reads vid/draw
// globals passed explicitly here.
#[allow(clippy::too_many_arguments)]
fn draw_sbar_char(image: &mut Image, conchars: &crate::wad::Qpic, ch: u8, vx: f32, vy: f32, xf: BarXf) {
    if conchars.width != 128 || conchars.height != 128 || conchars.data.len() < 128 * 128 {
        return;
    }
    let cell_x = (ch as usize % 16) * 8;
    let cell_y = (ch as usize / 16) * 8;
    // Destination top-left in framebuffer pixels and the 8x8 scaled extent.
    let (dst_x0, dst_y0) = xf.at(vx, vy);
    let dst_w = (8.0 * xf.scale).round().max(1.0) as i64;
    let dst_h = (8.0 * xf.scale).round().max(1.0) as i64;
    let inv_scale = 1.0 / xf.scale;
    // conchars uses palette index 0 as the transparent glyph background.
    blit_scaled(image, &conchars.data, 128, (cell_x, cell_y, 8, 8), (dst_x0, dst_y0, dst_w, dst_h), inv_scale, 0);
}

/// `Sbar_DrawInventory` (sbar.c): the `ibar` strip in the 24 virtual rows above
/// the status strip and, on it, the owned weapons, the four ammo counts, the
/// keys/powerups and the sigils. Called by [`draw_hud_into`] only while
/// `sb_lines > 24`. `xf` is the bar's transform.
fn draw_sbar_inventory(image: &mut Image, hud: &Hud, conchars: Option<&crate::wad::Qpic>, xf: BarXf) {
    let wad = hud.wad;
    // The inventory strip's background: Rogue swaps to the "powered" art while
    // a tier-2 weapon is active (Sbar_DrawInventory's first lines); id1 and
    // Hipnotic always draw `ibar`.
    if hud.mode == GameMode::Rogue {
        let name = if hud.weapon >= RIT_LAVA_NAILGUN { RSB_INVBAR_NAMES[0] } else { RSB_INVBAR_NAMES[1] };
        blit_named(image, wad, name, 0.0, -24.0, xf);
    } else {
        blit_named(image, wad, "ibar", 0.0, -24.0, xf);
    }

    // Weapon icons: for each owned weapon (items bit IT_SHOTGUN<<i, i=0..6), draw
    // `sb_weapons[flashon][i]` at Sbar_DrawPic(i*24, -16, ...): the `inva1..5_*`
    // flash for the first second after it was got, then the bright `inv2_*` for
    // the active weapon and the dim `inv_*` for the rest (weapon_flashon). Runs
    // unchanged for every mode — Rogue keeps all seven standard weapons as
    // upgrade bases; its own tier-2 icon (below) draws over the same slot when
    // that tier-2 weapon is the active one.
    for i in 0..7 {
        if hud.items & (IT_SHOTGUN << i) != 0 {
            let name = weapon_icon_name(weapon_flashon(hud, i), i);
            blit_named(image, wad, &name, (i as f32) * 24.0, -16.0, xf);
        }
    }

    // Hipnotic's four weapons (Sbar_DrawInventory's `if (hipnotic)` block):
    // laser cannon and mjolnir each at their own slot (176, 200), and a combo
    // slot at x=96 shared by the grenade launcher (if it also holds the
    // proximity gun) and the proximity gun (if it also holds a grenade
    // launcher) — exactly id's nested logic, including which one wins when a
    // player has picked up both.
    if hud.mode == GameMode::Hipnotic {
        let mut grenade_flashing = false;
        for (i, &bit) in HIPWEAPONS.iter().enumerate() {
            if hud.items & (1 << bit) == 0 {
                continue;
            }
            let flashon = flashon_for(hud, 1 << bit, bit);
            match i {
                2 => {
                    // The grenade-launcher slot (bit 4): only draws while the
                    // proximity gun is ALSO owned (the "combo" pic), and only
                    // while flashing.
                    if hud.items & HIT_PROXIMITY_GUN != 0 && flashon != 0 {
                        grenade_flashing = true;
                        let name = flashed_hip_name(flashon, HIP_WEAPON_SUFFIX[2]);
                        blit_named(image, wad, &name, 96.0, -16.0, xf);
                    }
                }
                3 => {
                    // The proximity-gun slot (bit 16): if the grenade launcher is
                    // ALSO owned, this slot shows the combo pic while flashing (or
                    // its settled "inv_" pic once the grenade slot stopped
                    // flashing); otherwise the plain proximity-gun pic.
                    if hud.items & (IT_SHOTGUN << 4) != 0 {
                        if flashon != 0 && !grenade_flashing {
                            let name = flashed_hip_name(flashon, HIP_WEAPON_SUFFIX[3]);
                            blit_named(image, wad, &name, 96.0, -16.0, xf);
                        } else if !grenade_flashing {
                            let name = flashed_hip_name(0, HIP_WEAPON_SUFFIX[3]);
                            blit_named(image, wad, &name, 96.0, -16.0, xf);
                        }
                    } else {
                        let name = flashed_hip_name(flashon, HIP_WEAPON_SUFFIX[4]);
                        blit_named(image, wad, &name, 96.0, -16.0, xf);
                    }
                }
                _ => {
                    let name = flashed_hip_name(flashon, HIP_WEAPON_SUFFIX[i]);
                    blit_named(image, wad, &name, 176.0 + (i as f32) * 24.0, -16.0, xf);
                }
            }
        }
    }

    // Rogue's powered-weapon row (Sbar_DrawInventory's `if (rogue)` block): the
    // currently active tier-2 weapon's own icon, drawn over the standard loop's
    // slot at the same column — `cl.stats[STAT_ACTIVEWEAPON]`, not `items`, so
    // only the ACTIVE one shows (no "owned but not selected" dim variant, and
    // no flash cycle: each has one pic).
    if hud.mode == GameMode::Rogue && hud.weapon >= RIT_LAVA_NAILGUN {
        for (i, name) in RSB_WEAPON_NAMES.iter().enumerate() {
            if hud.weapon == RIT_LAVA_NAILGUN << i {
                blit_named(image, wad, name, (i as f32 + 2.0) * 24.0, -16.0, xf);
            }
        }
    }

    // Ammo counts: the four totals (shells/nails/rockets/cells) in the top-right of
    // the ibar, small gold digits. Sbar_DrawInventory formats "%3i" (right-justified
    // in 3 chars) and draws each non-space char via Sbar_DrawCharacter at
    // ((6*i+1..3)*8 - 2, -24) using glyph `18 + digit` (the gold conchars digits).
    if let Some(cc) = conchars {
        let counts = [hud.ammo_shells, hud.ammo_nails, hud.ammo_rockets, hud.ammo_cells];
        for (i, &count) in counts.iter().enumerate() {
            // "%3i": right-justified, blanks for leading zeros, clamped to >=0.
            let s = format!("{:3}", count.max(0));
            let b = s.as_bytes();
            for (j, &c) in b.iter().enumerate() {
                if c == b' ' {
                    continue;
                }
                // Gold digit glyph 18 + (c - '0'); x = (6*i + 1 + j)*8 - 2, y = -24,
                // and Sbar_DrawCharacter (sbar.c:293) draws 4 pixels right of
                // its x: `Draw_Character (x + ((vid.width - 320)>>1) + 4, ...)`.
                let glyph = 18 + (c - b'0');
                let vx = ((6 * i + 1 + j) as f32) * 8.0 - 2.0 + 4.0;
                draw_sbar_char(image, cc, glyph, vx, -24.0, xf);
            }
        }
    }

    // Items: keys + powerups (sb_items[0..5]) for items bits 1<<(17+i), at
    // Sbar_DrawPic(192 + i*16, -16, ...). Hipnotic moves its two keys to the
    // main status strip (`draw_hud_into`'s "keys (hipnotic only)" block) and
    // skips them here (`!hipnotic || (i>1)`); the four powerups still draw.
    for (i, name) in SB_ITEM_NAMES.iter().enumerate() {
        if hud.mode == GameMode::Hipnotic && i <= 1 {
            continue;
        }
        if hud.items & (1 << (17 + i)) != 0 {
            blit_named(image, wad, name, 192.0 + (i as f32) * 16.0, -16.0, xf);
        }
    }
    // Hipnotic's two items (wetsuit, empathy shields), bits 1<<(24+i), at the
    // same slot sigils/Rogue's items use below (`if (hipnotic) {...}`, its own
    // `if`, independent of the rogue/sigil choice that follows).
    if hud.mode == GameMode::Hipnotic {
        for (i, name) in HIP_ITEM_NAMES.iter().enumerate() {
            if hud.items & (1 << (24 + i)) != 0 {
                blit_named(image, wad, name, 288.0 + (i as f32) * 16.0, -16.0, xf);
            }
        }
    }
    // Rogue's two items (shield, anti-grav belt), bits 1<<(29+i), at the same
    // slot; every other mode (id1 and Hipnotic alike) draws the sigils instead
    // (`if (rogue) {...} else { sigils }`).
    if hud.mode == GameMode::Rogue {
        for (i, name) in RSB_ITEM_NAMES.iter().enumerate() {
            if hud.items & (1 << (RIT_SHIELD_BIT + i)) != 0 {
                blit_named(image, wad, name, 288.0 + (i as f32) * 16.0, -16.0, xf);
            }
        }
    } else {
        for (i, name) in SB_SIGIL_NAMES.iter().enumerate() {
            if hud.items & (1 << (28 + i)) != 0 {
                blit_named(image, wad, name, 320.0 - 32.0 + (i as f32) * 8.0, -16.0, xf);
            }
        }
    }
}

/// The framebuffer rectangle [`draw_hud_into`]'s bar covers on a `vid_w x
/// vid_h` frame for `sb_lines`: its 320 columns, centred as `Sbar_DrawPic`
/// centres them, from `sb_lines` 2-D rows above the bottom (scaled with the
/// 2-D layer) to the frame's bottom — what the slop overlay's world under the
/// view stays out of ([`crate::screen::Refdef::below_parts`]). `sbar`, `ibar`
/// and `scorebar` have no transparent texel, so the bar covers all of it.
/// `None` with no bar (`sb_lines` 0).
pub fn status_bar_rect(vid_w: usize, vid_h: usize, sb_lines: i32) -> Option<crate::screen::ViewRect> {
    let xf = BarXf::new(vid_w, vid_h);
    let sc = screen_2d(vid_w, vid_h);
    if sb_lines <= 0 || !(xf.scale.is_finite() && xf.scale > 0.0) {
        return None;
    }
    let (x0, _) = xf.at(0.0, 0.0);
    let (x1, _) = xf.at(HUD_VIRT_W, 0.0);
    let (x0, x1) = (x0.clamp(0, vid_w as i64) as usize, x1.clamp(0, vid_w as i64) as usize);
    let y0 = sc.px(sc.h - sb_lines).clamp(0, vid_h as i64) as usize;
    Some(crate::screen::ViewRect { x: x0, y: y0, w: x1 - x0, h: vid_h - y0 })
}

/// Draw the Quake status bar (HUD) across the bottom of `image`, on top of the
/// finished 3-D frame — a faithful port of `sbar.c`'s `Sbar_Draw` (single-player /
/// non-deathmatch path).
///
/// The bar is 320 wide, centred at the bottom of the [`screen_2d`] screen
/// (`Sbar_DrawPic`'s `(vid.width - 320)>>1`), with `backtile` either side of
/// it on a wider screen (`Draw_TileClear (0, vid.height - sb_lines,
/// vid.width, sb_lines)`) — or, with [`SbarLayout::Overlay`], the game
/// either side of it; the "scaled 2-D" extra blows it up with the rest of
/// the 2-D layer. The *status area* is 48 virtual rows tall: the `ibar`
/// inventory strip (320x24) sits in the 24 rows ABOVE the `sbar` (320x24)
/// status strip — matching `Sbar_DrawPic(0, -24, sb_ibar)` (the C draws relative
/// to `vid.height - SBAR_HEIGHT`, so a virtual `y` maps straight to our `vy`).
///
/// How much of it draws follows `hud.sb_lines` ([`calc_refdef`](crate::screen::calc_refdef)): the inventory
/// strip only above 24 lines, the status strip only above 0 — but the death /
/// Tab scoreboard (`scorebar`) regardless, exactly like `Sbar_Draw`.
///
/// Drawing order (mirrors `Sbar_Draw` → `Sbar_DrawInventory` then the sbar block):
///  1. `ibar` strip, then on it: owned weapon icons (the selected one flashing its
///     `inva*` frames), the four small ammo counts, keys/powerups, and sigils.
///  2. `sbar` strip, then on it: the armour-type icon + armour number (left), the
///     animated player face (centre, x=112), the health number, the ammo-type
///     icon (x=224) and the current-ammo number (right).
///
/// Every pic is fetched via `wad.qpic(name).ok()` (and `conchars` via
/// `lump_data`), so a `gfx.wad` missing any element degrades gracefully — that
/// element just doesn't draw, never a panic and never an errored frame.
pub fn draw_hud_into(image: &mut Image, hud: &Hud) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    // The bar's transform: 320 wide, centred at the bottom of the 2-D screen;
    // the ibar sits 24 rows above the sbar strip (negative vy).
    let xf = BarXf::new(image.w, image.h);
    if !xf.scale.is_finite() || xf.scale <= 0.0 {
        return;
    }
    let wad = hud.wad;
    let conchars = conchars_pic(wad);

    // Sbar_Draw: `if (sb_lines && vid.width > 320) Draw_TileClear (0,
    // vid.height - sb_lines, vid.width, sb_lines);` — the backtile either side
    // of the bar (and under it, where the bar pics draw over it). With the slop
    // overlay the world under the view is there instead, and the bar's opaque
    // pics cover their own 320 columns.
    let sc = screen_2d(image.w, image.h);
    if hud.sb_lines > 0 && sc.w > HUD_VIRT_W as i32 && hud.sbar_layout == SbarLayout::Classic {
        let y0 = sc.px(sc.h - hud.sb_lines).max(0) as usize;
        draw_tile_clear(image, wad.qpic("backtile").ok().as_ref(), 0, y0, image.w, image.h - y0.min(image.h));
    }

    // ----- Inventory bar (Sbar_DrawInventory) -------------------------------
    // Sbar_Draw: `if (sb_lines > 24) Sbar_DrawInventory ();` — viewsize 110
    // (sb_lines 24) keeps only the status strip, 120 (0) neither.
    if hud.sb_lines > 24 {
        draw_sbar_inventory(image, hud, conchars.as_ref(), xf);
    }

    // ----- Status bar (the sbar block of Sbar_Draw) -------------------------
    // When the player is dead (health <= 0) or holding Tab, the C replaces the whole
    // status strip with the dark `scorebar` pic + the solo scoreboard
    // (Monsters/Secrets/Time/level), keeping the ibar above. (sbar.c:948-953.)
    if hud.health <= 0 || hud.show_scores {
        blit_named(image, wad, "scorebar", 0.0, 0.0, xf);
        if let Some(cc) = &conchars {
            draw_solo_scoreboard(image, cc, hud, xf);
        }
        return;
    }
    // `else if (sb_lines)`: no status strip at viewsize 120.
    if hud.sb_lines <= 0 {
        return;
    }

    // 1. Background strip (sbar, 320x24) at virtual (0,0).
    blit_named(image, wad, "sbar", 0.0, 0.0, xf);

    // Keys (Hipnotic only): Sbar_DrawInventory moved them off the inventory
    // bar ("so they would not be overwritten") to two fixed spots on the main
    // strip, drawn right after it, in id1's own `sb_items[0]`/`[1]` pics.
    if hud.mode == GameMode::Hipnotic {
        if hud.items & IT_KEY1 != 0 {
            blit_named(image, wad, SB_ITEM_NAMES[0], 209.0, 3.0, xf);
        }
        if hud.items & IT_KEY2 != 0 {
            blit_named(image, wad, SB_ITEM_NAMES[1], 209.0, 12.0, xf);
        }
    }

    // Armour field (Sbar_Draw, sbar.c:968-997). Under invulnerability the C draws a
    // gold "666" and the Pentagram-of-Protection disc over the armour slot and shows
    // NO real armour icon/number; otherwise the armour-type icon (Sbar_DrawPic(0, 0,
    // sb_armor[type])) keyed on IT_ARMOR3/2/1 plus the armour number at
    // Sbar_DrawNum(24, ..) — right edge virtual x=96, gold when <=25. Rogue keys the
    // icon off its own, differently-numbered RIT_ARMOR1/2/3 bits (standard's
    // IT_ARMOR1/2/3 bit positions are Rogue's weapon-tier-2 bits instead).
    if hud.items & IT_INVULNERABILITY != 0 {
        draw_num(image, 666, 96.0, 0.0, xf, wad, true);
        blit_named(image, wad, "disc", 0.0, 0.0, xf);
    } else {
        let (armor3, armor2, armor1) = if hud.mode == GameMode::Rogue {
            (RIT_ARMOR3, RIT_ARMOR2, RIT_ARMOR1)
        } else {
            (IT_ARMOR3, IT_ARMOR2, IT_ARMOR1)
        };
        if hud.items & armor3 != 0 {
            blit_named(image, wad, ARMOR_ICON_NAMES[2], 0.0, 0.0, xf);
        } else if hud.items & armor2 != 0 {
            blit_named(image, wad, ARMOR_ICON_NAMES[1], 0.0, 0.0, xf);
        } else if hud.items & armor1 != 0 {
            blit_named(image, wad, ARMOR_ICON_NAMES[0], 0.0, 0.0, xf);
        }
        draw_num(image, hud.armor, 96.0, 0.0, xf, wad, hud.armor <= 25);
    }

    // Face (Sbar_DrawFace) at x=112, y=0. Powerup faces take priority in the C's
    // order: invisibility+invulnerability, then quad, then invisibility, then
    // invulnerability; otherwise the health-bracket face, `sb_faces[f][anim]`
    // with anim 1 (the pain face) while `cl.time <= cl.faceanimtime`.
    let inv_iv = IT_INVISIBILITY | IT_INVULNERABILITY;
    if hud.items & inv_iv == inv_iv {
        blit_named(image, wad, FACE_INVIS_INVULN, 112.0, 0.0, xf);
    } else if hud.items & IT_QUAD != 0 {
        blit_named(image, wad, FACE_QUAD, 112.0, 0.0, xf);
    } else if hud.items & IT_INVISIBILITY != 0 {
        blit_named(image, wad, FACE_INVIS, 112.0, 0.0, xf);
    } else if hud.items & IT_INVULNERABILITY != 0 {
        blit_named(image, wad, FACE_INVULN, 112.0, 0.0, xf);
    } else {
        let names = if hud.face_pain { &FACE_PAIN_NAMES } else { &FACE_NAMES };
        let face = names[face_bracket(hud.health)];
        blit_named(image, wad, face, 112.0, 0.0, xf);
    }

    // Health number: Sbar_DrawNum(136, health, 3, health<=25) — right edge x=208.
    draw_num(image, hud.health, 208.0, 0.0, xf, wad, hud.health <= 25);

    // Ammo-type icon (Sbar_DrawPic(224, 0, sb_ammo[type])) by the active weapon's
    // ammo type, keyed on the items ammo bits IT_SHELLS/NAILS/ROCKETS/CELLS —
    // Rogue keys the standard four off its own RIT_* bits (not the same
    // numbers as standard's) and has three more of its own past them.
    if hud.mode == GameMode::Rogue {
        if hud.items & RIT_SHELLS != 0 {
            blit_named(image, wad, AMMO_ICON_NAMES[0], 224.0, 0.0, xf);
        } else if hud.items & RIT_NAILS != 0 {
            blit_named(image, wad, AMMO_ICON_NAMES[1], 224.0, 0.0, xf);
        } else if hud.items & RIT_ROCKETS != 0 {
            blit_named(image, wad, AMMO_ICON_NAMES[2], 224.0, 0.0, xf);
        } else if hud.items & RIT_CELLS != 0 {
            blit_named(image, wad, AMMO_ICON_NAMES[3], 224.0, 0.0, xf);
        } else {
            for &(bit, name) in &RSB_AMMO_ICONS {
                if hud.items & bit != 0 {
                    blit_named(image, wad, name, 224.0, 0.0, xf);
                    break;
                }
            }
        }
    } else if hud.items & IT_SHELLS != 0 {
        blit_named(image, wad, AMMO_ICON_NAMES[0], 224.0, 0.0, xf);
    } else if hud.items & IT_NAILS != 0 {
        blit_named(image, wad, AMMO_ICON_NAMES[1], 224.0, 0.0, xf);
    } else if hud.items & IT_ROCKETS != 0 {
        blit_named(image, wad, AMMO_ICON_NAMES[2], 224.0, 0.0, xf);
    } else if hud.items & IT_CELLS != 0 {
        blit_named(image, wad, AMMO_ICON_NAMES[3], 224.0, 0.0, xf);
    }

    // Current ammo number: Sbar_DrawNum(248, ammo, 3, ammo<=10) — right edge x=320.
    draw_num(image, hud.ammo, 320.0, 0.0, xf, wad, hud.ammo <= 10);
}

/// `Sbar_SoloScoreboard` (sbar.c:457): the single-player stats drawn over the
/// `scorebar` strip on death / Tab — kills, secrets, elapsed time, and the level
/// name. Positions are verbatim from the C (virtual sbar-space, y in 0..24): the
/// "Monsters" / "Secrets" lines at x=8 (rows 4, 12), "Time" at x=184 row 4, and the
/// level name right-justified ending at virtual x≈232 on row 12 (`232 - len*4`).
fn draw_solo_scoreboard(image: &mut Image, conchars: &crate::wad::Qpic, hud: &Hud, xf: BarXf) {
    let draw = |image: &mut Image, vx: f32, vy: f32, s: &str| {
        for (i, &c) in s.as_bytes().iter().enumerate() {
            // Sbar_DrawString blits the raw ASCII glyph (space included, harmless).
            draw_sbar_char(image, conchars, c, vx + (i as f32) * 8.0, vy, xf);
        }
    };
    draw(image, 8.0, 4.0, &format!("Monsters:{:3} /{:3}", hud.monsters, hud.total_monsters));
    draw(image, 8.0, 12.0, &format!("Secrets :{:3} /{:3}", hud.secrets, hud.total_secrets));
    // Time: minutes:tens-units from the server clock (sbar.c uses integer seconds).
    let t = hud.time.max(0.0) as i32;
    let minutes = t / 60;
    let seconds = t - 60 * minutes;
    let tens = seconds / 10;
    let units = seconds - 10 * tens;
    draw(image, 184.0, 4.0, &format!("Time :{minutes:3}:{tens}{units}"));
    // Level name, right-justified to end at virtual x≈232 (232 - len*4 start).
    let l = hud.level_name.len() as f32;
    draw(image, 232.0 - l * 4.0, 12.0, hud.level_name);
}

// ---------------------------------------------------------------------------
// Intermission + finale overlays (sbar.c Sbar_IntermissionOverlay /
// Sbar_FinaleOverlay + screen.c SCR_DrawCenterString's finale char reveal)
// ---------------------------------------------------------------------------

/// The level-complete numbers `Sbar_IntermissionOverlay` (sbar.c) draws over the
/// `gfx/inter.lmp` plaque: the completion time and the secrets/monsters counts
/// (`cl.completed_time`, `cl.stats[STAT_SECRETS/TOTALSECRETS/MONSTERS/
/// TOTALMONSTERS]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntermissionStats {
    /// `cl.completed_time` in whole seconds (latched when svc_intermission arrived).
    pub completed_time: i32,
    /// Found secrets (`found_secrets` QuakeC global / STAT_SECRETS).
    pub secrets: i32,
    /// Total secrets in the level (`total_secrets` / STAT_TOTALSECRETS).
    pub total_secrets: i32,
    /// Killed monsters (`killed_monsters` / STAT_MONSTERS).
    pub monsters: i32,
    /// Total monsters in the level (`total_monsters` / STAT_TOTALMONSTERS).
    pub total_monsters: i32,
}

/// `Sbar_IntermissionNumber` (sbar.c): draw `num` with the big white `num_*`
/// digit pics, right-justified into `digits` 24-px slots whose LEFT edge is at
/// virtual `vx` (a number shorter than `digits` starts `(digits-l)*24` further
/// right; a longer one keeps only its trailing `digits` digits). Virtual
/// coordinates in the 320x200 space, mapped by `scale`/`ox`/`oy` like the menu.
#[allow(clippy::too_many_arguments)]
fn intermission_number(
    image: &mut Image,
    wad: &crate::wad::Wad2,
    num: i32,
    vx: f32,
    vy: f32,
    digits: usize,
    scale: f32,
    ox: f32,
    oy: f32,
) {
    // Sbar_itoa renders the (possibly negative) value: a leading '-' draws as
    // the STAT_MINUS glyph — sb_nums[0][10], the `num_minus` wad pic (Sbar_Init).
    // Rust's `to_string` yields exactly the C's sign-then-digits form.
    let s = num.to_string();
    let b = s.as_bytes();
    let shown = if b.len() > digits { &b[b.len() - digits..] } else { b };
    let mut x = vx + (digits.saturating_sub(shown.len())) as f32 * 24.0;
    for &c in shown {
        let name = if c == b'-' { "num_minus" } else { NUM_NAMES[(c - b'0') as usize] };
        if let Ok(pic) = wad.qpic(name) {
            blit_qpic_at(image, &pic, x, vy, scale, ox, oy);
        }
        x += 24.0; // the C steps a fixed 24 per digit slot
    }
}

/// `Sbar_IntermissionOverlay` (sbar.c): the single-player level-complete screen —
/// the `gfx/complete.lmp` banner at (64,24), the `gfx/inter.lmp` plaque at (0,56),
/// and the big-number time (minutes:seconds), secrets found/total and monsters
/// killed/total beside the plaque's labels. The C draws them all with plain
/// `Draw_Pic`/`Draw_TransPic` at those screen coordinates — no centring — so
/// on a screen wider than 320 they sit in its top-left corner, while the
/// status bar (`Sbar_DrawPic`), the menus (`M_DrawPic`) and the episode's
/// finale ([`draw_finale_overlay`]) centre themselves: id's own
/// inconsistency, invisible at 320x200.
///
/// The "scaled 2-D" extra lays the 2-D layer out on a screen a little wider
/// than 320 on most frames (a 16:9 one is 384 wide, a phone's twice that),
/// so there the screen's 320 columns are centred as `Sbar_DrawPic` centres
/// the bar ([`Screen2d::centred_320_x`](crate::draw::Screen2d::centred_320_x)),
/// in line with the bar, the menus and the finale. Off, id's placement.
///
/// `complete`/`inter` are the two pak pics (`Draw_CachePic` in the C); either
/// being absent just skips that blit — the numbers still draw, never a panic.
/// The big digits and the colon/slash come from `gfx.wad` like the HUD's.
pub fn draw_intermission_overlay(
    image: &mut Image,
    wad: &crate::wad::Wad2,
    complete: Option<&crate::wad::Qpic>,
    inter: Option<&crate::wad::Qpic>,
    stats: &IntermissionStats,
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let sc = screen_2d(image.w, image.h);
    let scale = sc.scale;
    let ox = if scaled_2d() { sc.centred_320_x() } else { 0.0 };
    let oy = 0.0;

    // Draw_Pic(64, 24, "gfx/complete.lmp") — the "Level Complete" banner.
    if let Some(pic) = complete {
        blit_qpic_at(image, pic, 64.0, 24.0, scale, ox, oy);
    }
    // Draw_TransPic(0, 56, "gfx/inter.lmp") — the Time/Secrets/Kills plaque.
    if let Some(pic) = inter {
        blit_qpic_at(image, pic, 0.0, 56.0, scale, ox, oy);
    }

    // Time: minutes right-justified at (160,64) over 3 slots, then num_colon at
    // 234 and the two second digits at 246/266 (verbatim sbar.c coordinates).
    // DEVIATION: clamped at 0 — a negative time would make the C's direct
    // `sb_nums[0][num/10]` second-digit lookups index negatively (UB); the
    // signed stats rows below go through intermission_number's minus glyph.
    let t = stats.completed_time.max(0);
    let minutes = t / 60;
    let seconds = t - 60 * minutes;
    intermission_number(image, wad, minutes, 160.0, 64.0, 3, scale, ox, oy);
    if let Ok(pic) = wad.qpic("num_colon") {
        blit_qpic_at(image, &pic, 234.0, 64.0, scale, ox, oy);
    }
    if let Ok(pic) = wad.qpic(NUM_NAMES[(seconds / 10) as usize]) {
        blit_qpic_at(image, &pic, 246.0, 64.0, scale, ox, oy);
    }
    if let Ok(pic) = wad.qpic(NUM_NAMES[(seconds % 10) as usize]) {
        blit_qpic_at(image, &pic, 266.0, 64.0, scale, ox, oy);
    }

    // Secrets: found at (160,104), num_slash at 232, total at 240.
    intermission_number(image, wad, stats.secrets, 160.0, 104.0, 3, scale, ox, oy);
    if let Ok(pic) = wad.qpic("num_slash") {
        blit_qpic_at(image, &pic, 232.0, 104.0, scale, ox, oy);
    }
    intermission_number(image, wad, stats.total_secrets, 240.0, 104.0, 3, scale, ox, oy);

    // Monsters: killed at (160,144), num_slash at 232, total at 240.
    intermission_number(image, wad, stats.monsters, 160.0, 144.0, 3, scale, ox, oy);
    if let Ok(pic) = wad.qpic("num_slash") {
        blit_qpic_at(image, &pic, 232.0, 144.0, scale, ox, oy);
    }
    intermission_number(image, wad, stats.total_monsters, 240.0, 144.0, 3, scale, ox, oy);
}

/// `Sbar_FinaleOverlay` (sbar.c) + the finale half of `SCR_DrawCenterString`
/// (screen.c): the horizontally-centered `gfx/finale.lmp` plaque at y=16 and the
/// episode-end text revealed at `scr_printspeed` (8) characters per second of
/// `elapsed` (`cl.time - scr_centertime_start`). Pass `finale_pic = None` for
/// `svc_cutscene` (`cl.intermission == 3`), which draws the text alone.
pub fn draw_finale_overlay(
    image: &mut Image,
    conchars: Option<&crate::wad::Qpic>,
    finale_pic: Option<&crate::wad::Qpic>,
    text: &str,
    elapsed: f32,
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let sc = screen_2d(image.w, image.h);

    // Draw_TransPic((vid.width - pic->width)/2, 16, "gfx/finale.lmp").
    if let Some(pic) = finale_pic {
        let vx = ((sc.w - pic.width.max(0)) / 2) as f32;
        blit_qpic_at(image, pic, vx, 16.0, sc.scale, 0.0, 0.0);
    }
    // scr_printspeed defaults to "8" (screen.c): 8 characters per second —
    // crate::screen::scr_printspeed_remaining, shared with the mission packs'
    // `finaleFinished` builtin so both agree on when the reveal completes.
    if let Some(cc) = conchars {
        let remaining = crate::screen::scr_printspeed_remaining(elapsed);
        draw_center_string_revealed(image, cc, text, remaining);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::SB_LINES_FULL;
    use crate::wad::{CMP_NONE, LUMPINFO_SIZE, NAME_LEN, Qpic, TYP_QPIC, WADINFO_SIZE, Wad2};

    // -- HUD / status bar -----------------------------------------------------

    /// One synthetic qpic payload: width i32, height i32, then `w*h` indices.
    fn qpic_payload(w: i32, h: i32, fill: u8) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&w.to_le_bytes());
        v.extend_from_slice(&h.to_le_bytes());
        v.resize(8 + (w as usize) * (h as usize), fill);
        v
    }

    /// Append a 32-byte `lumpinfo_t` entry (mirrors `wad.rs`'s test helper).
    fn push_lump(dir: &mut Vec<u8>, filepos: i32, size: i32, name: &str) {
        dir.extend_from_slice(&filepos.to_le_bytes());
        dir.extend_from_slice(&size.to_le_bytes()); // disksize
        dir.extend_from_slice(&size.to_le_bytes()); // size
        dir.push(TYP_QPIC);
        dir.push(CMP_NONE);
        dir.push(0); // pad1
        dir.push(0); // pad2
        let mut field = [0u8; NAME_LEN];
        let nb = name.as_bytes();
        let n = nb.len().min(NAME_LEN);
        field[..n].copy_from_slice(&nb[..n]);
        dir.extend_from_slice(&field);
    }

    /// Build a synthetic `gfx.wad` containing `sbar` (320x24), `num_0..num_9`
    /// (24x24, each filled with palette index `100+d` so digits are recognisable
    /// and never transparent), `anum_0..anum_9` (24x24, index `120+d`), and the
    /// intermission `num_colon`/`num_slash`/`num_minus` (index 140/141/142).
    fn build_hud_wad() -> Wad2 {
        wad_from_pics(base_hud_pics())
    }

    /// [`build_hud_wad`]'s own (name, payload) pairs, reusable by
    /// [`build_hud_wad_with`] so a test can add mission-pack lumps without
    /// re-deriving the base set.
    fn base_hud_pics() -> Vec<(String, Vec<u8>)> {
        let mut pics: Vec<(String, Vec<u8>)> = Vec::new();
        pics.push(("sbar".to_string(), qpic_payload(320, 24, 1)));
        for d in 0..10u8 {
            pics.push((format!("num_{d}"), qpic_payload(24, 24, 100 + d)));
        }
        for d in 0..10u8 {
            pics.push((format!("anum_{d}"), qpic_payload(24, 24, 120 + d)));
        }
        pics.push(("num_colon".to_string(), qpic_payload(16, 24, 140)));
        pics.push(("num_slash".to_string(), qpic_payload(16, 24, 141)));
        pics.push(("num_minus".to_string(), qpic_payload(16, 24, 142)));
        pics
    }

    /// [`build_hud_wad`] plus `extra` named lumps (each `(name, fill)`, a
    /// 24x16 pic filled with `fill` so "did this draw" is a one-value pixel
    /// search) — for a test that exercises a lump the base set doesn't have
    /// (a mission pack's own). A name also in the base set (`"ibar"`, a
    /// mission pack's own background) replaces it.
    fn build_hud_wad_with(extra: &[(&str, u8)]) -> Wad2 {
        let mut pics = base_hud_pics();
        for &(name, fill) in extra {
            pics.retain(|(n, _)| n != name);
            let (w, h) = if name == "ibar" || name.starts_with("r_invbar") { (320, 24) } else { (24, 16) };
            pics.push((name.to_string(), qpic_payload(w, h, fill)));
        }
        wad_from_pics(pics)
    }

    /// Serialize `pics` as a minimal WAD2 and parse it back (the body both
    /// [`build_hud_wad`] and [`build_hud_wad_with`] share).
    fn wad_from_pics(pics: Vec<(String, Vec<u8>)>) -> Wad2 {
        // Lay payloads right after the 12-byte header; build the directory after.
        let mut payloads = Vec::new();
        let mut offsets = Vec::new();
        let mut pos = WADINFO_SIZE;
        for (_, p) in &pics {
            offsets.push(pos);
            payloads.extend_from_slice(p);
            pos += p.len();
        }
        let infotableofs = pos;

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"WAD2");
        bytes.extend_from_slice(&(pics.len() as i32).to_le_bytes());
        bytes.extend_from_slice(&(infotableofs as i32).to_le_bytes());
        bytes.extend_from_slice(&payloads);

        let mut dir = Vec::new();
        for ((name, p), &off) in pics.iter().zip(offsets.iter()) {
            push_lump(&mut dir, off as i32, p.len() as i32, name);
        }
        bytes.extend_from_slice(&dir);
        debug_assert_eq!(bytes.len(), infotableofs + pics.len() * LUMPINFO_SIZE);

        Wad2::parse(bytes).expect("synthetic gfx.wad parses")
    }

    #[test]
    fn sbar_glyphs_and_pics_match_the_per_pixel_blit() {
        // draw_sbar_char blits an 8x8 cell out of the 128-wide atlas and
        // blit_qpic a whole pic, both through draw::blit_scaled now; check
        // them against the per-pixel loops they replaced, at fractional
        // scales, clipped at every edge.
        let mut x = 12345u32;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x >> 5) as u8
        };
        let atlas = Qpic { width: 128, height: 128, data: (0..128 * 128).map(|_| next() % 4).collect() };
        let pic = Qpic { width: 24, height: 24, data: (0..24 * 24).map(|_| next() | 0xf0).collect() };
        for &scale in &[0.5f32, 1.0, 1.37, 2.0, 2.5, 3.5, 4.0] {
            let inv = 1.0 / scale;
            for &(vx, vy, vy_top) in
                &[(0.0f32, 0.0f32, 40.0f32), (-3.0, -24.0, 50.5), (60.0, 2.0, 30.25), (70.0, 10.0, 61.0)]
            {
                for ch in [0u8, 18, 27, 65, 255] {
                    let mut want = Image::new(80, 64, 5);
                    let mut got = Image::new(80, 64, 5);
                    let (cx, cy) = ((ch as usize % 16) * 8, (ch as usize / 16) * 8);
                    let x0 = (vx * scale).floor() as i64;
                    let y0 = (vy_top + vy * scale).floor() as i64;
                    let n = (8.0 * scale).round().max(1.0) as i64;
                    for dy in 0..n {
                        for dx in 0..n {
                            let (sx, sy) = ((dx as f32 * inv) as usize, (dy as f32 * inv) as usize);
                            if sx < 8 && sy < 8 {
                                let t = atlas.data[(cy + sy) * 128 + cx + sx];
                                if t != 0 {
                                    want.put((x0 + dx) as i32, (y0 + dy) as i32, t);
                                }
                            }
                        }
                    }
                    draw_sbar_char(&mut got, &atlas, ch, vx, vy, BarXf { scale, ox: 0.0, vy_top });
                    assert!(got.pixels == want.pixels, "glyph {ch} scale {scale} ({vx},{vy}) top {vy_top}");
                }
                let mut want = Image::new(80, 64, 5);
                let mut got = Image::new(80, 64, 5);
                let x0 = (vx * scale).floor() as i64;
                let y0 = (vy_top + vy * scale).floor() as i64;
                let n = (24.0 * scale).round().max(1.0) as i64;
                for dy in 0..n {
                    for dx in 0..n {
                        let (sx, sy) = ((dx as f32 * inv) as usize, (dy as f32 * inv) as usize);
                        if sx < 24 && sy < 24 {
                            let t = pic.data[sy * 24 + sx];
                            if t != HUD_TRANSPARENT {
                                want.put((x0 + dx) as i32, (y0 + dy) as i32, t);
                            }
                        }
                    }
                }
                blit_qpic(&mut got, &pic, vx, vy, BarXf { scale, ox: 0.0, vy_top });
                assert!(got.pixels == want.pixels, "pic scale {scale} ({vx},{vy}) top {vy_top}");
            }
        }
    }

    #[test]
    fn blit_qpic_respects_transparency_and_clips() {
        let mut img = Image::new(8, 8, 0);

        // A 3x3 pic: corners opaque (index 5), centre transparent (255), with a
        // distinct opaque edge (index 9) we can detect after clipping.
        let mut data = vec![5u8; 9];
        data[3 + 1] = HUD_TRANSPARENT; // centre (row 1, col 1) transparent
        data[2 * 3 + 2] = 9; // bottom-right (row 2, col 2) opaque, distinct
        let pic = Qpic { width: 3, height: 3, data };

        // scale 1, no vertical offset (vy_top = 0), placed at virtual (0,0).
        blit_qpic(&mut img, &pic, 0.0, 0.0, BarXf { scale: 1.0, ox: 0.0, vy_top: 0.0 });

        // The centre texel was transparent: the background pixel is untouched.
        assert_eq!(img.pixels[8 + 1], 0, "index-255 texel left bg unchanged");
        // An opaque corner drew palette index 5 -> [5,5,5].
        assert_eq!(img.pixels[0], 5, "opaque corner blitted");
        // The distinct bottom-right opaque texel drew index 9.
        assert_eq!(img.pixels[2 * 8 + 2], 9, "distinct opaque texel blitted");

        // Clipping: blit the same pic so it overhangs the right/bottom edges. The
        // texels that fall off-screen must be silently dropped (no panic), and the
        // on-screen part must still draw.
        let mut img2 = Image::new(8, 8, 0);
        // Place top-left at virtual (7,7): only the (0,0) texel is on-screen.
        blit_qpic(&mut img2, &pic, 7.0, 7.0, BarXf { scale: 1.0, ox: 0.0, vy_top: 0.0 });
        assert_eq!(img2.pixels[7 * 8 + 7], 5, "on-screen overhang texel drew");
        // Nothing wrapped to row 0 / col 0 from the off-screen part.
        let drawn = img2.pixels.iter().filter(|p| **p != 0).count();
        assert_eq!(drawn, 1, "only the single on-screen overhang texel drew");
    }

    #[test]
    fn draw_num_right_justifies() {
        let wad = build_hud_wad();

        // Right edge at virtual x=72 (3 * 24px digits), vy_top=0, scale=1.
        // num pics are 24x24. A 3-digit value (e.g. 100) fills [0,72); the digit
        // region (x in [0,72), y in [0,24)) must have changed.
        let mut img3 = Image::new(80, 24, 0);
        draw_num(&mut img3, 100, 72.0, 0.0, BarXf { scale: 1.0, ox: 0.0, vy_top: 0.0 }, &wad, false);
        let changed_3: usize =
            (0..24).flat_map(|y| (0..72).map(move |x| (x, y))).filter(|&(x, y)| img3.pixels[y * 80 + x] != 0).count();
        assert!(changed_3 > 0, "3-digit value changed pixels in the digit region");

        // A 1-digit value at the same right edge must occupy only the rightmost
        // 24px slot [48,72) and leave the left two slots [0,48) untouched, proving
        // right-justification (the units digit lands at the same right edge).
        let mut img1 = Image::new(80, 24, 0);
        draw_num(&mut img1, 7, 72.0, 0.0, BarXf { scale: 1.0, ox: 0.0, vy_top: 0.0 }, &wad, false);
        // Right slot [48,72) changed.
        let right_changed: usize =
            (0..24).flat_map(|y| (48..72).map(move |x| (x, y))).filter(|&(x, y)| img1.pixels[y * 80 + x] != 0).count();
        assert!(right_changed > 0, "1-digit value drew in the rightmost slot");
        // Left two slots [0,48) untouched.
        let left_changed: usize =
            (0..24).flat_map(|y| (0..48).map(move |x| (x, y))).filter(|&(x, y)| img1.pixels[y * 80 + x] != 0).count();
        assert_eq!(left_changed, 0, "1-digit value left the left slots blank (right-justified)");

        // Alignment at the right edge: the units digit of "7" and the units digit
        // of "100" occupy the same column band [48,72). Both should have drawn
        // there (num_7 = index 107, num_0 = index 100 — both non-transparent).
        let units_7: usize =
            (0..24).flat_map(|y| (48..72).map(move |x| (x, y))).filter(|&(x, y)| img1.pixels[y * 80 + x] != 0).count();
        let units_100: usize =
            (0..24).flat_map(|y| (48..72).map(move |x| (x, y))).filter(|&(x, y)| img3.pixels[y * 80 + x] != 0).count();
        assert_eq!(units_7, units_100, "units digit of 1- and 3-digit values align at the right edge");
    }

    #[test]
    fn draw_num_right_justifies_at_scale_2() {
        // Regression for the virtual/pixel unit-mix bug: at scale != 1 the digit
        // must land at PIXEL (vx*scale), not pixel vx. Right edge virtual x=72,
        // scale=2 => the units digit must end at pixel 144 (its 24-virtual = 48-px
        // cell spans px [96,144)), and nothing draws at/after px 144.
        let wad = build_hud_wad();
        let mut img = Image::new(200, 48, 0);
        draw_num(&mut img, 7, 72.0, 0.0, BarXf { scale: 2.0, ox: 0.0, vy_top: 0.0 }, &wad, false);

        // Pixels exist in the cell [96,144); none at or past 144.
        let in_cell =
            (0..48).flat_map(|y| (96..144).map(move |x| (x, y))).filter(|&(x, y)| img.pixels[y * 200 + x] != 0).count();
        assert!(in_cell > 0, "scale=2 digit drew in the px[96,144) cell ending at the scaled right edge");
        let past_edge = (0..48)
            .flat_map(|y| (144..200).map(move |x| (x, y)))
            .filter(|&(x, y)| img.pixels[y * 200 + x] != 0)
            .count();
        assert_eq!(past_edge, 0, "nothing drew past the scaled right edge px=144");
        // And it must NOT be jammed against px=72 (the old bug placed it there).
        let at_virtual_edge =
            (0..48).flat_map(|y| (48..96).map(move |x| (x, y))).filter(|&(x, y)| img.pixels[y * 200 + x] != 0).count();
        assert_eq!(at_virtual_edge, 0, "digit must not sit at the unscaled px=72 edge (the old unit-mix bug)");
    }

    #[test]
    fn draw_hud_changes_bottom_strip_only() {
        let wad = build_hud_wad();

        // A solid-filled image; the HUD must change the bottom 24-virtual-row bar
        // but leave the top of the frame untouched. Use a 320-wide frame so
        // scale == 1 and the bar is exactly the bottom 24 rows.
        let fill = 42u8;
        let mut img = Image::new(320, 200, fill);
        let hud = Hud {
            wad: &wad,
            mode: GameMode::Id1,
            health: 100,
            ammo: 25,
            armor: 50,
            items: 0,
            weapon: 0,
            ammo_shells: 0,
            ammo_nails: 0,
            ammo_rockets: 0,
            ammo_cells: 0,
            time: 0.0,
            item_gettime: None,
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            sb_lines: SB_LINES_FULL,
            sbar_layout: SbarLayout::Classic,
            face_pain: false,
        };
        draw_hud_into(&mut img, &hud);

        // The top of the frame (well above the 24-px bar) is untouched.
        for y in 0..(200 - 24) {
            for x in 0..320 {
                assert_eq!(img.pixels[y * 320 + x], fill, "row {y} col {x} above the bar must be untouched");
            }
        }
        // The bottom strip changed (the sbar background, index 1 -> [1,1,1],
        // covers the whole 320x24 bar).
        let changed_bottom: usize = (200 - 24..200)
            .flat_map(|y| (0..320).map(move |x| (x, y)))
            .filter(|&(x, y)| img.pixels[y * 320 + x] != fill)
            .count();
        assert!(changed_bottom > 0, "the bottom strip changed under the HUD");

        // The digits (index >= 100) drew on top of the sbar background somewhere
        // in the bar — proving health/ammo/armour numbers actually rendered.
        let has_digit =
            (200 - 24..200).flat_map(|y| (0..320).map(move |x| (x, y))).any(|(x, y)| img.pixels[y * 320 + x] >= 100);
        assert!(has_digit, "at least one big-number digit drew over the bar");
    }

    #[test]
    fn draw_hud_scales_to_wide_frame() {
        // A 640-wide frame (scale 2): the bar must still bottom-anchor and span
        // the full width without panicking, leaving the top untouched.
        let wad = build_hud_wad();
        let fill = 7u8;
        let mut img = Image::new(640, 400, fill);
        let hud = Hud {
            wad: &wad,
            mode: GameMode::Id1,
            health: 99,
            ammo: 100,
            armor: 0,
            items: 0,
            weapon: 0,
            ammo_shells: 0,
            ammo_nails: 0,
            ammo_rockets: 0,
            ammo_cells: 0,
            time: 0.0,
            item_gettime: None,
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            sb_lines: SB_LINES_FULL,
            sbar_layout: SbarLayout::Classic,
            face_pain: false,
        };
        draw_hud_into(&mut img, &hud);

        // Bar height in pixels = 24 * (640/320) = 48; the top must be untouched.
        let bar_px = (24.0 * (640.0 / 320.0)) as usize; // 48
        for y in 0..(400 - bar_px) {
            assert_eq!(img.pixels[y * 640], fill, "row {y} above the scaled bar untouched");
        }
        // Bottom row changed across a wide span (the scaled sbar covers it).
        let bottom = 399 * 640;
        let bottom_changed = (0..640).filter(|&x| img.pixels[bottom + x] != fill).count();
        assert!(bottom_changed > 320, "scaled sbar spans most of the 640-wide bottom row");
    }

    #[test]
    fn draw_hud_missing_pics_is_noop_not_panic() {
        // An empty WAD (no sbar/num pics) must degrade gracefully: the frame is
        // returned unchanged, no panic.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"WAD2");
        bytes.extend_from_slice(&0i32.to_le_bytes()); // numlumps
        bytes.extend_from_slice(&(WADINFO_SIZE as i32).to_le_bytes());
        let wad = Wad2::parse(bytes).expect("empty wad parses");
        let fill = 9u8;
        let mut img = Image::new(320, 200, fill);
        let hud = Hud {
            wad: &wad,
            mode: GameMode::Id1,
            health: 100,
            ammo: 50,
            armor: 25,
            items: 0,
            weapon: 0,
            ammo_shells: 0,
            ammo_nails: 0,
            ammo_rockets: 0,
            ammo_cells: 0,
            time: 0.0,
            item_gettime: None,
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            sb_lines: SB_LINES_FULL,
            sbar_layout: SbarLayout::Classic,
            face_pain: false,
        };
        draw_hud_into(&mut img, &hud);
        assert!(img.pixels.iter().all(|&p| p == fill), "missing pics leave the frame unchanged");
    }

    // -- Inventory bar / face / icons (Sbar_DrawInventory + Sbar_DrawFace) -----

    #[test]
    fn face_bracket_matches_sbar_drawface() {
        // Sbar_DrawFace: health >= 100 -> 4; else health/20 (int div), clamped 0..4.
        assert_eq!(face_bracket(0), 0); // dead-ish -> lowest (face5)
        assert_eq!(face_bracket(19), 0);
        assert_eq!(face_bracket(20), 1);
        assert_eq!(face_bracket(39), 1);
        assert_eq!(face_bracket(40), 2);
        assert_eq!(face_bracket(59), 2);
        assert_eq!(face_bracket(60), 3);
        assert_eq!(face_bracket(79), 3);
        assert_eq!(face_bracket(80), 4);
        assert_eq!(face_bracket(99), 4);
        assert_eq!(face_bracket(100), 4); // full health (face1)
        assert_eq!(face_bracket(250), 4); // mega-health still clamps to 4
        assert_eq!(face_bracket(-50), 0); // never panics / never out of range
        // The bracket indexes the FACE_NAMES table (face5..face1).
        assert_eq!(FACE_NAMES[face_bracket(100)], "face1");
        assert_eq!(FACE_NAMES[face_bracket(10)], "face5");
    }

    #[test]
    fn new_weapon_icons_flash_for_a_second() {
        // Sbar_DrawInventory: flashon = (int)((cl.time - item_gettime[i])*10);
        // < 10 cycles sb_weapons[flashon%5 + 2] (inva1..5), then inv2 for the
        // active weapon and inv for the rest. No get-times: settled.
        let wad = build_full_hud_wad();
        let mut gettime = [0.0f32; 32];
        gettime[4] = 10.0; // the rocket launcher (bit 4), got at t=10
        let hud = |time: f32, gt: Option<&'static [f32; 32]>| Hud {
            wad: &wad,
            mode: GameMode::Id1,
            health: 100,
            ammo: 0,
            armor: 0,
            items: IT_SHOTGUN | IT_SHOTGUN << 4,
            weapon: IT_SHOTGUN,
            ammo_shells: 0,
            ammo_nails: 0,
            ammo_rockets: 0,
            ammo_cells: 0,
            time,
            item_gettime: gt,
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            face_pain: false,
            sb_lines: SB_LINES_FULL,
            sbar_layout: SbarLayout::Classic,
        };
        let gt: &'static [f32; 32] = Box::leak(Box::new(gettime));
        let name = |h: &Hud, i: usize| weapon_icon_name(weapon_flashon(h, i), i);
        assert_eq!(name(&hud(10.0, Some(gt)), 4), "inva1_rlaunch");
        assert_eq!(name(&hud(10.15, Some(gt)), 4), "inva2_rlaunch");
        assert_eq!(name(&hud(10.45, Some(gt)), 4), "inva5_rlaunch");
        assert_eq!(name(&hud(10.55, Some(gt)), 4), "inva1_rlaunch", "wraps after 5");
        assert_eq!(name(&hud(11.0, Some(gt)), 4), "inv_rlaunch", "settled, not active");
        assert_eq!(name(&hud(11.0, Some(gt)), 0), "inv2_shotgun", "settled, active");
        assert_eq!(name(&hud(10.0, None), 4), "inv_rlaunch", "no get-times: settled");
        assert_eq!(weapon_icon_name(6, 6), "inva5_lightng");
    }

    #[test]
    fn intermission_overlay_draws_banner_plaque_and_numbers() {
        // Sbar_IntermissionOverlay on a 320x200 frame (scale 1, no offsets): the
        // banner at (64,24), the plaque at (0,56), and the verbatim sbar.c number
        // positions — minutes right-justified into 3 slots from x=160, colon at
        // 234, second digits at 246/266; secrets/monsters rows at y=104/144 with
        // num_slash at 232 and the totals from x=240.
        let wad = build_hud_wad();
        let mut img = Image::new(320, 200, 0);
        let complete = Qpic { width: 192, height: 24, data: vec![50u8; 192 * 24] };
        let inter = Qpic { width: 160, height: 144, data: vec![51u8; 160 * 144] };
        let stats = IntermissionStats {
            completed_time: 205, // 3:25
            secrets: 3,
            total_secrets: 7,
            monsters: 12,
            total_monsters: 45,
        };
        draw_intermission_overlay(&mut img, &wad, Some(&complete), Some(&inter), &stats);

        let px = |x: usize, y: usize| img.pixels[y * 320 + x];
        assert_eq!(px(64 + 5, 24 + 5), 50, "complete.lmp banner at (64,24)");
        assert_eq!(px(5, 56 + 100), 51, "inter.lmp plaque at (0,56)");
        // Time 3:25 — "3" right-justified: x = 160 + 2*24 = 208 (num_3 = idx 103).
        assert_eq!(px(208 + 2, 64 + 2), 103, "minutes digit 3 at x=208");
        assert_eq!(px(234 + 2, 64 + 2), 140, "num_colon at x=234");
        assert_eq!(px(246 + 2, 64 + 2), 102, "seconds tens 2 at x=246");
        assert_eq!(px(266 + 2, 64 + 2), 105, "seconds units 5 at x=266");
        // Secrets 3/7: found at x=208 (right-justified), slash 232, total at 288.
        assert_eq!(px(208 + 2, 104 + 2), 103, "secrets found 3");
        assert_eq!(px(232 + 2, 104 + 2), 141, "num_slash at x=232");
        assert_eq!(px(240 + 2 * 24 + 2, 104 + 2), 107, "secrets total 7");
        // Monsters 12/45: two digits start at x = 160 + 24 = 184.
        assert_eq!(px(184 + 2, 144 + 2), 101, "monsters tens 1 at x=184");
        assert_eq!(px(208 + 2, 144 + 2), 102, "monsters units 2 at x=208");
        assert_eq!(px(264 + 2, 144 + 2), 104, "total tens 4 at x=264");
        assert_eq!(px(288 + 2, 144 + 2), 105, "total units 5 at x=288");
        // The (missing-pic) graceful path: no panic with both pics absent.
        let mut img2 = Image::new(320, 200, 0);
        draw_intermission_overlay(&mut img2, &wad, None, None, &stats);
        assert_eq!(px(208 + 2, 64 + 2), 103, "numbers still draw without pics");
    }

    #[test]
    fn intermission_overlay_centres_on_the_scaled_2d_screen_as_the_bar_and_finale_do() {
        // Off, id's absolute coordinates: a 640x400 screen keeps the overlay
        // in its top-left corner. On (slop), a 2-D screen wider than 320 —
        // a wide 1315x535 frame at scale 2 (658 wide), and 1920x1080
        // at scale 5 (384 wide) — centres its 320 columns as Sbar_DrawPic
        // centres the bar; a 16:10 frame (320 wide) is id's placement.
        let wad = build_hud_wad();
        let complete = Qpic { width: 192, height: 24, data: vec![50u8; 192 * 24] };
        let inter = Qpic { width: 160, height: 144, data: vec![51u8; 160 * 144] };
        let finale = Qpic { width: 288, height: 24, data: vec![52u8; 288 * 24] };
        let stats =
            IntermissionStats { completed_time: 205, secrets: 3, total_secrets: 7, monsters: 12, total_monsters: 45 };
        // (scaled 2-D, frame, scale, framebuffer x of the overlay's x = 0)
        for (on, (w, h), scale, ox) in [
            (false, (640, 400), 1, 0),
            (false, (1920, 1080), 1, 0),
            (true, (1315, 535), 2, 338),  // ((658 - 320) >> 1) * 2
            (true, (1920, 1080), 5, 160), // ((384 - 320) >> 1) * 5
            (true, (1280, 800), 4, 0),
        ] {
            let _extra = crate::draw::Scaled2dGuard::set(on);
            let case = format!("{w}x{h} scaled 2-D {on}");
            let mut img = Image::new(w, h, 0);
            draw_intermission_overlay(&mut img, &wad, Some(&complete), Some(&inter), &stats);
            let px = |x: usize, y: usize| img.pixels[y * w + x];
            // Each pic's leftmost framebuffer column, on a row it covers.
            let left = |idx: u8, y: usize| (0..w).find(|&x| px(x, y * scale) == idx);
            assert_eq!(left(51, 100), Some(ox), "{case}: inter.lmp at x = 0");
            assert_eq!(left(50, 30), Some(ox + 64 * scale), "{case}: complete.lmp at x = 64");
            assert_eq!(px(ox + 210 * scale, 66 * scale), 103, "{case}: the minutes' 3 at x = 208");
            assert_eq!(px(ox + 290 * scale, 146 * scale), 105, "{case}: the monster total's 5 at x = 288");
            if !on {
                continue;
            }
            // In line with the status bar's 320 columns ...
            assert_eq!(BarXf::new(w, h).ox, ox as f32, "{case}: the bar's x = 0");
            // ... and the finale plaque's centre, (vid.width - 288) / 2.
            let mut fin = Image::new(w, h, 0);
            draw_finale_overlay(&mut fin, None, Some(&finale), "", 0.0);
            let row = &fin.pixels[20 * scale * w..(20 * scale + 1) * w];
            let lit: Vec<usize> = (0..w).filter(|&x| row[x] == 52).collect();
            let (a, b) = (lit[0], lit[lit.len() - 1] + 1);
            assert_eq!((a + b) / 2, ox + 160 * scale, "{case}: one centre with the finale");
            // Which is the frame's own, to within half a 2-D pixel.
            assert!((ox + 160 * scale).abs_diff(w / 2) * 2 <= scale, "{case}: centred");
        }
    }

    #[test]
    fn intermission_number_draws_leading_minus_glyph() {
        // Sbar_IntermissionNumber: Sbar_itoa keeps the sign, and a '-' draws
        // sb_nums[0][STAT_MINUS] — "num_minus" (index 142 in the test wad). -7
        // over 3 slots right-justifies like any 2-character number: the minus
        // lands at x = 160 + 24 = 184 and the digit at 208.
        let wad = build_hud_wad();
        let mut img = Image::new(320, 200, 0);
        intermission_number(&mut img, &wad, -7, 160.0, 64.0, 3, 1.0, 0.0, 0.0);
        let px = |x: usize, y: usize| img.pixels[y * 320 + x];
        assert_eq!(px(160 + 2, 64 + 2), 0, "first slot empty (2-char number)");
        assert_eq!(px(184 + 2, 64 + 2), 142, "minus glyph at x=184");
        assert_eq!(px(208 + 2, 64 + 2), 107, "digit 7 at x=208");
    }

    /// A fuller synthetic `gfx.wad` adding the inventory-bar art the base
    /// `build_hud_wad` omits: `ibar` (320x24, index 2), the 7 `inv_*` weapon icons +
    /// the 35 `inva{1..5}_*` flash icons (24x16, index 60), the 5 health faces +
    /// 4 powerup faces (24x24, index 70), the armour/ammo-type icons (24x24, index
    /// 80/85), the key/powerup/sigil item icons, and a 128x128 `conchars` whose
    /// gold-digit cells (18..27) are non-zero so the ammo counts render.
    fn build_full_hud_wad() -> Wad2 {
        let mut pics: Vec<(String, Vec<u8>)> = Vec::new();
        pics.push(("sbar".to_string(), qpic_payload(320, 24, 1)));
        pics.push(("ibar".to_string(), qpic_payload(320, 24, 2)));
        for d in 0..10u8 {
            pics.push((format!("num_{d}"), qpic_payload(24, 24, 100 + d)));
        }
        for d in 0..10u8 {
            pics.push((format!("anum_{d}"), qpic_payload(24, 24, 120 + d)));
        }
        // Weapon icons (dim inv_*, bright active inv2_*, and the 5 flash frames),
        // distinct index 60 so they show.
        for s in WEAPON_SUFFIX {
            pics.push((format!("inv_{s}"), qpic_payload(24, 16, 60)));
            pics.push((format!("inv2_{s}"), qpic_payload(24, 16, 60)));
            for f in 1..=5u8 {
                pics.push((format!("inva{f}_{s}"), qpic_payload(24, 16, 60)));
            }
        }
        // Faces (health brackets + powerups), index 70.
        for name in ["face5", "face4", "face3", "face2", "face1", "face_inv2", "face_quad", "face_invis", "face_invul2"]
        {
            pics.push((name.to_string(), qpic_payload(24, 24, 70)));
        }
        // Pain faces, index 71.
        for name in FACE_PAIN_NAMES {
            pics.push((name.to_string(), qpic_payload(24, 24, 71)));
        }
        // Armour-type + ammo-type icons, index 80 / 85.
        for name in ARMOR_ICON_NAMES {
            pics.push((name.to_string(), qpic_payload(24, 24, 80)));
        }
        for name in AMMO_ICON_NAMES {
            pics.push((name.to_string(), qpic_payload(24, 24, 85)));
        }
        // Keys / powerups / sigils, index 90.
        for name in SB_ITEM_NAMES {
            pics.push((name.to_string(), qpic_payload(16, 16, 90)));
        }
        for name in SB_SIGIL_NAMES {
            pics.push((name.to_string(), qpic_payload(8, 16, 90)));
        }
        // conchars: a raw 128x128 byte block (NO qpic header) — the gold digit
        // cells 18..27 (rows 1, cols 2..11) set to index 95 so the small ammo
        // counts draw a recognisable colour.
        let mut conchars_raw = vec![0u8; 128 * 128];
        for cell in 18..=27usize {
            let cx = (cell % 16) * 8;
            let cy = (cell / 16) * 8;
            for gy in 0..8 {
                for gx in 0..8 {
                    conchars_raw[(cy + gy) * 128 + (cx + gx)] = 95;
                }
            }
        }

        // Lay payloads after the 12-byte header. `conchars` is appended RAW (no
        // qpic_payload header) and registered as a non-QPIC lump via push_raw_lump.
        let mut payloads = Vec::new();
        let mut offsets = Vec::new();
        let mut pos = WADINFO_SIZE;
        for (_, p) in &pics {
            offsets.push(pos);
            payloads.extend_from_slice(p);
            pos += p.len();
        }
        let conchars_off = pos;
        payloads.extend_from_slice(&conchars_raw);
        pos += conchars_raw.len();
        let infotableofs = pos;

        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"WAD2");
        bytes.extend_from_slice(&((pics.len() + 1) as i32).to_le_bytes());
        bytes.extend_from_slice(&(infotableofs as i32).to_le_bytes());
        bytes.extend_from_slice(&payloads);

        let mut dir = Vec::new();
        for ((name, p), &off) in pics.iter().zip(offsets.iter()) {
            push_lump(&mut dir, off as i32, p.len() as i32, name);
        }
        // conchars as a raw lump (its type does not matter — lump_data reads bytes).
        push_lump(&mut dir, conchars_off as i32, conchars_raw.len() as i32, "conchars");
        bytes.extend_from_slice(&dir);

        Wad2::parse(bytes).expect("synthetic full gfx.wad parses")
    }

    #[test]
    fn draw_hud_inventory_bar_draws_above_sbar() {
        // With the full wad, the ibar (index 2) must fill the 24 virtual rows ABOVE
        // the 24-row sbar; a face must draw on the sbar; and the inventory elements
        // must appear at their sbar.c positions.
        let wad = build_full_hud_wad();
        let fill = 42u8;
        let mut img = Image::new(320, 200, fill); // scale 1: bar = bottom 48 rows
        let hud = Hud {
            wad: &wad,
            mode: GameMode::Id1,
            health: 100, // -> face1 (bracket 4)
            ammo: 25,
            armor: 80,
            // shotgun(1) + nailgun(4) owned; armour3; shells ammo type;
            // key1 (bit 17) + quad (bit 22); sigil1 (bit 28).
            items: IT_SHOTGUN | (IT_SHOTGUN << 2) | IT_ARMOR3 | IT_SHELLS | (1 << 17) | (1 << 22) | (1 << 28),
            weapon: IT_SHOTGUN, // shotgun selected -> flashes inva*_shotgun
            ammo_shells: 100,
            ammo_nails: 0,
            ammo_rockets: 0,
            ammo_cells: 0,
            time: 0.0,
            item_gettime: None,
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            sb_lines: SB_LINES_FULL,
            sbar_layout: SbarLayout::Classic,
            face_pain: false,
        };
        draw_hud_into(&mut img, &hud);

        // The ibar (index 2) occupies virtual rows -24..0, i.e. framebuffer rows
        // 152..176 at scale 1. Its background colour [2,2,2] must appear there.
        let ibar_rows = 152..176;
        let ibar_bg = ibar_rows
            .clone()
            .flat_map(|y| (0..320).map(move |x| (x, y)))
            .filter(|&(x, y)| img.pixels[y * 320 + x] == 2)
            .count();
        assert!(ibar_bg > 0, "ibar background fills the 24 rows above the sbar");

        // Rows above the whole 48-row status area (y < 152) stay the fill colour.
        for y in 0..152 {
            for x in 0..320 {
                assert_eq!(img.pixels[y * 320 + x], fill, "row {y} col {x} above the status area untouched");
            }
        }

        // A weapon icon (index 60) drew on the ibar (the active shotgun's bright
        // inv2_shotgun icon at x=0, y=-16 -> framebuffer rows ~160..176).
        let weapon_px = (160..176)
            .flat_map(|y| (0..24).map(move |x| (x, y)))
            .filter(|&(x, y)| img.pixels[y * 320 + x] == 60)
            .count();
        assert!(weapon_px > 0, "active weapon (inv2_*) icon drew on the ibar");

        // The face (index 70) drew at x=112 on the sbar (rows 176..200).
        let face_px = (176..200)
            .flat_map(|y| (112..136).map(move |x| (x, y)))
            .filter(|&(x, y)| img.pixels[y * 320 + x] == 70)
            .count();
        assert!(face_px > 0, "player face drew at x=112 on the sbar");

        // The armour-type icon (index 80) drew at x=0 on the sbar.
        let armor_icon = (176..200)
            .flat_map(|y| (0..24).map(move |x| (x, y)))
            .filter(|&(x, y)| img.pixels[y * 320 + x] == 80)
            .count();
        assert!(armor_icon > 0, "armour-type icon drew at x=0");

        // The ammo-type icon (index 85) drew at x=224 on the sbar.
        let ammo_icon = (176..200)
            .flat_map(|y| (224..248).map(move |x| (x, y)))
            .filter(|&(x, y)| img.pixels[y * 320 + x] == 85)
            .count();
        assert!(ammo_icon > 0, "ammo-type icon drew at x=224");

        // The small ammo counts (gold conchars digits, index 95) drew on the ibar.
        let count_px = ibar_rows
            .flat_map(|y| (0..320).map(move |x| (x, y)))
            .filter(|&(x, y)| img.pixels[y * 320 + x] == 95)
            .count();
        assert!(count_px > 0, "small ammo counts drew on the ibar");
        // "100" shells: the first digit at (6*0+1)*8 - 2, plus Sbar_DrawCharacter's 4.
        let first_x = (152..176)
            .flat_map(|y| (0..320).map(move |x| (x, y)))
            .filter(|&(x, y)| img.pixels[y * 320 + x] == 95)
            .map(|(x, _)| x)
            .min();
        assert_eq!(first_x, Some(10), "the shells count starts at x = 8 - 2 + 4");

        // A sigil (index 90) drew near the right edge of the ibar (x≈288).
        let sigil_px = (160..176)
            .flat_map(|y| (288..296).map(move |x| (x, y)))
            .filter(|&(x, y)| img.pixels[y * 320 + x] == 90)
            .count();
        assert!(sigil_px > 0, "sigil drew near the right edge of the ibar");
    }

    #[test]
    fn draw_hud_powerup_face_overrides_health_face() {
        // With quad active, Sbar_DrawFace draws the quad face regardless of health.
        let wad = build_full_hud_wad();
        let mut img = Image::new(320, 200, 0u8);
        let hud = Hud {
            wad: &wad,
            mode: GameMode::Id1,
            health: 100,
            ammo: 0,
            armor: 0,
            items: IT_QUAD,
            weapon: 0,
            ammo_shells: 0,
            ammo_nails: 0,
            ammo_rockets: 0,
            ammo_cells: 0,
            time: 0.0,
            item_gettime: None,
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            sb_lines: SB_LINES_FULL,
            sbar_layout: SbarLayout::Classic,
            face_pain: false,
        };
        // All face pics share index 70 here, so we can't distinguish quad vs health
        // by colour — instead assert the call path doesn't panic and a face drew.
        draw_hud_into(&mut img, &hud);
        let face_px = (176..200)
            .flat_map(|y| (112..136).map(move |x| (x, y)))
            .filter(|&(x, y)| img.pixels[y * 320 + x] == 70)
            .count();
        assert!(face_px > 0, "a powerup (quad) face drew at x=112");
    }

    #[test]
    fn draw_hud_pain_face_while_face_anim_runs() {
        // Sbar_DrawFace: `sb_faces[f][cl.time <= cl.faceanimtime]` — the pain
        // face of the health bracket right after a hit.
        let wad = build_full_hud_wad();
        let face = |face_pain: bool| {
            let mut img = Image::new(320, 200, 0u8);
            let hud = Hud {
                wad: &wad,
                mode: GameMode::Id1,
                health: 45,
                ammo: 0,
                armor: 0,
                items: 0,
                weapon: 0,
                ammo_shells: 0,
                ammo_nails: 0,
                ammo_rockets: 0,
                ammo_cells: 0,
                time: 0.0,
                item_gettime: None,
                monsters: 0,
                total_monsters: 0,
                secrets: 0,
                total_secrets: 0,
                level_name: "",
                show_scores: false,
                sb_lines: SB_LINES_FULL,
                sbar_layout: SbarLayout::Classic,
                face_pain,
            };
            draw_hud_into(&mut img, &hud);
            img.pixels[188 * 320 + 124]
        };
        assert_eq!(face(false), 70, "the steady face");
        assert_eq!(face(true), 71, "the pain face");
        assert_eq!(FACE_PAIN_NAMES[face_bracket(45)], "face_p3");
    }

    /// A gfx.wad with the three status-bar strips as solid colours: `sbar`
    /// (index 1), `ibar` (2) and `scorebar` (3).
    fn build_sbar_strips_wad() -> Wad2 {
        let pics: Vec<(String, Vec<u8>)> = vec![
            ("sbar".to_string(), qpic_payload(320, 24, 1)),
            ("ibar".to_string(), qpic_payload(320, 24, 2)),
            ("scorebar".to_string(), qpic_payload(320, 24, 3)),
        ];
        let mut payloads = Vec::new();
        let mut offsets = Vec::new();
        let mut pos = WADINFO_SIZE;
        for (_, p) in &pics {
            offsets.push(pos);
            payloads.extend_from_slice(p);
            pos += p.len();
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"WAD2");
        bytes.extend_from_slice(&(pics.len() as i32).to_le_bytes());
        bytes.extend_from_slice(&(pos as i32).to_le_bytes());
        bytes.extend_from_slice(&payloads);
        let mut dir = Vec::new();
        for ((name, p), &off) in pics.iter().zip(offsets.iter()) {
            push_lump(&mut dir, off as i32, p.len() as i32, name);
        }
        bytes.extend_from_slice(&dir);
        Wad2::parse(bytes).expect("synthetic strips wad parses")
    }

    #[test]
    fn draw_hud_follows_sb_lines_like_sbar_draw() {
        let wad = build_sbar_strips_wad();
        let fill = 42u8;
        let draw = |sb_lines: i32, health: i32| {
            let mut img = Image::new(320, 200, fill);
            let hud = Hud {
                wad: &wad,
                mode: GameMode::Id1,
                health,
                ammo: 0,
                armor: 0,
                items: 0,
                weapon: 0,
                ammo_shells: 0,
                ammo_nails: 0,
                ammo_rockets: 0,
                ammo_cells: 0,
                time: 0.0,
                item_gettime: None,
                monsters: 0,
                total_monsters: 0,
                secrets: 0,
                total_secrets: 0,
                level_name: "",
                show_scores: false,
                face_pain: false,
                sb_lines,
                sbar_layout: SbarLayout::Classic,
            };
            draw_hud_into(&mut img, &hud);
            img
        };
        let ibar_row = 160 * 320 + 5; // inside rows 152..176
        let sbar_row = 190 * 320 + 5; // inside rows 176..200
        // 48 lines (viewsize <= 100): inventory strip over the status strip.
        let img = draw(48, 100);
        assert_eq!((img.pixels[ibar_row], img.pixels[sbar_row]), (2, 1));
        assert_eq!(img.pixels[151 * 320 + 5], fill, "nothing above the 48 lines");
        // 24 lines (viewsize 110): the status strip alone.
        let img = draw(24, 100);
        assert_eq!((img.pixels[ibar_row], img.pixels[sbar_row]), (fill, 1));
        // 0 lines (viewsize 120): no status bar at all.
        let img = draw(0, 100);
        assert!(img.pixels.iter().all(|&p| p == fill), "sb_lines 0 draws nothing");
        // ...except the death scoreboard, which Sbar_Draw shows regardless.
        let img = draw(0, 0);
        assert_eq!((img.pixels[ibar_row], img.pixels[sbar_row]), (fill, 3));
        let img = draw(48, 0);
        assert_eq!((img.pixels[ibar_row], img.pixels[sbar_row]), (2, 3));
    }

    #[test]
    fn the_bar_over_the_view_leaves_the_game_either_side() {
        // A 2-D screen wider than the bar: 960x600 at 1:1, the bar's 48 rows
        // (552..600) in columns 320..640. The wad has no backtile, so id's
        // tile clear either side fills black (Draw_TileClear's fallback).
        let wad = build_sbar_strips_wad();
        let fill = 42u8;
        let draw = |sbar_layout: SbarLayout| {
            let mut img = Image::new(960, 600, fill);
            let hud = Hud {
                wad: &wad,
                mode: GameMode::Id1,
                health: 100,
                ammo: 0,
                armor: 0,
                items: 0,
                weapon: 0,
                ammo_shells: 0,
                ammo_nails: 0,
                ammo_rockets: 0,
                ammo_cells: 0,
                time: 0.0,
                item_gettime: None,
                monsters: 0,
                total_monsters: 0,
                secrets: 0,
                total_secrets: 0,
                level_name: "",
                show_scores: false,
                face_pain: false,
                sb_lines: SB_LINES_FULL,
                sbar_layout,
            };
            draw_hud_into(&mut img, &hud);
            img.pixels
        };
        let (id, over) = (draw(SbarLayout::Classic), draw(SbarLayout::Overlay));
        for y in 0..600 {
            for x in 0..960 {
                let i = y * 960 + x;
                let (bar_row, bar_col) = (y >= 552, (320..640).contains(&x));
                if bar_row && bar_col {
                    assert_eq!(over[i], id[i], "({x},{y}): the bar itself is the same");
                    assert_ne!(over[i], fill, "({x},{y}): its opaque pics cover it");
                } else if bar_row {
                    assert_eq!(
                        (id[i], over[i]),
                        (0, fill),
                        "({x},{y}): id tile-clears the side, the overlay leaves the view"
                    );
                } else {
                    assert_eq!((id[i], over[i]), (fill, fill), "({x},{y}): above the bar, untouched");
                }
            }
        }
    }

    // -- Hipnotic / Rogue item bits -------------------------------------------

    /// Every mission-pack constant against `quakedef.h`'s own numbers (not
    /// id's bit-shift expressions, the literal values), since there is no C
    /// oracle to catch a transcription slip here (the oracle's `-rogue` run
    /// hangs on at least one map — the mission's report says why — so this
    /// module's Rogue arms are proven only by these and the `draw_hud_into`
    /// tests below, not pixel-for-pixel against id's C).
    #[test]
    fn mission_pack_item_bits_match_quakedef_h() {
        assert_eq!(HIT_MJOLNIR_BIT, 7);
        assert_eq!(HIT_PROXIMITY_GUN_BIT, 16);
        assert_eq!(HIT_LASER_CANNON_BIT, 23);
        assert_eq!(HIT_PROXIMITY_GUN, 65536);
        assert_eq!(HIPWEAPONS, [23, 7, 4, 16]);
        assert_eq!(RIT_LAVA_NAILGUN, 4096);
        assert_eq!(RIT_SHELLS, 128);
        assert_eq!(RIT_NAILS, 256);
        assert_eq!(RIT_ROCKETS, 512);
        assert_eq!(RIT_CELLS, 1024);
        assert_eq!(RIT_ARMOR1, 8_388_608);
        assert_eq!(RIT_ARMOR2, 16_777_216);
        assert_eq!(RIT_ARMOR3, 33_554_432);
        assert_eq!(RIT_LAVA_NAILS, 67_108_864);
        assert_eq!(RIT_PLASMA_AMMO, 134_217_728);
        assert_eq!(RIT_MULTI_ROCKETS, 268_435_456);
        assert_eq!(1 << RIT_SHIELD_BIT, 536_870_912);
        assert_eq!(1 << (RIT_SHIELD_BIT + 1), 1_073_741_824);
        // The five tier-2 weapon icons are RIT_LAVA_NAILGUN<<0..4 exactly:
        // lava nailgun, lava super nailgun, multi-grenade (pic "r_gren"),
        // multi-rocket, plasma gun.
        assert_eq!(
            [
                RIT_LAVA_NAILGUN,
                RIT_LAVA_NAILGUN << 1,
                RIT_LAVA_NAILGUN << 2,
                RIT_LAVA_NAILGUN << 3,
                RIT_LAVA_NAILGUN << 4
            ],
            [4096, 8192, 16384, 32768, 65536]
        );
    }

    /// [`flashon_for`] (Hipnotic's weapons) against the same formula
    /// [`weapon_flashon`]'s own tests already prove for the standard ones —
    /// settled-active, settled-inactive, and the `inva1..5` cycle in the
    /// first second — just at Hipnotic's bit/slot (laser cannon, bit 23).
    #[test]
    fn flashon_for_cycles_like_weapon_flashon_at_a_different_bit_and_slot() {
        let wad = build_hud_wad();
        let laser = 1 << HIT_LASER_CANNON_BIT;
        let mk = |time: f32, gettime: &'static [f32; 32], weapon: i32| Hud {
            wad: &wad,
            mode: GameMode::Hipnotic,
            health: 100,
            ammo: 0,
            armor: 0,
            items: laser,
            weapon,
            ammo_shells: 0,
            ammo_nails: 0,
            ammo_rockets: 0,
            ammo_cells: 0,
            time,
            item_gettime: Some(gettime),
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            face_pain: false,
            sb_lines: SB_LINES_FULL,
            sbar_layout: SbarLayout::Classic,
        };
        static GT: [f32; 32] = {
            let mut g = [0.0f32; 32];
            g[HIT_LASER_CANNON_BIT] = 10.0; // got at t=10
            g
        };
        // Just got it (t=10.3, 0.3s in -> raw 3, wrapped 3%5+2=5 -> inva4):
        // neither settled value, so weapon-active doesn't matter yet.
        assert_eq!(flashon_for(&mk(10.3, &GT, 0), laser, HIT_LASER_CANNON_BIT), 5);
        // A full second on: settled. Active -> 1; not active -> 0.
        assert_eq!(flashon_for(&mk(11.0, &GT, laser), laser, HIT_LASER_CANNON_BIT), 1);
        assert_eq!(flashon_for(&mk(11.0, &GT, 0), laser, HIT_LASER_CANNON_BIT), 0);
        // No get-times at all (a demo without them): always settled.
        let no_gt = Hud { item_gettime: None, ..mk(11.0, &GT, laser) };
        assert_eq!(flashon_for(&no_gt, laser, HIT_LASER_CANNON_BIT), 1);
    }

    /// `hsb_weapons`/`rsb_weapons`/`hsb_items`/`rsb_items` icon names resolve
    /// to id's own lump names, and the mode gate actually changes what draws:
    /// a Hipnotic Hud shows the laser cannon in its own slot and wetsuit/
    /// shields in the sigil slot; the SAME items bits under `GameMode::Id1`
    /// (where they mean nothing) draw neither.
    #[test]
    fn hipnotic_weapons_and_items_draw_only_in_hipnotic_mode() {
        // Each lump this test touches gets a distinct fill, so "something
        // drew here" is unambiguous.
        let wad = build_hud_wad_with(&[
            ("inv_laser", 250),
            ("inv_mjolnir", 251),
            ("sb_wsuit", 252),
            ("sb_eshld", 253),
            ("ibar", 254),
        ]);

        let laser = 1 << HIT_LASER_CANNON_BIT;
        let mjolnir = 1 << HIT_MJOLNIR_BIT;
        let wetsuit = 1 << 24; // sbar.c's own `1<<(24+i)`, not HIT_WETSUIT (see the comment above)
        let shields = 1 << 25;
        let items = laser | mjolnir | wetsuit | shields;
        let base = |mode: GameMode| Hud {
            wad: &wad,
            mode,
            health: 100,
            ammo: 0,
            armor: 0,
            items,
            weapon: 0,
            ammo_shells: 0,
            ammo_nails: 0,
            ammo_rockets: 0,
            ammo_cells: 0,
            time: 0.0,
            item_gettime: None,
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            face_pain: false,
            sb_lines: SB_LINES_FULL,
            sbar_layout: SbarLayout::Classic,
        };

        let drew = |mode: GameMode, fill: u8| {
            let mut img = Image::new(320, 200, 0);
            draw_hud_into(&mut img, &base(mode));
            img.pixels.contains(&fill)
        };
        assert!(drew(GameMode::Hipnotic, 250), "laser cannon icon drew in Hipnotic mode");
        assert!(drew(GameMode::Hipnotic, 251), "mjolnir icon drew in Hipnotic mode");
        assert!(drew(GameMode::Hipnotic, 252), "wetsuit icon drew in Hipnotic mode");
        assert!(drew(GameMode::Hipnotic, 253), "empathy shields icon drew in Hipnotic mode");
        assert!(!drew(GameMode::Id1, 250), "the same items bits mean nothing to id1's own sbar path");
        assert!(!drew(GameMode::Id1, 252), "id1 never draws Hipnotic's item icons");
    }

    /// Rogue's remapped armour bit and tier-2 weapon row: the standard
    /// `IT_ARMOR1` bit (which Rogue repurposes for `RIT_LAVA_SUPER_NAILGUN`)
    /// does NOT show an armour icon in Rogue mode, but `RIT_ARMOR1` does; the
    /// active tier-2 weapon's own icon draws over the standard row's slot.
    #[test]
    fn rogue_remaps_the_armour_bits_and_draws_its_tier2_weapon_icon() {
        let wad = build_hud_wad_with(&[
            ("r_lava", 240),
            ("sb_armor1", 241),
            ("ibar", 242),
            ("r_invbar1", 243),
            ("r_invbar2", 244),
        ]);

        let base = |mode: GameMode, items: i32, weapon: i32| Hud {
            wad: &wad,
            mode,
            health: 100,
            ammo: 0,
            armor: 10,
            items,
            weapon,
            ammo_shells: 0,
            ammo_nails: 0,
            ammo_rockets: 0,
            ammo_cells: 0,
            time: 0.0,
            item_gettime: None,
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            face_pain: false,
            sb_lines: SB_LINES_FULL,
            sbar_layout: SbarLayout::Classic,
        };
        let drew = |hud: &Hud, fill: u8| {
            let mut img = Image::new(320, 200, 0);
            draw_hud_into(&mut img, hud);
            img.pixels.contains(&fill)
        };

        // Standard IT_ARMOR1 (8192): nothing, in Rogue mode (it's a weapon bit there).
        assert!(!drew(&base(GameMode::Rogue, IT_ARMOR1, 0), 241), "IT_ARMOR1 means a weapon to Rogue, not armour");
        // Rogue's own RIT_ARMOR1 (8388608): the armour icon.
        assert!(drew(&base(GameMode::Rogue, RIT_ARMOR1, 0), 241), "RIT_ARMOR1 shows the armour icon in Rogue mode");
        // The active tier-2 weapon (lava nailgun) draws its own icon.
        assert!(drew(&base(GameMode::Rogue, 0, RIT_LAVA_NAILGUN), 240), "the active tier-2 weapon's own icon drew");
        // The inventory background swaps once a tier-2 weapon is active: Rogue
        // never draws the plain "ibar" at all, only its own r_invbar1/2.
        assert!(drew(&base(GameMode::Rogue, 0, 0), 244), "r_invbar2 below RIT_LAVA_NAILGUN");
        assert!(drew(&base(GameMode::Rogue, 0, RIT_LAVA_NAILGUN), 243), "r_invbar1 at or above RIT_LAVA_NAILGUN");
        assert!(!drew(&base(GameMode::Rogue, 0, 0), 242), "Rogue never draws the plain ibar");
    }
}

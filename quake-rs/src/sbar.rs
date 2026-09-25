//! The status bar, the scoreboard, and the intermission/finale overlays.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/sbar.c` — `Sbar_Draw` and its parts, `Sbar_DrawScoreboard`,
//! `Sbar_IntermissionOverlay`, `Sbar_FinaleOverlay`.

use crate::draw::{
    blit_qpic_at, conchars_pic, HUD_TRANSPARENT, HUD_VIRT_W, MENU_VIRT_H, MENU_VIRT_W,
};
use crate::render::Image;
use crate::screen::draw_center_string_revealed;

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
// [`Hud`] holding a borrow of the parsed `gfx.wad`, the palette, and the three
// integer stats read off the player edict; [`draw_hud_into`] then blits the bar
// scaled to the actual framebuffer width and bottom-anchored, so a 320, 480, or
// 640-wide frame all get a full-width bar.
//
// Integration choice (lowest churn): the HUD is a *separate* `pub fn
// draw_hud_into(image, hud)` the scene callers invoke on the returned `Image`,
// rather than a new parameter on `render_scene_ext`. This leaves the renderer's
// signature — and every existing call site and test — untouched, so
// `render_scene`/`render_scene_ext` draw no HUD and all prior tests stay green.
//
// Faithfulness/safety: every WAD pic is fetched with `wad.qpic(name).ok()`, so a
// missing or malformed pic simply doesn't draw (never panics, never errors out
// the frame). Pixel writes go through bounds-checked `Image::put`-style logic,
// and HUD-pic texels equal to palette index 255 are skipped (Quake's transparent
// colour for the status-bar pics).

/// The status bar's height in virtual rows (`sbar.c` draws it as the bottom 24
/// rows of the 320x200 virtual screen).
const HUD_BAR_H: f32 = 24.0;

/// The Quake HUD overlay: the parsed `gfx.wad`, the screen palette, and the
/// player stats to display. Built by the caller each frame from the player edict
/// and the loaded `gfx.wad`; consumed by [`draw_hud_into`].
///
/// The `wad`/`palette` borrows carry an explicit lifetime `'a` so the caller can
/// keep one parsed [`Wad2`] alive and lend it per frame without cloning.
pub struct Hud<'a> {
    /// The parsed `gfx.wad`, which holds the `sbar`/`ibar`/`num_*`/`anum_*`/face/
    /// weapon/item/ammo/armor pics and the `conchars` font.
    pub wad: &'a crate::wad::Wad2,
    /// The screen palette (`gfx/palette.lmp`), used to colour the pic texels.
    pub palette: &'a [[u8; 3]; 256],
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
    /// The server clock in seconds (`cl.time`), driving the selected-weapon flash
    /// cycle and the face pain/grimace animation. (The orchestrator passes the
    /// raw server time; the per-item acquire times of `cl.item_gettime[]` are not
    /// tracked here, so weapon icons show their static owned/selected frame — see
    /// the weapon-flash note in [`draw_hud_into`].)
    pub time: f32,
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
}

/// Blit one `Qpic` at virtual position `(vx, vy)` in 320x200 space, scaled by
/// `scale` to the framebuffer and bottom-anchored (so the 24-px bar sits flush
/// at the bottom of any-height frame).
///
/// `vy_top` is the framebuffer y (in pixels) of virtual row 0 of the bar, i.e.
/// `image.h - HUD_BAR_H * scale`; a pic at virtual `(vx, vy)` lands its top-left
/// at `(vx*scale, vy_top + vy*scale)`. Each destination pixel samples its source
/// texel nearest-neighbour; texels equal to [`HUD_TRANSPARENT`] (255) are left
/// transparent, leaving the underlying 3-D pixel untouched. Every write is
/// clipped to the framebuffer, so a pic that overhangs an edge never panics.
fn blit_qpic(
    image: &mut Image,
    pic: &crate::wad::Qpic,
    vx: f32,
    vy: f32,
    scale: f32,
    vy_top: f32,
    palette: &[[u8; 3]; 256],
) {
    if pic.width <= 0 || pic.height <= 0 || scale <= 0.0 {
        return;
    }
    let pw = pic.width as usize;
    let ph = pic.height as usize;
    // Guard against a truncated/short pixel buffer (never index past it).
    if pic.data.len() < pw.saturating_mul(ph) {
        return;
    }

    // Destination top-left in framebuffer pixels, and the scaled pic extent.
    let dst_x0 = (vx * scale).floor() as i64;
    let dst_y0 = (vy_top + vy * scale).floor() as i64;
    let dst_w = (pw as f32 * scale).round().max(1.0) as i64;
    let dst_h = (ph as f32 * scale).round().max(1.0) as i64;
    let inv_scale = 1.0 / scale;

    for dy in 0..dst_h {
        let py = dst_y0 + dy;
        if py < 0 || py >= image.h as i64 {
            continue;
        }
        // Map this destination row back to a source texel row (nearest).
        let sy = (dy as f32 * inv_scale) as usize;
        if sy >= ph {
            continue;
        }
        for dx in 0..dst_w {
            let px = dst_x0 + dx;
            if px < 0 || px >= image.w as i64 {
                continue;
            }
            let sx = (dx as f32 * inv_scale) as usize;
            if sx >= pw {
                continue;
            }
            let texel = match pic.data.get(sy * pw + sx) {
                Some(&t) => t,
                None => continue,
            };
            if texel == HUD_TRANSPARENT {
                continue; // transparent: leave the 3-D pixel as-is
            }
            image.put(px as i32, py as i32, palette[texel as usize]);
        }
    }
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
fn draw_num(
    image: &mut Image,
    value: i32,
    vx: f32,
    vy: f32,
    scale: f32,
    vy_top: f32,
    wad: &crate::wad::Wad2,
    palette: &[[u8; 3]; 256],
    alt: bool,
) {
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
        let name = if alt {
            ANUM_NAMES[d as usize]
        } else {
            NUM_NAMES[d as usize]
        };
        if let Ok(pic) = wad.qpic(name) {
            let w = pic.width.max(0) as f32;
            pen -= w;
            blit_qpic(image, &pic, pen, vy, scale, vy_top, palette);
        } else {
            // Missing digit pic: still advance by a default 24-virtual slot so
            // the remaining digits keep their right-justified positions.
            pen -= 24.0;
        }
    }
}

/// The white big-number digit pic names (`num_0`..`num_9`).
const NUM_NAMES: [&str; 10] = [
    "num_0", "num_1", "num_2", "num_3", "num_4", "num_5", "num_6", "num_7", "num_8", "num_9",
];

/// The gold/alternate digit pic names (`anum_0`..`anum_9`), used for ammo.
const ANUM_NAMES: [&str; 10] = [
    "anum_0", "anum_1", "anum_2", "anum_3", "anum_4", "anum_5", "anum_6", "anum_7", "anum_8",
    "anum_9",
];

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

/// `inv_*` (owned, dim) weapon icon lump names, `sb_weapons[0][i]` in `Sbar_Init`.
const WEAPON_INV_NAMES: [&str; 7] = [
    "inv_shotgun", "inv_sshotgun", "inv_nailgun", "inv_snailgun", "inv_rlaunch", "inv_srlaunch",
    "inv_lightng",
];
/// The per-weapon name suffixes (`*_shotgun` … `*_lightng`) shared by the
/// `inv_*`/`inv2_*`/`inva{1..5}_*` icon families (`Sbar_Init`). Used to build the
/// selection-flash frame names for the active weapon.
const WEAPON_SUFFIX: [&str; 7] = [
    "shotgun", "sshotgun", "nailgun", "snailgun", "rlaunch", "srlaunch", "lightng",
];

/// `sb_ammo[type]` ammo-icon lump names (`Sbar_Init`): shells/nails/rocket/cells.
const AMMO_ICON_NAMES: [&str; 4] = ["sb_shells", "sb_nails", "sb_rocket", "sb_cells"];

/// `sb_armor[type]` armour-icon lump names (`Sbar_Init`).
const ARMOR_ICON_NAMES: [&str; 3] = ["sb_armor1", "sb_armor2", "sb_armor3"];

/// `sb_items[0..6]` (`Sbar_Init`): the keys + powerup icons drawn on the ibar.
/// In `items`-bit order from bit 17: key1, key2, invisibility(ring), invuln(pent),
/// suit, quad — matching `cl.items & (1<<(17+i))`.
const SB_ITEM_NAMES: [&str; 6] =
    ["sb_key1", "sb_key2", "sb_invis", "sb_invuln", "sb_suit", "sb_quad"];

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

/// The selection-flash frame name for the *currently selected* weapon `i` (0..6),
/// keyed on the server `time` the orchestrator passes.
///
/// `Sbar_DrawInventory` cycles the active weapon through its 5 flash frames
/// `inva1_*..inva5_*` (`sb_weapons[2+f][i]`) right after selection. We do not
/// track per-item acquire times (only one `time` is supplied), so we run the same
/// 5-frame cycle continuously off `time`: `frame = (int)(time*10) % 5` in 0..4,
/// then the 1-based `inva{frame+1}_<suffix>` lump name. Non-selected owned weapons
/// use the dim `inv_*` name from [`WEAPON_INV_NAMES`] (handled by the caller).
// Retained for the future per-item `cl.item_gettime`-driven 1-second pickup flash;
// the steady-state HUD now draws the settled `inv2_*` icon for the active weapon.
#[allow(dead_code)]
fn weapon_flash_name(i: usize, time: f32) -> String {
    let suffix = WEAPON_SUFFIX.get(i).copied().unwrap_or("shotgun");
    let f = ((time * 10.0).floor() as i64).rem_euclid(5) + 1;
    format!("inva{f}_{suffix}")
}

/// Try to fetch a HUD pic by name and blit it at virtual `(vx, vy)`; a missing or
/// unparseable lump is silently skipped (`wad.qpic(name).ok()`), so the bar
/// degrades gracefully exactly as the task requires.
// Mirrors Sbar_DrawPic (sbar.c); the C reads vid/draw globals passed explicitly here.
#[allow(clippy::too_many_arguments)]
fn blit_named(
    image: &mut Image,
    wad: &crate::wad::Wad2,
    name: &str,
    vx: f32,
    vy: f32,
    scale: f32,
    vy_top: f32,
    palette: &[[u8; 3]; 256],
) {
    if name.is_empty() {
        return;
    }
    if let Ok(pic) = wad.qpic(name) {
        blit_qpic(image, &pic, vx, vy, scale, vy_top, palette);
    }
}

/// Stamp one console-font glyph (`conchars` cell `ch`) at virtual `(vx, vy)` in
/// 320x200 bar space, scaled/anchored exactly like [`blit_qpic`] — a port of
/// `Sbar_DrawCharacter`'s `Draw_Character`.
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
fn draw_sbar_char(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    ch: u8,
    vx: f32,
    vy: f32,
    scale: f32,
    vy_top: f32,
    palette: &[[u8; 3]; 256],
) {
    if conchars.width != 128 || conchars.height != 128 || conchars.data.len() < 128 * 128 {
        return;
    }
    let cell_x = (ch as usize % 16) * 8;
    let cell_y = (ch as usize / 16) * 8;
    // Destination top-left in framebuffer pixels and the 8x8 scaled extent.
    let dst_x0 = (vx * scale).floor() as i64;
    let dst_y0 = (vy_top + vy * scale).floor() as i64;
    let dst_w = (8.0 * scale).round().max(1.0) as i64;
    let dst_h = (8.0 * scale).round().max(1.0) as i64;
    let inv_scale = 1.0 / scale;
    for dy in 0..dst_h {
        let py = dst_y0 + dy;
        if py < 0 || py >= image.h as i64 {
            continue;
        }
        let sy = (dy as f32 * inv_scale) as usize;
        if sy >= 8 {
            continue;
        }
        for dx in 0..dst_w {
            let px = dst_x0 + dx;
            if px < 0 || px >= image.w as i64 {
                continue;
            }
            let sx = (dx as f32 * inv_scale) as usize;
            if sx >= 8 {
                continue;
            }
            let texel = match conchars.data.get((cell_y + sy) * 128 + (cell_x + sx)) {
                Some(&t) => t,
                None => continue,
            };
            // conchars uses palette index 0 as the transparent glyph background.
            if texel == 0 {
                continue;
            }
            image.put(px as i32, py as i32, palette[texel as usize]);
        }
    }
}

/// `Sbar_DrawInventory` (sbar.c): the `ibar` strip in the 24 virtual rows above
/// the status strip and, on it, the owned weapons, the four ammo counts, the
/// keys/powerups and the sigils. Called by [`draw_hud_into`] only while
/// `sb_lines > 24`. `scale` / `vy_top` are the bar's transform (see there).
fn draw_sbar_inventory(
    image: &mut Image,
    hud: &Hud,
    conchars: Option<&crate::wad::Qpic>,
    scale: f32,
    vy_top: f32,
) {
    let wad = hud.wad;
    let pal = hud.palette;
    // The `ibar` strip in the 24 rows above the sbar: Sbar_DrawPic(0, -24, sb_ibar).
    blit_named(image, wad, "ibar", 0.0, -24.0, scale, vy_top, pal);

    // Weapon icons: for each owned weapon (items bit IT_SHOTGUN<<i, i=0..6), draw
    // its icon at Sbar_DrawPic(i*24, -16, ...). The currently-selected weapon shows
    // the bright `inv2_*` icon, the rest the dim `inv_*` icon (Sbar_DrawInventory:
    // for `flashon >= 10` — i.e. >1s after pickup, the steady state — the active
    // weapon draws `sb_weapons[1][i]` = `inv2_*`). The 1-second post-pickup
    // `inva1..5` flash needs per-item `cl.item_gettime`, which we don't track, so we
    // render the settled bright icon the player sees the rest of the time.
    for i in 0..7 {
        let bit = IT_SHOTGUN << i;
        if hud.items & bit != 0 {
            let selected = hud.weapon == bit;
            if selected {
                let name = format!("inv2_{}", WEAPON_SUFFIX[i]);
                blit_named(image, wad, &name, (i as f32) * 24.0, -16.0, scale, vy_top, pal);
            } else {
                blit_named(image, wad, WEAPON_INV_NAMES[i], (i as f32) * 24.0, -16.0, scale, vy_top, pal);
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
                // Gold digit glyph 18 + (c - '0'); x = (6*i + 1 + j)*8 - 2, y = -24.
                let glyph = 18 + (c - b'0');
                let vx = ((6 * i + 1 + j) as f32) * 8.0 - 2.0;
                draw_sbar_char(image, cc, glyph, vx, -24.0, scale, vy_top, pal);
            }
        }
    }

    // Items: keys + powerups (sb_items[0..5]) for items bits 1<<(17+i), at
    // Sbar_DrawPic(192 + i*16, -16, ...). Then sigils (sb_sigil[0..3]) for items
    // bits 1<<(28+i) at Sbar_DrawPic(320-32 + i*8, -16, ...).
    for (i, name) in SB_ITEM_NAMES.iter().enumerate() {
        if hud.items & (1 << (17 + i)) != 0 {
            blit_named(image, wad, name, 192.0 + (i as f32) * 16.0, -16.0, scale, vy_top, pal);
        }
    }
    for (i, name) in SB_SIGIL_NAMES.iter().enumerate() {
        if hud.items & (1 << (28 + i)) != 0 {
            blit_named(image, wad, name, 320.0 - 32.0 + (i as f32) * 8.0, -16.0, scale, vy_top, pal);
        }
    }
}

/// Draw the Quake status bar (HUD) across the bottom of `image`, on top of the
/// finished 3-D frame — a faithful port of `sbar.c`'s `Sbar_Draw` (single-player /
/// non-deathmatch path).
///
/// The whole bar is laid out in Quake's fixed 320x200 virtual space and scaled by
/// `image.w / 320` (nearest-neighbour) so it spans the full framebuffer width,
/// bottom-anchored. The *status area* is 48 virtual rows tall: the `ibar`
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
    // Scale the 320-wide virtual layout to the real framebuffer width.
    let scale = image.w as f32 / HUD_VIRT_W;
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    // Framebuffer y of virtual row 0 of the bar (top of the `sbar` strip); the
    // 24-px sbar sits flush at the bottom, the ibar 24 rows above it (negative vy).
    let vy_top = image.h as f32 - HUD_BAR_H * scale;
    let wad = hud.wad;
    let pal = hud.palette;
    let conchars = conchars_pic(wad);

    // ----- Inventory bar (Sbar_DrawInventory) -------------------------------
    // Sbar_Draw: `if (sb_lines > 24) Sbar_DrawInventory ();` — viewsize 110
    // (sb_lines 24) keeps only the status strip, 120 (0) neither.
    if hud.sb_lines > 24 {
        draw_sbar_inventory(image, hud, conchars.as_ref(), scale, vy_top);
    }

    // ----- Status bar (the sbar block of Sbar_Draw) -------------------------
    // When the player is dead (health <= 0) or holding Tab, the C replaces the whole
    // status strip with the dark `scorebar` pic + the solo scoreboard
    // (Monsters/Secrets/Time/level), keeping the ibar above. (sbar.c:948-953.)
    if hud.health <= 0 || hud.show_scores {
        blit_named(image, wad, "scorebar", 0.0, 0.0, scale, vy_top, pal);
        if let Some(cc) = &conchars {
            draw_solo_scoreboard(image, cc, hud, scale, vy_top, pal);
        }
        return;
    }
    // `else if (sb_lines)`: no status strip at viewsize 120.
    if hud.sb_lines <= 0 {
        return;
    }

    // 1. Background strip (sbar, 320x24) at virtual (0,0).
    blit_named(image, wad, "sbar", 0.0, 0.0, scale, vy_top, pal);

    // Armour field (Sbar_Draw, sbar.c:968-997). Under invulnerability the C draws a
    // gold "666" and the Pentagram-of-Protection disc over the armour slot and shows
    // NO real armour icon/number; otherwise the armour-type icon (Sbar_DrawPic(0, 0,
    // sb_armor[type])) keyed on IT_ARMOR3/2/1 plus the armour number at
    // Sbar_DrawNum(24, ..) — right edge virtual x=96, gold when <=25.
    if hud.items & IT_INVULNERABILITY != 0 {
        draw_num(image, 666, 96.0, 0.0, scale, vy_top, wad, pal, true);
        blit_named(image, wad, "disc", 0.0, 0.0, scale, vy_top, pal);
    } else {
        if hud.items & IT_ARMOR3 != 0 {
            blit_named(image, wad, ARMOR_ICON_NAMES[2], 0.0, 0.0, scale, vy_top, pal);
        } else if hud.items & IT_ARMOR2 != 0 {
            blit_named(image, wad, ARMOR_ICON_NAMES[1], 0.0, 0.0, scale, vy_top, pal);
        } else if hud.items & IT_ARMOR1 != 0 {
            blit_named(image, wad, ARMOR_ICON_NAMES[0], 0.0, 0.0, scale, vy_top, pal);
        }
        draw_num(image, hud.armor, 96.0, 0.0, scale, vy_top, wad, pal, hud.armor <= 25);
    }

    // Face (Sbar_DrawFace) at x=112, y=0. Powerup faces take priority in the C's
    // order: invisibility+invulnerability, then quad, then invisibility, then
    // invulnerability; otherwise the health-bracket face, `sb_faces[f][anim]`
    // with anim 1 (the pain face) while `cl.time <= cl.faceanimtime`.
    let inv_iv = IT_INVISIBILITY | IT_INVULNERABILITY;
    if hud.items & inv_iv == inv_iv {
        blit_named(image, wad, FACE_INVIS_INVULN, 112.0, 0.0, scale, vy_top, pal);
    } else if hud.items & IT_QUAD != 0 {
        blit_named(image, wad, FACE_QUAD, 112.0, 0.0, scale, vy_top, pal);
    } else if hud.items & IT_INVISIBILITY != 0 {
        blit_named(image, wad, FACE_INVIS, 112.0, 0.0, scale, vy_top, pal);
    } else if hud.items & IT_INVULNERABILITY != 0 {
        blit_named(image, wad, FACE_INVULN, 112.0, 0.0, scale, vy_top, pal);
    } else {
        let names = if hud.face_pain { &FACE_PAIN_NAMES } else { &FACE_NAMES };
        let face = names[face_bracket(hud.health)];
        blit_named(image, wad, face, 112.0, 0.0, scale, vy_top, pal);
    }

    // Health number: Sbar_DrawNum(136, health, 3, health<=25) — right edge x=208.
    draw_num(image, hud.health, 208.0, 0.0, scale, vy_top, wad, pal, hud.health <= 25);

    // Ammo-type icon (Sbar_DrawPic(224, 0, sb_ammo[type])) by the active weapon's
    // ammo type, keyed on the items ammo bits IT_SHELLS/NAILS/ROCKETS/CELLS.
    if hud.items & IT_SHELLS != 0 {
        blit_named(image, wad, AMMO_ICON_NAMES[0], 224.0, 0.0, scale, vy_top, pal);
    } else if hud.items & IT_NAILS != 0 {
        blit_named(image, wad, AMMO_ICON_NAMES[1], 224.0, 0.0, scale, vy_top, pal);
    } else if hud.items & IT_ROCKETS != 0 {
        blit_named(image, wad, AMMO_ICON_NAMES[2], 224.0, 0.0, scale, vy_top, pal);
    } else if hud.items & IT_CELLS != 0 {
        blit_named(image, wad, AMMO_ICON_NAMES[3], 224.0, 0.0, scale, vy_top, pal);
    }

    // Current ammo number: Sbar_DrawNum(248, ammo, 3, ammo<=10) — right edge x=320.
    draw_num(image, hud.ammo, 320.0, 0.0, scale, vy_top, wad, pal, hud.ammo <= 10);
}

/// `Sbar_SoloScoreboard` (sbar.c:457): the single-player stats drawn over the
/// `scorebar` strip on death / Tab — kills, secrets, elapsed time, and the level
/// name. Positions are verbatim from the C (virtual sbar-space, y in 0..24): the
/// "Monsters" / "Secrets" lines at x=8 (rows 4, 12), "Time" at x=184 row 4, and the
/// level name right-justified ending at virtual x≈232 on row 12 (`232 - len*4`).
fn draw_solo_scoreboard(
    image: &mut Image,
    conchars: &crate::wad::Qpic,
    hud: &Hud,
    scale: f32,
    vy_top: f32,
    pal: &[[u8; 3]; 256],
) {
    let draw = |image: &mut Image, vx: f32, vy: f32, s: &str| {
        for (i, &c) in s.as_bytes().iter().enumerate() {
            // Sbar_DrawString blits the raw ASCII glyph (space included, harmless).
            draw_sbar_char(image, conchars, c, vx + (i as f32) * 8.0, vy, scale, vy_top, pal);
        }
    };
    draw(
        image,
        8.0,
        4.0,
        &format!("Monsters:{:3} /{:3}", hud.monsters, hud.total_monsters),
    );
    draw(
        image,
        8.0,
        12.0,
        &format!("Secrets :{:3} /{:3}", hud.secrets, hud.total_secrets),
    );
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
    palette: &[[u8; 3]; 256],
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
            blit_qpic_at(image, &pic, x, vy, scale, ox, oy, palette);
        }
        x += 24.0; // the C steps a fixed 24 per digit slot
    }
}

/// `Sbar_IntermissionOverlay` (sbar.c): the single-player level-complete screen —
/// the `gfx/complete.lmp` banner at (64,24), the `gfx/inter.lmp` plaque at (0,56),
/// and the big-number time (minutes:seconds), secrets found/total and monsters
/// killed/total beside the plaque's labels. Drawn in the 320x200 virtual space,
/// uniformly scaled and centered like the menu (`min(w/320, h/200)`); on the
/// engine's 16:10 presets that equals the HUD's `w/320` with zero offset.
///
/// `complete`/`inter` are the two pak pics (`Draw_CachePic` in the C); either
/// being absent just skips that blit — the numbers still draw, never a panic.
/// The big digits and the colon/slash come from `gfx.wad` like the HUD's.
pub fn draw_intermission_overlay(
    image: &mut Image,
    wad: &crate::wad::Wad2,
    palette: &[[u8; 3]; 256],
    complete: Option<&crate::wad::Qpic>,
    inter: Option<&crate::wad::Qpic>,
    stats: &IntermissionStats,
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let sx = image.w as f32 / MENU_VIRT_W;
    let sy = image.h as f32 / MENU_VIRT_H;
    let scale = sx.min(sy);
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    let ox = (image.w as f32 - MENU_VIRT_W * scale) * 0.5;
    let oy = (image.h as f32 - MENU_VIRT_H * scale) * 0.5;

    // Draw_Pic(64, 24, "gfx/complete.lmp") — the "Level Complete" banner.
    if let Some(pic) = complete {
        blit_qpic_at(image, pic, 64.0, 24.0, scale, ox, oy, palette);
    }
    // Draw_TransPic(0, 56, "gfx/inter.lmp") — the Time/Secrets/Kills plaque.
    if let Some(pic) = inter {
        blit_qpic_at(image, pic, 0.0, 56.0, scale, ox, oy, palette);
    }

    // Time: minutes right-justified at (160,64) over 3 slots, then num_colon at
    // 234 and the two second digits at 246/266 (verbatim sbar.c coordinates).
    // DEVIATION: clamped at 0 — a negative time would make the C's direct
    // `sb_nums[0][num/10]` second-digit lookups index negatively (UB); the
    // signed stats rows below go through intermission_number's minus glyph.
    let t = stats.completed_time.max(0);
    let minutes = t / 60;
    let seconds = t - 60 * minutes;
    intermission_number(image, wad, palette, minutes, 160.0, 64.0, 3, scale, ox, oy);
    if let Ok(pic) = wad.qpic("num_colon") {
        blit_qpic_at(image, &pic, 234.0, 64.0, scale, ox, oy, palette);
    }
    if let Ok(pic) = wad.qpic(NUM_NAMES[(seconds / 10) as usize]) {
        blit_qpic_at(image, &pic, 246.0, 64.0, scale, ox, oy, palette);
    }
    if let Ok(pic) = wad.qpic(NUM_NAMES[(seconds % 10) as usize]) {
        blit_qpic_at(image, &pic, 266.0, 64.0, scale, ox, oy, palette);
    }

    // Secrets: found at (160,104), num_slash at 232, total at 240.
    intermission_number(image, wad, palette, stats.secrets, 160.0, 104.0, 3, scale, ox, oy);
    if let Ok(pic) = wad.qpic("num_slash") {
        blit_qpic_at(image, &pic, 232.0, 104.0, scale, ox, oy, palette);
    }
    intermission_number(image, wad, palette, stats.total_secrets, 240.0, 104.0, 3, scale, ox, oy);

    // Monsters: killed at (160,144), num_slash at 232, total at 240.
    intermission_number(image, wad, palette, stats.monsters, 160.0, 144.0, 3, scale, ox, oy);
    if let Ok(pic) = wad.qpic("num_slash") {
        blit_qpic_at(image, &pic, 232.0, 144.0, scale, ox, oy, palette);
    }
    intermission_number(image, wad, palette, stats.total_monsters, 240.0, 144.0, 3, scale, ox, oy);
}

/// `Sbar_FinaleOverlay` (sbar.c) + the finale half of `SCR_DrawCenterString`
/// (screen.c): the horizontally-centered `gfx/finale.lmp` plaque at y=16 and the
/// episode-end text revealed at `scr_printspeed` (8) characters per second of
/// `elapsed` (`cl.time - scr_centertime_start`). Pass `finale_pic = None` for
/// `svc_cutscene` (`cl.intermission == 3`), which draws the text alone.
pub fn draw_finale_overlay(
    image: &mut Image,
    conchars: Option<&crate::wad::Qpic>,
    palette: &[[u8; 3]; 256],
    finale_pic: Option<&crate::wad::Qpic>,
    text: &str,
    elapsed: f32,
) {
    if image.w == 0 || image.h == 0 {
        return;
    }
    let sx = image.w as f32 / MENU_VIRT_W;
    let sy = image.h as f32 / MENU_VIRT_H;
    let scale = sx.min(sy);
    if !scale.is_finite() || scale <= 0.0 {
        return;
    }
    let ox = (image.w as f32 - MENU_VIRT_W * scale) * 0.5;
    let oy = (image.h as f32 - MENU_VIRT_H * scale) * 0.5;

    // Draw_TransPic((vid.width - pic->width)/2, 16, "gfx/finale.lmp").
    if let Some(pic) = finale_pic {
        let vx = (MENU_VIRT_W - pic.width.max(0) as f32) * 0.5;
        blit_qpic_at(image, pic, vx, 16.0, scale, ox, oy, palette);
    }
    // scr_printspeed defaults to "8" (screen.c): 8 characters per second.
    if let Some(cc) = conchars {
        let remaining = (8.0 * elapsed.max(0.0)).min(9999.0) as i32;
        draw_center_string_revealed(image, cc, palette, text, remaining);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::fixtures::ramp_palette;
    use crate::screen::SB_LINES_FULL;
    use crate::wad::{Qpic, Wad2, CMP_NONE, LUMPINFO_SIZE, NAME_LEN, TYP_QPIC, WADINFO_SIZE};

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
        // (name, payload) pairs.
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
    fn blit_qpic_respects_transparency_and_clips() {
        let pal = ramp_palette();
        let mut img = Image::new(8, 8, [0, 0, 0]);

        // A 3x3 pic: corners opaque (index 5), centre transparent (255), with a
        // distinct opaque edge (index 9) we can detect after clipping.
        let mut data = vec![5u8; 9];
        data[3 + 1] = HUD_TRANSPARENT; // centre (row 1, col 1) transparent
        data[2 * 3 + 2] = 9; // bottom-right (row 2, col 2) opaque, distinct
        let pic = Qpic { width: 3, height: 3, data };

        // scale 1, no vertical offset (vy_top = 0), placed at virtual (0,0).
        blit_qpic(&mut img, &pic, 0.0, 0.0, 1.0, 0.0, &pal);

        // The centre texel was transparent: the background pixel is untouched.
        assert_eq!(img.rgb[8 + 1], [0, 0, 0], "index-255 texel left bg unchanged");
        // An opaque corner drew palette index 5 -> [5,5,5].
        assert_eq!(img.rgb[0], [5, 5, 5], "opaque corner blitted");
        // The distinct bottom-right opaque texel drew index 9.
        assert_eq!(img.rgb[2 * 8 + 2], [9, 9, 9], "distinct opaque texel blitted");

        // Clipping: blit the same pic so it overhangs the right/bottom edges. The
        // texels that fall off-screen must be silently dropped (no panic), and the
        // on-screen part must still draw.
        let mut img2 = Image::new(8, 8, [0, 0, 0]);
        // Place top-left at virtual (7,7): only the (0,0) texel is on-screen.
        blit_qpic(&mut img2, &pic, 7.0, 7.0, 1.0, 0.0, &pal);
        assert_eq!(img2.rgb[7 * 8 + 7], [5, 5, 5], "on-screen overhang texel drew");
        // Nothing wrapped to row 0 / col 0 from the off-screen part.
        let drawn = img2.rgb.iter().filter(|p| **p != [0, 0, 0]).count();
        assert_eq!(drawn, 1, "only the single on-screen overhang texel drew");
    }

    #[test]
    fn draw_num_right_justifies() {
        let wad = build_hud_wad();
        let pal = ramp_palette();

        // Right edge at virtual x=72 (3 * 24px digits), vy_top=0, scale=1.
        // num pics are 24x24. A 3-digit value (e.g. 100) fills [0,72); the digit
        // region (x in [0,72), y in [0,24)) must have changed.
        let mut img3 = Image::new(80, 24, [0, 0, 0]);
        draw_num(&mut img3, 100, 72.0, 0.0, 1.0, 0.0, &wad, &pal, false);
        let changed_3: usize = (0..24)
            .flat_map(|y| (0..72).map(move |x| (x, y)))
            .filter(|&(x, y)| img3.rgb[y * 80 + x] != [0, 0, 0])
            .count();
        assert!(changed_3 > 0, "3-digit value changed pixels in the digit region");

        // A 1-digit value at the same right edge must occupy only the rightmost
        // 24px slot [48,72) and leave the left two slots [0,48) untouched, proving
        // right-justification (the units digit lands at the same right edge).
        let mut img1 = Image::new(80, 24, [0, 0, 0]);
        draw_num(&mut img1, 7, 72.0, 0.0, 1.0, 0.0, &wad, &pal, false);
        // Right slot [48,72) changed.
        let right_changed: usize = (0..24)
            .flat_map(|y| (48..72).map(move |x| (x, y)))
            .filter(|&(x, y)| img1.rgb[y * 80 + x] != [0, 0, 0])
            .count();
        assert!(right_changed > 0, "1-digit value drew in the rightmost slot");
        // Left two slots [0,48) untouched.
        let left_changed: usize = (0..24)
            .flat_map(|y| (0..48).map(move |x| (x, y)))
            .filter(|&(x, y)| img1.rgb[y * 80 + x] != [0, 0, 0])
            .count();
        assert_eq!(left_changed, 0, "1-digit value left the left slots blank (right-justified)");

        // Alignment at the right edge: the units digit of "7" and the units digit
        // of "100" occupy the same column band [48,72). Both should have drawn
        // there (num_7 = index 107, num_0 = index 100 — both non-transparent).
        let units_7: usize = (0..24)
            .flat_map(|y| (48..72).map(move |x| (x, y)))
            .filter(|&(x, y)| img1.rgb[y * 80 + x] != [0, 0, 0])
            .count();
        let units_100: usize = (0..24)
            .flat_map(|y| (48..72).map(move |x| (x, y)))
            .filter(|&(x, y)| img3.rgb[y * 80 + x] != [0, 0, 0])
            .count();
        assert_eq!(units_7, units_100, "units digit of 1- and 3-digit values align at the right edge");
    }

    #[test]
    fn draw_num_right_justifies_at_scale_2() {
        // Regression for the virtual/pixel unit-mix bug: at scale != 1 the digit
        // must land at PIXEL (vx*scale), not pixel vx. Right edge virtual x=72,
        // scale=2 => the units digit must end at pixel 144 (its 24-virtual = 48-px
        // cell spans px [96,144)), and nothing draws at/after px 144.
        let wad = build_hud_wad();
        let pal = ramp_palette();
        let mut img = Image::new(200, 48, [0, 0, 0]);
        draw_num(&mut img, 7, 72.0, 0.0, 2.0, 0.0, &wad, &pal, false);

        // Pixels exist in the cell [96,144); none at or past 144.
        let in_cell = (0..48)
            .flat_map(|y| (96..144).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 200 + x] != [0, 0, 0])
            .count();
        assert!(in_cell > 0, "scale=2 digit drew in the px[96,144) cell ending at the scaled right edge");
        let past_edge = (0..48)
            .flat_map(|y| (144..200).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 200 + x] != [0, 0, 0])
            .count();
        assert_eq!(past_edge, 0, "nothing drew past the scaled right edge px=144");
        // And it must NOT be jammed against px=72 (the old bug placed it there).
        let at_virtual_edge = (0..48)
            .flat_map(|y| (48..96).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 200 + x] != [0, 0, 0])
            .count();
        assert_eq!(at_virtual_edge, 0, "digit must not sit at the unscaled px=72 edge (the old unit-mix bug)");
    }

    #[test]
    fn draw_hud_changes_bottom_strip_only() {
        let wad = build_hud_wad();
        let pal = ramp_palette();

        // A solid-filled image; the HUD must change the bottom 24-virtual-row bar
        // but leave the top of the frame untouched. Use a 320-wide frame so
        // scale == 1 and the bar is exactly the bottom 24 rows.
        let fill = [42u8, 42, 42];
        let mut img = Image::new(320, 200, fill);
        let hud = Hud {
            wad: &wad,
            palette: &pal,
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
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            sb_lines: SB_LINES_FULL,
            face_pain: false,
        };
        draw_hud_into(&mut img, &hud);

        // The top of the frame (well above the 24-px bar) is untouched.
        for y in 0..(200 - 24) {
            for x in 0..320 {
                assert_eq!(img.rgb[y * 320 + x], fill, "row {y} col {x} above the bar must be untouched");
            }
        }
        // The bottom strip changed (the sbar background, index 1 -> [1,1,1],
        // covers the whole 320x24 bar).
        let changed_bottom: usize = (200 - 24..200)
            .flat_map(|y| (0..320).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] != fill)
            .count();
        assert!(changed_bottom > 0, "the bottom strip changed under the HUD");

        // The digits (index >= 100) drew on top of the sbar background somewhere
        // in the bar — proving health/ammo/armour numbers actually rendered.
        let has_digit = (200 - 24..200)
            .flat_map(|y| (0..320).map(move |x| (x, y)))
            .any(|(x, y)| img.rgb[y * 320 + x][0] >= 100);
        assert!(has_digit, "at least one big-number digit drew over the bar");
    }

    #[test]
    fn draw_hud_scales_to_wide_frame() {
        // A 640-wide frame (scale 2): the bar must still bottom-anchor and span
        // the full width without panicking, leaving the top untouched.
        let wad = build_hud_wad();
        let pal = ramp_palette();
        let fill = [7u8, 7, 7];
        let mut img = Image::new(640, 400, fill);
        let hud = Hud {
            wad: &wad,
            palette: &pal,
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
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            sb_lines: SB_LINES_FULL,
            face_pain: false,
        };
        draw_hud_into(&mut img, &hud);

        // Bar height in pixels = 24 * (640/320) = 48; the top must be untouched.
        let bar_px = (24.0 * (640.0 / 320.0)) as usize; // 48
        for y in 0..(400 - bar_px) {
            assert_eq!(img.rgb[y * 640], fill, "row {y} above the scaled bar untouched");
        }
        // Bottom row changed across a wide span (the scaled sbar covers it).
        let bottom = 399 * 640;
        let bottom_changed = (0..640).filter(|&x| img.rgb[bottom + x] != fill).count();
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
        let pal = ramp_palette();
        let fill = [9u8, 9, 9];
        let mut img = Image::new(320, 200, fill);
        let hud = Hud {
            wad: &wad,
            palette: &pal,
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
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            sb_lines: SB_LINES_FULL,
            face_pain: false,
        };
        draw_hud_into(&mut img, &hud);
        assert!(img.rgb.iter().all(|&p| p == fill), "missing pics leave the frame unchanged");
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
    fn weapon_flash_name_cycles_five_frames() {
        // The selected-weapon flash cycles inva1..inva5 off (int)(time*10) % 5.
        assert_eq!(weapon_flash_name(0, 0.0), "inva1_shotgun");
        assert_eq!(weapon_flash_name(0, 0.1), "inva2_shotgun");
        assert_eq!(weapon_flash_name(0, 0.4), "inva5_shotgun");
        assert_eq!(weapon_flash_name(0, 0.5), "inva1_shotgun"); // wraps after 5
        // Per-weapon suffix is correct across the 7 weapons (shotgun..lightng).
        assert_eq!(weapon_flash_name(6, 0.0), "inva1_lightng");
        assert_eq!(weapon_flash_name(4, 0.0), "inva1_rlaunch");
    }

    #[test]
    fn intermission_overlay_draws_banner_plaque_and_numbers() {
        // Sbar_IntermissionOverlay on a 320x200 frame (scale 1, no offsets): the
        // banner at (64,24), the plaque at (0,56), and the verbatim sbar.c number
        // positions — minutes right-justified into 3 slots from x=160, colon at
        // 234, second digits at 246/266; secrets/monsters rows at y=104/144 with
        // num_slash at 232 and the totals from x=240.
        let pal = ramp_palette();
        let wad = build_hud_wad();
        let mut img = Image::new(320, 200, [0, 0, 0]);
        let complete = Qpic { width: 192, height: 24, data: vec![50u8; 192 * 24] };
        let inter = Qpic { width: 160, height: 144, data: vec![51u8; 160 * 144] };
        let stats = IntermissionStats {
            completed_time: 205, // 3:25
            secrets: 3,
            total_secrets: 7,
            monsters: 12,
            total_monsters: 45,
        };
        draw_intermission_overlay(&mut img, &wad, &pal, Some(&complete), Some(&inter), &stats);

        let px = |x: usize, y: usize| img.rgb[y * 320 + x];
        assert_eq!(px(64 + 5, 24 + 5), [50, 50, 50], "complete.lmp banner at (64,24)");
        assert_eq!(px(5, 56 + 100), [51, 51, 51], "inter.lmp plaque at (0,56)");
        // Time 3:25 — "3" right-justified: x = 160 + 2*24 = 208 (num_3 = idx 103).
        assert_eq!(px(208 + 2, 64 + 2), [103, 103, 103], "minutes digit 3 at x=208");
        assert_eq!(px(234 + 2, 64 + 2), [140, 140, 140], "num_colon at x=234");
        assert_eq!(px(246 + 2, 64 + 2), [102, 102, 102], "seconds tens 2 at x=246");
        assert_eq!(px(266 + 2, 64 + 2), [105, 105, 105], "seconds units 5 at x=266");
        // Secrets 3/7: found at x=208 (right-justified), slash 232, total at 288.
        assert_eq!(px(208 + 2, 104 + 2), [103, 103, 103], "secrets found 3");
        assert_eq!(px(232 + 2, 104 + 2), [141, 141, 141], "num_slash at x=232");
        assert_eq!(px(240 + 2 * 24 + 2, 104 + 2), [107, 107, 107], "secrets total 7");
        // Monsters 12/45: two digits start at x = 160 + 24 = 184.
        assert_eq!(px(184 + 2, 144 + 2), [101, 101, 101], "monsters tens 1 at x=184");
        assert_eq!(px(208 + 2, 144 + 2), [102, 102, 102], "monsters units 2 at x=208");
        assert_eq!(px(264 + 2, 144 + 2), [104, 104, 104], "total tens 4 at x=264");
        assert_eq!(px(288 + 2, 144 + 2), [105, 105, 105], "total units 5 at x=288");
        // The (missing-pic) graceful path: no panic with both pics absent.
        let mut img2 = Image::new(320, 200, [0, 0, 0]);
        draw_intermission_overlay(&mut img2, &wad, &pal, None, None, &stats);
        assert_eq!(px(208 + 2, 64 + 2), [103, 103, 103], "numbers still draw without pics");
    }

    #[test]
    fn intermission_number_draws_leading_minus_glyph() {
        // Sbar_IntermissionNumber: Sbar_itoa keeps the sign, and a '-' draws
        // sb_nums[0][STAT_MINUS] — "num_minus" (index 142 in the test wad). -7
        // over 3 slots right-justifies like any 2-character number: the minus
        // lands at x = 160 + 24 = 184 and the digit at 208.
        let pal = ramp_palette();
        let wad = build_hud_wad();
        let mut img = Image::new(320, 200, [0, 0, 0]);
        intermission_number(&mut img, &wad, &pal, -7, 160.0, 64.0, 3, 1.0, 0.0, 0.0);
        let px = |x: usize, y: usize| img.rgb[y * 320 + x];
        assert_eq!(px(160 + 2, 64 + 2), [0, 0, 0], "first slot empty (2-char number)");
        assert_eq!(px(184 + 2, 64 + 2), [142, 142, 142], "minus glyph at x=184");
        assert_eq!(px(208 + 2, 64 + 2), [107, 107, 107], "digit 7 at x=208");
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
        for name in ["face5", "face4", "face3", "face2", "face1", "face_inv2", "face_quad", "face_invis", "face_invul2"] {
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
        let pal = ramp_palette();
        let fill = [42u8, 42, 42];
        let mut img = Image::new(320, 200, fill); // scale 1: bar = bottom 48 rows
        let hud = Hud {
            wad: &wad,
            palette: &pal,
            health: 100,   // -> face1 (bracket 4)
            ammo: 25,
            armor: 80,
            // shotgun(1) + nailgun(4) owned; armour3; shells ammo type;
            // key1 (bit 17) + quad (bit 22); sigil1 (bit 28).
            items: IT_SHOTGUN | (IT_SHOTGUN << 2) | IT_ARMOR3 | IT_SHELLS
                | (1 << 17) | (1 << 22) | (1 << 28),
            weapon: IT_SHOTGUN, // shotgun selected -> flashes inva*_shotgun
            ammo_shells: 100,
            ammo_nails: 0,
            ammo_rockets: 0,
            ammo_cells: 0,
            time: 0.0,
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            sb_lines: SB_LINES_FULL,
            face_pain: false,
        };
        draw_hud_into(&mut img, &hud);

        // The ibar (index 2) occupies virtual rows -24..0, i.e. framebuffer rows
        // 152..176 at scale 1. Its background colour [2,2,2] must appear there.
        let ibar_rows = 152..176;
        let ibar_bg = ibar_rows
            .clone()
            .flat_map(|y| (0..320).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] == [2, 2, 2])
            .count();
        assert!(ibar_bg > 0, "ibar background fills the 24 rows above the sbar");

        // Rows above the whole 48-row status area (y < 152) stay the fill colour.
        for y in 0..152 {
            for x in 0..320 {
                assert_eq!(img.rgb[y * 320 + x], fill, "row {y} col {x} above the status area untouched");
            }
        }

        // A weapon icon (index 60) drew on the ibar (the active shotgun's bright
        // inv2_shotgun icon at x=0, y=-16 -> framebuffer rows ~160..176).
        let weapon_px = (160..176)
            .flat_map(|y| (0..24).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] == [60, 60, 60])
            .count();
        assert!(weapon_px > 0, "active weapon (inv2_*) icon drew on the ibar");

        // The face (index 70) drew at x=112 on the sbar (rows 176..200).
        let face_px = (176..200)
            .flat_map(|y| (112..136).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] == [70, 70, 70])
            .count();
        assert!(face_px > 0, "player face drew at x=112 on the sbar");

        // The armour-type icon (index 80) drew at x=0 on the sbar.
        let armor_icon = (176..200)
            .flat_map(|y| (0..24).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] == [80, 80, 80])
            .count();
        assert!(armor_icon > 0, "armour-type icon drew at x=0");

        // The ammo-type icon (index 85) drew at x=224 on the sbar.
        let ammo_icon = (176..200)
            .flat_map(|y| (224..248).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] == [85, 85, 85])
            .count();
        assert!(ammo_icon > 0, "ammo-type icon drew at x=224");

        // The small ammo counts (gold conchars digits, index 95) drew on the ibar.
        let count_px = ibar_rows
            .flat_map(|y| (0..320).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] == [95, 95, 95])
            .count();
        assert!(count_px > 0, "small ammo counts drew on the ibar");

        // A sigil (index 90) drew near the right edge of the ibar (x≈288).
        let sigil_px = (160..176)
            .flat_map(|y| (288..296).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] == [90, 90, 90])
            .count();
        assert!(sigil_px > 0, "sigil drew near the right edge of the ibar");
    }

    #[test]
    fn draw_hud_powerup_face_overrides_health_face() {
        // With quad active, Sbar_DrawFace draws the quad face regardless of health.
        let wad = build_full_hud_wad();
        let pal = ramp_palette();
        let mut img = Image::new(320, 200, [0u8, 0, 0]);
        let hud = Hud {
            wad: &wad,
            palette: &pal,
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
            monsters: 0,
            total_monsters: 0,
            secrets: 0,
            total_secrets: 0,
            level_name: "",
            show_scores: false,
            sb_lines: SB_LINES_FULL,
            face_pain: false,
        };
        // All face pics share index 70 here, so we can't distinguish quad vs health
        // by colour — instead assert the call path doesn't panic and a face drew.
        draw_hud_into(&mut img, &hud);
        let face_px = (176..200)
            .flat_map(|y| (112..136).map(move |x| (x, y)))
            .filter(|&(x, y)| img.rgb[y * 320 + x] == [70, 70, 70])
            .count();
        assert!(face_px > 0, "a powerup (quad) face drew at x=112");
    }

    #[test]
    fn draw_hud_pain_face_while_face_anim_runs() {
        // Sbar_DrawFace: `sb_faces[f][cl.time <= cl.faceanimtime]` — the pain
        // face of the health bracket right after a hit.
        let wad = build_full_hud_wad();
        let pal = ramp_palette();
        let face = |face_pain: bool| {
            let mut img = Image::new(320, 200, [0u8, 0, 0]);
            let hud = Hud {
                wad: &wad,
                palette: &pal,
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
                monsters: 0,
                total_monsters: 0,
                secrets: 0,
                total_secrets: 0,
                level_name: "",
                show_scores: false,
                sb_lines: SB_LINES_FULL,
                face_pain,
            };
            draw_hud_into(&mut img, &hud);
            img.rgb[188 * 320 + 124]
        };
        assert_eq!(face(false), [70, 70, 70], "the steady face");
        assert_eq!(face(true), [71, 71, 71], "the pain face");
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
        let pal = ramp_palette();
        let fill = [42u8, 42, 42];
        let draw = |sb_lines: i32, health: i32| {
            let mut img = Image::new(320, 200, fill);
            let hud = Hud {
                wad: &wad,
                palette: &pal,
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
                monsters: 0,
                total_monsters: 0,
                secrets: 0,
                total_secrets: 0,
                level_name: "",
                show_scores: false,
                face_pain: false,
                sb_lines,
            };
            draw_hud_into(&mut img, &hud);
            img
        };
        let ibar_row = 160 * 320 + 5; // inside rows 152..176
        let sbar_row = 190 * 320 + 5; // inside rows 176..200
        // 48 lines (viewsize <= 100): inventory strip over the status strip.
        let img = draw(48, 100);
        assert_eq!((img.rgb[ibar_row], img.rgb[sbar_row]), (pal[2], pal[1]));
        assert_eq!(img.rgb[151 * 320 + 5], fill, "nothing above the 48 lines");
        // 24 lines (viewsize 110): the status strip alone.
        let img = draw(24, 100);
        assert_eq!((img.rgb[ibar_row], img.rgb[sbar_row]), (fill, pal[1]));
        // 0 lines (viewsize 120): no status bar at all.
        let img = draw(0, 100);
        assert!(img.rgb.iter().all(|&p| p == fill), "sb_lines 0 draws nothing");
        // ...except the death scoreboard, which Sbar_Draw shows regardless.
        let img = draw(0, 0);
        assert_eq!((img.rgb[ibar_row], img.rgb[sbar_row]), (fill, pal[3]));
        let img = draw(48, 0);
        assert_eq!((img.rgb[ibar_row], img.rgb[sbar_row]), (pal[2], pal[3]));
    }
}

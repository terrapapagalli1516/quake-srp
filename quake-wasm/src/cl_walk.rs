//! The live client frame for the page — [`step_walk`] runs the client's
//! [`walk_frame`] (quake-rs `client::cl_main`: `CL_SendMove` into the server
//! tick, the client side of `CL_ParseServerMessage`, `CL_RelinkEntities`,
//! `V_CalcRefdef` and `SCR_UpdateScreen`'s 3-D view, blends and status bar) on
//! the page's [`Vid`](crate::vid::vid) and hands its sound calls to
//! [`snd_dma`](crate::snd_dma). Its tests drive the real client end to end on
//! the embedded shareware data.

use quake_rs::client::cl_main::{walk_frame, walk_frame_undrawn};
use quake_rs::client::Vid;
use quake_rs::render;

use crate::app::Walk;

/// One frame of the live walk at `render_w x render_h`: the finished screen
/// and its colour shifts (`cl.cshifts`, applied by the host after the menu and
/// console); the frame's sound calls are carried out.
pub(crate) fn step_walk(w: &mut Walk, dt: f64, menu_up: bool, vid: &Vid, draw: bool) -> (render::Image, Vec<([u8; 3], f32)>) {
    let frame = if draw { walk_frame(w, dt, menu_up, vid) } else { walk_frame_undrawn(w, dt, menu_up, vid) };
    crate::snd_dma::play(&w.pak, frame.sound);
    (frame.image, frame.cshifts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quake_rs::client::cl_main::{
        client_items, client_punchangle, offset_box, static_is_visible,
        ALIAS_MODEL_HALF,
    };
    use quake_rs::client::host_cmd::{try_changelevel, try_restart};
    use quake_rs::server::EntFlags;
    use quake_rs::server::wire_angle;
    use quake_rs::client::SoundCall;
    use quake_rs::tent::BeamModel;

    use crate::app::{boot, boot_attract, build_walk, build_walk_map, APP};
    use crate::console::{console_toggle, console_visible};
    use crate::host::step;
    use crate::menu::menu_select;
    use crate::test_util::*;
    use crate::vid::set_resolution;

    /// The console's scrollback, oldest first, without its blank lines.
    fn console_lines() -> Vec<String> {
        APP.with(|c| {
            let b = c.borrow();
            b.as_ref().unwrap().console.lines().filter(|l| !l.is_empty()).map(str::to_string).collect()
        })
    }

    /// Whether the App still has a walk, whether it is disconnected, and
    /// `cls.demonum`.
    fn game_state() -> (bool, bool, i32) {
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            (a.walk.is_some(), a.disconnected, a.cls.demonum)
        })
    }

    /// Host_Error from a QuakeC runtime error in the live game (the frame's
    /// Result was dropped, and the game went on): here `makevectors` (#1),
    /// which PlayerPreThink calls every frame, made to fail. As in id's, the
    /// report (the statement, the stack trace, the message) and
    /// `Host_Error: Program error` reach the console, the walk is gone
    /// (CL_Disconnect), and no demo loop starts (cls.demonum = -1).
    #[test]
    fn a_quakec_error_in_a_frame_ends_the_game_like_host_error() {
        set_resolution(320, 200);
        assert_eq!(boot(), 1);
        close_menu();
        walk_mut(|w| w.server.vm.set_builtin(1, |vm| Err(vm.run_error("test fault"))));
        step(0.05);
        assert_eq!(game_state(), (false, true, -1), "CL_Disconnect, cls.demonum = -1");
        let lines = console_lines();
        assert!(lines.iter().any(|l| l.ends_with(" : PlayerPreThink")), "the stack trace: {lines:?}");
        assert_eq!(lines[lines.len() - 2..], ["test fault", "Host_Error: Program error"]);
    }

    /// The VM's output log (every print the QuakeC made, dprint's included)
    /// is drained every frame, so it does not grow for a level's life.
    #[test]
    fn the_vm_output_log_is_drained_every_frame() {
        set_resolution(320, 200);
        assert_eq!(boot(), 1);
        close_menu();
        walk_mut(|w| w.server.vm.print("a line nobody reads\n"));
        step(0.05);
        assert_eq!(walk_mut(|w| w.server.vm.output().len()), 0);
    }

    /// The same from the console: `kill` runs ClientKill, whose first
    /// `bprint` (#23) is made to fail.
    #[test]
    fn a_quakec_error_in_kill_ends_the_game_like_host_error() {
        set_resolution(320, 200);
        assert_eq!(boot(), 1);
        close_menu();
        walk_mut(|w| w.server.vm.set_builtin(23, |vm| Err(vm.run_error("test fault"))));
        console_toggle();
        run_console_line("kill");
        assert_eq!(game_state(), (false, true, -1), "CL_Disconnect, cls.demonum = -1");
        let lines = console_lines();
        assert!(lines.iter().any(|l| l.ends_with(" : ClientKill")), "the stack trace: {lines:?}");
        assert_eq!(lines[lines.len() - 2..], ["test fault", "Host_Error: Program error"]);
    }

    /// The mission packs' re-release end-of-game credits roll
    /// (`Walk::pending_menu_credits`, set by `walk_frame` once builtin #79
    /// `finaleFinished` is finally true and `finale_transition` ran
    /// `localcmd("menu_credits\n")` -- AUDIT.md "The mission packs' paths",
    /// P7/B4): the App-level reaction (`finish_menu_credits`, `cl_demo.rs`)
    /// ends the session exactly as the Quit menu's "Y" does
    /// (`App::request_quit`) -- `CL_Disconnect`, the same end screen. The
    /// embedded shareware id1 this test boots never sets the flag on its
    /// own (its progs never declares the builtin); set it directly, standing
    /// in for the mission packs' `finale_transition` this fixture does not
    /// have, to prove the App-level wiring without needing the real pack
    /// data -- `pr_cmds.rs`'s `hipend_ending_...`/`r2m8_ending_...` already
    /// prove the QuakeC side of the chain against it.
    #[test]
    fn menu_credits_ends_the_session_like_the_quit_menu() {
        set_resolution(320, 200);
        assert_eq!(boot(), 1);
        close_menu();
        walk_mut(|w| w.pending_menu_credits = true);
        step(0.05);
        let (has_walk, disconnected, _) = game_state();
        assert!(!has_walk && disconnected, "CL_Disconnect ran");
        assert!(crate::app::take_quit().is_some(), "request_quit latched, as the Quit menu's Y does");
    }

    /// Regression for the one-time texture/lighting "pops" in the first second of
    /// live play (two distinct root causes, both whole-view shimmers):
    ///
    /// 1. **Spawn settle on screen.** QuakeC `PutClientInServer` places the player
    ///    at `spot.origin + '0 0 1'` (the start map's spawn floats ~5 units up),
    ///    and the port used to render frame 0 with ZERO physics frames after the
    ///    spawn — the player fell to the floor ON SCREEN over the first 2-3 frames
    ///    and every textured surface resampled (17.8-45.9% of pixels/frame).
    ///    WinQuake runs two SV_Physics ticks during the signon (the Host_Spawn_f
    ///    and Host_Begin_f frames) before SCR_EndLoadingPlaque re-enables drawing;
    ///    `Server::run_signon_frames` ports those, and every walk-building path
    ///    (boot / New Game / changelevel / restart) must call it.
    /// 2. **Plane-only dlight gating.** ~0.55s in (sv.time ~1.98 with the current
    ///    deterministic PRNG), the start map's distant `misc_fireball` lavaball
    ///    spawns with a rocket-trail dynamic light ~2000 units away behind walls;
    ///    `any_dlight_reaches`' old plane-distance-only test marked every
    ///    near-coplanar face in the VIEW as dynamically lit, kicking them off the
    ///    baked surface cache onto the per-pixel path (13.3% of pixels shifted in
    ///    one frame, then back when the light died). The gate now also tests the
    ///    face's texture-space extent (WinQuake's R_MarkLights is spatially
    ///    bounded by the BSP recursion).
    ///
    /// The 45-frame window covers both: settle would hit frames 0-2, the fireball
    /// ~frame 34.
    #[test]
    fn new_game_first_frames_render_a_settled_player_no_pop() {
        // The browser path: the attract demo, a key for the menu, then Single
        // Player > New Game.
        assert_eq!(boot_attract(), 1);
        step(1.0 / 60.0); // an attract-demo frame, like the live page
        crate::input::press(b' '); // any key during the demo: the menu
        menu_select(); // Main: Single Player
        menu_select(); // SP: New Game -> builds the start-map walk, closes menu

        // BEFORE the first frame renders the player must already be settled:
        // on the ground, no residual fall velocity (it spawns ~5 units up).
        let eye0 = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            let w = a.walk.as_ref().unwrap();
            let vel = w.server.vm.ent_get_vector(w.player, "velocity");
            assert!(w.server.vm.flags(w.player).contains(EntFlags::ONGROUND), "player on the ground at frame 0");
            assert_eq!(vel[2], 0.0, "no residual fall velocity at frame 0");
            w.server.player_view().0
        });

        // Frame 0, then 44 more static zero-input frames: the eye must stay
        // bit-identical (the settle pop was exactly this eye motion leaking into
        // the first rendered frames)...
        step(1.0 / 60.0);
        let mut prev: Vec<u8> = APP.with(|c| c.borrow().as_ref().unwrap().present.rgba());
        let (w, h) = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            (a.render_w, a.render_h)
        });
        for i in 1..45 {
            step(1.0 / 60.0);
            let eye = APP.with(|c| {
                let b = c.borrow();
                let a = b.as_ref().unwrap();
                a.walk.as_ref().unwrap().server.player_view().0
            });
            assert_eq!(eye, eye0, "static eye is bit-identical on frame {i}");
            // ...and consecutive frames stay near-identical. Faithful animation
            // in the static spawn view (scrolling sky, flame group-frames, the
            // 10 Hz lightstyle flicker) touches <= ~0.4% of pixels per frame;
            // the settle pop touched 17.8%-45.9% and the fireball-dlight pop
            // 13.3%. A 10 Hz light-style tick re-lights whole walls: with the
            // 2-D layer 1:1 (a 48-row bar, so a 960x552 view at 960x600) and
            // per-texel relighting that tick touches ~2.4%. A 4% ceiling still
            // separates bug from animation with a wide margin both ways.
            let fb: Vec<u8> = APP.with(|c| c.borrow().as_ref().unwrap().present.rgba());
            let nd = prev
                .chunks_exact(4)
                .zip(fb.chunks_exact(4))
                .filter(|(a4, b4)| a4[..3] != b4[..3])
                .count();
            assert!(
                nd <= w * h / 25,
                "frame {i} vs {}: {nd} px differ ({:.2}%) — a one-time view shift leaked into the first frames",
                i - 1,
                100.0 * nd as f64 / (w * h) as f64
            );
            prev = fb;
        }
    }

    #[test]
    fn thunderbolt_beam_renders_bolt_pixels_on_e1m1() {
        // End-to-end through the LIVE path: boot the e1m1 walk, cheat in the
        // thunderbolt (impulse 9 = all weapons + ammo, impulse 8 = lightning
        // gun), hold fire, and verify (a) the QuakeC's TE_LIGHTNING2 broadcast
        // landed in the beam store, (b) bolt2.mdl was loaded on demand at parse
        // time (Mod_ForName in CL_ParseTEnt), (c) the per-frame CL_UpdateTEnts
        // expansion produced pieces anchored at the muzzle (origin + '0 0 16',
        // W_FireLightning), and (d) the bolt actually changes rendered pixels —
        // an identical re-render (same rng, dt=0) with the beams cleared differs.
        let mut w = build_walk().expect("e1m1 walk boots from the embedded pak");
        // Let the spawn settle (telefrag effects, initial thinks).
        for _ in 0..10 {
            let _ = step_walk(&mut w, 0.05, false, &crate::vid::mode_vid(320, 200), true);
        }
        w.next_impulse = 9; // CheatCommand: all weapons + full cells
        let _ = step_walk(&mut w, 0.05, false, &crate::vid::mode_vid(320, 200), true);
        w.next_impulse = 8; // select the thunderbolt
        let _ = step_walk(&mut w, 0.05, false, &crate::vid::mode_vid(320, 200), true);
        // Hold fire across several frames (W_FireLightning re-broadcasts the
        // beam each weapon frame, exercising the same-entity slot REPLACEMENT).
        w.in_attack = true;
        for _ in 0..6 {
            let _ = step_walk(&mut w, 0.05, false, &crate::vid::mode_vid(320, 200), true);
        }
        assert!(
            w.beams.any_live(w.clock),
            "firing the thunderbolt put a live beam in the store"
        );
        assert!(
            matches!(w.model_cache.get("progs/bolt2.mdl"), Some(Some(_))),
            "TE_LIGHTNING2 loaded progs/bolt2.mdl on demand"
        );
        // The last frame's expansion is retained in the scratch buffer: the
        // thunderbolt is ONE beam (slot replacement, never stacked). The QuakeC
        // fires from origin + '0 0 16' with a 600-unit traceline, but
        // CL_UpdateTEnts re-anchors the VIEW entity's beam to the player's raw
        // ORIGIN every frame (the C quirk — the visible bolt hangs 16 units
        // below the muzzle), so the segment can run a hair over 600 units:
        // 1..=21 Bolt2 pieces.
        assert!(
            !w.beam_scratch.is_empty() && w.beam_scratch.len() <= 21,
            "one ~600-unit beam expands to 1..=21 pieces (got {})",
            w.beam_scratch.len()
        );
        assert!(
            w.beam_scratch.iter().all(|s| s.model == BeamModel::Bolt2),
            "thunderbolt pieces use bolt2.mdl"
        );
        // The first piece sits at the player ORIGIN: the WriteEntity short
        // carried the player edict number through the decoder (an int global —
        // a float read would have yielded ~0 and never matched w.player), and
        // the view-entity re-anchor replaced the broadcast start (origin+16).
        let player_origin = w.server.vm.ent_get_vector(w.player, "origin");
        let first = w.beam_scratch[0].origin;
        for i in 0..3 {
            assert!(
                (first[i] - player_origin[i]).abs() < 1.0,
                "first piece re-anchored to the player origin (axis {i}: {} vs {})",
                first[i],
                player_origin[i]
            );
        }

        // Pixel evidence: render the SAME state twice (dt = 0 -> no time passes,
        // restored rng -> identical dlight jitter draws), once with the live
        // beam and once with the store cleared. The ONLY difference is the bolt
        // model pieces, so differing pixels prove the bolt drew into the scene.
        let rng = w.prng;
        let (with_bolt, _) = step_walk(&mut w, 0.0, false, &crate::vid::mode_vid(320, 200), true);
        w.prng = rng;
        w.beams.clear();
        let (without_bolt, _) = step_walk(&mut w, 0.0, false, &crate::vid::mode_vid(320, 200), true);
        let diff = with_bolt
            .pixels
            .iter()
            .zip(without_bolt.pixels.iter())
            .filter(|(a, b)| a != b)
            .count();
        assert!(
            diff > 0,
            "the rendered thunderbolt changes pixels vs the beam-less frame"
        );
        // Optional visual evidence: QUAKE_DUMP_BEAM=/some/dir dumps the two
        // frames as PPMs for eyeballing (never set in CI; the asserts above are
        // the real check).
        if let Ok(dir) = std::env::var("QUAKE_DUMP_BEAM") {
            for (img, name) in [(&with_bolt, "with-bolt"), (&without_bolt, "without-bolt")] {
                // The palette indices as greys (a PGM).
                let mut buf = format!("P5\n{} {}\n255\n", img.w, img.h).into_bytes();
                buf.extend_from_slice(&img.pixels);
                let _ = std::fs::write(format!("{dir}/beam-{name}.pgm"), buf);
            }
        }
    }

    // -------------------------------------------------------------------
    // Intermission + finale (end-to-end against the real progs.dat)
    // -------------------------------------------------------------------

    /// The centre of the live map's `trigger_changelevel` brush volume (from the
    /// absmin/absmax its setmodel+link produced), to pin the player onto.
    fn changelevel_trigger_center() -> [f32; 3] {
        walk_mut(|w| {
            for e in 0..w.server.vm.num_edicts() {
                let ent = e as i32;
                if w.server.vm.is_free_edict(e as i32) {
                    continue;
                }
                if w.server.vm.ent_get_string(ent, "classname") == "trigger_changelevel" {
                    let amin = w.server.vm.ent_get_vector(ent, "absmin");
                    let amax = w.server.vm.ent_get_vector(ent, "absmax");
                    return [
                        0.5 * (amin[0] + amax[0]),
                        0.5 * (amin[1] + amax[1]),
                        0.5 * (amin[2] + amax[2]),
                    ];
                }
            }
            panic!("no trigger_changelevel in the live map");
        })
    }

    /// Pin the player onto the exit trigger and step until the QuakeC's
    /// `execute_changelevel` think fires `svc_intermission` (touch at frame N,
    /// the scheduled think 0.1s later). Panics if it never arrives.
    fn drive_into_exit() {
        let centre = changelevel_trigger_center();
        for _ in 0..40 {
            walk_mut(|w| {
                let p = w.player;
                w.server.vm.ent_set_vector(p, "origin", centre);
                w.server.vm.ent_set_vector(p, "velocity", [0.0, 0.0, 0.0]);
            });
            step(0.1);
            if walk_mut(|w| w.intermission) != 0 {
                return;
            }
        }
        panic!("svc_intermission never arrived after 40 frames on the exit trigger");
    }

    /// Write the current RGBA framebuffer as a binary PPM into `$QUAKE_DUMP_DIR`
    /// (the feature-evidence dumps); a no-op when the variable is unset.
    fn dump_frame(name: &str) {
        let Ok(dir) = std::env::var("QUAKE_DUMP_DIR") else { return };
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            let (w, h) = (a.render_w, a.render_h);
            let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
            for px in a.present.rgba().chunks(4).take(w * h) {
                out.extend_from_slice(&px[..3]);
            }
            let _ = std::fs::write(format!("{dir}/{name}.ppm"), out);
        });
    }

    /// Run QC `T_Damage(targ, inflictor, attacker, damage)` on the live server.
    fn qc_damage(w: &mut Walk, targ: i32, inflictor: i32, damage: f32) {
        use quake_rs::progs::OFS_PARM0;
        let f = w.server.vm.progs().find_function("T_Damage").expect("progs has T_Damage");
        let vm = &mut w.server.vm;
        vm.set_gi(OFS_PARM0, targ);
        vm.set_gi(OFS_PARM0 + 3, inflictor);
        vm.set_gi(OFS_PARM0 + 6, inflictor);
        vm.set_gf(OFS_PARM0 + 9, damage);
        vm.execute(f).expect("T_Damage runs");
    }

    /// CENSUS F16: QC T_Damage adds to `dmg_take`/`dmg_save` before its god-mode
    /// return, and SV_WriteClientdataToMessage sends svc_damage whenever they
    /// are non-zero — so a god-mode (or Pentagram) hit still flashes red, kicks
    /// the view and shows the pain face; the fields are zeroed once sent.
    #[test]
    fn damage_in_god_mode_still_flashes_and_kicks() {
        let mut w = build_walk().expect("e1m1 boots");
        for _ in 0..3 {
            step_walk(&mut w, 0.05, false, &crate::vid::mode_vid(320, 200), true);
        }
        let p = w.player;
        let flags = w.server.vm.ent_get_float(p, "flags") as i32;
        w.server.vm.ent_set_float(p, "flags", (flags | 64) as f32); // FL_GODMODE
        // The inflictor: a spot 100 units straight ahead (yaw 0 -> +x).
        w.yaw = 0.0;
        w.pitch = 0.0;
        let src = w.server.vm.spawn();
        let o = w.server.vm.ent_get_vector(p, "origin");
        w.server.vm.ent_set_vector(src, "origin", [o[0] + 100.0, o[1], o[2]]);
        qc_damage(&mut w, p, src, 20.0);
        assert_eq!(w.server.vm.ent_get_float(p, "health"), 100.0, "god mode: no health lost");
        assert_eq!(w.server.vm.ent_get_float(p, "dmg_take"), 20.0, "T_Damage counted the hit");
        let (_, cshifts) = step_walk(&mut w, 0.05, false, &crate::vid::mode_vid(320, 200), true);
        assert_eq!(w.server.vm.ent_get_float(p, "dmg_take"), 0.0, "sent and zeroed");
        assert!(
            cshifts.iter().any(|&(c, pct)| c == [255, 0, 0] && pct > 0.0),
            "a red damage cshift: {cshifts:?}"
        );
        // count = max(20*0.5, 10) = 10: percent 30, then one 0.05 s drop of 7.5,
        // truncated like the C's int percent.
        assert_eq!(w.damage_blend, 22.0);
        assert!(w.v_dmg_pitch > 5.0, "hit from the front pitches the view: {}", w.v_dmg_pitch);
        assert!(w.v_dmg_time > 0.0 && w.v_dmg_time < quake_rs::client::view::V_KICKTIME, "kick running");
        assert!(w.server.time() <= w.faceanimtime, "the pain face shows");
    }

    /// CENSUS L9: an explosion's dlight is drawn at its full 350 radius on the
    /// frame CL_ParseTEnt allocated it (CL_DecayLights runs after
    /// SCR_UpdateScreen), a light past its `die` is not drawn, and the frame
    /// still decays the pool once.
    #[test]
    fn dlights_are_drawn_before_they_decay() {
        let mut w = build_walk().expect("e1m1 boots");
        step_walk(&mut w, 0.05, false, &crate::vid::mode_vid(320, 200), true);
        let now = w.clock;
        w.dlights.alloc(0, [0.0; 3], 350.0, now + 0.5, 300.0, 0.0, f64::from(now));
        w.dlights.alloc(0, [64.0, 0.0, 0.0], 200.0, now - 0.01, 0.0, 0.0, f64::from(now));
        let drawn = w.dlights.active(f64::from(now));
        assert_eq!(drawn.len(), 1, "the dead light is not pushed");
        assert_eq!(drawn[0].radius, 350.0, "full radius on its first frame");
        let before = w.dlights.active(f64::from(w.clock)).iter().map(|d| d.radius).fold(0.0, f32::max);
        step_walk(&mut w, 0.05, false, &crate::vid::mode_vid(320, 200), true);
        let after = w.dlights.active(f64::from(w.clock)).iter().map(|d| d.radius).fold(0.0, f32::max);
        assert!((before - after - 0.05 * 300.0).abs() < 1e-3, "{before} -> {after}");
    }

    /// CENSUS L1: the client's punchangle is MSG_WriteChar'd — truncated to
    /// whole degrees — so the shotgun kick steps -2, -1, 0.
    #[test]
    fn punchangle_reaches_the_view_in_whole_degrees() {
        let mut w = build_walk().expect("e1m1 boots");
        let p = w.player;
        w.server.vm.ent_set_vector(p, "punchangle", [-1.9, 0.5, -2.0]);
        assert_eq!(client_punchangle(&w), [-1.0, 0.0, -2.0]);
        w.server.vm.ent_set_vector(p, "punchangle", [-4.0, 0.0, 0.0]);
        assert_eq!(client_punchangle(&w), [-4.0, 0.0, 0.0]);
    }

    /// CENSUS F18: CL_ParseClientdata stamps `cl.item_gettime` for every newly
    /// set items bit; the HUD then cycles the new weapon's `inva1..5` icons
    /// for a second. What a level starts with never flashes: CL_ClearState
    /// zeroes cl.items AND cl.time, and the signon's clientdata is parsed
    /// before CL_LerpPoint first snaps cl.time to the server's (cl_main.c
    /// CL_ReadFromServer), so the stamps are ~host_frametime and cl.time is
    /// past 1.2 by the first drawn frame. New game, restart, changelevel and
    /// load all start with the carried items seeded and every get-time 0.
    #[test]
    fn only_items_got_in_play_flash_not_what_a_level_starts_with() {
        let unflashed =
            |w: &Walk| w.cl_items == client_items(w) && w.item_gettime.iter().all(|&t| t == 0.0);
        let mut w = build_walk().expect("e1m1 boots");
        assert_ne!(client_items(&w) & 1, 0, "the player spawns with the shotgun (bit 0)");
        assert!(unflashed(&w), "new game");
        step_walk(&mut w, 0.05, false, &crate::vid::mode_vid(320, 200), true);
        assert!(unflashed(&w), "the first frame stamps nothing");
        for _ in 0..30 {
            step_walk(&mut w, 0.05, false, &crate::vid::mode_vid(320, 200), true);
        }
        w.next_impulse = 9; // every weapon
        step_walk(&mut w, 0.05, false, &crate::vid::mode_vid(320, 200), true);
        let t1 = w.server.time();
        assert_eq!(w.item_gettime[4], t1, "the rocket launcher (bit 4) was just got");
        assert_eq!(w.item_gettime[0], 0.0, "the carried shotgun was never stamped");

        try_restart(&mut w, &mut Vec::new());
        assert!(unflashed(&w), "restart");
        w.next_impulse = 9;
        step_walk(&mut w, 0.05, false, &crate::vid::mode_vid(320, 200), true);
        assert_ne!(w.item_gettime[4], 0.0, "got again after the restart");
        try_changelevel(&mut w, "e1m2", &mut Vec::new());
        assert_ne!(client_items(&w) & (1 << 4), 0, "the rocket launcher is carried");
        assert!(unflashed(&w), "changelevel");
        step_walk(&mut w, 0.05, false, &crate::vid::mode_vid(320, 200), true);
        assert!(unflashed(&w));

        let text = w.server.write_savegame();
        let rand = std::rc::Rc::clone(w.server.rand());
        let mut l = quake_rs::client::host_cmd::build_walk_savegame(
            w.pak.clone(),
            &text,
            &rand,
            &mut Vec::new(),
            quake_rs::vm::MAX_EDICTS,
        )
        .expect("the save loads");
        assert!(unflashed(&l), "load");
        step_walk(&mut l, 0.05, false, &crate::vid::mode_vid(320, 200), true);
        assert!(unflashed(&l));
    }

    /// CENSUS F1: svc_setangle carries MSG_WriteAngle's byte — whole degrees,
    /// 256 steps, read back signed — and a new level starts facing the angles
    /// PutClientInServer gave the player (Host_Spawn_f's setangle).
    #[test]
    fn setangle_quantises_like_the_wire_and_spawns_face_the_spot() {
        assert_eq!(wire_angle(90.0), 90.0);
        assert_eq!(wire_angle(45.0), 45.0);
        assert_eq!(wire_angle(270.0), -90.0, "byte 192 reads back as char -64");
        assert_eq!(wire_angle(10.9), 7.0 * 360.0 / 256.0, "(int)10.9*256/360 = 7");
        assert_eq!(wire_angle(-90.0), -90.0);
        let w = build_walk().expect("e1m1 boots");
        let spot = crate::app::player_start(&w.bsp.entities).expect("e1m1 has a start").1;
        assert_eq!(w.yaw, wire_angle(spot), "the view faces info_player_start's angle");
        assert_eq!(w.pitch, 0.0);
        assert_eq!(w.server.vm.ent_get_float(w.player, "fixangle"), 0.0);
    }

    /// CENSUS F2: behind the menu/console single player is paused
    /// (Host_ServerFrame skips SV_Physics, SV_RunClients skips SV_ClientThink):
    /// sv.time and cl.time stand still, the particles neither move nor die,
    /// and a queued impulse waits for the server. What the C runs off
    /// host_frametime / realtime keeps going: the damage fade and the
    /// centerprint countdown.
    #[test]
    fn single_player_pause_freezes_the_world_but_not_the_host_clock() {
        let mut w = build_walk().expect("e1m1 boots");
        for _ in 0..3 {
            step_walk(&mut w, 0.1, false, &crate::vid::mode_vid(320, 200), true);
        }
        w.particles.spawn_burst([0.0; 3], [0.0; 3], 73, 20, w.clock, &mut w.prng);
        w.centerprint = Some(("paused".into(), w.host_time + 2.0));
        w.damage_blend = 100.0;
        w.next_impulse = 2;
        let (sv0, cl0, parts0) = (w.server.time(), w.clock, w.particles.particles().len());
        let org0 = w.particles.particles()[0].origin;
        for _ in 0..25 {
            step_walk(&mut w, 0.1, true, &crate::vid::mode_vid(320, 200), true); // menu up for 2.5 s
        }
        assert_eq!(w.server.time(), sv0, "sv.time stands still");
        assert_eq!(w.clock, cl0, "cl.time stands still");
        assert_eq!(w.particles.particles().len(), parts0, "no particle dies");
        assert_eq!(w.particles.particles()[0].origin, org0, "no particle moves");
        assert_eq!(w.next_impulse, 2, "the impulse waits for the server");
        assert!(w.centerprint.is_none(), "the centerprint timed out behind the menu");
        assert_eq!(w.damage_blend, 0.0, "the damage flash faded behind the menu");
        step_walk(&mut w, 0.1, false, &crate::vid::mode_vid(320, 200), true);
        assert!(w.server.time() > sv0, "the game resumes when the menu closes");
        assert_eq!(w.next_impulse, 0, "and the waiting impulse is sent");
    }

    #[test]
    fn level_exit_runs_the_intermission_then_changelevel() {
        // The faithful end-of-level flow, driven end-to-end through the REAL
        // progs.dat: touching trigger_changelevel runs execute_changelevel (the
        // QC freezes the player on the info_intermission spot and WriteBytes
        // svc_intermission to MSG_ALL), the engine enters intermission mode and
        // draws the stats overlay, and only a button press AFTER
        // intermission_exittime (time+2) runs GotoNextMap -> changelevel(e1m2).
        assert_eq!(boot(), 1);
        set_resolution(320, 200); // debug-build render speed; clamps to the min preset
        close_menu(); // boot opens the menu; buttons are gated while it is up
        let before_shells = player_field("ammo_shells") as i32;

        drive_into_exit();

        // --- The engine is in intermission: camera frozen on the QC-moved
        // player, stats overlay up, status bar hidden.
        walk_mut(|w| {
            assert_eq!(w.intermission, 1, "svc_intermission set cl.intermission = 1");
            // cl.completed_time = cl.time = sv.time (cl_parse.c:939), and
            // SV_SpawnServer starts sv.time at 1.0 — the walk clock (starting at
            // 0) would read at least 1 second LOW here. drive_into_exit returned
            // the moment the latch happened, so the latched value IS the server's
            // current clock.
            assert!(w.completed_time >= 1.0, "completed_time latches sv.time (epoch 1.0)");
            assert_eq!(
                w.completed_time,
                w.server.time(),
                "completed_time = sv.time at the latch (no steps ran since)"
            );
            // execute_changelevel froze the player: MOVETYPE_NONE, modelindex 0,
            // view_ofs zeroed, moved to the info_intermission spot.
            assert_eq!(
                w.server.vm.ent_get_float(w.player, "movetype") as i32,
                0,
                "player frozen MOVETYPE_NONE"
            );
            assert_eq!(
                w.server.vm.ent_get_vector(w.player, "view_ofs"),
                [0.0, 0.0, 0.0],
                "view_ofs zeroed for the intermission camera"
            );
            // The stats the overlay shows come from the QC globals and are sane.
            assert!(
                w.server.vm.gget_float("total_monsters") > 0.0,
                "e1m1 reports a monster total"
            );
        });
        // The QC moved the player to the info_intermission spot (e1m1 has one);
        // its angles came from the spot's mangle via fixangle.
        let pinned = changelevel_trigger_center();
        walk_mut(|w| {
            let org = w.server.vm.ent_get_vector(w.player, "origin");
            assert_ne!(org, pinned, "player moved OFF the exit to the intermission spot");
        });

        // --- Overlay pixels: render the same frozen frame with and without the
        // intermission flag; the plaque/number region (virtual x>=160, y 56..160)
        // is 3-D view in one and Sbar_IntermissionOverlay in the other.
        let (with_overlay, without_overlay) = walk_mut(|w| {
            let a = step_walk(w, 0.0, false, &crate::vid::mode_vid(320, 200), true).0;
            w.intermission = 0;
            let b = step_walk(w, 0.0, false, &crate::vid::mode_vid(320, 200), true).0;
            w.intermission = 1;
            (a, b)
        });
        let region_differs = (56..160).any(|y| {
            (160..320).any(|x| with_overlay.pixels[y * 320 + x] != without_overlay.pixels[y * 320 + x])
        });
        assert!(region_differs, "the intermission overlay painted the stats region");
        // The crosshair (slop's `crosshair 1`) stays off the level-complete
        // screen, as id's GLQuake leaves it: the frozen frame is the same with
        // it on or off — and out of the intermission it does draw.
        let crosshair_changes = |intermission: u8| {
            walk_mut(|w| {
                w.intermission = intermission;
                w.crosshair = quake_rs::render::Crosshair::Cross;
                let on = step_walk(w, 0.0, false, &crate::vid::mode_vid(320, 200), true).0;
                w.crosshair = quake_rs::render::Crosshair::Off;
                let off = step_walk(w, 0.0, false, &crate::vid::mode_vid(320, 200), true).0;
                w.intermission = 1;
                on.pixels != off.pixels
            })
        };
        assert!(!crosshair_changes(1), "no crosshair over the intermission");
        assert!(crosshair_changes(0), "the crosshair draws outside it");
        dump_frame("intermission-e1m1");

        // --- No button: the intermission HOLDS even long past exittime.
        for _ in 0..25 {
            step(0.1);
        }
        walk_mut(|w| {
            assert_eq!(w.intermission, 1, "no button => still at the intermission");
            assert_eq!(w.map_name, "maps/e1m1.bsp", "no level change without a button");
        });

        // --- Attack pressed: IntermissionThink (time >= exittime, button down)
        // runs ExitIntermission -> GotoNextMap -> changelevel("e1m2"); the host
        // drains the pending request and swaps, carrying the inventory parms.
        walk_mut(|w| w.in_attack = true);
        for _ in 0..5 {
            step(0.1);
            if walk_mut(|w| w.map_name.clone()) != "maps/e1m1.bsp" {
                break;
            }
        }
        walk_mut(|w| {
            assert_eq!(w.map_name, "maps/e1m2.bsp", "the exit leads to e1m2");
            assert_eq!(w.intermission, 0, "the new level starts out of intermission");
            w.in_attack = false;
        });
        assert_eq!(
            player_field("ammo_shells") as i32,
            before_shells,
            "spawn parms carried the inventory across the swap"
        );
    }

    #[test]
    fn e1m7_exit_reaches_the_shareware_finale_and_sellscreen() {
        // Episode end: e1m7's exit runs the same intermission, but the SECOND
        // button press (ExitIntermission with intermission_running == 2 and
        // world.model == "maps/e1m7.bsp", cvar("registered") == 0) emits
        // svc_finale + the shareware episode text, and the THIRD press
        // (running == 3, shareware) emits svc_sellscreen — which pops the
        // Help/Ordering menu exactly like Cmd_ExecuteString("help").
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        close_menu();
        console_toggle();
        run_console_line("map e1m7");
        walk_mut(|w| assert_eq!(w.map_name, "maps/e1m7.bsp", "console map swap"));
        assert_eq!(console_visible(), 0, "a successful map command closed the console");
        close_menu(); // the fresh-walk path must not leave the menu gating input

        drive_into_exit();
        walk_mut(|w| assert_eq!(w.intermission, 1));

        // Hold attack: IntermissionThink exits as soon as time passes exittime
        // (time+2), then svc_finale arrives with the episode-end text.
        walk_mut(|w| w.in_attack = true);
        for _ in 0..30 {
            step(0.1);
            if walk_mut(|w| w.intermission) == 2 {
                break;
            }
        }
        walk_mut(|w| {
            assert_eq!(w.intermission, 2, "svc_finale set cl.intermission = 2");
            assert!(
                w.finale_text.starts_with("As the corpse of the monstrous entity"),
                "the shareware episode-1 finale text arrived; got {:?}",
                &w.finale_text[..w.finale_text.len().min(60)]
            );
            assert_eq!(w.map_name, "maps/e1m7.bsp", "the finale shows BEFORE any map change");
        });
        // Let ~1.5s of the slow text reveal pass, then dump the evidence frame.
        walk_mut(|w| w.in_attack = false);
        for _ in 0..15 {
            step(0.1);
        }
        dump_frame("finale-e1m7");

        // Third press (after the finale's exittime = time+1): shareware emits
        // svc_sellscreen; the dispatcher opens the menu on the Help screen.
        walk_mut(|w| w.in_attack = true);
        for _ in 0..30 {
            step(0.1);
            let open = APP.with(|c| c.borrow().as_ref().unwrap().menu.visible);
            if open {
                break;
            }
        }
        APP.with(|c| {
            let b = c.borrow();
            let menu = &b.as_ref().unwrap().menu;
            assert!(menu.visible, "svc_sellscreen popped the menu");
            assert_eq!(
                menu.screen(),
                render::MenuScreen::Help,
                "the sell screen is the Help/Ordering pages"
            );
        });
        walk_mut(|w| w.in_attack = false);
    }

    /// R_SetupFrame's r_dowarp: with the eye in water the view is rendered
    /// into the (at most) 320x200 warp buffer, and D_WarpScreen stretches it
    /// over the screen's view rectangle. So underwater a 640x400 screen and a
    /// 320x200 one render the same 320x152 view (the world pass writes the
    /// same pixels), while above water 640x400 draws four times as many.
    #[test]
    fn underwater_view_renders_into_the_warp_buffer() {
        use quake_rs::progs::OFS_PARM0;
        let mut w = build_walk().expect("e1m1 boots");
        let _ = step_walk(&mut w, 0.05, false, &crate::vid::mode_vid(320, 200), true);
        let p = w.player;
        w.server.vm.ent_set_float(p, "movetype", 8.0); // MOVETYPE_NOCLIP: stays put
        let frame_at = |w: &mut Walk, org: [f32; 3], rw: usize, rh: usize| {
            let vm = &mut w.server.vm;
            vm.set_gi(OFS_PARM0, p);
            vm.set_gv(OFS_PARM0 + 3, org);
            vm.call_builtin(2, 2).expect("setorigin");
            vm.ent_set_vector(p, "velocity", [0.0; 3]);
            w.renderer.stats_begin();
            let (img, _) = step_walk(w, 0.0, false, &crate::vid::mode_vid(rw, rh), true);
            let px = w.renderer.stats_end().world_pixels;
            let eye = [org[0], org[1], org[2] + 22.0];
            assert_eq!((img.w, img.h), (rw, rh));
            (px, quake_rs::world::point_contents(&w.bsp, eye))
        };
        // e1m1's start pool, eye at the review's oracle view (750, 898, -332),
        // then lifted above the water in the same room.
        let (under, above) = ([750.0, 898.0, -354.0], [750.0, 898.0, -272.0]);
        let (u640, c) = frame_at(&mut w, under, 640, 400);
        assert_eq!(c, quake_rs::bsp::CONTENTS_WATER);
        let (u320, _) = frame_at(&mut w, under, 320, 200);
        // id's 48-row bar is (int)(48 * 200/400) = 24 rows of the warp buffer
        // at 640x400: a 320x176 render, taller than 320x200's 320x152.
        assert!(u640 > u320, "underwater: 320x176 at 640x400 ({u640} vs {u320})");
        {
            // The "scaled 2-D" extra's bar is 48 rows of the 320x200 screen.
            let _extra = Scaled2dGuard::set(true);
            let (s640, _) = frame_at(&mut w, under, 640, 400);
            assert_eq!(s640, u320, "underwater, scaled 2-D: the same 320x152 render at both sizes");
        }
        let (a640, c) = frame_at(&mut w, above, 640, 400);
        assert_eq!(c, quake_rs::bsp::CONTENTS_EMPTY);
        let (a320, _) = frame_at(&mut w, above, 320, 200);
        assert!(a640 > 3 * a320, "above water: 640x400 renders at full size ({a640} vs {a320})");
    }

    /// cl.time is the server's clock: on a local server CL_LerpPoint snaps it
    /// to the message time, sv.time after the frame's physics. So the sky,
    /// liquids, the underwater warp and R_AnimateLight's `(int)(cl.time*10)`
    /// start at SV_SpawnServer's 1.0 plus the signon frames, not at 0, stop
    /// with the server behind the menu, and follow it across a restart and a
    /// changelevel (a load: `save_load_round_trips_the_world_digest`).
    #[test]
    fn client_clock_is_the_server_clock() {
        let mut w = build_walk().expect("e1m1 boots");
        let t0 = w.server.time();
        assert!(t0 > 1.0, "sv.time at spawn: 1.0 + the signon frames ({t0})");
        assert_eq!(w.clock, t0, "the first frame draws at cl.time = sv.time");
        for _ in 0..5 {
            let _ = step_walk(&mut w, 0.05, false, &crate::vid::mode_vid(320, 200), true);
            assert_eq!(w.clock, w.server.time());
        }
        let t = w.clock;
        let _ = step_walk(&mut w, 0.05, true, &crate::vid::mode_vid(320, 200), true); // paused behind the menu
        assert_eq!((w.clock, w.server.time()), (t, t));
        try_restart(&mut w, &mut Vec::new());
        assert_eq!(w.clock, w.server.time());
        assert!(w.clock < t, "a restarted level's clock starts over");
        try_changelevel(&mut w, "e1m2", &mut Vec::new());
        assert_eq!(w.map_name, "maps/e1m2.bsp");
        assert_eq!(w.clock, w.server.time());
        assert!(w.clock > 1.0 && w.clock < 2.0, "{}", w.clock);
    }

    /// The live host hands the server Host_FilterTime's double: `sv.time` (a
    /// double) adds exactly `host_frametime`, as SV_Physics does, not the f32
    /// the client frame times itself with.
    #[test]
    fn the_server_advances_by_the_hosts_double() {
        let mut w = build_walk().expect("e1m1 boots");
        let t0 = w.server.sv_time();
        let _ = step_walk(&mut w, 1.0 / 72.0, false, &crate::vid::mode_vid(320, 200), true);
        assert_eq!(w.server.sv_time(), t0 + 1.0 / 72.0);
        assert_ne!(1.0 / 72.0, f64::from((1.0f64 / 72.0) as f32));
        let _ = step_walk(&mut w, 0.05, true, &crate::vid::mode_vid(320, 200), true); // paused behind the menu
        assert_eq!(w.server.sv_time(), t0 + 1.0 / 72.0);
    }

    // -------------------------------------------------------------------
    // C1: the client relinks only what SV_WriteEntitiesToClient sends, and
    // draws statics through their efrags (R_StoreEfrags)
    // -------------------------------------------------------------------

    /// `PF_setorigin` through the engine's builtin: origin + SV_LinkEdict, so
    /// the edict's PVS leaves follow it.
    fn set_origin(w: &mut Walk, e: i32, org: [f32; 3]) {
        let vm = &mut w.server.vm;
        vm.set_gi(quake_rs::progs::OFS_PARM0, e);
        vm.set_gv(quake_rs::progs::OFS_PARM1, org);
        vm.call_builtin(2, 2).expect("setorigin");
    }

    /// Render the current state again without advancing anything (paused,
    /// dt 0, the same rng draws): the frame, and how many alias models reached
    /// the renderer.
    fn rerender(w: &mut Walk, rng: quake_rs::particles::Lcg) -> (render::Image, u64) {
        w.prng = rng;
        w.renderer.stats_begin();
        let (img, _) = step_walk(w, 0.0, true, &crate::vid::mode_vid(320, 200), true);
        (img, w.renderer.stats_end().alias_models)
    }

    fn pixels_differing(a: &render::Image, b: &render::Image) -> usize {
        a.pixels.iter().zip(b.pixels.iter()).filter(|(x, y)| x != y).count()
    }

    /// The start map facing north (yaw 90), settled; and the first alias-model
    /// entity that is not the player, to move around.
    fn start_facing_north() -> (Walk, i32) {
        let mut w = build_walk_map("maps/start.bsp").expect("start boots");
        w.yaw = 90.0;
        w.pitch = 0.0;
        for _ in 0..10 {
            let _ = step_walk(&mut w, 0.05, false, &crate::vid::mode_vid(320, 200), true);
        }
        let vm = &w.server.vm;
        let e = (1..vm.num_edicts() as i32)
            .find(|&e| {
                e != w.player && !vm.is_free_edict(e) && vm.ent_string_ref(e, "model").ends_with(".mdl")
            })
            .expect("an alias-model entity");
        (w, e)
    }

    /// C1 and CENSUS L22. An entity behind a wall, in a leaf outside the
    /// player's fat PVS, is not sent (SV_WriteEntitiesToClient), so the client
    /// neither draws it nor makes its EF_MUZZLEFLASH light — a light that would
    /// otherwise brighten the near side of the wall (R_MarkLights marks by
    /// plane distance and R_AddDynamicLights lights by |distance|, so a light
    /// does reach through a wall).
    #[test]
    fn an_entity_outside_the_fat_pvs_is_not_drawn_and_its_flash_lights_nothing() {
        use quake_rs::bsp::CONTENTS_SOLID;
        let (mut w, e) = start_facing_north();
        // The wall straight ahead, and open space 36 units behind it.
        let (eye, _) = w.server.player_view();
        let far = [eye[0], eye[1] + 3000.0, eye[2]];
        let tr = quake_rs::world::trace_world(&w.bsp, eye, far, [0.0; 3], [0.0; 3]);
        assert!(tr.fraction < 1.0, "a wall ahead");
        let n = tr.plane_normal;
        let behind = [tr.endpos[0] - n[0] * 36.0, tr.endpos[1] - n[1] * 36.0, tr.endpos[2] - n[2] * 36.0];
        assert_ne!(quake_rs::world::point_contents(&w.bsp, behind), CONTENTS_SOLID, "open space behind");
        set_origin(&mut w, e, behind);
        w.server.vm.ent_set_vector(e, "angles", [0.0, 90.0, 0.0]); // facing away
        assert!(!w.server.entities_sent_to_client()[e as usize], "behind the wall: not sent");

        let rng = w.prng;
        w.server.vm.ent_set_float(e, "effects", quake_rs::server::EF_MUZZLEFLASH as f32);
        let (firing, firing_models) = rerender(&mut w, rng);
        assert!(w.dlights.active(f64::from(w.clock)).iter().all(|d| d.key() != e), "no light for an entity not sent");
        w.server.vm.ent_set_float(e, "effects", 0.0);
        let (quiet, quiet_models) = rerender(&mut w, rng);
        assert_eq!(pixels_differing(&firing, &quiet), 0, "the flash behind the wall lights nothing");
        let modelindex = w.server.vm.ent_get_float(e, "modelindex");
        w.server.vm.ent_set_float(e, "modelindex", 0.0);
        let (_, hidden_models) = rerender(&mut w, rng);
        w.server.vm.ent_set_float(e, "modelindex", modelindex);
        assert_eq!(firing_models, hidden_models, "not handed to the renderer");
        assert_eq!(quiet_models, hidden_models);

        // The control: the light CL_RelinkEntities makes for a sent entity
        // (origin + 16 up + 18 forward, radius 200 before the rand()&31,
        // minlight 32) does light the visible side of the wall.
        let now = w.clock;
        let muzzle = [behind[0], behind[1] + 18.0, behind[2] + 16.0];
        w.dlights.alloc(e, muzzle, 200.0, now + 0.1, 0.0, 32.0, f64::from(now));
        let (lit, _) = rerender(&mut w, rng);
        assert!(
            pixels_differing(&lit, &quiet) > 100,
            "the light, had it been made, reaches through the wall ({} px)",
            pixels_differing(&lit, &quiet)
        );
    }

    /// The other side of C1: the same entity in plain view is sent, drawn,
    /// and its muzzle flash is made.
    #[test]
    fn an_entity_in_view_is_drawn_and_its_flash_made() {
        let (mut w, e) = start_facing_north();
        let (eye, _) = w.server.player_view();
        set_origin(&mut w, e, [eye[0], eye[1] + 100.0, eye[2] - 20.0]);
        assert!(w.server.entities_sent_to_client()[e as usize], "in view: sent");
        let rng = w.prng;
        w.server.vm.ent_set_float(e, "effects", quake_rs::server::EF_MUZZLEFLASH as f32);
        let (shown, shown_models) = rerender(&mut w, rng);
        assert!(w.dlights.active(f64::from(w.clock)).iter().any(|d| d.key() == e), "its muzzle flash is made");
        w.server.vm.ent_set_float(e, "effects", 0.0);
        w.server.vm.ent_set_float(e, "modelindex", 0.0);
        let (hidden, hidden_models) = rerender(&mut w, rng);
        assert_eq!(shown_models, hidden_models + 1, "handed to the renderer");
        assert!(pixels_differing(&shown, &hidden) > 0, "and drawn");
    }

    /// C1 for statics: the signon's statics are drawn when a leaf their box
    /// touches (R_AddEfrags) is in the view's PVS (R_MarkLeaves), which on the
    /// start map keeps some torches and drops others; the kept ones draw.
    #[test]
    fn statics_draw_through_efrags_in_the_view_pvs() {
        let (mut w, _) = start_facing_north();
        let (eye, _) = w.server.player_view();
        let view_pvs = w.bsp.leaf_pvs(render::point_in_leaf(&w.bsp, eye).unwrap_or(0));
        let statics = w.server.statics();
        let h = ALIAS_MODEL_HALF;
        let visible = statics
            .iter()
            .filter(|st| {
                let (lo, hi) = offset_box(st.origin, [-h; 3], [h; 3]);
                st.model.ends_with(".mdl") && static_is_visible(&w.bsp, &view_pvs, lo, hi)
            })
            .count();
        assert!(statics.len() > 30, "start's torches and flames are statics ({})", statics.len());
        assert!(
            visible > 0 && visible < statics.len(),
            "some statics in the view's PVS, not all ({visible} of {})",
            statics.len()
        );
        // PF_makestatic freed their edicts: no live entity carries their models.
        let vm = &w.server.vm;
        let static_model = |e: i32| statics.iter().any(|st| st.model == vm.ent_string_ref(e, "model"));
        assert_eq!(vm.live_edicts().filter(|&e| static_model(e)).count(), 0, "a static is no edict");
        let rng = w.prng;
        let (shown, shown_models) = rerender(&mut w, rng);
        // Without their alias models (as if the pak had none) the client draws
        // none of the torches and flames.
        for st in w.server.statics().iter().filter(|st| st.model.ends_with(".mdl")) {
            w.model_cache.insert(st.model.clone(), None);
        }
        let (hidden, hidden_models) = rerender(&mut w, rng);
        assert_eq!(shown_models, hidden_models + visible as u64, "the visible statics reach the renderer");
        assert!(pixels_differing(&shown, &hidden) > 0, "and draw");
    }

    // -------------------------------------------------------------------
    // The start map's teleporters and slipgates, as oracle/sound_walk.py
    // walks them through id's game
    // -------------------------------------------------------------------

    /// The centres of the brush triggers `classname` whose `key` is `value`
    /// (InitTrigger clears a trigger's `model`; its keys tell them apart).
    fn trigger_centres(w: &Walk, classname: &str, key: &str, value: &str) -> Vec<[f32; 3]> {
        let vm = &w.server.vm;
        (1..vm.num_edicts() as i32)
            .filter(|&e| {
                !vm.is_free_edict(e)
                    && vm.ent_string_ref(e, "classname") == classname
                    && vm.ent_string_ref(e, key) == value
            })
            .map(|e| {
                let (a, b) = (vm.ent_get_vector(e, "absmin"), vm.ent_get_vector(e, "absmax"));
                [0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1]), 0.5 * (a[2] + b[2])]
            })
            .collect()
    }

    /// One frame of the live walk at 72 Hz, and the calls it made into the
    /// sound layer.
    fn sound_frame(w: &mut Walk) -> Vec<SoundCall> {
        let frame = walk_frame(w, 1.0 / 72.0, false, &crate::vid::mode_vid(320, 200));
        render::recycle_image(frame.image);
        frame.sound
    }

    fn started(calls: &[SoundCall]) -> Vec<quake_rs::server::SoundEvent> {
        calls
            .iter()
            .flat_map(|c| if let SoundCall::Start { events, .. } = c { events.clone() } else { Vec::new() })
            .collect()
    }

    /// A sound reaches the mixer where id's client placed it:
    /// CL_ParseStartSoundPacket reads its position with MSG_ReadCoord, to the
    /// 1/8 unit. The NORMAL skill hall's teleporter, entered at x 544.3: the
    /// fog left where the player stood (play_teleport, 0.2 s on) sounds at x
    /// 544.25, as in id's game (sound_walk.py's `telegate`).
    #[test]
    fn a_teleporters_fog_sounds_where_ids_client_put_it() {
        let mut w = build_walk_map("maps/start.bsp").expect("start boots");
        // The skill halls end in trigger_teleports to the hub (t1); NORMAL's
        // is the middle one, at x 544.
        let c = *trigger_centres(&w, "trigger_teleport", "target", "t1")
            .iter()
            .find(|c| (c[0] - 544.0).abs() < 1.0)
            .expect("the NORMAL skill hall's teleporter");
        let p = w.player;
        set_origin(&mut w, p, [c[0] + 0.3, c[1], c[2]]);
        let mut fog = Vec::new();
        for _ in 0..30 {
            fog.extend(started(&sound_frame(&mut w)).into_iter().filter(|e| e.sample.starts_with("misc/r_tele")));
        }
        assert_eq!(fog.len(), 2, "a fog where the player stood and one at the destination: {fog:?}");
        let on_the_wire = |v: f32| (v * 8.0).fract() == 0.0;
        assert!(fog.iter().all(|e| e.origin.iter().all(|&v| on_the_wire(v))), "{fog:?}");
        assert!(fog.iter().any(|e| e.origin[0] == 544.25), "the player's x, 544.3, as the wire carried it: {fog:?}");
    }

    /// The first episode's slipgate makes no sound of its own: id's
    /// changelevel_touch starts none, and the frame it takes the player to
    /// e1m1 stops every sound (S_StopAllSounds) and starts e1m1's placed
    /// loops, nothing else (sound_walk.py walks into it through id's game).
    #[test]
    fn the_slipgate_to_e1m1_starts_no_sound() {
        let mut w = build_walk_map("maps/start.bsp").expect("start boots");
        for _ in 0..3 {
            let _ = sound_frame(&mut w);
        }
        let gate = trigger_centres(&w, "trigger_changelevel", "map", "e1m1")[0];
        let p = w.player;
        set_origin(&mut w, p, gate);
        let calls = sound_frame(&mut w);
        assert_eq!(w.map_name, "maps/e1m1.bsp", "the slipgate's frame loads e1m1");
        assert!(calls.iter().any(|c| matches!(c, SoundCall::StopAll)), "S_StopAllSounds");
        assert!(calls.iter().any(|c| matches!(c, SoundCall::Static(s) if !s.is_empty())), "e1m1's loops");
        assert_eq!(started(&calls), [], "no S_StartSound in the slipgate's frame");
    }
}

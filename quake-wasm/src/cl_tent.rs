//! The temp-entity client code is [`quake_rs::client::cl_tent`]; this is its
//! end-to-end test on the real e1m7, through the live frame.

#[cfg(test)]
mod tests {
    use quake_rs::math::{length, normalize, sub, vector_ma};
    use quake_rs::progs::OFS_PARM0;
    use quake_rs::server::probe_point_contents;
    use quake_rs::tent::BeamModel;

    use crate::app::{build_walk_map, Walk};
    use crate::cl_walk::step_walk;
    use crate::snd_dma::{clear_pending, pending_starts};

    /// `FL_GODMODE` (server.h), what `Host_God_f` toggles.
    const FL_GODMODE: i32 = 64;
    /// doors.qc / buttons.qc states: `STATE_TOP` 0, `STATE_BOTTOM` 1.
    const STATE_TOP: f32 = 0.0;
    const STATE_BOTTOM: f32 = 1.0;

    fn live(w: &Walk, e: i32) -> bool {
        !w.server.vm.is_free_edict(e)
    }

    /// Every in-use edict whose string `field` is `value`, in edict order (the
    /// order QuakeC's `find` walks).
    fn find_all(w: &Walk, field: &str, value: &str) -> Vec<i32> {
        (0..w.server.vm.num_edicts() as i32)
            .filter(|&e| live(w, e) && w.server.vm.ent_get_string(e, field) == value)
            .collect()
    }

    /// The `func_button` that targets `target`.
    fn button(w: &Walk, target: &str) -> i32 {
        find_all(w, "target", target)
            .into_iter()
            .find(|&e| w.server.vm.ent_get_string(e, "classname") == "func_button")
            .unwrap_or_else(|| panic!("a func_button targets {target}"))
    }

    /// Put the player at `org` at rest, through `PF_setorigin` (so its
    /// absmin/absmax and links follow).
    fn pin(w: &mut Walk, org: [f32; 3]) {
        let p = w.player;
        let vm = &mut w.server.vm;
        vm.set_gi(OFS_PARM0, p);
        vm.set_gv(OFS_PARM0 + 3, org);
        vm.call_builtin(2, 2).expect("setorigin");
        vm.ent_set_vector(p, "velocity", [0.0; 3]);
    }

    fn frame(w: &mut Walk) {
        let _ = step_walk(w, 0.1, false, &crate::vid::mode_vid(320, 200));
    }

    fn state(w: &Walk, e: i32) -> f32 {
        w.server.vm.ent_get_float(e, "state")
    }

    /// Stand the player on a floor `func_button`, 1 unit above its top: the
    /// frame's gravity move clips against the plate and SV_FlyMove's SV_Impact
    /// runs `button_touch` — the touch a player walking onto it makes. Waits
    /// for the button to be back at STATE_BOTTOM first (its `wait`), and
    /// returns once `button_fire` has moved it off.
    fn step_on_button(w: &mut Walk, b: i32) {
        for _ in 0..300 {
            if state(w, b) == STATE_BOTTOM {
                break;
            }
            frame(w);
        }
        let amin = w.server.vm.ent_get_vector(b, "absmin");
        let amax = w.server.vm.ent_get_vector(b, "absmax");
        let spot = [0.5 * (amin[0] + amax[0]), 0.5 * (amin[1] + amax[1]), amax[2] + 25.0];
        for _ in 0..10 {
            pin(w, spot);
            frame(w);
            if state(w, b) != STATE_BOTTOM {
                return;
            }
        }
        panic!("button {b} never fired under the player");
    }

    /// Write `img`'s palette indices as a binary greyscale PGM (the tests'
    /// dump format; convert to PNG outside).
    fn dump_pgm(path: &str, img: &quake_rs::render::Image) {
        let mut buf = format!("P5\n{} {}\n255\n", img.w, img.h).into_bytes();
        buf.extend_from_slice(&img.pixels);
        let _ = std::fs::write(path, buf);
    }

    #[test]
    fn chthon_lightning_reaches_the_client_and_kills_him_on_e1m7() {
        // The shareware finale, end to end on the REAL e1m7 through the live
        // path. The two floor buttons (t12/t13) lower the electrode doors
        // (target "lightning") into the pit; the third (t14) fires
        // event_lightning's lightning_use, whose lightning_fire think writes
        // svc_temp_entity / TE_LIGHTNING3 / WriteEntity(world) / p1 / p2 to
        // MSG_ALL every 0.1 s for a second. The client's CL_ParseServerMessage
        // reads svc_temp_entity out of the reliable buffer like any other, so
        // CL_ParseBeam stores a world-owned beam and CL_UpdateTEnts draws it
        // as progs/bolt3.mdl pieces across the pit. Before the fix only
        // MSG_BROADCAST temp entities were decoded: the server still shocked
        // and killed Chthon, but no bolt was ever drawn ("Chthon has no
        // electricity"). Three shocks kill him (skill 1: boss_awake gives him
        // 3 health), and the exit leads to the episode finale.
        let mut w = build_walk_map("maps/e1m7.bsp").expect("e1m7 boots from the embedded pak");
        let start = w.server.vm.ent_get_vector(w.player, "origin");
        let boss = find_all(&w, "classname", "monster_boss")[0];
        let event = find_all(&w, "classname", "event_lightning")[0];
        let electrodes = find_all(&w, "target", "lightning");
        assert_eq!(electrodes.len(), 2, "two electrode doors target \"lightning\"");
        // Host_God_f: Chthon's lava balls must not end the run.
        let flags = w.server.vm.ent_get_float(w.player, "flags") as i32 | FL_GODMODE;
        w.server.vm.ent_set_float(w.player, "flags", flags as f32);

        // Take the rune: item_sigil's SUB_UseTargets (t4) runs boss_awake.
        let sigil = find_all(&w, "classname", "item_sigil")[0];
        let (smin, smax) =
            (w.server.vm.ent_get_vector(sigil, "absmin"), w.server.vm.ent_get_vector(sigil, "absmax"));
        let sigil_centre: [f32; 3] = std::array::from_fn(|i| 0.5 * (smin[i] + smax[i]));
        for _ in 0..10 {
            pin(&mut w, sigil_centre);
            frame(&mut w);
            if w.server.vm.ent_get_float(boss, "health") > 0.0 {
                break;
            }
        }
        assert_eq!(w.server.vm.ent_get_float(boss, "health"), 3.0, "Chthon rose (skill 1)");
        pin(&mut w, start);

        for shock in 1..=3 {
            // Lower both electrodes into the pit (door_go_up -> STATE_TOP).
            let (b12, b13) = (button(&w, "t12"), button(&w, "t13"));
            step_on_button(&mut w, b12);
            step_on_button(&mut w, b13);
            pin(&mut w, start);
            for _ in 0..100 {
                if electrodes.iter().all(|&d| state(&w, d) == STATE_TOP) {
                    break;
                }
                frame(&mut w);
            }
            assert!(
                electrodes.iter().all(|&d| state(&w, d) == STATE_TOP),
                "shock {shock}: both electrodes down in the pit"
            );
            // lightning_fire's endpoints: p1 = le1's model-space box centre at
            // absmin_z - 16; p2 likewise from le2, then pulled 100 units back
            // towards p1 ("compensate for length of bolt").
            let tip = |d: i32| -> [f32; 3] {
                let (mn, mx) = (w.server.vm.ent_get_vector(d, "mins"), w.server.vm.ent_get_vector(d, "maxs"));
                let z = w.server.vm.ent_get_vector(d, "absmin")[2] - 16.0;
                [(mn[0] + mx[0]) * 0.5, (mn[1] + mx[1]) * 0.5, z]
            };
            let (p1, far) = (tip(electrodes[0]), tip(electrodes[1]));
            let p2 = vector_ma(far, -100.0, normalize(sub(far, p1)).0);
            let span = length(sub(p2, p1));

            // The lightning button (t14 -> event_lightning -> lightning_use).
            clear_pending();
            let health_before = w.server.vm.ent_get_float(boss, "health");
            let b14 = button(&w, "t14");
            step_on_button(&mut w, b14);
            pin(&mut w, start);
            let mut live_frames = 0;
            for _ in 0..20 {
                frame(&mut w);
                if !w.beams.any_live(w.clock) {
                    continue;
                }
                live_frames += 1;
                // CL_UpdateTEnts expanded the world-owned beam: bolt3 pieces
                // every 30 units from p1 (entity 0 is not the view entity, so
                // the start is not re-anchored) until the span is used up.
                assert!(w.beam_scratch.iter().all(|s| s.model == BeamModel::Bolt3), "only Chthon's bolt");
                let pieces = w.beam_scratch.len() as f32;
                assert!(
                    (pieces - span / 30.0).abs() <= 1.0,
                    "shock {shock}: one bolt across the pit ({pieces} pieces for {span} units)"
                );
                for (axis, (got, want)) in w.beam_scratch[0].origin.iter().zip(p1).enumerate() {
                    assert!((got - want).abs() < 0.01, "shock {shock}: the bolt starts at p1 (axis {axis})");
                }
                if shock == 1 && live_frames == 3 {
                    // Pixel evidence: look at the bolt from 350 units east of
                    // the pit's middle, 120 up (open air over the lava), and
                    // render the same state (dt = 0, same rng) with and without
                    // the beam store: the only difference is the bolt.
                    let keep = (w.server.vm.ent_get_vector(w.player, "origin"), w.yaw, w.pitch);
                    let mid: [f32; 3] = std::array::from_fn(|i| 0.5 * (p1[i] + p2[i]));
                    let (d, _) = normalize(sub(p2, p1));
                    let eye = [mid[0] - d[1] * 350.0, mid[1] + d[0] * 350.0, mid[2] + 120.0];
                    assert_eq!(probe_point_contents(&mut w.server.vm, eye), -1, "the eye is in open air");
                    let to = sub(mid, eye);
                    w.yaw = to[1].atan2(to[0]).to_degrees();
                    w.pitch = (-to[2]).atan2(to[0].hypot(to[1])).to_degrees();
                    pin(&mut w, [eye[0], eye[1], eye[2] - 22.0]); // view_ofs '0 0 22'
                    let (rng, beams) = (w.prng, w.beams.clone());
                    let (with_bolt, _) = step_walk(&mut w, 0.0, false, &crate::vid::mode_vid(640, 400));
                    w.prng = rng;
                    w.beams.clear();
                    let (without_bolt, _) = step_walk(&mut w, 0.0, false, &crate::vid::mode_vid(640, 400));
                    w.beams = beams;
                    let diff = with_bolt.pixels.iter().zip(&without_bolt.pixels).filter(|(a, b)| a != b).count();
                    assert!(diff > 2000, "the bolt across the pit changes {diff} pixels");
                    // QUAKE_DUMP_DIR=<dir>: keep the frame for eyeballing.
                    if let Ok(dir) = std::env::var("QUAKE_DUMP_DIR") {
                        dump_pgm(&format!("{dir}/chthon-lightning.pgm"), &with_bolt);
                    }
                    pin(&mut w, keep.0);
                    (w.yaw, w.pitch) = (keep.1, keep.2);
                }
            }
            // lightning_fire re-thinks every 0.1 s until lightning_end (1 s):
            // ten refreshes, each good for 0.2 s (CL_ParseBeam's endtime).
            assert!(live_frames >= 10, "shock {shock}: the bolt was live on {live_frames} frames");
            assert!(
                matches!(w.model_cache.get("progs/bolt3.mdl"), Some(Some(_))),
                "TE_LIGHTNING3 loaded progs/bolt3.mdl"
            );
            let powered = pending_starts().iter().any(|(e, _)| e.sample == "misc/power.wav" && e.entity == event);
            assert!(powered, "shock {shock}: misc/power.wav played from event_lightning");
            assert_eq!(
                w.server.vm.ent_get_float(boss, "health"),
                health_before - 1.0,
                "shock {shock}: lightning_use took one health"
            );
            // lightning_end: door_go_down brings the electrodes back up.
            for _ in 0..100 {
                if electrodes.iter().all(|&d| state(&w, d) == STATE_BOTTOM) {
                    break;
                }
                frame(&mut w);
            }
        }
        // boss_shockc -> boss_death1..10: lava splash, the kill, SUB_UseTargets,
        // remove(self).
        for _ in 0..30 {
            if !live(&w, boss) {
                break;
            }
            frame(&mut w);
        }
        assert!(!live(&w, boss), "Chthon died and was removed");
        assert_eq!(w.server.vm.gget_float("killed_monsters"), 1.0, "the kill counted");

        // The finale path: the exit (changelevel_touch -> execute_changelevel's
        // svc_intermission), then a button after intermission_exittime
        // (ExitIntermission on e1m7: svc_finale + Chthon's epitaph).
        let exit = find_all(&w, "classname", "trigger_changelevel")[0];
        let (xmin, xmax) =
            (w.server.vm.ent_get_vector(exit, "absmin"), w.server.vm.ent_get_vector(exit, "absmax"));
        pin(&mut w, std::array::from_fn(|i| 0.5 * (xmin[i] + xmax[i])));
        for _ in 0..10 {
            frame(&mut w);
            if w.intermission != 0 {
                break;
            }
        }
        assert_eq!(w.intermission, 1, "svc_intermission: the level-complete screen");
        for _ in 0..60 {
            frame(&mut w); // intermission_exittime = time + 5
        }
        w.in_attack = true;
        frame(&mut w);
        w.in_attack = false;
        frame(&mut w);
        assert_eq!(w.intermission, 2, "svc_finale: the episode-end text");
        assert!(w.finale_text.contains("Chthon"), "Chthon's epitaph: {:?}", w.finale_text);
    }
}

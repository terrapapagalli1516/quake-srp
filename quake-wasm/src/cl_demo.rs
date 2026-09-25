//! The demo client frame for the page — [`step_demo`] runs the client's
//! [`demo_frame`] (quake-rs `client::cl_demo`: a recorded `.dem` rendered like
//! live play) on the page's [`Vid`](crate::vid::vid) and hands its sound calls
//! to [`snd_dma`](crate::snd_dma). Its tests drive the real client on
//! synthetic and on id's recorded demos.

use quake_rs::client::cl_demo::demo_frame;
use quake_rs::render;

use crate::app::DemoPlay;

/// One frame of demo playback at `render_w x render_h`: the finished screen
/// and its colour shifts (`cl.cshifts`, applied by the host after the menu and
/// console); the frame's sound calls are carried out.
pub(crate) fn step_demo(
    d: &mut DemoPlay,
    dt: f32,
    menu_up: bool,
    render_w: usize,
    render_h: usize,
) -> (render::Image, Vec<([u8; 3], f32)>) {
    let frame = demo_frame(d, dt, menu_up, &crate::vid::vid(render_w, render_h));
    crate::snd_dma::play(&d.pak, frame.sound);
    (frame.image, frame.cshifts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quake_rs::client::cl_tent::rocket_trail_type;
    use quake_rs::client::view::V_KICKTIME;
    use quake_rs::tent::BeamModel;
    use quake_rs::demo::parse_demo;
    use quake_rs::mdl::Mdl;
    use quake_rs::particles::Lcg;
    use quake_rs::server::{SoundEvent, TempEntityEvent};

    use crate::app::{build_demo, pak};
    use crate::snd_dma::{
        poll_sound, set_audio_ready, sound_channel, sound_entity, sound_is_view_entity,
        sound_volume, SND_QUEUE,
    };
    use crate::test_util::*;
    use crate::vid::{DEFAULT_H, DEFAULT_W};
    use quake_rs::client::view::V_KICKPITCH;

    #[test]
    fn step_demo_shows_the_last_frame_before_looping() {
        // FIX-7: the wrap must be DEFERRED so frames[n-1] is rendered for one
        // step before looping back to frame 0. The old code reset to 0 the
        // instant `idx` reached n-1, so the final frame was never displayed.
        use quake_rs::demo::{Demo, DemoFrame};

        let frame = |t: f32| DemoFrame { time: t, ..Default::default() };
        let demo = Demo {
            level_name: "test".into(),
            static_sounds: Vec::new(),
            // map_name() reads model_precache[1]; unused by step_demo's indexing.
            model_precache: vec![String::new(), "maps/test.bsp".into()],
            sound_precache: Vec::new(),
            viewentity: 0,
            // Three frames at t = 0, 1, 2.
            frames: vec![frame(0.0), frame(1.0), frame(2.0)],
        };
        let mut d = DemoPlay::new(build_test_pak(&[]), render::demo_room(), [[0u8; 3]; 256], demo);
        d.prng = Lcg::new(1);
        let n = d.demo.frames.len();

        // Drive several 1.0s steps and record which frame index is RENDERED
        // (i.e. the value of `idx` chosen by step_demo for that frame).
        let mut shown = Vec::new();
        for _ in 0..5 {
            let _img = step_demo(&mut d, 1.0, false, DEFAULT_W, DEFAULT_H);
            shown.push(d.idx);
        }

        // The last frame (index n-1) must appear in the shown sequence, and it
        // must be displayed BEFORE the wrap-back-to-0 that follows it.
        let last = n - 1;
        let pos = shown
            .iter()
            .position(|&i| i == last)
            .expect("the final frame index must be rendered at least once");
        assert_eq!(
            shown.get(pos + 1).copied(),
            Some(0),
            "after the last frame is shown, the very next step wraps to frame 0; shown={shown:?}"
        );
        // Concretely: 1.0s steps over t={0,1,2} render [1, 2, 0, 1, 2] — frame 2
        // (the last) is shown, then it loops to 0.
        assert_eq!(shown, vec![1, 2, 0, 1, 2], "deferred-wrap playback order");
    }

    #[test]
    fn step_demo_spawns_recorded_effects_into_the_particle_pool() {
        // A frame carrying an svc_particle burst + a TE_EXPLOSION temp entity
        // must fill the live particle pool when playback advances onto it, and
        // must NOT re-spawn while the same frame lingers, and must reset on wrap.
        use quake_rs::demo::{Demo, DemoFrame};
        use quake_rs::server::{te_consts, ParticleBurst, TempEntityEvent};

        let plain = |t: f32| DemoFrame { time: t, ..Default::default() };
        // Frame 1 (t=0.05) carries the effects; frames 0 and 2 are empty. Frame
        // times are one ~Quake tick apart so a 0.05s step advances exactly one
        // frame and the explosion's ramp ages by a realistic amount (not all the
        // way through its 8-frame life in a single huge step).
        let effect_frame = DemoFrame {
            time: 0.05,
            view_origin: [0.0, 0.0, 0.0],
            view_entity_origin: [0.0, 0.0, 0.0],
            view_angles: [0.0, 0.0, 0.0],
            entities: Vec::new(),
            particles: vec![ParticleBurst {
                org: [0.0, 0.0, 0.0],
                dir: [0.0, 0.0, 0.0],
                color: 73,
                count: 20,
            }],
            temp_entities: vec![TempEntityEvent {
                te_type: te_consts::TE_EXPLOSION,
                pos: [10.0, 0.0, 0.0],
                end: [10.0, 0.0, 0.0],
                entity: 0,
                color_start: 0,
                color_length: 0,
            }],
            ..Default::default()
        };
        let demo = Demo {
            level_name: "test".into(),
            static_sounds: Vec::new(),
            model_precache: vec![String::new(), "maps/test.bsp".into()],
            sound_precache: Vec::new(),
            viewentity: 0,
            frames: vec![plain(0.0), effect_frame, plain(0.10)],
        };
        let mut d = DemoPlay::new(build_test_pak(&[]), render::demo_room(), [[0u8; 3]; 256], demo);
        d.prng = Lcg::new(1);

        // Step 0.05s: lands on frame 1 (the effect frame). The burst (20) +
        // explosion (1024) particles populate the pool; after one tick of aging
        // the bulk of the 1024-particle explosion is still alive.
        let _ = step_demo(&mut d, 0.05, false, DEFAULT_W, DEFAULT_H);
        assert_eq!(d.idx, 1, "advanced onto the effect frame");
        let after_first = d.particles.len();
        assert!(
            after_first > 500,
            "the burst + 1024-particle explosion populate the pool (got {after_first})"
        );

        // A tiny step that holds us on frame 1 must NOT re-spawn the explosion
        // (the pool only shrinks as particles age — it never jumps back up).
        let _ = step_demo(&mut d, 0.001, false, DEFAULT_W, DEFAULT_H);
        assert_eq!(d.idx, 1, "still on the effect frame");
        assert!(
            d.particles.len() <= after_first,
            "no double-spawn: pool did not grow while the frame lingered"
        );

        // Drive 0.05s steps until playback wraps back to frame 0. After landing
        // on the last frame the very NEXT step wraps (deferred-wrap, as the
        // dedicated test above verifies); the wrap resets the pool, and frame 0
        // carries no effects, so the pool is empty afterwards.
        let mut wrapped = false;
        for _ in 0..6 {
            let _ = step_demo(&mut d, 0.05, false, DEFAULT_W, DEFAULT_H);
            if d.idx == 0 {
                wrapped = true;
                break;
            }
        }
        assert!(wrapped, "playback looped back to the first frame within a cycle");
        assert_eq!(d.idx, 0, "looped back to the first frame");
        assert!(
            d.particles.is_empty(),
            "wrap reset the particle pool (no stale explosion across the loop)"
        );
    }

    #[test]
    fn demo_playback_replays_recorded_lightning_beams() {
        // The DEMO path: a synthetic frame carrying a recorded TE_LIGHTNING1
        // must refresh the beam store when playback advances onto it, expand
        // into bolt.mdl pieces on render, and clear on the loop wrap.
        use quake_rs::demo::{Demo, DemoFrame};
        use quake_rs::server::te_consts;

        let plain = |t: f32| DemoFrame {
            time: t,
            view_origin: [0.0, 0.0, 0.0],
            view_entity_origin: [0.0, 0.0, 0.0],
            view_angles: [0.0, 0.0, 0.0],
            entities: Vec::new(),
            particles: Vec::new(),
            temp_entities: Vec::new(),
            ..Default::default()
        };
        let mut bolt_frame = plain(0.05);
        bolt_frame.temp_entities = vec![TempEntityEvent {
            te_type: te_consts::TE_LIGHTNING1,
            pos: [0.0, 0.0, 0.0],
            end: [75.0, 0.0, 0.0],
            entity: 9,
            color_start: 0,
            color_length: 0,
        }];
        // The real bolt model from the embedded pak, at precache index 2 (the
        // .dem signon precaches progs/bolt.mdl; index 1 is the world).
        let bolt_mdl = pak()
            .and_then(|p| p.read_file("progs/bolt.mdl").ok().flatten())
            .and_then(|b| Mdl::parse(&b).ok())
            .expect("progs/bolt.mdl parses from the embedded pak");
        let demo = Demo {
            level_name: "test".into(),
            model_precache: vec![
                String::new(),
                "maps/test.bsp".into(),
                "progs/bolt.mdl".into(),
            ],
            sound_precache: Vec::new(),
            viewentity: 1,
            static_sounds: Vec::new(),
            frames: vec![plain(0.0), bolt_frame, plain(0.10)],
        };
        let mut d = DemoPlay::new(build_test_pak(&[]), render::demo_room(), [[0u8; 3]; 256], demo);
        d.models = vec![None, None, Some(bolt_mdl)];
        d.sprites = vec![None, None, None];
        d.colors = vec![[200; 3]; 3];
        d.prng = Lcg::new(1);

        // Advance onto the bolt frame: the recorded beam lands in the store and
        // the render expands it (75 units => 3 pieces at 0/30/60 along +x).
        let _ = step_demo(&mut d, 0.05, false, 160, 100);
        assert_eq!(d.idx, 1, "advanced onto the bolt frame");
        assert!(d.beams.any_live(0.05), "recorded TE_LIGHTNING1 refreshed a beam");
        assert_eq!(d.beam_scratch.len(), 3, "75 units expand to 3 pieces");
        assert!(d.beam_scratch.iter().all(|s| s.model == BeamModel::Bolt));

        // A lingering step does NOT re-parse (last_spawned_idx guard) but the
        // beam stays live until its 0.2 s endtime.
        let _ = step_demo(&mut d, 0.001, false, 160, 100);
        assert!(d.beams.any_live(0.05));

        // Advance to the LAST frame: at t=0.10 the beam (endtime 0.25) still
        // rides across frames — it is a client effect, not a per-frame one.
        let mut guard = 0;
        while d.idx != 2 {
            let _ = step_demo(&mut d, 0.05, false, 160, 100);
            guard += 1;
            assert!(guard < 10, "playback reaches the last frame");
        }
        assert!(
            d.beams.any_live(d.demo.frames[2].time),
            "beam still live on the last frame (t=0.10 < endtime 0.25)"
        );
        // The NEXT (tiny) step triggers the deferred loop wrap: back to frame 0
        // with the beam store cleared (no stale bolts carried into the replay;
        // the tiny dt keeps playback ON frame 0, before the bolt re-spawns).
        let _ = step_demo(&mut d, 0.001, false, 160, 100);
        assert_eq!(d.idx, 0, "playback wrapped");
        assert!(!d.beams.any_live(0.0), "the wrap cleared the beam store");
    }

    // =======================================================================

    // Demo parity: the recorded stream drives sound / sbar / viewmodel /
    // lightstyles / damage exactly like live play.
    // =======================================================================

    /// The REAL embedded demo1.dem decodes the full recorded stream the demo
    /// path previously discarded: hundreds of svc_sound one-shots, the
    /// clientdata stats (sbar source), the SU_WEAPON viewmodel index, the
    /// signon lightstyle table, and — via the signon gate — an in-world first
    /// frame (no void-camera intro).
    #[test]
    fn demo1_recorded_stream_carries_sounds_stats_styles_and_viewmodel() {
        let pak = pak().expect("embedded pak");
        let bytes = pak.read_file("demo1.dem").unwrap().expect("demo1.dem in pak");
        let demo = parse_demo(&bytes).expect("demo1 parses");

        // (a) recorded svc_sound events decoded: id's demo1 carries ~595
        // one-shots (gunshots, doors, monster barks). Assert a robust floor.
        let sounds: usize = demo.frames.iter().map(|f| f.sounds.len()).sum();
        assert!(sounds >= 500, "demo1 carries ~595 svc_sound events, got {sounds}");
        // Every event resolved its precache name (S_StartSound's sfx lookup).
        assert!(
            demo.frames.iter().flat_map(|f| &f.sounds).all(|s| !s.sample.is_empty()),
            "every recorded sound resolves a precache name"
        );

        // (d/e) clientdata: the sbar stats are present from the FIRST frame.
        let f0 = &demo.frames[0];
        assert_eq!(f0.client.health, 100, "fresh recorded player");
        assert_eq!(f0.client.ammo, 25);
        assert_eq!(f0.client.shells, 25);
        assert_eq!(f0.client.active_weapon, 1, "IT_SHOTGUN");
        assert_ne!(f0.client.items, 0, "recorded cl.items bits present");

        // (f) viewmodel: STAT_WEAPON resolves through the demo's precache to
        // the shotgun viewmodel.
        assert_eq!(
            demo.model_precache
                .get(f0.client.weapon_model.max(0) as usize)
                .map(|s| s.as_str()),
            Some("progs/v_shot.mdl"),
            "SU_WEAPON -> model_precache -> v_shot.mdl"
        );

        // (c) the recorded lightstyle table: style 0 = 'm' (the steady world)
        // plus the torch-flicker set from the signon.
        assert_eq!(f0.lightstyles.first().map(|s| s.as_str()), Some("m"));
        let nonempty = f0.lightstyles.iter().filter(|s| !s.is_empty()).count();
        assert!(nonempty >= 10, "signon carries the style table, got {nonempty}");

        // (i) no void-camera intro: the signon gate makes frame 0 in-world.
        assert!(f0.entities.len() > 10, "frame 0 renders the level's entities");
        assert_ne!(f0.view_origin, [0.0; 3], "frame 0 camera is in-world");

        // (j) the recorded SU_VELOCITY drives V_CalcBob: the run reaches real
        // ground speed (id's demo1 visibly bobs).
        let maxv = demo
            .frames
            .iter()
            .map(|f| (f.client.velocity[0].powi(2) + f.client.velocity[1].powi(2)).sqrt())
            .fold(0.0f32, f32::max);
        assert!(maxv > 200.0, "recorded velocity shows the player running ({maxv})");
    }

    /// (b) svc_stopsound census: id's shipped demos never send it — the
    /// (entity, channel) stop registry is protocol completeness, exercised by
    /// the synthetic decode test in quake-rs. If a future demo carries stops,
    /// the page's keyed-source registry honours them.
    #[test]
    fn id_demos_never_send_stopsound() {
        let pak = pak().expect("embedded pak");
        for name in ["demo1.dem", "demo2.dem", "demo3.dem"] {
            let bytes = pak.read_file(name).unwrap().expect("demo in pak");
            let demo = parse_demo(&bytes).expect("demo parses");
            let stops: usize = demo.frames.iter().map(|f| f.stop_sounds.len()).sum();
            assert_eq!(stops, 0, "{name} sends no svc_stopsound");
        }
    }

    /// A recorded svc_sound event queues through the SAME `queue_sounds` path
    /// live play uses — once per frame advance (the spawn guard), carrying its
    /// (entity, channel) override key for the page registry.
    #[test]
    fn step_demo_queues_recorded_sounds_through_the_live_path() {
        use quake_rs::demo::{Demo, DemoFrame};

        let plain = |t: f32| DemoFrame { time: t, ..Default::default() };
        let sound_frame = DemoFrame {
            time: 0.05,
            sounds: vec![SoundEvent {
                entity: 5,
                channel: 2,
                sound_index: 1,
                sample: "doors/x.wav".to_string(),
                origin: [64.0, 0.0, 0.0],
                volume: 0.5,
                attenuation: 1.0,
            }],
            ..Default::default()
        };
        let demo = Demo {
            level_name: "test".into(),
            static_sounds: Vec::new(),
            model_precache: vec![String::new(), "maps/test.bsp".into()],
            sound_precache: Vec::new(),
            viewentity: 1,
            frames: vec![plain(0.0), sound_frame, plain(0.10)],
        };
        let mut d = DemoPlay::new(build_test_pak(&[("sound/doors/x.wav", b"WAVE")]), render::demo_room(), [[0u8; 3]; 256], demo);
        d.prng = Lcg::new(1);

        reset_queue(); // clears SND_QUEUE + marks audio ready
        let _ = step_demo(&mut d, 0.05, false, 160, 100);
        assert_eq!(d.idx, 1, "advanced onto the sound frame");
        assert_eq!(
            SND_QUEUE.with(|q| q.borrow().len()),
            1,
            "the recorded svc_sound queued exactly once"
        );
        // Lingering on the same frame must not re-queue it.
        let _ = step_demo(&mut d, 0.0001, false, 160, 100);
        assert_eq!(SND_QUEUE.with(|q| q.borrow().len()), 1, "no re-queue while lingering");

        // The pop carries the spatial params + the (entity, channel) key.
        let len = poll_sound();
        assert!(len > 0, "WAV bytes loaded from the pak");
        assert_eq!(sound_volume(), 0.5);
        assert_eq!(sound_entity(), 5, "override key entity");
        assert_eq!(sound_channel(), 2, "override key channel");
        assert_eq!(
            sound_is_view_entity(),
            0,
            "entity 5 is not the recorded view entity (1)"
        );
        SND_QUEUE.with(|q| q.borrow_mut().clear());
        set_audio_ready(0);
    }

    /// A recorded svc_damage drives the SAME flash + view-kick math live play
    /// uses (V_ParseDamage): the deferred cshifts returned by step_demo carry
    /// the red flash, and the kick state arms + decays.
    #[test]
    fn step_demo_damage_event_drives_flash_and_kick() {
        use quake_rs::demo::{DamageEvent, Demo, DemoFrame};

        let plain = |t: f32| DemoFrame { time: t, ..Default::default() };
        let dmg_frame = DemoFrame {
            time: 0.05,
            // Attack from straight ahead (+x of a yaw-0 view at the origin).
            damage: vec![DamageEvent { armor: 0, blood: 20, from: [128.0, 0.0, 0.0] }],
            ..Default::default()
        };
        let demo = Demo {
            level_name: "test".into(),
            static_sounds: Vec::new(),
            model_precache: vec![String::new(), "maps/test.bsp".into()],
            sound_precache: Vec::new(),
            viewentity: 0,
            // Trailing frames keep the fade-out steps below from wrapping the
            // loop (a wrap re-spawns the damage frame's events).
            frames: vec![plain(0.0), dmg_frame, plain(0.10), plain(1.0), plain(2.0)],
        };
        let mut d = DemoPlay::new(build_test_pak(&[]), render::demo_room(), [[0u8; 3]; 256], demo);
        d.damage_color = [0, 0, 0];
        d.prng = Lcg::new(1);

        let (_img, cshifts) = step_demo(&mut d, 0.05, false, 160, 100);
        assert_eq!(d.idx, 1, "advanced onto the damage frame");
        // count = max(10, blood*0.5) = 10 -> percent 30, faded by 0.05*150 =
        // 7.5 within the same step (V_UpdatePalette) -> 22 (the C's int).
        assert!(
            d.damage_blend == 22.0,
            "V_ParseDamage percent 3*count then dt*150 fade, got {}",
            d.damage_blend
        );
        assert_eq!(d.damage_color, [255, 0, 0], "pure-blood red tint");
        assert_eq!(
            cshifts,
            vec![([255, 0, 0], d.damage_blend)],
            "the deferred cshifts carry the flash to the dispatcher"
        );
        // The directional kick armed (forward hit -> pitch kick, no roll) and
        // already decayed one step (v_dmg_time -= host_frametime).
        assert!(
            (d.v_dmg_time - (V_KICKTIME - 0.05)).abs() < 1e-3,
            "kick timer armed then decayed by dt"
        );
        assert!(d.v_dmg_roll.abs() < 1e-3, "head-on hit has no roll component");
        assert!(
            (d.v_dmg_pitch - 10.0 * V_KICKPITCH).abs() < 1e-3,
            "pitch kick = count * dot(from, forward) * v_kickpitch"
        );

        // The flash fades out over the following steps and the blend clears.
        for _ in 0..4 {
            let _ = step_demo(&mut d, 0.05, false, 160, 100);
        }
        assert_eq!(d.damage_blend, 0.0, "flash fully faded");
    }

    /// CENSUS F14: entity skins come from U_SKIN, else the baseline's skin
    /// (CL_ParseUpdate); yellow armour is armor.mdl skin 1 — demo2 and demo3
    /// show one (demo1's stays out of sight).
    #[test]
    fn demo_skins_come_from_the_stream() {
        let pak = pak().expect("pak");
        for name in ["demo2.dem", "demo3.dem"] {
            let demo = parse_demo(&pak.read_file(name).unwrap().unwrap()).unwrap();
            let armor = demo.model_precache.iter().position(|m| m == "progs/armor.mdl");
            let armor = armor.unwrap_or_else(|| panic!("{name} precaches armor.mdl"));
            let yellow = demo
                .frames
                .iter()
                .flat_map(|f| &f.entities)
                .any(|e| e.modelindex == armor && e.skin == 1);
            assert!(yellow, "{name} draws yellow armour (armor.mdl skin 1)");
        }
    }

    /// CENSUS F13: CL_RelinkEntities runs R_RocketTrail for model-flag trails in
    /// playback exactly as live: a recorded missile (progs/missile.mdl,
    /// EF_ROCKET) trails fire from its previous origin, one particle per 3
    /// units; its first sighting draws none, and a static never trails.
    #[test]
    fn step_demo_rocket_trails_from_the_previous_origin() {
        use quake_rs::demo::{Demo, DemoFrame, EntSnapshot};
        let missile = pak()
            .and_then(|p| p.read_file("progs/missile.mdl").ok().flatten())
            .and_then(|b| Mdl::parse(&b).ok())
            .expect("progs/missile.mdl parses");
        assert_ne!(rocket_trail_type(missile.header.flags), None, "missile.mdl carries EF_ROCKET");
        let ent = |num: i32, x: f32| EntSnapshot {
            num,
            modelindex: 2,
            frame: 0,
            skin: 0,
            origin: [x, 0.0, 0.0],
            angles: [0.0; 3],
            effects: 0,
        };
        let frame = |t: f32, x: f32| DemoFrame {
            time: t,
            entities: vec![ent(7, x), ent(-1, 500.0)],
            ..Default::default()
        };
        let demo = Demo {
            level_name: "test".into(),
            static_sounds: Vec::new(),
            model_precache: vec![String::new(), "maps/test.bsp".into(), "progs/missile.mdl".into()],
            sound_precache: Vec::new(),
            viewentity: 0,
            frames: vec![frame(0.0, 0.0), frame(0.05, 0.0), frame(0.10, 30.0), frame(1.0, 30.0)],
        };
        let mut d = DemoPlay::new(build_test_pak(&[]), render::demo_room(), [[0u8; 3]; 256], demo);
        d.models = vec![None, None, Some(missile)];
        let _ = step_demo(&mut d, 0.05, false, 160, 100);
        assert_eq!(d.idx, 1);
        assert_eq!(d.particles.len(), 0, "first sighting: no trail");
        let _ = step_demo(&mut d, 0.05, false, 160, 100);
        assert_eq!(d.idx, 2);
        assert_eq!(d.particles.len(), 10, "30 units of rocket trail, one per 3");
        // Along x = 0..30 (type 0 jitters each particle by rand()%6 - 3), far
        // from the static at x = 500.
        assert!(d.particles.particles().iter().all(|p| p.origin[0] > -4.0 && p.origin[0] < 34.0));
    }

    /// CENSUS F6: a recorded `svc_stufftext "bf"` runs V_BonusFlash_f — the
    /// gold cshift at 50%, dropped dt*100 per frame — and id's demo1 carries
    /// such pickups.
    #[test]
    fn step_demo_stufftext_bf_flashes_gold() {
        use quake_rs::demo::{Demo, DemoFrame};
        let plain = |t: f32| DemoFrame { time: t, ..Default::default() };
        let bf = DemoFrame { time: 0.05, stufftext: vec!["bf\n".into()], ..Default::default() };
        let demo = Demo {
            level_name: "test".into(),
            static_sounds: Vec::new(),
            model_precache: vec![String::new(), "maps/test.bsp".into()],
            sound_precache: Vec::new(),
            viewentity: 0,
            frames: vec![plain(0.0), bf, plain(0.10), plain(1.0)],
        };
        let mut d = DemoPlay::new(build_test_pak(&[]), render::demo_room(), [[0u8; 3]; 256], demo);
        let (_img, cshifts) = step_demo(&mut d, 0.05, false, 160, 100);
        assert!((d.bonus_blend - (50.0 - 0.05 * 100.0)).abs() < 1e-3, "{}", d.bonus_blend);
        assert_eq!(cshifts, vec![(quake_rs::client::view::BONUS_COLOR, d.bonus_blend)], "the bonus cshift");

        let pak = pak().expect("pak");
        let real = parse_demo(&pak.read_file("demo1.dem").unwrap().unwrap()).unwrap();
        let n = real.frames.iter().flat_map(|f| &f.stufftext).filter(|t| t.as_str() == "bf\n").count();
        assert!(n > 0, "demo1 stuffs bf on its pickups");
    }

    /// The real boot demo draws the recorded status bar (sbar pixels differ
    /// from the bare scene) and resolves the recorded viewmodel + lightstyles.
    #[test]
    fn build_demo_resolves_viewmodel_hud_and_recorded_styles() {
        let mut d = build_demo().expect("the embedded demo boots");

        // The recorded SU_WEAPON viewmodel parsed (v_shot.mdl).
        let f0 = &d.demo.frames[0];
        let wm = f0.client.weapon_model.max(0) as usize;
        assert!(
            matches!(d.models.get(wm), Some(Some(_))),
            "the recorded viewmodel's Mdl parsed from the pak"
        );
        // The recorded style table reaches the renderer's scale law: style 0
        // is the steady 'm' world (264/256, what the seeded default used to
        // hardcode) and the flicker styles are present.
        let scales = quake_rs::server::lightstyle_scales_at(&f0.lightstyles, f0.time);
        assert!((scales[0] - 264.0 / 256.0).abs() < 1e-6, "style 0 'm'");
        assert!(d.gfx_wad.is_some(), "sbar pics available for the demo HUD");

        // Status bar A/B: one step with the wad, then re-render the SAME frame
        // without it (dt == 0 holds the frame) — the sbar region must differ.
        let (with_hud, _) = step_demo(&mut d, 0.016, false, 320, 200);
        d.gfx_wad = None;
        let (without, _) = step_demo(&mut d, 0.0, false, 320, 200);
        assert_eq!(with_hud.rgb.len(), without.rgb.len());
        // Quake's sbar is the bottom 24 rows of the 320x200 virtual screen.
        let bar_rows = 24usize;
        let diff = (0..320 * bar_rows)
            .filter(|i| {
                let a = with_hud.rgb[(200 - bar_rows) * 320 + i];
                let b = without.rgb[(200 - bar_rows) * 320 + i];
                a != b
            })
            .count();
        assert!(diff > 500, "the drawn sbar changes the bar region ({diff} px)");
    }

    /// The loop wrap is seam-clean on the REAL demo: fast-forward to the last
    /// frame, take one more step, and the playback lands back on frame 0 —
    /// which, thanks to the parser's signon gate, is an IN-WORLD frame (the
    /// old stream emitted ~1.2 s of void-camera signon frames here).
    #[test]
    fn demo_loop_wrap_lands_on_the_in_world_first_frame() {
        let mut d = build_demo().expect("the embedded demo boots");
        let n = d.demo.frames.len();
        let _ = step_demo(&mut d, 1.0e6, false, 160, 100);
        assert_eq!(d.idx, n - 1, "fast-forwarded to the last frame");
        let (img, _) = step_demo(&mut d, 0.05, false, 160, 100);
        assert_eq!(d.idx, 0, "the wrap landed back on frame 0");
        assert!(
            !d.demo.frames[0].entities.is_empty(),
            "frame 0 is the post-signon in-world frame"
        );
        let lit = img
            .rgb
            .iter()
            .filter(|p| p[0] != 0 || p[1] != 0 || p[2] != 0)
            .count();
        assert!(
            lit * 2 > img.rgb.len(),
            "the post-wrap frame renders a real scene ({lit}/{} lit)",
            img.rgb.len()
        );
    }

    /// R_DrawParticles' `grav = frametime * sv_gravity.value * 0.05` reads the
    /// client's own cvar in playback too (it was a constant 800 here): at the
    /// default a recorded svc_particle puff (pt_slowgrav, dir 0) leaves its
    /// first 0.05 s frame at vz -2; after e1m8 (worldspawn sets 100, and the
    /// cvar outlives the map) at -0.25.
    #[test]
    fn demo_particles_fall_by_the_sv_gravity_cvar() {
        use quake_rs::demo::{Demo, DemoFrame};
        use quake_rs::server::ParticleBurst;
        let vz = || {
            let plain = |t: f32| DemoFrame { time: t, ..Default::default() };
            let puff = DemoFrame {
                time: 0.05,
                particles: vec![ParticleBurst { org: [0.0; 3], dir: [0.0; 3], color: 73, count: 20 }],
                ..Default::default()
            };
            let demo = Demo {
                level_name: "test".into(),
                static_sounds: Vec::new(),
                model_precache: vec![String::new(), "maps/test.bsp".into()],
                sound_precache: Vec::new(),
                viewentity: 0,
                frames: vec![plain(0.0), puff, plain(0.10), plain(0.15)],
            };
            let mut d = DemoPlay::new(build_test_pak(&[]), render::demo_room(), [[0u8; 3]; 256], demo);
            let _ = step_demo(&mut d, 0.05, false, 64, 40);
            assert_eq!(d.idx, 1);
            let v: Vec<f32> = d.particles.particles().iter().map(|p| p.velocity[2]).collect();
            assert!(!v.is_empty() && v.iter().all(|&z| z == v[0]), "{v:?}");
            v[0]
        };
        let _ = crate::app::build_walk_map("maps/e1m1.bsp").expect("e1m1"); // sv_gravity 800
        assert_eq!(vz(), -800.0 * 0.05 * 0.05);
        let _ = crate::app::build_walk_map("maps/e1m8.bsp").expect("e1m8"); // sv_gravity 100
        assert_eq!(vz(), -100.0 * 0.05 * 0.05);
        let _ = crate::app::build_walk_map("maps/e1m1.bsp");
    }

    /// CENSUS F18 on the recorded stream: C's demo playback parses the signon
    /// and frame 0's block in one CL_ReadFromServer, before the first
    /// CL_LerpPoint, so the recorded player's weapons are stamped at about
    /// host_frametime and nothing flashes when demo1 starts (or, here, when it
    /// loops). A weapon got later in the recording still flashes.
    #[test]
    fn demo_start_does_not_flash_the_recorded_weapons() {
        let mut d = build_demo().expect("the embedded demo boots");
        let items0 = d.demo.frames[0].client.items;
        assert_ne!(items0 & 1, 0, "demo1's player carries the shotgun");
        let unflashed = |d: &DemoPlay| d.item_gettime.iter().all(|&t| t == 0.0);
        assert_eq!(d.cl_items, items0);
        let _ = step_demo(&mut d, 0.05, false, 160, 100);
        assert!(unflashed(&d), "playback start");
        let _ = step_demo(&mut d, 1.0e6, false, 160, 100);
        let _ = step_demo(&mut d, 0.05, false, 160, 100);
        assert_eq!(d.idx, 0, "wrapped");
        assert_eq!(d.cl_items, items0);
        assert!(unflashed(&d), "the loop wrap");
        // A bit the recording gains later is stamped on its frame's clock.
        let got = d.demo.frames.iter().position(|f| f.client.items & !items0 != 0);
        if let Some(i) = got {
            let dt = d.demo.frames[i].time - d.demo.frames[0].time;
            let _ = step_demo(&mut d, dt, false, 160, 100);
            assert!(!unflashed(&d), "frame {i}'s new item is stamped");
        }
    }
}

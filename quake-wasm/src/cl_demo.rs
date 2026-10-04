//! cl_demo.c's host half for the page. [`step_demo`] runs the client's
//! [`demo_frame`] (quake-rs `client::cl_demo`: a recorded `.dem` rendered like
//! live play) on the page's [`Vid`](crate::vid::vid) and hands its sound calls
//! to [`snd_dma`](crate::snd_dma); [`step_timedemo`] does the same for a
//! `timedemo` frame. The rest is `cls`'s demo state against the App:
//! `CL_PlayDemo_f`, `CL_TimeDemo_f`, `CL_StopPlayback`/`CL_FinishTimeDemo`,
//! `CL_Disconnect`, `CL_NextDemo` and `Host_EndGame` (a demo's
//! `svc_disconnect`: the loop's next demo, or disconnected). Its tests drive
//! the real client on synthetic and on id's recorded demos.

use quake_rs::client::cl_demo::{default_extension, demo_frame, demo_frame_undrawn, timedemo_frame, MAX_DEMOS};
use quake_rs::client::{SoundCall, Vid};
use quake_rs::render;

use crate::app::{build_demo_file, App, DemoPlay, APP};

/// One frame of demo playback at `render_w x render_h`: the finished screen
/// (empty when not `draw`n: a frame-rate cap's) and its colour shifts
/// (`cl.cshifts`, applied by the host after the menu and console); the
/// frame's sound calls are carried out.
pub(crate) fn step_demo(d: &mut DemoPlay, dt: f32, menu_up: bool, vid: &Vid, draw: bool) -> (render::Image, Vec<([u8; 3], f32)>) {
    let frame = if draw { demo_frame(d, dt, menu_up, vid) } else { demo_frame_undrawn(d, dt, menu_up, vid) };
    crate::snd_dma::play(&d.pak, frame.sound);
    (frame.image, frame.cshifts)
}

/// A finished screen and its colour shifts (`cl.cshifts`).
type ShiftedFrame = (render::Image, Vec<([u8; 3], f32)>);

/// One host frame of `timedemo` on `vid` ([`timedemo_frame`]: the next
/// recorded message, drawn), or `None` when the demo has ended.
pub(crate) fn step_timedemo(d: &mut DemoPlay, frametime: f32, menu_up: bool, vid: &Vid) -> Option<ShiftedFrame> {
    let frame = timedemo_frame(d, frametime, menu_up, vid)?;
    crate::snd_dma::play(&d.pak, frame.sound);
    Some((frame.image, frame.cshifts))
}

/// `S_StopAllSounds (true)`: every sound, the loops and ambients included.
fn stop_all_sounds() {
    if let Some(pak) = crate::common::pak() {
        crate::snd_dma::play(&pak, vec![SoundCall::StopAll]);
    }
}

/// `CL_FinishTimeDemo`: the timedemo is over; its line goes to the console.
fn cl_finish_timedemo(a: &mut App) {
    a.cls.timedemo = false;
    let line = a.cls.td.finish(a.host_framecount, a.realtime);
    a.console.println(line);
}

/// `CL_StopPlayback`: a playing demo stops (a timedemo prints its line).
pub(crate) fn cl_stop_playback(a: &mut App) {
    if !a.demoplayback() {
        return;
    }
    a.demo = None;
    if a.cls.timedemo {
        cl_finish_timedemo(a);
    }
}

/// `CL_Disconnect`: every sound stops, a demo stops playing or the local game
/// shuts down (`Host_ShutdownServer`), and the client is disconnected — the
/// console covers the screen until something plays again.
pub(crate) fn cl_disconnect(a: &mut App) {
    stop_all_sounds();
    cl_stop_playback(a);
    a.cls.timedemo = false;
    a.demo = None;
    if let Some(w) = a.walk.take() {
        a.sv_gravity = w.server.sv_gravity();
    }
    a.mode = 1;
    a.disconnected = true;
}

/// The page's half of `Host_Error` (host.c), once the game has ended in one
/// — a QuakeC runtime error ([`Walk::host_error`](quake_rs::client::Walk),
/// set by the engine's `client::host::host_error`, which printed the report
/// and stopped the sounds): the report into the console if the frame's text
/// has not been taken yet, `CL_Disconnect`, and `cls.demonum = -1` so no demo
/// loop starts; disconnected, the console covers the screen.
pub(crate) fn finish_host_error(a: &mut App) {
    let Some(w) = a.walk.as_mut().filter(|w| w.host_error.is_some()) else { return };
    let printed = w.notify.take_printed();
    if !printed.is_empty() {
        a.console.print_notified(&printed);
    }
    cl_disconnect(a);
    a.cls.demonum = -1;
}

/// The mission packs' re-release-only end-of-game credits roll
/// ([`quake_rs::client::Walk::pending_menu_credits`], set once builtin #79
/// `finaleFinished` is finally true and `finale_transition`/`finale_6` ran
/// `localcmd("menu_credits\n")`): end the session exactly as the Quit menu's
/// "Y" does ([`App::request_quit`]), which shows the same end screen
/// (`web/PLATFORM.md`'s "Quit" — id's `end2.bin`/`end1.bin`, or the plain
/// message without one) and disconnects — the `disconnect` QuakeC queues
/// right after `menu_credits` needs no separate handling, since
/// `request_quit` already disconnects. No confirmation prompt: the game
/// itself decided this is over, not the player asking to leave. `id1` never
/// sets the flag, so this never fires for it.
pub(crate) fn finish_menu_credits(a: &mut App) {
    let Some(w) = a.walk.as_mut().filter(|w| w.pending_menu_credits) else { return };
    w.pending_menu_credits = false;
    a.request_quit();
}

/// `CL_PlayDemo_f` after its argument check: disconnect, print
/// `"Playing demo from <name>."`, and start the demo — or print "ERROR:
/// couldn't open." and stop the demo loop (`cls.demonum = -1`), staying
/// disconnected. `timedemo` builds it for [`step_timedemo`]. True when it
/// plays.
pub(crate) fn cl_play_demo(a: &mut App, arg: &str, timedemo: bool) -> bool {
    cl_disconnect(a);
    let name = default_extension(arg, ".dem");
    a.console.println(format!("Playing demo from {name}."));
    let Some(mut d) = build_demo_file(&name, timedemo) else {
        a.console.println("ERROR: couldn't open.");
        a.cls.demonum = -1;
        return false;
    };
    d.viewsize = a.settings.cvars.viewsize;
    d.sv_gravity = a.sv_gravity;
    a.demo = Some(d);
    a.mode = 1;
    a.disconnected = false;
    // The demo's signon ends in SCR_EndLoadingPlaque's Con_ClearNotify:
    // nothing printed so far (this line, a timedemo's result) is a notify
    // line over it.
    let _ = a.console.take_unnotified();
    true
}

/// `CL_TimeDemo_f` after its argument check: `CL_PlayDemo_f`, then the
/// measurement starts in this host frame — the one the next `step` runs
/// (the C runs the command inside the frame it counts from). The port only
/// sets `cls.timedemo` when the demo opened: id's sets it regardless, which
/// just leaves a failed `timedemo` running uncapped until the next disconnect.
pub(crate) fn cl_timedemo(a: &mut App, arg: &str) {
    if !cl_play_demo(a, arg, true) {
        return;
    }
    a.cls.timedemo = true;
    let frame = a.host_framecount;
    a.cls.td.start(frame);
}

/// `CL_NextDemo`: the loop's next demo (`playdemo cls.demos[cls.demonum]`),
/// wrapping to the first after the last listed one; nothing when the loop is
/// off. (`SCR_BeginLoadingPlaque` stops every sound; its plaque: AUDIT.md.)
pub(crate) fn cl_next_demo(a: &mut App) {
    if a.cls.demonum == -1 {
        return; // don't play demos
    }
    stop_all_sounds();
    let n = a.cls.demonum;
    if n as usize >= MAX_DEMOS || a.cls.demos[n as usize].is_empty() {
        a.cls.demonum = 0;
        if a.cls.demos[0].is_empty() {
            a.console.println("No demos listed with startdemos");
            a.cls.demonum = -1;
            return;
        }
    }
    let name = a.cls.demos[a.cls.demonum as usize].clone();
    // Cbuf_InsertText ("playdemo ...") runs after the increment below.
    a.cls.demonum += 1;
    cl_play_demo(a, &name, false);
}

/// `Host_EndGame` for a demo that has played its last message
/// (`svc_disconnect`): the demo loop's next demo, or — outside the loop —
/// `CL_Disconnect`.
pub(crate) fn host_end_game(a: &mut App) {
    if a.cls.demonum != -1 {
        cl_next_demo(a);
    } else {
        cl_disconnect(a);
    }
}

/// `1` while a `timedemo` runs (`cls.timedemo`): the page then runs host
/// frames back to back, a time slice's worth per animation frame, instead of
/// one per refresh.
pub(crate) fn timedemo_running() -> i32 {
    APP.with(|c| c.borrow().as_ref().map(|a| a.cls.timedemo as i32).unwrap_or(0))
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

    use crate::app::build_demo;
    use crate::common::pak;
    use crate::snd_dma::pending_starts;
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
            forcetrack: -1,
            cdtrack: None,
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
            let _img = step_demo(&mut d, 1.0, false, &crate::vid::mode_vid(DEFAULT_W, DEFAULT_H), true);
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
            forcetrack: -1,
            cdtrack: None,
            frames: vec![plain(0.0), effect_frame, plain(0.10)],
        };
        let mut d = DemoPlay::new(build_test_pak(&[]), render::demo_room(), [[0u8; 3]; 256], demo);
        d.prng = Lcg::new(1);

        // Step 0.05s: lands on frame 1 (the effect frame). The burst (20) +
        // explosion (1024) particles populate the pool; after one tick of aging
        // the bulk of the 1024-particle explosion is still alive.
        let _ = step_demo(&mut d, 0.05, false, &crate::vid::mode_vid(DEFAULT_W, DEFAULT_H), true);
        assert_eq!(d.idx, 1, "advanced onto the effect frame");
        let after_first = d.particles.len();
        assert!(
            after_first > 500,
            "the burst + 1024-particle explosion populate the pool (got {after_first})"
        );

        // A step that holds the clock on frame 1 (any later clock reads the
        // next message: CL_GetMessage) must NOT re-spawn the explosion (the
        // pool only shrinks as particles age — it never jumps back up).
        let _ = step_demo(&mut d, 0.0, false, &crate::vid::mode_vid(DEFAULT_W, DEFAULT_H), true);
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
            let _ = step_demo(&mut d, 0.05, false, &crate::vid::mode_vid(DEFAULT_W, DEFAULT_H), true);
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
            forcetrack: -1,
            cdtrack: None,
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
        let _ = step_demo(&mut d, 0.05, false, &crate::vid::mode_vid(160, 100), true);
        assert_eq!(d.idx, 1, "advanced onto the bolt frame");
        assert!(d.beams.any_live(0.05), "recorded TE_LIGHTNING1 refreshed a beam");
        assert_eq!(d.beam_scratch.len(), 3, "75 units expand to 3 pieces");
        assert!(d.beam_scratch.iter().all(|s| s.model == BeamModel::Bolt));

        // A lingering step does NOT re-parse (last_spawned_idx guard) but the
        // beam stays live until its 0.2 s endtime.
        let _ = step_demo(&mut d, 0.001, false, &crate::vid::mode_vid(160, 100), true);
        assert!(d.beams.any_live(0.05));

        // Advance to the LAST frame: at t=0.10 the beam (endtime 0.25) still
        // rides across frames — it is a client effect, not a per-frame one.
        let mut guard = 0;
        while d.idx != 2 {
            let _ = step_demo(&mut d, 0.05, false, &crate::vid::mode_vid(160, 100), true);
            guard += 1;
            assert!(guard < 10, "playback reaches the last frame");
        }
        assert!(
            d.beams.any_live(d.demo.frames[2].time),
            "beam still live on the last frame (t=0.10 < endtime 0.25)"
        );
        // The step that takes the clock past the last message triggers the
        // deferred loop wrap: back to frame 0 with the beam store cleared (no
        // stale bolts carried into the replay; the wrap's frame reads only
        // frame 0, before the bolt re-spawns).
        let _ = step_demo(&mut d, 0.05, false, &crate::vid::mode_vid(160, 100), true);
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

    /// A recorded svc_sound starts through the SAME sound calls live play
    /// makes — once per frame advance (the spawn guard), with its volume and
    /// (entity, channel) override key.
    #[test]
    fn step_demo_starts_recorded_sounds_through_the_live_path() {
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
            forcetrack: -1,
            cdtrack: None,
            frames: vec![plain(0.0), sound_frame, plain(0.10)],
        };
        let mut d = DemoPlay::new(build_test_pak(&[("sound/doors/x.wav", b"WAVE")]), render::demo_room(), [[0u8; 3]; 256], demo);
        d.prng = Lcg::new(1);

        reset_queue();
        let _ = step_demo(&mut d, 0.05, false, &crate::vid::mode_vid(160, 100), true);
        assert_eq!(d.idx, 1, "advanced onto the sound frame");
        assert_eq!(pending_starts().len(), 1, "the recorded svc_sound started exactly once");
        // Lingering on the same frame must not start it again.
        let _ = step_demo(&mut d, 0.0001, false, &crate::vid::mode_vid(160, 100), true);
        let starts = pending_starts();
        assert_eq!(starts.len(), 1, "no second start while lingering");

        // S_StartSound's arguments: the volume, the (entity, channel) key,
        // and the recorded view entity (1), which entity 5 is not.
        let (e, view) = &starts[0];
        assert_eq!((e.volume, e.entity, e.channel, *view), (0.5, 5, 2, 1));
        assert_eq!(e.sample, "doors/x.wav");
        reset_queue();
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
            forcetrack: -1,
            cdtrack: None,
            // Trailing frames keep the fade-out steps below from wrapping the
            // loop (a wrap re-spawns the damage frame's events).
            frames: vec![plain(0.0), dmg_frame, plain(0.10), plain(1.0), plain(2.0)],
        };
        let mut d = DemoPlay::new(build_test_pak(&[]), render::demo_room(), [[0u8; 3]; 256], demo);
        d.damage_color = [0, 0, 0];
        d.prng = Lcg::new(1);

        let (_img, cshifts) = step_demo(&mut d, 0.05, false, &crate::vid::mode_vid(160, 100), true);
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
            let _ = step_demo(&mut d, 0.05, false, &crate::vid::mode_vid(160, 100), true);
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
        // Each message's update puts it at x (a forcelink: no lerp).
        let ent = |num: i32, x: f32| EntSnapshot {
            num,
            modelindex: 2,
            origin: [x, 0.0, 0.0],
            prev_origin: [x, 0.0, 0.0],
            forcelink: true,
            ..EntSnapshot::default()
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
            forcetrack: -1,
            cdtrack: None,
            frames: vec![frame(0.0, 0.0), frame(0.05, 0.0), frame(0.10, 30.0), frame(1.0, 30.0)],
        };
        let mut d = DemoPlay::new(build_test_pak(&[]), render::demo_room(), [[0u8; 3]; 256], demo);
        d.models = vec![None, None, Some(missile)];
        let _ = step_demo(&mut d, 0.05, false, &crate::vid::mode_vid(160, 100), true);
        assert_eq!(d.idx, 1);
        assert_eq!(d.particles.len(), 0, "first sighting: no trail");
        let _ = step_demo(&mut d, 0.05, false, &crate::vid::mode_vid(160, 100), true);
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
            forcetrack: -1,
            cdtrack: None,
            frames: vec![plain(0.0), bf, plain(0.10), plain(1.0)],
        };
        let mut d = DemoPlay::new(build_test_pak(&[]), render::demo_room(), [[0u8; 3]; 256], demo);
        let (_img, cshifts) = step_demo(&mut d, 0.05, false, &crate::vid::mode_vid(160, 100), true);
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
        let scales = quake_rs::server::lightstyle_scales_at(&f0.lightstyles, f64::from(f0.time), quake_rs::server::LerpLightStyles::Classic);
        assert!((scales[0] - 264.0 / 256.0).abs() < 1e-6, "style 0 'm'");
        assert!(d.gfx_wad.is_some(), "sbar pics available for the demo HUD");

        // Status bar A/B: one step with the wad, then re-render the SAME frame
        // without it (dt == 0 holds the frame) — the sbar region must differ.
        let (with_hud, _) = step_demo(&mut d, 0.016, false, &crate::vid::mode_vid(320, 200), true);
        d.gfx_wad = None;
        let (without, _) = step_demo(&mut d, 0.0, false, &crate::vid::mode_vid(320, 200), true);
        assert_eq!(with_hud.pixels.len(), without.pixels.len());
        // Quake's sbar is the bottom 24 rows of the 320x200 virtual screen.
        let bar_rows = 24usize;
        let diff = (0..320 * bar_rows)
            .filter(|i| {
                let a = with_hud.pixels[(200 - bar_rows) * 320 + i];
                let b = without.pixels[(200 - bar_rows) * 320 + i];
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
        let _ = step_demo(&mut d, 1.0e6, false, &crate::vid::mode_vid(160, 100), true);
        assert_eq!(d.idx, n - 1, "fast-forwarded to the last frame");
        let (img, _) = step_demo(&mut d, 0.05, false, &crate::vid::mode_vid(160, 100), true);
        assert_eq!(d.idx, 0, "the wrap landed back on frame 0");
        assert!(
            !d.demo.frames[0].entities.is_empty(),
            "frame 0 is the post-signon in-world frame"
        );
        let lit = img
            .pixels
            .iter()
            .filter(|&&p| p != 0)
            .count();
        assert!(
            lit * 2 > img.pixels.len(),
            "the post-wrap frame renders a real scene ({lit}/{} lit)",
            img.pixels.len()
        );
    }

    // -- cls: the demo loop and the demo commands -------------------------------

    use crate::app::APP;

    /// The console's scrollback, oldest first, without its blank lines (a
    /// line exactly con_linewidth long is followed by one, as in the C).
    fn console_lines() -> Vec<String> {
        APP.with(|c| {
            let b = c.borrow();
            b.as_ref().unwrap().console.lines().filter(|l| !l.is_empty()).map(str::to_string).collect()
        })
    }

    /// The demo playing (its map), or None; and `cls.demonum`.
    fn playing() -> (Option<String>, i32) {
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            let map = a.demo.as_ref().filter(|_| a.demoplayback()).map(|d| d.demo.map_name().unwrap_or("").to_string());
            (map, a.cls.demonum)
        })
    }

    /// The playing demo's last frame has been shown (the next `step` ends it).
    fn to_end() {
        APP.with(|c| {
            let mut b = c.borrow_mut();
            let d = b.as_mut().unwrap().demo.as_mut().unwrap();
            d.idx = d.demo.frames.len() - 1;
        })
    }

    #[test]
    fn the_attract_loop_is_quake_rcs_startdemos_through_cl_nextdemo() {
        use crate::app::boot_attract;
        use crate::console::{console_toggle, console_visible};
        use crate::host::step;
        // 320x200: con_linewidth stays 38, so no Con_CheckResize clears the
        // notify lines on the first frame.
        crate::vid::set_resolution(320, 200);
        assert_eq!(boot_attract(), 1);
        let lines = console_lines();
        assert_eq!(
            lines[lines.len() - 2..],
            ["3 demo(s) in loop", "Playing demo from demo1.dem."],
            "Host_Startdemos_f + CL_NextDemo -> CL_PlayDemo_f print as id's"
        );
        let (demo1, n) = playing();
        assert_eq!((demo1.as_deref(), n), (Some("maps/e1m3.bsp"), 1), "demo1 plays; demos[1] is next");
        // Con_Printf stamps con_times, but the demo's signon ends in
        // SCR_EndLoadingPlaque's Con_ClearNotify: no notify line over it.
        step(0.05);
        let notified = APP.with(|c| {
            let b = c.borrow();
            let d = b.as_ref().unwrap().demo.as_ref().unwrap();
            let now = d.demo.frames[d.idx].time;
            d.notify.visible(now).iter().map(|l| l.to_string()).collect::<Vec<_>>()
        });
        assert!(notified.is_empty(), "{notified:?}");
        console_toggle();
        // Bad argument counts print the C's usage lines ("play", as id's).
        run_console_line("playdemo");
        run_console_line("timedemo a b");
        let lines = console_lines();
        assert_eq!(
            lines[lines.len() - 4..],
            ["]playdemo", "play <demoname> : plays a demo", "]timedemo a b", "timedemo <demoname> : gets demo speeds"]
        );
        // playdemo inside the loop: the loop carries on after it (demonum kept).
        run_console_line("playdemo demo3");
        let (demo3, n) = playing();
        assert_eq!(n, 1, "playdemo leaves the loop's place");
        assert!(demo3.is_some() && demo3 != demo1, "demo3 plays: {demo3:?}");
        assert_eq!(console_lines().last().map(String::as_str), Some("Playing demo from demo3.dem."));
        to_end();
        step(0.05);
        assert_eq!(playing().1, 2, "its svc_disconnect: CL_NextDemo played demos[1]");
        // stopdemo: disconnected, the console forced up over the whole screen.
        run_console_line("stopdemo");
        assert_eq!(playing(), (None, 2), "stopped; the loop keeps its place");
        step(0.05);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().console.open = false);
        assert_eq!(console_visible(), 1, "con_forcedup: typing goes to the console");
        APP.with(|c| c.borrow_mut().as_mut().unwrap().console.open = true);
        // A missing demo: CL_Disconnect, the error, the loop off.
        run_console_line("playdemo nosuch");
        let lines = console_lines();
        assert_eq!(lines[lines.len() - 2..], ["Playing demo from nosuch.dem.", "ERROR: couldn't open."]);
        assert_eq!(playing(), (None, -1), "cls.demonum = -1: stop demo loop");
        // startdemos with the loop off only sets the list (slot 0 here) ...
        run_console_line("startdemos demo2");
        assert_eq!(console_lines().last().map(String::as_str), Some("1 demo(s) in loop"));
        assert_eq!(playing(), (None, -1), "the loop was off: it stays off");
        // ... and `demos` goes back to it at the second slot (demo2 from quake.rc).
        run_console_line("demos");
        let (d, n) = playing();
        assert_eq!(n, 2, "Host_Demos_f: demonum -1 -> 1, then CL_NextDemo");
        assert!(d.is_some() && d != demo1 && d != demo3, "demos[1] = demo2 plays: {d:?}");
        // startdemos while something plays switches the loop off: the demo
        // then ends in CL_Disconnect instead of the next one.
        run_console_line("startdemos demo1 demo2 demo3");
        assert_eq!(playing().1, -1);
        to_end();
        step(0.05);
        assert_eq!(playing(), (None, -1), "Host_EndGame outside the loop disconnects");
    }

    #[test]
    fn disconnected_the_console_covers_the_screen_and_leaving_the_menu_resumes_the_loop() {
        use crate::app::boot_attract;
        use crate::console::console_toggle;
        use crate::host::step;
        use crate::menu::{menu_cancel, menu_visible};
        assert_eq!(boot_attract(), 1);
        console_toggle();
        run_console_line("stopdemo");
        step(0.05);
        // SCR_SetUpToDrawConsole's con_forcedup: the whole (2-D) screen.
        let (cur, h, fb) = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            (a.console.current(), a.render_h, a.present.rgba())
        });
        assert_eq!(cur, h as f32, "the console is all the way down");
        // The conback is drawn over all of it: no black rows left.
        let w = fb.len() / 4 / h;
        let bottom = &fb[(h - 1) * w * 4..];
        assert!(bottom.chunks_exact(4).any(|p| p[..3] != [0, 0, 0]), "the conback reaches the bottom row");
        // The console key (toggleconsole) brings up the menu over it; leaving
        // the menu resumes the loop (M_Main_Key K_ESCAPE: CL_NextDemo with
        // nothing playing).
        console_toggle();
        assert_eq!(menu_visible(), 1, "Con_ToggleConsole_f disconnected: M_Menu_Main_f");
        step(0.05);
        let with_menu = APP.with(|c| c.borrow().as_ref().unwrap().present.rgba());
        let changed = with_menu.chunks_exact(4).zip(fb.chunks_exact(4)).filter(|(a, b)| a != b).count();
        assert!(changed > 5000, "M_Draw puts the menu over the full console ({changed} px)");
        menu_cancel();
        let (d, n) = playing();
        assert!(d.is_some(), "the demo loop resumed");
        assert_eq!(n, 2, "at its place: demos[1] played");
        step(0.05);
        let cur = APP.with(|c| c.borrow().as_ref().unwrap().console.current());
        assert!(cur < h as f32, "connected again: the console slides away ({cur})");
    }

    // -- timedemo --------------------------------------------------------------

    /// Parse `CL_FinishTimeDemo`'s line: (frames, seconds, fps).
    fn timedemo_line(line: &str) -> Option<(i64, f32, f32)> {
        let rest = line.strip_suffix(" fps")?;
        let (frames, rest) = rest.split_once(" frames ")?;
        let (secs, fps) = rest.split_once(" seconds ")?;
        Some((frames.parse().ok()?, secs.trim().parse().ok()?, fps.trim().parse().ok()?))
    }

    #[test]
    fn timedemo_demo1_draws_969_frames_as_ids_and_the_loop_goes_on() {
        use crate::app::boot_attract;
        use crate::console::console_toggle;
        use crate::host::step;
        assert_eq!(boot_attract(), 1);
        console_toggle();
        crate::vid::set_resolution(320, 200);
        run_console_line("timedemo demo1");
        assert_eq!(timedemo_running(), 1);
        assert_eq!(console_lines().last().map(String::as_str), Some("Playing demo from demo1.dem."));
        // No 72 fps cap: every call is a host frame, one recorded message
        // each (1 ms apart here, so the measured time is exact).
        let mut frames = 0;
        let mut idx = Vec::new();
        while timedemo_running() == 1 && frames < 2000 {
            assert_eq!(step(0.001), 1, "a timedemo frame runs on every call");
            frames += 1;
            if frames <= 3 {
                idx.push(APP.with(|c| c.borrow().as_ref().unwrap().demo.as_ref().unwrap().idx));
            }
        }
        assert_eq!(idx, [1, 2, 3], "the first frame reads through the second message, then one a frame");
        let lines = console_lines();
        let line = lines.iter().rev().find(|l| l.contains(" frames ")).expect("the timedemo line");
        let (n, secs, fps) = timedemo_line(line).unwrap_or_else(|| panic!("{line:?}"));
        // id's oracle: `timedemo demo1` draws 969 frames (oracle/README.md).
        assert_eq!(n, 969, "{line}");
        assert_eq!(frames, 971, "969 counted, the first, and the one that read svc_disconnect");
        assert!((secs - 1.0).abs() < 1e-6 && (fps - 1000.0).abs() < 0.5, "969 x 1 ms: {line}");
        assert_eq!(line, &format!("{n} frames {secs:5.1} seconds {fps:5.1} fps"), "%i %5.1f %5.1f");
        // Host_EndGame inside the loop: CL_NextDemo (the loop's demos[1]),
        // played normally again (the 72 fps cap is back).
        let (d, demonum) = playing();
        assert!(d.is_some() && demonum == 2, "{d:?} {demonum}");
        assert_eq!(timedemo_running(), 0);
        let ran: i32 = (0..144).map(|_| step(1.0 / 144.0)).sum();
        assert_eq!(ran, 72, "the cap again");
    }

    #[test]
    fn timedemo_outside_the_loop_disconnects_and_a_stop_prints_the_partial_count() {
        use crate::app::boot_attract;
        use crate::console::console_toggle;
        use crate::host::step;
        assert_eq!(boot_attract(), 1);
        console_toggle();
        crate::vid::set_resolution(320, 200);
        // CL_StopPlayback mid-run finishes it: frames drawn after the first.
        run_console_line("timedemo demo2");
        for _ in 0..100 {
            step(0.002);
        }
        run_console_line("stopdemo");
        let lines = console_lines();
        let line = lines.iter().rev().find(|l| l.contains(" frames ")).expect("the partial line");
        let (n, secs, _) = timedemo_line(line).unwrap();
        assert_eq!((n, (secs * 10.0).round()), (99, 2.0), "{line}");
        assert_eq!(playing(), (None, 1), "stopped, disconnected; the loop keeps its place");
        // The loop off (startdemos while playing), a timedemo ends disconnected.
        run_console_line("playdemo demo1");
        run_console_line("startdemos demo1 demo2 demo3");
        run_console_line("timedemo demo3");
        let mut guard = 0;
        while timedemo_running() == 1 && guard < 3000 {
            step(0.001);
            guard += 1;
        }
        let lines = console_lines();
        let (n, _, _) = timedemo_line(lines.iter().rev().find(|l| l.contains(" frames ")).unwrap()).unwrap();
        assert_eq!(n, 1090, "demo3, as id's");
        assert_eq!(playing(), (None, -1), "Host_EndGame outside the loop: CL_Disconnect");
    }

    // -- pause during playback ---------------------------------------------------

    #[test]
    fn pause_goes_nowhere_in_a_demo_and_cannot_go_disconnected() {
        use crate::app::boot_attract;
        use crate::console::console_toggle;
        assert_eq!(boot_attract(), 1);
        console_toggle();
        run_console_line("pause");
        assert_eq!(console_lines().last().map(String::as_str), Some("]pause"), "demo playback: not really connected");
        run_console_line("stopdemo");
        run_console_line("pause");
        assert_eq!(console_lines().last().map(String::as_str), Some("Can't \"pause\", not connected"));
    }

    #[test]
    fn a_recorded_svc_setpause_shows_the_plaque() {
        let mut d = build_demo().expect("the embedded demo boots");
        let (plain, _) = step_demo(&mut d, 0.0, false, &crate::vid::mode_vid(320, 200), true);
        for f in d.demo.frames.iter_mut() {
            f.paused = true;
        }
        let (paused, _) = step_demo(&mut d, 0.0, false, &crate::vid::mode_vid(320, 200), true);
        let changed: Vec<usize> = (0..320 * 200).filter(|&i| plain.pixels[i] != paused.pixels[i]).collect();
        assert!(changed.len() > 1000, "the plaque is drawn ({} px)", changed.len());
        assert!(
            changed.iter().all(|&i| (96..224).contains(&(i % 320)) && (64..88).contains(&(i / 320))),
            "only where SCR_DrawPause puts it"
        );
    }

    /// R_DrawParticles' `grav = frametime * sv_gravity.value * 0.05` reads the
    /// client's own cvar in playback too (it was a constant 800 here): at the
    /// default a recorded svc_particle puff (pt_slowgrav, dir 0) leaves its
    /// first 0.05 s frame at vz -2; after e1m8 (worldspawn sets 100, and the
    /// cvar outlives the map) at -0.25. The page keeps the cvar across the
    /// disconnect that starts a demo (`App::sv_gravity`).
    #[test]
    fn demo_particles_fall_by_the_sv_gravity_cvar() {
        use quake_rs::demo::{Demo, DemoFrame};
        use quake_rs::server::ParticleBurst;
        let vz = |sv_gravity: f32| {
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
                forcetrack: -1,
                cdtrack: None,
                frames: vec![plain(0.0), puff, plain(0.10), plain(0.15)],
            };
            let mut d = DemoPlay::new(build_test_pak(&[]), render::demo_room(), [[0u8; 3]; 256], demo);
            d.sv_gravity = sv_gravity;
            let _ = step_demo(&mut d, 0.05, false, &crate::vid::mode_vid(64, 40), true);
            assert_eq!(d.idx, 1);
            let v: Vec<f32> = d.particles.particles().iter().map(|p| p.velocity[2]).collect();
            assert!(!v.is_empty() && v.iter().all(|&z| z == v[0]), "{v:?}");
            v[0]
        };
        // What the page hands the demo: the last game's cvar, kept by CL_Disconnect.
        let page_gravity_after = |map: &str| {
            let w = crate::app::build_walk_map(map).expect("map boots");
            let mut g = 0.0;
            crate::app::ensure_app(|a| {
                a.start_game(w);
                cl_disconnect(a);
                g = a.sv_gravity;
            });
            g
        };
        assert_eq!(vz(page_gravity_after("maps/e1m1.bsp")), -800.0 * 0.05 * 0.05);
        assert_eq!(vz(page_gravity_after("maps/e1m8.bsp")), -100.0 * 0.05 * 0.05);
        assert_eq!(page_gravity_after("maps/e1m1.bsp"), 800.0);
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
        let _ = step_demo(&mut d, 0.05, false, &crate::vid::mode_vid(160, 100), true);
        assert!(unflashed(&d), "playback start");
        let _ = step_demo(&mut d, 1.0e6, false, &crate::vid::mode_vid(160, 100), true);
        let _ = step_demo(&mut d, 0.05, false, &crate::vid::mode_vid(160, 100), true);
        assert_eq!(d.idx, 0, "wrapped");
        assert_eq!(d.cl_items, items0);
        assert!(unflashed(&d), "the loop wrap");
        // A bit the recording gains later is stamped on its frame's clock.
        let got = d.demo.frames.iter().position(|f| f.client.items & !items0 != 0);
        if let Some(i) = got {
            // (Playback starts 0.1 s before the first message: CL_LerpPoint.)
            let dt = d.demo.frames[i].time - d.demo.frames[0].time + 0.1;
            let _ = step_demo(&mut d, dt, false, &crate::vid::mode_vid(160, 100), true);
            assert!(!unflashed(&d), "frame {i}'s new item is stamped");
        }
    }
}

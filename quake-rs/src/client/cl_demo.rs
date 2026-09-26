//! cl_demo.c — playing a recorded `.dem`: `CL_PlayDemo_f`'s build of a
//! [`DemoPlay`], quake.rc's demo loop, and the demo client frame rendered like
//! live play: per-frame effect replay (svc_particle, temp entities,
//! svc_sound/stopsound, svc_damage, prints — cl_parse.c / cl_tent.c / view.c
//! `V_ParseDamage`) and [`demo_frame`], the recorded-POV `V_CalcRefdef` +
//! `SCR_UpdateScreen` for one frame. `timedemo` (`CL_TimeDemo_f`) plays a
//! demo one message per host frame ([`build_timedemo`], [`timedemo_frame`])
//! and [`TimeDemoClock`] keeps `cls.td_*` and prints `CL_FinishTimeDemo`'s
//! line.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/cl_demo.c`.

use std::borrow::Cow;

use crate::bsp::Bsp;
use crate::demo::{parse_demo, DemoFrame, EntSnapshot};
use crate::mdl::Mdl;
use crate::pak::Pak;
use crate::particles::{ParticleSystem, TrailHead, TrailStep};
use crate::render::{self, Camera, ModelInstance, Viewmodel};
use crate::stepping::Stepping;
use crate::tent::BeamModel;
use crate::wad::Qpic;

use super::cl_tent::{rocket_trail_type, spawn_temp_entity};
use super::host_cmd::IT_INVISIBILITY;
use super::view::{
    cshift_add, fade_cshifts, parse_damage, stamp_item_gettime, stufftext_bonus_flash, BONUS_COLOR,
    BONUS_PERCENT, FACE_ANIM_TIME, V_KICKTIME,
};
use super::{
    backtile_for, color_for_name, lap, render_options, s_update, ClientFrame, DemoPlay, Listener,
    Phase, SoundCall, Vid,
};

/// quake.rc's `startdemos demo1 demo2 demo3`: the attract loop's demos, played
/// in turn (`CL_NextDemo` on each demo's `svc_disconnect`), wrapping to the first.
pub const DEMOS: [&str; 3] = ["demo1.dem", "demo2.dem", "demo3.dem"];

/// `MAX_DEMOS` (client.h): the most demos `startdemos` keeps in its loop.
pub const MAX_DEMOS: usize = 8;

/// `COM_DefaultExtension` (common.c): `path` with `extension` (".dem")
/// appended unless its last path component already has a `.EXT` — `demo1`
/// becomes `demo1.dem`. (Like the C, the first character is never looked at.)
pub fn default_extension(path: &str, extension: &str) -> String {
    let b = path.as_bytes();
    let mut i = b.len();
    while i > 1 {
        i -= 1;
        match b[i] {
            b'/' => break,
            b'.' => return path.to_string(),
            _ => {}
        }
    }
    format!("{path}{extension}")
}

/// `playdemo` of [`DEMOS`]`[demonum % 3]` from `pak` (`CL_PlayDemo_f`): the
/// demo's sounds start through `sound` ([`SoundCall::StopAll`], then the
/// signon's static loops).
pub fn build_demo_n(pak: Pak, demonum: usize, sound: &mut Vec<SoundCall>) -> Option<DemoPlay> {
    let demonum = demonum % DEMOS.len();
    let mut d = build_demo(pak, DEMOS[demonum], sound)?;
    d.demonum = demonum;
    Some(d)
}

/// `playdemo <name>`'s load (`CL_PlayDemo_f`, and the first `CL_GetMessage`s
/// that read its signon): the demo file `name` (with its extension, as
/// `COM_FOpenFile` takes it; [`default_extension`]) from `pak`, its world
/// and models, played back smoothly — interpolated between the recorded
/// messages as `CL_RelinkEntities` does ([`demo_frame`]). `None` when the
/// file is missing or unplayable (the C prints "ERROR: couldn't open.").
/// The demo's sounds start through `sound` ([`SoundCall::StopAll`], then the
/// signon's static loops).
pub fn build_demo(pak: Pak, name: &str, sound: &mut Vec<SoundCall>) -> Option<DemoPlay> {
    build_demo_with(pak, name, sound, |bytes, rotating| {
        crate::demo::parse_demo_interpolated(bytes, 60.0, rotating).ok()
    })
}

/// `timedemo <name>`'s load (`CL_TimeDemo_f`): as [`build_demo`], but one
/// frame per recorded message, each at the message's own time with no
/// interpolation — what `CL_LerpPoint` gives while `cls.timedemo` is set
/// ([`crate::demo::parse_demo_timedemo`]). Played by [`timedemo_frame`].
pub fn build_timedemo(pak: Pak, name: &str, sound: &mut Vec<SoundCall>) -> Option<DemoPlay> {
    build_demo_with(pak, name, sound, |bytes, rotating| {
        crate::demo::parse_demo_timedemo(bytes, rotating).ok()
    })
}

/// The shared load of [`build_demo`] / [`build_timedemo`]: `parse` turns the
/// demo's bytes and the set of `EF_ROTATE` model indices into the frames.
fn build_demo_with(
    pak: Pak,
    name: &str,
    sound: &mut Vec<SoundCall>,
    parse: impl Fn(&[u8], &[usize]) -> Option<crate::demo::Demo>,
) -> Option<DemoPlay> {
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let demo_bytes = read(name)?;
    let demo = parse_demo(&demo_bytes).ok()?;
    let map = demo.map_name()?.to_string();
    let bsp = Bsp::parse(&read(&map)?).ok()?;
    let palette = render::parse_palette(&read("gfx/palette.lmp")?)?;

    // Load a model (alias or sprite) + colour per precache index.
    let mut models = Vec::with_capacity(demo.model_precache.len());
    let mut sprites: Vec<Option<crate::spr::Sprite>> =
        Vec::with_capacity(demo.model_precache.len());
    let mut colors = Vec::with_capacity(demo.model_precache.len());
    for name in &demo.model_precache {
        if name.ends_with(".mdl") {
            models.push(read(name).and_then(|b| Mdl::parse(&b).ok()));
            sprites.push(None);
        } else if name.ends_with(".spr") {
            models.push(None);
            sprites.push(read(name).and_then(|b| crate::spr::Sprite::parse(&b).ok()));
        } else {
            models.push(None);
            sprites.push(None);
        }
        colors.push(color_for_name(name));
    }
    if demo.frames.is_empty() {
        return None;
    }
    // Re-parse for playback (`parse`): with inter-frame interpolation —
    // smooth 60 fps playback instead of the choppy 10 Hz keyframes
    // (CL_LerpPoint) — or a timedemo's one frame per message, plus EF_ROTATE
    // spin for any model whose header flags it (rotating pickups). The set of
    // rotating model indices is derived from the just-loaded MDL headers.
    let rotating: Vec<usize> = models
        .iter()
        .enumerate()
        .filter_map(|(i, m)| {
            m.as_ref()
                .filter(|md| md.header.flags & crate::demo::EF_ROTATE != 0)
                .map(|_| i)
        })
        .collect();
    let demo = parse(&demo_bytes, &rotating)?;
    if demo.frames.is_empty() {
        return None;
    }
    // Demo committed (nothing below fails): tear down the previous level/mode's
    // looping audio and register the demo signon's `svc_spawnstaticsound` loops
    // (CL_ParseStaticSound ran these on the live client during demo playback
    // too — the e1m3 demo has its own torches).
    sound.push(SoundCall::StopAll);
    sound.push(SoundCall::Static(demo.static_sounds.clone()));
    // The overlay assets for a recorded intermission/finale (each optional —
    // a demo without one never touches them).
    let gfx_wad = read("gfx.wad").and_then(|b| crate::wad::Wad2::parse(b).ok());
    let conchars = gfx_wad.as_ref().and_then(render::conchars_pic);
    let lmp = |n: &str| -> Option<Qpic> { read(n).and_then(|b| Qpic::parse(&b).ok()) };
    // Resolve everything that reads the pak BEFORE the struct literal so the
    // `read`/`lmp` closure borrows end and `pak` can move into the DemoPlay.
    let colormap = read("gfx/colormap.lmp");
    let pic_complete = lmp("gfx/complete.lmp");
    let pic_inter = lmp("gfx/inter.lmp");
    let pic_finale = lmp("gfx/finale.lmp");
    let pic_pause = lmp("gfx/pause.lmp");
    let mut d = DemoPlay::new(pak, bsp, palette, demo);
    d.models = models;
    d.sprites = sprites;
    d.colormap = colormap;
    d.colors = colors;
    d.gfx_wad = gfx_wad;
    d.conchars = conchars;
    d.pic_complete = pic_complete;
    d.pic_inter = pic_inter;
    d.pic_finale = pic_finale;
    d.pic_pause = pic_pause;
    Some(d)
}

/// Spawn the recorded effects of demo frame `idx` into the live particle pool
/// exactly ONCE: a frame rendered across several steps (small `dt`) must not
/// re-spawn its bursts each step. `d.last_spawned_idx` records the most recently
/// spawned frame; this is a no-op when it already equals `idx`.
///
/// Each `svc_particle` burst replays through [`ParticleSystem::spawn_burst`],
/// except the explosion sentinel (`count >= 1024`, the demo parser's mapping of
/// the net `count == 255`) which routes to [`ParticleSystem::spawn_explosion`]
/// for the 1024-particle fiery burst. Each temp entity replays through the same
/// [`spawn_temp_entity`] mapping the live walk uses (explosion / impact / splash).
/// The frame's recorded server `time` is the absolute clock for particle
/// lifetimes (`spawn_*` set `die = now + life`).
fn spawn_demo_frame_effects(d: &mut DemoPlay, idx: usize, sound: &mut Vec<SoundCall>) {
    if d.last_spawned_idx == idx {
        return; // already spawned this frame's effects; don't double-spawn
    }
    d.last_spawned_idx = idx;
    let Some(frame) = d.demo.frames.get(idx) else { return };
    let now = frame.time;
    // The frame borrows `d.demo`; copy the small effect records out so we can
    // call &mut self spawn methods on `d.particles` without aliasing `d`.
    let bursts = frame.particles.clone();
    let tents = frame.temp_entities.clone();
    let sounds = frame.sounds.clone();
    let stops = frame.stop_sounds.clone();
    let damage = frame.damage.clone();
    let prints = frame.prints.clone();
    let centerprints = frame.centerprints.clone();
    let bonus = frame.stufftext.iter().any(|t| stufftext_bonus_flash(t));
    let view_entity_origin = frame.view_entity_origin;
    let view_angles = frame.view_angles;
    for b in &bursts {
        // svc_particle is always R_RunParticleEffect (spawn_burst) in id's
        // CL_ParseParticleEffect — the net count==255 sentinel just means 1024
        // particles (the demo parser already maps it), NOT the rocket
        // R_ParticleExplosion. Route every burst through spawn_burst.
        d.particles
            .spawn_burst(b.org, b.dir, b.color, b.count, now, &mut d.prng);
    }
    // CLIENT-SIDE temp-entity impact sounds (CL_ParseTEnt: tink/ric for
    // spikes, wizard/hit, hknight/hit, r_exp3 for explosions) — the C plays
    // these during demo playback too; they are NOT in the recorded svc_sound
    // stream. Collected here and queued through the same path as the recorded
    // sounds below.
    let mut te_sounds: Vec<crate::server::SoundEvent> = Vec::new();
    for ev in &tents {
        // Beam types refresh the entity's beam slot (CL_ParseBeam) with the
        // frame's recorded server time; demo_frame expands the live beams into
        // bolt-model instances every render (CL_UpdateTEnts), exactly like the
        // live walk.
        if let Some(bm) = BeamModel::from_te_type(ev.te_type) {
            d.beams.parse_beam(ev.entity, bm, ev.pos, ev.end, now);
            continue;
        }
        // Reuse the live-walk mapping (explosion/impact/splash) — including
        // its client-side impact sound, exactly like walk_frame's te_sounds.
        if let Some(name) = spawn_temp_entity(&mut d.particles, ev, now, &mut d.prng) {
            te_sounds.push(crate::server::SoundEvent {
                entity: 0,
                channel: 0,
                sound_index: -1,
                sample: name.to_string(),
                origin: ev.pos,
                volume: 1.0,
                attenuation: 1.0,
            });
        }
    }
    // The RECORDED svc_sound one-shots (CL_ParseStartSoundPacket ->
    // S_StartSound): queue through the SAME spatialized path live play uses.
    // The listener is the recorded camera pose, which demo_frame refreshes
    // every frame; the recorded view entity's own sounds (weapon fire, pain
    // grunts) get the full-volume centred treatment via the view_entity key.
    if !sounds.is_empty() {
        sound.push(SoundCall::Start { events: sounds, view_entity: d.demo.viewentity as i32 });
    }
    if !te_sounds.is_empty() {
        sound.push(SoundCall::Start { events: te_sounds, view_entity: d.demo.viewentity as i32 });
    }
    // svc_stopsound: hand the (entity, channel) stops to the sound layer (the
    // page stop()s its registered source for that key, S_StopSound).
    sound.push(SoundCall::Stop(stops));
    // svc_damage (V_ParseDamage, view.c): bump the damage cshift and compute
    // the directional view kick from the recorded attack origin.
    for dmg in &damage {
        let pd = parse_damage(dmg.armor, dmg.blood, dmg.from, view_entity_origin, view_angles);
        d.damage_blend = cshift_add(d.damage_blend, pd.percent);
        d.damage_color = pd.color;
        d.v_dmg_roll = pd.roll;
        d.v_dmg_pitch = pd.pitch;
        d.v_dmg_time = V_KICKTIME;
        d.faceanimtime = now + FACE_ANIM_TIME;
    }
    // svc_stufftext "bf" (V_BonusFlash_f): the gold pickup flash.
    if bonus {
        d.bonus_blend = BONUS_PERCENT;
    }
    // svc_print fragments go through Con_Print (pickups arrive as several
    // fragments on one console line), timed on the demo's recorded clock;
    // svc_centerprint replaces the current centered message (SCR_CenterPrint,
    // ~2 s).
    for p in &prints {
        d.notify.print(p, now);
    }
    if let Some(text) = centerprints.into_iter().next_back() {
        d.centerprint = Some((text, now + 2.0));
    }
}

/// One host frame of demo playback: the recorded clock advances by `dt` and
/// playback moves on to the message it has reached (spawning the effects of
/// every message passed on the way), then the frame is drawn
/// ([`render_demo_frame`]). After the last frame it loops to the first (the
/// host normally starts the next demo instead, `CL_NextDemo`).
pub fn demo_frame(d: &mut DemoPlay, dt: f32, menu_up: bool, vid: &Vid) -> ClientFrame {
    let mut sound = Vec::new();
    // Con_CheckResize: the notify lines are laid out con_linewidth wide.
    d.notify.check_resize(vid.width, vid.height);
    let n = d.demo.frames.len();
    let t0 = d.demo.frames[0].time;
    d.elapsed += dt;
    // Wrap BEFORE advancing: only loop back to frame 0 once we were already
    // sitting on the last frame on a prior step and time has run past it. This
    // defers the reset by one step so frames[n-1] is rendered (displayed for its
    // dt) before we snap back to the start — the previous code reset to 0 the
    // instant `idx` reached n-1, so the final frame was never shown.
    if d.idx + 1 >= n {
        d.idx = 0;
        d.elapsed = 0.0;
        // Looping restarts the recorded effect stream: drop every live particle
        // and beam and forget what was spawned so the replay from frame 0 is
        // identical to the first pass (no stale explosions/bolts carried across
        // the wrap). The per-POV view state resets too: damage flash/kick,
        // notify + centerprint text, and the stair-smoothing accumulator (their
        // expiries live on the recorded clock, which just jumped back to t0).
        d.particles = ParticleSystem::new();
        d.trail_org.clear();
        d.beams.clear();
        d.last_spawned_idx = usize::MAX;
        d.damage_blend = 0.0;
        d.bonus_blend = 0.0;
        d.faceanimtime = 0.0;
        d.cl_items = d.demo.frames[0].client.items; // unflashed (DemoPlay::new)
        d.item_gettime = [0.0; 32];
        d.v_dmg_time = 0.0;
        d.centerprint = None;
        d.notify.clear();
        d.oldz = f32::NAN;
    }
    // Advance to the frame matching the recorded server time. Stop at the last
    // frame (n-1); the wrap above handles looping on the FOLLOWING step. Spawn
    // the recorded effects of EACH frame we newly advance onto (a large dt can
    // step over several frames at once; missing one would drop its explosion).
    while d.idx + 1 < n && (d.demo.frames[d.idx + 1].time - t0) <= d.elapsed {
        d.idx += 1;
        spawn_demo_frame_effects(d, d.idx, &mut sound);
    }
    // Also spawn the landing frame's effects when we first arrive on it without
    // the while-loop running (e.g. the very first step lands on frame 0, or a
    // tiny dt holds us on the same frame the wrap reset us to). `last_spawned_idx`
    // guards against re-spawning while a frame lingers across several steps.
    spawn_demo_frame_effects(d, d.idx, &mut sound);
    let lerp = d.stepping == Stepping::Uncapped;
    render_demo_frame(d, dt, dt, lerp, menu_up, vid, sound)
}

/// One host frame of `timedemo` (`cls.timedemo`): `CL_GetMessage` reads
/// exactly one message a frame, whatever the time. The first frame — the one
/// `CL_TimeDemo_f` ran in — reads the whole signon and the message after the
/// one that completed it (frames 0 and 1 here), every later frame the next
/// message; each is drawn at its own time (`CL_LerpPoint` snaps `cl.time` to
/// `mtime[0]`). `frametime` is the host frame's `host_frametime` (the view
/// kick, the palette-shift fades, the stair smoothing and the sound ramps run
/// on it); the particles move by `cl.time - cl.oldtime`, the recorded time
/// between the two messages. `None` when there is no next message: the demo
/// has ended (its `svc_disconnect`, `Host_EndGame`, or the file running out,
/// `CL_StopPlayback`) and this frame draws nothing — the host finishes the
/// timedemo ([`TimeDemoClock::finish`]). Built by [`build_timedemo`].
pub fn timedemo_frame(d: &mut DemoPlay, frametime: f32, menu_up: bool, vid: &Vid) -> Option<ClientFrame> {
    let mut sound = Vec::new();
    d.notify.check_resize(vid.width, vid.height);
    let n = d.demo.frames.len();
    let first = d.last_spawned_idx == usize::MAX;
    if first {
        spawn_demo_frame_effects(d, 0, &mut sound);
    }
    if d.idx + 1 >= n {
        return None;
    }
    let oldtime = d.demo.frames[d.idx].time;
    d.idx += 1;
    spawn_demo_frame_effects(d, d.idx, &mut sound);
    let cl_frametime = d.demo.frames[d.idx].time - oldtime;
    Some(render_demo_frame(d, frametime, cl_frametime, false, menu_up, vid, sound))
}

/// What a demo frame draws that moves between messages: the client clock
/// (`cl.time`), the recorded POV and the entities.
pub struct DemoView<'a> {
    pub time: f32,
    pub view_origin: [f32; 3],
    pub view_entity_origin: [f32; 3],
    pub view_angles: [f32; 3],
    /// `cl.velocity` (the bob and the strafe lean).
    pub velocity: [f32; 3],
    pub entities: Cow<'a, [EntSnapshot]>,
}

/// The view of message `idx` of `frames` with the demo clock at `elapsed`
/// seconds past the first message. Classic draws the message's own (the
/// port's playback shows each message until the next is due). With `lerp`,
/// the uncapped client's: `CL_LerpPoint` and `CL_RelinkEntities` blend it
/// toward the next message by how far the clock has got between the two —
/// the POV, its velocity and every entity in both (angles the short way
/// round, and no blend across a move of 100 units or more on any axis, id's
/// teleport test) — so a 60 Hz recording moves smoothly at any display rate.
/// As in `CL_LerpPoint`, the blend spans at most the last 0.1 s before the
/// next message.
pub fn demo_view(frames: &[DemoFrame], idx: usize, elapsed: f32, lerp: bool) -> DemoView<'_> {
    let f = &frames[idx];
    let own = DemoView {
        time: f.time,
        view_origin: f.view_origin,
        view_entity_origin: f.view_entity_origin,
        view_angles: f.view_angles,
        velocity: f.client.velocity,
        entities: Cow::Borrowed(&f.entities),
    };
    let Some(next) = frames.get(idx + 1).filter(|n| lerp && f.intermission == 0 && n.intermission == 0) else {
        return own;
    };
    let t1 = f.time.max(next.time - 0.1);
    if next.time <= t1 {
        return own;
    }
    let clock = (frames[0].time + elapsed).clamp(f.time, next.time);
    let frac = ((clock - t1) / (next.time - t1)).clamp(0.0, 1.0);
    // CL_RelinkEntities: `if (delta[j] > 100 || delta[j] < -100) f = 1;`
    // ("assume a teleportation, not a motion") — here, stay put until the
    // next message shows the new place.
    let teleport = |a: [f32; 3], b: [f32; 3]| (0..3).any(|i| (b[i] - a[i]).abs() > 100.0);
    let blend = |a: [f32; 3], b: [f32; 3]| -> [f32; 3] {
        if teleport(a, b) {
            a
        } else {
            std::array::from_fn(|i| a[i] + frac * (b[i] - a[i]))
        }
    };
    let turn = |a: [f32; 3], b: [f32; 3]| -> [f32; 3] {
        std::array::from_fn(|i| {
            let d = b[i] - a[i];
            let d = if d > 180.0 { d - 360.0 } else if d < -180.0 { d + 360.0 } else { d };
            a[i] + frac * d
        })
    };
    let entities = f
        .entities
        .iter()
        .map(|e| match next.entities.iter().find(|n| e.num >= 0 && n.num == e.num) {
            Some(n) if !teleport(e.origin, n.origin) => {
                EntSnapshot { origin: blend(e.origin, n.origin), angles: turn(e.angles, n.angles), ..*e }
            }
            _ => *e,
        })
        .collect();
    DemoView {
        time: clock,
        view_origin: blend(f.view_origin, next.view_origin),
        view_entity_origin: blend(f.view_entity_origin, next.view_entity_origin),
        view_angles: turn(f.view_angles, next.view_angles),
        velocity: std::array::from_fn(|i| f.client.velocity[i] + frac * (next.client.velocity[i] - f.client.velocity[i])),
        entities: Cow::Owned(entities),
    }
}

/// Draw demo frame `d.idx` (its effects already spawned): the recorded POV's
/// `V_CalcRefdef`, `S_Update`, the 3-D view and `SCR_UpdateScreen`'s 2-D
/// layer. `dt` is `host_frametime`; `cl_frametime` is `cl.time - cl.oldtime`,
/// the particles' step (the two are the same frame time in ordinary
/// playback). `lerp` draws the uncapped client's view between messages
/// ([`demo_view`]; never in a timedemo, whose frames are one message each).
fn render_demo_frame(
    d: &mut DemoPlay,
    dt: f32,
    cl_frametime: f32,
    lerp: bool,
    menu_up: bool,
    vid: &Vid,
    mut sound: Vec<SoundCall>,
) -> ClientFrame {
    let (render_w, render_h) = (vid.width, vid.height);
    let f = &d.demo.frames[d.idx];
    // What moves — the clock, the POV, the entities: the message's own, or
    // (uncapped) CL_LerpPoint's view between it and the next.
    let v = demo_view(&d.demo.frames, d.idx, d.elapsed, lerp);

    // CL_RelinkEntities' model-flag trails (R_RocketTrail from the entity's
    // previous origin: rocket/lavaball fire, grenade smoke, gib blood, zombie
    // gibs, wizard/knight/vore tracers), exactly as in live play. A relinked
    // entity's first sighting (forcelink) starts at its own origin, so an
    // entity absent from this frame is forgotten. Statics never trail.
    // (EF_ROCKET's dlight is not drawn: demo playback has no dlights yet.)
    d.trail_org.retain(|num, _| v.entities.iter().any(|e| e.num == *num));
    let step = TrailStep { stepping: d.stepping, dt, now: v.time };
    for e in v.entities.iter() {
        if e.num < 0 {
            continue;
        }
        let flags = d.models.get(e.modelindex).and_then(|m| m.as_ref()).map_or(0, |m| m.header.flags);
        if let Some(ttype) = rocket_trail_type(flags) {
            let head = d.trail_org.entry(e.num).or_insert(TrailHead::at(e.origin));
            d.particles.spawn_trail(head, e.origin, ttype, step, &mut d.tracercount, &mut d.prng);
        }
    }

    let mut owned: Vec<ModelInstance> = Vec::new();
    let mut bmodels: Vec<render::BModelInstance> = Vec::new();
    let mut sprite_insts: Vec<render::SpriteInstance> = Vec::new();
    for e in v.entities.iter() {
        if let Some(Some(mdl)) = d.models.get(e.modelindex) {
            owned.push(ModelInstance {
                mdl,
                origin: e.origin,
                yaw: e.angles[1],
                // Demo entities carry full angles; orient projectiles (pitch/roll)
                // as the recorded stream did (R_AliasSetUpTransform).
                pitch: e.angles[0],
                roll: e.angles[2],
                color: d.colors.get(e.modelindex).copied().unwrap_or([200, 200, 200]),
                // Demo entities carry their current animation frame from the net
                // stream — use it so monsters in the demo are actually posed.
                frame: e.frame.max(0) as usize,
                // R_AliasSetupSkin: `skinnum = currententity->skinnum`.
                skinnum: e.skin,
            });
        } else if let Some(num) = d
            .demo
            .model_precache
            .get(e.modelindex)
            .and_then(|name| name.strip_prefix('*'))
            .and_then(|n| n.parse::<usize>().ok())
        {
            // Brush submodels (doors, platforms, ELEVATORS, buttons) are "*N"
            // precache names with no alias Mdl; they render at the entity origin
            // from world submodel N. The live walk passes these; the demo path used
            // to drop them entirely, so moving level geometry vanished behind the
            // boot-demo menu. `frame` picks the activated (+a..+j) texture cycle.
            bmodels.push(render::BModelInstance {
                model_index: num,
                origin: e.origin,
                frame: e.frame.max(0),
            });
        } else if let Some(Some(spr)) = d.sprites.get(e.modelindex) {
            // Sprite-model entity (the boot demo's s_explod.spr explosion flashes).
            sprite_insts.push(render::SpriteInstance {
                sprite: spr,
                origin: e.origin,
                frame: e.frame.max(0) as usize,
            });
        }
    }
    // CL_UpdateTEnts: expand the recorded lightning beams into bolt-model
    // pieces, exactly like the live walk. The bolt models resolve through the
    // demo's PRECACHE table (the .dem signon lists progs/bolt*.mdl); a beam
    // owned by the recorded view entity tracks its per-frame origin.
    if d.beams.any_live(v.time) {
        d.beams.update(
            v.time,
            d.demo.viewentity as i32,
            v.view_entity_origin,
            &mut d.prng,
            &mut d.beam_scratch,
        );
        for seg in &d.beam_scratch {
            let name = seg.model.model_name();
            let Some(idx) = d.demo.model_precache.iter().position(|n| n == name) else {
                continue; // model not precached (e.g. beam.mdl in shareware)
            };
            if let Some(Some(mdl)) = d.models.get(idx) {
                owned.push(ModelInstance {
                    mdl,
                    origin: seg.origin,
                    yaw: seg.yaw,
                    pitch: seg.pitch,
                    roll: seg.roll,
                    color: d.colors.get(idx).copied().unwrap_or([200, 200, 200]),
                    frame: 0,
                    skinnum: 0,
                });
            }
        }
    }
    // The recorded per-client state (svc_clientdata) drives V_CalcRefdef.
    let client = f.client;
    // CL_ParseClientdata's item get-times on the recorded clock.
    stamp_item_gettime(&mut d.cl_items, &mut d.item_gettime, client.items, f.time);
    let cam = if f.intermission != 0 {
        // V_CalcIntermissionRefdef (view.c): a recorded intermission renders
        // with the forced v_idlescale=1 idle sway (V_AddIdle, stock
        // cycle/level cvars) applied LIVE on top of the recorded (QC-placed)
        // camera angles — no bob, no punch, no kick, no stair smoothing.
        Camera {
            pos: v.view_origin,
            yaw: v.view_angles[1] + (v.time * 2.0).sin() * 0.3,
            pitch: -(v.view_angles[0] + (v.time * 1.0).sin() * 0.3),
            roll: v.view_angles[2] + (v.time * 0.5).sin() * 0.1,
            fov_deg: 90.0,
        }
    } else {
        // V_CalcRefdef (view.c) on the RECORDED stream, exactly like the C's
        // demo playback: head-bob from the recorded SU_VELOCITY (visible in
        // id's demo1 — the player runs), stair-step smoothing from the
        // recorded SU_ONGROUND, the strafe/damage/dead view roll
        // (V_CalcViewRoll) and the recorded punchangle added LAST. V_AddIdle
        // is a no-op here (v_idlescale defaults to 0 outside intermission);
        // the 1/32 anti-node-line epsilon is omitted, matching this port's
        // live walk. The listener pose below deliberately stays UNbobbed
        // (audio panning must not jitter with the head-bob), like walk_frame.
        let vel = v.velocity;
        let speed_xy = (vel[0] * vel[0] + vel[1] * vel[1]).sqrt();
        let bob = render::view_bob(speed_xy, v.time);
        let mut eye = v.view_origin; // view entity origin + recorded viewheight
        eye[2] += bob;
        // Stair-step smoothing (V_CalcRefdef ~960): the same port as
        // walk_frame's, driven by the recorded onground flag + the raw view
        // entity origin z.
        let origin_z = v.view_entity_origin[2];
        let sdt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        if d.oldz.is_finite() && client.onground && origin_z - d.oldz > 0.0 {
            d.oldz += sdt * 80.0;
            if d.oldz > origin_z {
                d.oldz = origin_z;
            }
            if origin_z - d.oldz > 12.0 {
                d.oldz = origin_z - 12.0;
            }
            eye[2] += d.oldz - origin_z;
        } else {
            d.oldz = origin_z;
        }
        // V_CalcViewRoll: strafe lean from the recorded velocity (the C reads
        // the view entity's angles, which V_CalcRefdef keeps at YAW =
        // viewangles[YAW], PITCH = -viewangles[PITCH]), plus the decaying
        // svc_damage kick; a dead POV (health <= 0) REPLACES the whole roll
        // with the 80-degree dead view (the recorded viewangles[ROLL] and the
        // lean/kick are wiped — the C assigns viewangles[ROLL] = 80). The
        // punchangle adds AFTER, per the C's VectorAdd ordering.
        let basis = [-v.view_angles[0], v.view_angles[1], 0.0];
        let mut roll_angle =
            v.view_angles[2] + crate::server::v_calc_roll(basis, vel);
        let mut dmg_pitch = 0.0;
        if d.v_dmg_time > 0.0 {
            roll_angle += d.v_dmg_time / V_KICKTIME * d.v_dmg_roll;
            dmg_pitch = d.v_dmg_time / V_KICKTIME * d.v_dmg_pitch;
            d.v_dmg_time -= sdt; // v_dmg_time -= host_frametime
        }
        if client.health <= 0 {
            roll_angle = 80.0; // dead view angle (replaces lean + kick + bank)
        }
        Camera {
            pos: eye,
            yaw: v.view_angles[1] + client.punchangle[1],
            // QuakeC pitch is +down; the renderer's is +up.
            pitch: -(v.view_angles[0] + dmg_pitch + client.punchangle[0]),
            roll: roll_angle + client.punchangle[2],
            fov_deg: 90.0,
        }
    };
    // Sound listener pose + the per-leaf ambient channels follow the demo
    // camera (the C's S_Update runs in demo playback too — the recorded e1m3
    // run drifts past water and open sky, and its placed torch loops pan with
    // the recorded view). Forward/right are the level yaw basis like walk_frame.
    {
        let yaw_rad = (v.view_angles[1] as f64).to_radians();
        let (sy, cy) = (yaw_rad.sin() as f32, yaw_rad.cos() as f32);
        let listener = Listener {
            pos: v.view_origin,
            forward: [cy, sy, 0.0],
            right: [sy, -cy, 0.0],
        };
        sound.push(s_update(&d.bsp, listener, dt));
    }
    // The recorded server time animates the demo's liquids/sky too. The live
    // particle pool (replayed from the recorded svc_particle / temp-entity
    // stream) is passed as (world pos, palette index) so blood/puffs/explosions
    // draw into the scene sharing its z-buffer. Demos carry no dynamic lights
    // here (empty; a deferred LOW).
    // R_DrawParticles' order, as in walk_frame: retire (`die < cl.time`), draw,
    // then move and ramp.
    d.particles.retire(v.time);
    let parts: Vec<([f32; 3], u8)> =
        d.particles.particles().iter().map(|p| (p.origin, p.color)).collect();
    // `grav = frametime * sv_gravity.value * 0.05`: R_DrawParticles reads the
    // client's own sv_gravity cvar in playback too — 800, or what the last map
    // played set it to (e1m8's worldspawn: 100), not the recording's.
    if cl_frametime.is_finite() && cl_frametime > 0.0 {
        d.particles.integrate(cl_frametime, v.time, crate::server::Server::sv_gravity_cvar() * 0.05);
    }
    // The RECORDED svc_lightstyle table drives the world lighting through the
    // same R_AnimateLight 10 Hz logic the live walk uses (lightstyle_scales_at)
    // — the demo's torch flicker matches the recording exactly. A synthetic
    // demo without a table (tests) falls back to the previous seeded default:
    // style 0 = 'm' (264/256, id's steady-world brightness), the rest neutral.
    let demo_styles = if f.lightstyles.is_empty() {
        let mut s = render::NEUTRAL_LIGHTSTYLE_SCALES;
        s[0] = 264.0 / 256.0;
        s
    } else {
        crate::server::lightstyle_scales_at(&f.lightstyles, v.time)
    };
    // The first-person weapon viewmodel: SU_WEAPON is the model PRECACHE index
    // (`view->model = cl.model_precache[cl.stats[STAT_WEAPON]]`, V_CalcRefdef),
    // SU_WEAPONFRAME its animation frame. Hidden exactly like R_DrawViewModel
    // (r_main.c ~606): invisible POV (Ring of Shadows), dead POV, or an
    // intermission (V_CalcIntermissionRefdef sets `view->model = NULL`).
    let hide_gun = f.intermission != 0
        || client.health <= 0
        || client.items & IT_INVISIBILITY != 0;
    let viewmodel = if hide_gun {
        None
    } else {
        match d.models.get(client.weapon_model.max(0) as usize) {
            Some(Some(mdl)) => {
                // V_CalcRefdef's gun origin from the recorded velocity's bob.
                let vel = v.velocity;
                let bob = render::view_bob((vel[0] * vel[0] + vel[1] * vel[1]).sqrt(), v.time);
                // CalcGunAngle: the recorded view (with the damage kick's
                // pitch) before the punch, and the recorded roll.
                let angles = render::viewmodel_angles(&cam, client.punchangle, v.view_angles[2]);
                Some(Viewmodel {
                    mdl,
                    frame: client.weaponframe.max(0) as usize,
                    origin_ofs: render::viewmodel_origin_ofs(angles, bob, d.viewsize),
                    angles,
                })
            }
            _ => None,
        }
    };
    // SCR_CalcRefdef: the same viewsize framing as live play (the C's demo IS
    // the client rendering a recorded stream).
    lap(Phase::Sim);
    let refdef = render::calc_refdef(render_w, render_h, d.viewsize, f.intermission != 0);
    let vrect = refdef.vrect;
    // R_SetupFrame's r_dowarp: a submerged recorded POV renders into the warp
    // buffer (at most 320x200) like live play.
    let eye_contents = crate::world::point_contents(&d.bsp, cam.pos);
    let dowarp = eye_contents <= crate::bsp::CONTENTS_WATER;
    let rvrect = if dowarp {
        crate::screen::warp_vrect(render_w, render_h, d.viewsize, f.intermission != 0, vid.video.hires)
    } else {
        vrect
    };
    let scene = render::Scene {
        colormap: d.colormap.as_deref(),
        time: v.time,
        light_styles: &demo_styles,
        bmodels: &bmodels,
        models: &owned,
        sprites: &sprite_insts,
        particles: &parts,
        viewmodel,
        options: render_options(&rvrect, vid),
        ..render::Scene::new(&d.bsp, cam, rvrect.w, rvrect.h, &d.palette)
    };
    // The screen, backtile around the view rectangle, and the view drawn
    // straight into it — or, submerged, into the warp buffer and then
    // D_WarpScreen'd over the rectangle while it wobbles: the warp applies to
    // the 3-D view FIRST; the content tint joins the deferred whole-screen
    // blend below (V_UpdatePalette order).
    let backtile = backtile_for(&vrect, render_w, render_h, d.gfx_wad.as_ref());
    let mut img = render::screen_with_backtile(vrect, render_w, render_h, backtile.as_ref(), &d.palette);
    if dowarp {
        let view = d.renderer.render(&scene);
        lap(Phase::Render3d);
        d.renderer.warp_into(view, &mut img, vrect, v.time, vid.video.hires);
    } else {
        d.renderer.render_into(&scene, &mut img);
        lap(Phase::Render3d);
    }
    // V_RenderView: the crosshair over the view, before the 2-D layer.
    if let Some(cc) = d.conchars.as_ref().filter(|_| d.crosshair) {
        render::draw_crosshair(&mut img, cc, &vrect, &d.palette);
    }
    lap(Phase::Post3d);
    // A recorded intermission/finale frame draws its overlay exactly like the
    // live walk (SCR_UpdateScreen's cl.intermission branches), gated on the game
    // owning the screen (`key_dest == key_game` — i.e. no menu/console up).
    if f.intermission != 0 && !menu_up {
        match f.intermission {
            1 => {
                if let Some(wad) = d.gfx_wad.as_ref() {
                    let stats = render::IntermissionStats {
                        completed_time: f.completed_time as i32,
                        secrets: f.stats.secrets,
                        total_secrets: f.stats.total_secrets,
                        monsters: f.stats.monsters,
                        total_monsters: f.stats.total_monsters,
                    };
                    render::draw_intermission_overlay(
                        &mut img,
                        wad,
                        &d.palette,
                        d.pic_complete.as_ref(),
                        d.pic_inter.as_ref(),
                        &stats,
                    );
                }
            }
            2 => render::draw_finale_overlay(
                &mut img,
                d.conchars.as_ref(),
                &d.palette,
                d.pic_finale.as_ref(),
                &f.finale_text,
                v.time - f.finale_start,
            ),
            _ => render::draw_finale_overlay(
                &mut img,
                d.conchars.as_ref(),
                &d.palette,
                None,
                &f.finale_text,
                v.time - f.finale_start,
            ),
        }
    } else if let Some(wad) = d.gfx_wad.as_ref() {
        // Status bar from the RECORDED cl.stats (svc_clientdata) — the C's
        // Sbar_Draw runs identically during demo playback, so the attract loop
        // shows the recorded player's health/ammo/armour/items exactly like
        // live play. Drawn under the menu/console like walk_frame's HUD (the C
        // draws the sbar regardless of key_dest; overlays paint on top).
        // draw_hud_into's dead-player branch shows the solo scoreboard when
        // the recorded health hits 0, like Sbar_Draw's scoreboard flip.
        let hud = render::Hud {
            wad,
            palette: &d.palette,
            health: client.health,
            ammo: client.ammo,
            armor: client.armor,
            items: client.items,
            weapon: client.active_weapon,
            ammo_shells: client.shells,
            ammo_nails: client.nails,
            ammo_rockets: client.rockets,
            ammo_cells: client.cells,
            // The recorded server clock (cl.time) drives the weapon-flash
            // cycle + face animation, exactly what sbar.c reads.
            time: v.time,
            item_gettime: Some(&d.item_gettime),
            monsters: f.stats.monsters,
            total_monsters: f.stats.total_monsters,
            secrets: f.stats.secrets,
            total_secrets: f.stats.total_secrets,
            level_name: &d.demo.level_name,
            show_scores: d.show_scores,
            face_pain: v.time <= d.faceanimtime,
            sb_lines: refdef.sb_lines,
        };
        render::draw_hud_into(&mut img, &hud);
    }

    // SCR_DrawPause: a recorded svc_setpause shows the plaque (outside an
    // intermission, whatever key_dest is). (V_RenderView also stops
    // V_CalcRefdef while cl.paused; a recording's pause keeps its recorded
    // view here — id's demos have none.)
    if f.paused && f.intermission == 0 {
        if let Some(pic) = d.pic_pause.as_ref() {
            render::draw_pause(&mut img, pic, &d.palette);
        }
    }

    // On-screen messages from the recorded svc_print / svc_centerprint stream,
    // drawn through the same overlays live play uses, with the same key_dest +
    // intermission gating as walk_frame. Expiries live on the recorded clock.
    if let Some((_, exp)) = &d.centerprint {
        if v.time >= *exp {
            d.centerprint = None;
        }
    }
    if !menu_up && f.intermission == 0 {
        if let Some(cc) = d.conchars.as_ref() {
            if let Some((text, _)) = &d.centerprint {
                render::draw_centerprint(&mut img, cc, &d.palette, text);
            }
            let lines = d.notify.visible(v.time);
            if !lines.is_empty() {
                render::draw_notify(&mut img, cc, &d.palette, &lines);
            }
        }
    }

    // Colour shifts (V_UpdatePalette; cl.cshifts order CONTENTS -> DAMAGE ->
    // BONUS -> POWERUP), all from the RECORDED stream: the eye-contents tint, the
    // svc_damage flash (faded dt*150 per frame like V_UpdatePalette), the
    // stuffed "bf" gold flash (dt*100), and the powerup tint from the
    // recorded cl.items. DEFERRED to the dispatcher so it tints the whole
    // composited frame (HUD + menu + console), like the live walk.
    {
        let sdt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        fade_cshifts(&mut d.damage_blend, &mut d.bonus_blend, sdt, d.stepping, &mut d.fade_clock);
    }
    let mut shifts: Vec<([u8; 3], f32)> = Vec::new();
    if let Some(cs) = render::content_cshift(eye_contents) {
        shifts.push(cs);
    }
    if d.damage_blend > 0.0 {
        shifts.push((d.damage_color, d.damage_blend));
    }
    if d.bonus_blend > 0.0 {
        shifts.push((BONUS_COLOR, d.bonus_blend));
    }
    if let Some(cs) = render::powerup_cshift(client.items) {
        shifts.push(cs);
    }
    lap(Phase::Hud2d);
    ClientFrame { image: img, cshifts: shifts, sound }
}


/// `cls.timedemo`'s bookkeeping (client.h `td_startframe`, `td_starttime`;
/// `td_lastframe`'s one message a frame is [`timedemo_frame`]'s): where the
/// measurement starts, and `CL_FinishTimeDemo`'s line. The host owns it, as
/// `cls` outlives a demo: `td_starttime` keeps its last value until a second
/// frame sets it again.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TimeDemoClock {
    /// `cls.td_startframe`: `host_framecount` when `timedemo` ran.
    pub td_startframe: i64,
    /// `cls.td_starttime` (a float in the C): `realtime` at the start of the
    /// second frame, "so the bogus time on the first frame doesn't count".
    pub td_starttime: f32,
}

impl TimeDemoClock {
    /// `CL_TimeDemo_f`: the measurement starts in this host frame.
    pub fn start(&mut self, host_framecount: i64) {
        self.td_startframe = host_framecount;
    }

    /// `CL_GetMessage` in a timedemo, about to read the host frame's message:
    /// the second frame grabs the real start time.
    pub fn message(&mut self, host_framecount: i64, realtime: f64) {
        if host_framecount == self.td_startframe + 1 {
            self.td_starttime = realtime as f32;
        }
    }

    /// `CL_FinishTimeDemo`'s line, `"%i frames %5.1f seconds %5.1f fps"`, at
    /// `host_framecount` and `realtime` of the frame the demo ended in: the
    /// frames drawn since the first ("the first frame didn't count") and the
    /// time since the second began.
    pub fn finish(&self, host_framecount: i64, realtime: f64) -> String {
        let frames = (host_framecount - self.td_startframe) - 1;
        let mut time = (realtime - self.td_starttime as f64) as f32;
        if time == 0.0 {
            time = 1.0;
        }
        let fps = frames as f32 / time;
        format!("{frames} frames {time:5.1} seconds {fps:5.1} fps")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Uncapped playback draws between messages as `CL_LerpPoint` does; Classic
    /// (and a timedemo) draws each message's own view.
    #[test]
    fn demo_view_blends_toward_the_next_message() {
        let ent = |num, x: f32, yaw| EntSnapshot {
            num,
            modelindex: 1,
            frame: 0,
            skin: 0,
            origin: [x, 0.0, 0.0],
            angles: [0.0, yaw, 0.0],
            effects: 0,
        };
        let mut a = DemoFrame { time: 1.0, view_origin: [0.0, 0.0, 22.0], view_angles: [0.0, 350.0, 0.0], ..Default::default() };
        a.entities = vec![ent(1, 0.0, 350.0), ent(2, 0.0, 0.0), ent(3, 5.0, 0.0)];
        let mut b = DemoFrame { time: 1.05, view_origin: [10.0, 0.0, 22.0], view_angles: [0.0, 10.0, 0.0], ..Default::default() };
        // Entity 1 moves 10 units and turns through north; 2 teleports; 3 is gone.
        b.entities = vec![ent(1, 10.0, 10.0), ent(2, 150.0, 0.0)];
        let frames = [a, b];
        // The clock at 1.025, halfway (`elapsed` counts from the first message).
        let v = demo_view(&frames, 0, 0.025, true);
        assert!((v.time - 1.025).abs() < 1e-6);
        assert!((v.view_origin[0] - 5.0).abs() < 1e-4);
        assert!((v.view_angles[1] - 360.0).abs() < 1e-3, "the short way round: {}", v.view_angles[1]);
        assert!((v.entities[0].origin[0] - 5.0).abs() < 1e-4 && (v.entities[0].angles[1] - 360.0).abs() < 1e-3);
        assert_eq!(v.entities[1].origin[0], 0.0, "a teleport does not blend");
        assert_eq!(v.entities[2].origin[0], 5.0, "an entity the next message lacks stays put");
        let own = demo_view(&frames, 0, 0.025, false);
        assert_eq!((own.time, own.view_origin[0], own.entities[0].origin[0]), (1.0, 0.0, 0.0));
        // A gap past 0.1 s blends over its last 0.1 s only (dropped packets).
        let late = [
            DemoFrame { time: 1.0, ..Default::default() },
            DemoFrame { time: 1.5, view_origin: [10.0, 0.0, 0.0], ..Default::default() },
        ];
        assert_eq!(demo_view(&late, 0, 0.2, true).view_origin[0], 0.0);
        assert!((demo_view(&late, 0, 0.45, true).view_origin[0] - 5.0).abs() < 1e-3);
    }

    #[test]
    fn timedemo_frame_reads_one_message_a_frame_from_the_second() {
        use crate::demo::{Demo, DemoFrame};
        let frame = |t: f32| DemoFrame { time: t, ..Default::default() };
        let demo = Demo {
            level_name: "test".into(),
            static_sounds: Vec::new(),
            model_precache: vec![String::new(), "maps/test.bsp".into()],
            sound_precache: Vec::new(),
            viewentity: 0,
            frames: vec![frame(1.0), frame(1.1), frame(1.1), frame(1.3)],
        };
        let pak = crate::pak::Pak::from_bytes("t".into(), {
            let mut img = b"PACK".to_vec();
            img.extend_from_slice(&12i32.to_le_bytes());
            img.extend_from_slice(&0i32.to_le_bytes());
            img
        })
        .unwrap();
        let mut d = DemoPlay::new(pak, render::demo_room(), [[0u8; 3]; 256], demo);
        let vid = Vid { width: 64, height: 40, display_aspect: 4.0 / 3.0, exact_perspective: false, video: render::VideoCvars::CLASSIC, mip: render::MipCvars::DEFAULT };
        // The first frame (CL_TimeDemo_f's) reads through the second message;
        // the time between messages is not what moves playback on.
        let mut shown = Vec::new();
        while let Some(f) = timedemo_frame(&mut d, 0.5, false, &vid) {
            shown.push((d.idx, d.demo.frames[d.idx].time));
            render::recycle_image(f.image);
        }
        assert_eq!(shown, [(1, 1.1), (2, 1.1), (3, 1.3)]);
        assert!(timedemo_frame(&mut d, 0.5, false, &vid).is_none(), "it stays ended");
    }

    #[test]
    fn default_extension_is_com_default_extension() {
        assert_eq!(default_extension("demo1", ".dem"), "demo1.dem");
        assert_eq!(default_extension("demo1.dem", ".dem"), "demo1.dem");
        assert_eq!(default_extension("mine.old", ".dem"), "mine.old");
        assert_eq!(default_extension("a.b/demo2", ".dem"), "a.b/demo2.dem");
        assert_eq!(default_extension("", ".dem"), ".dem");
        // The C stops at the first character without testing it.
        assert_eq!(default_extension(".x", ".dem"), ".x.dem");
    }

    #[test]
    fn finish_prints_cl_finishtimedemo_line() {
        // timedemo in host frame 10; the second frame (11) starts at 2.5 s;
        // the demo ends in frame 980 at 12.5 s: frames 11..979 were drawn.
        let mut c = TimeDemoClock::default();
        c.start(10);
        c.message(10, 2.0); // the first frame's time doesn't count
        c.message(11, 2.5);
        c.message(12, 2.51);
        assert_eq!(c.td_starttime, 2.5);
        assert_eq!(c.finish(980, 12.5), "969 frames  10.0 seconds  96.9 fps");
        // %5.1f pads to five columns and grows past them.
        assert_eq!(c.finish(980, 1002.5), "969 frames 1000.0 seconds   1.0 fps");
        // `if (!time) time = 1;`
        assert_eq!(c.finish(14, 2.5), "3 frames   1.0 seconds   3.0 fps");
    }
}

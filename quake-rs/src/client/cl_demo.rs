//! cl_demo.c — playing a recorded `.dem`: `CL_PlayDemo_f`'s build of a
//! [`DemoPlay`], quake.rc's demo loop, and the demo client frame rendered like
//! live play: per-frame effect replay (svc_particle, temp entities and their
//! lights, svc_sound/stopsound, svc_damage, prints — cl_parse.c / cl_tent.c /
//! view.c `V_ParseDamage`; the entities' light effects and trails of
//! `CL_RelinkEntities`) and [`demo_frame`], the recorded-POV `V_CalcRefdef` +
//! `SCR_UpdateScreen` for one frame. `timedemo` (`CL_TimeDemo_f`) plays a
//! demo one message per host frame ([`build_timedemo`], [`timedemo_frame`])
//! and [`TimeDemoClock`] keeps `cls.td_*` and prints `CL_FinishTimeDemo`'s
//! line.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Source: `WinQuake/cl_demo.c`.

use crate::bsp::Bsp;
use crate::cd_audio::CdCall;
use crate::demo::{EntSnapshot, parse_demo};
use crate::dlight::{DynamicLight, DynamicLights};
use crate::mdl::Mdl;
use crate::pak::Pak;
use crate::particles::{ParticleSystem, TrailHead, TrailStep};
use crate::render::{self, Camera, ModelInstance, Viewmodel};
use crate::server::EF_MUZZLEFLASH;
use crate::tent::BeamModel;
use crate::wad::Qpic;

use super::cl_tent::{TRAIL_ROCKET, rocket_trail_type, spawn_temp_entity};
use super::host_cmd::IT_INVISIBILITY;
use super::lerpmodels::{self, LerpModels};
use super::lerpmove::LerpMove;
use super::view::{
    BONUS_COLOR, BONUS_PERCENT, FACE_ANIM_TIME, V_KICKTIME, cshift_add, fade_cshifts, parse_damage, stamp_item_gettime,
    stufftext_bonus_flash,
};
use super::{
    ClientFrame, DemoPlay, Listener, Phase, SoundCall, Vid, backtile_for, color_for_name, draw_view, lap,
    render_options, s_update, warp_below,
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
/// and models, one frame per recorded message, played back as id's client
/// does — drawn between the two newest messages ([`demo_frame`]). `None` when
/// the file is missing or unplayable (the C prints "ERROR: couldn't open.").
/// The demo's sounds start through `sound` ([`SoundCall::StopAll`], then the
/// signon's static loops).
pub fn build_demo(pak: Pak, name: &str, sound: &mut Vec<SoundCall>) -> Option<DemoPlay> {
    build_demo_with(pak, name, sound, |bytes| parse_demo(bytes).ok())
}

/// `timedemo <name>`'s load (`CL_TimeDemo_f`): as [`build_demo`], without the
/// frame of the closing `svc_disconnect` ([`crate::demo::parse_demo_timedemo`]).
/// Played by [`timedemo_frame`], one message a frame, each drawn at its own
/// time with no interpolation — what `CL_LerpPoint` gives while
/// `cls.timedemo` is set.
pub fn build_timedemo(pak: Pak, name: &str, sound: &mut Vec<SoundCall>) -> Option<DemoPlay> {
    build_demo_with(pak, name, sound, |bytes| crate::demo::parse_demo_timedemo(bytes).ok())
}

/// The shared load of [`build_demo`] / [`build_timedemo`]: `parse` turns the
/// demo's bytes into its messages.
fn build_demo_with(
    pak: Pak,
    name: &str,
    sound: &mut Vec<SoundCall>,
    parse: impl Fn(&[u8]) -> Option<crate::demo::Demo>,
) -> Option<DemoPlay> {
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let demo = parse(&read(name)?)?;
    let map = demo.map_name()?.to_string();
    let bsp = Bsp::parse(&read(&map)?).ok()?;
    let palette = render::parse_palette(&read("gfx/palette.lmp")?)?;

    // Load a model (alias or sprite) + colour per precache index.
    let mut models = Vec::with_capacity(demo.model_precache.len());
    let mut sprites: Vec<Option<crate::spr::Sprite>> = Vec::with_capacity(demo.model_precache.len());
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
    // Demo committed (nothing below fails): tear down the previous level/mode's
    // looping audio and register the demo signon's `svc_spawnstaticsound` loops
    // (CL_ParseStaticSound ran these on the live client during demo playback
    // too — the e1m3 demo has its own torches).
    sound.push(SoundCall::StopAll);
    sound.push(SoundCall::Static(demo.static_sounds.clone()));
    // The signon's svc_cdtrack: the CD plays the level's track, or the one
    // the demo forces.
    if let Some(track) = demo.cdtrack {
        sound.push(SoundCall::Cd(CdCall::cdtrack(demo_cd_track(&demo, track))));
    }
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

/// The track `CL_ParseServerMessage` hands `CDAudio_Play` for a recorded
/// `svc_cdtrack` of `track` while a demo plays: `(byte)cls.forcetrack` when
/// the demo forces one (id's demo1: track 2), else the recorded track.
fn demo_cd_track(demo: &crate::demo::Demo, track: u8) -> u8 {
    if demo.forcetrack == -1 { track } else { demo.forcetrack as u8 }
}

/// `CL_ParseServerMessage`'s client-side effects of recorded message `idx`,
/// in the frame that reads it, exactly ONCE: `d.last_spawned_idx` records the
/// most recently read message; this is a no-op when it already equals `idx`.
/// `now` is `cl.time` as the message is read: the particles' and beams'
/// lifetimes, the pain face and the item get-times start from it.
///
/// Each `svc_particle` burst replays through [`ParticleSystem::spawn_burst`],
/// except the explosion sentinel (`count >= 1024`, the demo parser's mapping of
/// the net `count == 255`) which routes to [`ParticleSystem::spawn_explosion`]
/// for the 1024-particle fiery burst. Each temp entity replays through the same
/// [`spawn_temp_entity`] mapping the live walk uses (explosion / impact / splash).
/// (`spawn_*` set `die = now + life`.)
fn spawn_demo_frame_effects(d: &mut DemoPlay, idx: usize, time: f64, sound: &mut Vec<SoundCall>) {
    let now = time as f32;
    if d.last_spawned_idx == Some(idx) {
        return; // already spawned this frame's effects; don't double-spawn
    }
    d.last_spawned_idx = Some(idx);
    let Some(frame) = d.demo.frames.get(idx) else { return };
    // CL_ParseClientdata's item get-times.
    stamp_item_gettime(&mut d.cl_items, &mut d.item_gettime, frame.client.items, now);
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
    let cdtrack = frame.cdtrack.map(|t| demo_cd_track(&d.demo, t));
    // svc_setpause: cl.paused against the message before.
    let was_paused = idx.checked_sub(1).and_then(|i| d.demo.frames.get(i)).is_some_and(|f| f.paused);
    let pause = (frame.paused != was_paused).then_some(frame.paused);
    let view_entity_origin = frame.view_entity_origin;
    let view_angles = frame.view_angles;
    for b in &bursts {
        // svc_particle is always R_RunParticleEffect (spawn_burst) in id's
        // CL_ParseParticleEffect — the net count==255 sentinel just means 1024
        // particles (the demo parser already maps it), NOT the rocket
        // R_ParticleExplosion. Route every burst through spawn_burst.
        d.particles.spawn_burst(b.org, b.dir, b.color, b.count, now, &mut d.prng);
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
        if let Some(name) = spawn_temp_entity(&mut d.particles, &mut d.dlights, ev, time, &mut d.prng) {
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
    // svc_cdtrack and svc_setpause: CDAudio_Play, CDAudio_Pause/_Resume.
    if let Some(track) = cdtrack {
        sound.push(SoundCall::Cd(CdCall::cdtrack(track)));
    }
    if let Some(paused) = pause {
        sound.push(SoundCall::Cd(if paused { CdCall::Pause } else { CdCall::Resume }));
    }
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

/// One host frame of demo playback, as id's `CL_ReadFromServer` runs it for a
/// demo: `cl.time` advances by the host frame time `dt`; `CL_GetMessage`
/// reads recorded messages until one is newer than `cl.time` (spawning each
/// one's effects as it is read); `CL_LerpPoint` and `CL_RelinkEntities` draw
/// the camera and every entity between the two newest messages
/// (`cl_relink_entities`); then the frame is drawn (`render_demo_frame`).
/// So the view moves every frame at any frame rate, a message interval
/// behind the recording. After the last message it loops to the first (the
/// host normally starts the next demo instead, `CL_NextDemo`).
///
/// One departure, at the first frame: id's `CL_ClearState` zeroes `cl.time`
/// after `CL_ReadFromServer` has advanced it, so id's first frame starts from
/// 0 where this one starts from `dt`. Both are more than 0.1 s before a real
/// demo's first message, where `CL_LerpPoint` snaps the clock.
pub fn demo_frame(d: &mut DemoPlay, dt: f32, menu_up: bool, vid: &Vid) -> ClientFrame {
    playback_frame(d, dt, menu_up, vid, true)
}

/// [`demo_frame`] without its picture: a host frame a frame-rate cap does not
/// draw (`client::host::FrameCap`), as `cl_main::walk_frame_undrawn` is the
/// live game's — the messages read, the clock, the effects, particles and
/// lights, the fades and the sound as in a drawn frame; only the pixels
/// skipped, and its image empty.
pub fn demo_frame_undrawn(d: &mut DemoPlay, dt: f32, menu_up: bool, vid: &Vid) -> ClientFrame {
    playback_frame(d, dt, menu_up, vid, false)
}

/// [`demo_frame`], its picture drawn or not (`draw`).
fn playback_frame(d: &mut DemoPlay, dt: f32, menu_up: bool, vid: &Vid, draw: bool) -> ClientFrame {
    let mut sound = Vec::new();
    // Con_CheckResize: the notify lines are laid out con_linewidth wide.
    d.notify.check_resize(vid.width, vid.height);
    let n = d.demo.frames.len();
    let dt64 = if dt.is_finite() { f64::from(dt.max(0.0)) } else { 0.0 };
    let started = d.last_spawned_idx.is_some();
    if started && d.idx + 1 >= n && d.time + dt64 > f64::from(d.demo.frames[d.idx].time) {
        // The last message has been drawn and the clock has run past it: play
        // the recording again from its first message, as a fresh playback
        // whose clock starts at 0 (the host normally plays the next demo).
        restart_playback(d);
    } else {
        // CL_ReadFromServer: `cl.oldtime = cl.time; cl.time += host_frametime`.
        d.oldtime = d.time;
        d.time += dt64;
    }
    // CL_GetMessage: the first frame has the signon's last message (the
    // first update); then a message is read whenever `cl.time` has passed
    // the newest one (`cl.time <= cl.mtime[0]`: "don't need another yet").
    let first_read = if d.last_spawned_idx.is_none() { 0 } else { d.idx + 1 };
    spawn_demo_frame_effects(d, d.idx, d.time, &mut sound);
    while d.idx + 1 < n && d.time > f64::from(d.demo.frames[d.idx].time) {
        d.idx += 1;
        spawn_demo_frame_effects(d, d.idx, d.time, &mut sound);
    }
    let f = &d.demo.frames[d.idx];
    if f.disconnect {
        // Host_EndGame: the demo is over, and the frame leaves before
        // CL_RelinkEntities — what id draws (under the loading plaque, which
        // the port does not draw) is the last frame's view at the new clock.
        d.view.time = d.time as f32;
    } else {
        let frac = cl_lerp_point(&mut d.time, [f64::from(f.time), f64::from(f.prev_time)]);
        cl_relink_entities(d, frac, first_read, d.lerpmove);
    }
    // R_DrawParticles and the stair smoothing step by `cl.time - cl.oldtime`.
    let cl_frametime = (d.time - d.oldtime) as f32;
    render_demo_frame(d, dt, cl_frametime, menu_up, vid, sound, d.lerpmodels, draw)
}

/// The port's loop wrap ([`demo_frame`]): the playback starts over at its
/// first message with the clock at 0 and every per-playback effect, message
/// and view state reset, so the replay is identical to the first pass (no
/// stale explosions or bolts carried across the wrap).
fn restart_playback(d: &mut DemoPlay) {
    d.idx = 0;
    d.time = 0.0;
    d.oldtime = 0.0;
    d.particles = ParticleSystem::new();
    d.dlights = DynamicLights::new(); // CL_ClearState: memset (cl_dlights, 0, ...)
    d.trail_org.clear();
    d.beams.clear();
    d.last_spawned_idx = None;
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

/// One host frame of `timedemo` (`cls.timedemo`): `CL_GetMessage` reads
/// exactly one message a frame, whatever the time. The first frame — the one
/// `CL_TimeDemo_f` ran in — reads the whole signon and the message after the
/// one that completed it (frames 0 and 1 here), every later frame the next
/// message; each is drawn at its own time (`CL_LerpPoint` snaps `cl.time` to
/// `mtime[0]`, frac 1: nothing is interpolated). `frametime` is the host
/// frame's `host_frametime` (the view kick, the palette-shift fades and the
/// sound ramps run on it); the particles and the stair smoothing move by
/// `cl.time - cl.oldtime`, the recorded time between the two messages.
/// `None` when there is no next message: the demo has ended (its
/// `svc_disconnect`, `Host_EndGame`, or the file running out,
/// `CL_StopPlayback`) and this frame draws nothing — the host finishes the
/// timedemo ([`TimeDemoClock::finish`]). Built by [`build_timedemo`].
pub fn timedemo_frame(d: &mut DemoPlay, frametime: f32, menu_up: bool, vid: &Vid) -> Option<ClientFrame> {
    timedemo_frame_lerpmodels(d, frametime, menu_up, vid, LerpModels::Classic)
}

/// [`timedemo_frame`], but blending animation frames as `lerpmodels` says
/// instead of always Classic. Exists only so `quaketool timedemo
/// --lerpframe` can measure `r_lerpmodels`' own cost (the vertex pass reads
/// twice the frames): nothing else calls it with anything but
/// [`LerpModels::Classic`], so `timedemo_frame`'s own numbers — "id's own
/// measure agrees", FRAMERATE.md — are untouched.
pub fn timedemo_frame_lerpmodels(
    d: &mut DemoPlay,
    frametime: f32,
    menu_up: bool,
    vid: &Vid,
    lerpmodels: LerpModels,
) -> Option<ClientFrame> {
    let mut sound = Vec::new();
    d.notify.check_resize(vid.width, vid.height);
    let n = d.demo.frames.len();
    let first = d.last_spawned_idx.is_none();
    if first {
        spawn_demo_frame_effects(d, 0, f64::from(d.demo.frames[0].time), &mut sound);
    }
    if d.idx + 1 >= n {
        return None;
    }
    let first_read = if first { 0 } else { d.idx + 1 };
    let oldtime = d.demo.frames[d.idx].time;
    d.idx += 1;
    let now = d.demo.frames[d.idx].time;
    spawn_demo_frame_effects(d, d.idx, f64::from(now), &mut sound);
    d.oldtime = f64::from(oldtime);
    d.time = f64::from(now);
    // No glides either: a timedemo stays id's measure.
    cl_relink_entities(d, 1.0, first_read, LerpMove::Classic);
    Some(render_demo_frame(d, frametime, now - oldtime, menu_up, vid, sound, lerpmodels, true))
}

// ---------------------------------------------------------------------------
// CL_LerpPoint, CL_RelinkEntities (cl_main.c): the frame between two messages
// ---------------------------------------------------------------------------

/// What a demo frame draws that moves between messages — `CL_RelinkEntities`'
/// output: the client clock, the camera and the entities.
#[derive(Clone, Debug, Default)]
pub struct DemoView {
    /// `cl.time` as the frame draws it (after `CL_LerpPoint`).
    pub time: f32,
    /// The relinked view entity's origin raised by `cl.viewheight`: the eye
    /// before `V_CalcRefdef`'s bob and stair smoothing.
    pub view_origin: [f32; 3],
    /// The relinked view entity's origin.
    pub view_entity_origin: [f32; 3],
    /// The relinked view entity's angles: the way the recorded player's model
    /// faces, which aims its muzzle flash's light (not the camera's).
    pub view_entity_angles: [f32; 3],
    /// `cl.viewangles`, lerped between the recorded angles (`cls.demoplayback`).
    pub view_angles: [f32; 3],
    /// `cl.velocity` (the bob and the strafe lean).
    pub velocity: [f32; 3],
    /// The entities drawn: relinked ones where `CL_RelinkEntities` put them
    /// (`origin`, `angles`), statics as recorded.
    pub entities: Vec<EntSnapshot>,
    /// The dynamic lights the frame was drawn with (`R_PushDlights`' set, each
    /// with its slot in the pool), as they stood before `CL_DecayLights` took
    /// the frame's time off them. Filled when the frame is drawn.
    pub dlights: Vec<(usize, DynamicLight)>,
}

/// `CL_LerpPoint`: how far the frame at `cl.time` (`*time`) lies between the
/// two newest messages, `mtime` = `cl.mtime[0..1]`, as a fraction 0..=1. A
/// gap over 0.1 s ("dropped packet, or start of demo") counts as its last
/// 0.1 s; a clock more than 1% outside the interval is pulled back to its
/// nearer end, which is what keeps `cl.time` on the messages' clock (at the
/// start of a demo, it jumps to 0.1 s before the first message). The
/// arithmetic is the C's: `f` and the fraction are floats, the clocks
/// doubles. (id's `cl_nolerp`, `cls.timedemo` and `sv.active` cases, which
/// return 1, are [`timedemo_frame`]'s and the live walk's.)
pub fn cl_lerp_point(time: &mut f64, mtime: [f64; 2]) -> f32 {
    let mut f = (mtime[0] - mtime[1]) as f32;
    if f == 0.0 {
        *time = mtime[0];
        return 1.0;
    }
    let mut mtime1 = mtime[1];
    if f64::from(f) > 0.1 {
        // dropped packet, or start of demo
        mtime1 = mtime[0] - 0.1;
        f = 0.1;
    }
    let frac = ((*time - mtime1) / f64::from(f)) as f32;
    if frac < 0.0 {
        if frac < -0.01 {
            *time = mtime1;
        }
        0.0
    } else if frac > 1.0 {
        if frac > 1.01 {
            *time = mtime[0];
        }
        1.0
    } else {
        frac
    }
}

/// `CL_RelinkEntities`' move of one entity to `frac` of the way from the
/// previous message's position to this one's: straight to this message's
/// when `forcelink` is set, and also when any axis moved more than 100
/// units ("assume a teleportation, not a motion"); the angles the short way
/// round. The arithmetic is the C's (`msg_origins[1] + f*delta`).
pub fn relink(e: &EntSnapshot, frac: f32, forcelink: bool) -> ([f32; 3], [f32; 3]) {
    if forcelink {
        return (e.origin, e.angles);
    }
    let delta: [f32; 3] = std::array::from_fn(|j| e.origin[j] - e.prev_origin[j]);
    #[expect(clippy::manual_range_contains, reason = "id's `delta[j] > 100 || delta[j] < -100`")]
    let f = if delta.iter().any(|&d| d > 100.0 || d < -100.0) { 1.0 } else { frac };
    let origin = std::array::from_fn(|j| e.prev_origin[j] + f * delta[j]);
    (origin, lerp_angles(e.prev_angles, e.angles, f))
}

/// The angle lerp of `CL_RelinkEntities` (the entities' and the demo
/// camera's): each axis turns the short way round, its difference folded
/// into -180..180 before the fraction.
fn lerp_angles(from: [f32; 3], to: [f32; 3], frac: f32) -> [f32; 3] {
    std::array::from_fn(|j| {
        let mut d = to[j] - from[j];
        if d > 180.0 {
            d -= 360.0;
        } else if d < -180.0 {
            d += 360.0;
        }
        from[j] + frac * d
    })
}

/// `CL_RelinkEntities` for demo playback (`cls.demoplayback`): the frame's
/// `cl.velocity`, `cl.viewangles`, the view entity and every entity of the
/// newest message, `frac` of the way from the message before, into
/// `d.view`; `EF_ROTATE` models spin to `anglemod(100*cl.time)`.
///
/// `ent->forcelink` is set by the update of any message this frame read, the
/// ones from `first_read` to `d.idx` (none when `first_read > d.idx`), and
/// cleared once the entity is drawn. So a message's first-sighted or
/// `U_NOLERP` entity is drawn where the message put it in the frame that
/// reads it, and lerps from the message before in the frames after — id's
/// monsters (`U_NOLERP`) jump a message ahead for one frame and fall back.
/// With [`LerpMove::Smooth`] (the slop extra) they glide instead.
fn cl_relink_entities(d: &mut DemoPlay, frac: f32, first_read: usize, lerpmove: LerpMove) {
    let frames = &d.demo.frames;
    let f = &frames[d.idx];
    let read = first_read..=d.idx;
    let earlier = first_read..d.idx;
    // ent->forcelink as the reads left it: set by any update read this frame.
    let forced = |e: &EntSnapshot| {
        read.contains(&d.idx)
            && (e.forcelink
                || earlier.clone().any(|k| frames[k].entities.iter().any(|x| x.num == e.num && x.forcelink)))
    };
    let view_forced = read.clone().any(|k| frames[k].view_forcelink);

    let v = &mut d.view;
    v.time = d.time as f32;
    // "interpolate player info": a plain lerp, no teleport test.
    v.velocity = std::array::from_fn(|i| f.prev_velocity[i] + frac * (f.client.velocity[i] - f.prev_velocity[i]));
    // cls.demoplayback: "interpolate the angles".
    v.view_angles = lerp_angles(f.prev_view_angles, f.view_angles, frac);
    let view_entity = EntSnapshot {
        origin: f.view_entity_origin,
        prev_origin: f.view_prev_origin,
        angles: f.view_entity_angles,
        prev_angles: f.view_prev_entity_angles,
        ..EntSnapshot::default()
    };
    (v.view_entity_origin, v.view_entity_angles) = relink(&view_entity, frac, view_forced);
    v.view_origin = v.view_entity_origin;
    v.view_origin[2] += f.viewheight;

    // bobjrotate = anglemod(100*cl.time), the double product made a float.
    let bobjrotate = crate::math::anglemod((100.0 * d.time) as f32);
    let rotates = |modelindex: usize| {
        d.models.get(modelindex).and_then(Option::as_ref).is_some_and(|m| m.header.flags & crate::demo::EF_ROTATE != 0)
    };
    let smooth = lerpmove == LerpMove::Smooth;
    v.entities.clear();
    for e in &f.entities {
        let mut drawn = *e;
        if smooth && e.step && e.num >= 0 {
            // r_lerpmove (the slop extra): a monster is relinked where its
            // message put it (no U_NOLERP jump back), and glides from step
            // to step where it is drawn.
            let glide = d.glides.draw(e.num, e.modelindex, e.origin, e.angles, d.time);
            (drawn.origin, drawn.angles) = (glide.origin, glide.angles);
        } else if e.num >= 0 {
            (drawn.origin, drawn.angles) = relink(e, frac, forced(e));
        }
        // Rotate binary objects locally. (A static is never relinked in id's;
        // no id1 static spins.)
        if rotates(e.modelindex) {
            drawn.angles[1] = bobjrotate;
        }
        v.entities.push(drawn);
    }
    if smooth {
        d.glides.end_frame();
    } else {
        d.glides.clear();
    }
}

/// Draw the frame `CL_RelinkEntities` left in `d.view` (the newest message,
/// `d.idx`, read and its effects spawned): the recorded POV's
/// `V_CalcRefdef`, `S_Update`, the 3-D view and `SCR_UpdateScreen`'s 2-D
/// layer. `dt` is `host_frametime`; `cl_frametime` is `cl.time - cl.oldtime`,
/// the particles' and lights' decay and the stair smoothing's step (the two are
/// the same frame time in ordinary playback). `lerpmodels` is a parameter rather
/// than read off `d` (like `lerpmove` above it) so [`timedemo_frame`] can hold it to
/// [`LerpModels::Classic`] regardless of `d.lerpmodels`. Not `draw`n
/// ([`demo_frame_undrawn`]): everything but the pixels, and an empty image.
#[allow(clippy::too_many_arguments)]
fn render_demo_frame(
    d: &mut DemoPlay,
    dt: f32,
    cl_frametime: f32,
    menu_up: bool,
    vid: &Vid,
    mut sound: Vec<SoundCall>,
    lerpmodels: LerpModels,
    draw: bool,
) -> ClientFrame {
    let (render_w, render_h) = (vid.width, vid.height);
    // What moves — the clock, the POV, the entities — as the relink left it
    // (taken for the frame, so `d` stays free to mutate; put back at the end).
    let mut v = std::mem::take(&mut d.view);
    let f = &d.demo.frames[d.idx];

    // CL_RelinkEntities' effects, entity by entity in the order of their
    // numbers, exactly as in live play: the light effects (`EF_MUZZLEFLASH`,
    // `EF_BRIGHTLIGHT`, `EF_DIMLIGHT`: [`DynamicLights::relink_effects`]), then
    // the model-flag trail (R_RocketTrail from the entity's previous origin:
    // rocket/lavaball fire, grenade smoke, gib blood, zombie gibs,
    // wizard/knight/vore tracers) and a rocket's light. The lights are made at
    // the origin and angles the entity is relinked at (a light follows its
    // entity between messages) and at the clock `CL_LerpPoint` left. A
    // relinked entity's first sighting (forcelink) starts its trail at its
    // own origin, so an entity absent from this frame is forgotten. Statics
    // are never relinked: no trail, no light.
    d.trail_org.retain(|num, _| v.entities.iter().any(|e| e.num == *num));
    let step = TrailStep { stepping: d.stepping, dt, now: v.time };
    // The view entity is relinked in its place among them (the recorded player
    // is a lit entity like any other: its muzzle flash is the gun's flash), but
    // not drawn, so it is not in `v.entities`. Its recorded `effects` are the
    // newest message's: none when that did not update it. Its model is not
    // recorded (`modelindex` 0: no flags, so no trail and no rocket light — id's
    // player model has none).
    let view_num = d.demo.viewentity as i32;
    let view = (f.view_effects != 0).then(|| EntSnapshot {
        num: view_num,
        origin: v.view_entity_origin,
        angles: v.view_entity_angles,
        effects: f.view_effects,
        ..EntSnapshot::default()
    });
    let (below, above) = v.entities.split_at(v.entities.partition_point(|e| e.num >= 0 && e.num < view_num));
    for e in below.iter().chain(view.as_ref()).chain(above) {
        if e.num < 0 {
            continue;
        }
        d.dlights.relink_effects(e.num, e.origin, e.angles, e.effects, d.time, &mut d.prng);
        let flags = d.models.get(e.modelindex).and_then(|m| m.as_ref()).map_or(0, |m| m.header.flags);
        if let Some(ttype) = rocket_trail_type(flags) {
            let head = d.trail_org.entry(e.num).or_insert(TrailHead::at(e.origin));
            d.particles.spawn_trail(head, e.origin, ttype, step, &mut d.tracercount, &mut d.prng);
            if ttype == TRAIL_ROCKET {
                d.dlights.relink_rocket(e.num, e.origin, d.time);
            }
        }
    }

    let mut owned: Vec<ModelInstance> = Vec::new();
    let mut bmodels: Vec<render::BModelInstance> = Vec::new();
    let mut sprite_insts: Vec<render::SpriteInstance> = Vec::new();
    let smooth_frames = lerpmodels == LerpModels::Smooth;
    for e in v.entities.iter() {
        if let Some(Some(mdl)) = d.models.get(e.modelindex) {
            let frame = e.frame.max(0) as usize;
            // r_lerpmodels (the slop extra): blend this entity's animation,
            // like the live walk (`cl_main::walk_frame`); a static (`e.num <
            // 0`) never gets a new frame, so nothing to blend.
            let blend = if smooth_frames && e.num >= 0 {
                let is_group = mdl.frame_is_group(e.frame);
                // CL_RelinkEntities: the recorded effects byte, which holds
                // for every frame drawn from the message that carried it.
                if e.effects & EF_MUZZLEFLASH != 0 {
                    d.frame_lerps.muzzle_flash(e.num);
                }
                d.frame_lerps.blend(e.num, e.modelindex, frame, is_group, e.origin, d.time)
            } else {
                None
            };
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
                frame,
                blend,
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
                // Demo entities carry full angles (used above for alias
                // models too): a recorded mission-pack door/train rotates in
                // playback exactly as it did live.
                angles: e.angles,
            });
        } else if let Some(Some(spr)) = d.sprites.get(e.modelindex) {
            // Sprite-model entity (the boot demo's s_explod.spr explosion flashes).
            sprite_insts.push(render::SpriteInstance {
                sprite: spr,
                origin: e.origin,
                angles: e.angles,
                frame: e.frame.max(0) as usize,
                // R_DrawEntitiesOnList takes models and sprites in one list.
                models_before: owned.len(),
            });
        }
    }
    // CL_UpdateTEnts: expand the recorded lightning beams into bolt-model
    // pieces, exactly like the live walk. The bolt models resolve through the
    // demo's PRECACHE table (the .dem signon lists progs/bolt*.mdl); a beam
    // owned by the recorded view entity tracks its per-frame origin.
    if d.beams.any_live(v.time) {
        d.beams.update(v.time, d.demo.viewentity as i32, v.view_entity_origin, &mut d.prng, &mut d.beam_scratch);
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
                    blend: None,
                    skinnum: 0,
                });
            }
        }
    }
    // The recorded per-client state (svc_clientdata) drives V_CalcRefdef.
    let client = f.client;
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
        // walk_frame's, driven by the recorded onground flag + the relinked
        // view entity origin z, stepping by `steptime = cl.time - cl.oldtime`
        // (0 if negative).
        let origin_z = v.view_entity_origin[2];
        let sdt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        let steptime = if cl_frametime.is_finite() { cl_frametime.max(0.0) } else { 0.0 };
        if d.oldz.is_finite() && client.onground && origin_z - d.oldz > 0.0 {
            d.oldz += steptime * 80.0;
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
        let mut roll_angle = v.view_angles[2] + crate::server::v_calc_roll(basis, vel);
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
        let listener = Listener { pos: v.view_origin, forward: [cy, sy, 0.0], right: [sy, -cy, 0.0] };
        sound.push(s_update(&d.bsp, listener, dt));
    }
    // The recorded server time animates the demo's liquids/sky too. The live
    // particle pool (replayed from the recorded svc_particle / temp-entity
    // stream) is passed as (world pos, palette index) so blood/puffs/explosions
    // draw into the scene sharing its z-buffer. The dynamic lights are the
    // pool's (`R_PushDlights`: not past `die`, with a radius), at the radius
    // they were made with: `CL_DecayLights` shrinks them after the frame is
    // drawn (below), as in walk_frame.
    v.dlights.clear();
    v.dlights.extend(d.dlights.pushed(d.time));
    let lights: Vec<DynamicLight> = v.dlights.iter().map(|&(_, l)| l).collect();
    // R_DrawParticles' order, as in walk_frame: retire (`die < cl.time`), draw,
    // then move and ramp.
    d.particles.retire(v.time);
    let parts: Vec<([f32; 3], u8)> = d.particles.particles().iter().map(|p| (p.origin, p.color)).collect();
    // `grav = frametime * sv_gravity.value * 0.05`: R_DrawParticles reads the
    // client's own sv_gravity cvar in playback too — 800, or what the last map
    // played set it to (e1m8's worldspawn: 100), not the recording's.
    if cl_frametime.is_finite() && cl_frametime > 0.0 {
        d.particles.integrate(cl_frametime, v.time, d.sv_gravity * 0.05);
    }
    // The RECORDED svc_lightstyle table drives the world lighting through the
    // same R_AnimateLight 10 Hz logic the live walk uses (lightstyle_scales_at,
    // on the double clock `d.time`, whose float is `v.time`) — in Classic the
    // demo's flicker matches the recording exactly — stepped or gliding as
    // `r_lerplightstyles` says, as live play is. (Of the attract loop, the
    // first thing a visitor sees, only demo3's views hold an animated light;
    // the steady torches' flicker, `r_torchflicker`, is the renderer's, and
    // lights all three.) A synthetic demo without a table (tests) falls back
    // to the previous seeded default: style 0 = 'm' (264/256, id's
    // steady-world brightness), the rest neutral.
    let demo_styles = if f.lightstyles.is_empty() {
        let mut s = render::NEUTRAL_LIGHTSTYLE_SCALES;
        s[0] = 264.0 / 256.0;
        s
    } else {
        crate::server::lightstyle_scales_at(&f.lightstyles, d.time, vid.video.lightstyles)
    };
    // The first-person weapon viewmodel: SU_WEAPON is the model PRECACHE index
    // (`view->model = cl.model_precache[cl.stats[STAT_WEAPON]]`, V_CalcRefdef),
    // SU_WEAPONFRAME its animation frame. Hidden exactly like R_DrawViewModel
    // (r_main.c ~606): invisible POV (Ring of Shadows), dead POV, or an
    // intermission (V_CalcIntermissionRefdef sets `view->model = NULL`).
    let hide_gun = f.intermission != 0 || client.health <= 0 || client.items & IT_INVISIBILITY != 0;
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
                let weapon_frame = client.weaponframe.max(0) as usize;
                // r_lerpmodels: the recorded gun blends too, under
                // lerpmodels::VIEWMODEL's sentinel key like the live walk's
                // (the weapon precache index is a stable model identity —
                // unlike the live walk, the demo stream's weapon model IS
                // already an index, no name to hash).
                let blend = if smooth_frames {
                    let model_id = client.weapon_model.max(0) as usize;
                    let is_group = mdl.frame_is_group(client.weaponframe);
                    // The recorded player's flash is the gun's (`f.view_effects`).
                    if f.view_effects & EF_MUZZLEFLASH != 0 {
                        d.frame_lerps.muzzle_flash(lerpmodels::VIEWMODEL);
                    }
                    d.frame_lerps.blend(lerpmodels::VIEWMODEL, model_id, weapon_frame, is_group, cam.pos, d.time)
                } else {
                    None
                };
                Some(Viewmodel {
                    mdl,
                    frame: weapon_frame,
                    blend,
                    origin_ofs: render::viewmodel_origin_ofs(angles, bob, d.viewsize),
                    angles,
                })
            }
            _ => None,
        }
    };
    // Every blend this frame draws has been asked for, the entities' and the
    // view weapon's: forget the rest (and any flash nothing drew).
    if smooth_frames {
        d.frame_lerps.end_frame();
    } else {
        d.frame_lerps.clear();
    }
    // SCR_CalcRefdef: the same viewsize framing as live play (the C's demo IS
    // the client rendering a recorded stream).
    lap(Phase::Sim);
    let refdef = render::calc_refdef(render_w, render_h, d.viewsize, f.intermission != 0, d.sbar_layout);
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
        dlights: &lights,
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
    // blend below (V_UpdatePalette order). slop's status bar overlay goes on
    // drawing the world under the view, as live play does.
    let mut img = if draw {
        let backtile = backtile_for(&vrect, render_w, render_h, d.gfx_wad.as_ref());
        render::screen_with_backtile(vrect, render_w, render_h, backtile.as_ref())
    } else {
        render::Image::new(0, 0, 0)
    };
    if !draw {
        // Undrawn: no 3-D view, crosshair or 2-D layer (below).
    } else if dowarp {
        let below = warp_below(&refdef, vid);
        let view = d.renderer.render_extended(&scene, below);
        lap(Phase::Render3d);
        d.renderer.warp_into(view, &mut img, vrect, below, v.time, vid.video.hires);
    } else {
        draw_view(&mut d.renderer, &scene, &refdef, &mut img);
        lap(Phase::Render3d);
    }
    // Host_Frame runs CL_DecayLights after SCR_UpdateScreen: `radius -=
    // (cl.time - cl.oldtime) * decay` — the clock the messages gave this
    // frame, which is the host's step in ordinary playback.
    d.dlights.advance(cl_frametime, d.time);
    // V_RenderView: the crosshair over the view, before the 2-D layer — but
    // not over an intermission or finale, which id's GLQuake leaves it off
    // (gl_screen.c's SCR_UpdateScreen draws it only outside them): WinQuake
    // draws it there too, over the level's stats, with nothing to aim at.
    // (`crosshair` is a slop setting; Classic draws none.)
    if draw && f.intermission == 0 {
        render::draw_crosshair(&mut img, d.crosshair, d.conchars.as_ref(), &vrect);
    }
    lap(Phase::Post3d);
    // A recorded intermission/finale frame draws its overlay exactly like the
    // live walk (SCR_UpdateScreen's cl.intermission branches), gated on the game
    // owning the screen (`key_dest == key_game` — i.e. no menu/console up).
    if !draw {
        // Undrawn: no status bar or overlay.
    } else if f.intermission != 0 && !menu_up {
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
                        d.pic_complete.as_ref(),
                        d.pic_inter.as_ref(),
                        &stats,
                    );
                }
            }
            2 => render::draw_finale_overlay(
                &mut img,
                d.conchars.as_ref(),
                d.pic_finale.as_ref(),
                &f.finale_text,
                v.time - f.finale_start,
            ),
            _ => render::draw_finale_overlay(
                &mut img,
                d.conchars.as_ref(),
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
            // Demo playback has no live progs.dat to detect a mode from (the
            // recorded wire messages are all `draw_hud_into` reads); no
            // mission-pack demo is in scope here, so this is always id1.
            mode: crate::server::GameMode::Id1,
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
            sbar_layout: d.sbar_layout,
        };
        render::draw_hud_into(&mut img, &hud);
    }

    // SCR_DrawPause: a recorded svc_setpause shows the plaque (outside an
    // intermission, whatever key_dest is). (V_RenderView also stops
    // V_CalcRefdef while cl.paused; a recording's pause keeps its recorded
    // view here — id's demos have none.)
    if draw && f.paused && f.intermission == 0 {
        if let Some(pic) = d.pic_pause.as_ref() {
            render::draw_pause(&mut img, pic);
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
    if draw && !menu_up && f.intermission == 0 {
        if let Some(cc) = d.conchars.as_ref() {
            if let Some((text, _)) = &d.centerprint {
                render::draw_centerprint(&mut img, cc, text);
            }
            let lines = d.notify.visible(v.time);
            if !lines.is_empty() {
                render::draw_notify(&mut img, cc, &lines, render::notify_top(d.show_fps));
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
    d.view = v;
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

    use crate::demo::{Demo, DemoFrame};
    use crate::server::{EF_BRIGHTLIGHT, EF_DIMLIGHT};

    /// A playback of `frames` over the test room, with no assets.
    fn playback(frames: Vec<DemoFrame>) -> DemoPlay {
        playback_in(render::demo_room(), [[0u8; 3]; 256], frames)
    }

    /// A playback of `frames` over `bsp`, drawn with `palette`, with no assets.
    fn playback_in(bsp: Bsp, palette: [[u8; 3]; 256], frames: Vec<DemoFrame>) -> DemoPlay {
        let demo = Demo {
            level_name: "test".into(),
            static_sounds: Vec::new(),
            model_precache: vec![String::new(), "maps/test.bsp".into()],
            sound_precache: Vec::new(),
            viewentity: 0,
            forcetrack: -1,
            cdtrack: None,
            frames,
        };
        let pak = crate::pak::Pak::from_bytes("t".into(), {
            let mut img = b"PACK".to_vec();
            img.extend_from_slice(&12i32.to_le_bytes());
            img.extend_from_slice(&0i32.to_le_bytes());
            img
        })
        .unwrap();
        DemoPlay::new(pak, bsp, palette, demo)
    }

    const VID: Vid = Vid {
        width: 64,
        height: 40,
        display_aspect: 4.0 / 3.0,
        persp_span: render::PerspSpan::Spans16,
        video: render::VideoCvars::CLASSIC,
        mip: render::MipCvars::DEFAULT,
    };

    /// Entity `num` moved from `from` to `to` (x) this message.
    fn moved(num: i32, from: f32, to: f32) -> EntSnapshot {
        EntSnapshot { num, modelindex: 1, origin: [to, 0.0, 0.0], prev_origin: [from, 0.0, 0.0], ..Default::default() }
    }

    #[test]
    fn cl_lerp_point_is_ids() {
        let lerp = |time: f64, mtime: [f64; 2]| {
            let mut t = time;
            (cl_lerp_point(&mut t, mtime), t)
        };
        // Zero interval: snap to mtime[0] (frac 1).
        assert_eq!(lerp(1.7, [2.0, 2.0]), (1.0, 2.0));
        // Halfway through a 0.1 s interval; its ends.
        let mt = [1.6, 1.5];
        assert!((lerp(1.55, mt).0 - 0.5).abs() < 1e-5);
        assert_eq!(lerp(1.5, mt), (0.0, 1.5));
        assert_eq!(lerp(1.6, mt), (1.0, 1.6));
        // Out of range: clamped; pulled back to the nearer end when more than
        // 1% out (the frac is the float of the double division).
        assert_eq!(lerp(1.4995, mt), (0.0, 1.4995));
        assert_eq!(lerp(1.4, mt), (0.0, 1.5));
        assert_eq!(lerp(1.6005, mt), (1.0, 1.6005));
        assert_eq!(lerp(1.7, mt), (1.0, 1.6));
        // A gap over 0.1 s is its last 0.1 s ("dropped packet, or start of
        // demo"): the start of a demo jumps to 0.1 s before its first message.
        let big = [5.0, 4.0];
        assert!((lerp(4.95, big).0 - 0.5).abs() < 1e-4);
        assert_eq!(lerp(0.0, big), (0.0, 4.9));
    }

    #[test]
    fn relink_lerps_snaps_teleports_and_honours_forcelink() {
        let e = moved(1, 0.0, 10.0);
        assert_eq!(relink(&e, 0.5, false).0, [5.0, 0.0, 0.0]);
        assert_eq!(relink(&e, 0.5, true).0, [10.0, 0.0, 0.0], "forcelink: where the message put it");
        // More than 100 units on an axis is a teleport: straight to the new
        // place (f = 1); exactly 100 still lerps.
        assert_eq!(relink(&moved(1, 0.0, 150.0), 0.25, false).0, [150.0, 0.0, 0.0]);
        assert_eq!(relink(&moved(1, 0.0, 100.0), 0.5, false).0, [50.0, 0.0, 0.0]);
        // The angles turn the short way round: 350 -> 10 through 360.
        let turn = EntSnapshot { angles: [0.0, 10.0, 0.0], prev_angles: [0.0, 350.0, 0.0], ..Default::default() };
        assert!((relink(&turn, 0.5, false).1[1] - 360.0).abs() < 1e-4);
        let back = EntSnapshot { angles: [0.0, 350.0, 0.0], prev_angles: [0.0, 10.0, 0.0], ..Default::default() };
        assert!(relink(&back, 0.5, false).1[1].abs() < 1e-4);
    }

    /// id's `CL_ReadFromServer` at 72 Hz over messages 0.1 s apart: the clock
    /// reads a message as soon as it has passed the newest one, and draws the
    /// camera, the velocity and the entities between the two newest.
    #[test]
    fn demo_frame_reads_ahead_and_draws_between_the_two_newest_messages() {
        let msg = |time: f32, prev_time: f32, x: f32, prev_x: f32| DemoFrame {
            time,
            prev_time,
            view_entity_origin: [x, 0.0, 0.0],
            view_prev_origin: [prev_x, 0.0, 0.0],
            viewheight: 22.0,
            view_angles: [0.0, 10.0, 0.0],
            prev_view_angles: [0.0, 350.0, 0.0],
            prev_velocity: [0.0; 3],
            client: crate::demo::DemoClientData { velocity: [100.0, 0.0, 0.0], ..Default::default() },
            entities: vec![moved(5, prev_x, x)],
            ..Default::default()
        };
        let mut d = playback(vec![msg(1.0, 0.9, 0.0, 0.0), msg(1.1, 1.0, 10.0, 0.0), msg(1.2, 1.1, 20.0, 10.0)]);
        // The first frame starts more than 0.1 s before the first message:
        // CL_LerpPoint pulls the clock to 0.1 s before it (frac 0).
        render::recycle_image(demo_frame(&mut d, 1.0 / 72.0, false, &VID).image);
        assert_eq!((d.idx, d.time), (0, 0.9));
        // Seven 1/72 s frames later the clock (0.997) has not passed message 0.
        for _ in 0..7 {
            render::recycle_image(demo_frame(&mut d, 1.0 / 72.0, false, &VID).image);
        }
        assert_eq!(d.idx, 0);
        // The next frame (1.008) passes it: message 1 is read, and the frame
        // draws 8% of the way from message 0 to 1.
        render::recycle_image(demo_frame(&mut d, 1.0 / 72.0, false, &VID).image);
        assert_eq!(d.idx, 1);
        let frac = ((d.time - 1.0) / 0.1) as f32;
        assert!(frac > 0.0 && frac < 0.2, "{frac}");
        let v = &d.view;
        assert!((v.view_entity_origin[0] - 10.0 * frac).abs() < 1e-3);
        assert!((v.view_origin[2] - 22.0).abs() < 1e-6, "the eye is raised by the view height");
        assert!((v.entities[0].origin[0] - 10.0 * frac).abs() < 1e-3);
        assert!((v.velocity[0] - 100.0 * frac).abs() < 1e-3);
        let yaw = (v.view_angles[1] - 350.0 - 20.0 * frac).abs();
        assert!(yaw < 1e-3, "the recorded angles turn the short way: {:?}", v.view_angles);
        // Every frame moves the view: the next one is further along.
        let x = v.view_entity_origin[0];
        render::recycle_image(demo_frame(&mut d, 1.0 / 72.0, false, &VID).image);
        assert!(d.view.view_entity_origin[0] > x);
    }

    /// The demo animates its RECORDED light styles through the live walk's
    /// `lightstyle_scales_at`, stepped or gliding as the video cvars say
    /// (`r_lerplightstyles`): on a whole tenth the two frames are the same,
    /// halfway through one they are not.
    #[test]
    fn the_recorded_light_styles_step_or_glide_as_the_cvars_say() {
        // Style 1 (every wall's second lightmap) toggles 'a' / 'z' each tenth.
        let msg = |n: u8| DemoFrame {
            time: f32::from(n) / 10.0,
            prev_time: f32::from(n - 1) / 10.0,
            viewheight: 22.0,
            lightstyles: std::rc::Rc::new(vec!["m".into(), "az".into()]),
            ..Default::default()
        };
        let room = render::fixtures::lightmapped_demo_room(100, 200);
        let mut d = playback_in(room, render::fixtures::ramp_palette(), (10..14).map(msg).collect());
        d.viewsize = 120.0; // no status bar: the whole 64x40 is the view
        let smooth = Vid {
            video: render::VideoCvars {
                lightstyles: crate::server::LerpLightStyles::Smooth,
                ..render::VideoCvars::CLASSIC
            },
            ..VID
        };
        // The first frame pulls the clock to 0.1 s before the first message:
        // 0.9, tenth 9 ('z'). A dt of 0 draws the same frame again.
        let stepped = demo_frame(&mut d, 1.0 / 72.0, false, &VID).image;
        assert_eq!(d.time, 0.9);
        let glided = demo_frame(&mut d, 0.0, false, &smooth).image;
        assert!(stepped.pixels == glided.pixels, "on the tenth: id's frame");
        // 0.95: id's 'z' still, the glide halfway to 'a'.
        let glided = demo_frame(&mut d, 0.05, false, &smooth).image;
        let stepped = demo_frame(&mut d, 0.0, false, &VID).image;
        assert!((d.time - 0.95).abs() < 1e-6, "{}", d.time);
        assert!(stepped.pixels != glided.pixels, "mid-tenth the glide is between the letters");
    }

    /// The demo's steady torches flicker as the live game's do
    /// (`r_torchflicker`, the video cvars'): the renderer finds them in the
    /// map's entity lump and lights them by the demo's clock, so playback
    /// moves the light — and with the extra off it is id's frame.
    #[test]
    fn the_steady_torches_flicker_in_playback_as_the_cvars_say() {
        let msg = |n: u8| DemoFrame {
            time: f32::from(n) / 10.0,
            prev_time: f32::from(n - 1) / 10.0,
            viewheight: 22.0,
            lightstyles: std::rc::Rc::new(vec!["m".into()]),
            ..Default::default()
        };
        let mut room = render::fixtures::lightmapped_demo_room(120, 0);
        room.entities =
            "{ \"classname\" \"worldspawn\" }\n{ \"classname\" \"light_flame_large_yellow\" \"origin\" \"64 0 -64\" }"
                .into();
        let mut d = playback_in(room, render::fixtures::ramp_palette(), (10..40).map(msg).collect());
        d.viewsize = 120.0;
        let lit = Vid {
            video: render::VideoCvars { torches: render::TorchFlicker::STYLE, ..render::VideoCvars::CLASSIC },
            ..VID
        };
        let mut moved = 0;
        for f in 0..30 {
            let flicker = demo_frame(&mut d, if f == 0 { 1.0 / 72.0 } else { 0.05 }, false, &lit).image;
            let id = demo_frame(&mut d, 0.0, false, &VID).image;
            moved += usize::from(flicker.pixels != id.pixels);
        }
        assert!(moved > 20, "the flame's light moves in playback ({moved} of 30)");
    }

    /// `U_NOLERP` (id's monsters): drawn where the message put them in the
    /// frame that reads it, then lerped from the message before until the
    /// next one — id's step jumps a message ahead for a frame.
    #[test]
    fn a_nolerp_entity_snaps_in_the_reading_frame_then_lerps() {
        let step = |time: f32, x: f32, prev_x: f32| DemoFrame {
            time,
            prev_time: time - 0.1,
            entities: vec![EntSnapshot { forcelink: true, step: true, ..moved(5, prev_x, x) }],
            ..Default::default()
        };
        let mut d = playback(vec![step(1.0, 0.0, 0.0), step(1.1, 8.0, 0.0), step(1.2, 16.0, 8.0)]);
        let mut xs = Vec::new();
        for _ in 0..16 {
            render::recycle_image(demo_frame(&mut d, 1.0 / 72.0, false, &VID).image);
            xs.push((d.idx, d.view.entities[0].origin[0]));
        }
        let read = xs.iter().position(|&(idx, _)| idx == 1).expect("message 1 is read");
        assert_eq!(xs[read].1, 8.0, "the reading frame draws the new step");
        assert!(xs[read + 1].1 < 2.0, "the next frame falls back to lerping from 0: {xs:?}");
    }

    /// With `r_lerpmove` the same monster glides forward every frame
    /// instead (the slop extra; `client::lerpmove`).
    #[test]
    fn with_lerpmove_a_nolerp_entity_glides() {
        let step = |time: f32, x: f32, prev_x: f32| DemoFrame {
            time,
            prev_time: time - 0.1,
            entities: vec![EntSnapshot { forcelink: true, step: true, ..moved(5, prev_x, x) }],
            ..Default::default()
        };
        let mut d =
            playback(vec![step(1.0, 0.0, 0.0), step(1.1, 8.0, 0.0), step(1.2, 16.0, 8.0), step(1.3, 24.0, 16.0)]);
        d.lerpmove = LerpMove::Smooth;
        let mut xs = Vec::new();
        // (Frame 29 would pass the last message: the port's loop wrap.)
        for _ in 0..28 {
            render::recycle_image(demo_frame(&mut d, 1.0 / 72.0, false, &VID).image);
            xs.push(d.view.entities[0].origin[0]);
        }
        let first = xs.iter().position(|&x| x > 0.0).expect("it moves");
        assert!(xs[first..].windows(2).all(|w| w[1] > w[0]), "forward every frame: {xs:?}");
    }

    /// A model with no poses at all: `r_lerpmodels` keys on the model index
    /// and the frame number, and the frame-blend probes below never look
    /// inside.
    fn poseless_model() -> Mdl {
        use crate::mdl::MdlHeader;
        let header = MdlHeader {
            ident: 0,
            version: 6,
            scale: [1.0; 3],
            scale_origin: [0.0; 3],
            boundingradius: 0.0,
            eyeposition: [0.0; 3],
            numskins: 0,
            skinwidth: 0,
            skinheight: 0,
            numverts: 0,
            numtris: 0,
            numframes: 0,
            synctype: 0,
            flags: 0,
            size: 0.0,
        };
        Mdl { header, skins: Vec::new(), stverts: Vec::new(), triangles: Vec::new(), frames: Vec::new() }
    }

    /// What the blend of `key` (entity number, or the view weapon's) is
    /// right now, asked after the frame is drawn: the same call the draw
    /// made, so it changes nothing. `Some` while blending.
    fn blending(d: &mut DemoPlay, key: i32, frame: usize) -> bool {
        d.frame_lerps.blend(key, 1, frame, false, [0.0; 3], d.time).is_some()
    }

    /// Playback of a recorded flash: the monster's attack frame (message 1)
    /// and the recorded player's gun (`view_effects`) snap, and so does the
    /// pose change after; the one after that blends. The flash is the
    /// message's, so every frame drawn from it snaps, as `CL_RelinkEntities`
    /// re-raises it each frame while the entity's effects byte holds it.
    #[test]
    fn a_recorded_muzzle_flash_snaps_the_animation_for_two_changes() {
        use crate::demo::DemoClientData;
        // Message k: the monster (entity 5) at frame `k`, flashing in message 1 only; the gun
        // at weapon frame `k`, flashing with it.
        let msg = |k: i32| {
            let flash = if k == 1 { EF_MUZZLEFLASH } else { 0 };
            DemoFrame {
                time: 1.0 + 0.1 * k as f32,
                prev_time: 0.9 + 0.1 * k as f32,
                entities: vec![EntSnapshot { frame: k, effects: flash, ..moved(5, 0.0, 0.0) }],
                view_effects: flash,
                client: DemoClientData { health: 100, weapon_model: 1, weaponframe: k, ..Default::default() },
                ..Default::default()
            }
        };
        let mut d = playback((0..5).map(msg).collect());
        d.models = vec![None, Some(poseless_model())];
        d.lerpmodels = LerpModels::Smooth;
        // (message, monster blending, gun blending) as each frame leaves them.
        let mut seen: Vec<(usize, bool, bool)> = Vec::new();
        for _ in 0..40 {
            if d.idx + 1 >= d.demo.frames.len() {
                break; // the loop wrap: not under test
            }
            render::recycle_image(demo_frame(&mut d, 1.0 / 72.0, false, &VID).image);
            let k = d.idx as usize;
            seen.push((k, blending(&mut d, 5, k), blending(&mut d, lerpmodels::VIEWMODEL, k)));
        }
        let of = |k: usize| -> Vec<(bool, bool)> { seen.iter().filter(|s| s.0 == k).map(|s| (s.1, s.2)).collect() };
        assert!(of(1).len() > 3 && of(2).len() > 3 && of(3).len() > 3, "frames drawn from messages 1..=3: {seen:?}");
        assert!(of(1).iter().all(|&b| b == (false, false)), "the flash message snaps: {seen:?}");
        assert!(of(2).iter().all(|&b| b == (false, false)), "the change after it snaps: {seen:?}");
        assert_eq!(of(3).first(), Some(&(true, true)), "the third blends, both: {seen:?}");
    }

    #[test]
    fn ef_rotate_models_spin_to_100_times_the_clock() {
        use crate::mdl::MdlHeader;
        let mut d = playback(vec![DemoFrame {
            time: 1.5,
            prev_time: 1.5,
            entities: vec![EntSnapshot { num: 2, modelindex: 1, angles: [0.0, 30.0, 0.0], ..Default::default() }],
            ..Default::default()
        }]);
        // Only the header's flags matter to the relink.
        let header = MdlHeader {
            ident: 0,
            version: 6,
            scale: [1.0; 3],
            scale_origin: [0.0; 3],
            boundingradius: 0.0,
            eyeposition: [0.0; 3],
            numskins: 0,
            skinwidth: 0,
            skinheight: 0,
            numverts: 0,
            numtris: 0,
            numframes: 0,
            synctype: 0,
            flags: crate::demo::EF_ROTATE,
            size: 0.0,
        };
        let spinner = Mdl { header, skins: Vec::new(), stverts: Vec::new(), triangles: Vec::new(), frames: Vec::new() };
        d.models = vec![None, Some(spinner)];
        d.time = 1.5;
        cl_relink_entities(&mut d, 1.0, 0, LerpMove::Classic);
        assert_eq!(d.view.entities[0].angles[1], crate::math::anglemod(150.0));
        d.models = Vec::new();
        cl_relink_entities(&mut d, 1.0, 0, LerpMove::Classic);
        assert_eq!(d.view.entities[0].angles[1], 30.0, "no EF_ROTATE: the recorded yaw");
    }

    // ----- Dynamic lights in playback (CL_RelinkEntities, CL_ParseTEnt) -----

    /// Entity `num` at `origin`, facing `angles`, carrying `effects`, unmoved.
    fn lit(num: i32, origin: [f32; 3], angles: [f32; 3], effects: i32) -> EntSnapshot {
        EntSnapshot {
            num,
            modelindex: 1,
            origin,
            prev_origin: origin,
            angles,
            prev_angles: angles,
            effects,
            ..Default::default()
        }
    }

    /// The `n`th message, `n * 0.1 + 1.0` s, carrying `entities`.
    fn message(n: usize, entities: Vec<EntSnapshot>) -> DemoFrame {
        DemoFrame { time: 1.0 + 0.1 * n as f32, prev_time: 0.9 + 0.1 * n as f32, entities, ..Default::default() }
    }

    /// A playback of `frames` with the unflagged model on precache index 1, as
    /// a fresh playback's generator (`DemoPlay::new`'s seed) would jitter it.
    fn lit_playback(frames: Vec<DemoFrame>) -> DemoPlay {
        let mut d = playback(frames);
        d.models = vec![None, Some(poseless_model())];
        d
    }

    fn draw(d: &mut DemoPlay) {
        render::recycle_image(demo_frame(d, 1.0 / 72.0, false, &VID).image);
    }

    /// The light a recorded muzzle flash makes is the one the live client makes
    /// for an entity in that state: `DynamicLights::relink_effects`, at the
    /// origin and angles the entity is relinked at, with the clock the frame
    /// is drawn at, and the same draw of `rand()` for its radius.
    #[test]
    fn a_recorded_muzzle_flash_lights_the_world_like_a_live_one() {
        let mut d =
            lit_playback(vec![message(0, vec![lit(5, [100.0, 200.0, 50.0], [0.0, 90.0, 0.0], EF_MUZZLEFLASH)])]);
        let mut seed = d.prng;
        draw(&mut d);
        let now = d.time; // cl.time, the double
        let mut live = DynamicLights::new();
        live.relink_effects(5, [100.0, 200.0, 50.0], [0.0, 90.0, 0.0], EF_MUZZLEFLASH, now, &mut seed);
        assert_eq!(d.view.dlights, live.pushed(now).collect::<Vec<_>>(), "the same light, slot and all");
        let (_, l) = d.view.dlights[0];
        assert_eq!((l.key(), l.minlight, l.decay), (5, 32.0, 0.0));
        assert!((200.0..232.0).contains(&l.radius), "200 + (rand()&31): {}", l.radius);
        assert_eq!(l.die, (now + 0.1) as f32, "die = (float)(cl.time + 0.1)");
        // 16 up and 18 along the way it faces (+y here).
        assert!((l.origin[0] - 100.0).abs() < 1e-3 && (l.origin[1] - 218.0).abs() < 1e-3, "{:?}", l.origin);
        assert!((l.origin[2] - 66.0).abs() < 1e-3);
        // The pool carries it: the same light is what the next draw sees.
        assert_eq!(d.dlights.active_count(now), 1);
    }

    /// The light follows its entity between messages: it is made at the
    /// relinked (interpolated) origin, not where either message put it.
    #[test]
    fn a_light_follows_its_entity_between_messages() {
        let walking = |n: usize, from: f32, to: f32| {
            let mut e = lit(5, [to, 0.0, 0.0], [0.0; 3], EF_DIMLIGHT);
            e.prev_origin = [from, 0.0, 0.0];
            message(n, vec![e])
        };
        let mut d = lit_playback(vec![walking(0, 0.0, 0.0), walking(1, 0.0, 50.0), walking(2, 50.0, 100.0)]);
        let mut seen = Vec::new();
        for _ in 0..20 {
            draw(&mut d);
            let drawn = d.view.entities[0].origin[0];
            let (_, l) = d.view.dlights.iter().find(|(_, l)| l.key() == 5).expect("a dim light each frame");
            assert_eq!(l.origin, [drawn, 0.0, 0.0], "at the entity's relinked origin, not above it (EF_DIMLIGHT)");
            seen.push(drawn);
        }
        assert!(seen.windows(2).any(|w| w[1] > w[0] && w[1] < 50.0), "it moved between messages: {seen:?}");
    }

    /// The recorded player (the view entity) is lit like any entity, from its
    /// own angles, not the camera's, and takes its place among the others'
    /// numbers: slots are handed out in entity order.
    #[test]
    fn the_view_entity_flashes_from_its_own_angles_in_its_place() {
        let mut m = message(0, vec![lit(7, [0.0; 3], [0.0; 3], EF_DIMLIGHT), lit(3, [0.0; 3], [0.0; 3], EF_DIMLIGHT)]);
        m.entities.sort_by_key(|e| e.num);
        m.view_entity_origin = [10.0, 20.0, 30.0];
        m.view_prev_origin = [10.0, 20.0, 30.0];
        m.view_entity_angles = [0.0, 90.0, 0.0];
        m.view_prev_entity_angles = [0.0, 90.0, 0.0];
        m.view_angles = [0.0; 3]; // the camera looks down +x
        m.prev_view_angles = [0.0; 3];
        m.view_effects = EF_MUZZLEFLASH;
        let mut d = lit_playback(vec![m]);
        d.demo.viewentity = 5;
        draw(&mut d);
        let lights: Vec<_> = d.view.dlights.iter().map(|&(slot, l)| (slot, l.key())).collect();
        assert_eq!(lights, vec![(0, 3), (1, 5), (2, 7)], "entity order: 3, the view entity 5, 7");
        let flash = d.view.dlights[1].1;
        assert!((flash.origin[0] - 10.0).abs() < 1e-3 && (flash.origin[1] - 38.0).abs() < 1e-3, "{:?}", flash.origin);
        assert!((flash.origin[2] - 46.0).abs() < 1e-3);
        assert_eq!(flash.minlight, 32.0);
    }

    /// No effects bit, no light; a static is never relinked, so its recorded
    /// effects light nothing; a flash the message does not carry (the view
    /// entity not updated) lights nothing.
    #[test]
    fn only_relinked_entities_with_effects_make_lights() {
        let mut m =
            message(0, vec![lit(4, [0.0; 3], [0.0; 3], 0), lit(-1, [0.0; 3], [0.0; 3], EF_BRIGHTLIGHT | EF_DIMLIGHT)]);
        m.view_effects = 0;
        let mut d = lit_playback(vec![m]);
        draw(&mut d);
        assert!(d.view.dlights.is_empty(), "{:?}", d.view.dlights);
    }

    /// A flash is made again in every frame drawn from the message that
    /// carries it, and lives 0.1 s past the last: the entity's next message,
    /// without the bit, ends the making and the tail is the light's own.
    #[test]
    fn a_flash_is_made_every_frame_its_message_holds_it_and_outlives_it_by_a_tenth() {
        let flash = lit(5, [0.0; 3], [0.0; 3], EF_MUZZLEFLASH);
        let quiet = lit(5, [0.0; 3], [0.0; 3], 0);
        let mut d = lit_playback(vec![message(0, vec![flash]), message(1, vec![quiet]), message(2, vec![quiet])]);
        let (mut made_until, mut lit_frames) = (0.0f32, Vec::new());
        for _ in 0..30 {
            if d.idx + 1 >= d.demo.frames.len() {
                break; // the loop wrap: not under test
            }
            draw(&mut d);
            let drawn = !d.view.dlights.is_empty();
            if d.idx == 0 {
                assert!(drawn, "made again each frame message 0 holds it (t = {})", d.view.time);
                made_until = d.view.time;
            }
            lit_frames.push((d.view.time, drawn));
        }
        // The first frame of message 1: still lit by what the frame before made.
        assert!(lit_frames.iter().any(|&(t, lit)| lit && t > made_until), "the 0.1 s tail: {lit_frames:?}");
        for &(t, lit) in &lit_frames {
            assert_eq!(
                lit,
                t <= made_until + 0.1,
                "lit exactly while die >= cl.time, at {t} (made until {made_until})"
            );
        }
    }

    /// A rocket (a model flagged `EF_ROCKET`) lights its way: radius 200, 0.01 s,
    /// each frame, at the origin it is relinked at, keyed by its entity number.
    #[test]
    fn a_rocket_in_flight_carries_a_light() {
        let mut d = lit_playback(vec![message(0, vec![lit(9, [30.0, 40.0, 50.0], [0.0; 3], 0)])]);
        let mut rocket = poseless_model();
        rocket.header.flags = 1; // EF_ROCKET
        d.models = vec![None, Some(rocket)];
        draw(&mut d);
        let now = d.time;
        let lights: Vec<_> = d.view.dlights.iter().map(|&(slot, l)| (slot, l)).collect();
        assert_eq!(lights.len(), 1, "{lights:?}");
        let (slot, l) = lights[0];
        assert_eq!(slot, 0);
        assert_eq!((l.key(), l.origin, l.radius, l.minlight, l.decay), (9, [30.0, 40.0, 50.0], 200.0, 0.0, 0.0));
        assert_eq!(l.die, (now + 0.01) as f32, "die = (float)(cl.time + 0.01)");
        // A model with a grenade's flag (EF_GRENADE) leaves a trail but no light.
        let mut d = lit_playback(vec![message(0, vec![lit(9, [30.0, 40.0, 50.0], [0.0; 3], 0)])]);
        let mut grenade = poseless_model();
        grenade.header.flags = 2;
        d.models = vec![None, Some(grenade)];
        draw(&mut d);
        assert!(d.view.dlights.is_empty());
    }

    /// An explosion's light (`TE_EXPLOSION`, `TE_EXPLOSION2`) is made when the
    /// message is read, at the clock it is read at: drawn at its full 350 that
    /// frame, 300 a second less after each (`CL_DecayLights` runs after the
    /// draw), gone 0.5 s on; the tarbaby's blob has none.
    #[test]
    fn an_explosion_lights_up_and_fades_over_half_a_second() {
        use crate::server::{TempEntityEvent, te_consts::*};
        let boom = |te_type| TempEntityEvent {
            te_type,
            pos: [5.0, 6.0, 7.0],
            end: [5.0, 6.0, 7.0],
            entity: 0,
            color_start: 0,
            color_length: 0,
        };
        for (te, lit) in [(TE_EXPLOSION, true), (TE_EXPLOSION2, true), (TE_TAREXPLOSION, false)] {
            let mut messages: Vec<_> = (0..8).map(|n| message(n, Vec::new())).collect();
            messages[1].temp_entities.push(boom(te));
            let mut d = lit_playback(messages);
            let mut frames = Vec::new(); // (clock, radius) of each frame that drew the light
            while d.idx + 1 < d.demo.frames.len() {
                draw(&mut d);
                if let Some(&(_, l)) = d.view.dlights.first() {
                    frames.push((d.view.time, l.radius, l.die, (d.time - d.oldtime) as f32));
                }
            }
            assert_eq!(!frames.is_empty(), lit, "type {te}");
            if !lit {
                continue;
            }
            let (born, radius, die, _) = frames[0];
            assert_eq!(radius, 350.0, "the full radius on the frame it was made in");
            assert!((die - (born + 0.5)).abs() < 1e-6, "die = cl.time + 0.5");
            // Each later frame is drawn 300 * the step of the frame before smaller.
            for pair in frames.windows(2) {
                let (before, after) = (pair[0], pair[1]);
                assert!(after.1 < before.1, "shrinking: {frames:?}");
                assert!((before.1 - after.1 - 300.0 * before.3).abs() < 0.01, "decay*dt: {before:?} -> {after:?}");
            }
            let last = frames.last().unwrap().0;
            assert!(last <= die + 1e-6 && last > die - 0.05, "drawn until it dies: last {last}, die {die}");
        }
    }

    /// What the light does to the picture: the frame that reads a recorded
    /// explosion is lit by it (it is not without the message), and the
    /// lit frame is the same pixels on one render thread and on four — the
    /// bakes that rebuild the surfaces it touches are independent of how many
    /// threads share them.
    #[test]
    fn a_recorded_explosion_changes_the_frame_the_same_on_any_thread_count() {
        use crate::server::{TempEntityEvent, te_consts::TE_EXPLOSION};
        let first_lit_frame = |boom: bool, threads: usize| {
            let msgs = (0..3)
                .map(|n| {
                    let mut m = message(n, Vec::new());
                    m.viewheight = 22.0;
                    m.lightstyles = std::rc::Rc::new(vec!["m".into()]);
                    if boom && n == 1 {
                        m.temp_entities.push(TempEntityEvent {
                            te_type: TE_EXPLOSION,
                            pos: [48.0, 0.0, 0.0],
                            end: [48.0, 0.0, 0.0],
                            entity: 0,
                            color_start: 0,
                            color_length: 0,
                        });
                    }
                    m
                })
                .collect();
            let mut d =
                playback_in(render::fixtures::lightmapped_demo_room(120, 0), render::fixtures::ramp_palette(), msgs);
            d.viewsize = 120.0;
            d.renderer.set_threads(threads);
            // The frame that reads message 1 (the same frame with and without its explosion).
            loop {
                let image = demo_frame(&mut d, 1.0 / 72.0, false, &VID).image;
                if d.idx == 1 {
                    return (image.pixels.clone(), d.view.dlights.len());
                }
            }
        };
        let (without, lights) = first_lit_frame(false, 1);
        assert_eq!(lights, 0);
        let (one, lights) = first_lit_frame(true, 1);
        assert_eq!(lights, 1);
        assert_ne!(one, without, "the explosion's light is in the frame");
        let (four, _) = first_lit_frame(true, 4);
        assert_eq!(one, four, "the same pixels on four threads");
    }

    /// The recorded explosion lights timedemo's frames too (each message is
    /// its frame's), and the playback starting over clears the pool, as
    /// `CL_ClearState` does.
    #[test]
    fn timedemo_and_the_loop_wrap_light_and_clear_the_same_pool() {
        use crate::server::{TempEntityEvent, te_consts::*};
        let mut m = message(1, Vec::new());
        m.temp_entities.push(TempEntityEvent {
            te_type: TE_EXPLOSION,
            pos: [0.0; 3],
            end: [0.0; 3],
            entity: 0,
            color_start: 0,
            color_length: 0,
        });
        let mut d = lit_playback(vec![message(0, Vec::new()), m, message(2, Vec::new())]);
        let mut radii = Vec::new();
        while let Some(f) = timedemo_frame(&mut d, 1.0 / 72.0, false, &VID) {
            radii.push(d.view.dlights.first().map(|&(_, l)| l.radius));
            render::recycle_image(f.image);
        }
        // The first frame reads through message 1 and draws its explosion at
        // 350; message 2 is 0.1 s on: 0.1 * 300 less.
        assert_eq!(radii.len(), 2, "{radii:?}");
        assert_eq!(radii[0], Some(350.0));
        assert!((radii[1].unwrap() - 320.0).abs() < 0.01, "{radii:?}");
        restart_playback(&mut d);
        assert_eq!(d.dlights.active_count(1.0), 0);
        assert!(d.dlights.pushed(0.0).next().is_none());
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
            forcetrack: -1,
            cdtrack: None,
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
        let vid = Vid {
            width: 64,
            height: 40,
            display_aspect: 4.0 / 3.0,
            persp_span: render::PerspSpan::Spans16,
            video: render::VideoCvars::CLASSIC,
            mip: render::MipCvars::DEFAULT,
        };
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

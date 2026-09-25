//! Save/load — `Host_Savegame_f` / `Host_Loadgame_f` (host_cmd.c) over the
//! page's localStorage: the console halves with the C's guards and messages,
//! the rebuild of a [`Walk`] from a `.sav` text, and the scratch-buffer
//! exports the page moves the text through (plus the menu slot comments).

use std::cell::RefCell;

use quake_rs::bsp::Bsp;
use quake_rs::progs::Progs;
use quake_rs::render::Menu;
use quake_rs::server::Server;

use crate::snd_dma::{bump_sound_generation, queue_static_sounds};
use crate::{assemble_walk, clamp_pitch, ensure_app, pak, Walk};

// ---------------------------------------------------------------------------
// Savegame persistence bridge (page-owned localStorage)
//
// The C's Host_Savegame_f/Host_Loadgame_f read and write .sav FILES; in the
// browser the page owns persistence (localStorage), so the engine speaks text
// through the same shared-scratch-buffer style the sound path uses:
//
//  * `save <name>` (console) runs the C's guards, builds the .sav text via
//    Server::write_savegame, and queues (filename, text); the page polls
//    `poll_save()` each frame, reads the name/text out of linear memory, and
//    persists them under a per-name localStorage key. A storage failure
//    reports back through `save_store_failed()` (the async stand-in for the
//    C's synchronous "ERROR: couldn't open.").
//  * `load <name>` (console) prints the C's "Loading game from ..." line and
//    queues a request; the page polls `poll_load_request()`, fetches the
//    stored text, writes it into the wasm scratch via `sav_alloc()` +
//    linear-memory copy, then calls `load_game()`. `load_failed()` reports
//    the C's "ERROR: couldn't open." when the key does not exist OR when
//    `sav_alloc` rejects an oversized value (NULL: the page must not copy).
//  * `extract_save_comment()` parses a stored .sav from the same scratch and
//    returns its comment (underscores back to spaces, M_ScanSaves) for the
//    Load/Save menu slot listings.
// ---------------------------------------------------------------------------

thread_local! {
    /// Completed saves awaiting page pickup: `(filename, .sav text)` pairs.
    static SAVE_QUEUE: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
    /// The save `poll_save()` popped, pinned for the ptr/len exports.
    static SAVE_CUR: RefCell<(String, String)> =
        const { RefCell::new((String::new(), String::new())) };
    /// A pending load: the filename the page should fetch from localStorage.
    static LOAD_REQUEST: RefCell<Option<String>> = const { RefCell::new(None) };
    /// The request `poll_load_request()` popped, pinned for the ptr export.
    static LOAD_REQ_CUR: RefCell<String> = const { RefCell::new(String::new()) };
    /// Page->wasm scratch: stored .sav text handed back for `load_game()` /
    /// `extract_save_comment()` (the inbound twin of the sound scratch).
    static SAV_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    /// The comment `extract_save_comment()` produced, pinned for its ptr export.
    static SAVE_COMMENT: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Pop the next completed save into the pinned slot and return its TEXT byte
/// length (0 = queue empty). The page then reads `save_name_*` + `save_text_ptr`.
#[no_mangle]
pub extern "C" fn poll_save() -> i32 {
    SAVE_QUEUE.with(|q| {
        let Some(item) = q.borrow_mut().pop() else {
            return 0;
        };
        let len = item.1.len() as i32;
        SAVE_CUR.with(|c| *c.borrow_mut() = item);
        len
    })
}

/// Byte length of the popped save's filename.
#[no_mangle]
pub extern "C" fn save_name_len() -> i32 {
    SAVE_CUR.with(|c| c.borrow().0.len() as i32)
}

/// Pointer to the popped save's filename bytes.
#[no_mangle]
pub extern "C" fn save_name_ptr() -> *const u8 {
    SAVE_CUR.with(|c| c.borrow().0.as_ptr())
}

/// Pointer to the popped save's .sav text bytes (length = `poll_save()`'s return).
#[no_mangle]
pub extern "C" fn save_text_ptr() -> *const u8 {
    SAVE_CUR.with(|c| c.borrow().1.as_ptr())
}

/// The page failed to persist the popped save (localStorage threw — quota or
/// privacy mode). The C fails synchronously with "ERROR: couldn't open."
/// before writing; persistence here is asynchronous, so the error arrives a
/// frame after the optimistic "done." (documented deviation).
#[no_mangle]
pub extern "C" fn save_store_failed() {
    ensure_app(|a| {
        a.console
            .println("ERROR: couldn't store savegame (localStorage full?)");
    });
}

/// Pop a pending load request and return the filename's byte length (0 = none).
#[no_mangle]
pub extern "C" fn poll_load_request() -> i32 {
    LOAD_REQUEST.with(|r| {
        let Some(name) = r.borrow_mut().take() else {
            return 0;
        };
        let len = name.len() as i32;
        LOAD_REQ_CUR.with(|c| *c.borrow_mut() = name);
        len
    })
}

/// Pointer to the popped load request's filename bytes.
#[no_mangle]
pub extern "C" fn load_request_ptr() -> *const u8 {
    LOAD_REQ_CUR.with(|c| c.borrow().as_ptr())
}

/// The page found no stored save under the requested name: the C's fopen
/// failure path, `Con_Printf("ERROR: couldn't open.\n")`.
#[no_mangle]
pub extern "C" fn load_failed() {
    ensure_app(|a| a.console.println("ERROR: couldn't open."));
}

/// Hard cap on the inbound .sav scratch (a real save is ~100-400 KB; 8 MB is
/// far past any legitimate file) so a hostile length can't balloon memory.
const SAV_BUF_MAX: i32 = 8 * 1024 * 1024;

/// Resize the inbound .sav scratch to `len` bytes and return its pointer; the
/// page copies the stored text in, then calls `load_game()` /
/// `extract_save_comment()`. An out-of-range `len` (negative, or past the
/// 8 MB cap) FAILS CLOSED: the scratch is emptied and NULL comes back, and
/// the page must honour the rejection (skip the copy, report `load_failed`).
/// Returning any real pointer for a length we did not allocate would invite
/// the caller to write `len` bytes through it — the exact wild write into
/// linear memory the cap exists to prevent.
#[no_mangle]
pub extern "C" fn sav_alloc(len: i32) -> *mut u8 {
    SAV_BUF.with(|b| {
        let mut b = b.borrow_mut();
        b.clear();
        if !(0..=SAV_BUF_MAX).contains(&len) {
            return std::ptr::null_mut();
        }
        b.resize(len as usize, 0);
        b.as_mut_ptr()
    })
}

/// Load the game whose .sav text the page placed in the scratch buffer
/// (`Host_Loadgame_f`'s post-fopen half). On success the new walk replaces
/// the current mode (1); on any parse/load failure the RUNNING GAME IS LEFT
/// INTACT and the error prints to the console (0) — the C `Sys_Error`ed on a
/// malformed save; we degrade (documented deviation).
#[no_mangle]
pub extern "C" fn load_game() -> i32 {
    let bytes = SAV_BUF.with(|b| std::mem::take(&mut *b.borrow_mut()));
    let text = String::from_utf8_lossy(&bytes).into_owned();
    match build_walk_savegame(&text) {
        Ok(nw) => {
            ensure_app(|a| {
                a.walk = Some(nw);
                a.mode = 0;
                // The loaded game starts playing: close the console + menu
                // (the same post-swap treatment as the console `map` command).
                a.console.open = false;
                a.menu = Menu::new();
                a.menu.sync_resolution(a.render_w as i32, a.render_h as i32);
            });
            1
        }
        Err(msg) => {
            ensure_app(|a| a.console.println(msg));
            0
        }
    }
}

/// Parse the .sav text in the scratch buffer and pin its comment (underscores
/// converted back to spaces, like the C menu's `M_ScanSaves`) for the slot
/// listings; returns the comment's byte length, or 0 for an unparseable text.
#[no_mangle]
pub extern "C" fn extract_save_comment() -> i32 {
    let bytes = SAV_BUF.with(|b| std::mem::take(&mut *b.borrow_mut()));
    let text = String::from_utf8_lossy(&bytes);
    let Ok(sg) = quake_rs::save::parse_savegame(&text) else {
        return 0;
    };
    let comment = quake_rs::save::comment_for_display(&sg.comment);
    let len = comment.len() as i32;
    SAVE_COMMENT.with(|c| *c.borrow_mut() = comment);
    len
}

/// Pointer to the comment bytes `extract_save_comment()` produced.
#[no_mangle]
pub extern "C" fn save_comment_ptr() -> *const u8 {
    SAVE_COMMENT.with(|c| c.borrow().as_ptr())
}

/// MERGE SEAM (Load/Save menu <- localStorage): assign menu slot `slot`'s
/// comment from the savegame text the page just placed in the scratch via
/// [`sav_alloc`] (one stored `.sav` per call). An empty/absent/unparseable
/// buffer marks the slot unused (`"--- UNUSED SLOT ---"` in M_Load_Draw).
/// The page refreshes all 12 slots at boot and after every persisted save,
/// so the menu's listings always mirror what localStorage actually holds.
#[no_mangle]
pub extern "C" fn menu_set_save_comment(slot: i32) {
    let Ok(slot) = usize::try_from(slot) else { return };
    let bytes = SAV_BUF.with(|b| std::mem::take(&mut *b.borrow_mut()));
    let comment = if bytes.is_empty() {
        String::new()
    } else {
        let text = String::from_utf8_lossy(&bytes);
        match quake_rs::save::parse_savegame(&text) {
            Ok(sg) => quake_rs::save::comment_for_display(&sg.comment),
            Err(_) => String::new(),
        }
    };
    ensure_app(|a| a.menu.set_save_comment(slot, comment.clone()));
}

/// `COM_DefaultExtension` (common.c): append `ext` unless the last path
/// component already carries a `.` extension.
fn default_extension(path: &str, ext: &str) -> String {
    let last = path.rsplit('/').next().unwrap_or(path);
    if last.contains('.') {
        path.to_string()
    } else {
        format!("{path}{ext}")
    }
}

/// `Host_Savegame_f` (host_cmd.c), console half: run the C's guard sequence
/// (exact messages, same order — minus `cmd_source`/multiplayer, which don't
/// exist in this single-player shell), then queue the .sav text for the page.
/// `name` is `argv[1]` (`None` reproduces the C's `Cmd_Argc() != 2` usage
/// message at its position in the sequence). Also the host-side entry the
/// Save menu's `MenuAction::SaveSlot(i)` will call with `"s<i>"`.
pub(crate) fn do_save_command(name: Option<&str>) {
    ensure_app(|a| {
        // if (!sv.active) — no live single-player world (demo/attract mode).
        let playing = a.mode == 0 && a.walk.is_some();
        if !playing {
            a.console.println("Not playing a local game.");
            return;
        }
        if a.walk.as_ref().is_some_and(|w| w.intermission != 0) {
            a.console.println("Can't save in intermission.");
            return;
        }
        // (svs.maxclients != 1 — "Can't save multiplayer games." — is
        // unreachable here: this shell is single-client by construction.)
        let Some(name) = name.filter(|s| !s.is_empty()) else {
            a.console.println("save <savename> : save a game");
            return;
        };
        if name.contains("..") {
            a.console.println("Relative pathnames are not allowed.");
            return;
        }
        let Some(w) = a.walk.as_ref() else { return };
        if w.server.player_health() <= 0.0 {
            a.console.println("Can't savegame with a dead player");
            return;
        }
        let fname = default_extension(name, ".sav");
        a.console.println(format!("Saving game to {fname}..."));
        let text = w.server.write_savegame();
        SAVE_QUEUE.with(|q| q.borrow_mut().push((fname, text)));
        // The C prints "done." after its synchronous fwrite; the page's
        // localStorage write happens next frame and reports a failure via
        // save_store_failed() (documented deviation).
        a.console.println("done.");
    });
}

/// `Host_Loadgame_f` (host_cmd.c), console half: print the C's status line and
/// queue the request; the page fetches the stored text and calls back into
/// `load_game()` (or `load_failed()`). Also the host-side entry the Load
/// menu's `MenuAction::LoadSlot(i)` will call with `"s<i>"`.
pub(crate) fn do_load_command(name: Option<&str>) {
    let Some(name) = name.filter(|s| !s.is_empty()) else {
        ensure_app(|a| a.console.println("load <savename> : load a game"));
        return;
    };
    // (cls.demonum = -1 — "stop demo loop in case this fails" — has no
    // equivalent: the attract demo keeps idling until the swap commits.)
    let fname = default_extension(name, ".sav");
    ensure_app(|a| a.console.println(format!("Loading game from {fname}...")));
    LOAD_REQUEST.with(|r| *r.borrow_mut() = Some(fname));
}

/// `Host_Loadgame_f`'s post-fopen half: parse the header, spawn the named map,
/// and rebuild a [`Walk`] around [`Server::load_savegame`]'s reconstructed
/// world. Errors return the console message to print (the C's where it has
/// one); the caller leaves the current game untouched on `Err`.
fn build_walk_savegame(text: &str) -> Result<Walk, String> {
    use quake_rs::save::{parse_savegame, SAVEGAME_VERSION};

    let sg = parse_savegame(text).map_err(|e| e.to_string())?;
    if sg.version != SAVEGAME_VERSION {
        // Con_Printf ("Savegame is version %i, not %i\n", ...)
        return Err(format!(
            "Savegame is version {}, not {}",
            sg.version, SAVEGAME_VERSION
        ));
    }
    let couldnt = || "Couldn't load map".to_string(); // SV_SpawnServer failure
    let pak = pak().ok_or_else(couldnt)?;
    let read = |n: &str| pak.read_file(n).ok().flatten();
    let map = format!("maps/{}.bsp", sg.map_name);
    let map_bytes = read(&map).ok_or_else(couldnt)?;
    let sim_bsp = Bsp::parse(&map_bytes).map_err(|_| couldnt())?;
    let render_bsp = Bsp::parse(&map_bytes).map_err(|_| couldnt())?;
    let progs_bytes = read("progs.dat").ok_or_else(couldnt)?;
    let progs = Progs::parse(&progs_bytes).map_err(|e| e.to_string())?;

    // The engine-side load: header -> SV_SpawnServer (map spawn functions DO
    // run, rebuilding precaches; see save.rs) -> lightstyles -> globals ->
    // edicts -> sv.time/spawn_parms. No entrance script, no signon settle.
    let mut server =
        Server::load_savegame(sim_bsp, progs, Some(pak.clone()), text).map_err(|e| e.to_string())?;
    let player = server.player_edict();

    // The save's spawn parms are the level-ENTRY parms (svs.clients->
    // spawn_parms): a respawn on the loaded level restores the state the
    // player entered it with, exactly like an uninterrupted session.
    let entry_parms = sg.spawn_parms;

    // View angles from the loaded player's v_angle. DEVIATION: the C's
    // Host_Spawn_f sends an svc_setangle built from ent->v.angles (the model
    // angles, whose pitch is the C's -v_angle/3 quirk, roll forced 0 — "never
    // send a roll angle, because savegames can catch the server expecting the
    // client to correct it"); restoring v_angle directly gives back the exact
    // view the player saved with, roll-free here too (this shell has no
    // persistent roll state).
    let v_angle = server.vm.ent_get_vector(player, "v_angle");
    let yaw = v_angle[1];
    let pitch = clamp_pitch(v_angle[0]);

    // Capture this load's placed ambient loops (registered while the map's
    // spawn functions re-ran inside load_savegame) BEFORE the server moves
    // into the Walk; committed to the page only after assembly succeeds.
    let statics = server.drain_static_sounds();

    let mut w = assemble_walk(
        pak,
        map,
        server,
        player,
        entry_parms,
        render_bsp,
        yaw,
        pitch,
    )
    .ok_or_else(couldnt)?;

    // Committed: tear down the previous level/mode's looping audio, start this
    // level's, and drop one-shot events the load's spawn + settle ticks queued
    // (same treatment as every other walk-building path).
    bump_sound_generation();
    queue_static_sounds(&w.pak, &statics);
    let _ = w.server.drain_sounds();
    let _ = w.server.drain_particles();
    let _ = w.server.drain_temp_entities();
    let _ = w.server.drain_messages();
    let _ = w.server.drain_svc_events();
    Ok(w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::*;
    use crate::{
        boot, boot_attract, console_toggle, set_attack, set_move, set_resolution, step, APP,
    };

    // ------------------------------------------------------------ save/load

    /// The whole console scrollback as one string (oldest line first).
    fn console_text() -> String {
        APP.with(|c| {
            c.borrow()
                .as_ref()
                .map(|a| a.console.lines().collect::<Vec<_>>().join("\n"))
                .unwrap_or_default()
        })
    }

    /// A deterministic world-state digest: the player's view/inventory fields,
    /// the QC counters, and — for EVERY edict slot — classname, origin,
    /// velocity, solidity and the think schedule (function NAME + nextthink).
    /// Floats are formatted at the save format's own `%.6f` precision, so an
    /// exact text round-trip digests equal while any real divergence
    /// (a door mid-move, a monster's next think) shows up.
    fn world_digest() -> String {
        use std::fmt::Write as _;
        APP.with(|c| {
            let b = c.borrow();
            let w = b.as_ref().expect("app").walk.as_ref().expect("walk");
            let vm = &w.server.vm;
            let f6 = |v: f32| format!("{v:.6}");
            let v6 = |v: [f32; 3]| format!("{:.6} {:.6} {:.6}", v[0], v[1], v[2]);
            let mut d = String::new();
            let p = w.player;
            let _ = writeln!(
                d,
                "player org={} vel={} ang={} vang={}",
                v6(vm.ent_get_vector(p, "origin")),
                v6(vm.ent_get_vector(p, "velocity")),
                v6(vm.ent_get_vector(p, "angles")),
                v6(vm.ent_get_vector(p, "v_angle")),
            );
            for name in [
                "health", "armorvalue", "items", "weapon", "currentammo",
                "ammo_shells", "ammo_nails", "ammo_rockets", "ammo_cells",
                "deadflag", "weaponframe", "frags",
            ] {
                let _ = writeln!(d, "p.{name}={}", f6(vm.ent_get_float(p, name)));
            }
            let _ = writeln!(
                d,
                "time={} skill={} serverflags={} killed={} secrets={}",
                f6(w.server.time()),
                w.server.skill(),
                f6(vm.gget_float("serverflags")),
                f6(vm.gget_float("killed_monsters")),
                f6(vm.gget_float("found_secrets")),
            );
            for e in 0..vm.num_edicts() {
                let ent = e as i32;
                if vm.is_free_edict(ent) {
                    let _ = writeln!(d, "{e}: free");
                    continue;
                }
                let think = vm.ent_get_int(ent, "think");
                let think_name = vm
                    .progs
                    .functions
                    .get(think as usize)
                    .map(|f| vm.progs.string(f.s_name).to_string())
                    .unwrap_or_default();
                let _ = writeln!(
                    d,
                    "{e}: {} org={} vel={} solid={} nextthink={} think={} frame={} health={}",
                    vm.ent_string_ref(ent, "classname"),
                    v6(vm.ent_get_vector(ent, "origin")),
                    v6(vm.ent_get_vector(ent, "velocity")),
                    f6(vm.ent_get_float(ent, "solid")),
                    f6(vm.ent_get_float(ent, "nextthink")),
                    think_name,
                    f6(vm.ent_get_float(ent, "frame")),
                    f6(vm.ent_get_float(ent, "health")),
                );
            }
            d
        })
    }

    /// `Host_Savegame_f`/`Host_Loadgame_f` console guards, with the C's exact
    /// messages: demo mode refuses ("Not playing a local game."), intermission
    /// refuses, bad argc prints usage, ".." is rejected, a dead player refuses,
    /// and a missing stored slot reports the C's fopen failure.
    #[test]
    fn save_console_guards_match_host_savegame_f() {
        // Attract mode (demo playing) = !sv.active for this shell.
        assert_eq!(boot_attract(), 1);
        let in_demo = APP.with(|c| c.borrow().as_ref().unwrap().mode == 1);
        assert!(in_demo, "boot_attract plays the demo");
        console_toggle();
        run_console_line("save nope");
        assert!(
            console_text().contains("Not playing a local game."),
            "demo mode refuses: {}",
            console_text()
        );

        // A live walk now (console stays open across boot()).
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);

        run_console_line("save");
        assert!(console_text().contains("save <savename> : save a game"));
        run_console_line("save ../evil");
        assert!(console_text().contains("Relative pathnames are not allowed."));

        walk_mut(|w| w.intermission = 1);
        run_console_line("save x");
        assert!(console_text().contains("Can't save in intermission."));
        walk_mut(|w| w.intermission = 0);

        let p = walk_mut(|w| w.player);
        walk_mut(|w| w.server.vm.ent_set_float(p, "health", 0.0));
        run_console_line("save x");
        assert!(console_text().contains("Can't savegame with a dead player"));
        walk_mut(|w| w.server.vm.ent_set_float(p, "health", 100.0));

        run_console_line("load");
        assert!(console_text().contains("load <savename> : load a game"));

        // load of a slot the page can't find -> the page calls load_failed().
        run_console_line("load missing_slot");
        assert!(console_text().contains("Loading game from missing_slot.sav..."));
        assert!(poll_load_request() > 0, "the request reaches the page");
        let name = LOAD_REQ_CUR.with(|c| c.borrow().clone());
        assert_eq!(name, "missing_slot.sav");
        load_failed();
        assert!(console_text().contains("ERROR: couldn't open."));

        // And a healthy save passes the guards and queues for the page.
        run_console_line("save ok_slot");
        let text = console_text();
        assert!(text.contains("Saving game to ok_slot.sav..."), "{text}");
        assert!(text.contains("done."), "{text}");
        assert!(poll_save() > 0, "the .sav text is queued for the page");
        let (fname, sav) = SAVE_CUR.with(|c| c.borrow().clone());
        assert_eq!(fname, "ok_slot.sav");
        assert!(sav.starts_with("5\n"), "SAVEGAME_VERSION header");
    }

    /// End-to-end round-trip on the real embedded e1m1 + progs.dat through the
    /// exact browser path: play (walk toward the first door, self-rocket for
    /// REAL damage), `save` via the console, keep playing (divergence), then
    /// feed the stored text back like the page does and `load` — the world
    /// digest (player + every edict's origin/think schedule + counters) must
    /// equal the saved instant, and the loaded game must keep running.
    #[test]
    fn save_load_round_trips_the_world_digest() {
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);

        // Arm the rocket launcher (setup only — the damage itself travels the
        // real QuakeC chain) and select it through the real impulse path.
        walk_mut(|w| {
            let p = w.player;
            let items = w.server.vm.ent_get_float(p, "items") as i32 | IT_RL;
            w.server.vm.ent_set_float(p, "items", items as f32);
            w.server.vm.ent_set_float(p, "ammo_rockets", 5.0);
            w.next_impulse = 7;
        });
        step(0.05);

        // ~2.5s forward: across e1m1's start walkway toward the first door
        // (its trigger opens it — moving brush state for the digest).
        set_move(1.0, 0.0);
        for _ in 0..50 {
            step(0.05);
        }
        set_move(0.0, 0.0);

        // One self-rocket at the floor: T_RadiusDamage drops real health.
        walk_mut(|w| w.pitch = 80.0);
        set_attack(1);
        step(0.05);
        set_attack(0);
        for _ in 0..20 {
            step(0.05); // the rocket resolves; the world settles
        }
        walk_mut(|w| w.pitch = 0.0);
        step(0.05);
        let health = player_field("health");
        assert!(
            health > 0.0 && health < 100.0,
            "took real (survivable) rocket damage: {health}"
        );

        let digest_saved = world_digest();

        // Save through the REAL console path; grab what the page would store.
        console_toggle();
        run_console_line("save sl_round");
        let len = poll_save();
        assert!(len > 0, "a completed save is queued");
        let (fname, text) = SAVE_CUR.with(|c| c.borrow().clone());
        assert_eq!(fname, "sl_round.sav");
        assert_eq!(len as usize, text.len());
        console_toggle();

        // Keep playing: the world diverges from the saved instant.
        for _ in 0..40 {
            step(0.05);
        }
        assert_ne!(world_digest(), digest_saved, "play diverged after saving");

        // Load: the console requests, the page feeds the text back.
        console_toggle();
        run_console_line("load sl_round");
        assert!(poll_load_request() > 0);
        SAV_BUF.with(|b| *b.borrow_mut() = text.clone().into_bytes());
        assert_eq!(load_game(), 1, "the stored save loads");

        // ROUND-TRIP FIDELITY: the reloaded world equals the saved instant.
        assert_eq!(world_digest(), digest_saved, "load restored the saved world");

        // 100 frames crash-free on the loaded world.
        for _ in 0..100 {
            step(0.05);
        }
        assert!(player_field("health") > 0.0, "the loaded game keeps playing");
    }

    /// The page-reload path: save, tear the whole App down (a fresh browser
    /// session), boot, and load the stored text — the digest still matches.
    #[test]
    fn fresh_boot_then_load_restores_the_saved_digest() {
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);
        set_move(1.0, 0.0);
        for _ in 0..30 {
            step(0.05);
        }
        set_move(0.0, 0.0);
        step(0.05);

        let digest_saved = world_digest();
        console_toggle();
        run_console_line("save sl_reload");
        assert!(poll_save() > 0);
        let (_, text) = SAVE_CUR.with(|c| c.borrow().clone());

        // "Page reload": drop the entire App and boot a fresh session.
        APP.with(|c| *c.borrow_mut() = None);
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);

        SAV_BUF.with(|b| *b.borrow_mut() = text.into_bytes());
        assert_eq!(load_game(), 1, "the save loads in the fresh session");
        assert_eq!(
            world_digest(),
            digest_saved,
            "a fresh boot + load restores the same world"
        );
        for _ in 0..50 {
            step(0.05);
        }
        assert!(player_field("health") > 0.0);
    }

    /// Hostile input: garbage text, a wrong version, and a truncated save all
    /// fail CLEANLY — error on the console, the running game untouched, no
    /// panic. (The C `Sys_Error`s; degrading is the documented deviation.)
    #[test]
    fn hostile_sav_text_fails_cleanly_and_keeps_the_game() {
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);
        for _ in 0..5 {
            step(0.05);
        }
        let digest = world_digest();

        // Total garbage (including non-UTF8 bytes).
        SAV_BUF.with(|b| *b.borrow_mut() = b"complete {{{ garbage \x01\xff".to_vec());
        assert_eq!(load_game(), 0, "garbage is rejected");
        assert_eq!(world_digest(), digest, "the running game is untouched");

        // A real save to corrupt.
        console_toggle();
        run_console_line("save sl_hostile");
        assert!(poll_save() > 0);
        let (_, text) = SAVE_CUR.with(|c| c.borrow().clone());
        console_toggle();

        // Wrong version: the C's exact message.
        let mut wrong = text.clone();
        wrong.replace_range(0..1, "9");
        SAV_BUF.with(|b| *b.borrow_mut() = wrong.into_bytes());
        assert_eq!(load_game(), 0);
        assert!(
            console_text().contains("Savegame is version 9, not 5"),
            "{}",
            console_text()
        );
        assert_eq!(world_digest(), digest);

        // Truncated mid-block (cut inside the last "classname" key).
        let cut = text.rfind("\"classname\"").expect("save has classnames") + 5;
        SAV_BUF.with(|b| *b.borrow_mut() = text.as_bytes()[..cut].to_vec());
        assert_eq!(load_game(), 0, "a truncated save is rejected");
        assert_eq!(world_digest(), digest, "still untouched");

        // A rejected save must not leak its HEADER into the shared per-thread
        // transports the surviving game syncs from each frame: doctor the
        // header to a hostile skill (line 18) and style-0 pattern (line 21),
        // truncate the blocks so the load fails AFTER those were applied, and
        // confirm the running game's skill/lightstyle survive the next frame
        // (review finding: the failed-load restore in load_savegame).
        let style0 = walk_mut(|w| w.server.lightstyle(0).to_string());
        let skill = walk_mut(|w| w.server.skill());
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        lines[18] = "3".into(); // hostile current_skill
        lines[21] = "hostilepattern".into(); // hostile lightstyle 0
        let doctored = lines.join("\n");
        let cut = doctored.rfind("\"classname\"").expect("blocks survive doctoring") + 5;
        SAV_BUF.with(|b| *b.borrow_mut() = doctored.as_bytes()[..cut].to_vec());
        assert_eq!(load_game(), 0, "the doctored save is still rejected");
        assert_eq!(world_digest(), digest, "world (incl. skill) untouched");
        step(0.05); // run_frame re-syncs lightstyles from the shared transport
        assert_eq!(
            walk_mut(|w| w.server.lightstyle(0).to_string()),
            style0,
            "the failed load's lightstyles must not bleed into the survivor"
        );
        assert_eq!(walk_mut(|w| w.server.skill()), skill, "skill restored");

        // And the (never-replaced) game keeps stepping fine.
        for _ in 0..20 {
            step(0.05);
        }
        assert!(player_field("health") > 0.0);
    }

    /// `sav_alloc` fails CLOSED on a hostile length: NULL back (the page then
    /// skips the copy and reports `load_failed`) — never a pointer that
    /// invites a `len`-byte write the engine did not allocate (review
    /// finding: the old clamp-to-empty returned a dangling pointer the page
    /// would copy a >8 MB localStorage value through, smashing linear memory).
    #[test]
    fn sav_alloc_rejects_hostile_lengths_with_null() {
        assert!(sav_alloc(SAV_BUF_MAX + 1).is_null());
        assert!(sav_alloc(i32::MAX).is_null());
        assert!(sav_alloc(-1).is_null());
        assert!(sav_alloc(i32::MIN).is_null());
        // A rejection also empties the scratch, so a page that ignored the
        // NULL and called load_game anyway would parse "" (clean error),
        // never a stale prior text.
        SAV_BUF.with(|b| assert!(b.borrow().is_empty()));
        // In-range lengths (the cap itself included) still allocate.
        assert!(!sav_alloc(16).is_null());
        SAV_BUF.with(|b| assert_eq!(b.borrow().len(), 16));
        assert!(!sav_alloc(SAV_BUF_MAX).is_null());
        SAV_BUF.with(|b| assert_eq!(b.borrow().len(), SAV_BUF_MAX as usize));
    }

    /// The slot-listing primitive for the (sibling-branch) Load/Save menus:
    /// `extract_save_comment` parses a stored .sav from the scratch buffer and
    /// returns the M_ScanSaves-style display comment (underscores -> spaces).
    #[test]
    fn comment_extraction_for_slot_listings() {
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);
        step(0.05);
        console_toggle();
        run_console_line("save sl_comment");
        assert!(poll_save() > 0);
        let (_, text) = SAVE_CUR.with(|c| c.borrow().clone());

        SAV_BUF.with(|b| *b.borrow_mut() = text.into_bytes());
        let len = extract_save_comment();
        assert_eq!(len as usize, 39, "SAVEGAME_COMMENT_LENGTH");
        let comment = SAVE_COMMENT.with(|c| c.borrow().clone());
        // e1m1's worldspawn message is "the Slipgate Complex"; kills at col 22.
        assert!(comment.contains("Slipgate"), "{comment:?}");
        assert!(comment.contains("kills:"), "{comment:?}");
        assert!(!comment.contains('_'), "display form uses spaces: {comment:?}");

        // Garbage in the scratch -> 0, no panic.
        SAV_BUF.with(|b| *b.borrow_mut() = b"not a save".to_vec());
        assert_eq!(extract_save_comment(), 0);
    }
}

//! Save/load — `Host_Savegame_f` / `Host_Loadgame_f` (host_cmd.c) and the
//! Load/Save menus' `M_ScanSaves` (menu.c), over `std::fs` as the C wrote
//! them: the saves are `.sav` text files in the game directory
//! ([`crate::common`]), which the browser keeps in the page's storage. The
//! rebuild of a [`Walk`] from a save is the client's
//! ([`quake_rs::client::host_cmd::build_walk_savegame`]).

use quake_rs::client::host_cmd;
use quake_rs::menu::MAX_SAVEGAMES;

use crate::app::{ensure_app, Walk};
use crate::common::{self, pak};
use crate::snd_dma;

/// Load the game in the `.sav` `text` (`Host_Loadgame_f` after its fopen).
/// On success the new walk replaces the current mode (true); on any
/// parse/load failure the RUNNING GAME IS LEFT INTACT and the error prints to
/// the console (false) — the C `Sys_Error`ed on a malformed save; we degrade
/// (documented deviation).
pub(crate) fn load_game_text(text: &str) -> bool {
    match build_walk_savegame(text) {
        Ok(nw) => {
            ensure_app(|a| {
                a.start_game(nw);
                // The loaded game starts playing: close the console + menu
                // (the same post-swap treatment as the console `map` command).
                // Only the menu's NAVIGATION resets: Host_Loadgame_f never
                // touches a cvar (viewsize, gamma, volume, sensitivity, Always
                // Run, ...) nor keybindings[], and M_Load_Key only sets
                // m_state = m_none. Same reset as New Game; the slot comments
                // and the video mode survive with it. The console goes at
                // once (SCR_BeginLoadingPlaque zeroes scr_con_current).
                a.console.open = false;
                a.console.set_current(0.0);
                a.menu.reset_nav();
                crate::vid::sync_menu_resolution(a);
            });
            true
        }
        Err(msg) => {
            ensure_app(|a| a.console.println(msg));
            false
        }
    }
}

/// The Load/Save listing for one save's text (`M_ScanSaves`: the comment,
/// underscores back to spaces), or `None` for a text that does not parse.
pub(crate) fn save_comment(text: &str) -> Option<String> {
    let sg = quake_rs::save::parse_savegame(text).ok()?;
    Some(quake_rs::save::comment_for_display(&sg.comment))
}

/// `M_ScanSaves` (menu.c): each of the 12 slots' listing from `s<i>.sav` in
/// the game directory — its comment, or an unused slot where the file is
/// missing or does not parse. The C runs it whenever the Load or Save menu
/// opens (`M_Menu_Load_f`, `M_Menu_Save_f`); the program's loop does the
/// same (`sys`), and once at startup.
pub(crate) fn scan_saves() {
    let comments: [String; MAX_SAVEGAMES] = std::array::from_fn(|i| {
        common::read_file(&format!("s{i}.sav"))
            .ok()
            .and_then(|bytes| save_comment(&String::from_utf8_lossy(&bytes)))
            .unwrap_or_default()
    });
    ensure_app(|a| {
        for (slot, comment) in comments.into_iter().enumerate() {
            a.menu.set_save_comment(slot, comment);
        }
    });
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

/// `Host_Savegame_f` (host_cmd.c): the C's guard sequence (exact messages,
/// same order — minus `cmd_source`/multiplayer, which don't exist in this
/// single-player shell), then the .sav text written to `<gamedir>/<name>.sav`.
/// `name` is `argv[1]` (`None` reproduces the C's `Cmd_Argc() != 2` usage
/// message at its position in the sequence). The Save menu's
/// `MenuAction::SaveSlot(i)` calls it with `"s<i>"`.
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
        // fopen failing: "ERROR: couldn't open." (the page's storage keeping
        // the file is asynchronous, and reports its own failure on the
        // console: web/index.html).
        match common::write_file(&fname, text.as_bytes()) {
            Ok(()) => a.console.println("done."),
            Err(_) => a.console.println("ERROR: couldn't open."),
        }
    });
}

/// `Host_Loadgame_f` (host_cmd.c): print the C's status line, read
/// `<gamedir>/<name>.sav` ("ERROR: couldn't open." when it is not there) and
/// load it ([`load_game_text`]). The Load menu's `MenuAction::LoadSlot(i)`
/// calls it with `"s<i>"`.
pub(crate) fn do_load_command(name: Option<&str>) {
    let Some(name) = name.filter(|s| !s.is_empty()) else {
        ensure_app(|a| a.console.println("load <savename> : load a game"));
        return;
    };
    // (cls.demonum = -1 — "stop demo loop in case this fails" — has no
    // equivalent: the attract demo keeps idling until the swap commits.)
    let fname = default_extension(name, ".sav");
    ensure_app(|a| a.console.println(format!("Loading game from {fname}...")));
    match common::read_file(&fname) {
        Ok(bytes) => {
            load_game_text(&String::from_utf8_lossy(&bytes));
        }
        Err(_) => ensure_app(|a| a.console.println("ERROR: couldn't open.")),
    }
}

/// `Host_Loadgame_f`'s post-fopen half on the game's search path
/// ([`quake_rs::client::host_cmd::build_walk_savegame`]), its sound calls
/// carried out. Errors return the console message to print.
fn build_walk_savegame(text: &str) -> Result<Walk, String> {
    let pak = pak().ok_or_else(|| "Couldn't load map".to_string())?;
    let mut sound = Vec::new();
    // See build_walk_map: the live sv_max_edicts cvar, so a save written
    // with the extra on loads back with the same (or the session's current)
    // ceiling.
    let mut max_edicts = quake_rs::vm::MAX_EDICTS;
    crate::app::ensure_app(|a| max_edicts = a.settings.cvars.max_edicts as usize);
    let walk =
        host_cmd::build_walk_savegame(pak.clone(), text, &crate::app::session_rand(), &mut sound, max_edicts);
    snd_dma::play(&pak, sound);
    walk
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{boot, boot_attract, APP};
    use crate::console::console_toggle;
    use crate::host::step;
    use crate::input::{set_attack, set_move};
    use crate::test_util::*;
    use crate::vid::set_resolution;
    use quake_rs::render;

    // ------------------------------------------------------------ save/load

    /// A save file as the game directory holds it.
    fn stored(name: &str) -> String {
        String::from_utf8(common::read_file(name).expect("the save was written")).unwrap()
    }

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
                    .progs()
                    .functions
                    .get(think as usize)
                    .map(|f| vm.progs().string(f.s_name).to_string())
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

        // A live walk now (boot() opens the menu over it, the console up:
        // M_Menu_Main_f's key_dest = key_menu); back to the console.
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);
        console_toggle();

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

        // load of a slot that is not there: fopen fails.
        run_console_line("load missing_slot");
        assert!(console_text().contains("Loading game from missing_slot.sav..."));
        assert!(console_text().contains("ERROR: couldn't open."));

        // And a healthy save passes the guards and writes its file.
        run_console_line("save ok_slot");
        let text = console_text();
        assert!(text.contains("Saving game to ok_slot.sav..."), "{text}");
        assert!(text.contains("done."), "{text}");
        let sav = stored("ok_slot.sav");
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

        // Save through the REAL console path, into the game directory.
        console_toggle();
        run_console_line("save sl_round");
        let text = stored("sl_round.sav");
        console_toggle();

        // Keep playing: the world diverges from the saved instant.
        for _ in 0..40 {
            step(0.05);
        }
        assert_ne!(world_digest(), digest_saved, "play diverged after saving");

        // Load it back through the console.
        console_toggle();
        run_console_line("load sl_round");
        assert!(!console_text().contains("ERROR"), "{}", console_text());

        // ROUND-TRIP FIDELITY: the reloaded world equals the saved instant.
        assert_eq!(world_digest(), digest_saved, "load restored the saved world");
        // cl.time is the loaded sv.time (not a clock restarted at 0).
        let saved_time: f32 = text.lines().nth(20).unwrap().trim().parse().unwrap();
        walk_mut(|w| assert_eq!((w.clock, w.server.time()), (saved_time, saved_time)));

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
        let text = stored("sl_reload.sav");

        // "Page reload": drop the entire App and boot a fresh session.
        APP.with(|c| *c.borrow_mut() = None);
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);

        assert!(load_game_text(&text), "the save loads in the fresh session");
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

    /// `Host_Loadgame_f` never touches a cvar or `keybindings[]` (host_cmd.c;
    /// `M_Load_Key` only sets `m_state = m_none`): the review repro
    /// `viewsize 60`, `save t`, `load t` must keep Screen size 60, and every
    /// other option, rebind and slot listing with it. The load used to rebuild
    /// the Menu wholesale (`Menu::new()`), snapping all of them to defaults.
    #[test]
    fn load_keeps_every_option_and_binding() {
        use crate::menu::{menu_bind_key, menu_down, menu_right, menu_select, menu_up};
        assert_eq!(boot(), 1); // menu open on Main
        crate::test_util::use_slop(); // Always Run on, to toggle off
        set_resolution(320, 200);
        // Options through the real menu exports: Brightness and Always Run.
        menu_down();
        menu_down();
        menu_select(); // Main > Options
        for _ in 0..4 {
            menu_down();
        }
        menu_right(); // Brightness: v_gamma 1.0 -> 0.95
        for _ in 0..4 {
            menu_down();
        }
        menu_right(); // Always Run: on -> off
        for _ in 0..8 {
            menu_up();
        }
        menu_select(); // Customize controls
        menu_down();
        menu_down(); // "jump / swim up"
        menu_select();
        menu_bind_key(i32::from(b'j'));
        APP.with(|c| {
            let mut b = c.borrow_mut();
            let m = &mut b.as_mut().unwrap().menu;
            m.close();
            m.set_save_comment(4, "e1m1 slot four".to_string());
        });
        console_toggle();
        run_console_line("viewsize 60");
        run_console_line("save t");
        run_console_line("load t");
        assert!(!console_text().contains("ERROR"), "{}", console_text());
        APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            let (m, s) = (&a.menu, &a.settings);
            assert!(!m.visible && !a.console.open, "the loaded game plays");
            assert_eq!(s.cvars.viewsize, 60.0, "Screen size survives load");
            assert!((s.cvars.gamma - 0.95).abs() < 1e-6, "Brightness survives load");
            assert!(!s.cvars.always_run(), "Always Run (off) survives load");
            assert_eq!(
                s.binds.command(b'j'),
                Some(render::BIND_JUMP),
                "rebind survives load"
            );
            assert_eq!(
                m.save_comment(4),
                "e1m1 slot four",
                "slot listings survive load"
            );
            assert_eq!(m.resolution(), (320, 200), "the video mode survives load");
        });
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
        let garbage = String::from_utf8_lossy(b"complete {{{ garbage \x01\xff").into_owned();
        assert!(!load_game_text(&garbage), "garbage is rejected");
        assert_eq!(world_digest(), digest, "the running game is untouched");

        // A real save to corrupt.
        console_toggle();
        run_console_line("save sl_hostile");
        let text = stored("sl_hostile.sav");
        console_toggle();

        // Wrong version: the C's exact message.
        let mut wrong = text.clone();
        wrong.replace_range(0..1, "9");
        assert!(!load_game_text(&wrong));
        assert!(
            console_text().contains("Savegame is version 9, not 5"),
            "{}",
            console_text()
        );
        assert_eq!(world_digest(), digest);

        // Truncated mid-block (cut inside the last "classname" key).
        let cut = text.rfind("\"classname\"").expect("save has classnames") + 5;
        assert!(!load_game_text(&text[..cut]), "a truncated save is rejected");
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
        assert!(!load_game_text(&doctored[..cut]), "the doctored save is still rejected");
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

    /// `M_ScanSaves`: each slot's listing is the comment of `s<i>.sav` in
    /// the game directory (the display form: underscores back to spaces), and
    /// a slot whose file is missing or does not parse is unused.
    #[test]
    fn scan_saves_lists_the_slots_from_the_game_directory() {
        assert_eq!(boot(), 1);
        set_resolution(320, 200);
        APP.with(|c| c.borrow_mut().as_mut().unwrap().menu.visible = false);
        step(0.05);
        console_toggle();
        run_console_line("save s2");
        common::write_file("s5.sav", b"not a save").unwrap();
        scan_saves();
        let comment = |i| APP.with(|c| c.borrow().as_ref().unwrap().menu.save_comment(i).to_string());
        let c2 = comment(2);
        assert_eq!(c2.len(), 39, "SAVEGAME_COMMENT_LENGTH");
        // e1m1's worldspawn message is "the Slipgate Complex"; kills at col 22.
        assert!(c2.contains("Slipgate") && c2.contains("kills:"), "{c2:?}");
        assert!(!c2.contains('_'), "display form uses spaces: {c2:?}");
        assert_eq!(comment(5), "", "garbage is an unused slot");
        assert_eq!(comment(0), "", "a missing file is an unused slot");
        assert_eq!(save_comment("not a save"), None);
    }

    /// Second review: saves the port wrote before it named the player
    /// (Host_Spawn_f's `netname = host_client->name`, 2026-09-25) have no
    /// netname in the player's block, and a load keeps the save's fields
    /// (Host_Spawn_f skips the edict setup when `sv.loadgame`), so obituaries
    /// read "  was shot by ..." until the next level. Such a save loads as
    /// "player"; a save that names the player keeps its name.
    #[test]
    fn old_saves_load_with_the_player_named() {
        let w = crate::app::build_walk().expect("e1m1 boots");
        let text = w.server.write_savegame();
        let named = "\"netname\" \"player\"\n";
        assert_eq!(text.matches(named).count(), 1, "one player block names it");
        let netname = |t: &str| {
            let l = build_walk_savegame(t).expect("loads");
            l.server.vm.ent_get_string(l.player, "netname")
        };
        assert_eq!(netname(&text.replace(named, "")), "player", "an old save");
        assert_eq!(netname(&text.replace(named, "\"netname\" \"Ranger\"\n")), "Ranger");
    }

    /// id's saves hold no statics: `PF_makestatic` freed their edicts, so
    /// `ED_Write` writes each as an empty block, and `Host_Loadgame_f`'s
    /// `SV_SpawnServer` re-runs the map's spawn functions, whose makestatic
    /// calls rebuild the signon. The start map's torches and flames come back
    /// from a save the same, its file names none of them, and the load leaves
    /// every slot as the save had it: the old-save migration
    /// (`free_statics_an_old_save_kept`) never fires on a new save.
    #[test]
    fn a_save_holds_no_statics_and_its_load_rebuilds_them() {
        let w = crate::app::build_walk_map("maps/start.bsp").expect("start boots");
        assert!(w.server.statics().len() > 30, "start's torches and flames");
        let text = w.server.write_savegame();
        for class in ["light_torch_small_walltorch", "light_flame_large_yellow"] {
            assert!(!text.contains(class), "no {class} block in the save");
        }
        let loaded = build_walk_savegame(&text).expect("loads");
        assert_eq!(loaded.server.statics(), w.server.statics(), "the map's spawn rebuilt them");
        let (vm, lvm) = (&w.server.vm, &loaded.server.vm);
        assert_eq!(lvm.num_edicts(), vm.num_edicts(), "the save's slots");
        let freed: Vec<i32> = (0..vm.num_edicts() as i32).filter(|&e| lvm.is_free_edict(e) != vm.is_free_edict(e)).collect();
        assert_eq!(freed, [], "every slot free or live as saved: nothing migrated");
    }

    /// A save the port wrote before 2026-10-02 (`fleet/makestatic`) holds
    /// each static as a live edict, which the respawned map's signon
    /// statics would double. Its load frees them: the torches draw once,
    /// and the live edicts are the new save's (id's: the `edicts` check).
    #[test]
    fn an_old_save_s_kept_statics_are_freed_on_load() {
        let mut w = crate::app::build_walk_map("maps/start.bsp").expect("start boots");
        w.server.vm.ent_set_vector(w.player, "v_angle", [0.0, 90.0, 0.0]); // the torches ahead
        let new_text = w.server.write_savegame();
        let live = w.server.live_entities();

        // The old port's save: every static still a live edict, as its
        // makestatic left it (model, modelindex, frame, skin, origin, angles;
        // SOLID_NOT, no think).
        let statics = w.server.statics().to_vec();
        for st in &statics {
            let vm = &mut w.server.vm;
            let e = vm.spawn();
            vm.ent_set_string(e, "classname", "light_torch_small_walltorch");
            vm.ent_set_string(e, "model", &st.model);
            let index = vm.with_host(|_, h| h.find_model(&st.model)).flatten().expect("precached");
            vm.ent_set_float(e, "modelindex", index as f32);
            vm.ent_set_float(e, "frame", f32::from(st.frame));
            vm.ent_set_float(e, "skin", f32::from(st.skin));
            vm.ent_set_vector(e, "origin", st.origin);
            vm.ent_set_vector(e, "angles", st.angles);
        }
        let old_text = w.server.write_savegame();
        assert_eq!(old_text.matches("light_torch_small_walltorch").count(), statics.len(), "the old file's live statics");

        let mut new = build_walk_savegame(&new_text).expect("the new save loads");
        let mut old = build_walk_savegame(&old_text).expect("the old save loads");
        assert_eq!(old.server.statics(), new.server.statics());
        assert_eq!(old.server.live_entities(), live, "the live edicts are the new save's");
        assert_eq!(new.server.live_entities(), live);
        let vm = &old.server.vm;
        assert_eq!(vm.live_edicts().filter(|&e| vm.ent_string_ref(e, "classname") == "light_torch_small_walltorch").count(), 0);

        // Drawn once: the same alias models reach the renderer, the same pixels.
        let draw = |w: &mut Walk| {
            w.renderer.stats_begin();
            let (img, _) = crate::cl_walk::step_walk(w, 0.0, true, &crate::vid::mode_vid(320, 200));
            (img, w.renderer.stats_end().alias_models)
        };
        let ((new_img, new_models), (old_img, old_models)) = (draw(&mut new), draw(&mut old));
        assert!(new_models > 1, "torches in view ({new_models} alias models)");
        assert_eq!(old_models, new_models, "each torch drawn once");
        assert!(new_img.pixels == old_img.pixels, "the same frame");
    }
}

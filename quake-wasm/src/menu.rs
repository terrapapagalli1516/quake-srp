//! Menu glue — what menu.c's `M_Keydown` asks of the host: the actions the
//! engine's `Menu` returns (New Game, Video mode, Save/Load slot, Go to
//! console, leaving the main menu) carried out against the App, and the menu
//! key exports, each one key press through keys.c's `Key_Event`
//! ([`crate::input::key_event`]).

use quake_rs::keys::{
    K_BACKSPACE, K_DOWNARROW, K_ENTER, K_ESCAPE, K_LEFTARROW, K_RIGHTARROW, K_UPARROW,
};
use quake_rs::render::{self, MenuAction};

use crate::app::{build_walk_map, ensure_app, App, APP};
use crate::input::press;
use crate::savegame::{do_load_command, do_save_command};
use crate::vid::clamp_resolution;

/// What a menu action leaves for after the App borrow: the commands that
/// replace the whole walk or reach the page (`map start`, `save sN`, `load
/// sN`) run outside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuDeferred {
    /// Single Player > New Game (`map start`).
    NewGame,
    /// `save sN` from the Save menu.
    Save(usize),
    /// `load sN` from the Load menu.
    Load(usize),
}

/// Carry out what the menu asked for, under the App borrow; what must run
/// outside it comes back ([`run_menu_deferred`]).
pub(crate) fn apply_menu_action(a: &mut App, action: MenuAction) -> Option<MenuDeferred> {
    match action {
        MenuAction::NewGame => return Some(MenuDeferred::NewGame),
        // Save/Load menu slots -> the Host_Savegame_f/Host_Loadgame_f port,
        // via the same console-command path (`save sN`/`load sN`, the C's
        // "s%i.sav" naming): a load swaps the whole Walk.
        MenuAction::SaveSlot(i) => return Some(MenuDeferred::Save(i)),
        MenuAction::LoadSlot(i) => return Some(MenuDeferred::Load(i)),
        MenuAction::ResolutionChanged => {
            // Enter on a Video Options mode line (VID_MenuKey K_ENTER ->
            // VID_SetMode): the framebuffer takes the new (clamped) size; the
            // page notices the new width()/height(), re-fits the canvas and
            // persists it.
            let (rw, rh) = a.menu.resolution();
            let (w, h) = clamp_resolution(rw, rh);
            a.set_render_size(w, h);
        }
        MenuAction::OpenConsole => {
            // Options "Go to console": select() already closed the menu
            // (m_state = m_none); M_Options_Key runs Con_ToggleConsole_f.
            if !a.console.open {
                a.toggle_console();
            }
        }
        MenuAction::ResetDefaults => {
            // Options "Reset to defaults": select() reset the in-menu cvars
            // (viewsize/sensitivity/volume/... are read live each frame); the
            // video mode is not a default.cfg cvar and stays.
            a.menu.sync_resolution(a.render_w as i32, a.render_h as i32);
        }
        MenuAction::Resume => {
            // M_Main_Key K_ESCAPE: the demo loop back (`cls.demonum =
            // m_save_demonum;`) and, with nothing playing, its next demo (`if
            // (cls.demonum != -1 && !cls.demoplayback && cls.state !=
            // ca_connected) CL_NextDemo ();`).
            a.cls.demonum = a.m_save_demonum;
            if a.cls.demonum != -1 && !a.demoplayback() && a.disconnected {
                crate::cl_demo::cl_next_demo(a);
            }
        }
        // Closed/Back/None already applied to the menu state.
        MenuAction::Closed | MenuAction::Back | MenuAction::None => {}
    }
    None
}

/// The part of a menu action that runs outside the App borrow.
pub(crate) fn run_menu_deferred(d: MenuDeferred) {
    match d {
        MenuDeferred::NewGame => new_game(),
        MenuDeferred::Save(i) => do_save_command(Some(&format!("s{i}"))),
        MenuDeferred::Load(i) => do_load_command(Some(&format!("s{i}"))),
    }
}

/// One key press (down + up) for the menu, when it is up: a no-op otherwise,
/// so the page's legacy menu exports never reach the game's bindings.
fn menu_press(key: u8) {
    if APP.with(|c| c.borrow().as_ref().is_some_and(|a| a.menu.visible)) {
        press(key);
    }
}

// --- the menu's keys as exports (automation, tests: the page sends every key
// through `key_event`) -------------------------------------------------------

/// `K_UPARROW` in the menu (`M_*_Key`). No-op when the menu is hidden.
pub(crate) fn menu_up() {
    menu_press(K_UPARROW);
}

/// `K_DOWNARROW` in the menu. No-op when the menu is hidden.
pub(crate) fn menu_down() {
    menu_press(K_DOWNARROW);
}

/// `K_ENTER` in the menu: activate the highlighted item (New Game starts the
/// start hub and closes the menu; Load/Save slots, Video modes, Go to console
/// and the rest as `M_*_Key` does them). No-op when the menu is hidden.
pub(crate) fn menu_select() {
    menu_press(K_ENTER);
}

/// Single Player > New Game (`map start`): a fresh single-player game on the
/// start hub (NEW_GAME_MAP). Rebuild the whole walk — new Server, new
/// connected client — switch to walk mode and leave the menu closed. From the
/// hub the player picks skill + episode (changelevel).
fn new_game() {
    {
        if let Some(nw) = build_walk_map(render::NEW_GAME_MAP) {
            ensure_app(|a| {
                a.start_game(nw);
                // Reset the menu's NAVIGATION and leave it closed. The player's
                // options and key rebinds SURVIVE New Game: WinQuake's
                // M_SinglePlayer "New Game" just runs `map start` — cvars and
                // keybindings persist (the flagship "rebind keys / set Always
                // Run, then New Game" flow must not lose them).
                a.menu.reset_nav();
                // PRESERVE the chosen resolution across New Game (keep the live
                // framebuffer) and point the menu's current video mode at it,
                // instead of snapping back to DEFAULT — starting a game no longer
                // throws away a menu-picked resolution.
                a.menu.sync_resolution(a.render_w as i32, a.render_h as i32);
            });
        }
    }
}

/// Escape (`K_ESCAPE` through `Key_Event`): within the menu, back out a
/// screen (Main closes it and resumes the demo loop); outside it,
/// `M_ToggleMenu_f` — the main menu opens (or the console goes up).
pub(crate) fn menu_cancel() {
    press(K_ESCAPE);
}

/// Answer the Quit prompt "Yes" (the `y` key, `M_Quit_Key`): close the menu.
/// Also answers New Game's "Are you sure?" (SCR_ModalMessage's `y`), which
/// starts the new game. A no-op with the menu hidden.
pub(crate) fn menu_quit_yes() {
    menu_press(b'y');
}

/// Answer the Quit prompt "No" (the `n` key, `M_Quit_Key`): back to the
/// screen it rose from. A no-op with the menu hidden.
pub(crate) fn menu_quit_no() {
    menu_press(b'n');
}

/// `K_LEFTARROW` in the menu (`M_AdjustSliders (-1)` on Options; on
/// Load/Save/Keys/Video it moves the cursor, on Help it pages). No-op when
/// the menu is hidden. The Screen size row is `viewsize`, which the next
/// `step` frames the view with — it never touches the framebuffer size (that
/// is Enter on Video Options).
pub(crate) fn menu_left() {
    menu_press(K_LEFTARROW);
}

/// `K_RIGHTARROW` in the menu. See [`menu_left`].
pub(crate) fn menu_right() {
    menu_press(K_RIGHTARROW);
}

/// Backspace while the menu is up: on the Customize-controls screen this
/// unbinds the highlighted command (`M_Keys_Key` K_BACKSPACE/K_DEL); on every
/// other screen it does nothing.
pub(crate) fn menu_backspace() {
    menu_press(K_BACKSPACE);
}

/// 1 while the Keys screen is waiting for the next key to bind (`bind_grab`,
/// menu.c). The page reads this to route the NEXT raw keypress to
/// [`menu_bind_key`] instead of menu navigation.
pub(crate) fn menu_bind_grabbing() -> i32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| (a.menu.visible && a.menu.bind_grabbing()) as i32)
            .unwrap_or(0)
    })
}

/// A key press while the Keys screen waits for one (`M_Keys_Key`, the
/// `bind_grab` branch): Quake keynum in `0..256`, through `Key_Event` like
/// any key. Escape cancels, backtick is refused, any other key binds to the
/// highlighted command; the grab ends either way. A no-op when nothing is
/// grabbing.
pub(crate) fn menu_bind_key(keynum: i32) {
    if !(0..256).contains(&keynum) || menu_bind_grabbing() == 0 {
        return;
    }
    press(keynum as u8);
}

/// The menu screen currently showing, as a stable id — a read-only
/// verification/debug export (the browser checks the screen transitions:
/// Multiplayer opens, Save gates, Video applies). 0 Main, 1 SinglePlayer,
/// 2 Load, 3 Save, 4 Multiplayer, 5 Options, 6 Keys, 7 Video, 8 Help, 9 Quit,
/// 10 the port's Web extras, 11 Multiplayer > Setup.
pub(crate) fn menu_screen_id() -> i32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| match a.menu.screen() {
                render::MenuScreen::Main => 0,
                render::MenuScreen::SinglePlayer => 1,
                render::MenuScreen::Load => 2,
                render::MenuScreen::Save => 3,
                render::MenuScreen::Multiplayer => 4,
                render::MenuScreen::Options => 5,
                render::MenuScreen::Keys => 6,
                render::MenuScreen::Video => 7,
                render::MenuScreen::Help => 8,
                render::MenuScreen::Quit => 9,
                render::MenuScreen::Extras => 10,
                render::MenuScreen::Setup => 11,
            })
            .unwrap_or(0)
    })
}

// --- Web extras: the port's opt-in departures (Options > Web extras) --------

/// The Web extras as bits — 1 `wasm_uncapped`, 2 `wasm_showfps`,
/// 4 `wasm_exactpersp` ([`render::Extras::bits`]); 0 (all off, id's Quake)
/// by default. The page stores this in localStorage whenever it changes.
pub(crate) fn extras() -> i32 {
    APP.with(|c| c.borrow().as_ref().map(|a| a.menu.extras().bits() as i32).unwrap_or(0))
}

/// Set the Web extras from [`extras`]' bits: the page restoring the saved
/// choice after boot. Unknown bits are ignored.
pub(crate) fn set_extras(bits: i32) {
    ensure_app(|a| a.menu.set_extras(render::Extras::from_bits(bits as u32)));
}

/// 1 when the menu is currently visible (capturing input), else 0. The page
/// reads this to route Arrow/Enter keys to the menu vs. the game.
pub(crate) fn menu_visible() -> i32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| a.menu.visible as i32)
            .unwrap_or(0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{boot, boot_attract};
    use crate::host::step;
    use crate::input::{key_down, key_up};
    use crate::test_util::*;
    use crate::vid::{height, set_resolution, width};

    /// CENSUS L14 through the exports: with the walk running, Single Player >
    /// New Game asks first; 'n' keeps the game, 'y' starts the start hub.
    #[test]
    fn new_game_in_a_running_game_asks_first() {
        assert_eq!(boot(), 1); // e1m1 with the menu open over it
        step(0.05); // the host tells the menu sv.active
        menu_select(); // Main > Single Player
        menu_select(); // New Game -> "Are you sure?"
        let (confirm, map) = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            (a.menu.new_game_confirm(), a.walk.as_ref().unwrap().map_name.clone())
        });
        assert!(confirm, "the modal is up");
        assert_eq!(map, "maps/e1m1.bsp", "no new game yet");
        menu_quit_no();
        assert_eq!(menu_visible(), 1, "'n': the menu stays");
        menu_select();
        menu_quit_yes();
        let (vis, map) = APP.with(|c| {
            let b = c.borrow();
            let a = b.as_ref().unwrap();
            (a.menu.visible, a.walk.as_ref().unwrap().map_name.clone())
        });
        assert!(!vis, "'y' closes the menu");
        assert_eq!(map, render::NEW_GAME_MAP, "and starts the start hub");
    }

    #[test]
    fn video_menu_applies_a_preset_through_the_resolution_plumbing() {
        reset_queue();
        assert_eq!(boot(), 1);
        set_resolution(320, 200); // preset 0
        // boot() opened the menu on Main. Navigate: Options (cursor 2) ->
        // Video Options (row 12) -> down one mode -> Enter applies it.
        menu_down();
        menu_down();
        menu_select(); // -> Options
        for _ in 0..12 {
            menu_down(); // ROW_VIDEO
        }
        menu_select(); // -> Video mode list (cursor on the current preset, 0)
        assert_eq!(menu_screen(), render::MenuScreen::Video);
        menu_down(); // preset 1 = 480x300
        menu_select(); // VID_MenuKey K_ENTER -> VID_SetMode
        assert_eq!((width(), height()), (480, 300), "Enter applied the highlighted mode");
        assert_eq!(menu_screen(), render::MenuScreen::Video, "the list stays up");
        // Esc returns to Options (VID_MenuKey K_ESCAPE -> M_Menu_Options_f).
        menu_cancel();
        assert_eq!(menu_screen(), render::MenuScreen::Options);
    }

    #[test]
    fn load_save_screens_gate_and_emit_actions_via_exports() {
        reset_queue();
        // ATTRACT (demo) mode: no game running -> Save refuses to open.
        assert_eq!(boot_attract(), 1);
        step(0.05); // sync game_active (mode 1 -> false)
        menu_cancel(); // Escape: the menu over the demo
        menu_select(); // Main item 0 -> SinglePlayer
        menu_down();
        menu_down(); // cursor 2 = Save
        menu_select();
        assert_eq!(
            menu_screen(),
            render::MenuScreen::SinglePlayer,
            "Save refuses without a running game (M_Menu_Save_f's sv.active gate)"
        );
        // Load always opens; every slot is unused, so Enter does nothing.
        menu_up(); // cursor 1 = Load
        menu_select();
        assert_eq!(menu_screen(), render::MenuScreen::Load);
        menu_select(); // unused slot: M_Load_Key's !loadable return
        assert_eq!(menu_screen(), render::MenuScreen::Load, "unused slot stays put");
        assert_eq!(menu_visible(), 1);
        // Esc backs out to SinglePlayer.
        menu_cancel();
        assert_eq!(menu_screen(), render::MenuScreen::SinglePlayer);

        // WALK mode: the game runs -> Save opens; Enter emits SaveSlot (a
        // host no-op until the savegame engine lands) and closes the menu.
        assert_eq!(boot(), 1);
        step(0.05); // sync game_active (walk, no intermission -> true)
        menu_select(); // -> SinglePlayer
        menu_down();
        menu_down();
        menu_select(); // -> Save
        assert_eq!(menu_screen(), render::MenuScreen::Save);
        menu_down(); // slot 1
        menu_select(); // SaveSlot(1): menu closes like the C, host no-ops
        assert_eq!(menu_visible(), 0, "Save Enter closes the menu");
        // The world is untouched by the no-op (player still alive on e1m1).
        assert!(player_field("health") > 0.0);
    }

    #[test]
    fn web_extras_screen_toggles_through_the_exports() {
        assert_eq!(boot(), 1);
        assert_eq!(extras(), 0, "every extra defaults off");
        menu_down();
        menu_down();
        menu_select(); // -> Options
        for _ in 0..13 {
            menu_down(); // the port's row 13, Web extras
        }
        menu_select();
        assert_eq!(menu_screen_id(), 10, "Web extras opens its screen");
        menu_right(); // Uncapped framerate
        assert_eq!(extras(), 1);
        menu_down();
        menu_select(); // Enter toggles Show FPS
        assert_eq!(extras(), 3);
        menu_left(); // left toggles too
        assert_eq!(extras(), 1);
        menu_cancel();
        assert_eq!(menu_screen_id(), 5, "Esc returns to Options");
        menu_select(); // ...on the Web extras row
        assert_eq!(menu_screen_id(), 10);
        // The page's restore; unknown bits are dropped.
        set_extras(-1);
        assert_eq!(extras(), 15);
        set_extras(0);
        assert_eq!(extras(), 0);
        // They survive a re-boot (reset_nav) like the Options cvars.
        set_extras(2);
        assert_eq!(boot(), 1);
        assert_eq!(extras(), 2);
    }

    #[test]
    fn keys_screen_rebinds_forward_through_the_exports() {
        reset_queue();
        assert_eq!(boot(), 1);
        // Navigate: Options -> Customize controls (row 0).
        menu_down();
        menu_down();
        menu_select(); // -> Options
        menu_select(); // ROW_CONTROLS -> Keys screen
        assert_eq!(menu_screen(), render::MenuScreen::Keys);
        // Move to the "+forward" row (BIND_FORWARD = 3) and grab.
        for _ in 0..3 {
            menu_down();
        }
        assert_eq!(menu_bind_grabbing(), 0);
        menu_select();
        assert_eq!(menu_bind_grabbing(), 1, "Enter starts the bind grab");
        // +forward had two keys (w + UPARROW): the C unbinds them, then binds
        // the grabbed key.
        menu_bind_key(i32::from(b'o'));
        assert_eq!(menu_bind_grabbing(), 0);
        // Close the menu (Keys -> Options -> Main -> closed).
        menu_cancel();
        menu_cancel();
        menu_cancel();
        assert_eq!(menu_visible(), 0);
        // The new key drives +forward; the old one no longer does.
        key_down(i32::from(b'o'));
        step(0.05);
        // (400: Always Run defaults on, so +forward moves at the run speed.)
        assert_eq!(walk_mut(|w| w.key_move.fwd), 400.0, "rebound key moves forward");
        key_up(i32::from(b'o'));
        key_down(i32::from(b'w'));
        step(0.05);
        assert_eq!(walk_mut(|w| w.key_move.fwd), 0.0, "the old key was unbound");
        key_up(i32::from(b'w'));
    }
}

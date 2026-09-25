//! Menu glue — menu.c's `M_Keydown` as exports: the page's arrow/enter/escape
//! and Keys-screen presses reach the engine's `Menu`, and the actions it
//! returns (New Game, Video mode, Save/Load slot, Go to console) are carried
//! out here against the App.

use quake_rs::render::{self, MenuAction};

use crate::savegame::{do_load_command, do_save_command};
use crate::{build_walk_map, clamp_resolution, ensure_app, APP};

// --- main menu: keyboard navigation exports (ArrowUp/Down, Enter, Escape) ---

/// Move the menu cursor up one item (wraps), porting `K_UPARROW`. No-op when the
/// menu is hidden.
#[no_mangle]
pub extern "C" fn menu_up() {
    ensure_app(|a| {
        if a.menu.visible {
            a.menu.move_cursor(-1);
        }
    });
}

/// Move the menu cursor down one item (wraps), porting `K_DOWNARROW`. No-op when
/// the menu is hidden.
#[no_mangle]
pub extern "C" fn menu_down() {
    ensure_app(|a| {
        if a.menu.visible {
            a.menu.move_cursor(1);
        }
    });
}

/// Activate the highlighted menu item (Enter / `K_ENTER`). On
/// `MenuAction::NewGame` this rebuilds the walk on a fresh e1m1 (a new Server +
/// connected client) and closes the menu — the one-button "Single Player > New
/// Game". Other actions just update visibility/screen (handled inside `select`).
/// No-op when the menu is hidden.
#[no_mangle]
pub extern "C" fn menu_select() {
    // Decide the action under the borrow, then (if NewGame) rebuild the walk
    // afterward so we don't hold a &mut Walk while replacing it.
    let mut start_new_game = false;
    let mut new_size: Option<(usize, usize)> = None;
    let mut slot_action: Option<(bool, usize)> = None; // (is_save, slot)
    ensure_app(|a| {
        if a.menu.visible {
            match a.menu.select() {
                MenuAction::NewGame => start_new_game = true,
                MenuAction::ResolutionChanged => {
                    // Enter on a Video Options mode line (VID_MenuKey K_ENTER ->
                    // VID_SetMode): capture the new (clamped) size and resize the
                    // framebuffer after the borrow. The page notices the new
                    // width()/height(), re-fits the canvas and persists it.
                    let (rw, rh) = a.menu.resolution();
                    new_size = Some(clamp_resolution(rw, rh));
                }
                MenuAction::OpenConsole => {
                    // Options "Go to console": select() already closed the menu;
                    // open the drop-down console (Con_ToggleConsole_f).
                    a.console.open = true;
                }
                MenuAction::ResetDefaults => {
                    // Options "Reset to defaults": select() reset the in-menu
                    // cvars (viewsize/sensitivity/volume/... are read live each
                    // frame); the video mode is not a default.cfg cvar and stays.
                    a.menu.sync_resolution(a.render_w as i32, a.render_h as i32);
                }
                // Save/Load menu slots -> the Host_Savegame_f/Host_Loadgame_f
                // port, via the same console-command path (`save sN`/`load sN`,
                // the C's "s%i.sav" naming). Executed OUTSIDE this borrow:
                // a load swaps the whole Walk (like start_new_game).
                MenuAction::SaveSlot(i) => slot_action = Some((true, i)),
                MenuAction::LoadSlot(i) => slot_action = Some((false, i)),
                // Closed/Back/None already applied to the menu state inside
                // select(); nothing else for the host to do.
                _ => {}
            }
        }
    });
    if let Some((w, h)) = new_size {
        ensure_app(|a| a.set_render_size(w, h));
    }
    if let Some((is_save, i)) = slot_action {
        // Menu slot -> the same path as the console `save sN` / `load sN`
        // (Host_Savegame_f/Host_Loadgame_f port). Runs outside the borrow:
        // a successful load replaces the Walk.
        let name = format!("s{i}");
        if is_save {
            do_save_command(Some(&name));
        } else {
            do_load_command(Some(&name));
        }
    }
    if start_new_game {
        // Fresh single-player game on the start hub (NEW_GAME_MAP). Rebuild the
        // whole walk — new Server, new connected client — switch to walk mode and
        // leave the menu closed. From the hub the player picks skill + episode
        // (changelevel).
        if let Some(nw) = build_walk_map(render::NEW_GAME_MAP) {
            ensure_app(|a| {
                a.walk = Some(nw);
                a.mode = 0;
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

/// Back out of the menu (Escape / `K_ESCAPE`): a submenu returns to the main
/// screen; the main screen closes the menu. If the menu is hidden, OPEN it (so
/// Escape always reaches the menu, like Quake's `M_ToggleMenu_f` for `key_game`).
#[no_mangle]
pub extern "C" fn menu_cancel() {
    ensure_app(|a| {
        if a.menu.visible {
            let _ = a.menu.cancel();
        } else {
            a.menu.open();
        }
    });
}

/// Answer the Quit confirmation prompt "Yes" (the literal `Y` key, `M_Quit_Key`
/// 'y'/'Y'): close the menu (quit to the attract loop). A no-op off the Quit
/// screen, so the page can route a `Y` press here unconditionally while the menu
/// is up. Enter (`menu_select`) on the Quit screen does the same thing.
#[no_mangle]
pub extern "C" fn menu_quit_yes() {
    ensure_app(|a| {
        if a.menu.visible {
            let _ = a.menu.quit_yes();
        }
    });
}

/// Answer the Quit confirmation prompt "No" (the literal `N` key, `M_Quit_Key`
/// 'n'/'N'): back out to the screen the prompt rose from. A no-op off the Quit
/// screen. Escape (`menu_cancel`) on the Quit screen does the same thing.
#[no_mangle]
pub extern "C" fn menu_quit_no() {
    ensure_app(|a| {
        if a.menu.visible {
            let _ = a.menu.quit_no();
        }
    });
}

/// Adjust the highlighted Options row leftward (`K_LEFTARROW` -> `M_AdjustSliders
/// (-1)`; on Load/Save/Keys/Video it moves the cursor, on Help it pages). A
/// no-op when the menu is hidden. The Screen size row is `viewsize`, which the
/// next `step` frames the view with — it never touches the framebuffer size
/// (that is Enter on Video Options).
#[no_mangle]
pub extern "C" fn menu_left() {
    menu_adjust(-1);
}

/// Adjust the highlighted Options row rightward (`K_RIGHTARROW`). See
/// [`menu_left`].
#[no_mangle]
pub extern "C" fn menu_right() {
    menu_adjust(1);
}

/// Shared body of [`menu_left`]/[`menu_right`]. No-op when the menu is hidden.
fn menu_adjust(delta: i32) {
    ensure_app(|a| {
        if a.menu.visible {
            a.menu.adjust(delta);
        }
    });
}

/// Backspace/Del while the menu is up: on the Customize-controls screen this
/// unbinds the highlighted command (`M_Keys_Key` K_BACKSPACE/K_DEL); on every
/// other screen it is a no-op (the engine gates it).
#[no_mangle]
pub extern "C" fn menu_backspace() {
    ensure_app(|a| {
        if a.menu.visible {
            a.menu.keys_backspace();
        }
    });
}

/// 1 while the Keys screen is waiting for the next key to bind (`bind_grab`,
/// menu.c). The page reads this to route the NEXT raw keypress to
/// [`menu_bind_key`] instead of menu navigation.
#[no_mangle]
pub extern "C" fn menu_bind_grabbing() -> i32 {
    APP.with(|c| {
        c.borrow()
            .as_ref()
            .map(|a| (a.menu.visible && a.menu.bind_grabbing()) as i32)
            .unwrap_or(0)
    })
}

/// Deliver the grabbed key to the Keys screen (`M_Keys_Key`, the `bind_grab`
/// branch): Quake keynum in `0..256`. Escape cancels, backtick is refused, any
/// other key binds to the highlighted command; the grab ends either way. A
/// no-op when nothing is grabbing.
#[no_mangle]
pub extern "C" fn menu_bind_key(keynum: i32) {
    if !(0..256).contains(&keynum) {
        return;
    }
    ensure_app(|a| {
        if a.menu.visible {
            a.menu.bind_key(keynum as u8);
        }
    });
}

/// The menu screen currently showing, as a stable id — a read-only
/// verification/debug export (the browser checks the screen transitions:
/// Multiplayer opens, Save gates, Video applies). 0 Main, 1 SinglePlayer,
/// 2 Load, 3 Save, 4 Multiplayer, 5 Options, 6 Keys, 7 Video, 8 Help, 9 Quit.
#[no_mangle]
pub extern "C" fn menu_screen_id() -> i32 {
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
            })
            .unwrap_or(0)
    })
}

/// 1 when the menu is currently visible (capturing input), else 0. The page
/// reads this to route Arrow/Enter keys to the menu vs. the game.
#[no_mangle]
pub extern "C" fn menu_visible() -> i32 {
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
    use crate::test_util::*;
    use crate::{boot, boot_attract, height, key_down, key_up, set_resolution, step, width};

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

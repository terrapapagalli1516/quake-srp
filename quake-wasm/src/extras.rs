//! The port's Web extras: departures from id's WinQuake that are never the
//! default. (The one departure that IS on by default, Always Run, is id's own
//! Options setting with another default; it lives with the menu options.)
//!
//! One table lists them, [`render::WEB_EXTRAS`] (quake-rs `menu.rs`, where
//! the Options > Web extras page that draws its rows lives); one place holds
//! their values, the menu's [`render::Extras`], which the page keeps across
//! reloads (`extras` / `set_extras`). This file is their console side — each
//! extra's `wasm_*` variable (not an id name) behaves like one of id's cvars
//! (`Cvar_Command`: `wasm_x` prints `"wasm_x" is "0"`, `wasm_x 1` sets it, any
//! non-zero number is on) — and the renderer's per-frame copy of the values.
//!
//! | cvar | default | what 1 does |
//! |---|---|---|
//! | `wasm_uncapped` | 0 | a host frame every display refresh, without `Host_FilterTime`'s 72 fps cap (`host::step`) |
//! | `wasm_showfps` | 0 | QuakeWorld's `SCR_DrawFPS` frame-rate readout (`host::step`, `render::draw_fps`) |
//! | `wasm_scaled2d` | 0 | the 2-D layer blown up from 320x200 to fill the frame, where id draws it 1:1 at every resolution ([`quake_rs::draw::set_scaled_2d`], applied by `host::step`) |
//! | `wasm_exactpersp` | 0 | exact perspective at every pixel of walls and liquids, where id's renderer is exact every 16 pixels and affine in between (`D_DrawSpans16`, `Turbulent8`); [`quake_rs::render::RenderOptions::exact_perspective`] |

use std::cell::Cell;

use quake_rs::render::{self, WEB_EXTRAS};

use crate::app::App;

/// `Cvar_Command` for the extras: when `argv[0]` names one, print it (no
/// argument) or set it (`Q_atof` of the argument, non-zero is on) in the
/// menu, and return true; false when it is not an extra.
pub(crate) fn console_command(a: &mut App, argv: &[&str]) -> bool {
    let Some(name) = argv.first() else { return false };
    let Some(w) = WEB_EXTRAS.iter().find(|w| w.cvar.eq_ignore_ascii_case(name)) else {
        return false;
    };
    match argv.get(1) {
        None => {
            let on = a.menu.extras().get(w.extra) as u8;
            a.console.println(format!("\"{}\" is \"{on}\"", w.cvar));
        }
        Some(arg) => a.menu.set_extra(w.extra, arg.parse::<f32>().unwrap_or(0.0) != 0.0),
    }
    true
}

/// The extras' console variable names, for Tab completion.
pub(crate) fn cvar_names() -> impl Iterator<Item = &'static str> {
    WEB_EXTRAS.iter().map(|w| w.cvar)
}

/// The console's `wasm_help` lines for the extras, one per variable.
pub(crate) fn help_lines() -> impl Iterator<Item = String> {
    WEB_EXTRAS.iter().map(|w| format!("  {:<19}{}", format!("{} 0|1", w.cvar), w.summary))
}

thread_local! {
    static FRAME_EXTRAS: Cell<render::Extras> = const {
        Cell::new(render::Extras { uncapped: false, show_fps: false, exact_persp: false, scaled_2d: false })
    };
}

/// The extras as the frame being drawn sees them. The renderer's options are
/// built inside the client frame, under the App borrow, where the menu cannot
/// be reached; `host::step` copies the menu's extras here ([`set_frame_extras`])
/// before each frame.
pub(crate) fn extras() -> render::Extras {
    FRAME_EXTRAS.with(Cell::get)
}

/// Hand this frame's extras to the renderer (see [`extras`]).
pub(crate) fn set_frame_extras(e: render::Extras) {
    FRAME_EXTRAS.with(|c| c.set(e));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{boot, APP};
    use crate::console::console_toggle;
    use crate::host::step;
    use crate::test_util::{close_menu, run_console_line};

    /// A console line typed into the drop-down console, as the player would.
    fn console(line: &str) {
        console_toggle();
        run_console_line(line);
        console_toggle();
    }

    /// One frozen frame (dt = 0) of the live game.
    fn frame() -> Vec<u8> {
        step(0.0);
        APP.with(|c| c.borrow().as_ref().map(|a| a.fb.clone()).unwrap_or_default())
    }

    fn menu_extras() -> render::Extras {
        APP.with(|c| c.borrow().as_ref().unwrap().menu.extras())
    }

    #[test]
    fn wasm_exactpersp_is_a_cvar_that_switches_the_span_routine() {
        boot();
        close_menu();
        assert_eq!(menu_extras(), render::Extras::default(), "every extra is off by default");
        let id_spans = frame();
        assert!(!extras().exact_persp, "the frame saw it off");
        console("wasm_exactpersp 1");
        assert!(menu_extras().exact_persp, "the console sets the menu's extra");
        let exact = frame();
        assert!(extras().exact_persp, "and the next frame hands it to the renderer");
        assert_ne!(id_spans, exact, "exact perspective moves texels on the walls");
        console("wasm_exactpersp 0");
        assert!(!menu_extras().exact_persp);
        assert_eq!(frame(), id_spans, "and 0 is id's spans again");
        console("wasm_exactpersp junk");
        assert!(!menu_extras().exact_persp, "Q_atof of junk is 0");
    }
}

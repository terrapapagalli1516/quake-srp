//! The port's extras: departures from id's WinQuake that are never the
//! default. (The one departure that IS on by default, Always Run, is id's own
//! Options setting with another default; it lives with the menu options.)
//!
//! Every extra is a console variable named `wasm_*` — not an id name — that
//! behaves like one of id's cvars (`Cvar_Command`): `wasm_x` prints
//! `"wasm_x" is "0"`, `wasm_x 1` sets it (any non-zero number is on). They are
//! process state like id's cvars: not saved, back to 0 on a reload. This file
//! is the one place they are listed, parsed and stored; the code they switch
//! reads [`extras`]. An extras menu can drive the same table ([`CVARS`]).
//!
//! | cvar | default | what 1 does |
//! |---|---|---|
//! | `wasm_exactpersp` | 0 | exact perspective at every pixel of walls and liquids, where id's renderer is exact every 16 pixels and affine in between (`D_DrawSpans16`, `Turbulent8`); [`quake_rs::render::RenderOptions::exact_perspective`] |

use std::cell::Cell;

/// The extras' values (all off: id's behaviour).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Extras {
    /// `wasm_exactpersp`: exact per-pixel perspective.
    pub(crate) exact_persp: bool,
}

thread_local! {
    static EXTRAS: Cell<Extras> = const { Cell::new(Extras { exact_persp: false }) };
}

/// The extras as they stand.
pub(crate) fn extras() -> Extras {
    EXTRAS.with(Cell::get)
}

/// One extra's console variable: its name, and how it reads and sets its field.
pub(crate) struct ExtraCvar {
    pub(crate) name: &'static str,
    pub(crate) get: fn(&Extras) -> bool,
    pub(crate) set: fn(&mut Extras, bool),
}

/// Every extra, by console name.
pub(crate) const CVARS: &[ExtraCvar] = &[ExtraCvar {
    name: "wasm_exactpersp",
    get: |e| e.exact_persp,
    set: |e, on| e.exact_persp = on,
}];

/// Set an extra by console name; false if there is none.
pub(crate) fn set(name: &str, on: bool) -> bool {
    let Some(cv) = CVARS.iter().find(|cv| cv.name.eq_ignore_ascii_case(name)) else {
        return false;
    };
    EXTRAS.with(|c| {
        let mut e = c.get();
        (cv.set)(&mut e, on);
        c.set(e);
    });
    true
}

/// `Cvar_Command` for the extras: when `argv[0]` names one, print it (no
/// argument) or set it (`Q_atof` of the argument, non-zero is on) and return
/// the line to print, if any; `None` when it is not an extra.
pub(crate) fn console_command(argv: &[&str]) -> Option<Option<String>> {
    let name = argv.first()?;
    let cv = CVARS.iter().find(|cv| cv.name.eq_ignore_ascii_case(name))?;
    Some(match argv.get(1) {
        None => Some(format!("\"{}\" is \"{}\"", cv.name, (cv.get)(&extras()) as u8)),
        Some(arg) => {
            set(cv.name, arg.parse::<f32>().map(|v| v != 0.0).unwrap_or(false));
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{boot, APP};
    use crate::console::console_toggle;
    use crate::host::step;
    use crate::test_util::{close_menu, run_console_line};

    /// Puts the extras back to id's defaults when a test ends, pass or fail.
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            EXTRAS.with(|c| c.set(Extras::default()));
        }
    }

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

    #[test]
    fn wasm_exactpersp_is_a_cvar_that_switches_the_span_routine() {
        let _reset = Reset;
        assert_eq!(extras(), Extras::default(), "every extra is off by default");
        assert_eq!(console_command(&["wasm_exactpersp"]), Some(Some("\"wasm_exactpersp\" is \"0\"".into())));
        assert_eq!(console_command(&["viewsize"]), None, "not an extra");
        boot();
        close_menu();
        let id_spans = frame();
        console("wasm_exactpersp 1");
        assert!(extras().exact_persp);
        assert_eq!(console_command(&["WASM_EXACTPERSP"]), Some(Some("\"wasm_exactpersp\" is \"1\"".into())));
        let exact = frame();
        assert_ne!(id_spans, exact, "exact perspective moves texels on the walls");
        console("wasm_exactpersp 0");
        assert!(!extras().exact_persp);
        assert_eq!(frame(), id_spans, "and 0 is id's spans again");
        console("wasm_exactpersp junk");
        assert!(!extras().exact_persp, "Q_atof of junk is 0");
    }
}

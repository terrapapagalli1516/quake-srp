//! The 2-D oracle harness: drives the live [`App`](crate::app::App) from a
//! script, the way the page does (the same exports, the same `step`), and
//! writes the presented framebuffer. `oracle/screen2d.py` writes the script,
//! runs the same scenario through id's WinQuake (the C oracle) and diffs the
//! two screens pixel for pixel; see `oracle/README.md`.
//!
//! It is an ignored test, so it costs the shipped wasm nothing:
//!
//! ```sh
//! QUAKE_SCREEN_SCRIPT=script.txt cargo test --release --lib oracle_screen -- --ignored
//! ```
//!
//! One command per line (`#` starts a comment):
//!
//! | command | what |
//! |---|---|
//! | `res W H` | `set_resolution` |
//! | `blank IDX` | paint the 3-D view palette index IDX every frame (the C's `oracle_blank`); `-1` = off |
//! | `map NAME` | `boot()`, close the menu, then the console's `map NAME` |
//! | `frames N` | N host frames of 0.1 s (`step(0.1)`) |
//! | `cmd LINE` | a console command (`execute_console_command`) |
//! | `field NAME V [V V]` | set a field of the player's edict |
//! | `serverflags N` | the `serverflags` global |
//! | `impulse N` | queue an impulse with the next move |
//! | `console` | `console_toggle` |
//! | `type TEXT` | type TEXT into the console (no Enter: `key ENTER` submits it) |
//! | `key NAME` | a key press, down and up, through `Key_Event` (`key_event`) as the C's `oracle_key` (ESCAPE, ENTER, UPARROW, a, ...) |
//! | `showscores 0\|1` | hold / release Tab (`+showscores`) |
//! | `centerprint TEXT` | a QuakeC centerprint (`\n` = newline) |
//! | `print TEXT` | a QuakeC print (`\n` = newline): the notify lines and the console |
//! | `intermission N T [TEXT]` | svc_intermission (1) / svc_finale (2) / svc_cutscene (3), completed time T |
//! | `faceanim` | V_ParseDamage's pain face (`faceanimtime = cl.time + 0.2`) |
//! | `quitmsg N` | the quit prompt's message (`msgNumber`, else `rand()&7`) |
//! | `clocks REALTIME HOST_TIME CLTIME CENTERSTART` | the C shot frame's clocks, for the next `shot` |
//! | `shot PATH` | one more frame (`step(0.1)`, as the C's shot is the frame that ran `oracle_shot`), then the `clocks` handed over and the frame drawn again frozen (`step(0)`); writes the framebuffer as `PATH` (P6 PPM) |

use std::cell::Cell;

use quake_rs::render::Image;

use crate::app::{boot, ensure_app, APP};
use crate::console::{console_char, console_toggle};
use crate::host::step;
use crate::host_cmd::execute_console_command;
use crate::input::key_event;
use crate::vid::set_resolution;

thread_local! {
    /// The `blank` palette index (`None` = draw the 3-D view as usual).
    static BLANK: Cell<Option<u8>> = const { Cell::new(None) };
}

/// The live frame's view hook ([`quake_rs::client::set_view_hook`]): the 3-D
/// view as one flat colour while a script asks for it (the C oracle's
/// `oracle_blank` fills `scr_vrect` the same way).
fn blank_view(mut view: Image, palette: &[[u8; 3]; 256]) -> Image {
    if let Some(idx) = BLANK.with(Cell::get) {
        view.rgb.fill(palette[idx as usize]);
    }
    view
}

/// keys.c's key names (`Key_StringToKeynum`) for the keys a script presses:
/// a single character is itself, lower case.
fn keynum(name: &str) -> Option<i32> {
    use quake_rs::keys::*;
    Some(i32::from(match name.to_ascii_uppercase().as_str() {
        "TAB" => K_TAB,
        "ENTER" => K_ENTER,
        "ESCAPE" => K_ESCAPE,
        "SPACE" => K_SPACE,
        "BACKSPACE" => K_BACKSPACE,
        "UPARROW" => K_UPARROW,
        "DOWNARROW" => K_DOWNARROW,
        "LEFTARROW" => K_LEFTARROW,
        "RIGHTARROW" => K_RIGHTARROW,
        "SHIFT" => K_SHIFT,
        "DEL" => K_DEL,
        "PGUP" => K_PGUP,
        "PGDN" => K_PGDN,
        "HOME" => K_HOME,
        "END" => K_END,
        "PAUSE" => K_PAUSE,
        s if s.len() == 1 => s.to_ascii_lowercase().as_bytes()[0],
        _ => return None,
    }))
}

/// A key press, down and up, through `Key_Event` — what the C's
/// `oracle_key` does.
fn press(name: &str) {
    let k = keynum(name).unwrap_or_else(|| panic!("unknown key {name}"));
    key_event(k, 1, 0);
    key_event(k, 0, 0);
}

fn unescape(s: &str) -> String {
    s.replace("\\n", "\n")
}

fn write_ppm(path: &str) {
    let (w, h, fb) = APP.with(|c| {
        let b = c.borrow();
        let a = b.as_ref().expect("the app is booted");
        (a.render_w, a.render_h, a.fb.clone())
    });
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    for px in fb.chunks_exact(4) {
        out.extend_from_slice(&px[..3]);
    }
    std::fs::write(path, out).unwrap_or_else(|e| panic!("write {path}: {e}"));
}

fn run(script: &str) {
    let mut clocks: Option<(f64, f32, f32, f32)> = None;
    for (n, raw) in script.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (cmd, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();
        let args: Vec<&str> = rest.split_whitespace().collect();
        let num = |i: usize| -> f32 {
            args.get(i)
                .and_then(|s| s.parse().ok())
                .unwrap_or_else(|| panic!("line {}: `{line}` needs a number at {i}", n + 1))
        };
        match cmd {
            "res" => set_resolution(num(0) as i32, num(1) as i32),
            "blank" => {
                let v = num(0) as i32;
                BLANK.with(|b| b.set((0..256).contains(&v).then_some(v as u8)));
                quake_rs::client::set_view_hook(Some(blank_view));
            }
            "map" => {
                assert_eq!(boot(), 1, "boot");
                ensure_app(|a| a.menu.close());
                execute_console_command(&format!("map {rest}"));
            }
            "frames" => {
                for _ in 0..num(0) as usize {
                    step(0.1);
                }
            }
            "cmd" => execute_console_command(rest),
            "field" => {
                let name = args[0];
                let vals: Vec<f32> = args[1..].iter().map(|s| s.parse().unwrap()).collect();
                ensure_app(|a| {
                    let w = a.walk.as_mut().expect("a walk");
                    if vals.len() == 3 {
                        w.server.vm.ent_set_vector(w.player, name, [vals[0], vals[1], vals[2]]);
                    } else {
                        w.server.vm.ent_set_float(w.player, name, vals[0]);
                    }
                });
            }
            "serverflags" => ensure_app(|a| {
                a.walk.as_mut().expect("a walk").server.set_serverflags(num(0));
            }),
            "impulse" => ensure_app(|a| a.walk.as_mut().expect("a walk").next_impulse = num(0) as i32),
            "console" => console_toggle(),
            "type" => {
                for ch in rest.chars() {
                    console_char(ch as u32);
                }
            }
            "key" => press(rest),
            "showscores" => ensure_app(|a| a.keys_held[9] = num(0) != 0.0),
            "centerprint" => ensure_app(|a| {
                let w = a.walk.as_mut().expect("a walk");
                w.centerprint = Some((unescape(rest), w.host_time + 2.0));
            }),
            "print" => ensure_app(|a| {
                let w = a.walk.as_mut().expect("a walk");
                let t = w.host_time;
                w.notify.print(&unescape(rest), t);
            }),
            "intermission" => {
                let text = unescape(args.get(2..).map(|a| a.join(" ")).unwrap_or_default().as_str());
                let (kind, t) = (num(0) as u8, num(1));
                ensure_app(|a| {
                    let w = a.walk.as_mut().expect("a walk");
                    w.intermission = kind;
                    w.completed_time = t;
                    if kind >= 2 {
                        w.finale_text = text;
                        w.finale_start = w.clock;
                    }
                });
            }
            "quitmsg" => ensure_app(|a| a.menu.set_quit_message(num(0) as usize)),
            "faceanim" => ensure_app(|a| {
                let w = a.walk.as_mut().expect("a walk");
                w.faceanimtime = w.server.time() + 0.2;
            }),
            "clocks" => clocks = Some((num(0) as f64, num(1), num(2), num(3))),
            "shot" => {
                // The C's shot frame is a whole host frame (the one that ran
                // `oracle_shot`): run one, then hand the port that frame's
                // clocks and draw it again, frozen.
                step(0.1);
                if let Some((realtime, host_time, cltime, cstart)) = clocks.take() {
                    ensure_app(|a| {
                        // Host_FilterTime measures from oldrealtime: move it along.
                        a.oldrealtime += realtime - a.realtime;
                        a.realtime = realtime;
                        a.clock = host_time;
                        if let Some(w) = a.walk.as_mut() {
                            // The finale's reveal, as long into it as the C's.
                            w.finale_start = w.clock - (cltime - cstart);
                        }
                    });
                }
                step(0.0);
                write_ppm(rest);
            }
            _ => panic!("line {}: unknown command `{line}`", n + 1),
        }
    }
}

/// The harness entry point: runs `$QUAKE_SCREEN_SCRIPT` (does nothing
/// without it).
#[test]
#[ignore]
fn oracle_screen() {
    let Ok(path) = std::env::var("QUAKE_SCREEN_SCRIPT") else {
        return;
    };
    let script = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    run(&script);
}

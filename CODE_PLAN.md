# CODE_PLAN: making quake-rust a showcase of idiomatic Rust

Recon by the `rustcheck` agent, 2026-09-26, on `quake/2026` @ 3866e1b. No code changed.
Every number below was measured on scratch copies of the tree; the last section says how.
Items are sized for one agent in 2–4 hours. They are ranked, and each one says what it
touches and what it could collide with.

Every item is a refactor. It must leave Classic byte-identical, and the **identity check**
(§5, W0a.1) is how an item proves it. Since 2026-09-26 the identity check is
`uv run oracle/classic_check.py` (`oracle/README.md`, "Classic"), which covers W0a.1's
list and adds id's C.

---

## 0. Status (2026-09-26, end of the 2026 push)

The rest of this file is the plan as written that morning on `3866e1b`; its line numbers
are that tree's. Done since, each proven with the identity check:

| item | done by | merge | the key commits |
|---|---|---|---|
| R2, server state out of thread-locals | `q26/server` | `d5db64a` | `e707de1`..`0f83e40` (`Outbox`), `f4c8fd8` (`ServerCvars`), `4dc6130` (`QRand`) |
| R3, the renderer owns its state; `Scene` | `q26/multicore` | `a2c2944` | `35a862f` (`Renderer`, `Scene`, `begin_map`, no thread-locals), `f49e3c6` (row bands on N threads) |
| R4, settings and commands in the engine | `q26/settings` | `efa3bc7` | `02a32ad` (`Cvars`, `CVARS`, `cmd.rs`, `Bindings`) |
| R5, typed entity fields | `q26/vm` | `ea13e24` | `7bf7a3d` (`MoveType`, `Solid`, `EntFlags`, no by-name reads in engine code), `84d447d` (`Option` sentinels) |
| R7, quaketool's shape | `q26/tool` | `4f43321` | `d3f0ad3`, `cff6c1d`, `9d7095e` (one `Command` table, `Box<dyn Error>`), `69eca1c` (`forbid(unsafe_code)` on the binary and the integration tests) |
| R10, the VM's encapsulation and errors | `q26/vm`, `q26/server` | `ea13e24`, `d5db64a` | `9f2e847` (`Op` decoded at load), `99d95d1` (private `Vm`), `d63a0b4` (`intern` dedupe), `267fb8c` (output drained); `ff91bf2`, `b9a0986` (`QError::Program`: a QuakeC error ends the game as `Host_Error`) |

Also done, outside the numbered items:

- quake-wasm is a plain `fn main` WASI program on edition 2024 with
  `#![forbid(unsafe_code)]` (`q26/platform`, `a50d8d7`). Every crate and binary now
  forbids `unsafe` by attribute.
- The crate docs and the Cargo `description` describe the whole engine (W0b.8's first
  two bullets; `q26/review`, `5af262e`).
- `cargo doc` is clean outside `server/` and the VM (part of W0b.7; `b9c8ae5`).
- The `mipadjust` comment is fixed (W0b.8's last bullet; `q26/docs`).

**What is left: the next session's menu.**

- **W0a** (§5): pin the toolchain, edition 2024 for quake-rs, `rustfmt.toml` and one
  format commit, and the `[lints]` tables.
  - Nothing is pinned yet: there is no `rust-toolchain.toml`, and `rust-version` is still
    1.74.
  - quake-rs is on edition 2021.
  - 117 files are not rustfmt-clean.
- **W0b** (§5):
  - the 12 rustdoc warnings left in `server/` and the VM;
  - `missing_docs`, doctests (1 today);
  - moving the big test modules out.
- **R1**, the client frame, is the largest smell left. `walk_frame` is 1025 lines now
  (968 at the recon), and the demo frame still repeats the relink, view, screen and
  palette code.
- **R6**, a model precache and one shared map.
- **R8**, the menu's types. The closing review's first item belongs here: the old-era
  names (`wasm_*` cvars, `MenuScreen::Extras`, `EXTRAS_*`, the `extras` automation
  calls), renamed with `config.cfg` aliases.
- **R9**, the module layout. Last, alone.
- **R11**, a workspace.
- **§7**, the engine-owned `Host` session: `Host_Frame`, `Cmd_ExecuteString` and
  `Key_Event` still live in quake-wasm, and its end-to-end tests with them.
- **Loose ends** from the closing review:
  - 16 `thread_local!` blocks remain (from 39). Among them are `FILES`/`GAMEDIR` in
    `quake-wasm/src/common.rs`, `SCALED_2D` in `draw.rs`, the view hook in
    `client/mod.rs` and `recycle_image`'s pool in `render/mod.rs`.
  - Dead public functions: `Vm::global_ofs`, `Vm::ret_int`, `sound_names` in
    `server/mod.rs`.
  - 32 `#[allow(clippy::too_many_arguments)]` (from 49).

---

## 1. Verdict

**Where it already shines.**
- **The leaves are good Rust.** There are zero dependencies, and nothing in non-test code
  panics except 12 guarded `unwrap`/`expect` calls.
- **The file formats** decode through one bounds-checked cursor (`read.rs`) into a real
  error type (`error.rs`: `Display`, `Error::source`, `From<io::Error>`).
- **The QuakeC VM** is typed: an `Op` enum, `Fld`/`Glb` handles resolved once per progs
  (`vm.rs:114`), and a module doc that explains its memory model and why it has one.
- **Every module is named for the id file it ports**, and functions cite the C they port,
  so a reader can hold the port and id's source side by side.
- **Tests.** 611 tests run in 0.1 s. They are named for the id behaviour they pin (e.g.
  `keydown_is_each_screens_m_key`). Where the port reproduces an x86 quirk, it names the
  quirk and tests it (`render/edge.rs:377` `c_ftoi`).

**Where it would embarrass a showcase.** The problems are in the trunk, and they come
from growing a port one feature at a time.
- **One function is the whole client frame.** `client/cl_main.rs:175` `walk_frame` is
  968 lines, with a cognitive complexity of 110. Its demo twin,
  `client/cl_demo.rs:378` `render_demo_frame` (411 lines), repeats the relink, view,
  screen and palette code.
- **Two god structs carry the client state.** `Walk` has 57 pub fields and `DemoPlay`
  has 38; 29 of the field names appear in both.
- **Hidden global state: 31 `thread_local!` statics and 2 global atomics in the library.**
  - Server→client event queues: `server/msg.rs` has 7, and `server/host.rs` and
    `builtins.rs` have more.
  - Cvars: `skill`, `sv_gravity`, the scaled 2-D extra, the mip cvars.
  - Renderer caches, keyed on the `Bsp`'s *address* (`render/surf.rs:192`, `world.rs:153`).
  - Two process-global RNGs.
- **Telescoping argument lists.**
  - `render/mod.rs:866` `render_scene_ext_sprited` takes 16 arguments;
    `render_scene_ext` and `render_scene` wrap it.
  - 49 `#[allow(clippy::too_many_arguments)]`.
- **Strings and magic ints where types belong.**
  - The engine's hot paths read entity fields by name, e.g.
    `ent_get_float(ent, "flags") as i32 & FL_ONGROUND` (`server/sv_user.rs:54`), even
    though resolved handles exist.
  - The client picks the model kind by file suffix, every frame (`cl_main.rs:502`).
  - More: `intermission: u8` (0–3), the menu rows and binds as `usize` consts, and
    `-1`/`usize::MAX` sentinels.
- **Errors are swallowed in the frame loop.** `cl_main.rs:300` discards the server
  frame's `Result`, so a QuakeC runtime error vanishes silently.
- **Housekeeping.**
  - The tree is not rustfmt-formatted (1270 hunks in quake-rs, 309 in quake-wasm).
  - `rustdoc` gives 38 warnings (53 with private items), and there are 0 doctests.
  - The crate doc still describes only the file loaders (`lib.rs`).
  - `rust-version = "1.74"` has never been tested.
  - The `quaketool` binary and the integration tests are not under
    `forbid(unsafe_code)`: that attribute is on `lib.rs` only.

---

## 2. What was measured

| | quake-rs |
|---|---|
| Size | 40.2k lines of non-test code (including comments), 20.2k lines of tests, 8.8k doc-comment lines. 611 tests. |
| Files over 2k lines of code | `menu.rs` 2756 (+1817 test), `bin/quaketool.rs` 2528, `demo.rs` 2044 (+1329), `render/edge.rs` 2017. Over 2k with tests: `server/sv_phys.rs` 2316, `vm.rs` 2010. |
| Functions over 150 lines (non-test) | `cl_main.rs:175` walk_frame 968 · `quaketool.rs:603` cmd_playtest 461 · `quaketool/census.rs:346` census_map 460 · `cl_demo.rs:378` render_demo_frame 411 · `vm.rs:1091` execute 388 · `quaketool.rs:1661` cmd_scene 334 · `demo.rs:1292` parse_server_message 272 · `quaketool.rs:2059` cmd_view 272 · `server/sv_world.rs:174` sv_move 201 · `render/mod.rs:478` render_bsp 201 · `server/sv_phys.rs:229` push_move 192 · `quaketool.rs:1177` cmd_changelevel 190 · `menu.rs:1022` select 185 · `progs.rs:293` parse 160. In quake-wasm: `host_cmd.rs:114` execute_console_command 209. |
| Global state | 31 thread-local statics in the library: 11 server→client transports, 4 cvars, about 11 renderer caches or scratch buffers, 5 hooks/timers. Plus 2 global `AtomicU32` RNGs (`builtins.rs:135`, `server/sv_move.rs:315`), and 5 more thread-locals in `quaketool/census.rs`. |
| Panics in non-test code | 0 `panic!`/`unreachable!`/`assert!`. 7 `unwrap` (6 in `bsp.rs:995-1070`, 1 in the census). 5 `expect("just initialised")` on lazy caches. |
| Lints: default clippy | 0 warnings (1.93.1). |
| Lints: `pedantic` + `nursery`, all targets | 5476 findings. Mostly casts (2380: truncation 976, precision 512, wrap 476, sign 416), `doc_markdown` 642, `unreadable_literal` 501, `suboptimal_flops` 346, `cast_lossless` 333, `must_use_candidate` 240, `use_self` 213. Most per file: `render/alias.rs` 622, `vm.rs` 317, `menu.rs` 259, `render/raster.rs` 197. |
| Smells picked from `restriction` (non-test) | `as_conversions` 1700, `indexing_slicing` 722, `let_underscore_must_use` 182 (mostly `let _ = write!` into Strings), `unwrap_used` 7, `expect_used` 5, `exit` 2 (in the tool). |
| Lossy `as` casts, by type | `i32→usize` 218 (BSP and edict indices), `i32→f32` 210, `f32→i32` 151 (C `(int)`), `usize→i32` 100, `usize→f32` 99, `f64→f32` 73. |
| Docs | Every file has a module doc. `missing_docs` flags 473 pub items (progs.rs 108 and bsp.rs 95, which are on-disk struct fields; client/mod.rs 38; mdl.rs 32; keys.rs 25). rustdoc: 38 warnings (public) / 53 (private items). Doctests: 0. |
| Public surface | Every module is `pub mod`. 508 `pub fn`, 611 pub fields, 104 pub structs, 237 pub consts. `Vm` has 15 pub fields, including its internals (`globals`, `edict_fields`, `strings`, `builtins`). quake-wasm uses about 40 distinct engine paths. |
| Index loops | 297 `for i in 0..n`. Almost all are `0..3` vector components (id's `for (i=0;i<3;i++)`) or edict scans. Only 8 are `0..x.len()`. Not a real problem (see §3). |
| Errors | `QError { Io, Truncated, BadMagic, Invalid(String) }`, with 99 `Invalid` sites, including VM runtime errors. quaketool returns `Result<_, String>` from 39 functions, with 93 `map_err(\|e\| e.to_string())`. |

**Edition 2024 (dry run on a scratch copy).**
- **quake-rs.** `cargo fix --edition` changes one line: `render/edge.rs:1329` gains
  `+ use<'b>`, the new RPIT capture rule. `Cargo.toml` needs `rust-version` ≥ 1.85, since
  1.74 is rejected for edition 2024.
  - The other `rust-2024-compatibility` lints (`if_let_rescope`, `tail_expr_drop_order`,
    `keyword_idents_2024`, never-type fallback, expr fragments) find nothing.
  - On edition 2024: 601+2+8 tests pass, the goldens are unchanged
    (`4807aaa1`/`9ae2b478`/`c65b7046`), and clippy is clean except two lints that the
    higher MSRV unlocks (`manual_repeat_n` in `render/fixtures.rs:109`, `unnecessary_map_or`
    in `quaketool.rs:733`).
  - **Risk: none found.**
- **quake-wasm.** The change is 94 `#[no_mangle]` → `#[unsafe(no_mangle)]`. It is moot:
  the `platform` agent is removing the exports and moves the shell to 2024 itself.
- **rustfmt.** Under edition 2024, `cargo fmt` defaults to style edition 2024, so the
  formatting decision below covers it.

**Toolchain 1.98.1 (installed; I added its `clippy` component for this recon).**
- 0 new rustc warnings. New default clippy lints: 3 in quake-rs and 13 in quake-wasm.
  - quake-rs: `manual_checked_ops` `mdl.rs:396`, `while_let_loop` `save.rs:598`,
    `byte_char_slices` `menu.rs:3575`.
  - quake-wasm: `chunks_exact_to_as_chunks`.
- Tests pass and the goldens are identical.
- **Frame hashes are identical across toolchain and target.** `quaketool play … demo1,walk_e1m1,fire_e1m1 --res 320x200 --hash-every 60`
  prints the same hashes and sound tallies in all four cases: 1.93.1 and 1.98.1, each
  native and built for `wasm32-wasip1` and run under node's WASI. So wasm's libm
  (compiler-builtins) agrees with glibc on this workload today.
- **A pin is still wise.** Rust's float codegen is IEEE, so native output is stable, but
  two things drift with the toolchain:
  - the "clippy: 0 warnings" gate (each release adds lints);
  - wasm's software transcendental functions.

  Pin, and bump on purpose, running the identity check each time.

**rustfmt.** The code is formatted by hand in a style close to rustfmt at width 120. The
config that fits it best is `style_edition = "2024"`, `max_width = 120`,
`use_small_heuristics = "Max"`.
- Diff: 68 files, +2697 / −3986 lines.
- Checked: tests pass, goldens unchanged, and the result is idempotent.
- Default rustfmt settings would be much worse: +9416 / −2151.

---

## 3. Keep vs change

**Keep: id's shape on purpose.** Write `#[expect(lint, reason = "…")]` on these; do not
"fix" them.

| where | what | why keep |
|---|---|---|
| `render/edge.rs:206` `EdgeState` | `r_edge.c`/`r_draw.c`/`r_bsp.c` statics as fields, including the never-reset `leftenter`/`leftexit` | It is id's machine; the oracle pins it pixel-for-pixel. (Its *home*, a thread-local, is the part to change: R3.) |
| `render/edge.rs:377` `c_ftoi`, `render/raster.rs:604` `span_cached`, `edge.rs:1507` `R_ScanEdges` | 16.16/12.20 fixed point, 16-pixel spans, `as i32` truncation, index loops | The inner loops *are* the renderer. Iterators and checked casts would hide the arithmetic being ported. |
| `render/polyse.rs:593` `ADIVTAB`, `polyse.rs:445` `D_PolysetDrawSpans8` | id's table and affine filler | Same. Gets `#[rustfmt::skip]` and `unreadable_literal` allowed. |
| `vm.rs:1091` `execute` | one `match` over `Op` (`PR_ExecuteProgram`), and the `first` flag that reproduces `s++` | A flat dispatch is the clearest honest shape for an interpreter. What changes is the decode (R10), not the match. |
| `world.rs:371` `recursive_hull_check`, `math.rs:298` `box_on_plane_side` | id's recursion and signbits switch | Faithfulness; tested. |
| `trace.fraction == 1.0`, `sv_move.rs:364-404` float `while tdir <= 315.0` | exact float compares and float loops | id compares exactly. `float_cmp` is allowed project-wide (§5); `while_float` is a nursery lint and stays off. |
| `f32` vs `f64` choices | `sv.time` and `host_frametime` are `double` in id, the rest `float` | The types mirror the C on purpose (Classic timing depends on it). |
| `0..3` component loops, `pub type Vec3 = [f32; 3]` | `vec3_t` | A `Vec3` newtype with operators would touch about 480 type mentions plus the hand-expanded component math, and risk changing evaluation order, for little gain. **Not worth it** (§8). |

**Change: unidiomatic by accident.**

| where | smell | item |
|---|---|---|
| `client/cl_main.rs:175`, `cl_demo.rs:378`, `client/mod.rs:70,253` | 968-line frame; duplicated demo path; god structs | R1 |
| `server/msg.rs:69-657`, `builtins.rs:55,135`, `server/host.rs:50-170`, `server/lightstyle.rs:37`, `sv_move.rs:315` | thread-local queues and cvars, global RNGs. The rationale in `builtins.rs:131`, "the `Vm` struct has a fixed field set we must not extend", is obsolete. | R2 |
| `render/*` thread-locals, `render/surf.rs:192` `WorldFingerprint { ptr: bsp as *const Bsp as usize, … }`, `world.rs:153` | caches keyed on an address; hidden per-thread state | R3 |
| `render/mod.rs:725/826/866`, `edge.rs:414` (13 args) | telescoping entry points | R3 |
| `menu.rs:625` `Menu` owns cvars and `bindings: [Option<u8>; 256]`; `draw.rs:17` `SCALED_2D`; `surf.rs:291` `MIP_CVARS`; `quake-wasm/src/host_cmd.rs:114` | settings spread across the menu, thread-locals and the shell; commands as a 209-line string match | R4 |
| `server/mod.rs:97-120` `MOVETYPE_*`/`FL_*` as raw `i32`; by-name field reads in the engine (`sv_user.rs` 49 lines, `cl_main.rs` 30, `pr_cmds.rs` 17, `sv_main.rs` 15, `client/host_cmd.rs` 15) | stringly typed, magic ints | R5 |
| `client/mod.rs:90-99` three `HashMap<String, Option<_>>` model caches; suffix dispatch `cl_main.rs:423-587`; the map parsed twice (`client/mod.rs:74`) | stringly typed, duplicate data | R6 |
| `bin/quaketool.rs` `#[path = "quaketool/…"]` modules, 27 commands dispatched by hand, the module doc listing 8 of them | tool shape | R7 |
| `menu.rs:56-71` `ROW_*`, `menu.rs:258-297` `BIND_*` as `usize`; `client/mod.rs:229` `intermission: u8` | ints that want enums | R8, R1 |
| flat `src/` of 26 modules mixing formats, QC, client-side effects and 2-D UI; `render/mod.rs:65-81` re-exports the 2-D layer; the debug `render_bsp`/`demo_room` in `render/mod.rs` | layout | R9 |
| `vm.rs:261` 15 pub fields; `Op::from_u16` on every executed statement; VM errors as `QError::Invalid(String)` | encapsulation and error typing | R10 |
| `client/cl_main.rs:300` `let _ = w.server.client_frame_f64(..)`; the same at `server/host.rs:310-333` and `server/pr_edict.rs:338` | the live frame drops a QuakeC runtime error without a word (id's `Host_Error` prints it and ends the game) | R10 |

---

## 4. Rules for new code, starting now (waves 2–3)

Hand these to every agent, so that new work does not add to the debt below.
- **No new `thread_local!` or `static` state.** Put it on the struct that owns the
  lifetime (server, client, renderer, host session). The one exception is the platform's
  main-loop handle.
- **No more than 7 parameters.** Past that, add a struct (id's `refdef_t`, `entity_t`
  shapes). No new `#[allow(clippy::too_many_arguments)]`.
- **New settings are typed fields** on the settings object (R4), never loose globals, and
  never fields on `Menu`.
- **Read entity fields through `vm.fo.*`** (`Fld`), never by name, in per-frame code.
- **A new module is named for the id file it ports**, and its doc says which C functions
  it ports and why it departs where it does. Functions stay under ~120 lines; the
  exceptions are ported inner loops, marked `#[expect(clippy::too_many_lines, reason = "id's …")]`.
- **Suppress lints with `#[expect(…, reason = "…")]`,** not `#[allow]`.
- **Don't run rustfmt** on files you did not otherwise change (see W0a).

---

## 5. Quick mechanical wins (safe to batch)

Two windows at the start of wave 4. Run them **when no other branch is open**, because
they touch every file.

### W0a: toolchain, edition, format, lints (one agent, about 3 h)

1. **The identity check, as one command.** A script or `quaketool identity <pak>` that
   prints the following, so each later window diffs two text files:
   - the goldens' sha256;
   - `quaketool play <pak> demo1,demo2,demo3,walk_e1m1,walk_e1m3,fire_e1m1,quad_e1m1 --res 320x200,640x400 --hash-every 30`
     (hashes and sound tallies);
   - the `timedemo` frame counts (969/985/1090).

   Each piece takes seconds today.
2. **Pin the toolchain.** Add `rust-toolchain.toml` with `channel = "1.98.1"`,
   `components = ["clippy", "rustfmt"]` and `targets = ["wasm32-wasip1", "wasm32-wasip1-threads"]`.
   - The installed 1.98.1 lacks `rustfmt` (and `wasm32-unknown-unknown`, which the
     platform agent is dropping), so expect a one-time rustup download.
   - Set `rust-version` to the pinned version. 1.74 was never tested, and edition 2024
     needs ≥ 1.85.
   - Fix the three 1.98.1 lints.
3. **Edition 2024 for quake-rs.** One line (`edge.rs:1329`). Proof: tests and goldens
   (already shown on a scratch copy).
4. **rustfmt.** Add `rustfmt.toml` with `style_edition = "2024"`, `max_width = 120` and
   `use_small_heuristics = "Max"`.
   - Put `#[rustfmt::skip]` on id's tables (`ADIVTAB`, palette ramps, `WEB_EXTRAS`) if
     rustfmt explodes them.
   - Make it one commit, listed in `.git-blame-ignore-revs`.
5. **A `[lints]` table in both `Cargo.toml`s.** Proposed set below. It puts
   `unsafe_code = "forbid"` on *every* target (the quaketool binary and the tests
   included), which the crate attribute does not do today.
   - Tried on a scratch copy: 1547 findings.
   - `cargo clippy --fix` **one lint at a time** clears 643 of them (1426 lines changed
     in 59 files, verified by the tests, the goldens, and `play` hashes).
   - Do not run `--fix` with every lint at once: one bad suggestion (`E0271` from
     `redundant_closure_for_method_calls`) aborts every fix for the crate.
   - Left for hand fixes, about 250: `manual_let_else` 27, `items_after_statements` 23,
     `trivially_copy_pass_by_ref` 16, `format_push_string` 12 (`save.rs` → `write!`),
     `unwrap_used`/`expect_used` 12, and about 20 other lints with 1–7 findings each.
   - Also: 94 `elided_lifetimes_in_paths` (`cargo fix`), and 59 `#[allow]` →
     `#[expect(…, reason)]` (each reason names the id function).
6. **Small fixes.**
   - `bsp.rs:995-1070`: turn the unwrap chain into
     `anims[..max].iter().copied().collect::<Option<Vec<_>>>()`.
   - `surf.rs:542/671/760`, `world.rs:198`, `dlight.rs:146`:
     `slot.as_mut().expect("just initialised")` → `get_or_insert_with`.
   - Remove the redundant inner `#![forbid(unsafe_code)]` in `demo.rs:28`.

Proposed lint set. It is the one tried above; the allow-list is where id's arithmetic
makes the lint noise:

```toml
[lints.rust]
unsafe_code = "forbid"
unreachable_pub = "warn"
unused_qualifications = "warn"
rust_2018_idioms = { level = "warn", priority = -1 }
# missing_docs = "warn"            # switched on by W0b once the pub items are documented

[lints.rustdoc]
broken_intra_doc_links = "warn"

[lints.clippy]
all = { level = "warn", priority = -1 }
pedantic = { level = "warn", priority = -1 }
# id's C arithmetic: (int) truncation, int<->float, index casts. Audited by type in CODE_PLAN §2.
cast_possible_truncation = "allow"
cast_sign_loss = "allow"
cast_possible_wrap = "allow"
cast_precision_loss = "allow"
float_cmp = "allow"            # trace.fraction == 1.0, as id compares
unreadable_literal = "allow"   # tables copied from id's C
similar_names = "allow"        # id's names: u, v, s, t, zi, izi
many_single_char_names = "allow"
module_name_repetitions = "allow"
must_use_candidate = "allow"
unnecessary_wraps = "allow"    # every builtin shares fn(&mut Vm) -> Result<()>
doc_markdown = "allow"         # 569 C names without backticks; cosmetic
missing_errors_doc = "allow"
missing_panics_doc = "allow"
# restriction picks
unwrap_used = "warn"
expect_used = "warn"
panic = "warn"
dbg_macro = "warn"
todo = "warn"
allow_attributes = "warn"                  # use #[expect(..., reason = "...")]
allow_attributes_without_reason = "warn"
use_self = "warn"
```

It also needs a `clippy.toml` with `too-many-lines-threshold = 150`,
`allow-unwrap-in-tests = true`, `allow-expect-in-tests = true` and
`allow-panic-in-tests = true`. Let `too_many_lines` fire: the 14 functions in §2 should
each get fixed or get an `#[expect]` whose reason names id's function.

### W0b: docs (one agent, 2–3 h)

7. **Fix the 38 rustdoc warnings** (broken intra-doc links such as `Mdl`, `Wad2`, `new`;
   public docs linking private items).
8. **Fix the stale docs.**
   - `lib.rs` crate doc: it still says "the file loaders; the rest is a roadmap". Write
     the layer map and the zero-deps / no-unsafe / Classic rules instead.
   - The `Cargo.toml` `description` fields: quake-rs says "math library and asset-format
     loaders", and quake-wasm claims it is `forbid(unsafe_code)`.
   - The `quaketool.rs` module doc lists 8 of the 27 commands.
   - `server/mod.rs:45` says builtins read fields "by name".
   - `builtins.rs:131`: the "fixed field set" comment.
   - `server/mod.rs:113` `FL_WATERJUMP` "out of scope".
   - The `render/surf.rs` `mipadjust` comment, which AUDIT lists.
9. **Doctests.** Add 3–5 runnable examples that need no game data:
   - open an in-memory PAK;
   - parse the synthetic BSP;
   - run a QC function on the generated progs;
   - render `demo_room()` to an `Image`.
10. **`missing_docs`.** Document the 473 pub items. Most are on-disk fields, where one
    line naming the C field (`dface_t.planenum`) is enough. Then switch the lint on.
11. **Move big test modules out.** For files over ~1.5k lines (menu, demo, sv_phys,
    light, alias, sbar, surf, msg, bsp, world, particles), move `mod tests` into a
    `<module>/tests.rs` sibling (`#[cfg(test)] mod tests;`). It is a pure move. After it,
    four files are still over 2k lines of code: `menu.rs` 2756 (R8), `quaketool.rs` 2528
    (R7), `demo.rs` 2044 (R9 splits the framing from `cl_parse`) and `edge.rs` 2017
    (kept, since it is id's four files in one machine).

---

## 6. Ranked refactors

Each item gives **Value · Risk · Size**, what it touches, what it conflicts with, and its
proof. "Identity" means the identity check (W0a.1). Rank is value to the showcase per
unit of risk.

### R1. The client frame: one pipeline for live play and demos. Value **high** · Risk medium · Size 2 windows

This is the most visible smell (§1). id has one client state (`client_state_t cl`) and
one path (`CL_RelinkEntities` → `V_RenderView` → `SCR_UpdateScreen`) for both live play
and demos; the port has two copies.
- **R1a.**
  - Extract the 29 shared fields of `Walk`/`DemoPlay` into sub-structs that both embed:
    - `ClientAssets`: pak, palette, colormap, gfx_wad, conchars, the `pic_*`s;
    - `ViewState`: cshifts, `v_dmg_*`, `oldz`, `viewsize`, face and item flash times;
    - `Hud`: centerprint, notify, finale;
    - `Effects`: particles, prng, beams, trails, tracercount, dlights.
  - `intermission: u8` becomes `enum Intermission { None, Stats, Finale, Cutscene }`.
  - Split `walk_frame` into phase functions named for id's: `cl_adjust_angles`,
    `cl_base_move`, `host_server_frame`, `cl_take_server_events`, `cl_relink_entities` →
    an `EntityLists` value, `s_update_ambient`, `v_calc_refdef`, `render_view`,
    `scr_update_screen`, `v_update_palette`.
  - `walk_frame` becomes the ordered list of calls, as `_Host_Frame` reads, with no phase
    over ~120 lines.
- **R1b.** `demo_frame` calls the same phase functions on the same sub-structs. The
  demo-only differences become arguments: the recorded clock and entity list, and no
  server.
- **Touches:** `client/*`, callers in quake-wasm and in `quaketool play`/`playtest`.
- **Conflicts:** `client/` belongs to `framerate` now; `input` (wave 2) and `lerpmove`
  (wave 3) come later. Do it after those land.
- **Proof:**
  - identity: the `play` hashes for demo1–3 and the walks, and the sound tallies;
  - timedemo frame counts;
  - the shell's end-to-end tests.

### R2. Server state out of thread-locals. **Done** (`q26/server`, `d5db64a`; §0)

- **One `Outbox` for the transports.** Sounds, static sounds, particle bursts, messages,
  temp entities, svc events, stufftext, the changelevel/restart requests, and lightstyle
  writes all go into one `Outbox` owned by the server's `WorldModel` (the `vm::Host`
  impl, `vm.rs:86`).
  - Builtins reach it through the existing `vm.with_host` (`vm.rs:414`), plus one trait
    method: `fn outbox(&mut self) -> &mut Outbox`.
  - The server drains it into its frame report, as today.
  - Move one queue per commit.
- **`skill` and `sv_gravity`** (`server/host.rs:50,88`) become server fields, or R4's
  settings.
- **The RNGs.** `random()` (`builtins.rs:135`) and `SV_NewChaseDir`'s `rand` (`sv_move.rs:315`)
  move into a `QRand` owned by the host session and handed to each new `Server`.
  - The streams must continue across level loads, as they do now: `quaketool play`'s
    hashes depend on what ran before (`bin/quaketool/play.rs:28`).
  - Keep the two streams separate. id has one libc stream, but merging them would change
    every hash for no fidelity gain.
- **Touches:** `server/{msg,host,lightstyle,sv_move}.rs`, `builtins.rs`, `vm.rs` (the
  trait), `client/cl_main.rs` (drain).
- **Conflicts:** `server/`, the VM (`framerate` now).
- **Proof:** identity, a census diff of 0 new rows, tests. Tests stop depending on their
  order within a thread.

### R3. The renderer owns its state; a `Scene` replaces the argument lists. **Done** (`q26/multicore`, `a2c2944`; §0)

- **A `Renderer` struct owns what is now in thread-locals:**
  - `EdgeState`;
  - the surface, light and geometry caches;
  - `MipCvars` (becoming options);
  - the z-buffer and the RGB pool;
  - the warp tables, the dlight scratch, and the stats.

  `Renderer::begin_map(&Bsp)` resets the per-map state. That retires `WorldFingerprint`
  (identity by address). Build the hull tables in `world.rs` once, into the world model,
  as `Mod_MakeHull0` does at load.
- **`Scene<'a>`** (id's `refdef_t` plus the entity lists) replaces `render_scene` /
  `_ext` / `_ext_sprited` and `render_edges`' 13 arguments. That removes about 20
  `too_many_arguments` suppressions.
- **Why now, not in wave 4:** `multicore` will run the renderer on `std::thread`. With
  thread-locals, each worker silently gets its own cold surface cache and edge state.
  **Recommend the multicore agent do R3 as its step 1** (it owns `render/` then); if it
  does not, R3 is the first wave-4 window.
- **Touches:** `render/*`, `client/*` (callers), `quaketool` (scene/view/render), quake-wasm.
- **Conflicts:** `hires` now, `multicore` next, dithering in wave 3.
- **Proof:** goldens, oracle rows 100.00%, identity.

### R4. Settings and commands in the engine. **Done** (`q26/settings`, `efa3bc7`; §0)

These are the shape recommendations for that agent's work.
- **A typed `Cvars` struct** (plus a name table for the console and `config.cfg`:
  name, default, archive flag, get/set) owned by the host session and passed by `&` to
  the client frame and the renderer. It absorbs:
  - the menu's viewsize, sensitivity, volume, gamma, bgm, always-run, invert-mouse,
    lookspring, lookstrafe, name and extras;
  - `skill` and `sv_gravity`;
  - `SCALED_2D` and `MIP_CVARS`;
  - quake-wasm's per-frame `FRAME_EXTRAS` copy.
- **`Menu` edits it and stops owning it.**
- **A command table** (`cmd.c`'s `Cmd_AddCommand`) in the engine: console dispatch, Tab
  completion, `help` and `wasm_help` all read one list. Today there are three: the
  string match, the completion list and a hand-written help text.
- **Key bindings move from `Menu` to `keys`,** as a `Bindings` type (keys.c's
  `keybindings`).
- **Conflicts:** `menu.rs`, `draw.rs`, `render/surf.rs`, `server/host.rs`, quake-wasm
  (`extras.rs`, `host_cmd.rs`).
- **Proof:** identity, screen2d oracle, `verify_menu`/`verify_extras`.

### R5. Typed entity fields. **Done** (`q26/vm`, `ea13e24`; §0)

- **Enums and flags:** `MoveType` and `Solid` (each with an `Other(i32)` arm for the
  arbitrary values QC may write), and an `EntFlags(i32)` bit-set with `contains`.
- **Move every by-name read in engine code to `vm.fo`** (tools and tests may keep the
  by-name API). That fixes the stringly reads and the per-frame string hashing
  (`sv_user.rs:54` and the others in §3). Optionally, let the `FieldOfs` macro generate
  typed accessors (`vm.ent(e).origin()`).
- **Sentinels become `Option`:** `Server.player: i32 = -1`, `next_impulse`,
  `last_spawned_idx = usize::MAX`.
- **Touches:** `server/*`, `client/*`, `vm.rs`.
- **Conflicts:** `framerate` (server/client).
- **Proof:** identity, census, `quaketool simbench` before/after (expect faster).

### R6. A model precache and one shared map. Value medium · Risk low–medium · Size 1 window

- **One `ModelPrecache`, indexed by `modelindex`,** for live play and demos alike (the
  demo path already has `models: Vec<Option<Mdl>>`). Its entries are
  `enum ClientModel { World, Brush(usize), Alias(Mdl), Sprite(Sprite), External(Bsp) }`,
  resolved when a model is precached, the way `cl.model_precache` is filled. It replaces
  the three `HashMap<String, Option<_>>` caches and the per-frame suffix dispatch.
- **The map is parsed once and shared** as `Arc<Bsp>` between the server and the client
  (`Walk.bsp` is a second parse today). `Arc` also suits multicore.
- **Conflicts:** `client/`, `server/pr_cmds.rs` (setmodel), `render/world.rs`.
- **Proof:** identity, tests.

### R7. quaketool's shape. **Done** (`q26/tool`, `4f43321`; §0)

- **One directory, one command table.** Move to `src/bin/quaketool/main.rs`, with
  modules `assets` (info, ls, cat, bsp, map, mdl, spr, wad, dis, run), `render` (render,
  render-demo, menu, scene, view), `sim` (sim, simbench, playtest, changelevel, walk,
  demo), and the existing `census`, `play` and `timedemo`, plus the `framerate` and
  `hires` tools.
  - A `Command { name, usage, run }` table drives both dispatch and `--help`.
  - Commands return `Box<dyn Error>`, so `?` works on `QError`.
- **Keep `cmd_scene`'s pixel path verbatim.** It generates the goldens through its own
  entity gathering, not the client's. Say so in its doc; changing it means new goldens.
- **Conflicts:** `hires` (scene tooling), `framerate` (its tool).
- **Proof:** goldens, and identical `census`/`play`/`timedemo` output.

### R8. The menu's types (after R4). Value medium · Risk low · Size 1 window

- `ROW_*` (`menu.rs:56-71`) becomes `enum OptionsRow`, and `BIND_*` (`menu.rs:258-297`)
  becomes `enum Bind`.
- `select()` (`menu.rs:1022`, 185 lines) splits per screen.
- `Menu` shrinks to navigation state.
- **Conflicts:** `settings`, `input` (bindings).
- **Proof:** screen2d oracle, `verify_menu`, tests.

### R9. Module layout. Value medium (what a reader sees first) · Risk low (pure moves) · Size 1 window, **last**

- **Group the flat modules by layer:**
  - `formats/`: pak, wad, bsp, mdl, spr, crc, read;
  - `qc/`: progs, vm, builtins;
  - into `client/`: `demo` (cl_parse plus the framing), particles, tent, dlight, snd;
  - `ui/`: draw, screen, sbar, menu, keys, console.
- **`render/` stops re-exporting the 2-D layer** (`render/mod.rs:65-81`), and the debug
  `render_bsp` and `demo_room` go to `render/debug.rs`.
- Make it one commit per group, with import fixes only.
- **Conflicts:** every open branch, so run it when nothing else is open.
- **Proof:** identity, tests.

### R10. The VM's encapsulation and errors. **Done** (`q26/vm`, `ea13e24`, and `q26/server`, `d5db64a`; §0)

- **Decode `Statement.op` into `Op` once, at load,** with an `Op::Invalid(u16)` arm so
  a bad opcode still errors only when it executes.
- **Make `Vm`'s 15 pub fields private,** with accessors. `save.rs` and the census reach
  in today.
- **Add `QError::Vm(RunError { function, statement, message })`.** It lets `objerror`
  and `error` end the game as `Host_Error` does (AUDIT open, CENSUS L16). That is a
  fidelity change, so it goes in its own commit, with an AUDIT row.
- **Stop discarding frame results.** `client/cl_main.rs:300` does
  `let _ = w.server.client_frame_f64(..)`, so a QuakeC runtime error in a live frame
  vanishes; the settle frames (`server/host.rs:310-333`, `server/pr_edict.rs:338`) do
  the same. With a typed `RunError`, the client can do what id does: print the error to
  the console and end the game.
- Fold in AUDIT's `Vm::intern` dedupe and the `vm.output` drain.
- **Conflicts:** the VM (`framerate`).
- **Proof:** census, simbench before/after, tests.

### R11. Workspace (after `platform`). Value low–medium · Risk low · Size ½ window

- A root `Cargo.toml` `[workspace]` with both crates: one lock file, one `target/`,
  `[workspace.package]` (edition, rust-version, license) and `[workspace.lints]`.
- Profiles become workspace-level. Keep quake-wasm's `panic = "abort"` and
  `codegen-units = 1`, and check that native timings don't regress.
- Fix every path that names `quake-rs/target` or `quake-wasm/target`: the README, the
  `web/` scripts, `oracle/`.
- **Conflicts:** `platform` (build paths).

**Order in wave 4** (exclusive windows): W0a → W0b → R2 → R5 → R1a → R1b → R6 → R7 → R8
→ R10 → R9 last. R11 goes into W0a if `platform` has landed by then. R3 and R4 belong in
wave 2 with `multicore` and `settings`; if they are not done there, they go after W0b. If
the budget allows only four wave-4 windows, take **W0a, R1a, R1b, R2**.

---

## 7. The shell (quake-wasm, web)

These structural issues are likely to survive the `platform` rewrite.
- **Host logic lives in the shell.** id's `host.c`/`cmd.c`/`keys.c` live in quake-wasm:
  - `Host_Frame` gating (`host.rs` `step`);
  - `Cmd_ExecuteString` (`host_cmd.rs:114`);
  - `Key_Event` dispatch (`input.rs`);
  - the savegame glue.

  Native tools can't run them, so `quaketool play` re-creates the host's defaults. The
  engine should own a `Host` session (`frame(dt, &Input) -> Frame`, with a small
  platform trait for storage and sound out); R4's tables are its first piece.
  *Still so on 2026-09-26: the settings (R4) and `cmd.rs`'s table type are in the engine,
  but the command table itself (`quake-wasm/src/host_cmd.rs` `COMMANDS`), `step` and
  `Key_Event` are the shell's.*
- **The engine's end-to-end tests live in the shell** (135 tests, because they need the
  embedded pak). They belong in `quake-rs/tests`, reading `../quake-data` and skipping
  with a message when it is absent, as `oracle_screen` does. *(172 on 2026-09-26; the
  pak is a file now, read through the search path.)*
- **Extras reach the renderer** through a per-frame thread-local copy
  (`quake-wasm/src/extras.rs`) and engine setters (`draw::set_scaled_2d`,
  `render::set_mip_cvars`); R4 removes all three. *(Done but for one: `extras.rs` and
  `set_mip_cvars` are gone, the mip and video settings reach the renderer per frame
  through `Vid`, and `draw::set_scaled_2d` still sets a thread-local.)*

---

## 8. Not worth doing (accepted gaps)

- **A `Vec3` newtype with operators.** About 480 type mentions plus the component math,
  a risk of reordered float ops, and `[f32; 3]` *is* `vec3_t` and three QC cells.
- **Iterators or checked casts in the renderer's inner loops.** Also, splitting `execute`'s
  match (§3).
- **An `EdictId` newtype across the VM, server, client and saves.** Big churn. R5's
  `Option` sentinels and typed flags buy most of the clarity.
- **Fixing all 2380 pedantic cast findings.** They are id's C arithmetic; allow them
  project-wide (§5), and use `c_ftoi` by name where x86 overflow semantics matter.
- **`doc_markdown`'s 569 un-backticked C names.** Cosmetic.

---

## 9. How the numbers were made

All runs used scratch copies or `CARGO_TARGET_DIR` outside the worktree
(a scratch directory beside it), during one sitting.
- **Lints:** `cargo clippy --release --all-targets --message-format=json -- -W clippy::pedantic -W clippy::nursery`
  (and `--lib --bins` with selected `restriction` lints), aggregated by lint and file.
  `CLIPPY_CONF_DIR` set `too-many-lines-threshold = 150`.
- **Function lengths:** a brace-matching scan, counting physical lines.
- **Edition:** `cargo fix --edition --all-targets`, then `edition = "2024"`, tests, and
  the goldens (`quaketool scene … maps/e1m{1,2,3}.bsp`).
- **Toolchain:** `cargo +1.98.1 clippy/test/build`, the goldens, and
  `quaketool play <pak> demo1,walk_e1m1,fire_e1m1 --res 320x200 --hash-every 60`. That
  ran natively and as `wasm32-wasip1` builds under `node` (v25) `node:wasi`, for both
  1.93.1 and 1.98.1.
- **rustfmt:** `cargo fmt` under candidate `rustfmt.toml`s; `git diff --shortstat`; tests,
  goldens, idempotence.
- **The lint table:** added to a scratch `Cargo.toml`; `cargo clippy --fix` one lint at a
  time; tests, goldens and `play` hashes after.

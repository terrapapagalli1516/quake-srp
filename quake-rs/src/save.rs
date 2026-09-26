//! Single-player savegame serialization and loading.
//!
//! Ported from Quake (GPLv2). Copyright (C) 1996-1997 Id Software, Inc.
//! Sources:
//! * `WinQuake/host_cmd.c` — `Host_Savegame_f` (the `.sav` text layout:
//!   version 5 header, the 39-char comment, 16 spawn parms, `current_skill`,
//!   `sv.name`, `sv.time`, the 64 lightstyle lines, then the brace blocks),
//!   `Host_SavegameComment`, and `Host_Loadgame_f` (the load sequence:
//!   header → `SV_SpawnServer` → lightstyles → globals block → one block per
//!   edict, `sv.num_edicts`/`sv.time`/`spawn_parms` restored from the file).
//! * `WinQuake/pr_edict.c` — `ED_Write`, `ED_WriteGlobals`,
//!   `PR_UglyValueString` (how each value type serializes: entity as index,
//!   function/field by NAME), `ED_ParseGlobals`, `ED_ParseEdict`,
//!   `ED_ParseEpair`.
//!
//! ## What the C does — and does not — re-run on load (the load contract)
//!
//! `Host_Loadgame_f` calls `SV_SpawnServer(mapname)`, which **runs the map's
//! entity lump through `ED_LoadFromFile` normally** — every spawn function
//! executes, repopulating the model/sound precache tables (deterministically,
//! in map-entity order, so the saved `modelindex` values stay valid) and the
//! baseline lightstyles, and the two 0.1s settle frames run. THEN the `.sav`
//! text **overwrites** all of it: the 64 lightstyles, the `DEF_SAVEGLOBAL`
//! globals, and every edict (each block `memset`s the edict's fields first;
//! an empty `{}` block round-trips a free slot). `sv.num_edicts` and
//! `sv.time` come from the file. The client's entrance script is NOT re-run:
//! `Host_Spawn_f` sees `sv.loadgame` and skips `SetNewParms`/`ClientConnect`/
//! `PutClientInServer` ("loaded games are fully inited allready") — so the
//! net effect is exactly "world state from the save, precaches from the
//! deterministic respawn". No signon settle frames run after the overwrite.
//! The RNG state is NOT saved (the C does not persist `rand()`); post-load
//! divergence from an uninterrupted run is expected.
//!
//! ## Documented deviations (degrade, never abort)
//!
//! * The C `Sys_Error`s/`Host_Error`s on a malformed `.sav` ("First token
//!   isn't a brace", "EOF without closing brace", a bad epair). This port
//!   returns `Err(QError)` from [`Server::load_savegame`] instead — the
//!   front-end keeps the running game and prints the message. Recoverable
//!   per-pair problems (an unknown global/field name, a missing function)
//!   degrade exactly like the C's `Con_Printf` cases: a console warning
//!   (printed to the VM's output log, [`Vm::print`]) and the pair is skipped.
//! * Strings containing a literal `"` cannot round-trip (the C's
//!   `PR_UglyValueString` writes them raw inside quotes and `COM_Parse`
//!   truncates at the quote — same lossy behaviour as WinQuake).

use std::rc::Rc;

use crate::bsp::Bsp;
use crate::error::{QError, Result};
use crate::progs::{EType, Progs};
use crate::qrand::QRand;
use crate::server::{
    ed_new_string, link_edict, parse_float, parse_int, parse_vector, Server, Tokenizer,
    MAX_LIGHTSTYLES, NUM_SPAWN_PARMS,
};
use crate::vm::{Vm, MAX_EDICTS};

/// `SAVEGAME_VERSION` (host_cmd.c): the only `.sav` version written/accepted.
pub const SAVEGAME_VERSION: i32 = 5;

/// `SAVEGAME_COMMENT_LENGTH` (quakedef.h): the fixed comment width.
pub const SAVEGAME_COMMENT_LENGTH: usize = 39;

/// C `printf("%f", v)`: the fixed 6-decimal float format every number in a
/// `.sav` uses (spawn parms, sv.time, and all `ev_float`/`ev_vector` values).
/// Rust's `{:.6}` is the same correctly-rounded decimal of the exact value
/// (the C promotes the f32 to double losslessly first). 6 decimals uniquely
/// identify any f32 with |x| < 2^24, so gameplay values round-trip exactly.
fn fmt_f(v: f32) -> String {
    format!("{v:.6}")
}

// ---------------------------------------------------------------------------
// Writing (Host_Savegame_f / ED_WriteGlobals / ED_Write / PR_UglyValueString)
// ---------------------------------------------------------------------------

/// `Host_SavegameComment` (host_cmd.c): a fixed [`SAVEGAME_COMMENT_LENGTH`]
/// description — the level name (cl.levelname = worldspawn's `message`) over a
/// space-padded field, `kills:%3i/%3i` at column 22, then every space turned
/// into `_` "to make stdio happy" (the C loads it back with `fscanf("%s")`,
/// which stops at whitespace).
pub fn savegame_comment(levelname: &str, kills: i32, total_kills: i32) -> String {
    let mut text = [b' '; SAVEGAME_COMMENT_LENGTH];
    // memcpy(text, cl.levelname, strlen(cl.levelname)) — the C trusts the
    // level name to fit; we clamp at the comment width to stay in-bounds.
    for (slot, b) in text.iter_mut().zip(levelname.bytes()) {
        *slot = b;
    }
    // sprintf(kills, "kills:%3i/%3i", ...); memcpy(text+22, kills, ...).
    let kills_s = format!("kills:{kills:3}/{total_kills:3}");
    for (i, b) in kills_s.bytes().enumerate() {
        if let Some(slot) = text.get_mut(22 + i) {
            *slot = b;
        }
    }
    // convert space to _ to make stdio happy
    for b in text.iter_mut() {
        if *b == b' ' {
            *b = b'_';
        }
    }
    String::from_utf8_lossy(&text).into_owned()
}

/// `PR_UglyValueString` (pr_edict.c): serialize one value "easier to parse
/// than PR_ValueString". `c` holds the raw cells (1 for scalars, 3 for a
/// vector). Entity values are the edict index; functions and fields are
/// written by NAME (resolved through the progs tables) so a load can re-bind
/// them; floats/vectors use the C's `%f`.
fn ugly_value_string(vm: &Vm, etype: EType, c: [u32; 3]) -> String {
    match etype {
        EType::String => vm.get_string(c[0] as i32),
        EType::Entity => format!("{}", c[0] as i32),
        EType::Function => vm
            .progs()
            .functions
            .get(c[0] as usize)
            .map(|f| vm.progs().string(f.s_name).to_string())
            // The C indexes pr_functions unchecked (UB on a bad value); an
            // out-of-range function serializes as "" here (parse skips it).
            .unwrap_or_default(),
        EType::Field => {
            // ED_FieldAtOfs: the def whose ofs equals the stored value.
            let ofs = c[0] as i32;
            vm.progs()
                .fielddefs
                .iter()
                .find(|d| d.ofs as i32 == ofs)
                .map(|d| vm.progs().string(d.s_name).to_string())
                .unwrap_or_default()
        }
        EType::Void => "void".to_string(),
        EType::Float => fmt_f(f32::from_bits(c[0])),
        EType::Vector => format!(
            "{} {} {}",
            fmt_f(f32::from_bits(c[0])),
            fmt_f(f32::from_bits(c[1])),
            fmt_f(f32::from_bits(c[2]))
        ),
        // The C's default arm: pointers (and anything else) print "bad type N".
        EType::Pointer => "bad type 7".to_string(),
    }
}

/// `ED_WriteGlobals` (pr_edict.c): one brace block with every global def whose
/// type carries `DEF_SAVEGLOBAL` and whose base type is string/float/entity.
/// (No zero-skip here — that is an edict-field rule only.)
fn write_globals(vm: &Vm, out: &mut String) {
    out.push_str("{\n");
    for def in &vm.progs().globaldefs {
        if !def.save_global() {
            continue;
        }
        let etype = def.etype();
        if !matches!(etype, EType::String | EType::Float | EType::Entity) {
            continue;
        }
        let name = vm.progs().string(def.s_name);
        let ofs = def.ofs as usize;
        let cell = vm.gi(ofs) as u32;
        out.push_str(&format!(
            "\"{}\" \"{}\"\n",
            name,
            ugly_value_string(vm, etype, [cell, 0, 0])
        ));
    }
    out.push_str("}\n");
}

/// `ED_Write` (pr_edict.c): one brace block per edict. A free slot is an empty
/// `{}` block (so slot indices round-trip). Field defs are walked from index 1;
/// the `_x`/`_y`/`_z` vector-component defs (second-to-last char `_`) are
/// skipped, and a field whose raw cells are all zero is omitted (the load
/// memsets first, so zero is the default).
fn write_edict(vm: &Vm, e: i32, out: &mut String) {
    out.push_str("{\n");
    if vm.is_free_edict(e) {
        out.push_str("}\n");
        return;
    }
    for i in 1..vm.progs().fielddefs.len() {
        let d = vm.progs().fielddefs[i];
        let name = vm.progs().string(d.s_name);
        let nb = name.as_bytes();
        // if (name[strlen(name)-2] == '_') continue; // skip _x, _y, _z vars
        // (guarded for len<2 — the C would read out of bounds there).
        if nb.len() >= 2 && nb[nb.len() - 2] == b'_' {
            continue;
        }
        let etype = d.etype();
        let ofs = d.ofs as usize;
        // if the value is still all 0, skip the field (raw cell compare).
        let mut cells = [0u32; 3];
        let mut all_zero = true;
        for (j, slot) in cells.iter_mut().enumerate().take(etype.cells()) {
            *slot = vm.ei(e, ofs + j) as u32;
            if *slot != 0 {
                all_zero = false;
            }
        }
        if all_zero {
            continue;
        }
        let name = name.to_string(); // end the progs borrow before formatting
        out.push_str(&format!(
            "\"{}\" \"{}\"\n",
            name,
            ugly_value_string(vm, etype, cells)
        ));
    }
    out.push_str("}\n");
}

// ---------------------------------------------------------------------------
// Parsing (the .sav header + ED_ParseGlobals / ED_ParseEdict / ED_ParseEpair)
// ---------------------------------------------------------------------------

/// A parsed `.sav` header (everything `Host_Loadgame_f` reads with `fscanf`
/// before the brace blocks), plus the byte offset where the blocks begin.
#[derive(Debug, Clone)]
pub struct SaveGame {
    /// The `SAVEGAME_VERSION` the file claims (checked by the loader).
    pub version: i32,
    /// The [`savegame_comment`] line, underscores intact (see
    /// [`comment_for_display`] for the menu form).
    pub comment: String,
    /// `svs.clients->spawn_parms` — the level-entry parms.
    pub spawn_parms: [f32; NUM_SPAWN_PARMS],
    /// `current_skill`, decoded with the C's `(int)(tfloat + 0.1)` ("this
    /// silliness is so we can load 1.06 save files, which have float skill").
    pub skill: i32,
    /// `sv.name` — the bare map name (`"e1m1"`).
    pub map_name: String,
    /// `sv.time` at save.
    pub time: f32,
    /// The 64 `sv.lightstyles` lines.
    pub lightstyles: Vec<String>,
    /// Byte offset into the original text where the brace blocks start.
    pub blocks_ofs: usize,
}

/// `M_ScanSaves` (menu.c): the comment shown in the Load/Save menus converts
/// the stdio-friendly underscores back to spaces.
pub fn comment_for_display(comment: &str) -> String {
    comment.replace('_', " ")
}

/// Parse the line-oriented `.sav` header (the `fscanf` half of
/// `Host_Loadgame_f`). Numeric lines use `atof`/`atoi` semantics (garbage
/// reads as 0, like the C); a missing line is a clean error. Does NOT check
/// the version — the loader does, so it can print the C's exact message.
pub fn parse_savegame(text: &str) -> Result<SaveGame> {
    let mut pos = 0usize;
    let mut next_line = |what: &'static str| -> Result<&str> {
        if pos >= text.len() {
            return Err(QError::invalid(format!("savegame truncated at {what}")));
        }
        let rest = &text[pos..];
        let (line, advance) = match rest.find('\n') {
            Some(i) => (&rest[..i], i + 1),
            None => (rest, rest.len()),
        };
        pos += advance;
        Ok(line.trim_end_matches('\r'))
    };

    // fscanf (f, "%i\n", &version);
    let version = parse_int(next_line("version")?.trim());
    // fscanf (f, "%s\n", str); — the comment (underscored, so one token).
    let comment = next_line("comment")?.trim().to_string();
    // 16 spawn parms, "%f\n" each.
    let mut spawn_parms = [0.0f32; NUM_SPAWN_PARMS];
    for (i, p) in spawn_parms.iter_mut().enumerate() {
        *p = parse_float(next_line("spawn parms")?.trim());
        let _ = i;
    }
    // fscanf (f, "%f\n", &tfloat); current_skill = (int)(tfloat + 0.1);
    let skill = (parse_float(next_line("skill")?.trim()) + 0.1) as i32;
    // fscanf (f, "%s\n", mapname);
    let map_name = next_line("map name")?.trim().to_string();
    if map_name.is_empty() {
        return Err(QError::invalid("savegame has no map name"));
    }
    // fscanf (f, "%f\n", &time);
    let time = parse_float(next_line("time")?.trim());
    // the 64 lightstyle lines.
    let mut lightstyles = Vec::with_capacity(MAX_LIGHTSTYLES);
    for _ in 0..MAX_LIGHTSTYLES {
        lightstyles.push(next_line("lightstyles")?.trim().to_string());
    }

    Ok(SaveGame {
        version,
        comment,
        spawn_parms,
        skill,
        map_name,
        time,
        lightstyles,
        blocks_ofs: pos,
    })
}

/// Which value table an epair writes into (`ED_ParseEpair`'s `void *base`):
/// the global block, or one edict's fields.
enum EpairBase {
    Globals,
    Edict(i32),
}

/// `ED_ParseEpair` (pr_edict.c): write one parsed value by def type. Returns
/// `false` exactly where the C does (a named field/function that does not
/// exist) — the callers degrade that to a console warning instead of the C's
/// `Host_Error` (documented deviation).
fn parse_epair(vm: &mut Vm, base: &EpairBase, ofs: usize, etype: EType, s: &str) -> bool {
    // One raw-cell writer for both bases.
    fn put(vm: &mut Vm, base: &EpairBase, ofs: usize, cell: u32) {
        match base {
            EpairBase::Globals => vm.set_gi(ofs, cell as i32),
            EpairBase::Edict(e) => vm.set_ei(*e, ofs, cell as i32),
        }
    }
    match etype {
        EType::String => {
            // *(string_t *)d = ED_NewString(s) - pr_strings;
            let interned = ed_new_string(s);
            let s_t = vm.intern(&interned);
            put(vm, base, ofs, s_t as u32);
        }
        EType::Float => put(vm, base, ofs, parse_float(s).to_bits()),
        EType::Vector => {
            let v = parse_vector(s);
            for (j, x) in v.iter().enumerate() {
                put(vm, base, ofs + j, x.to_bits());
            }
        }
        EType::Entity => {
            // EDICT_TO_PROG(EDICT_NUM(atoi(s))): entity values are indices here.
            put(vm, base, ofs, parse_int(s) as u32);
        }
        EType::Field => {
            let Some(target) = vm.progs().field_offset(s) else {
                vm.print(&format!("Can't find field {s}\n"));
                return false;
            };
            put(vm, base, ofs, target as u32);
        }
        EType::Function => {
            let Some(fnum) = vm.progs().find_function(s) else {
                vm.print(&format!("Can't find function {s}\n"));
                return false;
            };
            put(vm, base, ofs, fnum as u32);
        }
        // ev_void / ev_pointer: the C's default arm stores nothing.
        EType::Void | EType::Pointer => {}
    }
    true
}

/// `ED_ParseGlobals` (pr_edict.c): key/value pairs until `}` into the global
/// block. An unknown key warns (`"'%s' is not a global"`) and is skipped.
fn parse_globals_block(vm: &mut Vm, tok: &mut Tokenizer) -> Result<()> {
    loop {
        // parse key
        let key = match tok.next_token() {
            Some(t) => t,
            None => return Err(QError::invalid("ED_ParseEntity: EOF without closing brace")),
        };
        if key.starts_with('}') {
            break;
        }
        // parse value
        let value = match tok.next_token() {
            Some(t) => t,
            None => return Err(QError::invalid("ED_ParseEntity: EOF without closing brace")),
        };
        if value.starts_with('}') {
            return Err(QError::invalid("ED_ParseEntity: closing brace without data"));
        }
        // ED_FindGlobal; copy the def out so the progs borrow ends.
        let Some((ofs, etype)) = vm.progs().find_global(&key).map(|d| (d.ofs as usize, d.etype()))
        else {
            vm.print(&format!("'{key}' is not a global\n"));
            continue;
        };
        // The C Host_Errors on a false return; we degrade (warning already
        // appended by parse_epair) and continue with the next pair.
        let _ = parse_epair(vm, &EpairBase::Globals, ofs, etype, &value);
    }
    Ok(())
}

/// `ED_ParseEdict` (pr_edict.c), the savegame path: key/value pairs until `}`
/// into edict `ent`'s fields, applying the same `angle`→`angles` /
/// `light`→`light_lev` / trailing-space / leading-`_` key hacks as the map
/// loader (the C shares one function). Returns `init` — whether any pair was
/// present; an empty block leaves the edict free (`if (!init) ent->free =
/// true`). Unknown keys warn (`"'%s' is not a field"`) and are skipped.
fn parse_edict_block(vm: &mut Vm, tok: &mut Tokenizer, ent: i32) -> Result<bool> {
    let mut init = false;
    loop {
        let key = match tok.next_token() {
            Some(t) => t,
            None => return Err(QError::invalid("ED_ParseEntity: EOF without closing brace")),
        };
        if key.starts_with('}') {
            break;
        }

        // anglehack: "angle" -> "angles", value rewritten "0 <v> 0" below.
        let anglehack = key == "angle";
        let mut keyname = if anglehack {
            "angles".to_string()
        } else if key == "light" {
            "light_lev".to_string() // hack for single light def
        } else {
            key
        };
        while keyname.ends_with(' ') {
            keyname.pop();
        }

        let value = match tok.next_token() {
            Some(t) => t,
            None => return Err(QError::invalid("ED_ParseEntity: EOF without closing brace")),
        };
        if value.starts_with('}') {
            return Err(QError::invalid("ED_ParseEntity: closing brace without data"));
        }
        init = true;

        // keynames with a leading underscore are utility comments; discarded.
        if keyname.starts_with('_') {
            continue;
        }

        let Some((ofs, etype)) = vm
            .progs()
            .find_field(&keyname)
            .map(|d| (d.ofs as usize, d.etype()))
        else {
            vm.print(&format!("'{keyname}' is not a field\n"));
            continue;
        };

        let value = if anglehack {
            format!("0 {value} 0")
        } else {
            value
        };
        let _ = parse_epair(vm, &EpairBase::Edict(ent), ofs, etype, &value);
    }
    Ok(init)
}

impl Server {
    /// `Host_Savegame_f`'s file body (host_cmd.c): the complete `.sav` text —
    /// version 5, the comment, the client's level-entry spawn parms,
    /// `current_skill`, `sv.name`, `sv.time`, 64 lightstyle lines (`"m"` for
    /// an unset style), the globals block, then one block per edict slot
    /// (free slots included, as empty `{}` blocks). The host decides where the
    /// text goes (a file for the CLI, localStorage for the browser); the
    /// engine API is text-out, exactly like the C is stdio-out.
    pub fn write_savegame(&self) -> String {
        let mut out = String::new();
        // fprintf (f, "%i\n", SAVEGAME_VERSION);
        out.push_str(&format!("{SAVEGAME_VERSION}\n"));
        // Host_SavegameComment: cl.levelname is the worldspawn `message`
        // (SV_SendServerinfo sends sv.edicts->v.message); the kill stats are
        // the killed_monsters/total_monsters progs globals (the client stats
        // mirror them via svc_updatestat).
        let levelname = self.vm.ent_str(0, self.vm.fo().message).to_string();
        let kills = self.vm.glob_float(self.vm.go().killed_monsters) as i32;
        let total = self.vm.glob_float(self.vm.go().total_monsters) as i32;
        out.push_str(&savegame_comment(&levelname, kills, total));
        out.push('\n');
        // for (...) fprintf (f, "%f\n", svs.clients->spawn_parms[i]);
        for p in self.client_spawn_parms {
            out.push_str(&fmt_f(p));
            out.push('\n');
        }
        // fprintf (f, "%d\n", current_skill);
        out.push_str(&format!("{}\n", self.skill()));
        // fprintf (f, "%s\n", sv.name);
        out.push_str(&self.map_name);
        out.push('\n');
        // fprintf (f, "%f\n", sv.time); -- sv.time is a double
        out.push_str(&format!("{:.6}", self.sv_time()));
        out.push('\n');
        // the 64 light styles ("m" when the C's sv.lightstyles[i] is NULL).
        for i in 0..MAX_LIGHTSTYLES {
            let s = self.lightstyle(i);
            out.push_str(if s.is_empty() { "m" } else { s });
            out.push('\n');
        }
        // ED_WriteGlobals, then ED_Write for every edict slot.
        write_globals(&self.vm, &mut out);
        for e in 0..self.vm.num_edicts() {
            write_edict(&self.vm, e as i32, &mut out);
        }
        out
    }

    /// `Host_Loadgame_f` (host_cmd.c): reconstruct a running server from a
    /// `.sav` text. The caller has already resolved the map named in the
    /// header (via [`parse_savegame`]) and hands in its parsed BSP + a fresh
    /// progs; this then follows the C's exact sequence:
    ///
    /// 1. version check (`"Savegame is version %i, not %i"`);
    /// 2. `SV_SpawnServer`: a fresh server on the map — skill from the save,
    ///    `set_map_name`, and a NORMAL `spawn_entities` (the map's spawn
    ///    functions run, rebuilding the precache tables deterministically and
    ///    running the two settle frames — see the module doc);
    /// 3. the 64 lightstyles from the save (overwriting the spawn's);
    /// 4. the brace blocks: globals first, then one block per edict —
    ///    each edict's fields are zeroed, parsed, and the live ones relinked
    ///    (`SV_LinkEdict(ent, false)`); empty blocks stay free;
    ///    `sv.num_edicts` is the block count;
    /// 5. `sv.time` and the client `spawn_parms` from the header. The
    ///    entrance script is NOT re-run (`sv.loadgame` in `Host_Spawn_f`).
    ///
    /// PORT NOTE: the C's player is always edict 1 (`svs.clients[0].edict`);
    /// this port reserves the player edict dynamically, so the loaded player
    /// is re-identified by its `classname "player"` (set by
    /// `PutClientInServer` before the save). A save without one is rejected.
    /// Any error leaves the caller's current game untouched (this builds a
    /// whole new `Server`). The new server draws from the host session's
    /// `rand` streams, as every server the session runs does
    /// ([`Server::set_rand`]).
    pub fn load_savegame(
        bsp: Bsp,
        progs: Progs,
        pak: Option<crate::pak::Pak>,
        rand: &Rc<QRand>,
        text: &str,
    ) -> Result<Server> {
        let sg = parse_savegame(text)?;
        if sg.version != SAVEGAME_VERSION {
            return Err(QError::invalid(format!(
                "Savegame is version {}, not {}",
                sg.version, SAVEGAME_VERSION
            )));
        }

        // Everything below builds a new server, with its own cvars, light
        // styles and outbox: a failure drops it and leaves the caller's game
        // as it was.
        let mut server = Server::with_pak(bsp, progs, pak)?;
        server.set_rand(Rc::clone(rand));
        Self::load_savegame_body(server, text, &sg)
    }

    /// The rest of [`Server::load_savegame`] on the fresh `server`: the C
    /// sequence, steps 2–5, plus the player re-identification.
    fn load_savegame_body(mut server: Server, text: &str, sg: &SaveGame) -> Result<Server> {
        // SV_SpawnServer (see the module doc: the map spawn functions DO run;
        // the save text then overwrites the world state they produced).
        // Cvar_SetValue ("skill", (float)current_skill) — before the spawn,
        // so the skill-flag entity filter matches the save's world exactly.
        server.set_skill(sg.skill as f32);
        server.set_map_name(&sg.map_name);
        server.spawn_entities()?;

        // load the light styles (all 64 lines overwrite sv.lightstyles).
        for (slot, s) in server.lightstyles.iter_mut().zip(&sg.lightstyles) {
            slot.clone_from(s);
        }

        // load the edicts out of the savegame file: entnum -1 is the globals.
        let mut tok = Tokenizer::new(&text[sg.blocks_ofs..]);
        let mut entnum: i64 = -1;
        loop {
            let Some(open) = tok.next_token() else {
                break; // end of file
            };
            if open != "{" {
                return Err(QError::invalid("First token isn't a brace"));
            }
            if entnum == -1 {
                // parse the global vars
                parse_globals_block(&mut server.vm, &mut tok)?;
            } else {
                // parse an edict: EDICT_NUM(entnum) — grow to the slot, but
                // never past the C's MAX_EDICTS array bound (Sys_Error there).
                let e = entnum as usize;
                if e >= MAX_EDICTS {
                    return Err(QError::invalid(format!(
                        "savegame has too many edicts (EDICT_NUM: bad number {e})"
                    )));
                }
                // memset (&ent->v, 0, progs->entityfields * 4); ent->free = false;
                server.vm.load_edict(e);

                let init = parse_edict_block(&mut server.vm, &mut tok, e as i32)?;
                if !init {
                    // ED_ParseEdict: a pairless block leaves the slot free.
                    server.vm.mark_edict_free(e);
                } else if e != 0 {
                    // link it into the bsp tree (SV_LinkEdict early-returns
                    // for the world edict, so skip slot 0 like the C).
                    link_edict(&mut server.vm, e as i32);
                }
            }
            entnum += 1;
        }

        // sv.num_edicts = entnum: the file's slot count is authoritative.
        // Slots the fresh spawn created ABOVE it are dropped (the C just
        // stops iterating them; our storage truncates to the same effect).
        if entnum < 1 {
            return Err(QError::invalid("savegame contains no edicts"));
        }
        let n = entnum as usize;
        server.vm.truncate_edicts(n);

        // sv.time = time; (`float time`, read by fscanf "%f": the double
        // clock restarts from that float. The globals block may have set the
        // `time` global already; the port sets it to the header's value, the
        // float every `pr_global_struct->time = sv.time` would store.)
        server.set_sv_time(f64::from(sg.time));

        // svs.clients->spawn_parms[i] = spawn_parms[i];
        server.client_spawn_parms = sg.spawn_parms;

        // Re-identify the player edict (see the PORT NOTE above). The C's
        // CL_EstablishConnection/Host_Reconnect_f path runs Host_Spawn_f with
        // sv.loadgame set: no entrance script, no signon settle frames.
        server.player = -1;
        for e in 1..server.vm.num_edicts() {
            let ent = e as i32;
            if !server.vm.is_free_edict(ent)
                && server.vm.ent_str(ent, server.vm.fo().classname) == "player"
            {
                server.player = ent;
                break;
            }
        }
        if server.player < 0 {
            return Err(QError::invalid("savegame has no player edict"));
        }

        Ok(server)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progs::{Def, Function, MAX_PARMS, PROG_VERSION, DEF_SAVEGLOBAL};

    // ---- synthetic progs.dat builder (mirrors vm.rs's test serializer) ----

    const HEADER_SIZE: usize = 60;

    // ev_* type codes (etype_t ordinals).
    const EV_STRING: u16 = 1;
    const EV_FLOAT: u16 = 2;
    const EV_VECTOR: u16 = 3;
    const EV_ENTITY: u16 = 4;
    const EV_FIELD: u16 = 5;
    const EV_FUNCTION: u16 = 6;

    fn ser_def(d: &Def) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&d.type_.to_le_bytes());
        v.extend_from_slice(&d.ofs.to_le_bytes());
        v.extend_from_slice(&d.s_name.to_le_bytes());
        v
    }
    fn ser_func(f: &Function) -> Vec<u8> {
        let mut v = Vec::new();
        for x in [
            f.first_statement,
            f.parm_start,
            f.locals,
            f.profile,
            f.s_name,
            f.s_file,
            f.numparms,
        ] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.extend_from_slice(&f.parm_size);
        v
    }

    struct Builder {
        strings: Vec<u8>,
        globaldefs: Vec<Def>,
        fielddefs: Vec<Def>,
        functions: Vec<Function>,
        nglobals: usize,
        entityfields: i32,
    }

    impl Builder {
        fn new() -> Builder {
            Builder {
                strings: vec![0u8],
                globaldefs: Vec::new(),
                // Real progs.dat carries a null fielddef at index 0 (qcc emits
                // it); ED_Write's loop starts at 1 to skip it, so the builder
                // must mirror it or the first real field would be skipped.
                fielddefs: vec![Def { type_: 0, ofs: 0, s_name: 0 }],
                functions: vec![Function {
                    first_statement: 0,
                    parm_start: 0,
                    locals: 0,
                    profile: 0,
                    s_name: 0,
                    s_file: 0,
                    numparms: 0,
                    parm_size: [0; MAX_PARMS],
                }],
                nglobals: 128,
                entityfields: 16,
            }
        }
        fn intern(&mut self, s: &str) -> i32 {
            let ofs = self.strings.len() as i32;
            self.strings.extend_from_slice(s.as_bytes());
            self.strings.push(0);
            ofs
        }
        fn add_global(&mut self, name: &str, type_: u16, ofs: u16) {
            let s = self.intern(name);
            self.globaldefs.push(Def { type_, ofs, s_name: s });
        }
        fn add_field(&mut self, name: &str, type_: u16, ofs: u16) {
            let s = self.intern(name);
            self.fielddefs.push(Def { type_, ofs, s_name: s });
        }
        /// A named builtin function record so `ev_function` values can
        /// round-trip by name (the statements are never executed here).
        fn add_named_function(&mut self, name: &str) -> usize {
            let s_name = self.intern(name);
            self.functions.push(Function {
                first_statement: -99, // builtin: never executed by these tests
                parm_start: 0,
                locals: 0,
                profile: 0,
                s_name,
                s_file: 0,
                numparms: 0,
                parm_size: [0; MAX_PARMS],
            });
            self.functions.len() - 1
        }
        fn build(&self) -> Vec<u8> {
            let globals: Vec<u32> = vec![0u32; self.nglobals];
            let mut body = Vec::new();
            let ofs_statements = HEADER_SIZE + body.len();
            let ofs_globaldefs = HEADER_SIZE + body.len();
            for d in &self.globaldefs {
                body.extend_from_slice(&ser_def(d));
            }
            let ofs_fielddefs = HEADER_SIZE + body.len();
            for d in &self.fielddefs {
                body.extend_from_slice(&ser_def(d));
            }
            let ofs_functions = HEADER_SIZE + body.len();
            for f in &self.functions {
                body.extend_from_slice(&ser_func(f));
            }
            let ofs_strings = HEADER_SIZE + body.len();
            body.extend_from_slice(&self.strings);
            let ofs_globals = HEADER_SIZE + body.len();
            for g in &globals {
                body.extend_from_slice(&g.to_le_bytes());
            }
            let header: [i32; 15] = [
                PROG_VERSION,
                0,
                ofs_statements as i32,
                0,
                ofs_globaldefs as i32,
                self.globaldefs.len() as i32,
                ofs_fielddefs as i32,
                self.fielddefs.len() as i32,
                ofs_functions as i32,
                self.functions.len() as i32,
                ofs_strings as i32,
                self.strings.len() as i32,
                ofs_globals as i32,
                globals.len() as i32,
                self.entityfields,
            ];
            let mut out = Vec::new();
            for x in header {
                out.extend_from_slice(&x.to_le_bytes());
            }
            out.extend_from_slice(&body);
            out
        }
    }

    /// An empty BSP (no geometry); world queries are total.
    fn empty_bsp() -> Bsp {
        Bsp {
            version: crate::bsp::BSPVERSION,
            entities: String::new(),
            planes: Vec::new(),
            vertexes: Vec::new(),
            edges: Vec::new(),
            faces: Vec::new(),
            nodes: Vec::new(),
            leafs: Vec::new(),
            clipnodes: Vec::new(),
            texinfo: Vec::new(),
            models: Vec::new(),
            marksurfaces: Vec::new(),
            surfedges: Vec::new(),
            textures: Vec::new(),
            visibility: Vec::new(),
            lighting: Vec::new(),
        }
    }

    /// A progs covering every savable def type: string/float/vector/entity/
    /// field/function fields (+ `_x/_y/_z` companions for the vector, like a
    /// real compiler emits), a SAVEGLOBAL float/string/entity trio, a
    /// non-saved global, and the well-known system globals.
    fn rich_progs() -> Progs {
        let mut b = Builder::new();
        // System globals the server touches.
        b.add_global("self", EV_ENTITY, 28);
        b.add_global("other", EV_ENTITY, 29);
        b.add_global("time", EV_FLOAT | DEF_SAVEGLOBAL, 30);
        b.add_global("world", EV_ENTITY, 31);
        b.add_global("frametime", EV_FLOAT, 32);
        b.add_global("mapname", EV_STRING, 33);
        // Saved globals (DEF_SAVEGLOBAL + string/float/entity).
        b.add_global("serverflags", EV_FLOAT | DEF_SAVEGLOBAL, 40);
        b.add_global("gamename", EV_STRING | DEF_SAVEGLOBAL, 41);
        b.add_global("lastboss", EV_ENTITY | DEF_SAVEGLOBAL, 42);
        // A saved VECTOR global must be skipped by ED_WriteGlobals.
        b.add_global("savedvec", EV_VECTOR | DEF_SAVEGLOBAL, 43);
        // A plain (non-SAVEGLOBAL) float global must be skipped too.
        b.add_global("scratch", EV_FLOAT, 46);

        // Fields. classname/origin like the real thing; origin gets the
        // compiler's _x/_y/_z component defs that ED_Write must skip.
        b.add_field("classname", EV_STRING, 1);
        b.add_field("origin", EV_VECTOR, 2);
        b.add_field("origin_x", EV_FLOAT, 2);
        b.add_field("origin_y", EV_FLOAT, 3);
        b.add_field("origin_z", EV_FLOAT, 4);
        b.add_field("health", EV_FLOAT, 5);
        b.add_field("enemy", EV_ENTITY, 6);
        b.add_field("think", EV_FUNCTION, 7);
        b.add_field("nextthink", EV_FLOAT, 8);
        b.add_field("message", EV_STRING, 9);
        b.add_field("dmg_inflictor", EV_FIELD, 10); // ev_field round-trip
        b.add_field("flags", EV_FLOAT, 11);

        b.add_named_function("monster_think");
        b.add_named_function("door_touch");

        Progs::parse(&b.build()).expect("synthetic progs parses")
    }

    fn server_with(progs: Progs) -> Server {
        Server::new(empty_bsp(), progs).expect("server builds")
    }

    #[test]
    fn comment_is_39_underscored_chars_with_kills_at_22() {
        let c = savegame_comment("the Slipgate Complex", 3, 26);
        assert_eq!(c.len(), SAVEGAME_COMMENT_LENGTH);
        assert!(!c.contains(' '), "all spaces become underscores: {c:?}");
        assert!(c.starts_with("the_Slipgate_Complex"));
        // kills:%3i/%3i at column 22 (spaces -> underscores).
        assert_eq!(&c[22..], "kills:__3/_26____");
        // The menu form converts back to spaces (M_ScanSaves).
        assert_eq!(comment_for_display(&c[..3]), "the");
        // An over-long level name is clamped, never panics (C would overflow).
        let long = savegame_comment(&"x".repeat(100), 1, 1);
        assert_eq!(long.len(), SAVEGAME_COMMENT_LENGTH);
    }

    #[test]
    fn globals_round_trip_through_save_text() {
        let mut s = server_with(rich_progs());
        s.set_map_name("e1m1");
        s.vm.gset_float("serverflags", 5.0);
        let g = s.vm.intern("a name with spaces");
        s.vm.gset_int("gamename", g);
        s.vm.gset_int("lastboss", 7);
        s.vm.gset_float("scratch", 99.0); // NOT saved
        s.vm.gset_float("time", 12.25);

        let text = s.write_savegame();
        // ED_WriteGlobals: SAVEGLOBAL string/float/entity only.
        assert!(text.contains("\"serverflags\" \"5.000000\""));
        assert!(text.contains("\"gamename\" \"a name with spaces\""));
        assert!(text.contains("\"lastboss\" \"7\""));
        assert!(!text.contains("scratch"), "non-SAVEGLOBAL global skipped");
        assert!(!text.contains("savedvec"), "vector global skipped");

        // Parse the globals back into a fresh VM via the block parser.
        let mut s2 = server_with(rich_progs());
        let sg = parse_savegame(&text).expect("header parses");
        let mut tok = Tokenizer::new(&text[sg.blocks_ofs..]);
        assert_eq!(tok.next_token().as_deref(), Some("{"));
        parse_globals_block(&mut s2.vm, &mut tok).expect("globals parse");
        assert_eq!(s2.vm.gget_float("serverflags"), 5.0);
        assert_eq!(
            s2.vm.get_string(s2.vm.gget_int("gamename")),
            "a name with spaces"
        );
        assert_eq!(s2.vm.gget_int("lastboss"), 7);
        assert_eq!(s2.vm.gget_float("scratch"), 0.0);
    }

    #[test]
    fn edicts_round_trip_strings_entities_functions_fields_and_free_slots() {
        let mut s = server_with(rich_progs());
        s.set_map_name("e1m2");

        // Edict 1: a monster with every value type in play.
        let e1 = s.vm.spawn();
        s.vm.ent_set_string(e1, "classname", "monster_army");
        s.vm.ent_set_vector(e1, "origin", [12.5, -7.0, 0.25]);
        s.vm.ent_set_float(e1, "health", 30.0);
        s.vm.ent_set_int(e1, "enemy", 3);
        let think = s.vm.progs().find_function("monster_think").unwrap() as i32;
        s.vm.ent_set_int(e1, "think", think);
        s.vm.ent_set_float(e1, "nextthink", 13.0625);
        s.vm.ent_set_string(e1, "message", "you got the\\nthing"); // raw backslash-n
        let fofs = s.vm.progs().field_offset("health").unwrap() as i32;
        s.vm.ent_set_int(e1, "dmg_inflictor", fofs); // ev_field by ofs

        // Edict 2: a slot that will be FREED (after e3 exists, so ED_Alloc's
        // reuse can't collapse the hole) -> an empty block that stays free.
        let e2 = s.vm.spawn();

        // Edict 3: the player (so the loader can re-identify it).
        let e3 = s.vm.spawn();
        s.vm.ent_set_string(e3, "classname", "player");
        s.vm.ent_set_float(e3, "health", 87.0);
        s.vm.ent_set_vector(e3, "origin", [100.0, 200.0, 24.0]);

        // Now punch the hole at slot 2.
        s.vm.free_edict(e2);

        let text = s.write_savegame();
        // Function serialized by NAME, field by NAME, entity by index.
        assert!(text.contains("\"think\" \"monster_think\""));
        assert!(text.contains("\"dmg_inflictor\" \"health\""));
        assert!(text.contains("\"enemy\" \"3\""));
        // _x/_y/_z companions are never written.
        assert!(!text.contains("origin_x"));
        // The freed slot is an empty block: "{\n}\n".
        assert!(text.contains("{\n}\n"), "free edict round-trips as {{}}");

        let s2 = Server::load_savegame(empty_bsp(), rich_progs(), None, &Rc::default(), &text)
            .expect("load_savegame");
        assert_eq!(s2.vm.num_edicts(), 4, "world + 3 slots, like the save");
        assert_eq!(s2.vm.ent_get_string(e1, "classname"), "monster_army");
        assert_eq!(s2.vm.ent_get_vector(e1, "origin"), [12.5, -7.0, 0.25]);
        assert_eq!(s2.vm.ent_get_float(e1, "health"), 30.0);
        assert_eq!(s2.vm.ent_get_int(e1, "enemy"), 3, "entity ref by index");
        let think2 = s2.vm.ent_get_int(e1, "think");
        assert_eq!(
            s2.vm.progs().string(s2.vm.progs().functions[think2 as usize].s_name),
            "monster_think",
            "function re-bound by name"
        );
        assert_eq!(s2.vm.ent_get_float(e1, "nextthink"), 13.0625);
        // ED_NewString on load turns the literal backslash-n into a newline
        // (C-faithful lossy round-trip; the save wrote the raw backslash).
        assert_eq!(s2.vm.ent_get_string(e1, "message"), "you got the\nthing");
        assert_eq!(
            s2.vm.ent_get_int(e1, "dmg_inflictor"),
            s2.vm.progs().field_offset("health").unwrap() as i32,
            "ev_field re-bound by name"
        );
        assert!(s2.vm.is_free_edict(e2), "the freed slot stays free");
        assert_eq!(s2.player_edict(), e3, "player re-identified by classname");
        assert_eq!(s2.vm.ent_get_float(e3, "health"), 87.0);
    }

    #[test]
    fn header_skill_time_parms_and_lightstyles_round_trip() {
        let mut s = server_with(rich_progs());
        s.set_map_name("maps/e1m3.bsp"); // qualified form normalises to bare
        s.set_skill(2.0);
        s.set_sv_time(33.5);
        s.client_spawn_parms[0] = 1.0;
        s.client_spawn_parms[3] = 25.0;
        s.lightstyles[0] = "m".into();
        s.lightstyles[5] = "jklmnopqrst".into();
        // A player so the loader accepts the save.
        let p = s.vm.spawn();
        s.vm.ent_set_string(p, "classname", "player");

        let text = s.write_savegame();
        let sg = parse_savegame(&text).expect("parses");
        assert_eq!(sg.version, SAVEGAME_VERSION);
        assert_eq!(sg.skill, 2);
        assert_eq!(sg.map_name, "e1m3");
        assert_eq!(sg.time, 33.5);
        assert_eq!(sg.spawn_parms[0], 1.0);
        assert_eq!(sg.spawn_parms[3], 25.0);
        assert_eq!(sg.lightstyles.len(), MAX_LIGHTSTYLES);
        assert_eq!(sg.lightstyles[5], "jklmnopqrst");
        assert_eq!(sg.lightstyles[1], "m", "unset style writes the C's \"m\"");

        let s2 = Server::load_savegame(empty_bsp(), rich_progs(), None, &Rc::default(), &text)
            .expect("loads");
        assert_eq!(s2.skill(), 2);
        assert_eq!(s2.time(), 33.5);
        assert_eq!(s2.client_spawn_parms[3], 25.0);
        assert_eq!(s2.lightstyle(5), "jklmnopqrst");
        assert_eq!(s2.lightstyle(63), "m");
    }

    #[test]
    fn hostile_input_errors_cleanly_never_panics() {
        // Truncated header.
        assert!(Server::load_savegame(empty_bsp(), rich_progs(), None, &Rc::default(), "5\n").is_err());
        // Wrong version: the C's message. (`.err()` not `.unwrap_err()`:
        // Server has no Debug impl.)
        let err = Server::load_savegame(empty_bsp(), rich_progs(), None, &Rc::default(), &"4\n".repeat(90))
            .err()
            .expect("wrong version is rejected");
        assert!(err.to_string().contains("Savegame is version 4, not 5"), "{err}");
        // Garbage where the blocks should be.
        let mut s = server_with(rich_progs());
        s.set_map_name("e1m1");
        let p = s.vm.spawn();
        s.vm.ent_set_string(p, "classname", "player");
        let good = s.write_savegame();
        let sg = parse_savegame(&good).unwrap();
        let garbage = format!("{}not-a-brace", &good[..sg.blocks_ofs]);
        let err = Server::load_savegame(empty_bsp(), rich_progs(), None, &Rc::default(), &garbage)
            .err()
            .expect("garbage blocks rejected");
        assert!(err.to_string().contains("First token isn't a brace"), "{err}");
        // A block cut off mid-pair: EOF without closing brace.
        let truncated = format!("{}{{\n\"classname\" ", &good[..sg.blocks_ofs]);
        let err = Server::load_savegame(empty_bsp(), rich_progs(), None, &Rc::default(), &truncated)
            .err()
            .expect("truncated block rejected");
        assert!(err.to_string().contains("EOF without closing brace"), "{err}");
        // No player edict in the blocks.
        let mut s2 = server_with(rich_progs());
        s2.set_map_name("e1m1");
        let no_player = s2.write_savegame();
        let err = Server::load_savegame(empty_bsp(), rich_progs(), None, &Rc::default(), &no_player)
            .err()
            .expect("player-less save rejected");
        assert!(err.to_string().contains("no player edict"), "{err}");
        // Empty text.
        assert!(Server::load_savegame(empty_bsp(), rich_progs(), None, &Rc::default(), "").is_err());
    }

    /// A REJECTED load must leave the caller's game alone: `load_savegame`
    /// sets the header's skill and styles on the server it builds before the
    /// blocks can fail to parse, and none of it may reach the surviving game
    /// (a review finding when the two shared thread-locals).
    #[test]
    fn failed_load_leaves_the_running_game_alone() {
        // A save whose header carries DIFFERENT styles (style 0 "zzz") and a
        // DIFFERENT skill (0) than the running game below, but whose blocks
        // are garbage so the load fails after the header is applied.
        let mut donor = server_with(rich_progs());
        donor.set_map_name("e1m2");
        donor.set_skill(0.0);
        donor.lightstyles[0] = "zzz".into();
        let dp = donor.vm.spawn();
        donor.vm.ent_set_string(dp, "classname", "player");
        let donor_text = donor.write_savegame();
        let donor_sg = parse_savegame(&donor_text).unwrap();
        let hostile = format!("{}not-a-brace", &donor_text[..donor_sg.blocks_ofs]);

        // The "running game": skill 2, an animated style 0.
        let mut running = server_with(rich_progs());
        running.set_map_name("e1m1");
        running.set_skill(2.0);
        running.lightstyles[0] = "abcdefg".into();

        let err = Server::load_savegame(empty_bsp(), rich_progs(), None, &Rc::default(), &hostile)
            .err()
            .expect("garbage blocks rejected");
        assert!(err.to_string().contains("First token isn't a brace"), "{err}");

        // The running game keeps its own state, through its next frame.
        assert_eq!(running.skill(), 2);
        running.run_frame(0.1).expect("frame");
        assert_eq!(running.lightstyle(0), "abcdefg");
    }

    #[test]
    fn unknown_globals_and_fields_warn_and_are_skipped() {
        let mut s = server_with(rich_progs());
        s.set_map_name("e1m1");
        let p = s.vm.spawn();
        s.vm.ent_set_string(p, "classname", "player");
        let good = s.write_savegame();
        let sg = parse_savegame(&good).unwrap();
        // Inject an unknown global and an unknown field into the blocks.
        let blocks = &good[sg.blocks_ofs..];
        let blocks = blocks.replacen('{', "{\n\"bogus_global\" \"1\"", 1);
        let blocks = blocks.replacen(
            "\"classname\" \"player\"",
            "\"bogus_field\" \"2\" \"classname\" \"player\"",
            1,
        );
        let doctored = format!("{}{}", &good[..sg.blocks_ofs], blocks);
        let s2 = Server::load_savegame(empty_bsp(), rich_progs(), None, &Rc::default(), &doctored)
            .expect("unknown names degrade, not abort");
        assert!(s2.vm.output().contains("'bogus_global' is not a global"));
        assert!(s2.vm.output().contains("'bogus_field' is not a field"));
        assert_eq!(s2.vm.ent_get_string(p, "classname"), "player");
    }

    #[test]
    fn missing_function_warns_and_pair_is_skipped() {
        let mut s = server_with(rich_progs());
        s.set_map_name("e1m1");
        let p = s.vm.spawn();
        s.vm.ent_set_string(p, "classname", "player");
        let think = s.vm.progs().find_function("monster_think").unwrap() as i32;
        s.vm.ent_set_int(p, "think", think);
        let good = s.write_savegame();
        let doctored = good.replace("monster_think", "no_such_function");
        let s2 = Server::load_savegame(empty_bsp(), rich_progs(), None, &Rc::default(), &doctored)
            .expect("missing function degrades (C Host_Errors)");
        assert!(s2.vm.output().contains("Can't find function no_such_function"));
        assert_eq!(s2.vm.ent_get_int(p, "think"), 0, "pair skipped, field stays 0");
    }

    #[test]
    fn save_text_shape_matches_host_savegame_f() {
        let mut s = server_with(rich_progs());
        s.set_map_name("e1m1");
        let p = s.vm.spawn();
        s.vm.ent_set_string(p, "classname", "player");
        let saved = 1_234.567_890_123_f64;
        s.set_sv_time(saved);
        let text = s.write_savegame();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "5", "version line");
        assert_eq!(lines[1].len(), SAVEGAME_COMMENT_LENGTH, "comment width");
        // 16 parm lines, all %f.
        for line in &lines[2..18] {
            assert!(line.contains('.'), "parm line is %f: {line:?}");
        }
        assert_eq!(lines[18], "1", "current_skill (default medium)");
        assert_eq!(lines[19], "e1m1", "sv.name");
        // fprintf("%f\n", sv.time) of the double (the float would print
        // 1234.567871).
        assert_eq!(lines[20], "1234.567890", "sv.time is %f of the double");
        // Host_Loadgame_f reads it with fscanf("%f") into `float time`, and
        // `sv.time = time`: the clock restarts from that float.
        let s2 = Server::load_savegame(empty_bsp(), rich_progs(), None, &Rc::default(), &text).expect("loads");
        assert_eq!(s2.sv_time(), f64::from(saved as f32));
        assert_eq!(s2.vm.gget_float("time"), saved as f32, "the QC global is its float");
        // 64 lightstyles then the globals block opener.
        assert_eq!(lines[21 + MAX_LIGHTSTYLES], "{", "globals block after styles");
    }
}

//! A map's entity lump read as plain text: what a command needs from a map —
//! where the player starts, where the exit leads, which classnames it holds —
//! without spawning it through QuakeC (`ED_LoadFromFile`). These are small
//! scanners over the `"key" "value"` pairs, not id's `ED_ParseEdict`.

use std::collections::HashMap;

/// Tally `"classname" "x"` values out of the raw entity lump text. A deliberately
/// tiny scanner (not the full QuakeC entity parser) — enough to summarize a map.
pub fn count_entity_classnames(ents: &str) -> HashMap<String, u32> {
    let mut out = HashMap::new();
    let mut prev_key: Option<String> = None;
    for (idx, tok) in ents.split('"').enumerate() {
        // Quoted strings sit at odd token indices.
        if idx % 2 == 1 {
            match prev_key.take() {
                Some(k) if k == "classname" => {
                    *out.entry(tok.to_string()).or_insert(0) += 1;
                }
                Some(_) => {}
                None => prev_key = Some(tok.to_string()),
            }
        }
    }
    out
}

/// Read the `map` key from the first `trigger_changelevel` block in an entity
/// lump (the bare destination map name, e.g. `"e1m2"`).
pub fn trigger_map_key(ents: &str) -> Option<String> {
    for block in ents.split('}') {
        let toks: Vec<&str> = block.split('"').collect();
        let mut classname = "";
        let mut map = None;
        let mut i = 1;
        while i + 2 < toks.len() {
            match toks[i] {
                "classname" => classname = toks[i + 2],
                "map" => map = Some(toks[i + 2].trim().to_string()),
                _ => {}
            }
            i += 4;
        }
        if classname == "trigger_changelevel" {
            return map;
        }
    }
    None
}

/// Find `info_player_start`'s origin and angle from the entity lump.
pub fn player_start(ents: &str) -> Option<([f32; 3], f32)> {
    for block in ents.split('}') {
        // collect "key" "value" pairs in this entity block
        let toks: Vec<&str> = block.split('"').collect();
        let mut classname = "";
        let mut origin = None;
        let mut angle = 0.0f32;
        let mut i = 1;
        while i + 2 < toks.len() {
            let key = toks[i];
            let val = toks[i + 2];
            match key {
                "classname" => classname = val,
                "origin" => {
                    let n: Vec<f32> = val.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                    if n.len() == 3 {
                        origin = Some([n[0], n[1], n[2]]);
                    }
                }
                "angle" => angle = val.trim().parse().unwrap_or(0.0),
                _ => {}
            }
            i += 4; // step over "key" <sep> "value" <sep>
        }
        if classname == "info_player_start" {
            if let Some(o) = origin {
                return Some((o, angle));
            }
        }
    }
    None
}

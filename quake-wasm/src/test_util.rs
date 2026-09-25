//! Shared fixtures for the crate's `#[cfg(test)]` modules: a synthetic pak
//! builder and the helpers that drive the live App the way the page does.

use quake_rs::pak::{Pak, DIRENTRY_SIZE, HEADER_SIZE, NAME_SIZE};

use crate::snd_dma::{set_audio_ready, SND_QUEUE};

/// Build a synthetic PACK image holding the given (name, contents) files, so
/// `queue_sounds` can resolve real bytes for hand-crafted sound names without
/// depending on the embedded pak's contents.
pub(crate) fn build_test_pak(files: &[(&str, &[u8])]) -> Pak {
    let mut contents = Vec::new();
    let mut positions = Vec::new();
    let mut cursor = HEADER_SIZE as i32;
    for (_, data) in files {
        positions.push((cursor, data.len() as i32));
        contents.extend_from_slice(data);
        cursor += data.len() as i32;
    }
    let dirofs = HEADER_SIZE + contents.len();
    let dirlen = files.len() * DIRENTRY_SIZE;

    let mut img = Vec::new();
    img.extend_from_slice(b"PACK");
    img.extend_from_slice(&(dirofs as i32).to_le_bytes());
    img.extend_from_slice(&(dirlen as i32).to_le_bytes());
    img.extend_from_slice(&contents);
    for (i, (name, _)) in files.iter().enumerate() {
        let mut name_field = [0u8; NAME_SIZE];
        let b = name.as_bytes();
        name_field[..b.len()].copy_from_slice(b);
        img.extend_from_slice(&name_field);
        img.extend_from_slice(&positions[i].0.to_le_bytes());
        img.extend_from_slice(&positions[i].1.to_le_bytes());
    }
    Pak::from_bytes("test".into(), img).expect("synthetic pak")
}

pub(crate) fn reset_queue() {
    SND_QUEUE.with(|q| q.borrow_mut().clear());
    set_audio_ready(1); // audio running so queue_sounds enqueues
}

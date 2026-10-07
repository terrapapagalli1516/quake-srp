//! Text in Quake's own lettering, for titles and labels: the console font
//! (`conchars` in `gfx.wad`, 8x8 cells — its white set, or its gold set
//! from character 128 on, as id's menus print their highlights) and the
//! status bar's big numerals (`num_*`, 24x24, and the red `anum_*`).
//! Every font pixel is drawn as a `scale x scale` square, never smoothed;
//! index 0 is transparent, as `Draw_Character` skips it.

use quake_rs::wad::{Qpic, Wad2};

/// Which lettering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Font {
    /// `conchars`' white characters.
    White,
    /// `conchars`' gold characters (the set from 128).
    Gold,
    /// The status bar's numerals (`num_0`..`num_9`, `num_minus`,
    /// `num_colon`, `num_slash`); other characters are spaces.
    Numbers,
    /// The red numerals (`anum_*`, the low-ammo and low-health digits).
    RedNumbers,
}

impl Font {
    pub fn parse(s: &str) -> Option<Font> {
        Some(match s {
            "white" | "conchars" => Font::White,
            "gold" => Font::Gold,
            "num" | "numbers" => Font::Numbers,
            "anum" | "red" => Font::RedNumbers,
            _ => return None,
        })
    }
}

/// The glyphs of a font: a picture per character.
pub struct Glyphs {
    font: Font,
    conchars: Option<Qpic>,
    nums: Vec<(char, Qpic)>,
}

impl Glyphs {
    pub fn new(wad: &Wad2, font: Font) -> Result<Glyphs, String> {
        let conchars = quake_rs::render::conchars_pic(wad);
        let prefix = if font == Font::RedNumbers { "anum_" } else { "num_" };
        let mut nums = Vec::new();
        if matches!(font, Font::Numbers | Font::RedNumbers) {
            for (c, name) in ('0'..='9').map(|c| (c, c.to_string())).chain([
                ('-', "minus".to_string()),
                (':', "colon".to_string()),
                ('/', "slash".to_string()),
            ]) {
                if let Ok(pic) = wad.qpic(&format!("{prefix}{name}")) {
                    nums.push((c, pic));
                }
            }
        }
        if conchars.is_none() && nums.is_empty() {
            return Err("gfx.wad has no font".into());
        }
        Ok(Glyphs { font, conchars, nums })
    }

    /// One character's cell: its width and height in font pixels.
    fn cell(&self) -> (usize, usize) {
        match self.font {
            Font::White | Font::Gold => (8, 8),
            _ => (24, 24),
        }
    }

    /// The size of `text` (lines split at `\n`) at `scale`.
    pub fn measure(&self, text: &str, scale: usize) -> (usize, usize) {
        let (cw, ch) = self.cell();
        let lines: Vec<&str> = text.split('\n').collect();
        let w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        (w * cw * scale, lines.len() * ch * scale)
    }

    /// The palette index of font pixel `(x, y)` of character `c`, or `None`
    /// where it is transparent.
    fn pixel(&self, c: char, x: usize, y: usize) -> Option<u8> {
        let v = match self.font {
            Font::White | Font::Gold => {
                let pic = self.conchars.as_ref()?;
                let mut code = if c.is_ascii() { c as usize } else { usize::from(b'?') };
                if self.font == Font::Gold && code < 128 && code != usize::from(b' ') {
                    code += 128;
                }
                let (row, col) = (code >> 4, code & 15);
                // Index 0 is conchars' transparency (`Draw_Character`).
                Some(*pic.data.get((row * 8 + y) * 128 + col * 8 + x)?).filter(|&v| v != 0)
            }
            _ => {
                let (_, pic) = self.nums.iter().find(|(k, _)| *k == c)?;
                if x >= pic.width as usize || y >= pic.height as usize {
                    return None;
                }
                // 255 is a picture's (`Draw_TransPic`).
                Some(pic.data[y * pic.width as usize + x]).filter(|&v| v != 255)
            }
        };
        v
    }

    /// Draw `text` with its top-left corner at `(x0, y0)` of an RGBA (or
    /// RGB) image `w` wide with `channels` bytes a pixel, at `scale`, in the
    /// palette's colours; with `shadow`, a black copy that many font pixels
    /// down and right first.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        img: &mut [u8],
        w: usize,
        channels: usize,
        palette: &[[u8; 3]; 256],
        text: &str,
        (x0, y0): (i64, i64),
        scale: usize,
        shadow: usize,
    ) {
        let h = img.len() / (w * channels).max(1);
        let (cw, ch) = self.cell();
        for pass in [true, false] {
            if pass && shadow == 0 {
                continue;
            }
            let off = if pass { (shadow * scale) as i64 } else { 0 };
            for (ln, line) in text.split('\n').enumerate() {
                for (k, c) in line.chars().enumerate() {
                    for fy in 0..ch {
                        for fx in 0..cw {
                            let Some(idx) = self.pixel(c, fx, fy) else { continue };
                            let rgb = if pass { [0, 0, 0] } else { palette[usize::from(idx)] };
                            let px = x0 + off + ((k * cw + fx) * scale) as i64;
                            let py = y0 + off + ((ln * ch + fy) * scale) as i64;
                            for sy in 0..scale as i64 {
                                for sx in 0..scale as i64 {
                                    let (x, y) = (px + sx, py + sy);
                                    if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
                                        continue;
                                    }
                                    let at = (y as usize * w + x as usize) * channels;
                                    img[at..at + 3].copy_from_slice(&rgb);
                                    if channels == 4 {
                                        img[at + 3] = 255;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

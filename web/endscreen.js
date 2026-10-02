// endscreen.js — id's end screen (sys_dos.c's `Sys_Quit`), shown once the
// game quits (web/PLATFORM.md, "Quit"). index.html loads this file
// unconditionally (any session can quit) and calls
// `QuakeEndScreen.show(registered, screen)` from the `Quit` record's
// payload: `screen` is that lump's 4000 raw bytes — 80x25 of (character,
// attribute) VGA text-mode pairs, straight off the wire — or `null` if the
// pak had neither `end1.bin` nor `end2.bin`.
//
// A page can't exit, so this is the browser's version of "give the desktop
// back": index.html has already left fullscreen and released the pointer
// and keyboard locks by the time this runs; this module just draws the
// screen DOS showed in its place, and restarts the page on any key, click
// or tap — the simplest robust way back in, since the worker's `main` has
// already returned and there is no live game left to resume
// (`quake-wasm/src/config.rs`: saves and `config.cfg` are already files the
// page persists in IndexedDB, so a reload loses nothing). With a mission
// pack on offer, index.html also hands over its game choices (its
// `gameList`, styled by its own CSS): shown under the screen, each a link
// into that game, whose presses stop at the link rather than restarting
// this one.
//
// An IIFE (like touch.js): index.html's own script shares this file's
// top-level scope (two classic `<script>`s in one document), so nothing
// here is a bare top-level name — only `window.QuakeEndScreen` is.
(() => {
'use strict';

// CP437 -> Unicode code points, index = byte value (Code page 437, the
// original IBM PC font: en.wikipedia.org/wiki/Code_page_437). No font file:
// every glyph the end screen uses — letters, the box-drawing lines, the
// handful of symbols — has a plain Unicode code point a system monospace
// font already carries. 0x00 maps to a plain space (its real glyph is
// blank) rather than U+0000, which a browser would rather not show at all.
const CP437 = [
  0x0020, 0x263a, 0x263b, 0x2665, 0x2666, 0x2663, 0x2660, 0x2022, 0x25d8, 0x25cb, 0x25d9, 0x2642, 0x2640, 0x266a, 0x266b, 0x263c,
  0x25ba, 0x25c4, 0x2195, 0x203c, 0x00b6, 0x00a7, 0x25ac, 0x21a8, 0x2191, 0x2193, 0x2192, 0x2190, 0x221f, 0x2194, 0x25b2, 0x25bc,
];
for (let c = 0x20; c <= 0x7e; c++) CP437.push(c); // 0x20-0x7e: plain ASCII
CP437.push(0x2302); // 0x7f: the house/delta glyph DOS put where ASCII has DEL
CP437.push(
  0x00c7, 0x00fc, 0x00e9, 0x00e2, 0x00e4, 0x00e0, 0x00e5, 0x00e7, 0x00ea, 0x00eb, 0x00e8, 0x00ef, 0x00ee, 0x00ec, 0x00c4, 0x00c5,
  0x00c9, 0x00e6, 0x00c6, 0x00f4, 0x00f6, 0x00f2, 0x00fb, 0x00f9, 0x00ff, 0x00d6, 0x00dc, 0x00a2, 0x00a3, 0x00a5, 0x20a7, 0x0192,
  0x00e1, 0x00ed, 0x00f3, 0x00fa, 0x00f1, 0x00d1, 0x00aa, 0x00ba, 0x00bf, 0x2310, 0x00ac, 0x00bd, 0x00bc, 0x00a1, 0x00ab, 0x00bb,
  0x2591, 0x2592, 0x2593, 0x2502, 0x2524, 0x2561, 0x2562, 0x2556, 0x2555, 0x2563, 0x2551, 0x2557, 0x255d, 0x255c, 0x255b, 0x2510,
  0x2514, 0x2534, 0x252c, 0x251c, 0x2500, 0x253c, 0x255e, 0x255f, 0x255a, 0x2554, 0x2569, 0x2566, 0x2560, 0x2550, 0x256c, 0x2567,
  0x2568, 0x2564, 0x2565, 0x2559, 0x2558, 0x2552, 0x2553, 0x256b, 0x256a, 0x2518, 0x250c, 0x2588, 0x2584, 0x258c, 0x2590, 0x2580,
  0x03b1, 0x00df, 0x0393, 0x03c0, 0x03a3, 0x03c3, 0x00b5, 0x03c4, 0x03a6, 0x0398, 0x03a9, 0x03b4, 0x221e, 0x03c6, 0x03b5, 0x2229,
  0x2261, 0x00b1, 0x2265, 0x2264, 0x2320, 0x2321, 0x00f7, 0x2248, 0x00b0, 0x2219, 0x00b7, 0x221a, 0x207f, 0x00b2, 0x25a0, 0x00a0,
);

// The 16-colour VGA DAC default palette (the BIOS text mode's, which is what
// `Sys_Quit`'s screen is drawn through): low nibble of the attribute byte is
// the foreground, bits 4-6 the background (3 bits: no bright backgrounds in
// this mode — bit 7 is blink, below).
const VGA16 = [
  '#000000', '#0000aa', '#00aa00', '#00aaaa', '#aa0000', '#aa00aa', '#aa5500', '#aaaaaa',
  '#555555', '#5555ff', '#55ff55', '#55ffff', '#ff5555', '#ff55ff', '#ffff55', '#ffffff',
];

const COLS = 80, ROWS = 25;

// One <style> element, made the first time a screen shows.
let styleInjected = false;
const CSS = `
#quakeEndScreen {
  position: fixed; inset: 0; z-index: 2147483647; box-sizing: border-box;
  padding: 2vh 2vw;
  background: #000; display: flex; flex-direction: column; align-items: center; justify-content: center;
  gap: 2vh; cursor: pointer; -webkit-user-select: none; user-select: none;
}
#quakeEndScreen .qesGrid {
  font-family: Consolas, 'Cascadia Mono', 'DejaVu Sans Mono', 'Courier New', monospace;
  font-size: min(1.9vw, 3.1vh);
  line-height: 1.22;
}
/* With the game choices under it: the 25 rows (1.22 lines each) share the
   height with them, at most 52px, the choices' own fingertip height. */
#quakeEndScreen.qesWithGames .qesGrid { font-size: min(1.9vw, calc((94vh - 52px) / 30.5)); }
#quakeEndScreen .qesGames { display: flex; align-items: center; gap: 12px; color: #aaa;
  font: 13px Consolas, 'DejaVu Sans Mono', monospace; }
#quakeEndScreen .qesRow { white-space: pre; }
/* An inline background covers only the glyphs' height, leaving a dark seam
   between rows; an inline-block one fills the whole line box, as a text-mode
   cell does. */
#quakeEndScreen .qesRow span { display: inline-block; vertical-align: top; }
#quakeEndScreen .qesBlink { animation: qesBlink 1s steps(1) infinite; }
@keyframes qesBlink { 50%, 100% { opacity: 0; } }
#quakeEndScreen .qesFallback {
  color: #aaa; font: 16px/1.5 Consolas, 'DejaVu Sans Mono', monospace; text-align: center; padding: 24px;
}
`;
function ensureStyle() {
  if (styleInjected) return;
  styleInjected = true;
  const style = document.createElement('style');
  style.textContent = CSS;
  document.head.appendChild(style);
}

// 80x25 of (character, attribute) pairs into one row's worth of <span>s, run
// length encoded by (fg, bg, blink) so a mostly-one-colour row (most of this
// screen) costs one element, not eighty.
// CP437's block elements, drawn as backgrounds rather than glyphs: a font's
// half block covers half its em box, not the taller line box, so a glyph
// leaves a strip of the cell's background showing (end1.bin's bottom edge,
// row 23, is 80 lower halves). Text mode filled exact half cells.
const BLOCKS = {
  0xdb: (fg) => fg,                                                         // █
  0xdc: (fg, bg) => `linear-gradient(to bottom, ${bg} 50%, ${fg} 50%)`,     // ▄
  0xdf: (fg, bg) => `linear-gradient(to bottom, ${fg} 50%, ${bg} 50%)`,     // ▀
  0xdd: (fg, bg) => `linear-gradient(to right, ${fg} 50%, ${bg} 50%) 0 0 / 1ch 100% repeat-x`, // ▌
  0xde: (fg, bg) => `linear-gradient(to right, ${bg} 50%, ${fg} 50%) 0 0 / 1ch 100% repeat-x`, // ▐
};

function buildRow(bytes, row) {
  const line = document.createElement('div');
  line.className = 'qesRow';
  let run = null;
  const flush = () => { if (run) line.appendChild(run.el); };
  for (let col = 0; col < COLS; col++) {
    const o = (row * COLS + col) * 2;
    const ch = bytes[o], attr = bytes[o + 1];
    const fg = VGA16[attr & 0x0f], bg = VGA16[(attr >> 4) & 0x07], blink = (attr & 0x80) !== 0;
    const block = BLOCKS[ch] ? ch : 0;
    const text = block ? ' ' : String.fromCodePoint(CP437[ch] ?? 0x20);
    if (run && run.fg === fg && run.bg === bg && run.blink === blink && run.block === block) {
      run.text += text;
      run.el.textContent = run.text;
    } else {
      flush();
      const el = document.createElement('span');
      el.style.color = fg;
      el.style.background = block ? BLOCKS[block](fg, bg) : bg;
      if (blink) el.className = 'qesBlink';
      el.textContent = text;
      run = { fg, bg, blink, block, text, el };
    }
  }
  flush();
  return line;
}

function buildScreen(bytes) {
  const grid = document.createElement('div');
  grid.className = 'qesGrid';
  for (let row = 0; row < ROWS; row++) grid.appendChild(buildRow(bytes, row));
  return grid;
}

let overlay = null;

// Show the end screen (or, without one, a plain message saying the game
// quit) over the whole page; any key, click or tap reloads it. `games`, if
// given, is index.html's game choices (an element), shown under it. Safe to
// call more than once (the second call is a no-op) — the program only ever
// sends one `Quit` record, but a page should never assume its own
// invariants.
function show(registered, screen, games) {
  if (overlay) return;
  ensureStyle();
  overlay = document.createElement('div');
  overlay.id = 'quakeEndScreen';
  overlay.setAttribute('role', 'img');
  overlay.setAttribute(
    'aria-label',
    screen ? `Quake has quit (${registered ? 'registered' : 'shareware'} ending screen) — press any key to restart`
           : 'Quake has quit — press any key to restart',
  );
  if (screen && screen.length >= COLS * ROWS * 2) {
    overlay.appendChild(buildScreen(screen));
  } else {
    const p = document.createElement('div');
    p.className = 'qesFallback';
    p.textContent = 'Quake has quit.\n\nPress any key, click or tap to play again.';
    overlay.appendChild(p);
  }
  if (games) {
    const row = document.createElement('div');
    row.className = 'qesGames';
    row.append('play', games);
    overlay.classList.add('qesWithGames');
    overlay.appendChild(row);
  }
  document.body.appendChild(overlay);
  const restart = () => location.reload();
  overlay.addEventListener('pointerdown', restart);
  overlay.addEventListener('touchstart', restart, { passive: true });
  window.addEventListener('keydown', restart, { capture: true });
}

window.QuakeEndScreen = { show };
})();

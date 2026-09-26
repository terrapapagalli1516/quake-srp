// touch.js — Quake on a touch screen: the touch controls, and what a phone
// needs of the page around them (web/PLATFORM.md, "Touch").
//
// index.html loads this file only on a touch screen (a coarse pointer, or
// `?touch` in the address) and hands it the page's few entry points:
// QuakeTouch.attach(host) (the host object is built in index.html's
// startTouch). Nothing here knows the protocol: a key goes through host.key
// (keys.c's Key_Event, the same records the keyboard sends), looking through
// host.look (IN_MouseMove's MOUSE record, so Options > Mouse Speed and
// Invert Mouse apply), and the rest through the program's calls
// (automation.rs): `set_move`, `set_attack`, `set_jump`, `set_impulse` for
// the controls — the client's analog inputs, which no key binding can take
// away — and `menu_tap`/`menu_point`, which ask the menu itself what is
// under a finger (quake-rs menu.rs, "Taps").
//
// What shows depends on the game's state (the State record):
//
//   the game, with in_touch (2026)  a stick under the left thumb wherever it
//                                   lands, look by dragging on the right,
//                                   FIRE (hold; drag it to aim too), JUMP,
//                                   next weapon, MENU
//   the game, in_touch off          MENU only (Classic: id's game has no
//                                   touch controls, but a phone must never be
//                                   left without a way to the menu)
//   a demo (the attract loop)       MENU; a tap anywhere is Escape, as any
//                                   key is in id's demo playback
//   the menu                        taps and drags on the menu; BACK is
//                                   Escape; YES / NO when it asks
//   the console                     KEYBOARD (the phone's own, through a
//                                   hidden text field; a tap on the console
//                                   too), TAB, the previous line; BACK
//                                   closes it; a drag scrolls
//   Multiplayer > Setup             KEYBOARD, for the names
//
// Around them: a prompt to turn the phone sideways, fullscreen and a
// landscape lock where the browser allows (Android), the screen kept awake
// in a game (Screen Wake Lock), audio resumed by any touch (iOS suspends
// it), and a live game paused under its menu when the page is hidden, until
// the player is back in the game. Haptics: rumble(), a hook for the
// gamepad's rumble events.
(function () {
  'use strict';

  // --- Tuning -----------------------------------------------------------------
  // Look: mouse counts per CSS pixel of drag. The engine turns 0.16 degrees
  // per count at the default Mouse Speed (quake-wasm input.rs M_YAW_PORT),
  // so a drag turns 0.32 degrees a pixel: a thumb's 280-pixel sweep across
  // the right of a phone is a right angle. Options > Mouse Speed scales it.
  const LOOK_COUNTS_PER_PX = 2;
  // in_touchaccel: a drag faster than this (CSS px per ms) turns up to
  // (1 + in_touchaccel) times as far.
  const ACCEL_FULL_SPEED = 2;
  // The stick: how far the thumb goes for full speed, and the dead zone
  // (a fraction of that) that keeps a resting thumb still.
  const STICK_RADIUS = 56;
  const STICK_DEADZONE = 0.12;
  // A finger that moves less than this is a tap.
  const TAP_SLOP = 10;
  // The stick owns the left of the screen, look the rest.
  const STICK_ZONE = 0.45;
  // The console: a key press per this much vertical drag (PgUp / PgDn).
  const SCROLL_STEP = 24;
  // Quake keynums (keys.h).
  const K = { TAB: 9, ENTER: 13, ESCAPE: 27, BACKSPACE: 127, UP: 128, PGDN: 149, PGUP: 150 };
  // The text field's content between keystrokes: a zero-width space whose
  // deletion is a Backspace (a field left empty sends none).
  const SENTINEL = '\u200b';

  let host = null;
  let layer = null, ui = {};
  let mode = 'boot';
  let accel = 0;                  // in_touchaccel, read when the menu or console closes
  const tracks = new Map();       // pointerId -> what that finger is doing
  let move = { fwd: 0, side: 0, sent: '0 0' };
  let menuPoint = null;           // a menu point to send on the next frame
  let flushQueued = false;
  let autoPaused = false;         // hidden while playing: paused under the menu
  let wakeLock = null;

  // --- The page's state -------------------------------------------------------
  function flags() { return host.state.flags; }
  function has(bit) { return !!(flags() & bit); }
  // What the touch screen is showing, from the State record's flags.
  function currentMode() {
    const ST = host.ST;
    if (!host.started()) return 'boot';
    if (has(ST.ASK)) return 'ask';
    if (has(ST.MENU)) return 'menu';
    if (has(ST.CONSOLE)) return 'console';
    if (has(ST.WALK)) return has(ST.TOUCH) ? 'play' : 'game';
    return 'demo';
  }

  // Every State record (index.html calls it): switch what shows.
  function state() {
    const next = currentMode();
    const naming = next === 'menu' && host.state.menuScreen === SETUP_SCREEN;
    if (next !== mode) {
      const was = mode;
      mode = next;
      layer.dataset.mode = mode;
      if (was === 'play') releaseAll();
      if (was === 'menu' || was === 'console' || was === 'boot') readSettings();
      // Back in the game after the page was hidden: the pause ends.
      if ((next === 'play' || next === 'game') && autoPaused) {
        autoPaused = false;
        if (has(host.ST.PAUSED)) call('exec pause');
      }
    }
    if (next !== 'console' && !naming) closeKeyboard();
    // Multiplayer > Setup's name fields want the keyboard too.
    if (ui.menuKeys.hidden === naming) ui.menuKeys.hidden = !naming;
    keepAwake(has(host.ST.WALK));
  }
  // menu_screen_id's Multiplayer > Setup (quake-wasm menu.rs).
  const SETUP_SCREEN = 11;

  // in_touchaccel, from the program (it changes only in the console).
  function readSettings() {
    call('cvar in_touchaccel').then(r => { accel = r.value > 0 ? r.value : 0; });
  }

  // --- Sending ----------------------------------------------------------------
  // A call line (automation.rs); its answer, or NaN when the game is not
  // running (a finger does not care).
  function call(line) { return host.call(line).catch(() => ({ value: NaN, text: '' })); }
  function press(keynum, ch) { host.key(keynum, true, ch || 0); host.key(keynum, false, 0); }
  // The stick and a menu drag go out once a display frame, the newest only.
  function queueFlush() {
    if (flushQueued) return;
    flushQueued = true;
    requestAnimationFrame(() => {
      flushQueued = false;
      const m = `${move.fwd.toFixed(3)} ${move.side.toFixed(3)}`;
      if (m !== move.sent) { move.sent = m; call('set_move ' + m); }
      if (menuPoint) { call(`menu_point ${menuPoint[0]} ${menuPoint[1]}`); menuPoint = null; }
    });
  }
  function setMove(fwd, side) { move.fwd = fwd; move.side = side; queueFlush(); }
  // Let go of everything held (a finger lifted behind the menu, the page
  // hidden): no stick, no fire, no jump.
  function releaseAll() {
    for (const t of tracks.values()) if (t.el) t.el.classList.remove('held');
    tracks.clear();
    hideStick();
    setMove(0, 0);
    call('set_attack 0');
    call('set_jump 0');
  }

  // The frame pixel under a client point (the canvas may be a 4:3 box, or
  // the window at whole device pixels: its CSS box maps onto the frame).
  function framePoint(x, y) {
    const c = host.canvas, r = c.getBoundingClientRect();
    const [w, h] = host.frameSize();
    return [Math.round((x - r.left - c.clientLeft) * w / c.clientWidth),
            Math.round((y - r.top - c.clientTop) * h / c.clientHeight)];
  }

  // Any touch may resume audio: iOS suspends (or "interrupts") it when the
  // page is hidden, and resuming needs a gesture.
  function wakeAudio() {
    host.unlockAudio();
    const ctx = host.audio();
    if (ctx && ctx.state !== 'running') ctx.resume().catch(() => {});
  }

  // --- The screen: the layer's fingers ------------------------------------------
  function onDown(e) {
    if (e.pointerType === 'mouse') return;
    e.preventDefault();
    layer.setPointerCapture?.(e.pointerId);
    const t = { x0: e.clientX, y0: e.clientY, x: e.clientX, y: e.clientY, at: e.timeStamp, moved: false, role: 'tap', scrolled: 0 };
    if (mode === 'play') {
      const stickFree = ![...tracks.values()].some(o => o.role === 'stick');
      t.role = e.clientX < innerWidth * STICK_ZONE && stickFree ? 'stick' : 'look';
      if (t.role === 'stick') showStick(t.x0, t.y0);
    } else if (mode === 'menu') {
      t.role = 'menu';
    } else if (mode === 'console') {
      t.role = 'scroll';
    }
    tracks.set(e.pointerId, t);
  }

  function onMove(e) {
    const t = tracks.get(e.pointerId);
    if (!t) return;
    e.preventDefault();
    const dx = e.clientX - t.x, dy = e.clientY - t.y, dt = Math.max(1, e.timeStamp - t.at);
    t.x = e.clientX; t.y = e.clientY; t.at = e.timeStamp;
    if (Math.hypot(t.x - t.x0, t.y - t.y0) > TAP_SLOP) t.moved = true;
    switch (t.role) {
      case 'stick': stickTo(t); break;
      case 'look': look(dx, dy, dt); break;
      case 'menu':
        if (t.moved) { menuPoint = framePoint(t.x, t.y); queueFlush(); }
        break;
      case 'scroll': {
        // Finger down, the text moves down: PgUp shows the lines above.
        const steps = Math.trunc((t.y - t.y0) / SCROLL_STEP) - t.scrolled;
        for (let i = 0; i < Math.abs(steps); i++) press(steps > 0 ? K.PGUP : K.PGDN);
        t.scrolled += steps;
        break;
      }
    }
  }

  function onUp(e) {
    const t = tracks.get(e.pointerId);
    if (!t) return;
    tracks.delete(e.pointerId);
    wakeAudio();
    if (e.type === 'pointercancel') { if (t.role === 'stick') { hideStick(); setMove(0, 0); } return; }
    switch (t.role) {
      case 'stick': hideStick(); setMove(0, 0); break;
      case 'menu':
        if (!t.moved) { const [x, y] = framePoint(t.x, t.y); call(`menu_tap ${x} ${y}`); }
        break;
      case 'scroll': if (!t.moved) openKeyboard(); break;
      case 'tap':
        // A demo plays: a tap is a key, and any key brings up the menu.
        if (!t.moved && mode === 'demo') press(K.ESCAPE);
        break;
    }
  }

  // Look: the drag as mouse counts, faster drags further with in_touchaccel.
  function look(dx, dy, dt) {
    const speed = Math.hypot(dx, dy) / dt;
    const gain = LOOK_COUNTS_PER_PX * (1 + accel * Math.min(speed / ACCEL_FULL_SPEED, 1));
    host.look(dx * gain, dy * gain);
  }

  // The stick: the thumb's offset from where it landed, past the dead zone,
  // as forward and side fractions (the client's analog move, full speed at
  // STICK_RADIUS).
  function stickTo(t) {
    let dx = (t.x - t.x0) / STICK_RADIUS, dy = (t.y - t.y0) / STICK_RADIUS;
    const m = Math.hypot(dx, dy);
    if (m > 1) { dx /= m; dy /= m; }
    // The throw past the dead zone, 0..1, along the thumb's direction.
    const r = Math.min(m, 1);
    const k = r <= STICK_DEADZONE ? 0 : (r - STICK_DEADZONE) / (1 - STICK_DEADZONE) / r;
    setMove(-dy * k, dx * k);
    ui.knob.style.transform = `translate(${dx * STICK_RADIUS}px, ${dy * STICK_RADIUS}px)`;
  }
  function showStick(x, y) {
    const r = layer.getBoundingClientRect();
    ui.stick.style.left = (x - r.left) + 'px';
    ui.stick.style.top = (y - r.top) + 'px';
    ui.knob.style.transform = '';
    ui.stick.classList.add('on');
  }
  function hideStick() { ui.stick.classList.remove('on'); ui.knob.style.transform = ''; }

  // --- The buttons -------------------------------------------------------------
  // A button: `down`/`up` when a finger lands on it and lifts (or slides away
  // and lifts: the pointer is captured). `aims` also lets that finger look.
  function button(el, { down, up, aims }) {
    el.addEventListener('pointerdown', (e) => {
      if (e.pointerType === 'mouse' && e.button !== 0) return;
      e.preventDefault();
      e.stopPropagation();
      el.setPointerCapture?.(e.pointerId);
      el.classList.add('held');
      if (aims) tracks.set(e.pointerId, { x: e.clientX, y: e.clientY, x0: e.clientX, y0: e.clientY, at: e.timeStamp, role: 'look', el });
      if (down) down(e);
    });
    const end = (e) => {
      if (!el.classList.contains('held')) return;
      el.classList.remove('held');
      tracks.delete(e.pointerId);
      wakeAudio();
      if (up) up(e);
    };
    el.addEventListener('pointerup', end);
    el.addEventListener('pointercancel', end);
    if (aims) el.addEventListener('pointermove', (e) => onMove(e));
  }

  // --- The phone's keyboard, for the console and Setup's names -----------------
  function openKeyboard() {
    ui.type.value = SENTINEL;
    ui.type.focus();   // in the tap's own handler: iOS shows its keyboard only then
    ui.type.setSelectionRange(1, 1);
  }
  function closeKeyboard() { if (document.activeElement === ui.type) ui.type.blur(); }
  function onType(e) {
    if (e && e.isComposing) return;               // a word still being composed (Android): at its end
    const v = ui.type.value;
    let text = v;
    if (v.startsWith(SENTINEL)) text = v.slice(SENTINEL.length);
    else press(K.BACKSPACE);                          // the sentinel went: Backspace
    for (const ch of text) {
      if (ch === '\n') { press(K.ENTER); continue; }
      const code = ch.codePointAt(0);
      // Printable ASCII is its own keynum (lower case, as the page's
      // keyboard sends it); anything else types nothing, as there.
      if (code >= 32 && code < 127) press(ch.toLowerCase().charCodeAt(0), code);
    }
    ui.type.value = SENTINEL;
    ui.type.setSelectionRange(1, 1);
  }
  function onTypeKey(e) {
    e.stopPropagation();                              // not the page's keyboard too
    if (e.type === 'keydown' && e.key === 'Enter') { e.preventDefault(); press(K.ENTER); }
  }

  // --- Around the controls: fullscreen, orientation, wake lock, hiding ----------
  const canFullscreen = () => !!(document.fullscreenEnabled && host.wrap.requestFullscreen);
  // Fullscreen, then the landscape lock (Android allows it only fullscreen).
  function goFullscreen() {
    if (!canFullscreen() || document.fullscreenElement) return;
    host.wrap.requestFullscreen({ navigationUI: 'hide' })
      .then(() => screen.orientation?.lock?.('landscape'))
      .catch(() => {});
  }
  function syncFullscreenButton() {
    ui.full.hidden = !canFullscreen() || !!document.fullscreenElement;
  }

  // The screen stays on during a game (the live game, its menu included).
  let wantAwake = false;
  async function keepAwake(on) {
    wantAwake = on;
    if (!('wakeLock' in navigator)) return;
    if (on && !wakeLock && !document.hidden) {
      wakeLock = 'asking';
      try {
        const lock = await navigator.wakeLock.request('screen');
        wakeLock = lock;
        lock.addEventListener('release', () => { if (wakeLock === lock) wakeLock = null; });
        if (!wantAwake) keepAwake(false);             // the game ended while it was asked for
      } catch (err) { wakeLock = null; }
    } else if (!on && wakeLock && wakeLock !== 'asking') {
      wakeLock.release().catch(() => {});
      wakeLock = null;
    }
  }

  // The page hidden (another app, the lock button): a live game pauses
  // (id's `pause`, its plaque) under its menu, and going back to the game
  // resumes it; the sound stops until the page is back.
  function onVisibility() {
    const ctx = host.audio();
    if (document.hidden) {
      releaseAll();
      closeKeyboard();
      if (has(host.ST.WALK) && !has(host.ST.PAUSED)) { call('exec pause'); autoPaused = true; }
      if (mode === 'play' || mode === 'game') press(K.ESCAPE);   // togglemenu: the menu opens
      if (ctx && ctx.state === 'running') ctx.suspend().catch(() => {});
    } else {
      if (ctx) ctx.resume().catch(() => {});
      keepAwake(has(host.ST.WALK));
    }
  }

  // Haptics: the hook the gamepad's rumble events can call too (the `input`
  // agent's damage and heavy-weapon rumble), with the Gamepad API's
  // dual-rumble magnitudes. navigator.vibrate has no strength, so a
  // stronger rumble buzzes longer. Android only: iOS Safari has no vibrate.
  function rumble(weak, strong, ms) {
    const s = Math.max(weak || 0, strong || 0);
    if (!navigator.vibrate || document.hidden || s < 0.1 || !(mode === 'play' || mode === 'game')) return;
    navigator.vibrate(Math.round(Math.min(ms || 100, 400) * s));
  }

  // --- Building it ---------------------------------------------------------------
  const CSS = `
  /* The touch layout: the picture fills the screen (the canvas box has no
     chrome), under the notch too; the controls keep to the safe area. */
  html.touch, html.touch body { height:100%; overflow:hidden; overscroll-behavior:none;
    -webkit-user-select:none; user-select:none; -webkit-touch-callout:none; -webkit-tap-highlight-color:transparent; }
  html.touch body { display:block; min-height:0; }
  html.touch body::before, html.touch body::after,
  html.touch #bar, html.touch #drawer, html.touch #lockChip, html.touch #fsHint { display:none !important; }
  html.touch #stage { position:fixed; inset:0; margin:0; display:block; }
  html.touch #wrap { position:absolute; inset:0; display:flex; align-items:center; justify-content:center; background:#000; }
  html.touch canvas { --chrome-v:0px; --chrome-h:0px; border:0; cursor:default;
    width:min(100vw, 100vh * 4 / 3); width:min(100vw, 100dvh * 4 / 3); }
  #touch { position:absolute; inset:0; touch-action:none; z-index:2;
    font-family:ui-monospace,'SF Mono',Menlo,monospace; color:#d9a546; }
  #touch[data-mode=boot] { display:none; }
  #touch .tb { position:absolute; display:flex; align-items:center; justify-content:center;
    box-sizing:border-box; border:2px solid rgba(181,131,47,.5); border-radius:50%;
    background:radial-gradient(circle at 50% 35%, rgba(58,45,28,.55), rgba(11,9,7,.55));
    box-shadow:inset 0 0 0 1px rgba(0,0,0,.6); color:rgba(217,165,70,.85);
    font-size:11px; font-weight:600; letter-spacing:.14em; text-indent:.14em; touch-action:none; }
  #touch .tb.held { background:rgba(181,131,47,.45); color:#fff0b0; border-color:#d9a546; }
  #touch .tb.pill { border-radius:4px; height:34px; padding:0 12px; }
  #touch [hidden] { display:none !important; }
  /* Which controls each state shows. */
  #touch .play, #touch .menuonly, #touch .console, #touch .ask, #touch .gameonly { display:none; }
  #touch[data-mode=play] .play,
  #touch[data-mode=play] .gameonly, #touch[data-mode=game] .gameonly, #touch[data-mode=demo] .gameonly,
  #touch[data-mode=menu] .menuonly, #touch[data-mode=ask] .ask,
  #touch[data-mode=console] .console { display:flex; }
  #tMenu, #tBack { top:calc(10px + env(safe-area-inset-top)); left:calc(12px + env(safe-area-inset-left)); }
  #tFull { top:calc(10px + env(safe-area-inset-top)); left:calc(92px + env(safe-area-inset-left)); width:34px; padding:0; }
  #tFire { width:84px; height:84px; right:calc(24px + env(safe-area-inset-right)); bottom:calc(28px + env(safe-area-inset-bottom)); }
  #tJump { width:62px; height:62px; right:calc(120px + env(safe-area-inset-right)); bottom:calc(20px + env(safe-area-inset-bottom)); }
  #tWeapon { width:58px; height:58px; right:calc(36px + env(safe-area-inset-right)); bottom:calc(126px + env(safe-area-inset-bottom));
    font-size:9px; letter-spacing:.04em; text-indent:.04em; }
  #tStick { position:absolute; width:${2 * STICK_RADIUS}px; height:${2 * STICK_RADIUS}px; margin:-${STICK_RADIUS}px 0 0 -${STICK_RADIUS}px;
    border:2px solid rgba(181,131,47,.35); border-radius:50%; background:rgba(11,9,7,.25);
    opacity:0; transition:opacity .12s; pointer-events:none; }
  #tStick.on { opacity:1; }
  #tKnob { position:absolute; left:50%; top:50%; width:44px; height:44px; margin:-22px 0 0 -22px; border-radius:50%;
    background:radial-gradient(circle at 50% 35%, rgba(217,165,70,.6), rgba(107,74,26,.6)); }
  #tHint { position:absolute; left:calc(40px + env(safe-area-inset-left)); bottom:calc(40px + env(safe-area-inset-bottom));
    width:${2 * STICK_RADIUS}px; height:${2 * STICK_RADIUS}px; border:2px dashed rgba(181,131,47,.18); border-radius:50%;
    pointer-events:none; }
  #touch .row { position:absolute; left:50%; transform:translateX(-50%); gap:10px; }
  #tAsk { bottom:calc(36px + env(safe-area-inset-bottom)); }
  #tAsk .tb { position:static; width:92px; }
  #tConsole { bottom:calc(16px + env(safe-area-inset-bottom)); }
  #tConsole .tb, #tMenuKeys .tb { position:static; }
  #tMenuKeys { bottom:calc(16px + env(safe-area-inset-bottom)); }
  #tType { position:absolute; left:0; top:0; width:1px; height:1px; opacity:0; border:0; padding:0;
    font-size:16px; /* iOS zooms into a smaller field */ }
  /* Held upright: turn the phone. A tap dismisses it for the session. */
  #tRotate { position:absolute; inset:0; z-index:3; display:none; flex-direction:column; gap:18px;
    align-items:center; justify-content:center; background:rgba(0,0,0,.88); color:#cfc6b6;
    font:14px ui-monospace,'SF Mono',Menlo,monospace; letter-spacing:.08em; text-align:center; padding:24px; }
  #tRotate small { color:#8a7d68; font-size:12px; }
  @media (orientation: portrait) { html.touch:not(.upright) #tRotate { display:flex; } }
  `;

  const ROTATE_SVG = `<svg width="72" height="72" viewBox="0 0 72 72" fill="none" stroke="#d9a546" stroke-width="3" aria-hidden="true">
    <rect x="24" y="8" width="24" height="42" rx="3"/><path d="M14 58 h44" stroke="#6b4a1a"/>
    <path d="M58 36 a22 22 0 0 1 -22 22" stroke-dasharray="4 4"/><path d="M40 54 l-4 4 4 4"/></svg>`;
  const FULL_SVG = `<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
    <path d="M1 6V1h5M10 1h5v5M15 10v5h-5M6 15H1v-5"/></svg>`;

  function build() {
    const style = document.createElement('style');
    style.textContent = CSS;
    document.head.appendChild(style);
    layer = document.createElement('div');
    layer.id = 'touch';
    layer.dataset.mode = 'boot';
    layer.innerHTML = `
      <div id="tHint" class="play"></div>
      <div id="tStick"><div id="tKnob"></div></div>
      <div id="tMenu" class="tb pill gameonly" role="button" aria-label="menu">MENU</div>
      <div id="tFull" class="tb pill gameonly" role="button" aria-label="fullscreen" hidden>${FULL_SVG}</div>
      <div id="tBack" class="tb pill menuonly console" role="button" aria-label="back">BACK</div>
      <div id="tFire" class="tb play" role="button" aria-label="fire">FIRE</div>
      <div id="tJump" class="tb play" role="button" aria-label="jump">JUMP</div>
      <div id="tWeapon" class="tb play" role="button" aria-label="next weapon">WEAPON</div>
      <div id="tAsk" class="row ask"><div id="tYes" class="tb pill" role="button">YES</div><div id="tNo" class="tb pill" role="button">NO</div></div>
      <div id="tConsole" class="row console">
        <div id="tKeyboard" class="tb pill" role="button">KEYBOARD</div>
        <div id="tTab" class="tb pill" role="button">TAB</div>
        <div id="tPrev" class="tb pill" role="button" aria-label="previous line">&#9650;</div>
      </div>
      <div id="tMenuKeys" class="row menuonly"><div id="tNameKeys" class="tb pill" role="button">KEYBOARD</div></div>
      <input id="tType" type="text" autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false" enterkeyhint="send" aria-label="type">`;
    host.wrap.appendChild(layer);
    const $ = (id) => layer.querySelector('#' + id);
    ui = { stick: $('tStick'), knob: $('tKnob'), full: $('tFull'), type: $('tType'), menuKeys: $('tMenuKeys') };

    layer.addEventListener('pointerdown', onDown);
    layer.addEventListener('pointermove', onMove);
    layer.addEventListener('pointerup', onUp);
    layer.addEventListener('pointercancel', onUp);
    layer.addEventListener('contextmenu', (e) => e.preventDefault());

    button($('tMenu'), { up: () => press(K.ESCAPE) });
    button($('tBack'), { up: () => (mode === 'console' ? call('exec toggleconsole') : press(K.ESCAPE)) });
    button($('tFull'), { up: goFullscreen });
    button($('tFire'), { down: () => call('set_attack 1'), up: () => call('set_attack 0'), aims: true });
    button($('tJump'), { down: () => call('set_jump 1'), up: () => call('set_jump 0') });
    // impulse 10: weapons.qc's CycleWeaponCommand, the next weapon owned.
    button($('tWeapon'), { up: () => call('set_impulse 10') });
    button($('tYes'), { up: () => press(121, 121) });   // y
    button($('tNo'), { up: () => press(110, 110) });    // n
    button($('tKeyboard'), { up: openKeyboard });
    button($('tNameKeys'), { up: openKeyboard });
    button($('tTab'), { up: () => press(K.TAB) });
    button($('tPrev'), { up: () => press(K.UP) });
    ui.type.addEventListener('input', onType);
    ui.type.addEventListener('compositionend', onType);
    ui.type.addEventListener('keydown', onTypeKey);
    ui.type.addEventListener('keyup', onTypeKey);
    // Held upright: turn the phone (over the start prompt too, so outside
    // the layer). A tap dismisses it for the session.
    const rotate = document.createElement('div');
    rotate.id = 'tRotate';
    rotate.setAttribute('role', 'button');
    rotate.innerHTML = `${ROTATE_SVG}<div>TURN YOUR PHONE SIDEWAYS</div><small>or tap to play upright</small>`;
    rotate.addEventListener('click', () => document.documentElement.classList.add('upright'));
    host.wrap.appendChild(rotate);

    document.addEventListener('visibilitychange', onVisibility);
    document.addEventListener('fullscreenchange', syncFullscreenButton);
    // The first tap (the page's "tap to start") also asks for fullscreen and
    // landscape, where the browser has them; iOS has neither for a page: its
    // fullscreen is the home-screen app (manifest.webmanifest).
    host.overlay.addEventListener('click', () => { if (host.started()) goFullscreen(); });
    syncFullscreenButton();
  }

  window.QuakeTouch = {
    // index.html's startTouch: the page's entry points (see the top).
    attach(h) {
      host = h;
      document.documentElement.classList.add('touch');
      host.overlay.querySelector('#play').textContent = 'tap to start';
      build();
      readSettings();
      state();
      return { state, rumble };
    },
    rumble: (weak, strong, ms) => rumble(weak, strong, ms),
  };
})();

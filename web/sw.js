// sw.js — offline play, quick reloads, and cross-origin isolation on any
// static host (web/PLATFORM.md, "Offline and install").
//
// One file with two roles. Run as the page's service worker (the bottom half
// registers it), it answers the page's requests:
//
//   - the game data (`id1/*.pak`, `id1/music/*`): from its cache when it has
//     them, else from the network, kept. id's own data never changes once
//     deployed (the shareware pak, a server's own pak1 and CD tracks,
//     web/PLATFORM.md's "A server's own files"); bump DATA_CACHE to fetch
//     it afresh.
//   - everything else (the page, wasi.js, touch.js, quake.wasm, the app
//     manifest and icons, and a server's own files.json): from the network,
//     kept for later, and from what was kept when the network fails. So
//     online the page is always the one deployed (an update takes effect at
//     the next load, with nothing to bump), and offline it is the one last
//     played — files.json included, so a deploy that later adds pak1 and
//     tracks is only offered once a player has been online since.
//   - every answer carries the two cross-origin isolation headers, which the
//     page's SharedArrayBuffers need. A server that sends them loses
//     nothing; one that cannot (a plain static host) gets them from here,
//     once the page has reloaded under this worker (the "coi-serviceworker"
//     technique; PLATFORM.md has its trade-offs).
//
// It keeps nothing the server marks `Cache-Control: no-store` (the browser
// checks' server does, so the checks leave no copies behind).
//
// Loaded by the page (a <script> in index.html's head), it registers itself
// and sets `window.quakeServiceWorker`, a Promise the page waits for before
// it starts: it resolves once this worker controls the page (the first
// visit included, so that visit's downloads are kept and the next can be
// offline), or at once where there are no service workers; and where the
// page is not isolated but a reload under the worker would make it so, it
// reloads the page instead (once per tab session).

if (typeof window === 'undefined') {
  // ===========================================================================
  // The service worker
  // ===========================================================================
  const SHELL_CACHE = 'quake-rs-shell';
  const DATA_CACHE = 'quake-rs-data-1';
  // The page's own small files, kept at install so the first visit can be
  // replayed offline (its navigation came before this worker). quake.wasm
  // and the pak are kept as the page fetches them through the worker.
  const PRECACHE = ['index.html', 'sw.js', 'wasi.js', 'touch.js', 'manifest.webmanifest',
                    'icons/icon-192.png', 'icons/icon-512.png', 'icons/apple-touch-icon.png'];

  self.addEventListener('install', (e) => {
    // A new version takes over at once: the page's files come from the
    // network anyway, so no open page is left on a mismatched cache. (Not
    // awaited: Chromium settles it only once the install is over.)
    self.skipWaiting();
    e.waitUntil((async () => {
      const cache = await caches.open(SHELL_CACHE);
      // One by one: a file this deploy lacks is skipped, not fatal.
      await Promise.all(PRECACHE.map(async (path) => {
        try {
          const resp = await fetch(path, { cache: 'no-cache' });
          // A body nobody reads holds its connection (six per host):
          // cancelled, or the page's own downloads wait behind it.
          if (keepable(resp)) await cache.put(path, resp);
          else await resp.body?.cancel();
        } catch (err) { /* offline, or not deployed */ }
      }));
    })());
  });

  self.addEventListener('activate', (e) => {
    e.waitUntil((async () => {
      for (const name of await caches.keys()) {
        if (name !== SHELL_CACHE && name !== DATA_CACHE) await caches.delete(name);
      }
      await self.clients.claim();
    })());
  });

  self.addEventListener('fetch', (e) => {
    const req = e.request;
    const url = new URL(req.url);
    if (req.method !== 'GET' || url.origin !== self.location.origin) return;
    if (isManifest(url)) e.respondWith(manifestFirst(req));
    else e.respondWith(isGameData(url) ? dataFirst(req) : networkFirst(req));
  });

  // id's own data, wherever a deploy keeps it: a pak, or a CD track under
  // id1/music/ ("A server's own files").
  function isGameData(url) {
    return /\/id1\/([^/]*\.pak|music\/[^/]+)$/i.test(url.pathname);
  }
  function isManifest(url) {
    return url.pathname === new URL('files.json', self.registration.scope).pathname;
  }

  // The server allowed it to be kept (a whole, successful answer, not
  // `no-store`).
  function keepable(resp) {
    return resp.ok && resp.status === 200 && !/no-store/i.test(resp.headers.get('Cache-Control') || '');
  }

  // The name a page file is kept under: its path in the scope, the page for
  // any navigation (`./`, `?classic`, `index.html`).
  function cacheKey(req) {
    if (req.mode === 'navigate') return 'index.html';
    const scope = new URL(self.registration.scope);
    const url = new URL(req.url);
    return url.pathname.startsWith(scope.pathname) ? url.pathname.slice(scope.pathname.length) : url.pathname;
  }

  async function networkFirst(req) {
    const key = cacheKey(req);
    try {
      const resp = await fetch(req);
      if (keepable(resp) && (req.mode === 'navigate' ? isPage(req) : true)) {
        const copy = resp.clone();
        caches.open(SHELL_CACHE).then(c => c.put(key, copy)).catch(() => {});
      }
      return isolated(resp);
    } catch (err) {
      const kept = await caches.match(key, { cacheName: SHELL_CACHE });
      if (kept) return isolated(kept);
      throw err;
    }
  }

  // files.json ("A server's own files"): network first and kept, like any
  // other page file — but a deploy with none of its own (most of them) must
  // never show the page a failing request: answered 200 with an empty list
  // instead of whatever the network gave (a 404, most often). Not cached
  // when empty, so a deploy that starts offering one is seen at the very
  // next online load, with nothing to bump.
  async function manifestFirst(req) {
    const key = cacheKey(req);
    try {
      const resp = await fetch(req);
      if (!resp.ok) return isolated(emptyManifest());   // most often: no files.json at all
      // Used either way; kept only if the server allows it (keepable is
      // about caching, not about whether this answer is a real one).
      if (keepable(resp)) {
        const copy = resp.clone();
        caches.open(SHELL_CACHE).then(c => c.put(key, copy)).catch(() => {});
      }
      return isolated(resp);
    } catch (err) {
      const kept = await caches.match(key, { cacheName: SHELL_CACHE });
      return isolated(kept || emptyManifest());
    }
  }
  function emptyManifest() {
    return new Response('{"files":[]}', { headers: { 'Content-Type': 'application/json' } });
  }

  // A navigation to the page itself (not to some other document in scope).
  function isPage(req) {
    const path = new URL(req.url).pathname;
    return path.endsWith('/') || path.endsWith('/index.html');
  }

  async function dataFirst(req) {
    const cache = await caches.open(DATA_CACHE);
    const key = cacheKey(req);
    const kept = await cache.match(key);
    if (kept) return isolated(kept);
    const resp = await fetch(req);
    if (keepable(resp)) {
      // Kept while the page reads it: the body goes both ways.
      cache.put(key, resp.clone()).catch(() => {});
    }
    return isolated(resp);
  }

  // The answer with the cross-origin isolation headers (and a resource
  // policy, which a same-origin load does not need but costs nothing).
  function isolated(resp) {
    if (resp.status === 0) return resp;   // opaque: not ours to change
    const headers = new Headers(resp.headers);
    headers.set('Cross-Origin-Opener-Policy', 'same-origin');
    headers.set('Cross-Origin-Embedder-Policy', 'require-corp');
    headers.set('Cross-Origin-Resource-Policy', 'same-origin');
    return new Response(resp.body, { status: resp.status, statusText: resp.statusText, headers });
  }
} else {
  // ===========================================================================
  // The page's half: register the worker, wait for it, isolate the page
  // ===========================================================================
  window.quakeServiceWorker = (async () => {
    const sw = navigator.serviceWorker;
    if (!sw || !window.isSecureContext) return 'none';
    const RELOADED = 'quake-rs.isolation-reload';
    const session = {
      get() { try { return sessionStorage.getItem(RELOADED); } catch (err) { return 'blocked'; } },
      set(v) { try { v ? sessionStorage.setItem(RELOADED, v) : sessionStorage.removeItem(RELOADED); } catch (err) {} },
    };
    let reg;
    try {
      reg = await sw.register('sw.js', { updateViaCache: 'none' });
    } catch (err) {
      console.warn('[quake] no service worker:', err.message || err);
      return 'failed';
    }
    // The first visit: the worker claims the page as it activates. (An
    // active worker that does not control the page is a hard reload, which
    // bypassed it: nothing to wait for.)
    if (!sw.controller && !reg.active) {
      await new Promise(resolve => {
        sw.addEventListener('controllerchange', resolve, { once: true });
        setTimeout(resolve, 3000);
      });
    }
    if (self.crossOriginIsolated) { session.set(null); return 'isolated'; }
    // Not isolated: the server sent no headers. Under the worker a reload
    // gets them — once, so a browser that ignores them does not loop.
    if ((sw.controller || reg.active) && !session.get()) {
      session.set('1');
      location.reload();
      return new Promise(() => {});
    }
    return 'not isolated';
  })();
}

#!/usr/bin/env -S uv run --with playwright --script
"""What the page does when keeping a file the program wrote fails, in a real
browser (web/PLATFORM.md, "Saves and settings go through std::fs"). The
program's own write always succeeds — its file lives in the worker's file
system for the session — and then the page keeps a copy in IndexedDB for the
next start; this checks that second step when the browser makes it hard:

- every slot, s0..s11, saved on a bigger map (e1m3), is kept, and nothing
  goes wrong (no slot number or save size is special);
- the database connection closed under the page — by the page itself, by the
  browser clearing the site's data (Chromium: CDP's
  `Storage.clearDataForOrigin`, as DevTools' "Clear site data" and eviction
  do), and for another tab that opens a newer version of the database (which
  this page lets go, rather than leave that tab blocked): the save is kept
  anyway, on a connection opened again, and the console says nothing;
- a write the browser refuses outright (a DataCloneError, here from a view on
  shared memory): the console line names that error, in one piece;
- a full store (a small quota: Chromium's CDP
  `Storage.overrideQuotaForOrigin`, Firefox's
  `dom.quotaManager.temporaryStorage.fixedLimit`, filled up by the check):
  the console says QuotaExceededError, how much the site uses of what it is
  allowed, what to do, and that a reload loses the save; the save still
  loads in the session; once there is room again, the next save is kept;
- no IndexedDB (a private window, the localStorage fallback) and
  localStorage full: the console says it is the browser's private or
  limited mode;
- the page asks for persistent storage once (not in Firefox, where asking
  shows a prompt).

Usage: verify_storage.py [webdir]   ($QUAKE_BROWSER: chromium or firefox)"""
import os, sys, time
from playwright.sync_api import sync_playwright
import isolated

WEB = isolated.webdir()
PORT = isolated.port(8179)
httpd = isolated.serve(WEB, PORT)
ORIGIN = f"http://127.0.0.1:{PORT}"
URL = f"{ORIGIN}/index.html"
BROWSER = os.environ.get("QUAKE_BROWSER", "chromium")
QUOTA_KB = 4096
fails = []


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + (f" ({detail})" if detail and not ok else ""))
    if not ok:
        fails.append(name + (f": {detail}" if detail else ""))


# How often the page asks for persistent storage.
COUNT_PERSIST = """
window.__persistAsked = 0;
if (self.StorageManager && StorageManager.prototype.persist) {
  const persist = StorageManager.prototype.persist;
  StorageManager.prototype.persist = function () { window.__persistAsked++; return persist.call(this); };
}
"""
# A private window where the page gets no IndexedDB.
NO_IDB = "Object.defineProperty(window, 'indexedDB', { get() { throw new DOMException('denied', 'SecurityError'); } });"


def open_game(ctx, pg=None):
    pg = pg or ctx.new_page()
    pg.on("pageerror", lambda e: fails.append("PAGEERROR: " + str(e)))
    pg.goto(URL, wait_until="load")
    pg.wait_for_function("window.quake && quake.ready", timeout=120000)
    pg.evaluate("document.getElementById('walkBtn').click()")
    isolated.wait_until(pg, "quake.text('map_name').then(m => m === 'maps/e1m1.bsp')", 20)
    return pg


def exec_line(pg, line):
    pg.evaluate(f"quake.callLine({('exec ' + line)!r})")


def console_text(pg):
    """The console's text with its line breaks taken out: Con_Print breaks a
    long line at the console's width, anywhere around a space, so a message
    is looked for in one run of text."""
    return pg.evaluate("quake.text('console_text')").replace("\n", "")


def console_after(pg, marker):
    """The console's lines after the last one that says `marker`."""
    text = console_text(pg)
    return text.split(marker)[-1] if marker in text else ""


def save(pg, slot, timeout=10.0):
    """`save sN`, then what the console printed after id's "Saving game to
    sN.sav..." (its own "done." and anything the page added), once the page
    has kept the new file or said why not (Firefox can take seconds to
    refuse a write to a full store)."""
    path = f"id1/s{slot}.sav"
    before = pg.evaluate(f"quake.kept('{path}')")
    exec_line(pg, f"save s{slot}")
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if (f"This session still has {path}" in console_after(pg, f"Saving game to s{slot}.sav...")
                or pg.evaluate(f"quake.kept('{path}')") not in (None, before)):
            break
        time.sleep(0.1)
    time.sleep(0.3)
    return console_after(pg, f"Saving game to s{slot}.sav...")


def kept_len(pg, path):
    return pg.evaluate(f"quake.kept('{path}').then(t => t === null ? -1 : t.length)")


with sync_playwright() as p:
    br = isolated.launch(p, ["--no-sandbox"],
                         prefs={"dom.quotaManager.temporaryStorage.fixedLimit": QUOTA_KB})
    vp = {"width": 820, "height": 540}

    # --- One tab, a working store: every slot, a bigger map ------------------
    ctx = br.new_context(viewport=vp)
    ctx.add_init_script(COUNT_PERSIST)
    pg = open_game(ctx)
    exec_line(pg, "map e1m3")
    isolated.wait_until(pg, "quake.text('map_name').then(m => m === 'maps/e1m3.bsp')", 20)
    time.sleep(1.0)
    after = [save(pg, s) for s in range(12)]
    time.sleep(1.0)
    sizes = [kept_len(pg, f"id1/s{s}.sav") for s in range(12)]
    check("every slot s0..s11 on e1m3 is kept, with nothing said",
          all(n > 100_000 for n in sizes) and not any("ERROR" in a for a in after),
          f"kept sizes {sizes}, console {[a for a in after if 'ERROR' in a][:1]}")
    asked = pg.evaluate("window.__persistAsked")
    if BROWSER == "firefox":
        check("Firefox is not asked for persistent storage (it would prompt)", asked == 0, f"asked {asked} times")
    else:
        check("persistent storage is asked for once", asked == 1, f"asked {asked} times")

    # --- The connection closed under the page --------------------------------
    pg.evaluate("storage.db.close()")
    out = save(pg, 1)
    check("closed by the page: the save is kept on a reopened connection",
          "done." in out and "ERROR" not in out and kept_len(pg, "id1/s1.sav") > 0, out.strip())

    if BROWSER == "chromium":
        cdp = ctx.new_cdp_session(pg)
        cdp.send("Storage.clearDataForOrigin", {"origin": ORIGIN, "storageTypes": "indexeddb"})
        time.sleep(0.5)
        out = save(pg, 2)
        check("site data cleared by the browser: the save is kept on a reopened connection",
              "ERROR" not in out and kept_len(pg, "id1/s2.sav") > 0 and kept_len(pg, "id1/s0.sav") < 0, out.strip())

    # Another tab opens a newer version: it is not blocked, and this tab's
    # next save is kept in the newer database.
    other = ctx.new_page()
    other.goto(f"{ORIGIN}/icons/icon-192.png")
    newer = other.evaluate("""() => new Promise(res => {
        const probe = indexedDB.open('quake-rs');
        probe.onsuccess = () => {
          const v = probe.result.version + 1;
          probe.result.close();
          const r = indexedDB.open('quake-rs', v);
          r.onblocked = () => res('blocked');
          r.onsuccess = () => { r.result.close(); res('opened ' + v); };
          r.onerror = () => res('error ' + r.error);
        };
        setTimeout(() => res('no answer'), 5000);
    })""")
    other.close()
    out = save(pg, 3)
    check("a newer version in another tab: that tab is not blocked, and the save is kept",
          newer.startswith("opened") and "ERROR" not in out and kept_len(pg, "id1/s3.sav") > 0, f"{newer}; {out.strip()}")

    # A write the browser refuses at once (put() throws: a view on shared
    # memory cannot be stored): the console names the error, in one piece.
    pg.evaluate("storage.apply({ op: 'write', path: 'id1/shared.sav', data: new Uint8Array(new SharedArrayBuffer(8)) })")
    time.sleep(1.0)
    out = console_after(pg, "keep id1/shared.sav")
    text = console_text(pg)
    check("a refused write names its error on the console",
          "ERROR: couldn't keep id1/shared.sav (DataCloneError" in text
          and "This session still has id1/shared.sav, but a reload loses it." in out, text[-300:])
    ctx.close()

    # --- A full store -------------------------------------------------------
    ctx = br.new_context(viewport=vp)
    pg = ctx.new_page()
    if BROWSER == "chromium":
        ctx.new_cdp_session(pg).send("Storage.overrideQuotaForOrigin", {"origin": ORIGIN, "quotaSize": QUOTA_KB * 1024})
    pg = open_game(ctx, pg)
    # Fill it to the brim with what does not compress: 256 KB at a time
    # until one does not fit, then smaller, down to 1 KB.
    filled = pg.evaluate("""async () => {
        let n = 0, last = 'room left';
        for (const size of [256, 64, 16, 4, 1].map(k => k * 1024)) {
          while (n < 200) {
            const b = new Uint8Array(size);
            for (let o = 0; o < size; o += 65536) crypto.getRandomValues(b.subarray(o, Math.min(size, o + 65536)));
            try { await storage.put('id1/filler' + n, b); n++; } catch (e) { last = e.name; break; }
          }
        }
        return [n, last];
    }""")
    # Firefox's database file can still have a page or two to spare (a save
    # compresses well): save until the store refuses one.
    for _ in range(6):
        out = save(pg, 5)
        if "This session still has" in out:
            break
    text = console_text(pg)
    check("full: the store did fill", filled[1] == "QuotaExceededError", str(filled))
    check("full: the console names QuotaExceededError, the usage, what to do, and that a reload loses it",
          "ERROR: couldn't keep id1/s5.sav (QuotaExceededError" in out
          and "storage for this site is full (it uses" in out and "Free some disk space" in out
          and "This session still has id1/s5.sav, but a reload loses it." in out, out.strip() or text[-300:])
    exec_line(pg, "load s5")
    time.sleep(1.5)
    loaded = console_after(pg, "Loading game from s5.sav...")
    check("full: the save still loads in this session",
          "Loading game from s5.sav..." in console_text(pg) and "ERROR" not in loaded, loaded.strip())
    pg.evaluate("Promise.all(Array.from({ length: 200 }, (_, i) => storage.remove('id1/filler' + i)))")
    # (Firefox gives the room back once it has deleted the files behind the
    # values, a moment after the transaction.)
    isolated.wait_until(pg, f"navigator.storage.estimate().then(e => e.usage < {QUOTA_KB * 1024 // 2})", 15, raising=False)
    out = save(pg, 5)
    check("full, then room again: the next save is kept", "ERROR" not in out and kept_len(pg, "id1/s5.sav") > 0, out.strip())
    ctx.close()

    # --- No IndexedDB: localStorage, full -------------------------------------
    ctx = br.new_context(viewport=vp)
    ctx.add_init_script(NO_IDB)
    pg = open_game(ctx)
    filled = pg.evaluate("""() => {
        let n = 0, last = 'room left';
        for (const size of [256, 64, 16, 4, 1].map(k => k * 1024)) {
          const chunk = 'x'.repeat(size);
          while (n < 400) {
            try { localStorage.setItem('filler' + n, chunk); n++; } catch (e) { last = e.name; break; }
          }
        }
        return [storage.fallback, n, last];
    }""")
    out = save(pg, 6)
    check("private mode: the page is on its localStorage fallback, and it filled",
          filled[0] is True and filled[2] == "QuotaExceededError", str(filled))
    check("private mode, full: the console says it is the browser's private or limited mode",
          "ERROR: couldn't keep id1/s6.sav (QuotaExceededError" in out and "private or limited mode" in out
          and "This session still has id1/s6.sav, but a reload loses it." in out, out.strip())
    ctx.close()
    br.close()
httpd.shutdown()

if fails:
    print("FAIL:", "; ".join(fails))
    raise SystemExit(1)
print(f"done: the page keeps the program's files through a closed connection, and says why when it cannot ({BROWSER})")

// apf.js — the bridge between a plugin's web UI and the plugin.
//
//   <script src="/__apf/apf.js"></script>
//   <input type="range" data-param="cutoff">       a slider bound to a parameter
//   <select data-param="wave"></select>            options are filled in
//   <input type="checkbox" data-param="bypass">
//   <span data-param-text="cutoff"></span>         the value as text ("1.20 kHz")
//   <span data-param-name="cutoff"></span>         the parameter's name
//   <span data-param-cc="cutoff"></span>           the MIDI controller learned for it ("CC 21")
//   <div data-apf-presets></div>                   a preset bar: list, previous/next, save, delete
//
// Right-click (or ctrl-click) any bound control for MIDI learn: the next controller moved
// on any MIDI channel moves that parameter, in every host, also without a DAW integration.
//
// Script API: apf.ready (a promise), apf.param(key), apf.on(key, fn(value, text)),
// apf.begin(key), apf.set(key, normalized), apf.end(key), apf.bind(rootElement),
// apf.presets.{list(), load(id), save(name, overwrite, meta), remove(id), current, on(fn)},
//   list() resolves to {presets: [{id, name, factory, type, subtype, mood: [words], description,
//   author}], dir}: factory presets in the plugin's order, then the user's by name; meta for
//   save is {type, subtype, mood: [words], description, author}, every member optional.
// apf.learn(key | null), apf.forget(key), apf.onSession(fn(session)) — session also holds
// controllers {wheel 0..1, pedal, bend -1..1}: what the player's mod wheel, sustain pedal and
// pitch wheel are doing.
//
// The plugin's own requests (its Controller, /__apf/plugin/<path>):
//   apf.plugin.get(path, query)      GET, query an object or a string; resolves to the parsed JSON
//   apf.plugin.post(path, body)      POST with body as JSON (a string is sent as is)
//   apf.plugin.raw(path, {method, query, body})   resolves to an ArrayBuffer (body: ArrayBuffer,
//                                    typed array, string or object)
//   apf.plugin.revision, apf.plugin.onChange(fn(revision))   the controller's state changed
//                                    (an edit, a preset, a session loaded by the host)
// A reply with an error status rejects with an Error carrying .status and .body (its text).
//
// Resizing (a resizable web view): apf.sizeLimits {min: [w, h], max: [w, h]} or null;
// apf.resize(width, height) asks the host for a new size (it answers with the window's new
// size, the page then gets a resize event); <div data-apf-resize-grip></div> is a corner grip
// that does this while dragged, for hosts that give the window no resize edge of their own
// (Logic's AUv3 windows). It is hidden while the web view is not resizable.
// apf.maximize() asks for the largest size the screen allows; apf.restore() goes back to
// the size before it; <button data-apf-maximize></button> toggles between the two.
//
// Polling: apf.poller(fn, {fast: 33, slow: 100, idle: 250}) calls fn fast while it reports
// activity (returns true; 1 means minor activity, kept at the `light` rate) or wake() is
// called, backs off to slow and then idle, and stops while the page is hidden. Returns {wake(), mark(), stop(), start(), interval, running}.
//
// Shortcuts: apf.keys.claim(["cmd+d", "cmd+z", "cmd+shift+z", "backspace", ...]) — while the
// page has the keyboard focus these go to it before the host's own key commands (⌘D would
// duplicate the track in Logic); call preventDefault() in your keydown handler for the ones
// you take, the rest still reach the host. Keys not claimed stay with the host (⌘S, space).
(() => {
  "use strict";
  const params = [];
  const byKey = new Map();
  const listeners = new Map();
  const sessionListeners = [];
  const editing = new Set();
  let gen = -1;
  // The current preset, the parameter waiting for MIDI learn, the learned controllers.
  const session = { preset: null, learn: -1, cc: [], controllers: { wheel: 0, pedal: false, bend: 0 } };

  const post = (msg) => window.ipc && window.ipc.postMessage(msg);
  const lookup = (key) => {
    const p = byKey.get(key);
    if (!p) console.warn("apf: no parameter", key);
    return p;
  };
  const queryOf = (args) =>
    typeof args === "string" ? args.replace(/^\?/, "") : Object.entries(args || {}).map(([k, v]) => `${k}=${encodeURIComponent(v)}`).join("&");
  // {type, subtype, mood: [words] | "a,b", description, author} as save arguments; members
  // left out stay out (no metadata at all: an overwritten preset keeps its own).
  const metaArgs = (meta) => {
    const out = {};
    if (!meta) return out;
    for (const k of ["type", "subtype", "description", "author"]) if (meta[k] != null) out[k] = String(meta[k]);
    if (meta.mood != null) out.mood = Array.isArray(meta.mood) ? meta.mood.join(",") : String(meta.mood);
    return out;
  };
  const call = async (path, args = {}) => {
    const q = queryOf(args);
    const r = await fetch(`/__apf/${path}${q ? "?" + q : ""}`, { cache: "no-store" });
    return r.json();
  };

  // --- the plugin's controller --------------------------------------------------------------
  let revision = null;
  const revisionListeners = [];
  async function pluginFetch(path, { method = "GET", query = null, body = undefined } = {}) {
    const q = queryOf(query);
    const init = { method, cache: "no-store" };
    if (body !== undefined && body !== null) {
      const binary = body instanceof ArrayBuffer || ArrayBuffer.isView(body) || body instanceof Blob;
      init.body = binary || typeof body === "string" ? body : JSON.stringify(body);
    }
    const r = await fetch(`/__apf/plugin/${String(path).replace(/^\/+/, "")}${q ? "?" + q : ""}`, init);
    if (!r.ok) {
      const text = await r.text().catch(() => "");
      const e = new Error(`apf.plugin: ${method} ${path}: ${r.status} ${text}`);
      e.status = r.status;
      e.body = text;
      throw e;
    }
    return r;
  }
  const json = async (r) => (r.status === 204 ? null : r.json());

  // A listener that throws must not stop the others, nor the polling.
  const safely = (fn, ...args) => {
    try {
      fn(...args);
    } catch (e) {
      apf.log("listener failed:", String(e), e && e.stack);
    }
  };

  function update(p, value, text) {
    if (p.value === value && p.text === text) return;
    p.value = value;
    p.text = text;
    for (const fn of listeners.get(p.key) || []) safely(fn, value, text, p);
  }

  function updateSession(data) {
    const next = { preset: data.preset || null, learn: data.learn, cc: data.cc || [], controllers: data.controllers || session.controllers };
    if (JSON.stringify(next) === JSON.stringify(session)) return;
    Object.assign(session, next);
    for (const fn of sessionListeners) safely(fn, session);
  }

  let restoreSize = null;
  const apf = {
    params,
    session,
    sizeLimits: null,
    resize(width, height) {
      if (!apf.sizeLimits) return false;
      window.ipc.postMessage(`resize ${Math.round(width)} ${Math.round(height)}`);
      return true;
    },
    maximize() {
      if (!apf.sizeLimits) return false;
      if (!restoreSize) restoreSize = [innerWidth, innerHeight];
      window.ipc.postMessage("maximize");
      return true;
    },
    restore() {
      if (!restoreSize) return false;
      const [w, h] = restoreSize;
      restoreSize = null;
      return apf.resize(w, h);
    },
    get maximized() {
      return restoreSize !== null;
    },
    keys: {
      claimed: [],
      claim(list) {
        apf.keys.claimed = Array.from(list || [], String);
        window.ipc.postMessage(`keys ${apf.keys.claimed.join(" ")}`);
      },
    },
    param: (key) => byKey.get(key),
    on(key, fn) {
      if (!listeners.has(key)) listeners.set(key, []);
      listeners.get(key).push(fn);
      const p = byKey.get(key);
      if (p) fn(p.value, p.text, p);
    },
    onSession(fn) {
      sessionListeners.push(fn);
      fn(session);
    },
    begin(key) {
      const p = lookup(key);
      if (!p || editing.has(p.index)) return;
      editing.add(p.index);
      post(`begin ${p.index}`);
    },
    set(key, normalized) {
      const p = lookup(key);
      if (!p) return;
      const v = Math.min(1, Math.max(0, Number(normalized)));
      post(`set ${p.index} ${v}`);
      update(p, v, p.text);
    },
    end(key) {
      const p = lookup(key);
      if (!p || !editing.has(p.index)) return;
      editing.delete(p.index);
      post(`end ${p.index}`);
    },
    // One complete edit (a click on an option, a checkbox).
    change(key, normalized) {
      apf.begin(key);
      apf.set(key, normalized);
      apf.end(key);
    },
    learn(key) {
      const p = key == null ? null : lookup(key);
      return call("learn", { index: p ? p.index : -1 }).then(poll);
    },
    forget(key) {
      const p = lookup(key);
      return p ? call("forget", { index: p.index }).then(poll) : Promise.resolve();
    },
    ccOf(key) {
      const p = byKey.get(key);
      const m = p && session.cc.find(([, i]) => i === p.index);
      return m ? m[0] : null;
    },
    presets: {
      get current() {
        return session.preset;
      },
      list: () => call("presets"),
      load: (id) => call("preset/load", { id }).then((r) => (poll(), r)),
      save: (name, overwrite = false, meta = null) => call("preset/save", { name, overwrite: overwrite ? 1 : 0, ...metaArgs(meta) }).then((r) => (poll(), r)),
      remove: (id) => call("preset/delete", { id }).then((r) => (poll(), r)),
      on: (fn) => apf.onSession((s) => fn(s.preset)),
    },
    plugin: {
      get: (path, query) => pluginFetch(path, { query }).then(json),
      post: (path, body = {}) => pluginFetch(path, { method: "POST", body }).then(json),
      raw: (path, options = {}) => pluginFetch(path, options).then((r) => r.arrayBuffer()),
      get revision() {
        return revision;
      },
      onChange(fn) {
        revisionListeners.push(fn);
      },
    },
    bind,
    // A line in the plugin's log (stderr): hosts show no web console.
    log: (...args) => post(`log ${args.map((a) => (typeof a === "string" ? a : JSON.stringify(a))).join(" ")}`),
  };

  function updateRevision(rev) {
    if (rev === undefined || rev === revision) return;
    revision = rev;
    for (const fn of revisionListeners) safely(fn, rev);
  }

  // Adaptive polling: apf.poller(fn, {fast: 33, slow: 100, idle: 250}). The values poll uses
  // it, and so can a plugin's page that asks its Controller for something over and over (a
  // meter, a spectrum). It calls fn every `fast` ms while there is activity, every `slow`
  // ms once nothing has happened for 1.5 s (`fastFor`), every `idle` ms after 6 s
  // (`slowFor`), and not at all while the page is hidden. Activity is: fn returning true (or
  // a promise of true: "something changed"), wake() or mark(), a `wakeOn` event, the page
  // becoming visible again. fn may return 1 for minor activity (a blinking light): that
  // keeps the poll at `light` ms (default: `slow`) for `fastFor` ms but is not full activity. Each poll can be a request into the plugin, and on Windows it
  // crosses from WebView2's process into the host's: 30 a second cost ~12 % of a core.
  function poller(fn, { fast = 33, slow = 100, idle = 250, light = slow, fastFor = 1500, slowFor = 6000, wakeOn = [], start = true } = {}) {
    let quietSince = performance.now();
    let lightSince = -Infinity;
    let timer = null;
    let busy = false;
    let running = false;
    let failed = false;
    const interval = () => {
      const now = performance.now();
      const quiet = now - quietSince;
      if (quiet < fastFor) return fast;
      if (now - lightSince < fastFor) return light;
      return quiet < slowFor ? slow : idle;
    };
    function schedule(ms) {
      clearTimeout(timer);
      timer = setTimeout(run, ms);
    }
    async function run() {
      timer = null;
      if (!running || document.hidden) return; // visibilitychange starts it again
      busy = true;
      try {
        const activity = await fn();
        if (activity === 1) lightSince = performance.now();
        else if (activity) quietSince = performance.now();
      } catch (e) {
        if (!failed) apf.log("poller: fn failed:", String(e)); // once; it is tried again
        failed = true;
      }
      busy = false;
      if (running) schedule(interval());
    }
    // Note activity without polling sooner.
    const mark = () => {
      quietSince = performance.now();
    };
    // Note activity, and poll now if the poll had slowed down (or stopped for a hidden page).
    function wake() {
      const was = interval();
      mark();
      if (running && !busy && !document.hidden && (was > fast || timer === null)) schedule(0);
    }
    const onVisible = () => !document.hidden && wake();
    const stop = () => {
      running = false;
      clearTimeout(timer);
      timer = null;
      for (const ev of wakeOn) window.removeEventListener(ev, wake, { capture: true });
      document.removeEventListener("visibilitychange", onVisible);
    };
    const begin = () => {
      if (running) return;
      running = true;
      mark();
      for (const ev of wakeOn) window.addEventListener(ev, wake, { passive: true, capture: true });
      document.addEventListener("visibilitychange", onVisible);
      schedule(0);
    };
    if (start) begin();
    return {
      wake,
      mark,
      stop,
      start: begin,
      get interval() {
        return interval();
      },
      get running() {
        return running;
      },
    };
  }
  apf.poller = poller;

  // The values poll: every 33 ms while values change or the user works in the page, slower
  // once nothing has changed for a while, none while the page is hidden.
  let valuesPoller = null;

  async function poll() {
    try {
      const r = await fetch(`/__apf/values?gen=${gen}${revision === null ? "" : `&rev=${revision}`}`, { cache: "no-store" });
      if (r.status !== 200) return false;
      if (valuesPoller) valuesPoller.mark();
      const data = await r.json();
      gen = data.gen;
      updateRevision(data.rev);
      data.values.forEach(([value, text], i) => {
        const p = params[i];
        // While the user drags a control, keep its value; still refresh the text.
        update(p, editing.has(i) ? p.value : value, text);
      });
      updateSession(data);
      return true;
    } catch (e) {
      /* the web view is closing */
      return false;
    }
  }

  function bindRange(el, p) {
    el.min = 0;
    el.max = 1;
    el.step = p.steps > 0 ? 1 / p.steps : "any";
    el.addEventListener("pointerdown", (e) => e.button === 0 && !e.ctrlKey && apf.begin(p.key));
    el.addEventListener("input", () => {
      apf.begin(p.key);
      apf.set(p.key, el.value);
    });
    const end = () => apf.end(p.key);
    el.addEventListener("pointerup", end);
    el.addEventListener("change", end);
    el.addEventListener("keyup", end);
    el.addEventListener("dblclick", () => apf.change(p.key, p.default));
    apf.on(p.key, (v) => {
      if (!editing.has(p.index)) el.value = v;
    });
  }

  function bindSelect(el, p) {
    if (!el.options.length && p.options) {
      p.options.forEach((name, i) => el.add(new Option(name, String(i))));
    }
    el.addEventListener("change", () => apf.change(p.key, p.steps ? Number(el.value) / p.steps : 0));
    apf.on(p.key, (v) => (el.value = String(Math.round(v * p.steps))));
  }

  function bindCheckbox(el, p) {
    el.addEventListener("change", () => apf.change(p.key, el.checked ? 1 : 0));
    apf.on(p.key, (v) => (el.checked = v >= 0.5));
  }

  function bind(root) {
    root.querySelectorAll("[data-param]").forEach((el) => {
      const p = lookup(el.dataset.param);
      if (!p) return;
      if (el.tagName === "SELECT") bindSelect(el, p);
      else if (el.type === "checkbox") bindCheckbox(el, p);
      else if (el.type === "range") bindRange(el, p);
      bindLearn(el, p.key);
    });
    root.querySelectorAll("[data-param-learn]").forEach((el) => bindLearn(el, el.dataset.paramLearn));
    root.querySelectorAll("[data-param-text]").forEach((el) => {
      apf.on(el.dataset.paramText, (_v, text) => (el.textContent = text));
    });
    root.querySelectorAll("[data-param-name]").forEach((el) => {
      const p = lookup(el.dataset.paramName);
      if (p) el.textContent = p.name;
    });
    root.querySelectorAll("[data-param-cc]").forEach((el) => {
      apf.onSession(() => {
        const cc = apf.ccOf(el.dataset.paramCc);
        el.textContent = cc == null ? "" : `CC ${cc}`;
      });
    });
    root.querySelectorAll("[data-apf-presets]").forEach(presetBar);
    root.querySelectorAll("[data-apf-resize-grip]").forEach(resizeGrip);
    root.querySelectorAll("[data-apf-maximize]").forEach(maximizeButton);
  }

  // A button that fills the screen with the web view and brings it back.
  function maximizeButton(el) {
    el.hidden = !apf.sizeLimits;
    const show = () => {
      el.innerHTML = icon(apf.maximized ? "minimize" : "maximize");
      el.title = apf.maximized ? "Restore size" : "Fill the screen";
      el.setAttribute("aria-label", el.title);
    };
    el.addEventListener("click", () => {
      if (apf.maximized) apf.restore();
      else apf.maximize();
      show();
    });
    show();
  }

  // A corner grip: dragging it asks the host for the size under the pointer, at most once per
  // animation frame.
  function resizeGrip(el) {
    style();
    el.classList.add("apf-grip");
    el.hidden = !apf.sizeLimits;
    el.innerHTML = icon("grip");
    el.title = "Resize";
    let start = null;
    let frame = 0;
    let want = null;
    const send = () => {
      frame = 0;
      if (want) apf.resize(want[0], want[1]);
    };
    el.addEventListener("pointerdown", (e) => {
      if (!apf.sizeLimits || e.button !== 0) return;
      start = { x: e.screenX, y: e.screenY, w: innerWidth, h: innerHeight };
      el.setPointerCapture(e.pointerId);
      e.preventDefault();
    });
    el.addEventListener("pointermove", (e) => {
      if (!start) return;
      const { min, max } = apf.sizeLimits;
      want = [
        Math.min(max[0], Math.max(min[0], start.w + e.screenX - start.x)),
        Math.min(max[1], Math.max(min[1], start.h + e.screenY - start.y)),
      ];
      if (!frame) frame = requestAnimationFrame(send);
    });
    const end = () => (start = null);
    el.addEventListener("pointerup", end);
    el.addEventListener("pointercancel", end);
  }

  // --- small UI kit: line icons, a menu, an inline dialog -----------------------------------

  const ICON = {
    left: '<path d="M15 18l-6-6 6-6"/>',
    right: '<path d="M9 18l6-6-6-6"/>',
    down: '<path d="M6 9l6 6 6-6"/>',
    save: '<path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2z"/><path d="M17 21v-8H7v8"/><path d="M7 3v5h8"/>',
    trash: '<path d="M3 6h18"/><path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/><path d="M10 11v6"/><path d="M14 11v6"/><path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2"/>',
    knob: '<circle cx="12" cy="12" r="8"/><path d="M12 12l3-5"/>',
    x: '<path d="M18 6L6 18"/><path d="M6 6l12 12"/>',
    reset: '<path d="M3 12a9 9 0 1 0 3-6.7"/><path d="M3 4v5h5"/>',
    grip: '<path d="M21 15l-6 6"/><path d="M21 8l-13 13"/>',
    maximize: '<path d="M15 3h6v6"/><path d="M9 21H3v-6"/><path d="M21 3l-7 7"/><path d="M3 21l7-7"/>',
    minimize: '<path d="M4 14h6v6"/><path d="M20 10h-6V4"/><path d="M14 10l7-7"/><path d="M3 21l7-7"/>',
  };
  const icon = (name) =>
    `<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${ICON[name]}</svg>`;

  const STYLE = `
    .apf-presets { display: flex; align-items: center; gap: 6px; position: relative; }
    .apf-btn { display: inline-flex; align-items: center; justify-content: center; gap: 6px; height: 26px; min-width: 26px;
      padding: 0 7px; border-radius: 6px; border: 1px solid var(--apf-line, #2a2d37); background: transparent;
      color: var(--apf-dim, #8b8fa0); font: inherit; cursor: pointer; }
    .apf-btn:hover:not(:disabled) { color: var(--apf-text, #e6e7ec); border-color: var(--apf-accent, #5eb3ff); }
    .apf-btn:disabled { opacity: .35; cursor: default; }
    .apf-name { flex: 1; justify-content: space-between; color: var(--apf-text, #e6e7ec); min-width: 0; }
    .apf-name span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .apf-name em { font-style: normal; color: var(--apf-accent, #5eb3ff); margin-left: 4px; }
    .apf-menu { position: fixed; z-index: 1000; min-width: 180px; max-height: 70vh; overflow: auto; padding: 4px;
      background: var(--apf-panel, #1b1d24); border: 1px solid var(--apf-line, #2a2d37); border-radius: 8px;
      box-shadow: 0 8px 24px rgba(0,0,0,.45); color: var(--apf-text, #e6e7ec); }
    .apf-menu .apf-head { padding: 6px 10px 2px; font-size: 10px; text-transform: uppercase; letter-spacing: .08em; color: var(--apf-dim, #8b8fa0); }
    .apf-menu .apf-item { display: flex; align-items: center; gap: 8px; width: 100%; padding: 6px 10px; border: 0; border-radius: 5px;
      background: transparent; color: inherit; font: inherit; text-align: left; cursor: pointer; }
    .apf-menu .apf-item:hover, .apf-menu .apf-item.apf-on { background: var(--apf-accent-dim, #2b5d87); }
    .apf-menu .apf-empty { padding: 6px 10px; color: var(--apf-dim, #8b8fa0); }
    .apf-dialog { position: fixed; inset: 0; z-index: 1001; display: flex; align-items: center; justify-content: center; background: rgba(0,0,0,.45); }
    .apf-dialog > div { width: min(320px, 90vw); padding: 14px; border-radius: 10px; background: var(--apf-panel, #1b1d24);
      border: 1px solid var(--apf-line, #2a2d37); color: var(--apf-text, #e6e7ec); }
    .apf-dialog p { margin: 0 0 10px; }
    .apf-dialog input { width: 100%; box-sizing: border-box; height: 28px; padding: 0 8px; border-radius: 6px; font: inherit;
      border: 1px solid var(--apf-line, #2a2d37); background: var(--apf-bg, #121318); color: inherit; user-select: text; -webkit-user-select: text; }
    .apf-dialog .apf-row { display: flex; justify-content: flex-end; gap: 6px; margin-top: 12px; }
    .apf-dialog .apf-error { color: #ff8a80; min-height: 1.2em; margin-top: 6px; font-size: 12px; }
    .apf-dialog .apf-primary { color: var(--apf-text, #e6e7ec); border-color: var(--apf-accent, #5eb3ff); }
    .apf-learning { outline: 2px solid var(--apf-accent, #5eb3ff); outline-offset: 3px; border-radius: 4px; animation: apf-pulse 1s ease-in-out infinite; }
    @keyframes apf-pulse { 50% { outline-color: transparent; } }
    .apf-grip { position: fixed; right: 0; bottom: 0; z-index: 998; display: flex; align-items: flex-end; justify-content: flex-end;
      width: 18px; height: 18px; padding: 2px; box-sizing: border-box; color: var(--apf-dim, #8b8fa0); cursor: nwse-resize; touch-action: none; }
    .apf-grip:hover { color: var(--apf-text, #e6e7ec); }
    .apf-grip[hidden] { display: none; }
    .apf-banner { position: fixed; left: 50%; bottom: 10px; transform: translateX(-50%); z-index: 999; display: flex; align-items: center; gap: 8px;
      padding: 6px 8px 6px 12px; border-radius: 8px; background: var(--apf-accent-dim, #2b5d87); color: var(--apf-text, #e6e7ec);
      box-shadow: 0 4px 16px rgba(0,0,0,.4); }
  `;
  let styled = false;
  function style() {
    if (styled) return;
    styled = true;
    const s = document.createElement("style");
    s.textContent = STYLE;
    document.head.prepend(s);
  }

  let openMenu = null;
  function closeMenu() {
    if (openMenu) openMenu.remove();
    openMenu = null;
  }
  document.addEventListener("pointerdown", (e) => openMenu && !openMenu.contains(e.target) && closeMenu(), true);
  document.addEventListener("keydown", (e) => {
    if (e.key !== "Escape") return;
    if (openMenu) closeMenu();
    else if (session.learn >= 0) apf.learn(null);
  });

  // Windows: the webview gets every key while it has the focus, and the host none. As on the
  // Mac, where the host sees a key before the page, the keys the page did not claim
  // (apf.keys.claim) go on to the host's window, so its own shortcuts work over the web view too
  // (space plays, Ctrl+S saves); a claimed key only when the page did not take it
  // (preventDefault). Nothing typed into a text field goes on. Checked after the whole
  // dispatch, when every handler of the page has had it. "cmd" in a claim is Ctrl here.
  if (/^Win/.test(navigator.platform)) {
    const forwarded = new Set();
    const field = (t) => t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName));
    const NAMES = { " ": "space", arrowleft: "left", arrowright: "right", arrowup: "up", arrowdown: "down", del: "delete", esc: "escape", return: "enter" };
    const name = (k) => {
      const l = String(k).toLowerCase();
      return NAMES[l] || l;
    };
    const claimed = (e) => {
      const key = name(e.key);
      return apf.keys.claimed.some((c) => {
        const parts = c.toLowerCase().endsWith("++") ? [...c.toLowerCase().slice(0, -2).split("+"), "+"] : c.toLowerCase().split("+");
        const want = { ctrl: false, shift: false, alt: false, key: "" };
        for (const p of parts) {
          if (["cmd", "command", "meta", "ctrl", "control"].includes(p)) want.ctrl = true;
          else if (p === "shift") want.shift = true;
          else if (["alt", "option", "opt"].includes(p)) want.alt = true;
          else want.key = name(p);
        }
        // Shift belongs to a printable character: a claim without it matches either.
        const shiftOk = want.shift === e.shiftKey || (!want.shift && want.key.length === 1);
        return want.key === key && want.ctrl === (e.ctrlKey || e.metaKey) && want.alt === e.altKey && shiftOk;
      });
    };
    const forward = (e) => {
      if (!e.isTrusted || e.isComposing || !e.keyCode) return;
      const isClaimed = e.type === "keydown" && claimed(e);
      setTimeout(() => {
        if (e.type === "keydown") {
          if (field(e.target) || (isClaimed && e.defaultPrevented)) return;
          forwarded.add(e.keyCode);
        } else if (!forwarded.delete(e.keyCode)) return;
        post(`hostkey ${e.type === "keydown" ? "down" : "up"} ${e.keyCode} ${e.altKey ? 1 : 0}`);
      }, 0);
    };
    // Capture phase: a page handler that stops the event's propagation must not hide it.
    window.addEventListener("keydown", forward, true);
    window.addEventListener("keyup", forward, true);
  }

  // items: [{label, icon?, on?, title?, action} | {head} | {empty}] at (x, y)
  function menu(items, x, y) {
    style();
    closeMenu();
    const m = document.createElement("div");
    m.className = "apf-menu";
    for (const it of items) {
      if (it.head) {
        const h = document.createElement("div");
        h.className = "apf-head";
        h.textContent = it.head;
        m.appendChild(h);
      } else if (it.empty) {
        const h = document.createElement("div");
        h.className = "apf-empty";
        h.textContent = it.empty;
        m.appendChild(h);
      } else {
        const b = document.createElement("button");
        b.className = "apf-item" + (it.on ? " apf-on" : "");
        b.innerHTML = (it.icon ? icon(it.icon) : "") + "<span></span>";
        b.lastChild.textContent = it.label;
        if (it.title) b.title = it.title;
        b.addEventListener("click", () => {
          closeMenu();
          it.action();
        });
        m.appendChild(b);
      }
    }
    document.body.appendChild(m);
    const r = m.getBoundingClientRect();
    m.style.left = `${Math.max(4, Math.min(x, innerWidth - r.width - 4))}px`;
    m.style.top = `${Math.max(4, Math.min(y, innerHeight - r.height - 4))}px`;
    openMenu = m;
  }

  // A modal: a message, optionally a text field; resolves to the text / true, or null.
  function dialog({ message, value = null, ok = "OK", validate = null }) {
    style();
    return new Promise((resolve) => {
      const d = document.createElement("div");
      d.className = "apf-dialog";
      d.innerHTML = `<div><p></p>${value != null ? "<input>" : ""}<div class="apf-error"></div>
        <div class="apf-row"><button class="apf-btn apf-cancel">Cancel</button><button class="apf-btn apf-primary apf-ok"></button></div></div>`;
      d.querySelector("p").textContent = message;
      d.querySelector(".apf-ok").textContent = ok;
      const input = d.querySelector("input");
      const error = d.querySelector(".apf-error");
      const done = async (result) => {
        if (result != null && validate) {
          const msg = await validate(result);
          if (msg) {
            error.textContent = msg;
            return;
          }
        }
        d.remove();
        resolve(result);
      };
      d.querySelector(".apf-cancel").addEventListener("click", () => done(null));
      d.querySelector(".apf-ok").addEventListener("click", () => done(input ? input.value : true));
      d.addEventListener("keydown", (e) => {
        e.stopPropagation();
        if (e.key === "Escape") done(null);
        if (e.key === "Enter") done(input ? input.value : true);
      });
      document.body.appendChild(d);
      if (input) {
        input.value = value;
        input.focus();
        input.select();
      } else d.querySelector(".apf-ok").focus();
    });
  }

  // --- MIDI learn -------------------------------------------------------------------------------

  function bindLearn(el, key) {
    el.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      const p = byKey.get(key);
      if (!p) return;
      const cc = apf.ccOf(key);
      const items = [{ head: p.name }];
      if (session.learn === p.index) items.push({ label: "Cancel MIDI learn", icon: "x", action: () => apf.learn(null) });
      else items.push({ label: "MIDI learn", icon: "knob", action: () => apf.learn(key) });
      if (cc != null) items.push({ label: `Forget CC ${cc}`, icon: "x", action: () => apf.forget(key) });
      items.push({ label: "Default value", icon: "reset", action: () => apf.change(key, p.default) });
      menu(items, e.clientX, e.clientY);
    });
  }

  let banner = null;
  function showLearn(s) {
    document.querySelectorAll(".apf-learning").forEach((el) => el.classList.remove("apf-learning"));
    const p = params[s.learn];
    if (!p) {
      if (banner) banner.remove();
      banner = null;
      return;
    }
    document.querySelectorAll(`[data-param="${p.key}"], [data-param-learn="${p.key}"]`).forEach((el) => el.classList.add("apf-learning"));
    style();
    if (!banner) {
      banner = document.createElement("div");
      banner.className = "apf-banner";
      banner.innerHTML = `${icon("knob")}<span></span><button class="apf-btn" title="Cancel (Esc)">${icon("x")}</button>`;
      banner.querySelector("button").addEventListener("click", () => apf.learn(null));
      document.body.appendChild(banner);
    }
    banner.querySelector("span").textContent = `Move a knob or fader on your controller for ${p.name}`;
  }

  // --- preset bar ------------------------------------------------------------------------------

  function presetBar(root) {
    style();
    root.classList.add("apf-presets");
    root.innerHTML = `
      <button class="apf-btn apf-prev" title="Previous preset">${icon("left")}</button>
      <button class="apf-btn apf-next" title="Next preset">${icon("right")}</button>
      <button class="apf-btn apf-name" title="Presets"><span></span>${icon("down")}</button>
      <button class="apf-btn apf-save" title="Save preset">${icon("save")}</button>
      <button class="apf-btn apf-delete" title="Delete preset">${icon("trash")}</button>`;
    const nameEl = root.querySelector(".apf-name span");
    const del = root.querySelector(".apf-delete");

    apf.presets.on((cur) => {
      nameEl.textContent = cur ? cur.name : "Unsaved sound";
      if (cur && cur.changed) nameEl.insertAdjacentHTML("beforeend", "<em title=\"Changed since loaded\">*</em>");
      del.disabled = !cur || cur.factory;
    });

    const load = async (id) => {
      const r = await apf.presets.load(id);
      if (!r.ok) await dialog({ message: r.error });
    };
    // Factory presets under their types, in the order the types first come (untyped ones
    // under "Factory"), then the user's: the menu's order, and the arrows'.
    const grouped = (presets) => {
      const groups = new Map();
      for (const p of presets.filter((p) => p.factory)) {
        const head = p.type || "Factory";
        if (!groups.has(head)) groups.set(head, []);
        groups.get(head).push(p);
      }
      return { groups, user: presets.filter((p) => !p.factory) };
    };
    const step = async (delta) => {
      const { groups, user } = grouped((await apf.presets.list()).presets);
      const presets = [...[...groups.values()].flat(), ...user];
      if (!presets.length) return;
      const cur = session.preset ? presets.findIndex((p) => p.id === session.preset.id) : -1;
      const next = cur < 0 ? (delta > 0 ? 0 : presets.length - 1) : (cur + delta + presets.length) % presets.length;
      load(presets[next].id);
    };
    root.querySelector(".apf-prev").addEventListener("click", () => step(-1));
    root.querySelector(".apf-next").addEventListener("click", () => step(1));

    root.querySelector(".apf-name").addEventListener("click", async (e) => {
      if (openMenu) return closeMenu();
      // Measure before awaiting: currentTarget is gone once the event has been dispatched.
      const r = e.currentTarget.getBoundingClientRect();
      const { presets } = await apf.presets.list();
      const items = [];
      const entry = (p) => ({
        label: p.name,
        title: p.description || undefined,
        on: session.preset && session.preset.id === p.id,
        action: () => load(p.id),
      });
      const { groups, user } = grouped(presets);
      for (const [head, list] of groups) {
        items.push({ head });
        list.forEach((p) => items.push(entry(p)));
      }
      items.push({ head: "User" });
      if (user.length) user.forEach((p) => items.push(entry(p)));
      else items.push({ empty: "Save a sound to see it here" });
      menu(items, r.left, r.bottom + 4);
    });

    root.querySelector(".apf-save").addEventListener("click", async () => {
      const cur = session.preset;
      const suggested = cur && !cur.factory ? cur.name : cur ? `${cur.name} (edit)` : "";
      await dialog({
        message: "Save the current sound as a preset",
        value: suggested,
        ok: "Save",
        validate: async (text) => {
          const n = text.trim();
          if (!n) return "Give the preset a name.";
          // A new preset made from another is still the same kind of sound: it takes over
          // the type, subtype and mood (not the description).
          let meta = null;
          if (cur) {
            const { presets } = await apf.presets.list();
            const from = presets.find((p) => p.id === cur.id);
            if (from && (from.type || from.subtype || from.mood.length)) meta = { type: from.type, subtype: from.subtype, mood: from.mood };
          }
          const r = await apf.presets.save(n, false, meta);
          if (r.ok) return null;
          if (r.exists) {
            const yes = await dialog({ message: `A preset called “${n}” exists. Replace it?`, ok: "Replace" });
            if (!yes) return "Choose another name, or replace it.";
            const again = await apf.presets.save(n, true);
            return again.ok ? null : again.error;
          }
          return r.error;
        },
      });
    });

    del.addEventListener("click", async () => {
      const cur = session.preset;
      if (!cur || cur.factory) return;
      if (!(await dialog({ message: `Delete the preset “${cur.name}”?`, ok: "Delete" }))) return;
      const r = await apf.presets.remove(cur.id);
      if (!r.ok) await dialog({ message: r.error });
    });
  }

  apf.ready = (async () => {
    const info = await (await fetch("/__apf/info", { cache: "no-store" })).json();
    gen = info.gen;
    if (info.rev !== undefined) revision = info.rev;
    if (info.resize) apf.sizeLimits = info.resize;
    for (const p of info.params) {
      params.push(p);
      byKey.set(p.key, p);
    }
    updateSession(info);
    if (document.readyState === "loading") {
      await new Promise((r) => document.addEventListener("DOMContentLoaded", r, { once: true }));
    }
    bind(document);
    apf.onSession(showLearn);
    valuesPoller = poller(poll, { fast: 33, slow: 100, idle: 250, wakeOn: ["pointerdown", "pointermove", "keydown", "wheel"] });
    return apf;
  })();

  window.apf = apf;
})();

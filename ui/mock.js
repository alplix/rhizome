// A stand-in for the backend, used when the page is opened in an ordinary
// browser instead of the Tauri window. It speaks the same interface as the real
// one (see api.js) and produces the same event shapes, so the whole interface
// can be developed, looked at and tested without connecting to a network.
//
// The demo data is deliberately awkward: markup, dangerous links, Turkish text,
// formatting codes and very long lines are all in there, because those are the
// things the interface has to handle safely and gracefully.

import { fold } from "./lib.js";

const COLOURS = [
  "#ffffff", "#000000", "#00007f", "#009300", "#ff0000", "#7f0000", "#9c009c", "#fc7f00",
  "#ffff00", "#00fc00", "#009393", "#00ffff", "#0000fc", "#ff00ff", "#7f7f7f", "#d2d2d2",
];

// The same decoding the backend does, so the demo shows formatting too.
export function spansFrom(text) {
  const spans = [];
  let style = {};
  let buffer = "";
  const flush = () => {
    if (buffer) spans.push({ text: buffer, ...style });
    buffer = "";
  };
  for (let i = 0; i < text.length; i += 1) {
    const c = text[i];
    const toggle = { "\x02": "bold", "\x1d": "italic", "\x1f": "underline", "\x1e": "strike", "\x11": "mono", "\x16": "reverse" }[c];
    if (toggle) {
      flush();
      style = { ...style, [toggle]: !style[toggle] };
      if (!style[toggle]) delete style[toggle];
    } else if (c === "\x0f") {
      flush();
      style = {};
    } else if (c === "\x03") {
      flush();
      const m = /^(\d{1,2})(?:,(\d{1,2}))?/.exec(text.slice(i + 1));
      if (m) {
        style = { ...style, fg: COLOURS[Number(m[1])] };
        if (m[2] !== undefined && COLOURS[Number(m[2])]) style.bg = COLOURS[Number(m[2])];
        if (!style.fg) delete style.fg;
        i += m[0].length;
      } else {
        delete style.fg;
        delete style.bg;
      }
    } else {
      buffer += c;
    }
  }
  flush();
  return spans;
}

const plainOf = (text) => spansFrom(text).map((s) => s.text).join("");

// Removes accents and case, so "dunya" finds "dünya", as the real search does.
const normal = (s) => s.normalize("NFD").replace(/\p{M}/gu, "").toLowerCase();

function parseQuery(input) {
  const q = { terms: [], from: null, buffer: null };
  for (const m of input.matchAll(/"([^"]*)"|(\S+)/g)) {
    if (m[1] !== undefined) {
      if (m[1].trim()) q.terms.push({ text: normal(m[1].trim()), prefix: false });
      continue;
    }
    const word = m[2];
    const lower = word.toLowerCase();
    if (lower.startsWith("from:")) q.from = word.slice(5) || q.from;
    else if (lower.startsWith("in:")) q.buffer = word.slice(3) || q.buffer;
    else {
      const stripped = word.replace(/\*+$/, "");
      if (/[\p{L}\p{N}_]/u.test(stripped)) q.terms.push({ text: normal(stripped), prefix: stripped !== word });
    }
  }
  return q;
}

function snippetFor(plain, terms) {
  const hay = normal(plain);
  const at = terms.map((t) => hay.indexOf(t.text)).filter((i) => i >= 0).sort((a, b) => a - b)[0] ?? 0;
  const start = Math.max(0, at - 40);
  const end = Math.min(plain.length, at + 80);
  const window = plain.slice(start, end);
  const lower = normal(window);
  const marks = [];
  for (const t of terms) {
    let from = 0;
    for (;;) {
      const i = lower.indexOf(t.text, from);
      if (i === -1) break;
      marks.push([i, i + t.text.length]);
      from = i + t.text.length;
    }
  }
  marks.sort((a, b) => a[0] - b[0]);
  const parts = [];
  let cursor = 0;
  for (const [s, e] of marks) {
    if (s < cursor) continue;
    if (s > cursor) parts.push({ text: window.slice(cursor, s), hit: false });
    parts.push({ text: window.slice(s, e), hit: true });
    cursor = e;
  }
  if (cursor < window.length) parts.push({ text: window.slice(cursor), hit: false });
  const out = start > 0 ? [{ text: "…", hit: false }, ...parts] : parts;
  return end < plain.length ? [...out, { text: "…", hit: false }] : out;
}

// A small deterministic generator, so the demo looks the same every time.
function lcg(seed) {
  let s = seed;
  return () => {
    s = (Math.imul(s, 1664525) + 1013904223) >>> 0;
    return s / 4294967296;
  };
}

const PEOPLE = ["bob", "carol", "dave", "eve", "mert", "zeynep", "kernelhacker"];

const RHIZOME_LINES = [
  "the Tauri window is up, engine and store are wired together now",
  "rusqlite with bundled SQLite means no system library on Windows",
  "FTS5 external content keeps the index small, the text lives once",
  "Merhaba dünya, şu Türkçe hatayı gördüm — İstanbul'dan selamlar",
  "has anyone tried `unicode61 remove_diacritics 2` with Turkish? dunya finds dünya",
  "\x02bold claim\x02: this is \x0304red\x03, \x0312blue on dark\x03, \x1ditalic\x1d and \x1funderlined\x1f",
  "see https://github.com/alplix/rhizome/issues/1 for the tracking issue",
  "the wiki page (https://en.wikipedia.org/wiki/Rust_(programming_language)) covers it.",
  "SASL PLAIN over TLS only, never over plaintext",
  "reconnect uses backoff with jitter so a netsplit does not stampede the server",
  "I pasted the log: `kmalloc_array(n, size, GFP_KERNEL)` returned NULL again",
  "does the token bucket allow a burst of five then one per second?",
  "cursor paging beats offset paging when messages arrive while you scroll",
  "Türkçe karakterler: ç ğ ı ö ş ü — hepsi doğru görünüyor mu?",
];

const KERNEL_LINES = [
  "BUG: kernel NULL pointer dereference at 0000000000000008",
  "here is the backtrace: RIP: 0010:kmalloc_array+0x2a/0x60",
  "did you enable KASAN? it usually points straight at the bad access",
  "bisecting now, the regression is somewhere in the 6.9 merge window",
  "that oops is in the driver, not the scheduler",
  "you can reproduce it with `kmalloc_array` and a size of zero",
  "patch posted to the list: https://lore.kernel.org/lkml/20260925.1234@example/",
];

// Packs an event the way the real backend does, so the demo history and the
// live lines take the same shape.
const eventRow = (over) => ({ kind: "event", spans: [], plain: "", highlight: false, ...over });

function seedHistory(network, now) {
  const rand = lcg(42);
  const rows = [];
  let id = 1;
  const add = (buffer, sender, text, time_ms, extra = {}) =>
    rows.push({
      id: id++,
      network,
      buffer,
      sender,
      kind: "privmsg",
      spans: spansFrom(text),
      plain: plainOf(text),
      time_ms,
      own: sender === "alp",
      highlight: false,
      msgid: `demo-${id}`,
      ...extra,
    });
  const addEvent = (buffer, sender, verb, args, time_ms, own = false) =>
    rows.push({ id: id++, network, buffer, sender, time_ms, own, ...eventRow({ event: { verb, args } }) });

  for (const [buffer, lines] of [["#rhizome", RHIZOME_LINES], ["#kernel", KERNEL_LINES]]) {
    let t = now - 3 * 24 * 3600 * 1000;
    addEvent(buffer, "alp", "join", [], t - 1000, true);
    for (let i = 0; i < 140; i += 1) {
      t += Math.floor(rand() * 40 * 60 * 1000) + 30_000;
      const roll = rand();
      if (roll < 0.06) {
        addEvent(buffer, PEOPLE[Math.floor(rand() * PEOPLE.length)], "join", [], t);
      } else if (roll < 0.1) {
        addEvent(buffer, PEOPLE[Math.floor(rand() * PEOPLE.length)], "quit", ["Ping timeout: 240 seconds"], t);
      }
      const sender = rand() < 0.15 ? "alp" : PEOPLE[Math.floor(rand() * PEOPLE.length)];
      const text = lines[Math.floor(rand() * lines.length)];
      add(buffer, sender, `${text}`, t + 1);
    }
  }
  const t = now - 2 * 3600 * 1000;
  addEvent("#rhizome", "op", "topic", ["Rhizome, a modern IRC client"], t - 60_000);
  add("#rhizome", "eve", '<img src=x onerror=alert(1)> <script>alert(2)</script> <b>not bold</b>', t + 1);
  add("#rhizome", "mert", "javascript:alert(1) file:///C:/Windows/System32/calc.exe ms-msdt:/id and the real one https://example.com/ok.", t + 2);
  add("#rhizome", "bob", "alp: did you capture the \x02backtrace\x02 for the oops?", t + 3, { highlight: true });
  add("#rhizome", "carol", "facepalms at the backtrace", t + 4, { kind: "action", spans: [{ text: "facepalms at the backtrace" }] });
  add("#rhizome", "zeynep", "an unbroken token: " + "x".repeat(180), t + 5);
  add("#rhizome", "dave", "a long sentence that keeps going " + "and going ".repeat(40) + "until it has to wrap.", t + 6);
  addEvent("#rhizome", "kernelhacker", "kick", ["eve", "off-topic"], t + 7);
  add("dave", "dave", "psst, check the backtrace I mailed you", now - 90 * 60 * 1000, { highlight: true });
  return { rows, nextId: id };
}

const DEFAULT_SETTINGS = {
  theme: "system", accent: "theme", density: "comfortable", font_size: "medium",
  language: "auto", time_format: "24h", show_events: true, notifications: true,
};

export function createMock() {
  const NETWORK = "demo";
  const now = Date.now();
  const listeners = new Set();
  const seeded = seedHistory(NETWORK, now);
  const log = seeded.rows;
  let nextId = seeded.nextId;
  let timers = [];
  const running = new Set();
  const saved = new Map(); // remembered passwords, by network id

  // How far each conversation has been read, by folded name.
  const readMs = new Map([
    ["#rhizome", now - 100 * 60 * 1000],
    ["#kernel", now],
    ["dave", now - 2 * 3600 * 1000],
  ]);

  let settings = { ...DEFAULT_SETTINGS };
  try {
    const stored = JSON.parse(localStorage.getItem("rhizome-mock-settings") ?? "null");
    if (stored && typeof stored === "object") settings = { ...settings, ...stored };
  } catch {
    // Settings simply are not remembered in this browser.
  }

  const profiles = [
    { id: "demo", name: "Demo network", host: "irc.demo.invalid", port: 6697, tls: true, nick: "alp", username: "alp", realname: "Alp", channels: ["#rhizome", "#kernel"], sasl_account: null, autoconnect: false },
    { id: "libera", name: "Libera.Chat", host: "irc.libera.chat", port: 6697, tls: true, nick: "alp", username: "alp", realname: "Alp", channels: ["#rhizome"], sasl_account: "alp", autoconnect: false },
  ];

  const emit = (network, event) => {
    for (const cb of [...listeners]) cb({ network, event });
  };
  const later = (ms, fn) => timers.push(setTimeout(fn, ms));

  function record(buffer, sender, text, extra = {}) {
    const row = {
      id: nextId++,
      network: NETWORK,
      buffer,
      sender,
      kind: "privmsg",
      spans: spansFrom(text),
      plain: plainOf(text),
      time_ms: Date.now(),
      own: sender === "alp",
      highlight: false,
      msgid: `demo-live-${nextId}`,
      ...extra,
    };
    log.push(row);
    return row;
  }

  function say(buffer, sender, text, extra) {
    const { id, ...message } = record(buffer, sender, text, extra);
    emit(NETWORK, { type: "message", message });
  }

  // Something that happened: logged and shown as the same line, like the real backend.
  function happen(buffer, sender, verb, args = [], own = false) {
    const row = { id: nextId++, network: NETWORK, buffer, sender, time_ms: Date.now(), own, ...eventRow({ event: { verb, args } }) };
    log.push(row);
    const { id, ...message } = row;
    emit(NETWORK, { type: "message", message });
  }

  const members = {
    "#rhizome": [{ nick: "alp", prefixes: "@" }, { nick: "bob", prefixes: "+" }, { nick: "carol", prefixes: "" }, { nick: "dave", prefixes: "" }, { nick: "eve", prefixes: "" }, { nick: "mert", prefixes: "" }, { nick: "zeynep", prefixes: "" }],
    "#kernel": [{ nick: "alp", prefixes: "" }, { nick: "kernelhacker", prefixes: "@" }, { nick: "bob", prefixes: "" }],
  };

  function join(channel) {
    emit(NETWORK, { type: "joined", channel });
    happen(channel, "alp", "join", [], true);
    emit(NETWORK, { type: "topic", channel, topic: channel === "#kernel" ? "Kernel talk | oops reports welcome" : "Rhizome, a modern IRC client | https://github.com/alplix/rhizome" });
    emit(NETWORK, { type: "names", channel, members: members[channel] ?? [{ nick: "alp", prefixes: "" }] });
  }

  function startChatter() {
    const lines = [...RHIZOME_LINES, ...KERNEL_LINES];
    timers.push(
      setInterval(() => {
        if (!running.has(NETWORK)) return;
        const channel = Math.random() < 0.6 ? "#rhizome" : "#kernel";
        const sender = PEOPLE[Math.floor(Math.random() * PEOPLE.length)];
        const roll = Math.random();
        if (roll < 0.12) {
          emit(NETWORK, { type: "member_joined", channel, nick: "guest42" });
          happen(channel, "guest42", "join");
          return;
        }
        if (roll < 0.2) {
          emit(NETWORK, { type: "member_parted", channel, nick: "guest42", reason: "later" });
          happen(channel, "guest42", "part", ["later"]);
          return;
        }
        const text = Math.random() < 0.15 ? `alp: ${lines[Math.floor(Math.random() * lines.length)]}` : lines[Math.floor(Math.random() * lines.length)];
        say(channel, sender, text, { highlight: text.startsWith("alp:") });
      }, 7000),
    );
  }

  function stop() {
    for (const t of timers) {
      clearTimeout(t);
      clearInterval(t);
    }
    timers = [];
  }

  const key = (b) => fold(b);
  const byTime = (a, b) => a.time_ms - b.time_ms || a.id - b.id;
  const isChat = (r) => r.kind !== "event";

  return {
    mode: "mock",
    startupNotices: async () => ["You are looking at demo data: this page is not running inside the Rhizome application."],
    appInfo: async () => ({ name: "Rhizome", version: "demo", license: "GPL-3.0-or-later", data_dir: "(demo: nothing is saved)" }),
    getSettings: async () => structuredClone(settings),
    saveSettings: async (next) => {
      settings = { ...settings, ...structuredClone(next) };
      try {
        localStorage.setItem("rhizome-mock-settings", JSON.stringify(settings));
      } catch {
        // Not remembered; fine for a demo.
      }
    },
    listProfiles: async () => structuredClone(profiles),
    saveProfile: async (profile) => {
      if (!profile.id || !profile.host || !profile.nick) throw new Error("the profile is incomplete");
      const i = profiles.findIndex((p) => p.id === profile.id);
      if (i === -1) profiles.push(structuredClone(profile));
      else {
        if (profiles[i].sasl_account !== profile.sasl_account) saved.delete(profile.id);
        profiles[i] = structuredClone(profile);
      }
    },
    deleteProfile: async (id) => {
      const i = profiles.findIndex((p) => p.id === id);
      if (i !== -1) profiles.splice(i, 1);
      saved.delete(id);
    },
    hasSavedPassword: async (id) => saved.has(id),
    forgetPassword: async (id) => void saved.delete(id),

    connect: async (id, password, remember = false) => {
      const profile = profiles.find((p) => p.id === id);
      if (!profile) throw new Error(`there is no saved network called ${id}`);
      let pw = password;
      if (profile.sasl_account) {
        if (!pw && saved.has(id)) pw = saved.get(id);
        else if (pw) {
          if (remember) saved.set(id, pw);
          else saved.delete(id);
        }
        if (!pw) throw new Error(`the password for ${profile.sasl_account} is needed to log in`);
      }
      if (running.has(id)) throw new Error(`${id} is already connected`);
      later(0, () => emit(id, { type: "connecting" }));
      later(250, () => emit(id, { type: "connected" }));
      if (pw === "wrong") {
        later(600, () => {
          const forgot = saved.delete(id);
          emit(id, { type: "auth_failed", reason: "SASL authentication failed (904)", forgot_password: forgot });
          emit(id, { type: "disconnected", reason: "SASL authentication failed (904)" });
          emit(id, { type: "closed" });
        });
        return;
      }
      running.add(id);
      later(600, () => {
        emit(id, { type: "registered", nick: profile.nick });
        emit(id, { type: "network", name: "Demo.Net" });
        emit(id, { type: "server", text: "Welcome to the demo network. Nothing here is real." });
        for (const channel of profile.channels) join(channel);
        startChatter();
      });
    },
    disconnect: async (id) => {
      if (!running.has(id)) throw new Error(`${id} is not connected`);
      running.delete(id);
      stop();
      emit(id, { type: "closed" });
    },

    sendMessage: async (network, target, text, kind = "privmsg") => {
      if (!running.has(network)) throw new Error(`${network} is not connected`);
      for (const line of text.split("\n").filter((l) => l.trim())) {
        const { id, ...message } = record(target, "alp", line, kind === "action" ? { kind: "action" } : kind === "notice" ? { kind: "notice" } : {});
        emit(network, { type: "message", message });
      }
    },
    join: async (network, channels) => {
      if (!running.has(network)) throw new Error(`${network} is not connected`);
      for (const c of channels) join(c);
    },
    part: async (network, channel) => {
      emit(network, { type: "parted", channel });
      happen(channel, "alp", "part", [""], true);
    },
    setNick: async (network, nick) => emit(network, { type: "nick_changed", old: "alp", new: nick, channels: Object.keys(members), own: true }),
    raw: async (network, line) => emit(network, { type: "server", text: `(demo) sent: ${line}` }),

    scrollback: async (network, buffer, before, limit) => {
      let rows = log.filter((r) => key(r.buffer) === key(buffer)).sort(byTime);
      if (before) rows = rows.filter((r) => r.time_ms < before.time_ms || (r.time_ms === before.time_ms && r.id < before.id));
      return structuredClone(rows.slice(-limit));
    },
    around: async (id, radius) => {
      const centre = log.find((r) => r.id === id);
      if (!centre) return [];
      const rows = log.filter((r) => key(r.buffer) === key(centre.buffer)).sort(byTime);
      const i = rows.findIndex((r) => r.id === id);
      return structuredClone(rows.slice(Math.max(0, i - radius), i + radius + 1));
    },
    buffers: async () => {
      const names = [...new Set(log.map((r) => r.buffer))];
      return names.map((name) => {
        const rows = log.filter((r) => key(r.buffer) === key(name) && isChat(r));
        const since = readMs.get(key(name)) ?? 0;
        const unread = rows.filter((r) => !r.own && r.time_ms > since);
        return {
          name,
          messages: rows.length,
          last_time_ms: rows.length ? Math.max(...rows.map((r) => r.time_ms)) : null,
          unread: unread.length,
          highlights: unread.filter((r) => r.highlight).length,
        };
      });
    },
    markRead: async (network, buffer, timeMs) => {
      readMs.set(key(buffer), Math.max(readMs.get(key(buffer)) ?? 0, timeMs));
    },
    clearHistory: async (network, buffer) => {
      const before = log.length;
      for (let i = log.length - 1; i >= 0; i -= 1) if (key(log[i].buffer) === key(buffer)) log.splice(i, 1);
      return before - log.length;
    },
    search: async (query, network, newestFirst, limit) => {
      const q = parseQuery(query);
      if (q.terms.length === 0 && !q.from && !q.buffer) return [];
      const hits = log.filter((r) => {
        if (!isChat(r)) return false;
        if (q.from && normal(r.sender) !== normal(q.from)) return false;
        if (q.buffer && key(r.buffer) !== key(q.buffer)) return false;
        const hay = normal(r.plain);
        return q.terms.every((t) => hay.includes(t.text));
      });
      hits.sort((a, b) => (newestFirst || q.terms.length === 0 ? byTime(b, a) : a.plain.length - b.plain.length || byTime(b, a)));
      return hits.slice(0, limit).map((r) => ({ message: structuredClone(r), snippet: q.terms.length ? snippetFor(r.plain, q.terms) : [{ text: r.plain.slice(0, 160), hit: false }] }));
    },
    openUrl: async (url) => {
      // The real backend refuses anything but http(s); so does the demo.
      if (!/^https?:\/\/[^\s/]+/i.test(url)) throw new Error("only http and https addresses can be opened");
      console.info("[demo] would open in the system browser:", url);
      window.__lastOpenedUrl = url;
    },
    notify: async (title, body) => {
      (window.__notifications ??= []).push({ title, body });
      return true;
    },
    onEvent: async (callback) => {
      listeners.add(callback);
      return () => listeners.delete(callback);
    },
  };
}

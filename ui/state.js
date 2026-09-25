// The interface's model of what is going on: networks, the conversations in
// them, and who is in each channel. Events from the backend are folded into it
// by `applyEnvelope`; nothing here touches the DOM, so it is tested with
// `node --test ui/tests`.
//
// The backend is the source of truth for history (the log) and for the engine's
// view of each channel. This model only has to be good enough to draw the
// current screen, and to be rebuilt from the same events.

import { fold, isChannelName, stripFormatting as clean } from "./lib.js";

// The key of the buffer that holds a network's own messages. It cannot collide
// with a channel or a nick, neither of which can be called "*".
export const SERVER = "*";

// The most lines kept per buffer in memory. Older ones are still in the log.
export const MAX_LINES = 1500;

// The usual membership prefixes, most privileged first. Used only to keep a
// member list ordered after a local change; the backend's ordering, which
// follows the network's own PREFIX, is used whenever it sends a full list.
const PREFIX_ORDER = "~&@%+";

export function createState() {
  return {
    networks: new Map(),
    active: null, // { network, key } or null
    notices: [], // problems not tied to a network
    toast: null, // { text, level } for the most recent thing worth interrupting for
  };
}

export function ensureNetwork(state, id, name = id) {
  let net = state.networks.get(id);
  if (!net) {
    net = {
      id,
      name,
      status: "idle", // idle | connecting | connected | registered | waiting | failed
      nick: null,
      retryAt: null,
      buffers: new Map(),
      order: [],
    };
    state.networks.set(id, net);
    ensureBuffer(net, SERVER, "server");
  }
  return net;
}

export function ensureBuffer(net, name, kind) {
  const key = name === SERVER ? SERVER : fold(name);
  let buffer = net.buffers.get(key);
  if (!buffer) {
    buffer = {
      key,
      name: name === SERVER ? net.name : name,
      kind: kind ?? (isChannelName(name) ? "channel" : "query"),
      joined: false,
      topic: null,
      members: [],
      lines: [],
      // How many lines have ever been added. The view compares it with how many it
      // has drawn to know exactly what is new, even when old lines are trimmed off
      // the front at the same time.
      total: 0,
      seen: new Set(),
      unread: 0,
      highlights: 0,
      unreadFrom: null, // index of the first line that arrived while unseen
      loaded: false, // whether stored history has been merged in
      hasMore: true, // whether older history may exist in the log
      context: null, // set while showing a search result instead of the live end
    };
    net.buffers.set(key, buffer);
    net.order.push(key);
  } else if (name !== SERVER) {
    // Show the spelling most recently used.
    buffer.name = name;
  }
  return buffer;
}

export function findBuffer(state, network, key) {
  return state.networks.get(network)?.buffers.get(key) ?? null;
}

export function activeBuffer(state) {
  return state.active ? findBuffer(state, state.active.network, state.active.key) : null;
}

// ---- lines -------------------------------------------------------------------

function lineTime(line) {
  return line.kind === "message" ? line.message.time_ms : line.time;
}

function trim(buffer) {
  const extra = buffer.lines.length - MAX_LINES;
  if (extra <= 0) return;
  buffer.lines.splice(0, extra);
  if (buffer.unreadFrom !== null) buffer.unreadFrom = Math.max(0, buffer.unreadFrom - extra);
  buffer.hasMore = true;
}

function pushLine(state, net, buffer, line, { counts = false, highlight = false } = {}) {
  const visible = state.active?.network === net.id && state.active.key === buffer.key;
  // While a search result is on screen, its lines are a fixed window into the
  // past. Live lines are not added to it (they are in the log, and appear when
  // the person goes back to now), but they still count as unread if unseen.
  if (buffer.context) {
    if (counts && !visible) {
      buffer.unread += 1;
      if (highlight) buffer.highlights += 1;
    }
    return;
  }
  buffer.lines.push(line);
  buffer.total += 1;
  if (counts && !visible) {
    if (buffer.unreadFrom === null) buffer.unreadFrom = buffer.lines.length - 1;
    buffer.unread += 1;
    if (highlight) buffer.highlights += 1;
  }
  trim(buffer);
}

export function addSystem(state, network, key, text, level = "info", now = Date.now()) {
  const net = state.networks.get(network);
  const buffer = net?.buffers.get(key);
  if (!buffer) return;
  pushLine(state, net, buffer, { kind: "system", time: now, text, level });
}

// A system line in every channel we share with someone (for quits and nick
// changes, which the network reports once, not per channel).
function addToChannels(state, net, names, text, level, now) {
  for (const name of names) {
    const buffer = net.buffers.get(fold(name));
    if (buffer) pushLine(state, net, buffer, { kind: "system", time: now, text, level });
  }
}

// ---- members -----------------------------------------------------------------

function rank(member) {
  const top = member.prefixes[0];
  const index = top === undefined ? -1 : PREFIX_ORDER.indexOf(top);
  return index === -1 ? PREFIX_ORDER.length : index;
}

export function sortMembers(members) {
  return members.sort((a, b) => rank(a) - rank(b) || fold(a.nick).localeCompare(fold(b.nick)));
}

function memberIndex(buffer, nick) {
  const key = fold(nick);
  return buffer.members.findIndex((m) => fold(m.nick) === key);
}

function addMember(buffer, nick) {
  if (memberIndex(buffer, nick) === -1) {
    buffer.members.push({ nick, prefixes: "" });
    sortMembers(buffer.members);
  }
}

function removeMember(buffer, nick) {
  const i = memberIndex(buffer, nick);
  if (i !== -1) buffer.members.splice(i, 1);
}

// ---- buffers -----------------------------------------------------------------

// Makes a buffer the one on screen. Leaving a buffer forgets where its unread
// marker was; arriving at one clears its counters but keeps the marker, so the
// place you left off is still visible until you leave again.
export function setActive(state, network, key) {
  const previous = activeBuffer(state);
  if (previous && !(state.active.network === network && state.active.key === key)) {
    previous.unreadFrom = null;
    previous.context = null;
  }
  const buffer = findBuffer(state, network, key);
  if (!buffer) return null;
  state.active = { network, key };
  buffer.unread = 0;
  buffer.highlights = 0;
  return buffer;
}

export function closeBuffer(state, network, key) {
  const net = state.networks.get(network);
  if (!net || key === SERVER) return;
  net.buffers.delete(key);
  net.order = net.order.filter((k) => k !== key);
  if (state.active?.network === network && state.active.key === key) {
    state.active = { network, key: SERVER };
  }
}

const dedupeKey = (m) => m.msgid ?? `${m.time_ms}|${m.sender}|${m.plain}`;

// Merges a page of stored history (oldest first) with the lines already on
// screen. A message that arrived live while the page was loading is also in the
// log, so anything with the same id (or, lacking one, the same time, sender and
// text) is kept once.
export function mergeHistory(buffer, stored, { pageSize = 100 } = {}) {
  const have = new Set(stored.map(dedupeKey));
  const keep = buffer.lines.filter((l) => l.kind !== "message" || !have.has(dedupeKey(l.message)));
  const lines = [...stored.map((message) => ({ kind: "message", message })), ...keep];
  // Interleave system lines with messages by time; the sort is stable, so lines
  // sharing a millisecond keep their order.
  lines.sort((a, b) => lineTime(a) - lineTime(b));
  buffer.lines = lines;
  buffer.seen = new Set(lines.filter((l) => l.kind === "message").map((l) => dedupeKey(l.message)));
  buffer.loaded = true;
  buffer.hasMore = stored.length >= pageSize;
  buffer.unreadFrom = null;
}

// Adds an older page (oldest first) in front of what is shown.
export function prependHistory(buffer, older, { pageSize = 100 } = {}) {
  const fresh = older.filter((m) => !buffer.seen.has(dedupeKey(m)));
  for (const m of fresh) buffer.seen.add(dedupeKey(m));
  buffer.lines = [...fresh.map((message) => ({ kind: "message", message })), ...buffer.lines];
  buffer.hasMore = older.length >= pageSize;
  return fresh.length;
}

// Replaces a buffer's lines with a window around a search result.
export function showContext(buffer, messages, targetId) {
  buffer.lines = messages.map((message) => ({ kind: "message", message }));
  buffer.seen = new Set(messages.map(dedupeKey));
  buffer.context = { targetId };
  buffer.hasMore = true;
  buffer.unreadFrom = null;
}

// Leaves the search-result view; the caller reloads the live end.
export function leaveContext(buffer) {
  buffer.context = null;
  buffer.lines = [];
  buffer.seen = new Set();
  buffer.loaded = false;
}

// ---- events ------------------------------------------------------------------

const MODE_NEEDING_NAMES = /[qaohv]/;

// Folds one event from the backend into the state. Returns a list of effects
// for the caller to carry out (things that need the backend, or the screen).
export function applyEnvelope(state, envelope, now = Date.now()) {
  const effects = [];
  const { event } = envelope;

  if (envelope.network === "") {
    if (event.type === "error") {
      state.notices.push(event.text);
      state.toast = { text: event.text, level: "error" };
    }
    return effects;
  }

  const net = ensureNetwork(state, envelope.network);
  const server = net.buffers.get(SERVER);
  const say = (text, level = "info") =>
    pushLine(state, net, server, { kind: "system", time: now, text, level });

  switch (event.type) {
    case "connecting":
      net.status = "connecting";
      net.retryAt = null;
      say("Connecting…");
      break;
    case "connected":
      net.status = "connected";
      say("Connected, logging in…");
      break;
    case "registered":
      net.status = "registered";
      net.nick = event.nick;
      say(`Connected as ${event.nick}`);
      effects.push({ type: "registered", network: net.id });
      break;
    case "network":
      say(`Network: ${event.name}`);
      break;
    case "disconnected": {
      const willRetry = event.retry_in_ms !== undefined;
      net.status = willRetry ? "waiting" : "failed";
      net.retryAt = willRetry ? now + event.retry_in_ms : null;
      say(
        willRetry
          ? `Disconnected (${event.reason}). Reconnecting in ${Math.max(1, Math.round(event.retry_in_ms / 1000))}s…`
          : `Disconnected (${event.reason}). Not reconnecting.`,
        "error",
      );
      for (const buffer of net.buffers.values()) {
        if (buffer.kind === "channel" && buffer.joined) {
          buffer.joined = false;
          pushLine(state, net, buffer, { kind: "system", time: now, text: "Disconnected", level: "error" });
        }
      }
      break;
    }
    case "closed":
      if (net.status !== "failed") net.status = "idle";
      net.retryAt = null;
      break;

    case "message": {
      const m = event.message;
      const buffer = ensureBuffer(net, m.buffer);
      const key = dedupeKey(m);
      if (buffer.seen.has(key)) break;
      buffer.seen.add(key);
      pushLine(state, net, buffer, { kind: "message", message: m }, { counts: !m.own, highlight: m.highlight });
      break;
    }
    case "server":
      say(clean(event.text));
      break;
    case "ctcp":
      say(`CTCP ${event.command} ${event.reply ? "reply from" : "request from"} ${event.from}`);
      break;

    case "joined": {
      const buffer = ensureBuffer(net, event.channel, "channel");
      buffer.joined = true;
      buffer.members = [];
      pushLine(state, net, buffer, { kind: "system", time: now, text: `You joined ${event.channel}`, level: "quiet" });
      effects.push({ type: "joined", network: net.id, key: buffer.key });
      break;
    }
    case "parted": {
      const buffer = ensureBuffer(net, event.channel, "channel");
      buffer.joined = false;
      buffer.members = [];
      pushLine(state, net, buffer, {
        kind: "system",
        time: now,
        text: `You left ${event.channel}${event.reason ? ` (${clean(event.reason)})` : ""}`,
        level: "quiet",
      });
      break;
    }
    case "kicked": {
      const buffer = ensureBuffer(net, event.channel, "channel");
      buffer.joined = false;
      buffer.members = [];
      pushLine(state, net, buffer, {
        kind: "system",
        time: now,
        text: `${event.by} removed you from ${event.channel}${event.reason ? ` (${clean(event.reason)})` : ""}`,
        level: "error",
      });
      state.toast = { text: `Removed from ${event.channel} by ${event.by}`, level: "error" };
      break;
    }
    case "member_joined": {
      const buffer = ensureBuffer(net, event.channel, "channel");
      addMember(buffer, event.nick);
      pushLine(state, net, buffer, { kind: "system", time: now, text: `→ ${event.nick} joined`, level: "quiet" });
      break;
    }
    case "member_parted": {
      const buffer = ensureBuffer(net, event.channel, "channel");
      removeMember(buffer, event.nick);
      pushLine(state, net, buffer, {
        kind: "system",
        time: now,
        text: `← ${event.nick} left${event.reason ? ` (${clean(event.reason)})` : ""}`,
        level: "quiet",
      });
      break;
    }
    case "member_kicked": {
      const buffer = ensureBuffer(net, event.channel, "channel");
      removeMember(buffer, event.nick);
      pushLine(state, net, buffer, {
        kind: "system",
        time: now,
        text: `${event.nick} was removed by ${event.by}${event.reason ? ` (${clean(event.reason)})` : ""}`,
        level: "info",
      });
      break;
    }
    case "member_quit": {
      for (const name of event.channels) {
        const buffer = net.buffers.get(fold(name));
        if (buffer) removeMember(buffer, event.nick);
      }
      addToChannels(state, net, event.channels, `⇠ ${event.nick} quit${event.reason ? ` (${clean(event.reason)})` : ""}`, "quiet", now);
      break;
    }
    case "nick_changed": {
      for (const name of event.channels) {
        const buffer = net.buffers.get(fold(name));
        if (!buffer) continue;
        const i = memberIndex(buffer, event.old);
        if (i !== -1) buffer.members[i].nick = event.new;
        sortMembers(buffer.members);
      }
      if (event.own) {
        net.nick = event.new;
        say(`You are now known as ${event.new}`);
      }
      addToChannels(
        state,
        net,
        event.channels,
        event.own ? `You are now known as ${event.new}` : `${event.old} is now known as ${event.new}`,
        "quiet",
        now,
      );
      break;
    }
    case "topic": {
      const buffer = ensureBuffer(net, event.channel, "channel");
      buffer.topic = event.topic ? clean(event.topic) : null;
      pushLine(state, net, buffer, {
        kind: "system",
        time: now,
        text: buffer.topic ? `Topic: ${buffer.topic}` : "The topic was cleared",
        level: "info",
      });
      break;
    }
    case "names": {
      const buffer = ensureBuffer(net, event.channel, "channel");
      buffer.members = event.members.map((m) => ({ nick: m.nick, prefixes: m.prefixes ?? "" }));
      break;
    }
    case "mode": {
      const channel = isChannelName(event.target) ? net.buffers.get(fold(event.target)) : null;
      const text = `${event.by} sets mode ${event.modes}`;
      if (channel) {
        pushLine(state, net, channel, { kind: "system", time: now, text, level: "quiet" });
        // Someone gained or lost a privilege. The engine tracks it, but tells us
        // only the mode string, so ask for the fresh list rather than guess.
        if (MODE_NEEDING_NAMES.test(event.modes.split(" ")[0])) {
          effects.push({ type: "refresh_names", network: net.id, channel: channel.name });
        }
      } else {
        say(text);
      }
      break;
    }

    case "error":
      say(`${event.code ? `${event.code}: ` : ""}${event.text}`, "error");
      state.toast = { text: event.text, level: "error" };
      break;
    case "server_error":
      say(clean(event.text), "error");
      break;
    case "auth_failed":
      say(`Login failed: ${event.reason}`, "error");
      state.toast = { text: `Login failed: ${event.reason}`, level: "error" };
      break;
    default:
      break;
  }
  return effects;
}

// Pure helpers for the interface. Nothing in here touches the DOM, so all of it
// is tested with `node --test ui/tests`.

// ---- names -----------------------------------------------------------------

// IRC compares names under RFC 1459 rules, in which `[]\~` are the upper-case
// forms of `{}|^`. Using toLowerCase() alone would treat `Nick[` and `nick{` as
// different people on the many networks where they are the same.
const RFC1459 = { "[": "{", "]": "}", "\\": "|", "~": "^" };

export function fold(name) {
  return name.replace(/[A-Z[\]\\~]/g, (c) => RFC1459[c] ?? c.toLowerCase());
}

export function isChannelName(name) {
  return /^[#&+!]/.test(name);
}

// ---- formatting codes ------------------------------------------------------

// Removes IRC's in-band formatting (bold, colour and the rest) from text that is
// shown as a single plain line: a topic, a quit reason, server notices. Message
// bodies are styled by the backend instead; this is for everything else, where a
// stray control character would otherwise be drawn as a box.
const FORMATTING = /[\x02\x0f\x11\x16\x1d\x1e\x1f]|\x03(?:\d{1,2}(?:,\d{1,2})?)?|\x04(?:[0-9a-fA-F]{6}(?:,[0-9a-fA-F]{6})?)?/g;

export function stripFormatting(text) {
  return text.replace(FORMATTING, "");
}

// ---- links -----------------------------------------------------------------

const URL_CANDIDATE = /https?:\/\/[^\s<>"'` ]+/gi;
const TRAILING_PUNCTUATION = /[.,;:!?'"*_~]$/;
const PAIRS = { ")": "(", "]": "[", "}": "{" };

function count(text, char) {
  let n = 0;
  for (const c of text) if (c === char) n += 1;
  return n;
}

// Removes what surrounds a URL in prose: a full stop after it, or the closing
// parenthesis of "(see https://example.com)". A closing bracket that has its own
// opening bracket inside the URL, as in a Wikipedia address, is kept.
function trimUrl(candidate) {
  let url = candidate;
  for (;;) {
    if (TRAILING_PUNCTUATION.test(url)) {
      url = url.slice(0, -1);
      continue;
    }
    const last = url.at(-1);
    if (last in PAIRS && count(url, last) > count(url, PAIRS[last])) {
      url = url.slice(0, -1);
      continue;
    }
    return url;
  }
}

// Whether a string is a web address safe to hand to the system browser. Only
// http and https qualify: a link is untrusted input, and `javascript:`, `file:`
// or a registered app scheme must never be opened on someone's behalf.
export function isSafeWebUrl(text) {
  try {
    const url = new URL(text);
    return (url.protocol === "http:" || url.protocol === "https:") && url.hostname !== "";
  } catch {
    return false;
  }
}

// Splits text into plain and link segments: [{ text }, { text, url }, ...].
// The segments always concatenate back to the original text.
export function linkify(text) {
  const segments = [];
  let last = 0;
  for (const match of text.matchAll(URL_CANDIDATE)) {
    const url = trimUrl(match[0]);
    if (!isSafeWebUrl(url)) continue;
    if (match.index > last) segments.push({ text: text.slice(last, match.index) });
    segments.push({ text: url, url });
    last = match.index + url.length;
  }
  if (last < text.length) segments.push({ text: text.slice(last) });
  return segments;
}

// ---- inline code -----------------------------------------------------------

// Splits text at `backtick` spans, as developers write code in chat:
// [{ text }, { text, code: true }, ...]. The backticks themselves are dropped.
// An unmatched backtick, or one with nothing between a pair, is ordinary text,
// and a span never crosses a line break.
export function splitInlineCode(text) {
  const parts = [];
  let last = 0;
  for (const match of text.matchAll(/`([^`\n]+)`/g)) {
    if (match.index > last) parts.push({ text: text.slice(last, match.index) });
    parts.push({ text: match[1], code: true });
    last = match.index + match[0].length;
  }
  if (last < text.length) parts.push({ text: text.slice(last) });
  return parts;
}

// ---- colour ----------------------------------------------------------------

function parseHex(hex) {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex);
  if (!m) return null;
  const n = parseInt(m[1], 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

function toHex([r, g, b]) {
  return "#" + [r, g, b].map((v) => Math.round(v).toString(16).padStart(2, "0")).join("");
}

function luminance([r, g, b]) {
  const channel = (v) => {
    const s = v / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
}

// WCAG contrast ratio, from 1 (identical) to 21 (black on white).
export function contrastRatio(a, b) {
  const [la, lb] = [luminance(parseHex(a)), luminance(parseHex(b))];
  const [hi, lo] = la > lb ? [la, lb] : [lb, la];
  return (hi + 0.05) / (lo + 0.05);
}

// IRC colours are picked with a white or black background in mind, so red text
// can land on a dark theme where it is unreadable. This moves a colour toward
// the opposite extreme just far enough to be legible on `background`, keeping
// its hue.
export function readableOn(foreground, background, minimum = 3.5) {
  const fg = parseHex(foreground);
  const bg = parseHex(background);
  if (!fg || !bg) return foreground;
  const target = luminance(bg) < 0.4 ? [255, 255, 255] : [0, 0, 0];
  let current = fg;
  for (let step = 0; step <= 20; step += 1) {
    if (contrastRatio(toHex(current), background) >= minimum) return toHex(current);
    current = current.map((v, i) => v + (target[i] - v) * 0.15);
  }
  return toHex(target);
}

// A stable hue for a nick, so a person keeps one colour. Case-folded, so
// `Alp` and `alp` match.
export function nickHue(nick) {
  let hash = 2166136261;
  for (const c of fold(nick)) {
    hash ^= c.codePointAt(0);
    hash = Math.imul(hash, 16777619);
  }
  return (hash >>> 0) % 360;
}

// ---- time ------------------------------------------------------------------

const pad = (n) => String(n).padStart(2, "0");

// `hour12` shows "9:05 PM" instead of "21:05".
export function formatTime(ms, hour12 = false) {
  const d = new Date(ms);
  if (!hour12) return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  return `${d.getHours() % 12 || 12}:${pad(d.getMinutes())} ${d.getHours() < 12 ? "AM" : "PM"}`;
}

// A byte count as a person would say it: whole numbers below 1000, one
// decimal place above, the smallest unit that keeps the number under 1000.
export function formatBytes(n) {
  if (n < 1000) return `${n} B`;
  const units = ["kB", "MB", "GB", "TB"];
  let value = n / 1000;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
}

export function sameDay(a, b) {
  const [x, y] = [new Date(a), new Date(b)];
  return x.getFullYear() === y.getFullYear() && x.getMonth() === y.getMonth() && x.getDate() === y.getDate();
}

// `locale` is a BCP 47 tag such as "tr-TR"; undefined uses the browser's own.
export function formatDay(ms, locale) {
  return new Date(ms).toLocaleDateString(locale, {
    weekday: "long",
    day: "numeric",
    month: "long",
    year: "numeric",
  });
}

// ---- what the person typed -------------------------------------------------

// The commands, for /help: [what to type, translation key of what it does].
export const HELP = [
  ["/join #channel[,#other]", "help.join"],
  ["/part [#channel] [reason]", "help.part"],
  ["/me text", "help.me"],
  ["/msg nick text", "help.msg"],
  ["/nick newnick", "help.nick"],
  ["/topic [text]", "help.topic"],
  ["/whois nick", "help.whois"],
  ["/search words", "help.search"],
  ["/clear", "help.clear"],
  ["/dcc send nick path", "help.dcc"],
  ["/quit", "help.quit"],
  ["/raw LINE", "help.raw"],
  ["//text", "help.escape"],
];

// Turns a line the person typed into an action. `context` is
// { buffer, isChannel } for the buffer they typed it in.
//
// An unknown /command is reported, never sent: a typo such as `/joinn #x`
// would otherwise be posted to the channel as chat.
export function parseInput(line, context = {}) {
  const text = line.replace(/\s+$/, "");
  if (text.trim() === "") return null;

  if (text.startsWith("//")) return { type: "message", text: text.slice(1) };
  if (!text.startsWith("/")) return { type: "message", text };

  const [, name, rest = ""] = /^\/(\S*)\s*([\s\S]*)$/.exec(text);
  const command = name.toLowerCase();
  const args = rest.trim();
  const words = args === "" ? [] : args.split(/\s+/);
  // Errors carry a translation key and its parameters, never prose, so the
  // interface can say them in the person's language.
  const need = (usage) => ({ type: "error", key: "err.usage", params: { usage } });
  const channelOnly = (command) => ({ type: "error", key: "err.channel_only", params: { command } });
  const inBuffer = context.buffer && context.buffer !== "*";

  switch (command) {
    case "join":
    case "j":
      if (words.length === 0) return need("/join #channel[,#other]");
      return { type: "join", channels: words[0].split(",").filter(Boolean) };
    case "part":
    case "leave": {
      if (words.length && isChannelName(words[0])) {
        return { type: "part", channel: words[0], reason: words.slice(1).join(" ") || null };
      }
      if (!inBuffer || !context.isChannel) return need("/part #channel [reason]");
      return { type: "part", channel: context.buffer, reason: args || null };
    }
    case "me":
      return args === "" ? need("/me text") : { type: "action", text: args };
    case "msg":
    case "query":
    case "m": {
      if (words.length === 0) return need("/msg nick text");
      const target = words[0];
      const body = rest.trim().slice(target.length).trim();
      return { type: "query", target, text: body || null };
    }
    case "notice": {
      if (words.length < 2) return need("/notice target text");
      return { type: "notice", target: words[0], text: rest.trim().slice(words[0].length).trim() };
    }
    case "nick":
      return words.length === 1 ? { type: "nick", nick: words[0] } : need("/nick newnick");
    case "quit":
    case "disconnect":
      return { type: "quit" };
    case "raw":
    case "quote":
      return args === "" ? need("/raw LINE") : { type: "raw", line: args };
    case "search":
    case "find":
      return { type: "search", query: args };
    case "topic":
      if (!inBuffer || !context.isChannel) return channelOnly("/topic");
      return { type: "raw", line: args === "" ? `TOPIC ${context.buffer}` : `TOPIC ${context.buffer} :${args}` };
    case "whois":
      return words.length === 1 ? { type: "raw", line: `WHOIS ${words[0]}` } : need("/whois nick");
    case "names":
      if (!inBuffer || !context.isChannel) return channelOnly("/names");
      return { type: "raw", line: `NAMES ${context.buffer}` };
    case "kick": {
      if (!inBuffer || !context.isChannel) return channelOnly("/kick");
      if (words.length === 0) return need("/kick nick [reason]");
      const reason = args.slice(words[0].length).trim();
      return { type: "raw", line: reason ? `KICK ${context.buffer} ${words[0]} :${reason}` : `KICK ${context.buffer} ${words[0]}` };
    }
    case "mode": {
      if (words.length === 0) return inBuffer ? { type: "raw", line: `MODE ${context.buffer}` } : need("/mode target [modes]");
      // `+` starts a channel name on networks with modeless channels but also
      // starts every mode string ("+o"), so it cannot be what tells a target
      // from modes here. `+o bob` means "op bob in this channel".
      const namesATarget = /^[#&!]/.test(words[0]);
      const line = namesATarget || !inBuffer || !context.isChannel ? args : `${context.buffer} ${args}`;
      return { type: "raw", line: `MODE ${line}` };
    }
    case "away":
      return { type: "raw", line: args === "" ? "AWAY" : `AWAY :${args}` };
    case "clear":
      return inBuffer
        ? { type: "clear" }
        : { type: "error", key: "err.conversation_only", params: { command: "/clear" } };
    case "dcc": {
      const usage = () => need("/dcc send nick path");
      if (words.length < 3 || words[0].toLowerCase() !== "send") return usage();
      const target = words[1];
      const path = rest.trim().slice(4).trim().slice(target.length).trim();
      return path ? { type: "dcc_send", target, path } : usage();
    }
    case "help":
    case "?":
      return { type: "help" };
    default:
      return { type: "error", key: "err.unknown_command", params: { name } };
  }
}

// ---- nick completion -------------------------------------------------------

// Completes the nick under the caret when Tab is pressed, and cycles through
// the matches on repeated presses. `previous` is what the last call returned as
// `state`, or null. Returns { text, caret, state }, or null if nothing matches.
//
// The text is always `head + inserted + tail`: `head` is everything before the
// word being completed and `tail` everything after the caret. Cycling swaps
// `inserted` and leaves the other two alone.
export function completeNick(text, caret, nicks, previous) {
  const continuing =
    previous && previous.text === text && previous.caret === caret && previous.candidates.length > 0;

  let start;
  let candidates;
  let index;
  if (continuing) {
    ({ start, candidates } = previous);
    index = (previous.index + 1) % candidates.length;
  } else {
    const before = text.slice(0, caret);
    start = before.search(/\S*$/);
    const prefix = before.slice(start);
    if (prefix === "") return null;
    const folded = fold(prefix);
    candidates = nicks
      .filter((n) => fold(n).startsWith(folded))
      .sort((a, b) => fold(a).localeCompare(fold(b)));
    if (candidates.length === 0) return null;
    index = 0;
  }

  // A nick at the start of a line is being addressed ("alp: "); elsewhere it is
  // part of a sentence.
  const suffix = start === 0 ? ": " : " ";
  const head = text.slice(0, start);
  const tail = text.slice(caret);
  const inserted = candidates[index] + suffix;
  const result = head + inserted + tail;
  const newCaret = head.length + inserted.length;

  return {
    text: result,
    caret: newCaret,
    state: { text: result, caret: newCaret, start, candidates, index },
  };
}

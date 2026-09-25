// Building DOM for messages.
//
// Everything a stranger on the network wrote reaches the page through this file,
// so it follows one rule without exception: text goes in with `textContent` or
// `createTextNode`, never `innerHTML`. Markup in a message is therefore shown as
// characters, not interpreted. The one dynamic style used, a colour, is set
// through the CSS object model, which the content security policy allows (unlike
// a `style` attribute).

import { describeEvent } from "./events.js";
import { t } from "./i18n.js";
import { formatDay, formatTime, linkify, nickHue, readableOn, sameDay, splitInlineCode } from "./lib.js";

// How times and days are written, set by the application from the person's
// settings.
const format = { hour12: false, locale: undefined };

export function configureRender({ hour12, locale }) {
  if (hour12 !== undefined) format.hour12 = hour12;
  if (locale !== undefined) format.locale = locale;
}

export function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

// The page background as a hex colour, for making IRC colours legible on it.
function pageBackground() {
  return getComputedStyle(document.documentElement).getPropertyValue("--bg").trim() || "#14171a";
}

function linkElement(segment) {
  const a = el("a", "link", segment.text);
  a.href = segment.url;
  a.rel = "noopener noreferrer";
  a.title = segment.url;
  a.dataset.url = segment.url;
  return a;
}

// Appends text to `parent`: `code` in backticks becomes monospace, and web
// addresses outside code become links.
function appendText(parent, text) {
  for (const piece of splitInlineCode(text)) {
    if (piece.code) {
      parent.appendChild(el("code", "inline-code", piece.text));
      continue;
    }
    for (const segment of linkify(piece.text)) {
      parent.appendChild(segment.url ? linkElement(segment) : document.createTextNode(segment.text));
    }
  }
}

// Turns the backend's styled spans into nodes.
export function renderSpans(spans) {
  const fragment = document.createDocumentFragment();
  const background = pageBackground();
  for (const span of spans) {
    const styled = span.bold || span.italic || span.underline || span.strike || span.mono || span.reverse || span.fg || span.bg;
    if (!styled) {
      appendText(fragment, span.text);
      continue;
    }
    const node = el("span");
    if (span.bold) node.classList.add("bold");
    if (span.italic) node.classList.add("italic");
    if (span.underline) node.classList.add("underline");
    if (span.strike) node.classList.add("strike");
    if (span.mono) node.classList.add("mono");
    if (span.reverse) node.classList.add("reverse");
    // IRC colours assume a white or black page; nudge them until they can be
    // read on this one.
    if (span.fg && !span.reverse) node.style.setProperty("color", readableOn(span.fg, span.bg ?? background));
    if (span.bg && !span.reverse) node.style.setProperty("background-color", span.bg);
    appendText(node, span.text);
    fragment.appendChild(node);
  }
  return fragment;
}

function timeElement(ms) {
  const time = el("time", "time", formatTime(ms, format.hour12));
  const date = new Date(ms);
  time.dateTime = date.toISOString();
  time.title = date.toLocaleString(format.locale);
  return time;
}

export function renderMessageLine(message, { targetId } = {}) {
  if (message.kind === "event") return renderEventLine(message, { targetId });

  const row = el("div", "line message");
  if (message.own) row.classList.add("own");
  if (message.highlight) row.classList.add("highlight");
  if (message.kind === "action") row.classList.add("action");
  if (message.kind === "notice") row.classList.add("notice");
  if (message.id != null) row.dataset.id = String(message.id);
  if (targetId != null && message.id === targetId) row.classList.add("target");

  row.appendChild(timeElement(message.time_ms));

  const label = message.kind === "action" ? "*" : message.kind === "notice" ? `-${message.sender}-` : message.sender;
  const nick = el("button", "nick", label);
  nick.type = "button";
  nick.dataset.nick = message.sender;
  nick.title = message.sender;
  nick.style.setProperty("--h", String(nickHue(message.sender)));
  row.appendChild(nick);

  const body = el("span", "body");
  if (message.kind === "action") body.appendChild(document.createTextNode(`${message.sender} `));
  body.appendChild(renderSpans(message.spans));
  row.appendChild(body);
  return row;
}

// Something that happened, drawn quietly: a join, a part, a topic change.
function renderEventLine(message, { targetId } = {}) {
  const row = el("div", `line event ${message.event?.verb ?? ""}`);
  if (message.id != null) row.dataset.id = String(message.id);
  if (targetId != null && message.id === targetId) row.classList.add("target");
  row.appendChild(timeElement(message.time_ms));
  row.appendChild(el("span", "text", describeEvent(message)));
  return row;
}

export function renderSystemLine(line) {
  const row = el("div", `line system ${line.level ?? "info"}`);
  row.appendChild(timeElement(line.time));
  row.appendChild(el("span", "text", line.text));
  return row;
}

const lineTime = (line) => (line.kind === "message" ? line.message.time_ms : line.time);

// Renders lines with a divider whenever the day changes. `previousTime` is the
// time of the line drawn just before (for appending); `unreadFrom` is the index
// of the first unread line, if the marker should be drawn.
export function renderLines(lines, { unreadFrom = null, targetId = null, previousTime = null } = {}) {
  const fragment = document.createDocumentFragment();
  let last = previousTime;
  lines.forEach((line, index) => {
    const time = lineTime(line);
    if (last === null || !sameDay(last, time)) {
      const day = el("div", "day");
      day.appendChild(el("span", undefined, formatDay(time, format.locale)));
      fragment.appendChild(day);
    }
    if (unreadFrom !== null && index === unreadFrom) {
      fragment.appendChild(el("div", "unread-marker", t("messages.new_marker")));
    }
    fragment.appendChild(
      line.kind === "message" ? renderMessageLine(line.message, { targetId }) : renderSystemLine(line),
    );
    last = time;
  });
  return fragment;
}

export { lineTime };

// The interface's controller: it listens to the backend, keeps `state` up to
// date, and draws it. Anything that can be decided without the DOM lives in
// lib.js, state.js or events.js, where it is tested.

import { api } from "./api.js";
import { ACCENTS, DENSITIES, FONT_SIZES, THEMES, applyAppearance, cacheAppearance } from "./appearance.js";
import { LANGUAGES, language, locale, setLanguage, t, tn, translateDocument } from "./i18n.js";
import { HELP, completeNick, fold, isChannelName, nickHue, parseInput } from "./lib.js";
import { PRESETS } from "./presets.js";
import { configureRender, el, lineTime, renderLines } from "./render.js";
import {
  MAX_LINES,
  SERVER,
  activeBuffer,
  addSystem,
  applyEnvelope,
  applyKnownBuffers,
  clearBuffer,
  closeBuffer,
  createState,
  ensureBuffer,
  ensureNetwork,
  findBuffer,
  lastChatTime,
  leaveContext,
  markUnreadFrom,
  mergeHistory,
  prependHistory,
  setActive,
  setFocused,
  showContext,
} from "./state.js";

const $ = (id) => document.getElementById(id);
const PAGE = 100;

const DEFAULT_SETTINGS = {
  theme: "system",
  accent: "theme",
  density: "comfortable",
  font_size: "medium",
  language: "auto",
  time_format: "24h",
  show_events: true,
  notifications: true,
};

const state = createState();
let profiles = [];
let settings = { ...DEFAULT_SETTINGS };

// What is drawn right now, and the small amount of state the view itself keeps.
const view = {
  renderedTotal: 0, // how many of the buffer's lines have been drawn
  lastTime: null, // time of the last drawn line, for day dividers
  loadingOlder: false,
  unseen: 0, // messages that arrived while scrolled up
  sent: [], // what was typed, for the Up arrow
  sentIndex: -1,
  completion: null,
};

// ---- small helpers -------------------------------------------------------

let toastTimer = 0;
function toast(text, level = "info") {
  const box = $("toast");
  box.textContent = text;
  box.className = level === "error" ? "error" : "";
  box.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (box.hidden = true), level === "error" ? 9000 : 5000);
}

const fail = (error) => toast(String(error?.message ?? error), "error");

const activeNet = () => (state.active ? state.networks.get(state.active.network) : null);
const isActive = (network, key) => state.active?.network === network && state.active.key === key;
const isLive = (net) => ["registered", "connecting", "connected", "waiting"].includes(net.status);

function slug(name) {
  const s = name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 32);
  return s || "network";
}

function uniqueId(base) {
  let id = base;
  for (let n = 2; profiles.some((p) => p.id === id); n += 1) id = `${base.slice(0, 28)}-${n}`;
  return id;
}

function statusText(net) {
  switch (net.status) {
    case "registered": return net.nick ? t("status.connected_as", { nick: net.nick }) : t("status.connected");
    case "connecting": return t("status.connecting");
    case "connected": return t("status.logging_in");
    case "waiting": {
      const seconds = Math.max(0, Math.ceil(((net.retryAt ?? Date.now()) - Date.now()) / 1000));
      return t("status.reconnecting", { seconds });
    }
    case "failed": return t("status.disconnected");
    default: return t("status.idle");
  }
}

// ---- modal questions -----------------------------------------------------

// Shows a modal dialog and resolves with what `answer()` returns when its form
// is submitted, or with null if the person cancels.
//
// The answer is taken from the form's own submit and the cancel button's own
// click. The dialog's `close` event is used only as a fallback for Escape: it is
// dispatched asynchronously, and code that waits for it can wait far longer than
// the person expects.
function ask(dialog, { form, cancel, answer }) {
  return new Promise((resolve) => {
    let settled = false;
    const finish = (value) => {
      if (settled) return;
      settled = true;
      form.removeEventListener("submit", onSubmit);
      cancel.removeEventListener("click", onCancel);
      dialog.removeEventListener("close", onCancel);
      dialog.removeEventListener("cancel", onCancel);
      if (dialog.open) dialog.close();
      resolve(value);
    };
    const onSubmit = (event) => {
      event.preventDefault();
      finish(answer());
    };
    const onCancel = () => finish(null);
    form.addEventListener("submit", onSubmit);
    cancel.addEventListener("click", onCancel);
    dialog.addEventListener("close", onCancel);
    dialog.addEventListener("cancel", onCancel);
    dialog.showModal();
  });
}

// Asks for confirmation of something that cannot be undone.
async function confirmDialog({ title, body, ok }) {
  $("confirm-title").textContent = title;
  $("confirm-body").textContent = body;
  $("btn-confirm-ok").textContent = ok;
  const answer = await ask($("confirm-dialog"), {
    form: $("confirm-form"),
    cancel: $("btn-confirm-cancel"),
    answer: () => true,
  });
  return answer === true;
}

// ---- settings -----------------------------------------------------------------

const radioNames = { theme: "theme", accent: "accent", density: "density", font_size: "font" };

// Applies the settings to the window: language, theme, time format, and what is
// shown. With `save`, also writes them to the settings file.
function applySettings(next = {}, { save = false, rerender = true } = {}) {
  settings = { ...settings, ...next };
  setLanguage(settings.language);
  translateDocument();
  applyAppearance(settings, document.documentElement, matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.dataset.hour12 = String(settings.time_format === "12h");
  configureRender({ hour12: settings.time_format === "12h", locale: locale() });
  $("messages").classList.toggle("hide-events", !settings.show_events);
  cacheAppearance(settings);
  syncSettingsControls();
  if (rerender) renderAll({ scroll: "same" });
  if (save) api.saveSettings(settings).catch(fail);
}

// A small preview of a theme, drawn with that theme's own colours.
function themePreview(theme) {
  const pv = el("span", "pv");
  pv.dataset.theme = theme;
  pv.append(el("i"), el("i"), el("i"));
  return pv;
}

function radioLabel(group, value, className) {
  const label = el("label", className);
  const input = el("input");
  input.type = "radio";
  input.name = group;
  input.value = value;
  label.append(input);
  return { label, input };
}

// Builds the controls in the settings dialog. Called again when the language
// changes, since their labels are text.
function buildSettingsControls() {
  const themeGrid = $("theme-grid");
  themeGrid.replaceChildren();
  for (const theme of THEMES) {
    const { label } = radioLabel("theme", theme, "theme-card");
    const frame = el("span", "frame");
    if (theme === "system") frame.append(themePreview("graphite"), themePreview("daylight"));
    else frame.append(themePreview(theme));
    label.append(frame, el("span", "name", t(`theme.${theme}`)));
    themeGrid.append(label);
  }

  const accentRow = $("accent-row");
  accentRow.replaceChildren();
  for (const accent of ACCENTS) {
    const { label } = radioLabel("accent", accent, "accent-dot");
    label.dataset.accent = accent;
    label.title = t(`accent.${accent}`);
    label.append(el("span"));
    accentRow.append(label);
  }

  const segmented = (container, group, values, key) => {
    container.replaceChildren();
    for (const value of values) {
      const { label } = radioLabel(group, value);
      label.append(document.createTextNode(t(`${key}.${value}`)));
      container.append(label);
    }
  };
  segmented($("density-row"), "density", DENSITIES, "density");
  segmented($("font-row"), "font", FONT_SIZES, "font");

  const select = $("s-language");
  select.replaceChildren();
  const auto = el("option", undefined, t("settings.language_auto"));
  auto.value = "auto";
  select.append(auto);
  for (const [code, name] of LANGUAGES) {
    const option = el("option", undefined, name);
    option.value = code;
    select.append(option);
  }

  const shortcuts = $("shortcut-list");
  shortcuts.replaceChildren();
  for (const [keys, what] of [
    [["Ctrl", "K"], "shortcut.search"],
    [["Ctrl", ","], "shortcut.settings"],
    [["Alt", "↑ / ↓"], "shortcut.switch"],
    [["Tab"], "shortcut.complete"],
    [["↑"], "shortcut.recall"],
    [["Shift", "Enter"], "shortcut.newline"],
    [["PgUp / PgDn"], "shortcut.scroll"],
    [["Esc"], "shortcut.close"],
  ]) {
    const dt = el("dt");
    keys.forEach((k) => dt.append(el("kbd", undefined, k)));
    shortcuts.append(dt, el("dd", undefined, t(what)));
  }
  syncSettingsControls();
}

// Makes the controls show the current settings.
function syncSettingsControls() {
  for (const [key, group] of Object.entries(radioNames)) {
    for (const input of document.querySelectorAll(`input[name="${group}"]`)) input.checked = input.value === settings[key];
  }
  $("s-language").value = settings.language;
  $("s-time").value = settings.time_format;
  $("s-events").checked = settings.show_events;
  $("s-notify").checked = settings.notifications;
}

async function openSettings() {
  buildSettingsControls();
  try {
    const info = await api.appInfo();
    $("about-version").textContent = info.version;
    $("about-license").textContent = info.license;
    $("about-data").textContent = info.data_dir;
  } catch (error) {
    fail(error);
  }
  if (!$("settings-dialog").open) $("settings-dialog").showModal();
}

function bindSettings() {
  $("settings-dialog").addEventListener("change", (event) => {
    const input = event.target;
    if (input.type === "radio") {
      const key = Object.keys(radioNames).find((k) => radioNames[k] === input.name);
      if (key) applySettings({ [key]: input.value }, { save: true });
    } else if (input.id === "s-language") {
      applySettings({ language: input.value }, { save: true });
      buildSettingsControls();
    } else if (input.id === "s-time") {
      applySettings({ time_format: input.value }, { save: true });
    } else if (input.id === "s-events") {
      applySettings({ show_events: input.checked }, { save: true });
    } else if (input.id === "s-notify") {
      applySettings({ notifications: input.checked }, { save: true, rerender: false });
    }
  });
  $("btn-test-notify").addEventListener("click", async () => {
    const shown = await api.notify("Rhizome", t("settings.test_body")).catch(() => false);
    if (!shown) toast(t("settings.notify_blocked"), "error");
  });
  // "System" follows the operating system, including while the window is open.
  matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    if (settings.theme === "system") applySettings({}, {});
  });
}

// ---- sidebar -------------------------------------------------------------

function orderedKeys(net) {
  const keys = [...net.buffers.keys()].filter((k) => k !== SERVER);
  const rank = (k) => (net.buffers.get(k).kind === "channel" ? 0 : 1);
  return keys.sort((a, b) => rank(a) - rank(b) || a.localeCompare(b));
}

function badge(buffer) {
  if (buffer.unread === 0) return null;
  const b = el("span", buffer.highlights > 0 ? "badge highlight" : "badge", buffer.unread > 99 ? "99+" : String(buffer.unread));
  b.title =
    buffer.highlights > 0
      ? t("sidebar.unread_mentions", { count: buffer.unread, mentions: buffer.highlights })
      : t("sidebar.unread", { count: buffer.unread });
  return b;
}

const chevron = () => {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("width", "14");
  svg.setAttribute("height", "14");
  svg.setAttribute("aria-hidden", "true");
  const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute("d", "M9 6l6 6-6 6");
  svg.append(path);
  return svg;
};

function renderSidebar() {
  const list = $("buffer-list");
  list.replaceChildren();
  for (const net of state.networks.values()) {
    const section = el("section", "network");
    const head = el("div", "network-head");

    const toggle = el("button", "network-toggle");
    toggle.type = "button";
    toggle.dataset.network = net.id;
    toggle.dataset.action = "collapse";
    toggle.setAttribute("aria-expanded", String(!net.collapsed));
    toggle.setAttribute("aria-label", net.collapsed ? t("sidebar.expand") : t("sidebar.collapse"));
    toggle.append(chevron());

    const name = el("button", "network-name");
    name.type = "button";
    name.dataset.network = net.id;
    name.dataset.key = SERVER;
    if (isActive(net.id, SERVER)) name.setAttribute("aria-current", "true");
    const dot = el("span", `dot ${net.status}`);
    dot.title = statusText(net);
    name.append(dot, el("span", "label", net.name));
    const serverBadge = badge(net.buffers.get(SERVER));
    if (serverBadge) name.append(serverBadge);

    const connect = el("button", "link-btn", isLive(net) ? t("sidebar.disconnect") : t("sidebar.connect"));
    connect.type = "button";
    connect.dataset.network = net.id;
    connect.dataset.action = isLive(net) ? "disconnect" : "connect";
    head.append(toggle, name, connect);
    section.append(head);

    if (!net.collapsed) {
      const keys = orderedKeys(net);
      const channels = keys.filter((k) => net.buffers.get(k).kind === "channel");
      const talks = keys.filter((k) => net.buffers.get(k).kind !== "channel");
      const buffers = el("ul", "buffers");
      const groups = [];
      if (channels.length) groups.push([t("sidebar.channels"), channels]);
      if (talks.length) groups.push([t("sidebar.conversations"), talks]);
      const labelled = keys.length > 4 && groups.length > 1;
      for (const [label, members] of groups) {
        if (labelled) buffers.append(el("li", "group-label", label));
        for (const key of members) buffers.append(bufferItem(net, key));
      }
      section.append(buffers);
    }
    list.append(section);
  }
  updateTitle();
}

function bufferItem(net, key) {
  const buffer = net.buffers.get(key);
  const item = el("li");
  const button = el("button", "buffer");
  button.type = "button";
  button.dataset.network = net.id;
  button.dataset.key = key;
  if (isActive(net.id, key)) button.setAttribute("aria-current", "true");
  if (buffer.unread > 0) button.classList.add("unread");
  if (buffer.kind === "channel" && !buffer.joined) button.classList.add("parted");
  button.append(el("span", "hash", buffer.kind === "channel" ? "#" : "@"));
  button.append(el("span", "label", buffer.kind === "channel" ? buffer.name.replace(/^[#&+!]/, "") : buffer.name));
  const b = badge(buffer);
  if (b) button.append(b);

  const close = el("button", "buffer-close", "×");
  close.type = "button";
  close.dataset.network = net.id;
  close.dataset.key = key;
  close.dataset.action = "close";
  close.title = buffer.kind === "channel" && buffer.joined ? t("sidebar.leave_channel") : t("sidebar.close");
  close.setAttribute("aria-label", close.title);
  item.append(button, close);
  return item;
}

function updateTitle() {
  let mentions = 0;
  for (const net of state.networks.values()) for (const b of net.buffers.values()) mentions += b.highlights;
  document.title = mentions > 0 ? `(${mentions}) Rhizome` : "Rhizome";
}

// ---- header, members, composer, welcome ----------------------------------

function renderHeader() {
  const buffer = activeBuffer(state);
  const net = activeNet();
  const status = $("status");
  if (!buffer || !net) {
    $("title-name").textContent = "Rhizome";
    $("title-topic").textContent = t("header.add_network");
    status.textContent = "";
    $("context-banner").hidden = true;
    return;
  }
  $("title-name").textContent = buffer.kind === "server" ? net.name : buffer.name;
  const topic =
    buffer.kind === "channel" ? (buffer.topic ?? "") : buffer.kind === "server" ? t("header.server", { name: net.name }) : t("header.private");
  $("title-topic").textContent = topic;
  $("title-topic").title = topic;
  status.textContent = statusText(net);
  status.classList.toggle("problem", net.status === "failed" || net.status === "waiting");
  $("context-banner").hidden = !buffer.context;
}

function renderMembers() {
  const buffer = activeBuffer(state);
  const list = $("members");
  list.replaceChildren();
  if (!buffer || buffer.kind !== "channel") {
    $("members-title").textContent = buffer?.kind === "query" ? t("members.conversation") : t("top.members");
    return;
  }
  $("members-title").textContent = t("members.title", { count: buffer.members.length });
  const fragment = document.createDocumentFragment();
  for (const member of buffer.members) {
    const item = el("li");
    const button = el("button", "member");
    button.type = "button";
    button.dataset.nick = member.nick;
    button.title = t("members.message", { nick: member.nick });
    const nick = el("span", "nick", member.nick);
    nick.style.setProperty("--h", String(nickHue(member.nick)));
    button.append(el("span", "prefix", member.prefixes[0] ?? ""), nick);
    item.append(button);
    fragment.append(item);
  }
  list.append(fragment);
}

function renderComposer() {
  const buffer = activeBuffer(state);
  const input = $("input");
  if (!buffer) input.placeholder = t("composer.placeholder_none");
  else if (buffer.kind === "server") input.placeholder = t("composer.placeholder_server");
  else input.placeholder = t("composer.placeholder_buffer", { name: buffer.name });
}

// The first-run screen, shown until there is a network to talk to.
function renderWelcome() {
  const show = state.networks.size === 0;
  $("welcome").hidden = !show;
  if (!show) return;
  const chips = $("welcome-presets");
  chips.replaceChildren();
  for (const preset of PRESETS) {
    const chip = el("button", "preset-chip", preset.name);
    chip.type = "button";
    chip.addEventListener("click", () => openProfileEditor(null, preset));
    chips.append(chip);
  }
}

function renderAll(options) {
  renderSidebar();
  renderHeader();
  renderMembers();
  renderComposer();
  renderWelcome();
  renderMessages(options);
}

// ---- messages ------------------------------------------------------------

const messagesBox = () => $("messages");
const atBottom = () => {
  const box = messagesBox();
  return box.scrollHeight - box.scrollTop - box.clientHeight < 48;
};

function updatePill() {
  const pill = $("new-pill");
  pill.hidden = view.unseen === 0;
  if (view.unseen > 0) pill.textContent = tn("messages.pill", view.unseen);
}

function emptyText(buffer) {
  if (!buffer) return t("empty.no_network");
  if (buffer.kind === "server") return t("empty.server");
  return t("empty.none");
}

// Redraws the active buffer. `scroll` is "bottom", "target" (the search result),
// "keep" (after loading older history, so the view does not jump) or "same"
// (leave the scroll position alone, for a change of theme or language).
function renderMessages({ scroll = "bottom", previousHeight = 0 } = {}) {
  const buffer = activeBuffer(state);
  const box = messagesBox();
  const before = box.scrollTop;
  const wasAtBottom = atBottom();
  box.replaceChildren();
  view.unseen = 0;
  updatePill();

  if (!buffer || buffer.lines.length === 0) {
    box.append(el("p", "empty", emptyText(buffer)));
    view.renderedTotal = buffer?.total ?? 0;
    view.lastTime = null;
    return;
  }
  box.append(
    renderLines(buffer.lines, {
      unreadFrom: buffer.unreadFrom,
      targetId: buffer.context?.targetId ?? null,
    }),
  );
  view.renderedTotal = buffer.total;
  view.lastTime = lineTime(buffer.lines.at(-1));

  if (scroll === "target") {
    box.querySelector(".target")?.scrollIntoView({ block: "center" });
  } else if (scroll === "keep") {
    box.scrollTop += box.scrollHeight - previousHeight;
  } else if (scroll === "same") {
    box.scrollTop = wasAtBottom ? box.scrollHeight : before;
  } else {
    box.scrollTop = box.scrollHeight;
  }
}

// Draws only what is new since the last draw, keeping the scroll position
// sensible: stay at the bottom if you were there, otherwise count what you have
// not seen.
function appendNew(buffer) {
  const box = messagesBox();
  const added = buffer.total - view.renderedTotal;
  if (added <= 0) return;
  // A search result is a fixed window; live lines are not added to it.
  if (buffer.context) return;
  if (added > buffer.lines.length || added > 60 || box.childElementCount > MAX_LINES * 2) {
    renderMessages({ scroll: "bottom" });
    return;
  }
  const stick = atBottom();
  const fresh = buffer.lines.slice(-added);
  box.querySelector(".empty")?.remove();
  box.append(renderLines(fresh, { previousTime: view.lastTime }));
  view.renderedTotal = buffer.total;
  view.lastTime = lineTime(buffer.lines.at(-1));
  if (stick) {
    box.scrollTop = box.scrollHeight;
  } else {
    view.unseen += fresh.filter((l) => l.kind === "message" && l.message.kind !== "event" && !l.message.own).length;
    updatePill();
  }
}

// ---- reading and marking read -----------------------------------------------

const markTimers = new Map();

// Tells the log how far a conversation has been read, shortly after the last
// message that was on screen. Waiting a moment folds a burst of messages into
// one write.
function scheduleMarkRead(network, buffer, delay = 700) {
  if (buffer.context || !state.focused) return;
  const upTo = lastChatTime(buffer);
  if (upTo === null) return;
  const id = `${network}\0${buffer.key}`;
  clearTimeout(markTimers.get(id));
  markTimers.set(
    id,
    setTimeout(() => {
      markTimers.delete(id);
      api.markRead(network, buffer.name, upTo).catch(() => {});
    }, delay),
  );
}

// Marks now, without the delay: used when leaving a conversation.
function markReadNow(network, buffer) {
  const id = `${network}\0${buffer.key}`;
  clearTimeout(markTimers.get(id));
  markTimers.delete(id);
  const upTo = lastChatTime(buffer);
  if (upTo !== null && !buffer.context) api.markRead(network, buffer.name, upTo).catch(() => {});
}

async function loadHistory(network, buffer, { unread = 0 } = {}) {
  if (buffer.kind === "server") {
    buffer.loaded = true;
    return;
  }
  try {
    const page = await api.scrollback(network, buffer.name, null, PAGE);
    mergeHistory(buffer, page, { pageSize: PAGE });
    // Show where the unread messages begin, from what the log said was unread.
    if (unread > 0) markUnreadFrom(buffer, unread);
  } catch (error) {
    buffer.loaded = true;
    fail(error);
  }
  if (isActive(network, buffer.key)) {
    renderMessages({ scroll: "bottom" });
    scheduleMarkRead(network, buffer, 300);
  }
}

async function loadOlder(buffer) {
  const network = state.active?.network;
  if (view.loadingOlder || !buffer.hasMore || !buffer.loaded || !network) return;
  const first = buffer.lines.find((l) => l.kind === "message" && l.message.id != null);
  if (!first) {
    buffer.hasMore = false;
    return;
  }
  view.loadingOlder = true;
  try {
    const before = { time_ms: first.message.time_ms, id: first.message.id };
    const page = await api.scrollback(network, buffer.name, before, PAGE);
    const previousHeight = messagesBox().scrollHeight;
    prependHistory(buffer, page, { pageSize: PAGE });
    if (isActive(network, buffer.key)) renderMessages({ scroll: "keep", previousHeight });
  } catch (error) {
    fail(error);
  } finally {
    view.loadingOlder = false;
  }
}

// ---- switching buffers ---------------------------------------------------

async function activate(network, key) {
  const previous = activeBuffer(state);
  if (previous && !isActive(network, key)) markReadNow(state.active.network, previous);
  const target = findBuffer(state, network, key);
  // Remember what was unread before opening it clears the count.
  const unread = target ? target.unread : 0;
  const buffer = setActive(state, network, key);
  if (!buffer) return;
  view.completion = null;
  document.body.classList.remove("show-sidebar", "show-members");
  renderAll({ scroll: "bottom" });
  $("input").focus();
  if (!buffer.loaded) await loadHistory(network, buffer, { unread });
  else scheduleMarkRead(network, buffer, 300);
}

function flatBuffers() {
  const out = [];
  for (const net of state.networks.values()) {
    out.push([net.id, SERVER]);
    if (!net.collapsed) for (const key of orderedKeys(net)) out.push([net.id, key]);
  }
  return out;
}

function cycleBuffer(step) {
  const all = flatBuffers();
  if (all.length === 0) return;
  const current = all.findIndex(([n, k]) => isActive(n, k));
  const [network, key] = all[(current + step + all.length) % all.length];
  activate(network, key);
}

// ---- backend events ------------------------------------------------------

const lastNotified = new Map();

// Tells the desktop about a message that needs attention, if the person is not
// already looking at it and has not switched notifications off.
function maybeNotify(effect) {
  const { message: m } = effect;
  if (!settings.notifications || !m.highlight) return;
  if (state.focused && isActive(effect.network, effect.key)) return;
  const id = `${effect.network}\0${effect.key}`;
  if (Date.now() - (lastNotified.get(id) ?? 0) < 3000) return;
  lastNotified.set(id, Date.now());
  const title = isChannelName(m.buffer) ? t("notify.mention", { sender: m.sender, buffer: m.buffer }) : t("notify.private", { sender: m.sender });
  api.notify(title, m.plain.slice(0, 140)).catch(() => {});
}

function runEffect(effect) {
  switch (effect.type) {
    case "joined": {
      // Joining is the moment the person usually wants to see the channel, but
      // never yank them away from a conversation they are reading.
      const current = activeBuffer(state);
      if (!current || (current.kind === "server" && state.active.network === effect.network)) {
        activate(effect.network, effect.key);
      }
      break;
    }
    case "registered":
      api.buffers(effect.network)
        .then((list) => {
          const net = state.networks.get(effect.network);
          if (net) applyKnownBuffers(net, list.filter((b) => !isChannelName(b.name)));
          renderSidebar();
        })
        .catch(() => {});
      break;
    case "incoming": {
      const buffer = findBuffer(state, effect.network, effect.key);
      if (buffer && isActive(effect.network, effect.key)) scheduleMarkRead(effect.network, buffer);
      maybeNotify(effect);
      break;
    }
    case "refresh_names":
      scheduleNamesRefresh(effect.network, effect.channel);
      break;
    default:
      break;
  }
}

const namesTimers = new Map();
function scheduleNamesRefresh(network, channel) {
  const key = `${network}\0${fold(channel)}`;
  clearTimeout(namesTimers.get(key));
  namesTimers.set(
    key,
    setTimeout(() => {
      namesTimers.delete(key);
      api.raw(network, `NAMES ${channel}`).catch(() => {});
    }, 400),
  );
}

function onEnvelope(envelope) {
  const before = activeBuffer(state);
  const effects = applyEnvelope(state, envelope);
  effects.forEach(runEffect);

  renderSidebar();
  const active = activeBuffer(state);
  if (active && envelope.network === state.active.network) {
    renderHeader();
    const type = envelope.event.type;
    if (["names", "member_joined", "member_parted", "member_kicked", "member_quit", "nick_changed", "joined", "parted", "kicked"].includes(type)) {
      renderMembers();
    }
  }
  if (active && active === before) appendNew(active);
  if (state.toast) {
    toast(state.toast.text, state.toast.level);
    state.toast = null;
  }
}

// ---- sending -------------------------------------------------------------

async function paste(text) {
  const lines = text.split("\n").filter((l) => l.trim() !== "");
  if (lines.length <= 3) return true;
  const target = activeBuffer(state)?.name ?? "";
  $("paste-lead").textContent = t("paste.lead", { count: lines.length, target });
  $("paste-preview").textContent =
    lines.slice(0, 8).join("\n") + (lines.length > 8 ? `\n${t("paste.more", { count: lines.length - 8 })}` : "");
  $("btn-paste-send").textContent = t("paste.send", { count: lines.length });
  const answer = await ask($("paste-dialog"), {
    form: $("paste-form"),
    cancel: $("btn-paste-cancel"),
    answer: () => true,
  });
  return answer === true;
}

function say(buffer, net, text, level = "info") {
  addSystem(state, net.id, buffer.key, text, level);
  appendNew(buffer);
}

async function runAction(action, net, buffer) {
  const target = buffer.name;
  switch (action.type) {
    case "message":
    case "action": {
      if (buffer.kind === "server") {
        toast(t("toast.pick_channel"), "error");
        return false;
      }
      if (!(await paste(action.text))) return false;
      await api.sendMessage(net.id, target, action.text, action.type === "action" ? "action" : "privmsg");
      return true;
    }
    case "notice":
      await api.sendMessage(net.id, action.target, action.text, "notice");
      return true;
    case "query": {
      const opened = ensureBuffer(net, action.target, isChannelName(action.target) ? "channel" : "query");
      await activate(net.id, opened.key);
      if (action.text) await api.sendMessage(net.id, action.target, action.text, "privmsg");
      return true;
    }
    case "join":
      await api.join(net.id, action.channels);
      return true;
    case "part":
      await api.part(net.id, action.channel, action.reason);
      return true;
    case "nick":
      await api.setNick(net.id, action.nick);
      return true;
    case "quit":
      await api.disconnect(net.id);
      return true;
    case "raw":
      await api.raw(net.id, action.line);
      return true;
    case "search":
      openSearch(action.query);
      return true;
    case "clear": {
      const ok = await confirmDialog({
        title: t("confirm.clear_title"),
        body: t("confirm.clear_body", { name: buffer.name }),
        ok: t("confirm.clear_ok"),
      });
      if (!ok) return false;
      const removed = await api.clearHistory(net.id, buffer.name);
      clearBuffer(buffer);
      renderAll({ scroll: "bottom" });
      toast(tn("toast.history_cleared", removed));
      return true;
    }
    case "help":
      for (const [usage, key] of HELP) say(buffer, net, `${usage}  —  ${t(key)}`);
      return true;
    case "error":
      say(buffer, net, t(action.key, action.params), "error");
      return true;
    default:
      return true;
  }
}

async function submit() {
  const input = $("input");
  const text = input.value;
  const net = activeNet();
  const buffer = activeBuffer(state);
  if (!net || !buffer) {
    toast(t("toast.add_network_first"), "error");
    return;
  }
  const action = parseInput(text, { buffer: buffer.kind === "server" ? SERVER : buffer.name, isChannel: buffer.kind === "channel" });
  if (!action) return;
  if (["message", "action", "notice", "query", "join", "part", "nick", "raw"].includes(action.type) && net.status !== "registered") {
    toast(t("toast.not_connected", { name: net.name }), "error");
    return;
  }
  try {
    const sent = await runAction(action, net, buffer);
    if (!sent) return;
  } catch (error) {
    fail(error);
    return;
  }
  view.sent.push(text);
  view.sent = view.sent.slice(-100);
  view.sentIndex = -1;
  input.value = "";
  view.completion = null;
  resizeInput();
}

function resizeInput() {
  const input = $("input");
  input.style.height = "auto";
  input.style.height = `${Math.min(input.scrollHeight, 160)}px`;
}

function insertAtCaret(text) {
  const input = $("input");
  const { selectionStart: start, selectionEnd: end, value } = input;
  input.value = value.slice(0, start) + text + value.slice(end);
  input.selectionStart = input.selectionEnd = start + text.length;
  input.focus();
  resizeInput();
}

// ---- network and profile dialogs -----------------------------------------

async function refreshProfiles() {
  try {
    profiles = await api.listProfiles();
  } catch (error) {
    profiles = [];
    fail(error);
  }
  for (const p of profiles) {
    const net = ensureNetwork(state, p.id, p.name);
    net.name = p.name;
    net.buffers.get(SERVER).name = p.name;
  }
}

// Fills the sidebar with the conversations the log already has for each
// network, so unread counts are there before anything connects.
async function loadKnownBuffers() {
  await Promise.all(
    profiles.map(async (p) => {
      try {
        applyKnownBuffers(state.networks.get(p.id), await api.buffers(p.id));
      } catch {
        // The sidebar simply starts with what is live.
      }
    }),
  );
}

async function renderProfileList() {
  const list = $("profile-list");
  list.replaceChildren();
  if (profiles.length === 0) {
    list.append(el("li", "none", t("networks.none")));
    return;
  }
  const remembered = await Promise.all(profiles.map((p) => (p.sasl_account ? api.hasSavedPassword(p.id).catch(() => false) : false)));
  profiles.forEach((p, index) => {
    const net = state.networks.get(p.id);
    const item = el("li");
    const info = el("div", "info");
    const title = el("strong", undefined, p.name);
    if (p.autoconnect) title.append(el("span", "tag", t("networks.auto")));
    const details = [`${p.host}:${p.port}${p.tls ? "" : ` ${t("networks.no_tls")}`}`, p.nick];
    if (p.sasl_account) details.push(t("networks.account", { account: p.sasl_account }));
    info.append(title, el("span", undefined, details.join(" · ")));
    item.append(info);

    const button = (label, onClick) => {
      const b = el("button", undefined, label);
      b.type = "button";
      b.addEventListener("click", onClick);
      item.append(b);
    };
    button(net && isLive(net) ? t("networks.disconnect") : t("networks.connect"), async () => {
      if (net && isLive(net)) await api.disconnect(p.id).catch(fail);
      else await connectProfile(p.id);
      renderProfileList();
    });
    if (remembered[index]) {
      button(t("networks.forget_password"), async () => {
        await api.forgetPassword(p.id).catch(fail);
        toast(t("toast.password_forgotten"));
        renderProfileList();
      });
    }
    button(t("networks.edit"), () => openProfileEditor(p));
    button(t("networks.delete"), async () => {
      if (net && isLive(net)) return toast(t("toast.disconnect_first"), "error");
      try {
        await api.deleteProfile(p.id);
        state.networks.delete(p.id);
        if (state.active?.network === p.id) state.active = null;
        await refreshProfiles();
        renderProfileList();
        renderAll();
      } catch (error) {
        fail(error);
      }
    });
    list.append(item);
  });
}

function openNetworks() {
  renderProfileList();
  $("networks-dialog").showModal();
}

function fillPresetChoices(selected) {
  const select = $("p-preset");
  select.replaceChildren();
  const custom = el("option", undefined, t("profile.preset_custom"));
  custom.value = "";
  select.append(custom);
  for (const preset of PRESETS) {
    const option = el("option", undefined, preset.name);
    option.value = preset.id;
    select.append(option);
  }
  select.value = selected?.id ?? "";
}

function applyPreset(preset) {
  if (!preset) return;
  $("p-name").value = preset.name;
  $("p-host").value = preset.host;
  $("p-port").value = String(preset.port);
  $("p-tls").checked = preset.tls;
}

function openProfileEditor(profile, preset = null) {
  const dialog = $("profile-dialog");
  const form = $("profile-form");
  form.dataset.editing = profile?.id ?? "";
  $("preset-row").hidden = Boolean(profile);
  fillPresetChoices(preset);
  $("profile-title").textContent = profile ? t("profile.edit_title", { name: profile.name }) : t("profile.add_title");
  $("p-name").value = profile?.name ?? "";
  $("p-host").value = profile?.host ?? "";
  $("p-port").value = String(profile?.port ?? 6697);
  $("p-tls").checked = profile?.tls ?? true;
  $("p-nick").value = profile?.nick ?? "";
  $("p-username").value = profile?.username ?? "";
  $("p-realname").value = profile?.realname ?? "Rhizome";
  $("p-channels").value = (profile?.channels ?? []).join(", ");
  $("p-account").value = profile?.sasl_account ?? "";
  $("p-autoconnect").checked = profile?.autoconnect ?? false;
  if (!profile) applyPreset(preset);
  $("profile-error").hidden = true;
  if ($("networks-dialog").open) $("networks-dialog").close();
  if (!dialog.open) dialog.showModal();
  (preset && !profile ? $("p-nick") : $("p-name")).focus();
}

async function saveProfileFromForm() {
  const editing = $("profile-form").dataset.editing;
  const name = $("p-name").value.trim();
  const nick = $("p-nick").value.trim();
  const profile = {
    id: editing || uniqueId(slug(name)),
    name,
    host: $("p-host").value.trim(),
    port: Number($("p-port").value) || 6697,
    tls: $("p-tls").checked,
    nick,
    username: $("p-username").value.trim() || nick,
    realname: $("p-realname").value.trim() || "Rhizome",
    channels: $("p-channels").value.split(/[\s,]+/).filter(Boolean),
    sasl_account: $("p-account").value.trim() || null,
    autoconnect: $("p-autoconnect").checked,
  };
  try {
    await api.saveProfile(profile);
  } catch (error) {
    const box = $("profile-error");
    box.textContent = String(error?.message ?? error);
    box.hidden = false;
    return;
  }
  $("profile-dialog").close();
  await refreshProfiles();
  renderProfileList();
  renderAll();
  if (!activeBuffer(state)) await activate(profile.id, SERVER);
}

// Asks for the password of the account a network logs in to. Resolves to
// { password, remember }, or null if the person cancels.
async function askPassword(profile) {
  const input = $("password-input");
  $("password-for").textContent = t("password.for", { account: profile.sasl_account });
  input.value = "";
  $("password-remember").checked = false;
  try {
    const pending = ask($("password-dialog"), {
      form: $("password-form"),
      cancel: $("btn-password-cancel"),
      answer: () => ({ password: input.value, remember: $("password-remember").checked }),
    });
    input.focus();
    return await pending;
  } finally {
    // Whatever was typed is wiped as soon as it has been read.
    input.value = "";
  }
}

// Connects a saved network. A network that logs in needs a password: a
// remembered one is used silently, otherwise it is asked for, unless this is an
// automatic connection at startup, in which case nobody is there to ask.
async function connectProfile(id, { auto = false } = {}) {
  const profile = profiles.find((p) => p.id === id);
  if (!profile) return;
  let password = null;
  let remember = false;
  if (profile.sasl_account) {
    const saved = await api.hasSavedPassword(id).catch(() => false);
    if (!saved) {
      if (auto) {
        const net = state.networks.get(id);
        if (net) say(net.buffers.get(SERVER), net, t("sys.autoconnect_needs_password", { account: profile.sasl_account }), "error");
        return;
      }
      const answer = await askPassword(profile);
      if (answer === null) return;
      ({ password, remember } = answer);
    }
  }
  try {
    await api.connect(id, password, remember);
    if (!auto) await activate(id, SERVER);
  } catch (error) {
    fail(error);
  }
}

// ---- search --------------------------------------------------------------

const search = { token: 0, hits: [], selected: -1, timer: 0 };

function openSearch(query = "") {
  const dialog = $("search-dialog");
  if (!dialog.open) dialog.showModal();
  const input = $("search-input");
  if (query) input.value = query;
  input.focus();
  input.select();
  runSearch();
}

function selectHit(index) {
  const items = $("search-results").children;
  if (items.length === 0) return;
  search.selected = (index + items.length) % items.length;
  [...items].forEach((item, i) => item.setAttribute("aria-selected", String(i === search.selected)));
  items[search.selected].scrollIntoView({ block: "nearest" });
}

function renderHits(hits, query) {
  const list = $("search-results");
  list.replaceChildren();
  search.hits = hits;
  search.selected = -1;
  if (hits.length === 0) {
    list.append(el("li", "search-none", query.trim() ? t("search.none") : t("search.prompt")));
    $("search-count").textContent = "";
    return;
  }
  hits.forEach((hit, index) => {
    const m = hit.message;
    const item = el("li");
    item.setAttribute("role", "option");
    item.setAttribute("aria-selected", "false");
    item.dataset.index = String(index);
    const meta = el("div", "hit-meta");
    meta.append(el("span", "where", m.buffer), el("span", undefined, m.sender), el("time", undefined, new Date(m.time_ms).toLocaleString(locale())));
    const snippet = el("div", "hit-snippet");
    for (const part of hit.snippet) {
      snippet.append(part.hit ? el("mark", undefined, part.text) : document.createTextNode(part.text));
    }
    item.append(meta, snippet);
    list.append(item);
  });
  $("search-count").textContent = tn("search.count", hits.length, { plus: hits.length >= 50 ? "+" : "" });
  selectHit(0);
}

async function runSearch() {
  const query = $("search-input").value;
  const token = ++search.token;
  if (!query.trim()) return renderHits([], query);
  const network = $("search-scope").value === "this" ? (state.active?.network ?? null) : null;
  try {
    const hits = await api.search(query, network, $("search-order").value === "newest", 50);
    // Ignore an answer to a question that has since been replaced.
    if (token === search.token) renderHits(hits, query);
  } catch (error) {
    if (token === search.token) {
      $("search-results").replaceChildren(el("li", "search-none", String(error?.message ?? error)));
    }
  }
}

async function openHit(hit) {
  const m = hit.message;
  $("search-dialog").close();
  const net = ensureNetwork(state, m.network);
  const buffer = ensureBuffer(net, m.buffer);
  try {
    const context = await api.around(m.id, 30);
    const previous = activeBuffer(state);
    if (previous && !isActive(m.network, buffer.key)) markReadNow(state.active.network, previous);
    setActive(state, m.network, buffer.key);
    showContext(buffer, context, m.id);
    renderAll({ scroll: "target" });
  } catch (error) {
    fail(error);
  }
}

async function backToLive() {
  const buffer = activeBuffer(state);
  if (!buffer) return;
  leaveContext(buffer);
  renderHeader();
  await loadHistory(state.active.network, buffer);
}

// ---- wiring --------------------------------------------------------------

function setWindowFocus(focused) {
  const buffer = setFocused(state, focused);
  renderSidebar();
  if (buffer && state.active) scheduleMarkRead(state.active.network, buffer, 200);
}

function bindEvents() {
  // A link must never navigate the application window, whatever it is and
  // however it was activated. This runs first, for every anchor on the page.
  document.addEventListener(
    "click",
    (event) => {
      const anchor = event.target.closest?.("a[href]");
      if (!anchor) return;
      event.preventDefault();
      const url = anchor.dataset.url;
      if (url) api.openUrl(url).catch(fail);
    },
    true,
  );
  document.addEventListener("auxclick", (event) => event.target.closest?.("a[href]") && event.preventDefault(), true);

  window.addEventListener("focus", () => setWindowFocus(true));
  window.addEventListener("blur", () => setWindowFocus(false));
  document.addEventListener("visibilitychange", () => setWindowFocus(!document.hidden && document.hasFocus()));

  $("buffer-list").addEventListener("click", (event) => {
    const target = event.target.closest("button");
    if (!target) return;
    const { network, key, action } = target.dataset;
    if (action === "collapse") {
      const net = state.networks.get(network);
      net.collapsed = !net.collapsed;
      renderSidebar();
      return;
    }
    if (action === "connect") return void connectProfile(network);
    if (action === "disconnect") return void api.disconnect(network).catch(fail);
    if (action === "close") {
      const buffer = findBuffer(state, network, key);
      if (buffer?.kind === "channel" && buffer.joined) api.part(network, buffer.name).catch(fail);
      closeBuffer(state, network, key);
      renderAll();
      return;
    }
    if (key !== undefined) activate(network, key);
  });

  $("messages").addEventListener("click", (event) => {
    const nick = event.target.closest?.("button.nick");
    if (nick) insertAtCaret(`${nick.dataset.nick}: `);
  });
  $("messages").addEventListener("scroll", () => {
    const box = messagesBox();
    if (atBottom() && view.unseen > 0) {
      view.unseen = 0;
      updatePill();
    }
    const buffer = activeBuffer(state);
    if (buffer && box.scrollTop < 120) loadOlder(buffer);
  });
  $("new-pill").addEventListener("click", () => {
    messagesBox().scrollTop = messagesBox().scrollHeight;
  });

  $("members").addEventListener("click", (event) => {
    const button = event.target.closest("button.member");
    const net = activeNet();
    if (!button || !net) return;
    const opened = ensureBuffer(net, button.dataset.nick, "query");
    activate(net.id, opened.key);
  });

  const input = $("input");
  $("composer").addEventListener("submit", (event) => {
    event.preventDefault();
    submit();
  });
  input.addEventListener("input", () => {
    view.completion = null;
    resizeInput();
  });
  input.addEventListener("keydown", (event) => {
    if (event.isComposing) return;
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      submit();
    } else if (event.key === "Tab" && !event.shiftKey) {
      const buffer = activeBuffer(state);
      if (!buffer || buffer.kind !== "channel") return;
      const nicks = buffer.members.map((m) => m.nick);
      const result = completeNick(input.value, input.selectionStart, nicks, view.completion);
      if (result) {
        event.preventDefault();
        input.value = result.text;
        input.selectionStart = input.selectionEnd = result.caret;
        view.completion = result.state;
        resizeInput();
      }
    } else if (event.key === "ArrowUp" && input.value === "" && view.sent.length > 0) {
      event.preventDefault();
      view.sentIndex = view.sentIndex === -1 ? view.sent.length - 1 : Math.max(0, view.sentIndex - 1);
      input.value = view.sent[view.sentIndex];
      resizeInput();
    } else if (event.key === "ArrowDown" && view.sentIndex !== -1) {
      event.preventDefault();
      view.sentIndex += 1;
      input.value = view.sentIndex >= view.sent.length ? "" : view.sent[view.sentIndex];
      if (view.sentIndex >= view.sent.length) view.sentIndex = -1;
      resizeInput();
    } else if (event.key === "PageUp" || event.key === "PageDown") {
      event.preventDefault();
      messagesBox().scrollBy({ top: (event.key === "PageUp" ? -1 : 1) * messagesBox().clientHeight * 0.9 });
    }
  });

  document.addEventListener("keydown", (event) => {
    const command = event.ctrlKey || event.metaKey;
    if (command && event.key.toLowerCase() === "k") {
      event.preventDefault();
      openSearch();
    } else if (command && event.key === ",") {
      event.preventDefault();
      openSettings();
    } else if (event.altKey && (event.key === "ArrowUp" || event.key === "ArrowDown")) {
      event.preventDefault();
      cycleBuffer(event.key === "ArrowDown" ? 1 : -1);
    }
  });

  for (const id of ["btn-search", "btn-search-side"]) $(id).addEventListener("click", () => openSearch());
  for (const id of ["btn-settings-side"]) $(id).addEventListener("click", openSettings);
  $("btn-networks").addEventListener("click", openNetworks);
  $("btn-add-network").addEventListener("click", () => openProfileEditor(null));
  $("welcome-add").addEventListener("click", () => openProfileEditor(null));
  $("btn-live").addEventListener("click", backToLive);
  $("toggle-sidebar").addEventListener("click", () => document.body.classList.toggle("show-sidebar"));
  $("toggle-members").addEventListener("click", () => {
    // On a wide screen the panel is a column that can be hidden; on a narrow one
    // it is a drawer that can be opened. Either way "pressed" means "showing".
    const narrow = window.matchMedia("(max-width: 900px)").matches;
    const showing = narrow
      ? document.body.classList.toggle("show-members")
      : !document.body.classList.toggle("members-hidden");
    $("toggle-members").setAttribute("aria-pressed", String(showing));
  });

  // Profile editor
  $("profile-form").addEventListener("submit", (event) => {
    event.preventDefault();
    saveProfileFromForm();
  });
  $("btn-profile-cancel").addEventListener("click", () => $("profile-dialog").close());
  $("p-preset").addEventListener("change", () => applyPreset(PRESETS.find((p) => p.id === $("p-preset").value)));

  // Search
  const debounced = () => {
    clearTimeout(search.timer);
    search.timer = setTimeout(runSearch, 150);
  };
  $("search-input").addEventListener("input", debounced);
  $("search-scope").addEventListener("change", runSearch);
  $("search-order").addEventListener("change", runSearch);
  $("search-input").addEventListener("keydown", (event) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      selectHit(search.selected + 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      selectHit(search.selected - 1);
    } else if (event.key === "Enter") {
      event.preventDefault();
      const hit = search.hits[search.selected];
      if (hit) openHit(hit);
    }
  });
  $("search-results").addEventListener("click", (event) => {
    const item = event.target.closest("li[data-index]");
    if (item) openHit(search.hits[Number(item.dataset.index)]);
  });

  bindSettings();

  // While reconnecting, keep the countdown honest.
  setInterval(() => {
    const net = activeNet();
    if (net?.status === "waiting") renderHeader();
  }, 1000);
}

// Connects the networks marked to connect at startup, a moment apart so they
// do not all open at the same instant.
function autoconnect() {
  profiles
    .filter((p) => p.autoconnect)
    .forEach((p, index) => setTimeout(() => connectProfile(p.id, { auto: true }), 250 * index));
}

async function init() {
  state.focused = document.hasFocus();
  bindEvents();

  try {
    settings = { ...settings, ...(await api.getSettings()) };
  } catch (error) {
    fail(error);
  }
  buildSettingsControls();
  applySettings({}, { rerender: false });

  await api.onEvent(onEnvelope);
  try {
    const notices = await api.startupNotices();
    if (notices.length > 0) toast(notices.join("\n"), api.mode === "tauri" ? "error" : "info");
  } catch {
    // Not fatal: the notices are informational.
  }

  await refreshProfiles();
  await loadKnownBuffers();
  if (profiles.length === 0) {
    renderAll();
    return;
  }
  const first = state.networks.values().next().value;
  await activate(first.id, SERVER);
  autoconnect();
}

// Exposed for automated checks; harmless in the application.
window.__rhizome = {
  state,
  api,
  settings: () => settings,
  language,
  setFocus: setWindowFocus,
};

init().catch(fail);

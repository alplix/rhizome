// The interface's controller: it listens to the backend, keeps `state` up to
// date, and draws it. Anything that can be decided without the DOM lives in
// lib.js or state.js, where it is tested.

import { api } from "./api.js";
import {
  SERVER,
  activeBuffer,
  addSystem,
  applyEnvelope,
  closeBuffer,
  createState,
  ensureBuffer,
  ensureNetwork,
  findBuffer,
  leaveContext,
  mergeHistory,
  prependHistory,
  setActive,
  showContext,
  MAX_LINES,
} from "./state.js";
import { HELP, completeNick, fold, isChannelName, nickHue, parseInput } from "./lib.js";
import { el, lineTime, renderLines } from "./render.js";

const $ = (id) => document.getElementById(id);
const PAGE = 100;

const state = createState();
let profiles = [];

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
    case "registered": return net.nick ? `Connected as ${net.nick}` : "Connected";
    case "connecting": return "Connecting…";
    case "connected": return "Logging in…";
    case "waiting": {
      const seconds = Math.max(0, Math.ceil(((net.retryAt ?? Date.now()) - Date.now()) / 1000));
      return `Reconnecting in ${seconds}s…`;
    }
    case "failed": return "Disconnected";
    default: return "Not connected";
  }
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
  b.title = buffer.highlights > 0 ? `${buffer.unread} unread, ${buffer.highlights} mentioning you` : `${buffer.unread} unread`;
  return b;
}

function renderSidebar() {
  const list = $("buffer-list");
  list.replaceChildren();
  for (const net of state.networks.values()) {
    const section = el("section", "network");
    const head = el("div", "network-head");

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

    const toggle = el("button", "link-btn", isLive(net) ? "Disconnect" : "Connect");
    toggle.type = "button";
    toggle.dataset.network = net.id;
    toggle.dataset.action = isLive(net) ? "disconnect" : "connect";
    head.append(name, toggle);

    const buffers = el("ul", "buffers");
    for (const key of orderedKeys(net)) {
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
      close.title = buffer.kind === "channel" && buffer.joined ? "Leave channel" : "Close";
      close.setAttribute("aria-label", close.title);
      item.append(button, close);
      buffers.append(item);
    }
    section.append(head, buffers);
    list.append(section);
  }
  updateTitle();
}

function updateTitle() {
  let mentions = 0;
  for (const net of state.networks.values()) for (const b of net.buffers.values()) mentions += b.highlights;
  document.title = mentions > 0 ? `(${mentions}) Rhizome` : "Rhizome";
}

// ---- header, members, composer -------------------------------------------

function renderHeader() {
  const buffer = activeBuffer(state);
  const net = activeNet();
  const status = $("status");
  if (!buffer || !net) {
    $("title-name").textContent = "Rhizome";
    $("title-topic").textContent = "Add a network to get started.";
    status.textContent = "";
    $("context-banner").hidden = true;
    return;
  }
  $("title-name").textContent = buffer.kind === "server" ? net.name : buffer.name;
  const topic = buffer.kind === "channel" ? (buffer.topic ?? "") : buffer.kind === "server" ? `${net.name} · server messages` : "Private conversation";
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
    $("members-title").textContent = buffer?.kind === "query" ? "Conversation" : "Members";
    return;
  }
  $("members-title").textContent = `Members · ${buffer.members.length}`;
  const fragment = document.createDocumentFragment();
  for (const member of buffer.members) {
    const item = el("li");
    const button = el("button", "member");
    button.type = "button";
    button.dataset.nick = member.nick;
    button.title = `Message ${member.nick}`;
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
  if (!buffer) {
    input.placeholder = "Add a network to begin";
  } else if (buffer.kind === "server") {
    input.placeholder = "Commands only here. Try /join #channel or /help";
  } else {
    input.placeholder = `Message ${buffer.name}`;
  }
}

function renderAll(options) {
  renderSidebar();
  renderHeader();
  renderMembers();
  renderComposer();
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
  if (view.unseen > 0) pill.textContent = `↓ ${view.unseen} new message${view.unseen === 1 ? "" : "s"}`;
}

function emptyText(buffer) {
  if (!buffer) return "Add a network with the + button to get started.";
  if (buffer.kind === "server") return "Messages from the server appear here.";
  return "No messages yet.";
}

// Redraws the active buffer. `scroll` is "bottom", "target" (the search result)
// or "keep" (after loading older history, so the view does not jump).
function renderMessages({ scroll = "bottom", previousHeight = 0 } = {}) {
  const buffer = activeBuffer(state);
  const box = messagesBox();
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
    view.unseen += fresh.filter((l) => l.kind === "message" && !l.message.own).length;
    updatePill();
  }
}

async function loadHistory(network, buffer) {
  if (buffer.kind === "server") {
    buffer.loaded = true;
    return;
  }
  try {
    const page = await api.scrollback(network, buffer.name, null, PAGE);
    mergeHistory(buffer, page, { pageSize: PAGE });
  } catch (error) {
    buffer.loaded = true;
    fail(error);
  }
  if (isActive(network, buffer.key)) renderMessages({ scroll: "bottom" });
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
  const buffer = setActive(state, network, key);
  if (!buffer) return;
  view.completion = null;
  document.body.classList.remove("show-sidebar", "show-members");
  renderAll({ scroll: "bottom" });
  $("input").focus();
  if (!buffer.loaded) await loadHistory(network, buffer);
}

function flatBuffers() {
  const out = [];
  for (const net of state.networks.values()) {
    out.push([net.id, SERVER]);
    for (const key of orderedKeys(net)) out.push([net.id, key]);
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
          for (const b of list) if (net && !isChannelName(b.name)) ensureBuffer(net, b.name, "query");
          renderSidebar();
        })
        .catch(() => {});
      break;
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

// ---- sending -------------------------------------------------------------

async function paste(text) {
  const lines = text.split("\n").filter((l) => l.trim() !== "");
  if (lines.length <= 3) return true;
  $("paste-lead").textContent = `This will send ${lines.length} separate messages to ${activeBuffer(state)?.name ?? "the channel"}.`;
  $("paste-preview").textContent = lines.slice(0, 8).join("\n") + (lines.length > 8 ? `\n… and ${lines.length - 8} more` : "");
  $("btn-paste-send").textContent = `Send ${lines.length} messages`;
  const answer = await ask($("paste-dialog"), {
    form: $("paste-form"),
    cancel: $("btn-paste-cancel"),
    answer: () => true,
  });
  return answer === true;
}

async function runAction(action, net, buffer) {
  const target = buffer.name;
  switch (action.type) {
    case "message":
    case "action": {
      if (buffer.kind === "server") {
        toast("Pick a channel or conversation first. Try /join #channel", "error");
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
    case "help":
      for (const line of HELP) addSystem(state, net.id, buffer.key, line, "info");
      appendNew(buffer);
      return true;
    case "error":
      addSystem(state, net.id, buffer.key, action.text, "error");
      appendNew(buffer);
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
    toast("Add a network first.", "error");
    return;
  }
  const action = parseInput(text, { buffer: buffer.kind === "server" ? SERVER : buffer.name, isChannel: buffer.kind === "channel" });
  if (!action) return;
  if (["message", "action", "notice", "query", "join", "part", "nick", "raw"].includes(action.type) && net.status !== "registered") {
    toast(`${net.name} is not connected.`, "error");
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
    const server = net.buffers.get(SERVER);
    server.name = p.name;
  }
}

function renderProfileList() {
  const list = $("profile-list");
  list.replaceChildren();
  if (profiles.length === 0) {
    list.append(el("li", "none", "No networks yet."));
    return;
  }
  for (const p of profiles) {
    const net = state.networks.get(p.id);
    const item = el("li");
    const info = el("div", "info");
    info.append(el("strong", undefined, p.name), el("span", undefined, `${p.host}:${p.port}${p.tls ? "" : " (no TLS)"} · ${p.nick}${p.sasl_account ? ` · account ${p.sasl_account}` : ""}`));

    const connect = el("button", undefined, net && isLive(net) ? "Disconnect" : "Connect");
    connect.type = "button";
    connect.addEventListener("click", async () => {
      if (net && isLive(net)) await api.disconnect(p.id).catch(fail);
      else await connectProfile(p.id);
      renderProfileList();
    });
    const edit = el("button", undefined, "Edit");
    edit.type = "button";
    edit.addEventListener("click", () => openProfileEditor(p));
    const remove = el("button", undefined, "Delete");
    remove.type = "button";
    remove.addEventListener("click", async () => {
      if (net && isLive(net)) return toast("Disconnect before deleting a network.", "error");
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
    item.append(info, connect, edit, remove);
    list.append(item);
  }
}

function openNetworks() {
  renderProfileList();
  $("networks-dialog").showModal();
}

function openProfileEditor(profile) {
  const dialog = $("profile-dialog");
  const form = $("profile-form");
  form.dataset.editing = profile?.id ?? "";
  $("profile-title").textContent = profile ? `Edit ${profile.name}` : "Add a network";
  $("p-name").value = profile?.name ?? "";
  $("p-host").value = profile?.host ?? "";
  $("p-port").value = String(profile?.port ?? 6697);
  $("p-tls").checked = profile?.tls ?? true;
  $("p-nick").value = profile?.nick ?? "";
  $("p-username").value = profile?.username ?? "";
  $("p-realname").value = profile?.realname ?? "Rhizome";
  $("p-channels").value = (profile?.channels ?? []).join(", ");
  $("p-account").value = profile?.sasl_account ?? "";
  $("profile-error").hidden = true;
  if (!dialog.open) dialog.showModal();
  $("p-name").focus();
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

async function askPassword(profile) {
  const input = $("password-input");
  $("password-account").textContent = profile.sasl_account;
  input.value = "";
  try {
    const pending = ask($("password-dialog"), {
      form: $("password-form"),
      cancel: $("btn-password-cancel"),
      answer: () => input.value,
    });
    input.focus();
    return await pending;
  } finally {
    // Whatever was typed is wiped as soon as it has been read.
    input.value = "";
  }
}

async function connectProfile(id) {
  const profile = profiles.find((p) => p.id === id);
  if (!profile) return;
  let password = null;
  if (profile.sasl_account) {
    password = await askPassword(profile);
    if (password === null) return;
  }
  try {
    await api.connect(id, password);
    await activate(id, SERVER);
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
    const none = el("li", "search-none", query.trim() ? "Nothing found." : "Type to search every message you have logged.");
    list.append(none);
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
    meta.append(
      el("span", "where", m.buffer),
      el("span", undefined, m.sender),
      el("time", undefined, new Date(m.time_ms).toLocaleString()),
    );
    const snippet = el("div", "hit-snippet");
    for (const part of hit.snippet) {
      snippet.append(part.hit ? el("mark", undefined, part.text) : document.createTextNode(part.text));
    }
    item.append(meta, snippet);
    list.append(item);
  });
  $("search-count").textContent = `${hits.length}${hits.length >= 50 ? "+" : ""} result${hits.length === 1 ? "" : "s"}`;
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

  $("buffer-list").addEventListener("click", (event) => {
    const target = event.target.closest("button");
    if (!target) return;
    const { network, key, action } = target.dataset;
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
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
      event.preventDefault();
      openSearch();
    } else if (event.altKey && (event.key === "ArrowUp" || event.key === "ArrowDown")) {
      event.preventDefault();
      cycleBuffer(event.key === "ArrowDown" ? 1 : -1);
    }
  });

  $("btn-search").addEventListener("click", () => openSearch());
  $("btn-networks").addEventListener("click", openNetworks);
  $("btn-add-network").addEventListener("click", () => {
    $("networks-dialog").close();
    openProfileEditor(null);
  });
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

  // While reconnecting, keep the countdown honest.
  setInterval(() => {
    const net = activeNet();
    if (net?.status === "waiting") renderHeader();
  }, 1000);
}

async function init() {
  bindEvents();
  await api.onEvent(onEnvelope);
  try {
    const notices = await api.startupNotices();
    if (notices.length > 0) toast(notices.join("\n"), api.mode === "tauri" ? "error" : "info");
  } catch {
    // Not fatal: the notices are informational.
  }
  await refreshProfiles();
  if (profiles.length === 0) {
    renderAll();
    openProfileEditor(null);
    return;
  }
  const first = state.networks.values().next().value;
  await activate(first.id, SERVER);
}

// Exposed for the demo's automated checks; harmless in the application.
window.__rhizome = { state, api };

init().catch(fail);

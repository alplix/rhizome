import test from "node:test";
import assert from "node:assert/strict";

import {
  createState,
  ensureNetwork,
  ensureBuffer,
  findBuffer,
  activeBuffer,
  applyEnvelope,
  setActive,
  closeBuffer,
  mergeHistory,
  prependHistory,
  showContext,
  leaveContext,
  sortMembers,
  addSystem,
  SERVER,
  MAX_LINES,
} from "../state.js";

const NET = "libera";
const now = 1_000_000;

const send = (state, event, network = NET, at = now) => applyEnvelope(state, { network, event }, at);

const msg = (over = {}) => ({
  buffer: "#rhizome",
  sender: "bob",
  kind: "privmsg",
  spans: [{ text: "hi" }],
  plain: "hi",
  time_ms: now,
  own: false,
  highlight: false,
  ...over,
});

const chat = (state, over, network = NET) => send(state, { type: "message", message: msg(over) }, network);

function inChannel() {
  const state = createState();
  send(state, { type: "registered", nick: "alp" });
  send(state, { type: "joined", channel: "#rhizome" });
  send(state, {
    type: "names",
    channel: "#rhizome",
    members: [
      { nick: "alp", prefixes: "@" },
      { nick: "bob", prefixes: "+" },
      { nick: "carol", prefixes: "" },
    ],
  });
  return state;
}

const channel = (state) => findBuffer(state, NET, "#rhizome");
const nicks = (state) => channel(state).members.map((m) => m.nick);

// ---- connection lifecycle ---------------------------------------------------------

test("a network starts with a server buffer and follows the connection through its states", () => {
  const state = createState();
  send(state, { type: "connecting" });
  const net = state.networks.get(NET);
  assert.equal(net.status, "connecting");
  assert.ok(net.buffers.has(SERVER));

  send(state, { type: "connected" });
  assert.equal(net.status, "connected");
  const effects = send(state, { type: "registered", nick: "alp" });
  assert.equal(net.status, "registered");
  assert.equal(net.nick, "alp");
  assert.deepEqual(effects, [{ type: "registered", network: NET }]);
});

test("a dropped connection with a retry is waiting, and one without is failed", () => {
  const state = createState();
  send(state, { type: "registered", nick: "alp" });
  send(state, { type: "disconnected", reason: "ping timeout", retry_in_ms: 2500 });
  const net = state.networks.get(NET);
  assert.equal(net.status, "waiting");
  assert.equal(net.retryAt, now + 2500);

  send(state, { type: "disconnected", reason: "login failed" });
  assert.equal(net.status, "failed");
  assert.equal(net.retryAt, null);

  // Closing after a failure must not paper over it.
  send(state, { type: "closed" });
  assert.equal(net.status, "failed");
});

test("closed after an ordinary disconnect returns the network to idle", () => {
  const state = createState();
  send(state, { type: "registered", nick: "alp" });
  send(state, { type: "closed" });
  assert.equal(state.networks.get(NET).status, "idle");
});

test("a disconnect marks joined channels as left and notes the gap in each", () => {
  const state = inChannel();
  send(state, { type: "disconnected", reason: "x", retry_in_ms: 1000 });
  assert.equal(channel(state).joined, false);
  assert.equal(channel(state).lines.at(-1).text, "Disconnected");
});

test("errors and login failures interrupt with a toast", () => {
  const state = createState();
  send(state, { type: "auth_failed", reason: "bad password (904)" });
  assert.match(state.toast.text, /bad password/);
  send(state, { type: "error", code: 474, text: "#x Cannot join channel (+b)" });
  assert.equal(state.toast.level, "error");
  const server = state.networks.get(NET).buffers.get(SERVER);
  assert.ok(server.lines.some((l) => l.text.includes("474")));
});

test("an event with no network is a global notice", () => {
  const state = createState();
  applyEnvelope(state, { network: "", event: { type: "error", code: 0, text: "could not save to the log" } }, now);
  assert.deepEqual(state.notices, ["could not save to the log"]);
  assert.equal(state.networks.size, 0);
});

// ---- channels and members ----------------------------------------------------------

test("joining creates a channel buffer and asks to be shown", () => {
  const state = createState();
  const effects = send(state, { type: "joined", channel: "#rhizome" });
  assert.equal(channel(state).joined, true);
  assert.deepEqual(effects, [{ type: "joined", network: NET, key: "#rhizome" }]);
});

test("a names list sets the members in the order given", () => {
  const state = inChannel();
  assert.deepEqual(nicks(state), ["alp", "bob", "carol"]);
});

test("members join, part, quit and are kicked", () => {
  const state = inChannel();
  send(state, { type: "member_joined", channel: "#rhizome", nick: "dave" });
  assert.ok(nicks(state).includes("dave"));
  send(state, { type: "member_parted", channel: "#rhizome", nick: "dave", reason: "later" });
  assert.ok(!nicks(state).includes("dave"));

  send(state, { type: "member_quit", nick: "carol", reason: "gone", channels: ["#rhizome"] });
  assert.ok(!nicks(state).includes("carol"));

  send(state, { type: "member_kicked", channel: "#rhizome", nick: "bob", by: "op", reason: "x" });
  assert.deepEqual(nicks(state), ["alp"]);
});

test("a member joining twice is listed once", () => {
  const state = inChannel();
  send(state, { type: "member_joined", channel: "#rhizome", nick: "bob" });
  assert.equal(nicks(state).filter((n) => n === "bob").length, 1);
});

test("member matching follows the network's name folding", () => {
  const state = createState();
  send(state, { type: "joined", channel: "#c" });
  send(state, { type: "names", channel: "#c", members: [{ nick: "Dave[away]", prefixes: "" }] });
  send(state, { type: "member_parted", channel: "#C", nick: "dave{away}" });
  assert.deepEqual(findBuffer(state, NET, "#c").members, []);
});

test("a nick change renames the member, keeps the prefix and notes it in shared channels only", () => {
  const state = inChannel();
  ensureBuffer(state.networks.get(NET), "#other", "channel");
  send(state, { type: "nick_changed", old: "bob", new: "robert", channels: ["#rhizome"], own: false });
  const robert = channel(state).members.find((m) => m.nick === "robert");
  assert.equal(robert.prefixes, "+");
  assert.ok(!nicks(state).includes("bob"));
  assert.ok(channel(state).lines.at(-1).text.includes("bob is now known as robert"));
  assert.equal(findBuffer(state, NET, "#other").lines.length, 0);
});

test("our own nick change updates the network's nick", () => {
  const state = inChannel();
  send(state, { type: "nick_changed", old: "alp", new: "alp_away", channels: ["#rhizome"], own: true });
  assert.equal(state.networks.get(NET).nick, "alp_away");
});

test("leaving or being kicked empties the channel", () => {
  const state = inChannel();
  send(state, { type: "kicked", channel: "#rhizome", by: "op", reason: "behave" });
  assert.equal(channel(state).joined, false);
  assert.deepEqual(channel(state).members, []);
  assert.match(state.toast.text, /Removed from #rhizome/);

  const state2 = inChannel();
  send(state2, { type: "parted", channel: "#rhizome" });
  assert.equal(channel(state2).joined, false);
});

test("the topic is stored and cleared", () => {
  const state = inChannel();
  send(state, { type: "topic", channel: "#rhizome", topic: "welcome" });
  assert.equal(channel(state).topic, "welcome");
  send(state, { type: "topic", channel: "#rhizome" });
  assert.equal(channel(state).topic, null);
});

test("a privilege change asks for a fresh names list; other modes do not", () => {
  const state = inChannel();
  let effects = send(state, { type: "mode", target: "#rhizome", by: "op", modes: "+o carol" });
  assert.deepEqual(effects, [{ type: "refresh_names", network: NET, channel: "#rhizome" }]);
  effects = send(state, { type: "mode", target: "#rhizome", by: "op", modes: "+m" });
  assert.deepEqual(effects, []);
  // A user mode goes to the server buffer, not a channel.
  effects = send(state, { type: "mode", target: "alp", by: "alp", modes: "+i" });
  assert.deepEqual(effects, []);
});

test("sortMembers orders by privilege then name", () => {
  const sorted = sortMembers([
    { nick: "zed", prefixes: "" },
    { nick: "amy", prefixes: "+" },
    { nick: "bob", prefixes: "@" },
    { nick: "Cal", prefixes: "" },
    { nick: "dan", prefixes: "~" },
  ]);
  assert.deepEqual(sorted.map((m) => m.nick), ["dan", "bob", "amy", "Cal", "zed"]);
});

// ---- messages and unread counts ---------------------------------------------------

test("a message creates its buffer and is shown when that buffer is active", () => {
  const state = createState();
  chat(state, {});
  setActive(state, NET, "#rhizome");
  assert.equal(channel(state).lines.length, 1);
  chat(state, { plain: "second", msgid: "m2" });
  assert.equal(channel(state).unread, 0, "the buffer on screen has nothing unread");
});

test("messages in a buffer that is not on screen count as unread, and mentions as highlights", () => {
  const state = inChannel();
  setActive(state, NET, SERVER);
  chat(state, { msgid: "a" });
  chat(state, { msgid: "b", highlight: true });
  assert.equal(channel(state).unread, 2);
  assert.equal(channel(state).highlights, 1);
});

test("our own messages are never unread", () => {
  const state = inChannel();
  setActive(state, NET, SERVER);
  chat(state, { own: true, msgid: "mine" });
  assert.equal(channel(state).unread, 0);
});

test("opening a buffer clears its counters but keeps the unread marker until you leave", () => {
  const state = inChannel();
  setActive(state, NET, SERVER);
  const before = channel(state).lines.length;
  chat(state, { msgid: "a" });
  chat(state, { msgid: "b" });
  assert.equal(channel(state).unreadFrom, before);

  setActive(state, NET, "#rhizome");
  assert.equal(channel(state).unread, 0);
  assert.equal(channel(state).unreadFrom, before, "the marker stays so you can see where you left off");

  setActive(state, NET, SERVER);
  assert.equal(channel(state).unreadFrom, null);
});

test("a private message opens a query buffer named for the sender", () => {
  const state = createState();
  chat(state, { buffer: "dave", sender: "dave", highlight: true });
  const query = findBuffer(state, NET, "dave");
  assert.equal(query.kind, "query");
  assert.equal(query.highlights, 1);
});

test("buffer names match case-insensitively but show the latest spelling", () => {
  const state = createState();
  chat(state, { buffer: "#Rhizome", msgid: "1" });
  chat(state, { buffer: "#rhizome", msgid: "2" });
  const net = state.networks.get(NET);
  assert.equal([...net.buffers.keys()].filter((k) => k !== SERVER).length, 1);
  assert.equal(findBuffer(state, NET, "#rhizome").name, "#rhizome");
});

test("the same message arriving twice is shown once", () => {
  const state = inChannel();
  chat(state, { msgid: "same" });
  chat(state, { msgid: "same" });
  chat(state, { msgid: undefined, plain: "no id", time_ms: 5 });
  chat(state, { msgid: undefined, plain: "no id", time_ms: 5 });
  assert.equal(channel(state).lines.filter((l) => l.kind === "message").length, 2);
});

test("a buffer keeps at most MAX_LINES lines and adjusts its unread marker", () => {
  const state = inChannel();
  setActive(state, NET, SERVER);
  for (let i = 0; i < MAX_LINES + 50; i += 1) chat(state, { msgid: `m${i}`, time_ms: i });
  const buffer = channel(state);
  assert.equal(buffer.lines.length, MAX_LINES);
  assert.ok(buffer.unreadFrom >= 0 && buffer.unreadFrom < MAX_LINES);
  assert.equal(buffer.hasMore, true, "older lines exist in the log");
});

test("closing a buffer removes it and falls back to the server buffer", () => {
  const state = createState();
  chat(state, { buffer: "dave", sender: "dave" });
  setActive(state, NET, "dave");
  closeBuffer(state, NET, "dave");
  assert.equal(findBuffer(state, NET, "dave"), null);
  assert.equal(activeBuffer(state).key, SERVER);
  closeBuffer(state, NET, SERVER);
  assert.ok(findBuffer(state, NET, SERVER), "the server buffer cannot be closed");
});

// ---- history --------------------------------------------------------------------------

const stored = (id, time_ms, over = {}) =>
  msg({ id, time_ms, msgid: `s${id}`, plain: `stored ${id}`, spans: [{ text: `stored ${id}` }], ...over });

test("merging history puts stored messages first and drops live duplicates of them", () => {
  const state = inChannel();
  chat(state, { msgid: "s2", time_ms: 200, plain: "stored 2" }); // arrived live, also in the log
  chat(state, { msgid: "live", time_ms: 300, plain: "only live" });
  mergeHistory(channel(state), [stored(1, 100), stored(2, 200)]);

  const texts = channel(state).lines.filter((l) => l.kind === "message").map((l) => l.message.plain);
  assert.deepEqual(texts, ["stored 1", "stored 2", "only live"]);
  assert.equal(channel(state).loaded, true);
});

test("merging history de-duplicates messages that have no id by time, sender and text", () => {
  const state = inChannel();
  chat(state, { msgid: undefined, time_ms: 100, plain: "no id", sender: "bob" });
  mergeHistory(channel(state), [msg({ id: 1, time_ms: 100, plain: "no id", sender: "bob", msgid: undefined })]);
  assert.equal(channel(state).lines.filter((l) => l.kind === "message").length, 1);
});

test("merging keeps system lines and interleaves them by time", () => {
  const state = createState();
  send(state, { type: "joined", channel: "#rhizome" }, NET, 150);
  mergeHistory(channel(state), [stored(1, 100), stored(2, 200)]);
  const order = channel(state).lines.map((l) => (l.kind === "message" ? l.message.plain : l.text));
  assert.deepEqual(order, ["stored 1", "You joined #rhizome", "stored 2"]);
});

test("a full page says there may be more history, a short one says there is not", () => {
  const state = inChannel();
  const full = Array.from({ length: 100 }, (_, i) => stored(i + 1, i));
  mergeHistory(channel(state), full);
  assert.equal(channel(state).hasMore, true);
  mergeHistory(channel(state), full.slice(0, 5));
  assert.equal(channel(state).hasMore, false);
});

test("prepending an older page adds only messages not already shown", () => {
  const state = inChannel();
  mergeHistory(channel(state), [stored(3, 300), stored(4, 400)], { pageSize: 2 });
  const added = prependHistory(channel(state), [stored(2, 200), stored(3, 300)], { pageSize: 2 });
  assert.equal(added, 1, "message 3 was already shown");
  const texts = channel(state).lines.filter((l) => l.kind === "message").map((l) => l.message.plain);
  assert.deepEqual(texts, ["stored 2", "stored 3", "stored 4"]);
});

test("showing a search result replaces the lines, and leaving reloads", () => {
  const state = inChannel();
  chat(state, { msgid: "live" });
  showContext(channel(state), [stored(7, 700), stored(8, 800)], 8);
  assert.deepEqual(channel(state).context, { targetId: 8 });
  assert.equal(channel(state).lines.length, 2);

  leaveContext(channel(state));
  assert.equal(channel(state).context, null);
  assert.equal(channel(state).loaded, false, "the live end has to be loaded again");
});

test("leaving a buffer drops any search-result view it was in", () => {
  const state = inChannel();
  setActive(state, NET, "#rhizome");
  showContext(channel(state), [stored(1, 100)], 1);
  setActive(state, NET, SERVER);
  assert.equal(channel(state).context, null);
});

test("addSystem writes a line to a named buffer and ignores an unknown one", () => {
  const state = inChannel();
  addSystem(state, NET, "#rhizome", "hello", "info", 5);
  assert.equal(channel(state).lines.at(-1).text, "hello");
  addSystem(state, NET, "#nowhere", "x");
  addSystem(state, "nonet", SERVER, "x");
});

test("ensureNetwork is idempotent", () => {
  const state = createState();
  const a = ensureNetwork(state, "x", "Name");
  const b = ensureNetwork(state, "x", "Other");
  assert.equal(a, b);
  assert.equal(a.name, "Name");
});

test("the total counts every line ever added, including ones trimmed away", () => {
  const state = inChannel();
  const before = channel(state).total;
  for (let i = 0; i < MAX_LINES + 10; i += 1) chat(state, { msgid: `t${i}`, time_ms: i });
  assert.equal(channel(state).total - before, MAX_LINES + 10);
  assert.equal(channel(state).lines.length, MAX_LINES);
});

test("formatting codes in topics, server text and reasons are not shown as control characters", () => {
  const state = inChannel();
  send(state, { type: "topic", channel: "#rhizome", topic: "\x02Welcome\x02 to \x0304rhizome\x03" });
  assert.equal(channel(state).topic, "Welcome to rhizome");
  send(state, { type: "server", text: "\x02Notice:\x02 maintenance" });
  const server = state.networks.get(NET).buffers.get(SERVER);
  assert.equal(server.lines.at(-1).text, "Notice: maintenance");
  send(state, { type: "member_quit", nick: "bob", reason: "\x0304bye\x03", channels: ["#rhizome"] });
  assert.match(channel(state).lines.at(-1).text, /quit \(bye\)/);
});

test("live lines do not join a search result window but still count as unread", () => {
  const state = inChannel();
  setActive(state, NET, "#rhizome");
  showContext(channel(state), [stored(7, 700), stored(8, 800)], 8);
  const total = channel(state).total;
  chat(state, { msgid: "live-while-viewing" });
  assert.equal(channel(state).lines.length, 2, "the fixed window is untouched");
  assert.equal(channel(state).total, total);

  // Unseen (another buffer is on screen): it counts as unread.
  const other = inChannel();
  setActive(other, NET, SERVER);
  showContext(findBuffer(other, NET, "#rhizome"), [stored(1, 1)], 1);
  chat(other, { msgid: "x", highlight: true });
  assert.equal(findBuffer(other, NET, "#rhizome").unread, 1);
  assert.equal(findBuffer(other, NET, "#rhizome").highlights, 1);
});

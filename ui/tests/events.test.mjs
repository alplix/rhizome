import test from "node:test";
import assert from "node:assert/strict";

import { describeEvent } from "../events.js";
import { setLanguage } from "../i18n.js";

test.afterEach(() => setLanguage("en"));

const line = (verb, args, over = {}) => ({ sender: "bob", buffer: "#rhizome", own: false, event: { verb, args }, ...over });

test("joins and parts name the person, or say 'you' for our own", () => {
  assert.equal(describeEvent(line("join", [])), "→ bob joined");
  assert.equal(describeEvent(line("join", [], { own: true, sender: "alp" })), "You joined #rhizome");
  assert.equal(describeEvent(line("part", [""])), "← bob left");
  assert.equal(describeEvent(line("part", ["lunch"])), "← bob left (lunch)");
  assert.equal(describeEvent(line("part", ["lunch"], { own: true })), "You left #rhizome (lunch)");
  assert.equal(describeEvent(line("part", [""], { own: true })), "You left #rhizome");
});

test("a quit gives the reason when there is one", () => {
  assert.equal(describeEvent(line("quit", [""])), "⇠ bob quit");
  assert.equal(describeEvent(line("quit", ["Ping timeout: 240 seconds"])), "⇠ bob quit (Ping timeout: 240 seconds)");
});

test("a kick names who did it and to whom", () => {
  assert.equal(describeEvent(line("kick", ["eve", "spam"], { sender: "op" })), "eve was removed by op (spam)");
  assert.equal(describeEvent(line("kick", ["eve", ""], { sender: "op" })), "eve was removed by op");
  assert.equal(describeEvent(line("kick", ["alp", "rules"], { sender: "op", own: true })), "op removed you from #rhizome (rules)");
});

test("a nick change reads from either side", () => {
  assert.equal(describeEvent(line("nick", ["robert"])), "bob is now known as robert");
  assert.equal(describeEvent(line("nick", ["alp2"], { own: true })), "You are now known as alp2");
});

test("a topic change quotes the topic, and clearing it says so", () => {
  assert.equal(describeEvent(line("topic", ["new topic"], { sender: "op" })), "op changed the topic to: new topic");
  assert.equal(describeEvent(line("topic", [""], { sender: "op" })), "op cleared the topic");
});

test("a mode change shows the modes as sent", () => {
  assert.equal(describeEvent(line("mode", ["+o carol"], { sender: "op" })), "op set mode +o carol");
});

test("formatting codes in a stranger's reason are removed, not shown as boxes", () => {
  assert.equal(describeEvent(line("quit", ["\x0304gone\x03 \x02now\x02"])), "⇠ bob quit (gone now)");
  assert.equal(describeEvent(line("topic", ["\x02Welcome\x02"], { sender: "op" })), "op changed the topic to: Welcome");
});

test("text from a stranger is not interpreted as a template", () => {
  // A reason that contains a placeholder must appear literally.
  assert.equal(describeEvent(line("part", ["{nick} {channel}"])), "← bob left ({nick} {channel})");
});

test("an unrecognised or malformed event is still shown as something", () => {
  assert.match(describeEvent(line("teleport", [])), /does not recognise/);
  assert.match(describeEvent({ sender: "x", buffer: "#c", own: false }), /does not recognise/);
  assert.equal(describeEvent(line("join", undefined)), "→ bob joined", "missing arguments are treated as none");
});

test("events read in Turkish", () => {
  setLanguage("tr");
  assert.equal(describeEvent(line("join", [])), "→ bob katıldı");
  assert.equal(describeEvent(line("join", [], { own: true })), "#rhizome kanalına katıldınız");
  assert.equal(describeEvent(line("kick", ["eve", "spam"], { sender: "op" })), "eve, op tarafından çıkarıldı (spam)");
  assert.equal(describeEvent(line("nick", ["robert"])), "bob artık robert olarak biliniyor");
});

import test from "node:test";
import assert from "node:assert/strict";

import {
  fold,
  isChannelName,
  linkify,
  isSafeWebUrl,
  contrastRatio,
  readableOn,
  nickHue,
  sameDay,
  formatTime,
  parseInput,
  completeNick,
} from "../lib.js";

// ---- names -------------------------------------------------------------------

test("fold follows RFC 1459, not plain lower-casing", () => {
  assert.equal(fold("Nick[]"), "nick{}");
  assert.equal(fold("A\\B~"), "a|b^");
  assert.equal(fold("ŞİĞ"), "ŞİĞ", "non-ASCII is left alone, as the server leaves it");
  assert.equal(fold("#Rhizome"), "#rhizome");
});

test("channel names start with a channel prefix", () => {
  for (const c of ["#a", "&local", "+modeless", "!safe"]) assert.equal(isChannelName(c), true, c);
  for (const n of ["alp", "", "a#b"]) assert.equal(isChannelName(n), false, n);
});

// ---- links -------------------------------------------------------------------

const urls = (text) => linkify(text).filter((s) => s.url).map((s) => s.url);
const rejoin = (text) => linkify(text).map((s) => s.text).join("");

test("linkify finds web addresses and keeps the text intact", () => {
  const text = "see https://github.com/alplix/rhizome/issues/1 for details";
  assert.deepEqual(urls(text), ["https://github.com/alplix/rhizome/issues/1"]);
  assert.equal(rejoin(text), text);
});

test("linkify drops the punctuation that surrounds a link in prose", () => {
  assert.deepEqual(urls("go to https://example.com."), ["https://example.com"]);
  assert.deepEqual(urls("really? https://example.com!"), ["https://example.com"]);
  assert.deepEqual(urls("(see https://example.com/a)"), ["https://example.com/a"]);
  assert.deepEqual(urls("<https://example.com>"), ["https://example.com"]);
  assert.deepEqual(urls('say "https://example.com/x",'), ["https://example.com/x"]);
});

test("linkify keeps a bracket that belongs to the address", () => {
  assert.deepEqual(
    urls("https://en.wikipedia.org/wiki/Rust_(programming_language)"),
    ["https://en.wikipedia.org/wiki/Rust_(programming_language)"],
  );
  assert.deepEqual(
    urls("(https://en.wikipedia.org/wiki/Rust_(programming_language))"),
    ["https://en.wikipedia.org/wiki/Rust_(programming_language)"],
  );
});

test("linkify finds several links and preserves everything between them", () => {
  const text = "a http://one.example b https://two.example/x c";
  assert.deepEqual(urls(text), ["http://one.example", "https://two.example/x"]);
  assert.equal(rejoin(text), text);
});

test("linkify ignores anything that is not plain http or https", () => {
  for (const text of [
    "javascript:alert(1)",
    "click javascript://%0aalert(1)",
    "file:///C:/Windows/System32/calc.exe",
    "data:text/html,<script>alert(1)</script>",
    "ms-msdt:/id PCWDiagnostic",
    "vscode://file/x",
    "ftp://example.com",
    "example.com",
    "www.example.com",
  ]) {
    assert.deepEqual(urls(text), [], text);
    assert.equal(rejoin(text), text, "text must survive untouched");
  }
});

test("linkify does not turn markup into links or elements", () => {
  const text = '<a href="https://evil.example">x</a>';
  // The address is found, but the surrounding markup stays as inert text.
  assert.equal(rejoin(text), text);
  assert.deepEqual(urls(text), ["https://evil.example"]);
});

test("linkify handles empty and link-only text", () => {
  assert.deepEqual(linkify(""), []);
  assert.deepEqual(linkify("https://example.com"), [{ text: "https://example.com", url: "https://example.com" }]);
});

test("isSafeWebUrl accepts only http(s) with a host", () => {
  assert.equal(isSafeWebUrl("https://example.com/x"), true);
  assert.equal(isSafeWebUrl("http://localhost:8080"), true);
  for (const bad of ["javascript:alert(1)", "https://", "file:///x", "", "not a url", "//example.com"]) {
    assert.equal(isSafeWebUrl(bad), false, bad);
  }
});

// ---- colour ------------------------------------------------------------------

test("contrast ratio matches the known extremes", () => {
  assert.ok(Math.abs(contrastRatio("#000000", "#ffffff") - 21) < 0.01);
  assert.equal(contrastRatio("#777777", "#777777"), 1);
});

test("readableOn lifts a dark colour on a dark background and leaves a good one alone", () => {
  const dark = "#1a1d21";
  const lifted = readableOn("#000000", dark);
  assert.ok(contrastRatio(lifted, dark) >= 3.5, `${lifted} on ${dark}`);
  // Already legible: unchanged.
  assert.equal(readableOn("#ffffff", dark), "#ffffff");
  // Dark text on a dark theme keeps its hue: blue stays blue-ish.
  const blue = readableOn("#00007f", dark);
  const [r, , b] = [1, 3, 5].map((i) => parseInt(blue.slice(i, i + 2), 16));
  assert.ok(b > r, `${blue} should still lean blue`);
});

test("readableOn darkens a pale colour on a light background", () => {
  const light = "#ffffff";
  const fixed = readableOn("#ffff00", light);
  assert.ok(contrastRatio(fixed, light) >= 3.5, fixed);
});

test("readableOn passes through values it cannot read", () => {
  assert.equal(readableOn("red", "#000000"), "red");
  assert.equal(readableOn("#ff0000", "nonsense"), "#ff0000");
});

test("nickHue is stable, folded and in range", () => {
  assert.equal(nickHue("alp"), nickHue("alp"));
  assert.equal(nickHue("Alp"), nickHue("alp"));
  assert.equal(nickHue("Nick["), nickHue("nick{"));
  for (const n of ["a", "bob", "carol", "ŞağLam", ""]) {
    const h = nickHue(n);
    assert.ok(Number.isInteger(h) && h >= 0 && h < 360, `${n} -> ${h}`);
  }
  assert.notEqual(nickHue("bob"), nickHue("carol"));
});

// ---- time ----------------------------------------------------------------------

test("time formatting is zero-padded and day comparison ignores the clock time", () => {
  const morning = new Date(2026, 8, 25, 9, 5).getTime();
  assert.equal(formatTime(morning), "09:05");
  assert.equal(sameDay(morning, new Date(2026, 8, 25, 23, 59).getTime()), true);
  assert.equal(sameDay(morning, new Date(2026, 8, 26, 0, 1).getTime()), false);
});

// ---- typed input -----------------------------------------------------------------

const chan = { buffer: "#rhizome", isChannel: true };
const query = { buffer: "bob", isChannel: false };

test("ordinary text is a message and blank input is nothing", () => {
  assert.deepEqual(parseInput("merhaba dünya", chan), { type: "message", text: "merhaba dünya" });
  assert.equal(parseInput("   ", chan), null);
  assert.equal(parseInput("", chan), null);
  // Leading spaces are the person's, trailing ones are not.
  assert.equal(parseInput("  indented  ", chan).text, "  indented");
});

test("a doubled slash sends a message that starts with a slash", () => {
  assert.deepEqual(parseInput("//shrug", chan), { type: "message", text: "/shrug" });
  assert.deepEqual(parseInput("/etc/passwd is a path", chan).type, "error", "a single slash is a command");
});

test("an unknown command is reported and never sent as chat", () => {
  const r = parseInput("/joinn #x", chan);
  assert.equal(r.type, "error");
  assert.match(r.text, /unknown command \/joinn/);
});

test("join takes one or several channels", () => {
  assert.deepEqual(parseInput("/join #a", chan), { type: "join", channels: ["#a"] });
  assert.deepEqual(parseInput("/j #a,#b,,#c", chan), { type: "join", channels: ["#a", "#b", "#c"] });
  assert.equal(parseInput("/join", chan).type, "error");
});

test("part uses the current channel unless one is named", () => {
  assert.deepEqual(parseInput("/part", chan), { type: "part", channel: "#rhizome", reason: null });
  assert.deepEqual(parseInput("/part going home", chan), { type: "part", channel: "#rhizome", reason: "going home" });
  assert.deepEqual(parseInput("/part #other bye", chan), { type: "part", channel: "#other", reason: "bye" });
  assert.equal(parseInput("/part", query).type, "error", "a private conversation is not a channel");
});

test("me, msg and notice split their arguments", () => {
  assert.deepEqual(parseInput("/me waves at everyone", chan), { type: "action", text: "waves at everyone" });
  assert.equal(parseInput("/me", chan).type, "error");
  assert.deepEqual(parseInput("/msg bob hello there", chan), { type: "query", target: "bob", text: "hello there" });
  assert.deepEqual(parseInput("/query bob", chan), { type: "query", target: "bob", text: null });
  assert.deepEqual(parseInput("/notice #c heads up", chan), { type: "notice", target: "#c", text: "heads up" });
  assert.equal(parseInput("/msg", chan).type, "error");
});

test("msg keeps the message's own spacing", () => {
  assert.equal(parseInput("/msg bob  two  spaces", chan).text, "two  spaces");
});

test("nick needs exactly one word", () => {
  assert.deepEqual(parseInput("/nick alp2", chan), { type: "nick", nick: "alp2" });
  assert.equal(parseInput("/nick a b", chan).type, "error");
  assert.equal(parseInput("/nick", chan).type, "error");
});

test("commands that become raw lines are built for the current channel", () => {
  assert.deepEqual(parseInput("/topic", chan), { type: "raw", line: "TOPIC #rhizome" });
  assert.deepEqual(parseInput("/topic new topic here", chan), { type: "raw", line: "TOPIC #rhizome :new topic here" });
  assert.deepEqual(parseInput("/whois bob", chan), { type: "raw", line: "WHOIS bob" });
  assert.deepEqual(parseInput("/names", chan), { type: "raw", line: "NAMES #rhizome" });
  assert.deepEqual(parseInput("/kick bob", chan), { type: "raw", line: "KICK #rhizome bob" });
  assert.deepEqual(parseInput("/kick bob being rude", chan), { type: "raw", line: "KICK #rhizome bob :being rude" });
  assert.deepEqual(parseInput("/away lunch", chan), { type: "raw", line: "AWAY :lunch" });
  assert.deepEqual(parseInput("/away", chan), { type: "raw", line: "AWAY" });
  assert.equal(parseInput("/topic", query).type, "error");
  assert.equal(parseInput("/kick bob", query).type, "error");
});

test("mode targets the current channel unless another target is given", () => {
  assert.deepEqual(parseInput("/mode", chan), { type: "raw", line: "MODE #rhizome" });
  assert.deepEqual(parseInput("/mode +o bob", chan), { type: "raw", line: "MODE #rhizome +o bob" });
  assert.deepEqual(parseInput("/mode #other +m", chan), { type: "raw", line: "MODE #other +m" });
});

test("raw, search, quit and help", () => {
  assert.deepEqual(parseInput("/raw WHO #c", chan), { type: "raw", line: "WHO #c" });
  assert.equal(parseInput("/raw", chan).type, "error");
  assert.deepEqual(parseInput("/search null pointer", chan), { type: "search", query: "null pointer" });
  assert.deepEqual(parseInput("/quit", chan), { type: "quit" });
  assert.deepEqual(parseInput("/help", chan), { type: "help" });
  assert.deepEqual(parseInput("/JOIN #A", chan), { type: "join", channels: ["#A"] }, "commands are case-insensitive");
});

test("commands in the server buffer have no channel to act on", () => {
  const server = { buffer: "*", isChannel: false };
  assert.equal(parseInput("/topic", server).type, "error");
  assert.equal(parseInput("/mode", server).type, "error");
  assert.deepEqual(parseInput("/join #a", server), { type: "join", channels: ["#a"] });
});

// ---- nick completion ----------------------------------------------------------------

const nicks = ["alp", "Alice", "bob", "Bobby", "carol"];

test("Tab completes a nick at the start of a line with a colon", () => {
  const r = completeNick("bo", 2, nicks, null);
  assert.equal(r.text, "bob: ");
  assert.equal(r.caret, 5);
});

test("Tab completes mid-sentence with a plain space", () => {
  const text = "thanks bo";
  const r = completeNick(text, text.length, nicks, null);
  assert.equal(r.text, "thanks bob ");
});

test("completion ignores case and cycles through the matches", () => {
  let r = completeNick("al", 2, nicks, null);
  assert.equal(r.text, "Alice: ", "sorted case-insensitively: Alice before alp");
  r = completeNick(r.text, r.caret, nicks, r.state);
  assert.equal(r.text, "alp: ");
  r = completeNick(r.text, r.caret, nicks, r.state);
  assert.equal(r.text, "Alice: ", "wraps around");
});

test("completion keeps the text after the caret", () => {
  const r = completeNick("car and more", 3, nicks, null);
  assert.equal(r.text, "carol:  and more");
  assert.equal(r.caret, 7);
});

test("nothing matches, or nothing has been typed: no change", () => {
  assert.equal(completeNick("zzz", 3, nicks, null), null);
  assert.equal(completeNick("hello ", 6, nicks, null), null);
  assert.equal(completeNick("", 0, nicks, null), null);
  assert.equal(completeNick("bo", 2, [], null), null);
});

test("completion uses the network's name folding", () => {
  const r = completeNick("nick{", 5, ["Nick[away]"], null);
  assert.equal(r.text, "Nick[away]: ");
});

test("editing the text between presses starts a fresh completion", () => {
  let r = completeNick("al", 2, nicks, null);
  const edited = completeNick(r.text + "x", r.caret + 1, nicks, r.state);
  assert.equal(edited, null, "the word under the caret is now 'x'");
});

// ---- formatting codes ------------------------------------------------------------

import { stripFormatting } from "../lib.js";

test("stripFormatting removes IRC control codes and keeps the words", () => {
  assert.equal(stripFormatting("\x02bold\x02 and \x0304,08red on yellow\x03 and \x1ditalic\x0f"), "bold and red on yellow and italic");
  assert.equal(stripFormatting("\x04ff8800,000000hex\x04 colour"), "hex colour");
  assert.equal(stripFormatting("no codes here"), "no codes here");
  assert.equal(stripFormatting("Türkçe \x02şğü\x02"), "Türkçe şğü");
  // A comma that follows a bare colour code is text, not a background colour.
  assert.equal(stripFormatting("\x03,x text"), ",x text");
});

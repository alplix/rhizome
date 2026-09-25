import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import { DICTIONARIES, LANGUAGES, detectLanguage, language, locale, setLanguage, t, tn } from "../i18n.js";
import { ACCENTS, DENSITIES, FONT_SIZES, THEMES } from "../appearance.js";
import { HELP } from "../lib.js";
import { describeEvent } from "../events.js";

const UI = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (name) => readFileSync(join(UI, name), "utf8");

const en = DICTIONARIES.en;
const placeholders = (text) => [...text.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();

test.afterEach(() => setLanguage("en"));

// ---- the dictionaries -------------------------------------------------------------

test("every language has exactly the keys English has", () => {
  for (const [code, dictionary] of Object.entries(DICTIONARIES)) {
    if (code === "en") continue;
    const missing = Object.keys(en).filter((k) => !(k in dictionary));
    const extra = Object.keys(dictionary).filter((k) => !(k in en));
    assert.deepEqual(missing, [], `${code} is missing keys`);
    assert.deepEqual(extra, [], `${code} has keys English does not`);
  }
});

test("a translation uses the same placeholders as the English it translates", () => {
  for (const [code, dictionary] of Object.entries(DICTIONARIES)) {
    for (const [key, text] of Object.entries(dictionary)) {
      assert.deepEqual(placeholders(text), placeholders(en[key] ?? ""), `${code}: ${key}`);
    }
  }
});

test("no translation is empty or padded with whitespace", () => {
  for (const [code, dictionary] of Object.entries(DICTIONARIES)) {
    for (const [key, text] of Object.entries(dictionary)) {
      assert.equal(typeof text, "string", `${code}: ${key}`);
      assert.ok(text.length > 0, `${code}: ${key} is empty`);
      assert.equal(text, text.trim(), `${code}: ${key} has stray whitespace`);
    }
  }
});

test("counted text has both forms", () => {
  for (const dictionary of Object.values(DICTIONARIES)) {
    const bases = new Set(Object.keys(dictionary).filter((k) => /\.(one|other)$/.test(k)).map((k) => k.replace(/\.(one|other)$/, "")));
    for (const base of bases) {
      assert.ok(`${base}.one` in dictionary && `${base}.other` in dictionary, `${base} needs .one and .other`);
    }
  }
});

test("the language list names each language in itself", () => {
  assert.deepEqual(LANGUAGES.map(([code]) => code).sort(), Object.keys(DICTIONARIES).sort());
  assert.equal(LANGUAGES.find(([c]) => c === "tr")[1], "Türkçe");
});

// ---- every key the code uses exists ---------------------------------------------------

function literalKeys() {
  const keys = new Set();
  const counted = new Set();
  const sources = readdirSync(UI).filter((f) => f.endsWith(".js") && !["i18n.js", "mock.js"].includes(f));
  for (const file of sources) {
    const text = read(file);
    for (const m of text.matchAll(/\bt\(\s*"([a-z_]+(?:\.[a-z_0-9]+)+)"/g)) keys.add(m[1]);
    for (const m of text.matchAll(/\btn\(\s*"([a-z_]+(?:\.[a-z_0-9]+)+)"/g)) counted.add(m[1]);
    for (const m of text.matchAll(/"((?:help|err|shortcut)\.[a-z_]+)"/g)) keys.add(m[1]);
    // `t(event.reply ? "sys.ctcp_reply" : "sys.ctcp_request", ...)`
    for (const m of text.matchAll(/\?\s*"(sys\.[a-z_]+)"\s*:\s*"(sys\.[a-z_]+)"/g)) {
      keys.add(m[1]);
      keys.add(m[2]);
    }
  }
  const html = read("index.html");
  for (const m of html.matchAll(/data-i18n(?:-placeholder|-title|-aria)?="([^"]+)"/g)) keys.add(m[1]);
  return { keys, counted };
}

test("every translation key used in the code and the page is defined", () => {
  const { keys, counted } = literalKeys();
  assert.ok(keys.size > 100, "the scan found the keys");
  const undefinedKeys = [...keys].filter((k) => !(k in en));
  assert.deepEqual(undefinedKeys, [], "used but not defined");
  for (const base of counted) assert.ok(`${base}.one` in en && `${base}.other` in en, `${base} is counted but has no forms`);
});

test("every key that is defined is used somewhere", () => {
  const { keys, counted } = literalKeys();
  // Families built from lists rather than written out.
  for (const theme of THEMES) keys.add(`theme.${theme}`);
  for (const accent of ACCENTS) keys.add(`accent.${accent}`);
  for (const d of DENSITIES) keys.add(`density.${d}`);
  for (const f of FONT_SIZES) keys.add(`font.${f}`);
  for (const [, key] of HELP) keys.add(key);
  for (const verbKey of Object.keys(en).filter((k) => k.startsWith("event."))) keys.add(verbKey);
  const unused = Object.keys(en).filter((k) => {
    const base = k.replace(/\.(one|other)$/, "");
    return !keys.has(k) && !counted.has(base);
  });
  assert.deepEqual(unused, [], "defined but never used");
});

test("the dynamic families are all defined", () => {
  for (const theme of THEMES) assert.ok(`theme.${theme}` in en, theme);
  for (const accent of ACCENTS) assert.ok(`accent.${accent}` in en, accent);
  for (const d of DENSITIES) assert.ok(`density.${d}` in en, d);
  for (const f of FONT_SIZES) assert.ok(`font.${f}` in en, f);
  for (const [usage, key] of HELP) {
    assert.ok(key in en, key);
    assert.ok(usage.startsWith("/"), usage);
  }
});

// ---- the translator -------------------------------------------------------------------

test("placeholders are filled and unknown ones are left visible", () => {
  assert.equal(t("status.connected_as", { nick: "alp" }), "Connected as alp");
  assert.equal(t("status.connected_as", {}), "Connected as {nick}");
  assert.equal(t("status.connected_as"), "Connected as {nick}");
  assert.equal(t("status.connected_as", { nick: "alp", unused: 1 }), "Connected as alp");
});

test("a value that looks like a placeholder is inserted as text, not expanded again", () => {
  assert.equal(t("status.connected_as", { nick: "{nick}" }), "Connected as {nick}");
  assert.equal(t("members.message", { nick: "$&" }), "Message $&");
});

test("a key that exists nowhere shows itself", () => {
  assert.equal(t("no.such.key"), "no.such.key");
});

test("switching language changes the text and falls back to English for a missing key", () => {
  setLanguage("tr");
  assert.equal(language(), "tr");
  assert.equal(t("status.idle"), "Bağlı değil");
  assert.equal(locale(), "tr-TR");

  // A key present only in English still reads, in English.
  DICTIONARIES.en["test.only_english"] = "English only";
  assert.equal(t("test.only_english"), "English only");
  delete DICTIONARIES.en["test.only_english"];

  setLanguage("en");
  assert.equal(t("status.idle"), "Not connected");
  assert.equal(locale(), "en-US");
});

test("an unknown language falls back to English", () => {
  assert.equal(setLanguage("xx"), "en");
  assert.equal(setLanguage(undefined), detectLanguage());
});

test("the operating system's language is matched, with or without a region", () => {
  assert.equal(detectLanguage({ language: "tr" }), "tr");
  assert.equal(detectLanguage({ language: "tr-TR" }), "tr");
  assert.equal(detectLanguage({ language: "TR-tr" }), "tr");
  assert.equal(detectLanguage({ language: "en-GB" }), "en");
  assert.equal(detectLanguage({ language: "de-DE" }), "en", "unsupported: English");
  assert.equal(detectLanguage({}), "en");
  assert.equal(detectLanguage(undefined) === "en" || typeof detectLanguage(undefined) === "string", true);
});

test("counted text picks the right form", () => {
  assert.equal(tn("messages.pill", 1), "↓ 1 new message");
  assert.equal(tn("messages.pill", 2), "↓ 2 new messages");
  assert.equal(tn("messages.pill", 0), "↓ 0 new messages");
  assert.equal(tn("search.count", 1, { plus: "" }), "1 result");
  assert.equal(tn("search.count", 50, { plus: "+" }), "50+ results");

  setLanguage("tr");
  // Turkish does not pluralise after a number.
  assert.equal(tn("messages.pill", 1), "↓ 1 yeni mesaj");
  assert.equal(tn("messages.pill", 7), "↓ 7 yeni mesaj");
});

// ---- events read correctly in every language -------------------------------------------

const EVENTS = [
  ["join", [], false],
  ["join", [], true],
  ["part", [""], false],
  ["part", ["going home"], false],
  ["part", [""], true],
  ["part", ["bye"], true],
  ["quit", [""], false],
  ["quit", ["Ping timeout"], false],
  ["kick", ["eve", ""], false],
  ["kick", ["eve", "spam"], false],
  ["kick", ["alp", ""], true],
  ["kick", ["alp", "off-topic"], true],
  ["nick", ["robert"], false],
  ["nick", ["alp2"], true],
  ["topic", ["a new topic"], false],
  ["topic", [""], false],
  ["mode", ["+o bob"], false],
];

test("every kind of event is put into words, in every language, with nothing left unfilled", () => {
  for (const code of Object.keys(DICTIONARIES)) {
    setLanguage(code);
    for (const [verb, args, own] of EVENTS) {
      const sentence = describeEvent({ sender: "bob", buffer: "#rhizome", own, event: { verb, args } });
      assert.ok(sentence.length > 0);
      assert.doesNotMatch(sentence, /\{\w+\}/, `${code} ${verb}: ${sentence}`);
      assert.doesNotMatch(sentence, /^event\./, `${code} ${verb} shows its key`);
    }
  }
});

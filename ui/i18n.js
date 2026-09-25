// Translating the interface.
//
// Every string the person reads goes through `t(key, params)`. The dictionaries
// live in locales/, one file per language, and English is the reference: a key
// missing from another language falls back to English rather than showing a raw
// key, and a test (ui/tests/i18n.test.mjs) checks that the languages stay in
// step, that placeholders match, and that every key used in the code exists.
//
// Plural forms use `key.one` and `key.other`, chosen with Intl.PluralRules, so
// a language with more forms can add them without touching the code.

import en from "./locales/en.js";
import tr from "./locales/tr.js";

export const DICTIONARIES = { en, tr };

// The languages the person can choose, with each written in its own name so it
// can be found whatever the current language is.
export const LANGUAGES = [
  ["en", "English"],
  ["tr", "Türkçe"],
];

let current = "en";

// The best supported language for a browser's `navigator.language`.
export function detectLanguage(navigatorLike = globalThis.navigator) {
  const wanted = String(navigatorLike?.language ?? "en").toLowerCase();
  return Object.keys(DICTIONARIES).find((code) => wanted === code || wanted.startsWith(`${code}-`)) ?? "en";
}

// Applies the `language` setting: "auto" or a language code.
export function setLanguage(setting) {
  current = setting === "auto" || setting == null ? detectLanguage() : setting in DICTIONARIES ? setting : "en";
  if (typeof document !== "undefined") document.documentElement.lang = current;
  return current;
}

export function language() {
  return current;
}

// A locale for dates and numbers.
export function locale() {
  return current === "tr" ? "tr-TR" : "en-US";
}

function fill(template, params) {
  if (!params) return template;
  return template.replace(/\{(\w+)\}/g, (whole, name) => (name in params ? String(params[name]) : whole));
}

function lookup(key) {
  return DICTIONARIES[current][key] ?? DICTIONARIES.en[key];
}

// The text for a key in the current language, with `{name}` placeholders filled.
// A key that exists nowhere is returned as it is, so a mistake is visible
// instead of blank.
export function t(key, params) {
  const template = lookup(key);
  return template === undefined ? key : fill(template, params);
}

// The text for a count: `key.one` or `key.other`, with `{count}` filled in.
export function tn(key, count, params) {
  const form = new Intl.PluralRules(locale()).select(count);
  const chosen = lookup(`${key}.${form}`) === undefined ? "other" : form;
  return t(`${key}.${chosen}`, { count, ...params });
}

// Translates the static text in the page: elements carrying `data-i18n`
// (text), `data-i18n-placeholder`, `data-i18n-title` (title and accessible
// name) or `data-i18n-aria` (accessible name only).
export function translateDocument(root = document) {
  for (const node of root.querySelectorAll("[data-i18n]")) node.textContent = t(node.dataset.i18n);
  for (const node of root.querySelectorAll("[data-i18n-placeholder]")) node.placeholder = t(node.dataset.i18nPlaceholder);
  for (const node of root.querySelectorAll("[data-i18n-title]")) {
    const text = t(node.dataset.i18nTitle);
    node.title = text;
    node.setAttribute("aria-label", text);
  }
  for (const node of root.querySelectorAll("[data-i18n-aria]")) node.setAttribute("aria-label", t(node.dataset.i18nAria));
}

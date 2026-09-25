// The themes are only values in a stylesheet, which is easy to get wrong in a
// way nobody notices until a tired person cannot read a message. So the
// stylesheet is read and every combination the interface actually draws is
// measured against the WCAG contrast thresholds.

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

import { ACCENTS, CACHE_KEY, DENSITIES, FONT_SIZES, LIGHT_THEMES, THEMES, applyAppearance, cacheAppearance, modeOf, resolveTheme } from "../appearance.js";
import { contrastRatio } from "../lib.js";

const UI = join(dirname(fileURLToPath(import.meta.url)), "..");
const css = readFileSync(join(UI, "style.css"), "utf8");
const bootScript = readFileSync(join(UI, "theme-boot.js"), "utf8");

// ---- reading the stylesheet -------------------------------------------------------

function variables(block) {
  const out = {};
  for (const m of block.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) out[m[1]] = m[2].trim();
  return out;
}

// Every `[data-KEY="VALUE"] { ... }` block, merged by value.
function blocksFor(key) {
  const out = {};
  for (const m of css.matchAll(new RegExp(`\\[data-${key}="(\\w+)"\\]\\s*\\{([^}]*)\\}`, "g"))) {
    out[m[1]] = { ...(out[m[1]] ?? {}), ...variables(m[2]) };
  }
  return out;
}

const themeVars = blocksFor("theme");
const modeVars = blocksFor("mode");
const CONCRETE = THEMES.filter((t) => t !== "system");

function hslToHex(h, s, l) {
  s /= 100;
  l /= 100;
  const k = (n) => (n + h / 30) % 12;
  const a = s * Math.min(l, 1 - l);
  const f = (n) => l - a * Math.max(-1, Math.min(k(n) - 3, Math.min(9 - k(n), 1)));
  const hex = (x) => Math.round(x * 255).toString(16).padStart(2, "0");
  return `#${hex(f(0))}${hex(f(8))}${hex(f(4))}`;
}

const pct = (value) => Number.parseFloat(value);

// ---- the stylesheet defines what the interface needs -------------------------------

test("every concrete theme is defined, with every colour the interface uses", () => {
  const needed = ["--bg", "--bg-elev", "--bg-side", "--bg-hover", "--fg", "--fg-dim", "--border", "--border-strong", "--accent", "--accent-fg", "--error", "--warn", "--link", "--focus"];
  for (const theme of CONCRETE) {
    assert.ok(themeVars[theme], `no [data-theme="${theme}"] block`);
    for (const name of needed) assert.match(themeVars[theme][name] ?? "", /^#[0-9a-f]{6}$/i, `${theme}: ${name} must be a six-digit hex colour`);
  }
});

test("there is a block for no theme the code does not know about", () => {
  assert.deepEqual(Object.keys(themeVars).sort(), [...CONCRETE].sort());
});

test("both light and dark modes define every accent", () => {
  for (const mode of ["light", "dark"]) {
    for (const accent of ACCENTS.filter((a) => a !== "theme")) {
      assert.match(modeVars[mode][`--accent-${accent}`] ?? "", /^#[0-9a-f]{6}$/i, `${mode} ${accent}`);
      assert.match(modeVars[mode][`--accent-${accent}-fg`] ?? "", /^#[0-9a-f]{6}$/i, `${mode} ${accent} text`);
    }
  }
});

test("each accent is wired to the html element", () => {
  for (const accent of ACCENTS.filter((a) => a !== "theme")) {
    assert.match(css, new RegExp(`html\\[data-accent="${accent}"\\]\\s*\\{[^}]*--accent:\\s*var\\(--accent-${accent}\\)`), accent);
  }
});

// ---- contrast ---------------------------------------------------------------------

test("body text is comfortably readable on every theme", () => {
  for (const theme of CONCRETE) {
    const v = themeVars[theme];
    for (const surface of ["--bg", "--bg-elev", "--bg-side"]) {
      const ratio = contrastRatio(v["--fg"], v[surface]);
      assert.ok(ratio >= 7, `${theme}: text on ${surface} is ${ratio.toFixed(2)}:1, want 7`);
    }
  }
});

test("secondary text, links and errors meet the AA threshold on every surface they appear on", () => {
  for (const theme of CONCRETE) {
    const v = themeVars[theme];
    for (const surface of ["--bg", "--bg-elev", "--bg-side"]) {
      for (const [name, colour] of [["dim text", "--fg-dim"], ["links", "--link"], ["errors", "--error"]]) {
        const ratio = contrastRatio(v[colour], v[surface]);
        assert.ok(ratio >= 4.5, `${theme}: ${name} on ${surface} is ${ratio.toFixed(2)}:1, want 4.5`);
      }
    }
  }
});

test("the theme's own accent carries its label text and stands out from the page", () => {
  for (const theme of CONCRETE) {
    const v = themeVars[theme];
    assert.ok(contrastRatio(v["--accent-fg"], v["--accent"]) >= 4.5, `${theme}: label on accent`);
    for (const surface of ["--bg", "--bg-side", "--bg-elev"]) {
      assert.ok(contrastRatio(v["--accent"], v[surface]) >= 3, `${theme}: accent on ${surface}`);
    }
    assert.ok(contrastRatio(v["--focus"], v["--bg"]) >= 3, `${theme}: focus ring`);
    assert.ok(contrastRatio(v["--warn"], v["--bg"]) >= 3, `${theme}: warning colour`);
  }
});

test("every accent the person can choose works in every theme of its mode", () => {
  for (const theme of CONCRETE) {
    const mode = modeOf(theme);
    const v = themeVars[theme];
    for (const accent of ACCENTS.filter((a) => a !== "theme")) {
      const colour = modeVars[mode][`--accent-${accent}`];
      const label = modeVars[mode][`--accent-${accent}-fg`];
      assert.ok(contrastRatio(label, colour) >= 4.5, `${mode} ${accent}: label on accent`);
      for (const surface of ["--bg", "--bg-side", "--bg-elev"]) {
        assert.ok(contrastRatio(colour, v[surface]) >= 3, `${theme} + ${accent}: accent on ${surface} is ${contrastRatio(colour, v[surface]).toFixed(2)}:1`);
      }
    }
  }
});

test("nick colours are readable at every hue, on the message list and the member list", () => {
  for (const theme of CONCRETE) {
    const v = { ...modeVars[modeOf(theme)], ...themeVars[theme] };
    const s = pct(v["--nick-s"]);
    const l = pct(v["--nick-l"]);
    for (let hue = 0; hue < 360; hue += 5) {
      const colour = hslToHex(hue, s, l);
      for (const surface of ["--bg", "--bg-side"]) {
        const ratio = contrastRatio(colour, v[surface]);
        assert.ok(ratio >= 4.5, `${theme}: hue ${hue} (${colour}) on ${surface} is ${ratio.toFixed(2)}:1`);
      }
    }
  }
});

test("the high contrast theme is genuinely higher than the others", () => {
  const hc = themeVars.contrast;
  assert.ok(contrastRatio(hc["--fg"], hc["--bg"]) >= 20);
  assert.ok(contrastRatio(hc["--fg-dim"], hc["--bg"]) >= 12);
  assert.ok(contrastRatio(hc["--border"], hc["--bg"]) >= 3, "borders themselves are visible");
});

// ---- choosing a theme --------------------------------------------------------------

test("'system' becomes a dark or light theme by the operating system's preference", () => {
  assert.equal(resolveTheme("system", true), "graphite");
  assert.equal(resolveTheme("system", false), "daylight");
  for (const theme of CONCRETE) assert.equal(resolveTheme(theme, true), theme);
  assert.equal(resolveTheme("a-theme-from-the-future", false), "daylight");
  assert.equal(resolveTheme(undefined, true), "graphite");
});

test("light and dark themes are told apart", () => {
  for (const theme of CONCRETE) assert.equal(modeOf(theme), LIGHT_THEMES.includes(theme) ? "light" : "dark");
  // And the list agrees with what the colours actually are.
  for (const theme of CONCRETE) {
    const bg = themeVars[theme]["--bg"];
    const light = contrastRatio(bg, "#ffffff") < contrastRatio(bg, "#000000");
    assert.equal(modeOf(theme) === "light", light, `${theme} is declared ${modeOf(theme)} but looks ${light ? "light" : "dark"}`);
  }
});

const fakeRoot = () => ({ dataset: {} });

test("applying the appearance sets the attributes the stylesheet keys on", () => {
  const root = fakeRoot();
  const theme = applyAppearance({ theme: "midnight", accent: "rose", density: "compact", font_size: "large" }, root, false);
  assert.equal(theme, "midnight");
  assert.deepEqual(root.dataset, { theme: "midnight", mode: "dark", accent: "rose", density: "compact", font: "large" });
});

test("the theme's own accent removes the accent attribute rather than setting it", () => {
  const root = fakeRoot();
  applyAppearance({ theme: "paper", accent: "blue" }, root, false);
  assert.equal(root.dataset.accent, "blue");
  applyAppearance({ theme: "paper", accent: "theme" }, root, false);
  assert.ok(!("accent" in root.dataset));
  assert.equal(root.dataset.mode, "light");
});

test("nonsense settings fall back to sensible ones instead of breaking the page", () => {
  const root = fakeRoot();
  applyAppearance({ theme: 5, accent: "chartreuse", density: "x", font_size: null }, root, true);
  assert.deepEqual(root.dataset, { theme: "graphite", mode: "dark", density: "comfortable", font: "medium" });
});

test("the choices are cached for the next start, and a broken storage does not matter", () => {
  const stored = {};
  cacheAppearance({ theme: "forest", accent: "teal", density: "compact", font_size: "small", show_events: false }, { setItem: (k, v) => (stored[k] = v) });
  assert.deepEqual(JSON.parse(stored[CACHE_KEY]), { theme: "forest", accent: "teal", density: "compact", font_size: "small" });

  assert.doesNotThrow(() =>
    cacheAppearance({ theme: "forest" }, { setItem: () => { throw new Error("quota"); } }),
  );
  assert.doesNotThrow(() => cacheAppearance({ theme: "forest" }, undefined));
});

// ---- the script that runs before the first paint -------------------------------------

test("the early script knows the same themes and options as the application", () => {
  const list = (name) => JSON.parse(bootScript.match(new RegExp(`var ${name} = (\\[[^\\]]*\\])`))[1]);
  assert.deepEqual(list("themes"), CONCRETE);
  assert.deepEqual(list("light"), LIGHT_THEMES);

  for (const accent of ACCENTS.filter((a) => a !== "theme")) assert.ok(bootScript.includes(`"${accent}"`), accent);
  for (const d of DENSITIES) assert.ok(bootScript.includes(`"${d}"`), d);
  for (const f of FONT_SIZES) assert.ok(bootScript.includes(`"${f}"`), f);
  assert.ok(bootScript.includes(CACHE_KEY), "reads the same storage key it is written under");
});

test("the early script falls back safely when storage is empty or broken", () => {
  const run = (getItem, prefersDark) => {
    const root = { dataset: {} };
    const sandbox = {
      document: { documentElement: root },
      localStorage: { getItem },
      window: { matchMedia: () => ({ matches: prefersDark }) },
    };
    new Function("document", "localStorage", "window", bootScript)(sandbox.document, sandbox.localStorage, sandbox.window);
    return root.dataset;
  };
  assert.deepEqual(run(() => null, true), { theme: "graphite", mode: "dark" });
  assert.deepEqual(run(() => null, false), { theme: "daylight", mode: "light" });
  assert.deepEqual(
    run(() => JSON.stringify({ theme: "paper", accent: "violet", density: "compact", font_size: "large" }), true),
    { theme: "paper", mode: "light", accent: "violet", density: "compact", font: "large" },
  );
  assert.deepEqual(run(() => { throw new Error("blocked"); }, false), { theme: "graphite", mode: "dark" });
  assert.equal(run(() => "not json", true).theme, "graphite");
  // "system" and unknown names resolve by the operating system, as in the application.
  assert.equal(run(() => JSON.stringify({ theme: "system" }), false).theme, "daylight");
  assert.equal(run(() => JSON.stringify({ theme: "nope" }), true).theme, "graphite");
});

// How the window looks: theme, accent, density and text size.
//
// The choices are applied as `data-*` attributes on the <html> element, and the
// stylesheet does the rest. Nothing here draws anything, so the resolving is
// tested in Node; theme-boot.js applies the same attributes from a cached copy
// before the first paint, so the window does not flash the wrong theme.

// Every theme the person can choose. "system" follows the operating system.
export const THEMES = ["system", "graphite", "midnight", "forest", "paper", "daylight", "contrast"];

// The themes drawn on a light background; the rest are dark. (theme-boot.js
// repeats this list; a test checks the two agree.)
export const LIGHT_THEMES = ["paper", "daylight"];

// "theme" keeps the theme's own accent colour.
export const ACCENTS = ["theme", "blue", "violet", "orange", "rose", "teal"];

export const DENSITIES = ["comfortable", "compact"];
export const FONT_SIZES = ["small", "medium", "large"];

// The key under which the choices are cached for the next start.
export const CACHE_KEY = "rhizome-appearance";

// The concrete theme for a setting: "system" becomes graphite or daylight
// according to the operating system's preference.
export function resolveTheme(setting, prefersDark) {
  if (setting === "system" || !THEMES.includes(setting)) return prefersDark ? "graphite" : "daylight";
  return setting;
}

export function modeOf(theme) {
  return LIGHT_THEMES.includes(theme) ? "light" : "dark";
}

// Sets the attributes the stylesheet keys on. `settings` is the person's
// settings object; anything unrecognised falls back to the default.
export function applyAppearance(settings, root = document.documentElement, prefersDark = false) {
  const theme = resolveTheme(settings.theme, prefersDark);
  root.dataset.theme = theme;
  root.dataset.mode = modeOf(theme);

  if (ACCENTS.includes(settings.accent) && settings.accent !== "theme") root.dataset.accent = settings.accent;
  else delete root.dataset.accent;

  root.dataset.density = DENSITIES.includes(settings.density) ? settings.density : "comfortable";
  root.dataset.font = FONT_SIZES.includes(settings.font_size) ? settings.font_size : "medium";
  return theme;
}

// Remembers the choices so the next start can apply them before the first
// paint. The backend's settings file remains the source of truth.
export function cacheAppearance(settings, storage = globalThis.localStorage) {
  try {
    storage.setItem(
      CACHE_KEY,
      JSON.stringify({ theme: settings.theme, accent: settings.accent, density: settings.density, font_size: settings.font_size }),
    );
  } catch {
    // Storage can be unavailable; the window then simply starts in the default
    // theme for a moment.
  }
}

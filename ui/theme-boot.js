// Applies the saved appearance before the first paint.
//
// This is a plain script, loaded from <head> before the stylesheet is used, so
// the window opens in the person's theme instead of flashing the default and
// then switching. It repeats a little of appearance.js on purpose: modules load
// later than this needs to run. A test keeps the two in step.
(function () {
  "use strict";
  var root = document.documentElement;
  try {
    var saved = JSON.parse(localStorage.getItem("rhizome-appearance") || "null") || {};
    var prefersDark = window.matchMedia("(prefers-color-scheme: dark)").matches;
    var themes = ["graphite", "midnight", "forest", "paper", "daylight", "contrast"];
    var light = ["paper", "daylight"];
    var theme = themes.indexOf(saved.theme) >= 0 ? saved.theme : prefersDark ? "graphite" : "daylight";
    root.dataset.theme = theme;
    root.dataset.mode = light.indexOf(theme) >= 0 ? "light" : "dark";
    if (["blue", "violet", "orange", "rose", "teal"].indexOf(saved.accent) >= 0) root.dataset.accent = saved.accent;
    if (["comfortable", "compact"].indexOf(saved.density) >= 0) root.dataset.density = saved.density;
    if (["small", "medium", "large"].indexOf(saved.font_size) >= 0) root.dataset.font = saved.font_size;
  } catch (error) {
    // No stored choice (or storage is unavailable): the stylesheet's defaults apply.
    root.dataset.theme = "graphite";
    root.dataset.mode = "dark";
  }
})();

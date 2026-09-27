// The interface's only way to reach the outside: the Tauri backend when the page
// runs inside the application, or a demo backend when it is opened in an
// ordinary browser (for development). Both provide exactly the same methods.

import { createMock } from "./mock.js";

function tauriApi() {
  const { invoke } = window.__TAURI__.core;
  const { listen } = window.__TAURI__.event;
  return {
    mode: "tauri",
    startupNotices: () => invoke("startup_notices"),
    appInfo: () => invoke("app_info"),
    getSettings: () => invoke("get_settings"),
    saveSettings: (settings) => invoke("save_settings", { settings }),
    listProfiles: () => invoke("list_profiles"),
    saveProfile: (profile) => invoke("save_profile", { profile }),
    deleteProfile: (id) => invoke("delete_profile", { id }),
    hasSavedPassword: (id) => invoke("has_saved_password", { id }),
    forgetPassword: (id) => invoke("forget_password", { id }),
    connect: (id, saslPassword, remember = false) =>
      invoke("connect", { id, saslPassword: saslPassword ?? null, remember }),
    disconnect: (id) => invoke("disconnect", { id }),
    sendMessage: (network, target, text, kind = "privmsg") => invoke("send_message", { network, target, text, kind }),
    join: (network, channels) => invoke("join", { network, channels }),
    part: (network, channel, reason = null) => invoke("part", { network, channel, reason }),
    setNick: (network, nick) => invoke("set_nick", { network, nick }),
    raw: (network, line) => invoke("raw", { network, line }),
    dccSend: (network, target, path) => invoke("dcc_send", { network, target, path }),
    dccAccept: (id) => invoke("dcc_accept", { id }),
    dccDecline: (id) => invoke("dcc_decline", { id }),
    scrollback: (network, buffer, before, limit) =>
      invoke("scrollback", { network, buffer, before: before ?? null, limit }),
    search: (query, network, newestFirst, limit) =>
      invoke("search", { query, network: network ?? null, newestFirst, limit }),
    around: (id, radius) => invoke("around", { id, radius }),
    buffers: (network) => invoke("buffers", { network }),
    markRead: (network, buffer, timeMs) => invoke("mark_read", { network, buffer, timeMs }),
    clearHistory: (network, buffer) => invoke("clear_history", { network, buffer }),
    openUrl: (url) => invoke("open_url", { url }),
    // Shows a desktop notification, asking for permission the first time.
    // Resolves to whether one was shown.
    notify: async (title, body) => {
      const notification = window.__TAURI__.notification;
      if (!notification) return false;
      let granted = await notification.isPermissionGranted();
      if (!granted) granted = (await notification.requestPermission()) === "granted";
      if (!granted) return false;
      notification.sendNotification({ title, body });
      return true;
    },
    // Resolves to a function that stops listening.
    onEvent: (callback) => listen("rhizome://event", (event) => callback(event.payload)),
  };
}

export const api = window.__TAURI__ ? tauriApi() : createMock();

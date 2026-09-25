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
    listProfiles: () => invoke("list_profiles"),
    saveProfile: (profile) => invoke("save_profile", { profile }),
    deleteProfile: (id) => invoke("delete_profile", { id }),
    connect: (id, saslPassword) => invoke("connect", { id, saslPassword: saslPassword ?? null }),
    disconnect: (id) => invoke("disconnect", { id }),
    sendMessage: (network, target, text, kind = "privmsg") => invoke("send_message", { network, target, text, kind }),
    join: (network, channels) => invoke("join", { network, channels }),
    part: (network, channel, reason = null) => invoke("part", { network, channel, reason }),
    setNick: (network, nick) => invoke("set_nick", { network, nick }),
    raw: (network, line) => invoke("raw", { network, line }),
    scrollback: (network, buffer, before, limit) =>
      invoke("scrollback", { network, buffer, before: before ?? null, limit }),
    search: (query, network, newestFirst, limit) =>
      invoke("search", { query, network: network ?? null, newestFirst, limit }),
    around: (id, radius) => invoke("around", { id, radius }),
    buffers: (network) => invoke("buffers", { network }),
    openUrl: (url) => invoke("open_url", { url }),
    // Resolves to a function that stops listening.
    onEvent: (callback) => listen("rhizome://event", (event) => callback(event.payload)),
  };
}

export const api = window.__TAURI__ ? tauriApi() : createMock();

// Networks worth offering on the first run, so a new person does not have to
// know a server address. Only the address is filled in: which channels to join,
// and under what nick, is theirs to choose.

export const PRESETS = [
  { id: "libera", name: "Libera.Chat", host: "irc.libera.chat", port: 6697, tls: true },
  { id: "oftc", name: "OFTC", host: "irc.oftc.net", port: 6697, tls: true },
  { id: "hackint", name: "hackint", host: "irc.hackint.org", port: 6697, tls: true },
];

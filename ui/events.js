// Putting an event line into words.
//
// The backend records what happened as a verb and its arguments and never as
// prose, so the same history reads correctly in whichever language the person
// chooses, including history written before they changed it. This turns one
// such line into a sentence.

import { stripFormatting } from "./lib.js";
import { t } from "./i18n.js";

// `message` is a message of kind "event": `sender` is who did it (or to whom it
// happened), `buffer` the conversation, `own` whether it concerns us, and
// `event` the verb with its arguments.
export function describeEvent(message) {
  const { event, sender, buffer: channel, own } = message;
  if (!event) return t("event.unknown");
  const args = event.args ?? [];
  const reason = stripFormatting(args[0] ?? "");

  switch (event.verb) {
    case "join":
      return own ? t("event.join_own", { channel }) : t("event.join", { nick: sender });
    case "part":
      if (own) return reason ? t("event.part_own_reason", { channel, reason }) : t("event.part_own", { channel });
      return reason ? t("event.part_reason", { nick: sender, reason }) : t("event.part", { nick: sender });
    case "quit":
      return reason ? t("event.quit_reason", { nick: sender, reason }) : t("event.quit", { nick: sender });
    case "kick": {
      const victim = args[0] ?? "";
      const why = stripFormatting(args[1] ?? "");
      if (own) return why ? t("event.kick_own_reason", { by: sender, channel, reason: why }) : t("event.kick_own", { by: sender, channel });
      return why
        ? t("event.kick_reason", { victim, by: sender, reason: why })
        : t("event.kick", { victim, by: sender });
    }
    case "nick":
      return own ? t("event.nick_own", { new: args[0] ?? "" }) : t("event.nick", { old: sender, new: args[0] ?? "" });
    case "topic": {
      const topic = stripFormatting(args[0] ?? "");
      return topic ? t("event.topic", { by: sender, topic }) : t("event.topic_cleared", { by: sender });
    }
    case "mode":
      return t("event.mode", { by: sender, modes: args[0] ?? "" });
    default:
      return t("event.unknown");
  }
}

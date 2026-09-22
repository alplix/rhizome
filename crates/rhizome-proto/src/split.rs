//! Splitting outgoing text to fit the 512-byte line limit.
//!
//! Two things make this subtle enough to deserve its own module.
//!
//! First, the limit is in *bytes*, not characters. A line of Turkish, Greek or
//! CJK text hits the limit at well under 512 characters, and cutting at a
//! character count will eventually slice a multi-byte sequence in half and
//! emit mojibake.
//!
//! Second, the limit applies to the line *the server relays*, not the line the
//! client sends. The server prepends `:nick!user@host ` before passing the
//! message on, so a message sized to 512 bytes locally arrives truncated. The
//! client has to subtract its own hostmask, which it learns from the server
//! after connecting.

use crate::message::MAX_MESSAGE_BYTES;

/// A conservative hostmask length to assume before the server has told us our
/// own. Real masks are shorter than this once a cloak or hostname is applied,
/// so messages sized against it are safe but slightly shorter than necessary.
const ASSUMED_MASK_BYTES: usize = 100;

/// The number of payload bytes available for a `PRIVMSG` or `NOTICE` body.
///
/// `mask` is the client's own `nick!user@host` as the server sees it; pass
/// `None` before it is known and a conservative estimate is used instead.
pub fn payload_budget(command: &str, target: &str, mask: Option<&str>) -> usize {
    // ":" + mask + " " + command + " " + target + " :" + CRLF
    let overhead = 1
        + mask.map_or(ASSUMED_MASK_BYTES, str::len)
        + 1
        + command.len()
        + 1
        + target.len()
        + 2
        + 2;
    MAX_MESSAGE_BYTES.saturating_sub(overhead)
}

/// Splits `text` into chunks of at most `max_bytes` bytes each.
///
/// Chunks never cut a character in half. Where possible the split falls on a
/// space and that space is consumed, so reassembling the chunks with a single
/// space reproduces the original. When a single word is longer than
/// `max_bytes` it is cut at a character boundary, since there is no
/// alternative that fits.
///
/// Returns an empty vector for empty input, and always makes progress: a
/// `max_bytes` too small to hold even one character still yields one character
/// per chunk rather than looping forever.
pub fn split_utf8(text: &str, max_bytes: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;

    while !rest.is_empty() {
        if rest.len() <= max_bytes {
            out.push(rest);
            break;
        }

        // Largest character boundary that still fits in the budget.
        let mut hard_end = max_bytes;
        while hard_end > 0 && !rest.is_char_boundary(hard_end) {
            hard_end -= 1;
        }
        if hard_end == 0 {
            // The budget cannot hold the first character. Emit it alone so the
            // loop terminates; the caller has given us an unusable limit.
            hard_end = rest
                .chars()
                .next()
                .map(char::len_utf8)
                .expect("rest is non-empty");
        }

        // If a space sits just past the window, the window already ends on a
        // word boundary and can be taken whole. Falling through to `rfind`
        // here would back up to the *previous* space and waste the last word
        // of every chunk.
        if rest.as_bytes().get(hard_end) == Some(&b' ') {
            out.push(&rest[..hard_end]);
            rest = &rest[hard_end + 1..];
            continue;
        }

        match rest[..hard_end].rfind(' ') {
            // Break on the space and swallow it.
            Some(i) if i > 0 => {
                out.push(&rest[..i]);
                rest = &rest[i + 1..];
            }
            // No usable space: cut mid-word at the character boundary.
            _ => {
                out.push(&rest[..hard_end]);
                rest = &rest[hard_end..];
            }
        }
    }

    out
}

/// Splits a message body into the chunks to send as consecutive `PRIVMSG` or
/// `NOTICE` commands, each sized so the server's relayed copy still fits.
pub fn split_for_send<'a>(
    command: &str,
    target: &str,
    mask: Option<&str>,
    text: &'a str,
) -> Vec<&'a str> {
    split_utf8(text, payload_budget(command, target, mask))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_one_chunk() {
        assert_eq!(split_utf8("hello", 100), vec!["hello"]);
        assert_eq!(split_utf8("", 100), Vec::<&str>::new());
    }

    #[test]
    fn splits_on_spaces_and_consumes_them() {
        let chunks = split_utf8("aaa bbb ccc ddd", 7);
        assert_eq!(chunks, vec!["aaa bbb", "ccc ddd"]);
        assert_eq!(chunks.join(" "), "aaa bbb ccc ddd");
    }

    #[test]
    fn never_cuts_a_multibyte_character_in_half() {
        // Each "ş" is two bytes, so a 5-byte budget holds two characters and
        // one byte of the third. Cutting at 5 bytes would produce invalid
        // UTF-8; the split must back off to 4.
        let text = "şşşşşş";
        for max in 1..=12 {
            let chunks = split_utf8(text, max);
            assert_eq!(chunks.concat(), text, "lost data at max={max}");
            for chunk in &chunks {
                assert!(std::str::from_utf8(chunk.as_bytes()).is_ok());
            }
        }
    }

    #[test]
    fn turkish_text_is_measured_in_bytes_not_characters() {
        // 20 characters but 40 bytes: a character-based splitter would emit
        // this as one chunk and the server would truncate it.
        let text = "şğüöçşğüöçşğüöçşğüöç";
        assert_eq!(text.chars().count(), 20);
        assert_eq!(text.len(), 40);
        let chunks = split_utf8(text, 30);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.len() <= 30));
    }

    #[test]
    fn a_word_longer_than_the_budget_is_cut_mid_word() {
        let chunks = split_utf8("supercalifragilistic", 6);
        assert_eq!(chunks, vec!["superc", "alifra", "gilist", "ic"]);
        assert_eq!(chunks.concat(), "supercalifragilistic");
    }

    #[test]
    fn makes_progress_even_with_an_unusable_budget() {
        // A budget smaller than one character must not loop forever.
        let chunks = split_utf8("şşş", 1);
        assert_eq!(chunks, vec!["ş", "ş", "ş"]);
    }

    #[test]
    fn budget_shrinks_as_the_target_and_mask_grow() {
        let short = payload_budget("PRIVMSG", "#a", Some("n!u@h"));
        let long = payload_budget(
            "PRIVMSG",
            "#a-very-long-channel-name-indeed",
            Some("longnick!longuser@some.very.long.cloak.example.org"),
        );
        assert!(long < short);
        assert!(short < MAX_MESSAGE_BYTES);
    }

    #[test]
    fn unknown_mask_yields_a_conservative_budget() {
        let known = payload_budget("PRIVMSG", "#chan", Some("n!u@h"));
        let unknown = payload_budget("PRIVMSG", "#chan", None);
        assert!(unknown < known);
    }

    #[test]
    fn relayed_line_fits_in_512_bytes() {
        let mask = "alp!~alp@user/alp";
        let target = "#rhizome";
        let text = "x".repeat(2000);
        for chunk in split_for_send("PRIVMSG", target, Some(mask), &text) {
            let relayed = format!(":{mask} PRIVMSG {target} :{chunk}\r\n");
            assert!(
                relayed.len() <= MAX_MESSAGE_BYTES,
                "relayed line was {} bytes",
                relayed.len()
            );
        }
    }
}

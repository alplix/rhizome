//! Turning a byte stream into lines.
//!
//! TCP delivers arbitrary chunks, so a read may end mid-line, hold several
//! lines, or split a multi-byte character across two reads. [`LineCodec`]
//! buffers until a full line is available. It has no I/O of its own, which
//! keeps it testable with nothing but byte slices.
//!
//! A server that never sends a newline would otherwise make the buffer grow
//! without bound, so lines over [`MAX_LINE_BYTES`] are discarded rather than
//! buffered.

use rhizome_proto::message::{MAX_MESSAGE_BYTES, MAX_TAGS_BYTES};

/// The longest line accepted: the tag section plus the message body.
///
/// Servers occasionally exceed the nominal limits slightly, so this is a
/// ceiling for protecting memory, not a strict conformance check.
pub const MAX_LINE_BYTES: usize = MAX_TAGS_BYTES + MAX_MESSAGE_BYTES;

/// Accumulates bytes and yields complete lines.
#[derive(Debug, Default)]
pub struct LineCodec {
    buf: Vec<u8>,
    /// Set while skipping the remainder of an oversized line.
    discarding: bool,
    oversized: u64,
}

impl LineCodec {
    pub fn new() -> LineCodec {
        LineCodec::default()
    }

    /// Appends freshly read bytes.
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// How many oversized lines have been discarded so far.
    pub fn oversized(&self) -> u64 {
        self.oversized
    }

    /// The next complete line with its terminator removed, or `None` when more
    /// bytes are needed. Blank lines are skipped.
    ///
    /// Input that is not valid UTF-8 is decoded lossily. Some older networks
    /// still carry legacy encodings, and one bad byte should cost one
    /// replacement character, not the whole line.
    pub fn next_line(&mut self) -> Option<String> {
        loop {
            let newline = self.buf.iter().position(|&b| b == b'\n');

            if self.discarding {
                match newline {
                    Some(i) => {
                        self.buf.drain(..=i);
                        self.discarding = false;
                        continue;
                    }
                    None => {
                        self.buf.clear();
                        return None;
                    }
                }
            }

            let Some(i) = newline else {
                if self.buf.len() > MAX_LINE_BYTES {
                    // No terminator and already too long: stop buffering and
                    // skip until the line ends.
                    self.buf.clear();
                    self.discarding = true;
                    self.oversized += 1;
                }
                return None;
            };

            let raw: Vec<u8> = self.buf.drain(..=i).collect();
            if raw.len() > MAX_LINE_BYTES + 2 {
                self.oversized += 1;
                continue;
            }
            let trimmed = raw
                .strip_suffix(b"\n")
                .map(|r| r.strip_suffix(b"\r").unwrap_or(r))
                .unwrap_or(&raw);
            if trimmed.is_empty() {
                continue;
            }
            return Some(decode(trimmed));
        }
    }
}

fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(codec: &mut LineCodec) -> Vec<String> {
        std::iter::from_fn(|| codec.next_line()).collect()
    }

    #[test]
    fn splits_multiple_lines_from_one_read() {
        let mut c = LineCodec::new();
        c.push(b"PING :a\r\nPING :b\r\n");
        assert_eq!(lines(&mut c), vec!["PING :a", "PING :b"]);
    }

    #[test]
    fn holds_a_partial_line_until_it_completes() {
        let mut c = LineCodec::new();
        c.push(b"PRIVMSG #c :hel");
        assert_eq!(c.next_line(), None);
        c.push(b"lo\r\n");
        assert_eq!(c.next_line().as_deref(), Some("PRIVMSG #c :hello"));
    }

    #[test]
    fn a_multibyte_character_split_across_reads_survives() {
        // "ş" is 0xC5 0x9F. The read boundary falls between the two bytes.
        let text = "PRIVMSG #c :merhaba şu\r\n".as_bytes();
        let cut = text.iter().position(|&b| b == 0xC5).unwrap() + 1;
        let mut c = LineCodec::new();
        c.push(&text[..cut]);
        assert_eq!(c.next_line(), None);
        c.push(&text[cut..]);
        assert_eq!(c.next_line().as_deref(), Some("PRIVMSG #c :merhaba şu"));
    }

    #[test]
    fn accepts_a_bare_lf_terminator() {
        let mut c = LineCodec::new();
        c.push(b"PING :x\n");
        assert_eq!(c.next_line().as_deref(), Some("PING :x"));
    }

    #[test]
    fn blank_lines_are_skipped() {
        let mut c = LineCodec::new();
        c.push(b"\r\n\r\nPING :x\r\n\r\n");
        assert_eq!(lines(&mut c), vec!["PING :x"]);
    }

    #[test]
    fn invalid_utf8_costs_one_replacement_character_not_the_line() {
        let mut c = LineCodec::new();
        c.push(b"PRIVMSG #c :caf\xE9 ok\r\n");
        let line = c.next_line().unwrap();
        assert!(line.starts_with("PRIVMSG #c :caf"));
        assert!(line.ends_with(" ok"));
        assert!(line.contains('\u{FFFD}'));
    }

    #[test]
    fn a_line_with_no_terminator_cannot_grow_the_buffer_forever() {
        let mut c = LineCodec::new();
        // Feed far more than the limit with no newline in sight.
        for _ in 0..100 {
            c.push(&[b'x'; 1000]);
            assert_eq!(c.next_line(), None);
        }
        assert!(c.buf.len() <= MAX_LINE_BYTES + 1000);
        assert_eq!(c.oversized(), 1);
    }

    #[test]
    fn recovers_on_the_line_after_an_oversized_one() {
        let mut c = LineCodec::new();
        c.push(&vec![b'x'; MAX_LINE_BYTES + 500]);
        assert_eq!(c.next_line(), None);
        // The tail of the oversized line and then a good one.
        c.push(b"tail of the long line\r\nPING :ok\r\n");
        assert_eq!(lines(&mut c), vec!["PING :ok"]);
    }

    #[test]
    fn an_oversized_line_that_does_arrive_whole_is_dropped() {
        let mut c = LineCodec::new();
        let mut big = vec![b'x'; MAX_LINE_BYTES + 100];
        big.extend_from_slice(b"\r\nPING :ok\r\n");
        c.push(&big);
        assert_eq!(lines(&mut c), vec!["PING :ok"]);
        assert_eq!(c.oversized(), 1);
    }
}

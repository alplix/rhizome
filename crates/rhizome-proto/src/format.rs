//! In-band text formatting: the control characters IRC uses for bold, colour
//! and the rest.
//!
//! IRC carries styling as control characters inside the message body rather
//! than as markup. A client that does not decode them shows raw control
//! characters; a client that merely deletes them loses the styling. This
//! module turns a body into styled [`Span`]s for rendering, and also offers
//! [`strip`] for the places that want plain text — notably the search index,
//! which should match on words rather than on colour codes.
//!
//! The codes are the de facto mIRC set; there is no RFC for any of this.

use std::fmt;

/// Toggles bold.
pub const BOLD: char = '\u{02}';
/// Introduces a colour, as one or two digits, optionally `,` and one or two
/// more. Bare, it resets colours.
pub const COLOR: char = '\u{03}';
/// Introduces a 6-digit hex colour, optionally `,` and six more. Bare, it
/// resets colours.
pub const HEX_COLOR: char = '\u{04}';
/// Resets every attribute.
pub const RESET: char = '\u{0F}';
/// Toggles monospace.
pub const MONOSPACE: char = '\u{11}';
/// Swaps foreground and background.
pub const REVERSE: char = '\u{16}';
/// Toggles italic.
pub const ITALIC: char = '\u{1D}';
/// Toggles strikethrough.
pub const STRIKETHROUGH: char = '\u{1E}';
/// Toggles underline.
pub const UNDERLINE: char = '\u{1F}';

/// A colour, either an index into the mIRC palette or a literal RGB value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    /// A palette index. 0–15 are the classic colours; 16–98 are the extended
    /// palette; 99 means "default".
    Index(u8),
    /// A literal colour from a `\x04` hex code.
    Rgb(u8, u8, u8),
}

/// The classic 16-colour mIRC palette as RGB.
const CLASSIC_PALETTE: [(u8, u8, u8); 16] = [
    (255, 255, 255), // 0  white
    (0, 0, 0),       // 1  black
    (0, 0, 127),     // 2  blue
    (0, 147, 0),     // 3  green
    (255, 0, 0),     // 4  red
    (127, 0, 0),     // 5  brown
    (156, 0, 156),   // 6  magenta
    (252, 127, 0),   // 7  orange
    (255, 255, 0),   // 8  yellow
    (0, 252, 0),     // 9  light green
    (0, 147, 147),   // 10 cyan
    (0, 255, 255),   // 11 light cyan
    (0, 0, 252),     // 12 light blue
    (255, 0, 255),   // 13 pink
    (127, 127, 127), // 14 grey
    (210, 210, 210), // 15 light grey
];

impl Color {
    /// Resolves to an RGB value.
    ///
    /// Returns `None` for index 99 ("default", meaning the theme decides) and
    /// for the extended palette 16–98, whose table the UI layer carries so
    /// that it can adjust it for light and dark themes.
    pub fn to_rgb(self) -> Option<(u8, u8, u8)> {
        match self {
            Color::Rgb(r, g, b) => Some((r, g, b)),
            Color::Index(i) if (i as usize) < CLASSIC_PALETTE.len() => {
                Some(CLASSIC_PALETTE[i as usize])
            }
            Color::Index(_) => None,
        }
    }
}

/// The attributes in effect for a run of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub monospace: bool,
    pub reverse: bool,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
}

impl Style {
    /// Whether this run needs any markup at all.
    pub fn is_plain(&self) -> bool {
        *self == Style::default()
    }
}

/// A run of text sharing one [`Style`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

/// Splits a message body into styled runs.
///
/// Adjacent runs always differ in style, and empty runs are dropped, so the
/// result is the shortest sequence that reproduces the input.
pub fn parse(text: &str) -> Vec<Span> {
    let bytes = text.as_bytes();
    let mut spans: Vec<Span> = Vec::new();
    let mut style = Style::default();
    let mut run_start = 0usize;
    let mut i = 0usize;

    // Closes the run ending at `end` under `style`, if it is non-empty.
    fn flush(spans: &mut Vec<Span>, text: &str, start: usize, end: usize, style: Style) {
        if end > start {
            spans.push(Span {
                text: text[start..end].to_owned(),
                style,
            });
        }
    }

    while i < bytes.len() {
        let c = bytes[i];
        // Every control character we care about is ASCII, so indexing by byte
        // is safe: multi-byte UTF-8 sequences never contain a byte < 0x80.
        let next = match c {
            0x02 | 0x1D | 0x1F | 0x1E | 0x11 | 0x16 | 0x0F | 0x03 | 0x04 => c,
            _ => {
                i += 1;
                continue;
            }
        };

        flush(&mut spans, text, run_start, i, style);
        i += 1;

        match next {
            0x02 => style.bold = !style.bold,
            0x1D => style.italic = !style.italic,
            0x1F => style.underline = !style.underline,
            0x1E => style.strikethrough = !style.strikethrough,
            0x11 => style.monospace = !style.monospace,
            0x16 => style.reverse = !style.reverse,
            0x0F => style = Style::default(),
            0x03 => {
                let (fg, after_fg) = take_digits(bytes, i, 2);
                match fg {
                    None => {
                        // A bare colour code clears colours but leaves the
                        // other attributes alone.
                        style.fg = None;
                        style.bg = None;
                        i = after_fg;
                    }
                    Some(fg) => {
                        style.fg = Some(Color::Index(fg));
                        i = after_fg;
                        // A comma only introduces a background if digits
                        // actually follow; otherwise it is literal text.
                        if bytes.get(i) == Some(&b',') {
                            let (bg, after_bg) = take_digits(bytes, i + 1, 2);
                            if let Some(bg) = bg {
                                style.bg = Some(Color::Index(bg));
                                i = after_bg;
                            }
                        }
                    }
                }
            }
            0x04 => {
                let (fg, after_fg) = take_hex(bytes, i);
                match fg {
                    None => {
                        style.fg = None;
                        style.bg = None;
                        i = after_fg;
                    }
                    Some((r, g, b)) => {
                        style.fg = Some(Color::Rgb(r, g, b));
                        i = after_fg;
                        if bytes.get(i) == Some(&b',') {
                            let (bg, after_bg) = take_hex(bytes, i + 1);
                            if let Some((r, g, b)) = bg {
                                style.bg = Some(Color::Rgb(r, g, b));
                                i = after_bg;
                            }
                        }
                    }
                }
            }
            _ => unreachable!("only formatting codes reach this match"),
        }

        run_start = i;
    }

    flush(&mut spans, text, run_start, bytes.len(), style);
    spans
}

/// Removes every formatting code, leaving the text a reader would see.
///
/// Use this for the search index, for notification previews, and anywhere else
/// that compares message text, so that a colour code in the middle of a word
/// does not stop it matching.
pub fn strip(text: &str) -> String {
    // Fast path: most messages carry no formatting at all.
    if !text.bytes().any(is_format_byte) {
        return text.to_owned();
    }
    parse(text).into_iter().map(|s| s.text).collect()
}

/// Whether a message carries any formatting.
pub fn has_formatting(text: &str) -> bool {
    text.bytes().any(is_format_byte)
}

fn is_format_byte(b: u8) -> bool {
    matches!(b, 0x02 | 0x03 | 0x04 | 0x0F | 0x11 | 0x16 | 0x1D | 0x1E | 0x1F)
}

/// Reads up to `max` ASCII digits starting at `i`, returning the value and the
/// index just past them.
fn take_digits(bytes: &[u8], mut i: usize, max: usize) -> (Option<u8>, usize) {
    let start = i;
    while i < bytes.len() && i - start < max && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == start {
        return (None, i);
    }
    // At most two ASCII digits, so the value is at most 99 and fits in a u8.
    let value = bytes[start..i]
        .iter()
        .fold(0u8, |acc, b| acc * 10 + (b - b'0'));
    (Some(value), i)
}

/// Reads exactly six hex digits starting at `i`.
fn take_hex(bytes: &[u8], i: usize) -> (Option<(u8, u8, u8)>, usize) {
    if i + 6 > bytes.len() || !bytes[i..i + 6].iter().all(u8::is_ascii_hexdigit) {
        return (None, i);
    }
    let nibble = |b: u8| -> u8 {
        match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            _ => b - b'A' + 10,
        }
    };
    let byte_at = |o: usize| nibble(bytes[i + o]) * 16 + nibble(bytes[i + o + 1]);
    (Some((byte_at(0), byte_at(2), byte_at(4))), i + 6)
}

/// Renders spans back into a formatted body.
///
/// This is not a byte-for-byte inverse of [`parse`] — it emits a reset before
/// each style change rather than minimal toggles — but the rendered result is
/// visually identical.
pub fn render(spans: &[Span]) -> String {
    let mut out = String::new();
    for span in spans {
        let s = span.style;
        if !out.is_empty() {
            out.push(RESET);
        }
        if s.bold {
            out.push(BOLD);
        }
        if s.italic {
            out.push(ITALIC);
        }
        if s.underline {
            out.push(UNDERLINE);
        }
        if s.strikethrough {
            out.push(STRIKETHROUGH);
        }
        if s.monospace {
            out.push(MONOSPACE);
        }
        if s.reverse {
            out.push(REVERSE);
        }
        match (s.fg, s.bg) {
            (Some(Color::Index(f)), Some(Color::Index(b))) => {
                out.push_str(&format!("{COLOR}{f:02},{b:02}"))
            }
            (Some(Color::Index(f)), _) => out.push_str(&format!("{COLOR}{f:02}")),
            (Some(Color::Rgb(r, g, b)), _) => {
                out.push_str(&format!("{HEX_COLOR}{r:02X}{g:02X}{b:02X}"))
            }
            _ => {}
        }
        out.push_str(&span.text);
    }
    out
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(text: &str) -> Vec<Span> {
        vec![Span {
            text: text.into(),
            style: Style::default(),
        }]
    }

    #[test]
    fn unformatted_text_is_one_plain_span() {
        assert_eq!(parse("hello world"), plain("hello world"));
        assert!(!has_formatting("hello"));
        assert_eq!(strip("hello"), "hello");
    }

    #[test]
    fn bold_toggles_on_and_off() {
        let spans = parse("a\u{02}b\u{02}c");
        assert_eq!(spans.len(), 3);
        assert!(!spans[0].style.bold);
        assert!(spans[1].style.bold);
        assert!(!spans[2].style.bold);
        assert_eq!(spans[1].text, "b");
    }

    #[test]
    fn reset_clears_everything() {
        let spans = parse("\u{02}\u{1F}bold\u{0F}plain");
        assert_eq!(spans.len(), 2);
        assert!(spans[0].style.bold && spans[0].style.underline);
        assert!(spans[1].style.is_plain());
    }

    #[test]
    fn colour_with_foreground_and_background() {
        let spans = parse("\u{03}04,08warning");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].style.fg, Some(Color::Index(4)));
        assert_eq!(spans[0].style.bg, Some(Color::Index(8)));
        assert_eq!(spans[0].text, "warning");
    }

    #[test]
    fn single_digit_colour_is_accepted() {
        let spans = parse("\u{03}4red");
        assert_eq!(spans[0].style.fg, Some(Color::Index(4)));
        assert_eq!(spans[0].text, "red");
    }

    #[test]
    fn colour_reads_at_most_two_digits() {
        // "\x0312345" is colour 12 followed by the literal text "345".
        let spans = parse("\u{03}12345");
        assert_eq!(spans[0].style.fg, Some(Color::Index(12)));
        assert_eq!(spans[0].text, "345");
    }

    #[test]
    fn comma_not_followed_by_digits_is_literal_text() {
        // The classic bug: eating the comma turns "4, then" into " then".
        let spans = parse("\u{03}4, then");
        assert_eq!(spans[0].style.fg, Some(Color::Index(4)));
        assert_eq!(spans[0].style.bg, None);
        assert_eq!(spans[0].text, ", then");
    }

    #[test]
    fn bare_colour_code_resets_colours_only() {
        let spans = parse("\u{02}\u{03}04red\u{03}still bold");
        let last = spans.last().unwrap();
        assert_eq!(last.text, "still bold");
        assert_eq!(last.style.fg, None);
        assert!(last.style.bold, "a bare colour code must not clear bold");
    }

    #[test]
    fn hex_colour_is_parsed() {
        let spans = parse("\u{04}FF8800,000000orange");
        assert_eq!(spans[0].style.fg, Some(Color::Rgb(0xFF, 0x88, 0x00)));
        assert_eq!(spans[0].style.bg, Some(Color::Rgb(0, 0, 0)));
        assert_eq!(spans[0].text, "orange");
    }

    #[test]
    fn truncated_hex_colour_is_treated_as_a_reset() {
        let spans = parse("\u{04}FF88hello");
        assert_eq!(spans[0].style.fg, None);
        assert_eq!(spans[0].text, "FF88hello");
    }

    #[test]
    fn strip_removes_codes_but_keeps_text() {
        let text = "\u{02}bold\u{0F} and \u{03}04,08red\u{03} and \u{1D}italic\u{1D}";
        assert_eq!(strip(text), "bold and red and italic");
        assert!(has_formatting(text));
    }

    #[test]
    fn strip_preserves_non_ascii_text() {
        let text = "\u{02}şğüöç\u{02} İĞÜ";
        assert_eq!(strip(text), "şğüöç İĞÜ");
    }

    #[test]
    fn formatting_around_multibyte_text_does_not_corrupt_it() {
        let spans = parse("\u{03}04Merhaba şğüöç\u{0F} dünya");
        assert_eq!(spans[0].text, "Merhaba şğüöç");
        assert_eq!(spans[1].text, " dünya");
        assert_eq!(strip("\u{03}04Merhaba şğüöç\u{0F} dünya"), "Merhaba şğüöç dünya");
    }

    #[test]
    fn empty_runs_are_dropped() {
        // Back-to-back codes produce no text between them.
        let spans = parse("\u{02}\u{1F}\u{03}04x");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "x");
    }

    #[test]
    fn trailing_code_with_no_text_produces_no_span() {
        assert_eq!(parse("text\u{02}"), plain("text"));
        assert_eq!(parse("\u{02}"), Vec::<Span>::new());
        assert_eq!(parse(""), Vec::<Span>::new());
    }

    #[test]
    fn classic_palette_resolves_but_extended_defers_to_the_ui() {
        assert_eq!(Color::Index(4).to_rgb(), Some((255, 0, 0)));
        assert_eq!(Color::Index(50).to_rgb(), None);
        assert_eq!(Color::Index(99).to_rgb(), None);
        assert_eq!(Color::Rgb(1, 2, 3).to_rgb(), Some((1, 2, 3)));
    }

    #[test]
    fn render_round_trips_through_parse() {
        for text in [
            "plain",
            "\u{02}bold\u{0F} normal",
            "\u{03}04,08red on yellow",
            "\u{04}FF8800hex",
            "\u{02}\u{1D}\u{1F}everything",
        ] {
            let spans = parse(text);
            let rendered = render(&spans);
            assert_eq!(
                parse(&rendered)
                    .iter()
                    .map(|s| (s.text.clone(), s.style))
                    .collect::<Vec<_>>(),
                spans
                    .iter()
                    .map(|s| (s.text.clone(), s.style))
                    .collect::<Vec<_>>(),
                "re-parsing the render of {text:?} changed it"
            );
        }
    }
}

//! Case-insensitive comparison of nicks and channel names.
//!
//! IRC predates Unicode and inherits a quirk from RFC 1459: because the
//! original servers were written for a character set where `{}|` were the
//! lowercase forms of `[]\`, those characters compare equal. A server
//! announces which rule it uses in the `CASEMAPPING` token of its `005`
//! reply.
//!
//! This means [`str::to_lowercase`] is never the right tool for comparing two
//! nicks. Use [`CaseMapping::fold`] or [`CaseMapping::eq`] everywhere a nick
//! or channel name is compared, looked up in a map, or used as a key.

use std::fmt;

/// The case-folding rule a server uses for nicks and channel names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CaseMapping {
    /// `A-Z` fold to `a-z` and nothing else.
    Ascii,
    /// ASCII, plus `[]\` folding to `{}|` and `~` folding to `^`.
    ///
    /// This is the default when a server does not say otherwise, because it
    /// is what RFC 1459 specifies and what most networks still use.
    #[default]
    Rfc1459,
    /// ASCII, plus `[]\` folding to `{}|`, but leaving `~` alone.
    Rfc1459Strict,
}

impl CaseMapping {
    /// Parses a `CASEMAPPING` token value. An unrecognized value falls back to
    /// [`CaseMapping::Rfc1459`], matching how servers behave when they omit
    /// the token entirely.
    pub fn parse(value: &str) -> CaseMapping {
        match value.to_ascii_lowercase().as_str() {
            "ascii" => CaseMapping::Ascii,
            "rfc1459-strict" => CaseMapping::Rfc1459Strict,
            _ => CaseMapping::Rfc1459,
        }
    }

    /// Folds a single byte.
    ///
    /// Operating on bytes is safe for UTF-8 input: every byte this function
    /// changes is below `0x80`, and no byte of a multi-byte UTF-8 sequence is
    /// below `0x80`, so non-ASCII text passes through untouched.
    fn fold_byte(self, b: u8) -> u8 {
        match b {
            b'A'..=b'Z' => b + 32,
            b'[' | b']' | b'\\' if self != CaseMapping::Ascii => b + 32,
            b'~' if self == CaseMapping::Rfc1459 => b'^',
            _ => b,
        }
    }

    /// Folds a nick or channel name into a canonical form suitable for use as
    /// a map key or for equality comparison.
    pub fn fold(self, s: &str) -> String {
        let bytes: Vec<u8> = s.bytes().map(|b| self.fold_byte(b)).collect();
        // Safe by construction: see the note on `fold_byte`. Only ASCII bytes
        // are rewritten, and only to other ASCII bytes, so UTF-8 structure is
        // preserved.
        String::from_utf8(bytes).expect("folding only rewrites ASCII bytes")
    }

    /// Compares two nicks or channel names under this mapping, without
    /// allocating.
    pub fn eq(self, a: &str, b: &str) -> bool {
        a.len() == b.len()
            && a.bytes()
                .zip(b.bytes())
                .all(|(x, y)| self.fold_byte(x) == self.fold_byte(y))
    }
}

impl fmt::Display for CaseMapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CaseMapping::Ascii => "ascii",
            CaseMapping::Rfc1459 => "rfc1459",
            CaseMapping::Rfc1459Strict => "rfc1459-strict",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_letters_fold_under_every_mapping() {
        for mapping in [
            CaseMapping::Ascii,
            CaseMapping::Rfc1459,
            CaseMapping::Rfc1459Strict,
        ] {
            assert!(mapping.eq("Alp", "alp"));
            assert!(mapping.eq("#Channel", "#channel"));
            assert!(!mapping.eq("alp", "alpx"));
        }
    }

    #[test]
    fn brackets_fold_only_under_rfc1459_mappings() {
        // The classic trap: these two are the same nick on most networks.
        assert!(CaseMapping::Rfc1459.eq("nick[]", "nick{}"));
        assert!(CaseMapping::Rfc1459Strict.eq("nick[]", "nick{}"));
        assert!(!CaseMapping::Ascii.eq("nick[]", "nick{}"));

        assert!(CaseMapping::Rfc1459.eq("a\\b", "a|b"));
        assert!(!CaseMapping::Ascii.eq("a\\b", "a|b"));
    }

    #[test]
    fn tilde_folds_under_rfc1459_but_not_strict() {
        assert!(CaseMapping::Rfc1459.eq("a~b", "a^b"));
        assert!(!CaseMapping::Rfc1459Strict.eq("a~b", "a^b"));
        assert!(!CaseMapping::Ascii.eq("a~b", "a^b"));
    }

    #[test]
    fn non_ascii_passes_through_unchanged() {
        // Turkish text must survive folding byte-for-byte; in particular the
        // dotted/dotless i pair must not be touched, since the server does not
        // fold it either.
        let mapping = CaseMapping::Rfc1459;
        assert_eq!(mapping.fold("ŞİĞÜÖÇ"), "ŞİĞÜÖÇ");
        assert_eq!(mapping.fold("Alp_Şık"), "alp_Şık");
        assert!(!mapping.eq("İ", "i"));
        assert!(mapping.eq("Çğü", "Çğü"));
    }

    #[test]
    fn fold_and_eq_agree() {
        let mapping = CaseMapping::Rfc1459;
        for (a, b) in [("Nick[]", "nick{}"), ("ALP", "alp"), ("a~", "a^")] {
            assert_eq!(mapping.fold(a), mapping.fold(b));
            assert!(mapping.eq(a, b));
        }
    }

    #[test]
    fn unknown_casemapping_value_falls_back_to_rfc1459() {
        assert_eq!(CaseMapping::parse("ascii"), CaseMapping::Ascii);
        assert_eq!(CaseMapping::parse("RFC1459-STRICT"), CaseMapping::Rfc1459Strict);
        assert_eq!(CaseMapping::parse("something-new"), CaseMapping::Rfc1459);
        assert_eq!(CaseMapping::default(), CaseMapping::Rfc1459);
    }
}

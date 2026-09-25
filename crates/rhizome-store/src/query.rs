//! Turning what a person types into a safe FTS5 query.
//!
//! FTS5 has its own query language, with operators (`AND`, `OR`, `NOT`,
//! `NEAR`), column filters (`col:`), grouping and quotes. Passing raw user
//! input to it means that searching for `it's "broken"` or `C++` or the word
//! `AND` is a syntax error, and searching for `sender : foo` reaches into the
//! index's internals. So nothing typed is ever passed through as syntax.
//! Instead each word is quoted as a literal string, and the small set of
//! features Rhizome does want are recognised here and rebuilt safely:
//!
//! | You type | It means |
//! |---|---|
//! | `null pointer` | both words, anywhere, in any order |
//! | `"null pointer"` | that exact phrase |
//! | `kmall*` | any word starting with `kmall` |
//! | `from:bob` | only messages from `bob` |
//! | `in:#kernel` | only messages in `#kernel` |

/// A search box's contents, split into an FTS5 expression and filters.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedQuery {
    /// The FTS5 expression, or `None` when the query has no searchable words
    /// (for example only `from:bob`).
    pub fts: Option<String>,
    /// The words and phrases searched for, as typed, for showing in a UI.
    pub terms: Vec<String>,
    /// A `from:` filter.
    pub from: Option<String>,
    /// An `in:` filter.
    pub buffer: Option<String>,
}

impl ParsedQuery {
    /// Whether the query asks for nothing at all.
    pub fn is_empty(&self) -> bool {
        self.fts.is_none() && self.from.is_none() && self.buffer.is_none()
    }
}

/// One piece of the input: a word, or the contents of a quoted phrase.
struct Token {
    text: String,
    quoted: bool,
}

/// Splits on whitespace, keeping `"quoted phrases"` together.
///
/// A quote in the middle of a word ends that word and starts a phrase, and an
/// unterminated quote runs to the end of the input: the person is typing, and
/// a half-finished phrase should still search rather than fail.
fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;

    let finish = |current: &mut String, quoted: bool, tokens: &mut Vec<Token>| {
        if !current.is_empty() {
            tokens.push(Token {
                text: std::mem::take(current),
                quoted,
            });
        }
    };

    for c in input.chars() {
        match (c, in_quote) {
            ('"', false) => {
                finish(&mut current, false, &mut tokens);
                in_quote = true;
            }
            ('"', true) => {
                finish(&mut current, true, &mut tokens);
                in_quote = false;
            }
            (c, false) if c.is_whitespace() => finish(&mut current, false, &mut tokens),
            (c, _) => current.push(c),
        }
    }
    finish(&mut current, in_quote, &mut tokens);
    tokens
}

/// Whether a piece of text can be matched by the index at all.
///
/// The tokenizer indexes letters, digits and underscores. A word made only of
/// punctuation, like `::` or `->`, produces no index terms, and handing FTS5 an
/// empty phrase is either an error or a query that silently matches nothing.
fn is_searchable(text: &str) -> bool {
    text.chars().any(|c| c.is_alphanumeric() || c == '_')
}

/// Quotes text as an FTS5 string literal.
fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

/// Parses the contents of a search box.
pub fn parse(input: &str) -> ParsedQuery {
    let mut parsed = ParsedQuery::default();
    let mut expressions = Vec::new();

    for token in tokenize(input) {
        if !token.quoted {
            let lower = token.text.to_lowercase();
            // A filter key consumes its token even with no value yet. Someone
            // partway through typing `from:` must not be searching for the
            // literal word "from:" while they finish.
            if let Some(value) = filter_value(&token.text, &lower, "from:") {
                if !value.is_empty() {
                    parsed.from = Some(value);
                }
                continue;
            }
            if let Some(value) = filter_value(&token.text, &lower, "in:") {
                if !value.is_empty() {
                    parsed.buffer = Some(value);
                }
                continue;
            }
        }

        let (text, prefix) = if token.quoted {
            (token.text.as_str(), false)
        } else {
            // A trailing `*` asks for a prefix match. Strip every trailing
            // star so `foo**` does not become a syntax error.
            let stripped = token.text.trim_end_matches('*');
            (stripped, stripped.len() != token.text.len())
        };
        if !is_searchable(text) {
            continue;
        }
        parsed.terms.push(text.to_owned());
        expressions.push(format!("{}{}", quote(text), if prefix { "*" } else { "" }));
    }

    if !expressions.is_empty() {
        parsed.fts = Some(expressions.join(" "));
    }
    parsed
}

/// The value of a `key:value` filter token (possibly empty), or `None` if this
/// token is not that filter. `lower` is the token lowercased, used to match the
/// key case-insensitively while keeping the value's own case.
fn filter_value(original: &str, lower: &str, key: &str) -> Option<String> {
    if !lower.starts_with(key) {
        return None;
    }
    // `key` is ASCII, so this byte offset is a character boundary in the
    // original whenever it matched in the lowercased copy.
    original.get(key.len()..).map(|v| v.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fts(input: &str) -> Option<String> {
        parse(input).fts
    }

    #[test]
    fn plain_words_are_quoted_and_anded() {
        assert_eq!(fts("null pointer"), Some("\"null\" \"pointer\"".into()));
        assert_eq!(parse("null pointer").terms, vec!["null", "pointer"]);
    }

    #[test]
    fn a_quoted_phrase_stays_one_unit() {
        assert_eq!(fts("\"null pointer\""), Some("\"null pointer\"".into()));
        assert_eq!(
            fts("bug \"null pointer\" kernel"),
            Some("\"bug\" \"null pointer\" \"kernel\"".into())
        );
    }

    #[test]
    fn a_trailing_star_is_a_prefix_match() {
        assert_eq!(fts("kmall*"), Some("\"kmall\"*".into()));
        assert_eq!(fts("foo**"), Some("\"foo\"*".into()));
        // Inside a phrase the star is just a character.
        assert_eq!(fts("\"foo*\""), Some("\"foo*\"".into()));
    }

    #[test]
    fn filters_are_extracted_and_are_not_searched_for() {
        let q = parse("backtrace from:bob in:#kernel");
        assert_eq!(q.from.as_deref(), Some("bob"));
        assert_eq!(q.buffer.as_deref(), Some("#kernel"));
        assert_eq!(q.fts, Some("\"backtrace\"".into()));
        assert_eq!(q.terms, vec!["backtrace"]);
    }

    #[test]
    fn filter_keys_are_case_insensitive_but_values_keep_their_case() {
        let q = parse("FROM:Bob IN:#Kernel");
        assert_eq!(q.from.as_deref(), Some("Bob"));
        assert_eq!(q.buffer.as_deref(), Some("#Kernel"));
    }

    #[test]
    fn a_filter_with_no_value_is_ignored_not_an_error() {
        let q = parse("from: in:");
        assert!(q.is_empty());
    }

    #[test]
    fn filters_alone_make_a_valid_query() {
        let q = parse("from:bob");
        assert!(!q.is_empty());
        assert_eq!(q.fts, None);
    }

    #[test]
    fn a_quoted_filter_is_searched_as_text_not_applied() {
        // Quoting is how you search for the literal text "from:bob".
        let q = parse("\"from:bob\"");
        assert_eq!(q.from, None);
        assert_eq!(q.fts, Some("\"from:bob\"".into()));
    }

    #[test]
    fn fts5_operators_are_neutralised() {
        // Unquoted, each of these would be an operator or a syntax error.
        for word in ["AND", "OR", "NOT", "NEAR", "NEAR/3"] {
            assert_eq!(fts(word), Some(format!("\"{word}\"")), "{word}");
        }
        assert_eq!(fts("a OR b"), Some("\"a\" \"OR\" \"b\"".into()));
    }

    #[test]
    fn embedded_quotes_are_doubled_not_left_to_break_out() {
        // A quote opens a phrase, so this is `it` then the phrase `s `.
        let q = parse("it\"s");
        for expr in q.fts.iter() {
            assert_eq!(expr.matches('"').count() % 2, 0, "unbalanced quotes: {expr}");
        }
        assert_eq!(quote("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn an_attempted_injection_stays_inside_one_string_literal() {
        let q = parse("\" OR 1=1 -- \" ) NOT (");
        let expr = q.fts.unwrap();
        // Every character of the expression is inside a quoted literal or a
        // separator between them.
        let mut depth = 0;
        let mut chars = expr.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '"' {
                if depth == 1 && chars.peek() == Some(&'"') {
                    chars.next(); // an escaped quote inside a literal
                } else {
                    depth = 1 - depth;
                }
            } else if depth == 0 {
                assert!(c == ' ' || c == '*', "syntax leaked outside a literal: {expr}");
            }
        }
        assert_eq!(depth, 0, "unterminated literal in {expr}");
    }

    #[test]
    fn punctuation_only_words_are_dropped() {
        assert_eq!(fts("::"), None);
        assert_eq!(fts("-> ==="), None);
        assert_eq!(fts("std::vector"), Some("\"std::vector\"".into()));
        assert_eq!(fts("C++"), Some("\"C++\"".into()));
    }

    #[test]
    fn an_unterminated_quote_still_searches() {
        assert_eq!(fts("\"null poin"), Some("\"null poin\"".into()));
    }

    #[test]
    fn empty_and_blank_input_is_an_empty_query() {
        assert!(parse("").is_empty());
        assert!(parse("   \t ").is_empty());
        assert!(parse("\"\"").is_empty());
    }

    #[test]
    fn non_ascii_words_survive() {
        assert_eq!(fts("şaşırtıcı"), Some("\"şaşırtıcı\"".into()));
        assert_eq!(fts("İSTANBUL"), Some("\"İSTANBUL\"".into()));
    }

    #[test]
    fn identifiers_with_underscores_are_searchable() {
        assert_eq!(fts("kmalloc_array"), Some("\"kmalloc_array\"".into()));
        assert_eq!(fts("__init"), Some("\"__init\"".into()));
    }
}

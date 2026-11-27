//! How each dialect reads the text between and around its tokens: which characters are
//! whitespace, where a comment starts and ends, whether block comments nest, which bytes make a
//! name, which quotes escape with a backslash, and what a `$` opens. Every scanner of a statement's
//! text reads these rules from here, so that a rule is written once: the placeholder lexer
//! ([`crate::lex`]), the leading comment ([`crate::comment`]), the result list's items
//! ([`crate::analysis::result_items`]), and the migrations' statement splitter and highlighter.
//! The readers that can't share this code, in TypeScript and in `engine/syntax`, are held to the
//! same readings by `tests/lexical.json`, which every reader's tests run ([`Corpus`]).
//!
//! Each rule is the database's own: Postgres's `scan.l`, SQLite's `tokenize.c`, MySQL's
//! `sql_lex.cc` and ClickHouse's `Lexer.cpp`.

use crate::dialect::Dialect;

/// What a `$` that stands outside a name opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dollar {
    /// Nothing: a `$` is a character of a name, or a parameter's prefix (MySQL, SQLite).
    Nothing,
    /// Postgres's dollar quote, `$$` or `$tag$`, whose tag is a name with no `$` in it (`$本文$`).
    /// One that no tag closes is an error.
    Quote,
    /// ClickHouse's heredoc, `$tag$`, whose tag is ASCII letters, digits and `_`, or none. It is one
    /// only where the same tag closes it later; otherwise the `$` opens nothing.
    Heredoc,
}

/// Where `#` starts a line comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hash {
    Never,
    /// Anywhere outside a string or a name (MySQL).
    Always,
    /// Only before a space or a `!` (ClickHouse, whose `#!` opens a script's first line).
    BeforeSpaceOrBang,
}

/// One dialect's rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    pub dialect: Dialect,
    /// Whether a vertical tab is whitespace wherever it stands: everywhere but SQLite, which reads
    /// one only inside a run of whitespace another space starts, and refuses one that starts a run.
    /// Postgres before 17 reads it as no token at all, so a statement with one fails there either
    /// way.
    pub vertical_tab: bool,
    /// Whether the characters past ASCII that Unicode calls spaces are whitespace between tokens,
    /// as ClickHouse reads them, for text pasted from a word processor. Everywhere else they are
    /// characters of a name.
    pub unicode_spaces: bool,
    /// Whether a carriage return ends a `--` comment as a line feed does (Postgres). Elsewhere the
    /// comment runs on to the line feed.
    pub carriage_return_ends_comment: bool,
    /// Whether `--` starts a comment only before whitespace, a control character or the text's
    /// end (MySQL, which reads `--x` as two minus signs).
    pub dashes_need_a_space: bool,
    pub hash: Hash,
    /// Whether `//` starts a line comment (ClickHouse).
    pub slash_comments: bool,
    /// Whether a `/*` inside a block comment opens one more, which its own `*/` closes first
    /// (Postgres, ClickHouse).
    pub nested_comments: bool,
    /// Whether a block comment that no `*/` closes runs to the text's end (SQLite), where
    /// elsewhere it is an error.
    pub open_comment_runs_on: bool,
    /// Whether `E'…'` is a string in which a backslash escapes (Postgres).
    pub escape_strings: bool,
    /// Whether a backslash escapes in a `'…'` string by default (MySQL, unless its `sql_mode` has
    /// NO_BACKSLASH_ESCAPES, and ClickHouse).
    pub backslash_in_strings: bool,
    /// Whether a backslash escapes in a name quoted with `"` or backquotes (ClickHouse).
    pub backslash_in_names: bool,
    /// Whether `"…"` is a string by default (MySQL, unless its `sql_mode` has ANSI_QUOTES).
    pub double_quoted_strings: bool,
    /// Whether `[…]` quotes a name (SQLite), where elsewhere it is a subscript or an array.
    pub bracketed_names: bool,
    /// Whether typographic quotes quote, as in text pasted from a word processor: `‘…’` a string
    /// and `“…”` a name, each to its first closing quote, with no escape (ClickHouse).
    pub typographic_quotes: bool,
    pub dollar: Dollar,
}

impl Rules {
    pub const fn of(dialect: Dialect) -> Rules {
        let base = Rules {
            dialect,
            vertical_tab: true,
            unicode_spaces: false,
            carriage_return_ends_comment: false,
            dashes_need_a_space: false,
            hash: Hash::Never,
            slash_comments: false,
            nested_comments: false,
            open_comment_runs_on: false,
            escape_strings: false,
            backslash_in_strings: false,
            backslash_in_names: false,
            double_quoted_strings: false,
            bracketed_names: false,
            typographic_quotes: false,
            dollar: Dollar::Nothing,
        };
        match dialect {
            Dialect::Postgres => Rules { carriage_return_ends_comment: true, nested_comments: true, escape_strings: true, dollar: Dollar::Quote, ..base },
            Dialect::Sqlite => Rules { vertical_tab: false, open_comment_runs_on: true, bracketed_names: true, ..base },
            Dialect::Mysql => Rules { dashes_need_a_space: true, hash: Hash::Always, backslash_in_strings: true, double_quoted_strings: true, ..base },
            Dialect::Clickhouse => Rules { unicode_spaces: true, hash: Hash::BeforeSpaceOrBang, slash_comments: true, nested_comments: true, backslash_in_strings: true, backslash_in_names: true, typographic_quotes: true, dollar: Dollar::Heredoc, ..base },
        }
    }

    /// The length in bytes of the whitespace character at `i`, or 0 where none stands there. ASCII's
    /// space, tab, line feed, carriage return and form feed are whitespace everywhere; then the
    /// vertical tab and Unicode's spaces where the dialect reads them so. A no-break space or an
    /// ideographic space is a character of a name on Postgres, MySQL and SQLite (`as x\u{a0}` names
    /// a column `x\u{a0}`), so no reader may take one for spacing there.
    pub fn space_at(&self, b: &[u8], i: usize) -> usize {
        match b.get(i) {
            Some(b' ' | b'\t' | b'\n' | b'\r' | 0x0c) => 1,
            Some(0x0b) if self.vertical_tab || (i > 0 && matches!(b[i - 1], b' ' | b'\t' | b'\n' | b'\r' | 0x0c | 0x0b)) => 1,
            Some(&c) if c >= 0x80 && self.unicode_spaces => unicode_space(&b[i..]),
            _ => 0,
        }
    }

    /// The first byte at or after `i` that starts no whitespace.
    pub fn skip_spaces(&self, b: &[u8], mut i: usize) -> usize {
        loop {
            match self.space_at(b, i) {
                0 => return i,
                n => i += n,
            }
        }
    }

    /// Whether a name (a keyword or an identifier) may start with the byte: an ASCII letter or `_`
    /// everywhere, a byte past ASCII on Postgres, SQLite and MySQL, and a `$` on MySQL and
    /// ClickHouse.
    pub fn name_start(&self, c: u8) -> bool {
        c.is_ascii_alphabetic()
            || c == b'_'
            || match self.dialect {
                Dialect::Postgres | Dialect::Sqlite => c >= 0x80,
                Dialect::Mysql => c >= 0x80 || c == b'$',
                Dialect::Clickhouse => c == b'$',
            }
    }

    /// Whether a name may go on with the byte past its first: a digit and a `$` everywhere, and a
    /// byte past ASCII but on ClickHouse, whose names are ASCII. So `a$$b` and `é$a` are each one
    /// name, in which nothing opens.
    pub fn name_part(&self, c: u8) -> bool {
        c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || (c >= 0x80 && self.dialect != Dialect::Clickhouse)
    }

    /// The end of the name that starts at `start`, whose first byte [`Rules::name_start`] takes.
    pub fn name_end(&self, b: &[u8], start: usize) -> usize {
        (start + 1..b.len()).find(|&j| !self.name_part(b[j])).unwrap_or(b.len())
    }

    /// Whether a line comment starts at `i`: `--`, and `#` and `//` where the dialect reads one.
    pub fn line_comment_at(&self, b: &[u8], i: usize) -> bool {
        match b.get(i) {
            Some(b'/') => self.slash_comments && b.get(i + 1) == Some(&b'/'),
            // MySQL's `--` is a comment before whitespace or a control character (DEL too), or at
            // the text's end: `:a --:b` is `:a - (-:b)` there.
            Some(b'-') => b.get(i + 1) == Some(&b'-') && !(self.dashes_need_a_space && b.get(i + 2).is_some_and(|&n| n > b' ' && n != 0x7f)),
            Some(b'#') => match self.hash {
                Hash::Never => false,
                Hash::Always => true,
                Hash::BeforeSpaceOrBang => matches!(b.get(i + 1), Some(b' ' | b'!')),
            },
            _ => false,
        }
    }

    /// Where the line comment that starts at `i` ends: at the line end, which it doesn't take in,
    /// or at the text's end.
    pub fn line_comment_end(&self, b: &[u8], i: usize) -> usize {
        (i..b.len()).find(|&j| b[j] == b'\n' || (self.carriage_return_ends_comment && b[j] == b'\r')).unwrap_or(b.len())
    }

    /// Where the block comment that opens at `i` (`/*`) ends, past its `*/`. `None` where nothing
    /// closes it and the dialect refuses that: everywhere but SQLite, which reads it to the text's
    /// end, unless the `/*` is the text's last two bytes, which SQLite reads as two operators.
    pub fn block_comment_end(&self, b: &[u8], i: usize) -> Option<usize> {
        let (mut depth, mut j) = (1usize, i + 2);
        while j < b.len() {
            if b[j] == b'*' && b.get(j + 1) == Some(&b'/') {
                depth -= 1;
                j += 2;
                if depth == 0 {
                    return Some(j);
                }
            } else if self.nested_comments && b[j] == b'/' && b.get(j + 1) == Some(&b'*') {
                depth += 1;
                j += 2;
            } else {
                j += 1;
            }
        }
        (self.open_comment_runs_on && i + 2 < b.len()).then_some(b.len())
    }

    /// The end of the dollar quote's or heredoc's opening tag at `i`, one past its second `$`,
    /// where one opens there. A `$` inside a name opens neither, which the caller knows: on
    /// Postgres and ClickHouse a `$` goes on a name.
    pub fn dollar_tag_end(&self, b: &[u8], i: usize) -> Option<usize> {
        if b.get(i) != Some(&b'$') {
            return None;
        }
        let tag_char: fn(u8) -> bool = match self.dollar {
            Dollar::Nothing => return None,
            // A Postgres tag is a name with no `$` in it: it starts as a name does, so `$1` is a
            // parameter.
            Dollar::Quote if b.get(i + 1).is_some_and(|&c| c.is_ascii_digit()) => return None,
            Dollar::Quote => |c| c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80,
            Dollar::Heredoc => |c| c.is_ascii_alphanumeric() || c == b'_',
        };
        let j = (i + 1..b.len()).find(|&j| !tag_char(b[j])).unwrap_or(b.len());
        (b.get(j) == Some(&b'$')).then_some(j + 1)
    }

    /// The last byte of the typographic quote that closes the one opening at `i`, where one opens
    /// there: `’` for `‘` and `”` for `“`. ClickHouse takes one only with at least six bytes from it.
    pub fn typographic_quote_at(&self, b: &[u8], i: usize) -> Option<u8> {
        match b.get(i..i + 3)? {
            [0xe2, 0x80, 0x98] if self.typographic_quotes && i + 5 < b.len() => Some(0x99),
            [0xe2, 0x80, 0x9c] if self.typographic_quotes && i + 5 < b.len() => Some(0x9d),
            _ => None,
        }
    }

    /// Where the typographic quote opening at `i` closes, past its closing quote, where one does.
    pub fn typographic_quote_end(&self, b: &[u8], i: usize, close: u8) -> Option<usize> {
        b[i + 3..].windows(3).position(|w| w == [0xe2, 0x80, close]).map(|at| i + 3 + at + 3)
    }

    /// Where the dollar quote or heredoc whose tag runs from `i` to `tag_end` closes, past its
    /// closing tag, where one does.
    pub fn dollar_quote_end(&self, b: &[u8], i: usize, tag_end: usize) -> Option<usize> {
        let tag = &b[i..tag_end];
        b[tag_end..].windows(tag.len()).position(|w| w == tag).map(|at| tag_end + at + tag.len())
    }
}

/// The length of a space past ASCII at the start of `b` as ClickHouse skips one, or 0: U+0085,
/// U+00A0, U+180E, U+2000 to U+200D, U+2028, U+2029, U+202F, U+205F, U+2060, U+3000 and U+FEFF.
fn unicode_space(b: &[u8]) -> usize {
    let text = match std::str::from_utf8(&b[..b.len().min(4)]) {
        Ok(text) => text,
        Err(e) => std::str::from_utf8(&b[..e.valid_up_to()]).unwrap_or_default(),
    };
    match text.chars().next() {
        Some(c @ ('\u{85}' | '\u{a0}' | '\u{180e}' | '\u{2000}'..='\u{200d}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{2060}' | '\u{3000}' | '\u{feff}')) => c.len_utf8(),
        _ => 0,
    }
}

/// The inputs in `tests/lexical.json`, each with its reading on each dialect that the test lists:
/// the comments, the strings and quoted names, and the parameters, each in the order the text has
/// them, and the prose of a leading comment. Every scanner's tests hold it to the readings it
/// makes, so that the TypeScript readers and `engine/syntax`'s scanners agree with these rules.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Corpus {
    pub cases: Vec<Case>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Case {
    /// What the input shows.
    pub about: String,
    pub text: String,
    /// The readings, by dialect name.
    pub readings: std::collections::BTreeMap<String, Reading>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reading {
    #[serde(default)]
    pub comments: Vec<String>,
    #[serde(default)]
    pub strings: Vec<String>,
    /// The parameters' names, `None` where the lexer refuses the statement, with this error.
    #[serde(default)]
    pub params: Option<Vec<String>>,
    #[serde(default)]
    pub error: Option<String>,
    /// The leading comment's prose, where the case says.
    #[serde(default)]
    pub prose: Option<String>,
}

impl Corpus {
    /// The corpus at `path`, which a test of another crate names from its own manifest's
    /// directory: `concat!(env!("CARGO_MANIFEST_DIR"), "/../core/tests/lexical.json")`.
    pub fn read(path: &str) -> Corpus {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    /// This crate's copy, for its own tests.
    #[cfg(test)]
    pub fn load() -> Corpus {
        Corpus::read(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/lexical.json"))
    }

    /// Each case's text and reading on `dialect`, where the case has one.
    pub fn on(&self, dialect: Dialect) -> impl Iterator<Item = (&Case, &Reading)> {
        self.cases.iter().filter_map(move |case| case.readings.get(dialect.name()).map(|r| (case, r)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitespace_is_each_dialects_own() {
        let at = |d: Dialect, text: &str| Rules::of(d).space_at(text.as_bytes(), 0);
        for d in [Dialect::Postgres, Dialect::Mysql, Dialect::Clickhouse] {
            assert_eq!(at(d, "\u{b}"), 1, "{d:?}");
        }
        assert_eq!(at(Dialect::Sqlite, "\u{b}"), 0);
        // SQLite reads a vertical tab inside a run of whitespace another space starts.
        assert_eq!(Rules::of(Dialect::Sqlite).skip_spaces(b" \x0b\x0bx", 0), 3);
        assert_eq!(Rules::of(Dialect::Sqlite).skip_spaces(b"1\x0bx", 1), 1);
        for d in [Dialect::Postgres, Dialect::Mysql, Dialect::Sqlite] {
            assert_eq!(at(d, "\u{a0}"), 0, "{d:?}");
            assert_eq!(at(d, "\u{3000}"), 0, "{d:?}");
        }
        assert_eq!(at(Dialect::Clickhouse, "\u{a0}x"), 2);
        assert_eq!(at(Dialect::Clickhouse, "\u{3000}"), 3);
        assert_eq!(at(Dialect::Clickhouse, "\u{feff}"), 3);
        assert_eq!(at(Dialect::Clickhouse, "\u{1680}"), 0);
        assert_eq!(at(Dialect::Clickhouse, "é"), 0);
        // A text cut inside a character is no space.
        assert_eq!(Rules::of(Dialect::Clickhouse).space_at(&"\u{3000}".as_bytes()[..2], 0), 0);
    }

    #[test]
    fn names_and_tags() {
        let pg = Rules::of(Dialect::Postgres);
        assert_eq!(pg.name_end(b"a$$b c", 0), 4);
        assert_eq!(Rules::of(Dialect::Sqlite).name_end("é$a,".as_bytes(), 0), 4);
        assert_eq!(Rules::of(Dialect::Clickhouse).name_end("a$é".as_bytes(), 0), 2);
        assert_eq!(pg.dollar_tag_end("$本文$x".as_bytes(), 0), Some(8));
        assert_eq!(pg.dollar_tag_end(b"$$", 0), Some(2));
        assert_eq!(pg.dollar_tag_end(b"$1$", 0), None);
        let ch = Rules::of(Dialect::Clickhouse);
        assert_eq!(ch.dollar_tag_end(b"$1$", 0), Some(3));
        assert_eq!(ch.dollar_tag_end("$本$".as_bytes(), 0), None);
        assert_eq!(ch.dollar_tag_end(b"$ x $", 0), None);
        assert_eq!(Rules::of(Dialect::Sqlite).dollar_tag_end(b"$$", 0), None);
    }

    #[test]
    fn comments() {
        let mysql = Rules::of(Dialect::Mysql);
        assert!(mysql.line_comment_at(b"-- x", 0) && mysql.line_comment_at(b"--\x7f", 0) && mysql.line_comment_at(b"--", 0) && !mysql.line_comment_at(b"--x", 0));
        assert_eq!(Rules::of(Dialect::Postgres).line_comment_end(b"-- a\rb", 0), 4);
        assert_eq!(mysql.line_comment_end(b"-- a\rb", 0), 6);
        assert_eq!(Rules::of(Dialect::Postgres).block_comment_end(b"/* /* */ */x", 0), Some(11));
        assert_eq!(Rules::of(Dialect::Sqlite).block_comment_end(b"/* /* */ */x", 0), Some(8));
        assert_eq!(Rules::of(Dialect::Sqlite).block_comment_end(b"/* a", 0), Some(4));
        assert_eq!(Rules::of(Dialect::Sqlite).block_comment_end(b"/*", 0), None);
        assert_eq!(mysql.block_comment_end(b"/* a", 0), None);
        let ch = Rules::of(Dialect::Clickhouse);
        assert!(ch.line_comment_at(b"// a", 0) && !Rules::of(Dialect::Postgres).line_comment_at(b"// a", 0));
        let quoted = "‘it's’ x".as_bytes();
        assert_eq!(ch.typographic_quote_at(quoted, 0).map(|close| ch.typographic_quote_end(quoted, 0, close)), Some(Some(10)));
        assert_eq!(Rules::of(Dialect::Mysql).typographic_quote_at(quoted, 0), None);
    }

    #[test]
    fn the_corpus_reads() {
        let corpus = Corpus::load();
        assert!(corpus.cases.iter().all(|c| !c.readings.is_empty() && c.readings.keys().all(|d| ["postgres", "sqlite", "mysql", "clickhouse"].contains(&d.as_str()))));
    }
}

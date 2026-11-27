//! Placeholders, read off the raw statement text.
//!
//! One named form works in every dialect, `:name`, which is also the form mysql2's
//! `namedPlaceholders` and SQLite accept; SQLite's own `@name` and `$name` are named placeholders
//! there too. Beside them a statement may use its database's own positional form instead, never
//! both: `$1` on Postgres, `?` on MySQL, `?` and `?NNN` on SQLite. A form the database does not
//! have is refused where the database would misread it: SQLite takes `$1` for a parameter named
//! `$1`, and a Postgres `$name` outside a dollar quote is a syntax error, so each is an error that
//! says what to write; MySQL's `@name` is a user variable and `$` a character of its identifiers,
//! so both stay text there, as `@` (an operator) does on Postgres. A parameter's name drops the
//! prefix: `:id`, `@id` and `$id` are `id`, and `$2` and `?2` are both `2`. SQLite reads `:id` and
//! `@id` as two parameters, so the named placeholders of one statement share one prefix. A bare `?` takes the number after the largest one so far, as SQLite numbers it, so `?, ?`
//! is `1`, `2`. Each parameter appears once in a statement, so that one place types it: a value
//! needed twice is selected once in a `with` and read from there, and every later use of a name
//! is an error at its place ([`repeated`]); a bare `?` is a parameter of its own. A name may be
//! dotted (`:auth.user_id`): each `.segment` starts like a name, so `arr[1:2]` and `:t.*` end
//! where they should. The engine rewrites placeholders to whatever the
//! database binds by position (`$n` on Postgres, `?` on MySQL, `?n` for the SQLite prepare, where
//! SQLite's own `:name` would end at the first dot) and keeps a map back to the author's text, so
//! a server error at a rewritten position lands on the right characters of the `.sql` file.
//!
//! The pass reads text by its dialect's rules ([`crate::lexical`]): which characters are
//! whitespace, single-quoted literals (with `''`), Postgres's `E''` escape strings (with
//! backslashes) and dollar quoting, double-quoted identifiers, line comments, block comments,
//! which nest on Postgres, and `::` casts. A name is read whole, so nothing opens inside one: a
//! `$` and a byte past ASCII are characters of a Postgres and a SQLite name (`a$$b`, `é$a`). SQLite's block comment does not nest: it ends at its first `*/`,
//! or at the end of the text where no `*/` closes it, as SQLite reads it. MySQL reads text its own way, by the session's
//! `sql_mode` ([`MysqlText`]): a backslash in a string escapes the character after it, a
//! double-quoted word is a string, `#` starts a line comment as `-- ` does (`--` followed by
//! anything else is two minus signs), a block comment ends at its first `*/`, and backquotes quote
//! a name. The spans it skipped as text are returned as [`Lexed::opaque`], so a caller that looks
//! for its own construct in the statement does not need a scanner of its own.
//!
//! ClickHouse writes a parameter with its type, `{name:Type}`, and has no other form: a `{` starts
//! nothing else there, and the whole of it, braces included, is the placeholder. `:name` works too
//! where a cast gives it its type, `:name::Type`, which is the only place it is read as one: a `?`
//! is ClickHouse's conditional operator, whose `:` may stand before a name (`c ? a :b`), and a
//! typed JSON path writes one too (`doc.a.:Int64`). Each use of a ClickHouse parameter declares its
//! type, so a name may appear more than once there, and its engine holds the uses to one type. Its
//! names have no dots. Its text is read its way: a backslash escapes in a string and in a name
//! quoted with `"` or backquotes, `#` starts a line comment before a space or `!`, a block comment
//! nests, `$tag$…$tag$` quotes a string where the same tag closes it, and Unicode's spaces are
//! whitespace.

use crate::dialect::Dialect;
use crate::lexical::{Dollar, Rules};
use serde::Serialize;

/// One placeholder occurrence, by byte offsets in the original text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Placeholder {
    /// The parameter name: the author's for a named placeholder (dots included, prefix dropped),
    /// and the position's number for a positional one (`$3` and SQLite's `?3` are both `3`; a
    /// bare `?` is one past the largest number before it).
    pub name: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Style {
    None,
    Named,
    Positional,
    Mixed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Lexed {
    /// Distinct parameter names in binding order: first appearance for named parameters, and
    /// by number for positional ones, so `params[k]` is the statement's `$(k + 1)`.
    pub params: Vec<String>,
    pub placeholders: Vec<Placeholder>,
    pub style: Style,
    /// Every span the walk read as text rather than code, `(start, end)` in byte offsets, in
    /// source order and never overlapping: string literals (an `E''` string from its `E`), dollar
    /// quotes, quoted identifiers, line comments (up to their newline) and block comments.
    pub opaque: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LexError {
    pub message: String,
    pub offset: usize,
}

impl std::fmt::Display for LexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.offset)
    }
}

impl std::error::Error for LexError {}

/// The placeholder syntax a rewrite emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// `$n`, numbered by binding order.
    Dollar,
    /// `?`, one per occurrence, in text order; the binding order repeats a name where it repeats.
    Question,
    /// `?n`, numbered by binding order (the parameter's 1-based index in [`Lexed::params`]), so a
    /// repeated name is one position. An author's `$1 ... $n` keeps its numbers; unlike
    /// [`Target::Dollar`], a gap closes (`$3` alone is `?1`): the binding order is `params` either
    /// way, and SQLite reads `?n` as position `n`.
    Numbered,
    /// `{name}`, ClickHouse's own form less its type, which the driver binds by name: the
    /// ClickHouse engine writes each one's type through [`rewrite_with`]. One entry of the binding
    /// order per distinct name.
    Braced,
}

/// A rewritten statement plus the map back to the author's text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Rewritten {
    pub sql: String,
    /// The binding order: for `?` one entry per occurrence, for `$n` and `?n` one per distinct name.
    pub order: Vec<String>,
    /// Per placeholder in text order: `(original start, original end, rewritten start, rewritten end)`.
    pub spans: Vec<(usize, usize, usize, usize)>,
    /// Text the rewrite adds that the original does not hold, in text order: `(original offset,
    /// rewritten start, rewritten end)`, as the collation a MySQL statement's own text needs
    /// (the MySQL engine's `mysql_collation`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inserted: Vec<(usize, usize, usize)>,
}

impl Rewritten {
    /// The byte offset in the original text that a byte offset in the rewritten text refers to.
    pub fn to_original(&self, pos: usize) -> usize {
        let mut delta: isize = 0;
        let mut regions = self.spans.clone();
        regions.extend(self.inserted.iter().map(|&(at, rs, re)| (at, at, rs, re)));
        regions.sort_by_key(|&(_, _, rs, _)| rs);
        for (os, oe, rs, re) in regions {
            if pos >= re {
                delta += (oe - os) as isize - (re - rs) as isize;
            } else if pos >= rs {
                return os;
            } else {
                break;
            }
        }
        (pos as isize + delta).max(0) as usize
    }
}

fn is_name_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_name_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Whether a ClickHouse statement's code from `from`, where a conditional's `?` ends, up to `to`,
/// where a `:` stands, is a whole operand: each parenthesis and bracket it opens is closed, and
/// what it ends with ends an expression, as a name, a number, a string, a placeholder and a
/// closing bracket do, and an operator, a comma, an opening bracket and a word that an operand
/// follows do not. `None` where it closes a bracket it did not open: the `?` stands in brackets
/// that end before the `:`.
fn whole_operand(sql: &str, opaque: &[(usize, usize)], from: usize, to: usize) -> Option<bool> {
    // The words an operand follows.
    const BEFORE_AN_OPERAND: [&str; 21] = ["and", "or", "not", "in", "like", "ilike", "between", "is", "then", "else", "when", "case", "div", "mod", "select", "where", "having", "by", "on", "interval", "distinct"];
    let b = sql.as_bytes();
    let (mut at, mut depth, mut ended) = (from, 0usize, false);
    while at < to {
        // A comment is no part of the operand; a string, a quoted name and a heredoc end one.
        if let Some(&(_, end)) = opaque.iter().find(|&&(start, _)| start == at) {
            ended = ended || !is_comment(&sql[at..]);
            at = end;
            continue;
        }
        let c = b[at];
        match c {
            _ if CLICKHOUSE.space_at(b, at) > 0 => {
                at += CLICKHOUSE.space_at(b, at);
                continue;
            }
            _ if is_name_char(c) => {
                // A name or a number, whole: a `$` is a character of a ClickHouse name past its first.
                let end = (at..b.len()).find(|&i| !(is_name_char(b[i]) || b[i] == b'$')).unwrap_or(b.len());
                ended = !BEFORE_AN_OPERAND.iter().any(|word| sql[at..end].eq_ignore_ascii_case(word));
                at = end;
                continue;
            }
            // A placeholder in braces, whole.
            b'{' => match braces_end(b, at + 1) {
                Some(close) if close < to => {
                    ended = true;
                    at = close + 1;
                    continue;
                }
                _ => ended = false,
            },
            b'(' | b'[' => {
                depth += 1;
                ended = false;
            }
            b')' | b']' => {
                depth = depth.checked_sub(1)?;
                ended = true;
            }
            _ => ended = false,
        }
        at += 1;
    }
    Some(depth == 0 && ended)
}

fn prev_is_name_char(b: &[u8], i: usize) -> bool {
    i > 0 && is_name_char(b[i - 1])
}

fn name_end(b: &[u8], start: usize) -> usize {
    let mut j = start;
    while j < b.len() && is_name_char(b[j]) {
        j += 1;
    }
    j
}

/// A name or a number from `start`, whole: a name as its dialect reads one ([`Rules::name_end`]),
/// which starts with no digit (`1$t$` is a number and a dollar quote's opener on Postgres).
fn word_end(b: &[u8], start: usize, rules: &Rules) -> usize {
    if rules.name_start(b[start]) {
        rules.name_end(b, start)
    } else {
        name_end(b, start)
    }
}

/// ClickHouse's rules, which its conditional's operand is read by.
const CLICKHOUSE: Rules = Rules::of(Dialect::Clickhouse);

/// The end of the named placeholder whose prefix (`:`, or SQLite's `@` and `$`) stands at `i`
/// ([`dotted_name_end`]). A byte the database reads as more of the same name may not follow it
/// (`:a$b`, `:aé`): the database reads one longer name there, a parameter Reylo never binds, or
/// text it refuses after the placeholder Reylo writes in its place.
fn placeholder_end(sql: &str, i: usize, rules: &Rules) -> Result<usize, LexError> {
    let b = sql.as_bytes();
    let end = dotted_name_end(b, i + 1);
    if !b.get(end).is_some_and(|&c| rules.name_part(c)) {
        return Ok(end);
    }
    let whole = rules.name_end(b, end);
    // SQLite's parameter is its prefix and the name after it; elsewhere the name runs on alone.
    let message = match rules.dialect {
        Dialect::Sqlite => format!("SQLite reads {} as one parameter, and a placeholder's name is ASCII letters, digits and _ alone; rename it", &sql[i..whole]),
        Dialect::Mysql => format!("MySQL reads {} as one name, and a placeholder's name is ASCII letters, digits and _ alone; rename the parameter", &sql[i + 1..whole]),
        _ => format!("Postgres reads {} as one name, and a placeholder's name is ASCII letters, digits and _ alone; rename the parameter", &sql[i + 1..whole]),
    };
    Err(LexError { message, offset: i })
}

/// A placeholder name from `start`: a name, then any `.segment` that starts like one
/// (`:auth.user_id`). `:t.*` stops before the dot.
fn dotted_name_end(b: &[u8], start: usize) -> usize {
    let mut j = name_end(b, start);
    while b.get(j) == Some(&b'.') && b.get(j + 1).is_some_and(|c| is_name_start(*c)) {
        j = name_end(b, j + 1);
    }
    j
}

/// What the walk read last, for the one place it matters: a Postgres subscript's `[` (after an
/// operand) against an array constructor's (after `array`, or nested in one).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Last {
    Other,
    /// A name other than `array`, a number, a quoted identifier, a placeholder, `)` or `]`.
    Operand,
    /// The `[` of a subscript, with nothing but whitespace and comments read since.
    Subscript,
}

fn digits_end(b: &[u8], start: usize) -> usize {
    let mut j = start;
    while j < b.len() && b[j].is_ascii_digit() {
        j += 1;
    }
    j
}

/// How a MySQL session reads strings, by its `sql_mode`. The default is the server's default
/// mode, which is also how the MySQL parser reads them by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MysqlText {
    /// A backslash in a string escapes the character after it (`'it\'s'`), unless the mode has
    /// NO_BACKSLASH_ESCAPES.
    pub backslash_escapes: bool,
    /// A double-quoted word is a name, not a string, where the mode has ANSI_QUOTES. A name
    /// escapes nothing with a backslash.
    pub ansi_quotes: bool,
}

impl Default for MysqlText {
    fn default() -> Self {
        MysqlText { backslash_escapes: true, ansi_quotes: false }
    }
}

impl MysqlText {
    /// Every reading a session may have.
    pub const ALL: [MysqlText; 4] = [
        MysqlText { backslash_escapes: true, ansi_quotes: false },
        MysqlText { backslash_escapes: false, ansi_quotes: false },
        MysqlText { backslash_escapes: true, ansi_quotes: true },
        MysqlText { backslash_escapes: false, ansi_quotes: true },
    ];

    /// The reading of a session whose `@@sql_mode` is `mode`. ANSI (and the modes that imply it)
    /// is expanded by the server when it is set, so the two flags are read as they are listed.
    pub fn of_mode(mode: &str) -> MysqlText {
        let set = |name: &str| mode.split(',').any(|m| m.trim().eq_ignore_ascii_case(name));
        MysqlText { backslash_escapes: !set("NO_BACKSLASH_ESCAPES"), ansi_quotes: set("ANSI_QUOTES") }
    }
}

/// From the quote at `i`, the offset one past the closing one, or `None` if the text ends first.
/// A doubled quote is one character; where `escapes`, so is a backslash and what follows it.
fn quoted_end(b: &[u8], i: usize, escapes: bool) -> Option<usize> {
    let quote = b[i];
    let mut j = i + 1;
    loop {
        match b.get(j)? {
            b'\\' if escapes => j += 2,
            &q if q == quote && b.get(j + 1) == Some(&quote) => j += 2,
            &q if q == quote => return Some(j + 1),
            _ => j += 1,
        }
    }
}

/// Which named placeholders a statement may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named {
    /// `:name` everywhere, and SQLite's own `@name` and `$name` there: the loader's rule.
    Dialect,
    /// `:name` alone; SQLite's `@name` and `$name` are errors: the portable spelling alone.
    Colon,
}

pub fn lex(sql: &str, dialect: Dialect) -> Result<Lexed, LexError> {
    lex_with(sql, dialect, Named::Dialect)
}

/// MySQL reads its text by the session's mode; [`lex_with`] reads it as the default mode does.
pub fn lex_with(sql: &str, dialect: Dialect, named: Named) -> Result<Lexed, LexError> {
    lex_text(sql, dialect, named, MysqlText::default())
}

/// A MySQL statement as a session of this reading reads it.
pub fn lex_mysql(sql: &str, named: Named, text: MysqlText) -> Result<Lexed, LexError> {
    lex_text(sql, Dialect::Mysql, named, text)
}

fn lex_text(sql: &str, dialect: Dialect, named: Named, text: MysqlText) -> Result<Lexed, LexError> {
    let sqlite_named = dialect == Dialect::Sqlite && named == Named::Dialect;
    let mysql = dialect == Dialect::Mysql;
    let clickhouse = dialect == Dialect::Clickhouse;
    let rules = Rules::of(dialect);
    let b = sql.as_bytes();
    let mut i = 0usize;
    let mut last = Last::Other;
    let mut params: Vec<String> = Vec::new();
    let mut placeholders: Vec<Placeholder> = Vec::new();
    let mut opaque: Vec<(usize, usize)> = Vec::new();
    // The largest position a positional placeholder has named so far; a bare `?` takes the next.
    let mut largest = 0u64;
    // The span of the first named placeholder, whose prefix every later one repeats.
    let mut first_named: Option<(usize, usize)> = None;
    // Where each `?` of a ClickHouse conditional that has not met its `:` ends.
    let mut conditionals: Vec<usize> = Vec::new();
    let mut push = |name: String, start: usize, end: usize, params: &mut Vec<String>| {
        if !params.contains(&name) {
            params.push(name.clone());
        }
        placeholders.push(Placeholder { name, start, end });
    };
    let position = |digits: &str, largest: &mut u64| {
        let n = digits.trim_start_matches('0');
        let n = if n.is_empty() { "0" } else { n };
        *largest = (*largest).max(n.parse().unwrap_or(u64::MAX));
        n.to_string()
    };
    while i < b.len() {
        let c = b[i];
        match c {
            // A MySQL string, unless ANSI_QUOTES makes `"` a name's quote.
            b'\'' | b'"' if c == b'\'' || (mysql && !text.ansi_quotes) => {
                let escapes = if mysql { text.backslash_escapes } else { rules.backslash_in_strings };
                let Some(end) = quoted_end(b, i, escapes) else {
                    return Err(LexError { message: "unterminated string literal".into(), offset: i });
                };
                opaque.push((i, end));
                i = end;
                last = Last::Other;
            }
            // Postgres's escape string: SQLite, MySQL and ClickHouse have none, so there `e'\'` is
            // a name and a string.
            b'E' | b'e' if rules.escape_strings && b.get(i + 1) == Some(&b'\'') && !(i > 0 && rules.name_part(b[i - 1])) => {
                let start = i;
                i += 2;
                loop {
                    match b.get(i) {
                        None => return Err(LexError { message: "unterminated escape string".into(), offset: start }),
                        Some(b'\\') => i += 2,
                        Some(b'\'') if b.get(i + 1) == Some(&b'\'') => i += 2,
                        Some(b'\'') => {
                            i += 1;
                            break;
                        }
                        Some(_) => i += 1,
                    }
                }
                opaque.push((start, i));
                last = Last::Other;
            }
            b'"' | b'`' => {
                let Some(end) = quoted_end(b, i, rules.backslash_in_names) else {
                    return Err(LexError { message: "unterminated quoted identifier".into(), offset: i });
                };
                opaque.push((i, end));
                i = end;
                last = Last::Operand;
            }
            // A line comment, to its line end ([`Rules::line_comment_at`]): MySQL's `--` is one only
            // before a space or a control character, or at the end of the text (`:a --:b` is
            // `:a - (-:b)`), ClickHouse's `#` only before a space or a `!`, and its `//` too, and
            // Postgres ends one at a carriage return as at a line feed.
            b'-' | b'#' | b'/' if rules.line_comment_at(b, i) => {
                let end = rules.line_comment_end(b, i);
                opaque.push((i, end));
                i = end;
            }
            // A block comment nests on Postgres and ClickHouse, and elsewhere ends at its first `*/`.
            // One that no `*/` closes runs to the end of the text on SQLite, which prepares it so,
            // and is an error elsewhere. A `/*` with nothing after it is an error on SQLite too,
            // which reads it as the operators `/` and `*`.
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let Some(end) = rules.block_comment_end(b, i) else {
                    return Err(LexError { message: "unterminated block comment".into(), offset: i });
                };
                opaque.push((i, end));
                i = end;
            }
            // ClickHouse's typographic quotes: `‘…’` a string and `“…”` a name, to the first closing quote.
            0xe2 if rules.typographic_quote_at(b, i).is_some() => {
                let close = rules.typographic_quote_at(b, i).unwrap_or_default();
                let Some(end) = rules.typographic_quote_end(b, i, close) else {
                    let message = if close == 0x99 { "unterminated string literal" } else { "unterminated quoted identifier" };
                    return Err(LexError { message: message.into(), offset: i });
                };
                opaque.push((i, end));
                i = end;
                last = Last::Operand;
            }
            b':' => {
                if b.get(i + 1) == Some(&b':') {
                    // A `::` cast: the type name after it is not a parameter.
                    i += 2;
                    if i < b.len() && is_name_start(b[i]) {
                        i = word_end(b, i, &rules);
                    }
                    last = Last::Operand;
                    continue;
                }
                // The `:` of a ClickHouse conditional, `c ? a : b`: the first after its `?` that
                // a whole operand stands before. A name and a cast may follow it, and are no
                // parameter (`c ? a :b::String`); a `:` inside the operand opens one
                // (`c ? abs(:n::Int32) : 0`, `c ? 1 + :n::Int32 : 0`, `c ? :n::Int32 : 0`).
                let mut separates = false;
                while let Some(&after) = conditionals.last() {
                    match whole_operand(sql, &opaque, after, i) {
                        // The parentheses the `?` stands in closed before any `:`: it awaits none.
                        None => {
                            conditionals.pop();
                        }
                        Some(whole) => {
                            separates = whole;
                            break;
                        }
                    }
                }
                if separates {
                    conditionals.pop();
                    i += 1;
                    last = Last::Other;
                    continue;
                }
                match b.get(i + 1) {
                    // A colon that opens a Postgres subscript is a slice with no lower bound
                    // (`tags[:n]` is `tags[1:n]`), since no expression starts with a colon there,
                    // so the name after it is a column's; a parameter as the subscript is written
                    // `tags[(:n)]`. An array constructor's `[` (`array[:a, :b]`) follows `array`
                    // or another `[`, not an operand.
                    _ if last == Last::Subscript && dialect == Dialect::Postgres => {
                        i += 1;
                        last = Last::Other;
                    }
                    // A colon right after a name or a number opens nothing (`arr[lo:hi]`), and a
                    // Postgres name may end in `$` or a byte past ASCII (`x[café:fin]`).
                    _ if dialect == Dialect::Postgres && i > 0 && rules.name_part(b[i - 1]) => {
                        i += 1;
                        last = Last::Other;
                    }
                    // On ClickHouse a `:name` is a parameter only where a cast types it
                    // (`:name::Type`): the `:` of its conditional operator may stand before a
                    // name (`c ? a :b`), and so does a typed JSON path's (`doc.a.:Int64`).
                    Some(&n) if is_name_start(n) && !prev_is_name_char(b, i) && clickhouse => {
                        let end = name_end(b, i + 1);
                        let cast = b.get(end) == Some(&b':') && b.get(end + 1) == Some(&b':') && b.get(end + 2).is_some_and(|&c| is_name_start(c));
                        if cast && (i == 0 || b[i - 1] != b'.') {
                            one_prefix(sql, &mut first_named, i, end)?;
                            push(sql[i + 1..end].to_string(), i, end, &mut params);
                            i = end;
                            last = Last::Operand;
                        } else {
                            i += 1;
                            last = Last::Other;
                        }
                    }
                    Some(&n) if is_name_start(n) && !prev_is_name_char(b, i) => {
                        let end = placeholder_end(sql, i, &rules)?;
                        one_prefix(sql, &mut first_named, i, end)?;
                        push(sql[i + 1..end].to_string(), i, end, &mut params);
                        i = end;
                        last = Last::Operand;
                    }
                    _ => {
                        i += 1;
                        last = Last::Other;
                    }
                }
            }
            // ClickHouse's `{name:Type}`, the only thing a `{` starts there: the placeholder is the
            // whole of it, to the `}` that closes it, past the parentheses and strings of its type
            // (`{at:DateTime64(3, 'UTC')}`). One with no name, colon or type is left to the server,
            // whose error says what it expects.
            b'{' if clickhouse => {
                let name_start = rules.skip_spaces(b, i + 1);
                // A ClickHouse name may hold a `$` past its first character.
                let name_stop = if b.get(name_start).is_some_and(|&c| is_name_start(c)) { (name_start..b.len()).find(|&at| !(is_name_char(b[at]) || b[at] == b'$')).unwrap_or(b.len()) } else { name_start };
                let colon = rules.skip_spaces(b, name_stop);
                let close = if name_stop > name_start && b.get(colon) == Some(&b':') && b.get(colon + 1) != Some(&b':') { braces_end(b, colon + 1) } else { None };
                match close {
                    Some(close) if !sql[colon + 1..close].trim().is_empty() => {
                        one_prefix(sql, &mut first_named, i, close + 1)?;
                        push(sql[name_start..name_stop].to_string(), i, close + 1, &mut params);
                        i = close + 1;
                        last = Last::Operand;
                    }
                    _ => {
                        i += 1;
                        last = Last::Other;
                    }
                }
            }
            b'$' => {
                // A `$` inside a ClickHouse name is the name's (`a$b$`), and opens no heredoc.
                let named = clickhouse && (prev_is_name_char(b, i) || (i > 0 && b[i - 1] == b'$'));
                // A Postgres dollar quote, and ClickHouse's heredoc, whose tag is ASCII letters,
                // digits and `_` (`$1$hi$1$`) and which is one only where the tag closes it later.
                if let Some(tag_end) = rules.dollar_tag_end(b, i).filter(|_| !named) {
                    match rules.dollar_quote_end(b, i, tag_end) {
                        Some(end) => {
                            opaque.push((i, end));
                            i = end;
                            last = Last::Other;
                            continue;
                        }
                        None if rules.dollar == Dollar::Quote => {
                            return Err(LexError { message: format!("unterminated dollar-quoted string {}", &sql[i..tag_end]), offset: i });
                        }
                        None => {}
                    }
                }
                match b.get(i + 1) {
                    // `a$1` and `a$b` are identifiers.
                    _ if prev_is_name_char(b, i) => {
                        i += 1;
                        last = Last::Operand;
                    }
                    Some(&n) if n.is_ascii_digit() && dialect == Dialect::Postgres => {
                        let end = digits_end(b, i + 1);
                        push(position(&sql[i + 1..end], &mut largest), i, end, &mut params);
                        i = end;
                        last = Last::Operand;
                    }
                    Some(&n) if is_name_start(n) && sqlite_named => {
                        let end = placeholder_end(sql, i, &rules)?;
                        one_prefix(sql, &mut first_named, i, end)?;
                        push(sql[i + 1..end].to_string(), i, end, &mut params);
                        i = end;
                        last = Last::Operand;
                    }
                    Some(&n) if (is_name_start(n) || n.is_ascii_digit()) && dialect != Dialect::Mysql => {
                        return Err(foreign(sql, i, dialect));
                    }
                    _ => {
                        i += 1;
                        last = Last::Other;
                    }
                }
            }
            b'@' => match b.get(i + 1) {
                Some(&n) if is_name_start(n) && !prev_is_name_char(b, i) && sqlite_named => {
                    let end = placeholder_end(sql, i, &rules)?;
                    one_prefix(sql, &mut first_named, i, end)?;
                    push(sql[i + 1..end].to_string(), i, end, &mut params);
                    i = end;
                    last = Last::Operand;
                }
                Some(&n) if is_name_start(n) && !prev_is_name_char(b, i) && dialect == Dialect::Sqlite => {
                    return Err(foreign(sql, i, dialect));
                }
                _ => {
                    i += 1;
                    last = Last::Other;
                }
            },
            // ClickHouse's `?` is its conditional operator (`c ? a : b`), and it binds by name alone.
            b'?' if clickhouse => {
                i += 1;
                conditionals.push(i);
                last = Last::Other;
            }
            b'?' if dialect != Dialect::Postgres => {
                if b.get(i + 1).is_some_and(u8::is_ascii_digit) && dialect == Dialect::Mysql {
                    let end = digits_end(b, i + 1);
                    return Err(LexError { message: format!("MySQL has no numbered placeholder {}; write ? for each value, or :name", &sql[i..end]), offset: i });
                }
                if b.get(i + 1).is_some_and(u8::is_ascii_digit) {
                    // SQLite's `?NNN` names position NNN, as `$NNN` does, so it takes that name.
                    let end = digits_end(b, i + 1);
                    push(position(&sql[i + 1..end], &mut largest), i, end, &mut params);
                    i = end;
                } else {
                    largest = largest.saturating_add(1);
                    push(largest.to_string(), i, i + 1, &mut params);
                    i += 1;
                }
                last = Last::Operand;
            }
            // Whitespace leaves what was read last as it was, as a comment does.
            _ if rules.space_at(b, i) > 0 => i += rules.space_at(b, i),
            // A name or a number, whole, so that nothing opens inside one: `array` opens a
            // constructor, anything else is an operand.
            _ if is_name_char(c) || rules.name_start(c) => {
                let end = word_end(b, i, &rules);
                last = if sql[i..end].eq_ignore_ascii_case("array") { Last::Other } else { Last::Operand };
                i = end;
            }
            b')' | b']' => {
                i += 1;
                last = Last::Operand;
            }
            // SQLite's `[name]` is a quoted identifier, which ends at the first `]`: a quote or a
            // `?` inside it is part of the name.
            b'[' if dialect == Dialect::Sqlite => {
                let Some(close) = b[i..].iter().position(|&c| c == b']') else {
                    return Err(LexError { message: "unterminated quoted identifier".into(), offset: i });
                };
                opaque.push((i, i + close + 1));
                i += close + 1;
                last = Last::Operand;
            }
            b'[' => {
                i += 1;
                last = if last == Last::Operand { Last::Subscript } else { Last::Other };
            }
            _ => {
                i += 1;
                last = Last::Other;
            }
        }
    }
    let positional = params.iter().filter(|p| is_positional(p)).count();
    let style = match (positional, params.len()) {
        (_, 0) => Style::None,
        (0, _) => Style::Named,
        (p, n) if p == n => Style::Positional,
        _ => Style::Mixed,
    };
    if style == Style::Positional {
        // `$2 ... $1` binds `$1` first: the describe reports the parameters by number.
        params.sort_by_key(|p| p.parse::<u64>().unwrap_or(u64::MAX));
    }
    Ok(Lexed { params, placeholders, style, opaque })
}

/// The offset of the `}` that closes a ClickHouse placeholder whose type starts at `from`: the
/// first one outside the type's parentheses and its strings, in which a backslash escapes.
fn braces_end(b: &[u8], from: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut j = from;
    while j < b.len() {
        match b[j] {
            b'\'' | b'"' | b'`' => j = quoted_end(b, j, true)?,
            b'(' => {
                depth += 1;
                j += 1;
            }
            b')' => {
                depth = depth.checked_sub(1)?;
                j += 1;
            }
            b'}' if depth == 0 => return Some(j),
            b'{' | b'}' | b';' => return None,
            _ => j += 1,
        }
    }
    None
}

/// SQLite reads `:a`, `@a` and `$a` as three parameters, and a name here drops its prefix, so every
/// named placeholder of a statement repeats the prefix of the first.
fn one_prefix(sql: &str, first: &mut Option<(usize, usize)>, start: usize, end: usize) -> Result<(), LexError> {
    match *first {
        None => {
            *first = Some((start, end));
            Ok(())
        }
        Some((s, _)) if sql.as_bytes()[s] == sql.as_bytes()[start] => Ok(()),
        // ClickHouse's two forms, of which neither is the other with another prefix.
        Some((s, e)) if sql.as_bytes()[s] == b'{' || sql.as_bytes()[start] == b'{' => Err(LexError {
            message: format!("{} and {} are written two ways; write every parameter of a statement one way, as {{name:Type}} or as :name::Type", &sql[start..end], &sql[s..e]),
            offset: start,
        }),
        Some((s, e)) => Err(LexError {
            message: format!(
                "{} and {} have different prefixes; write every named parameter of a statement with one prefix, here {}{}",
                &sql[start..end],
                &sql[s..e],
                &sql[s..s + 1],
                &sql[start + 1..end]
            ),
            offset: start,
        }),
    }
}

/// The error for a `$name`, `$1` or `@name` the dialect would misread: Reylo's named placeholder is
/// `:name` (and, outside [`Named::Colon`], SQLite's own `@name` and `$name`), and SQLite's
/// numbered one `?1`.
fn foreign(sql: &str, i: usize, dialect: Dialect) -> LexError {
    let b = sql.as_bytes();
    let digits = b.get(i + 1).is_some_and(u8::is_ascii_digit);
    let end = if digits { digits_end(b, i + 1) } else { dotted_name_end(b, i + 1) };
    let written = &sql[i..end];
    let fix = if digits { format!("?{}", &sql[i + 1..end]) } else { format!(":{}", &sql[i + 1..end]) };
    let message = match dialect {
        Dialect::Sqlite => format!("SQLite reads {written} as a parameter that Reylo does not bind; write {fix}"),
        Dialect::Clickhouse => format!("{written} is not a placeholder on ClickHouse, which writes a parameter with its type; write {{{}:Type}}", if digits { format!("p{}", &sql[i + 1..end]) } else { sql[i + 1..end].to_string() }),
        _ => format!("{written} is not a placeholder here; Reylo's named placeholder is {fix}"),
    };
    LexError { message, offset: i }
}

/// One use of a parameter after its first, by byte offsets in the original text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repeated {
    pub message: String,
    pub start: usize,
    pub end: usize,
}

impl From<Repeated> for LexError {
    fn from(r: Repeated) -> LexError {
        LexError { message: r.message, offset: r.start }
    }
}

/// Every use of a parameter after its first, each with the error that says so. Each parameter
/// appears once in a statement, so that one place types it:
/// a value read twice is selected once in a `with` and read from there. The rule holds for every
/// form a parameter is named by: `:name` (a dotted global such as `:auth.user_id` too), SQLite's
/// `@name` and `$name`, and the positional `$n` and `?n`, whose name is the number, so `?1` after
/// a bare `?` that took 1 repeats it. A bare `?` is a parameter of its own and never repeats.
/// Reylo's analysis reports each one where it is written, and a host's build may fail on it.
pub fn repeated(sql: &str, lexed: &Lexed) -> Vec<Repeated> {
    let mut seen: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    for p in &lexed.placeholders {
        if !seen.contains(&p.name.as_str()) {
            seen.push(&p.name);
            continue;
        }
        let written = &sql[p.start..p.end];
        // The name the example selects the value as: the last segment of a dotted name, and `pn`
        // for a position.
        let column = if is_positional(&p.name) { format!("p{}", p.name) } else { p.name.rsplit('.').next().unwrap_or(&p.name).to_string() };
        // The example casts the value, since nothing else types a parameter a select list reads:
        // Postgres takes it as text, MySQL as a string, and SQLite leaves it untyped.
        out.push(Repeated {
            message: format!(
                "The parameter {written} appears more than once; use each parameter once, and select it in a with to read it twice, cast to its type, as in: with input as (select {written}::<type> as {column}) select … from input"
            ),
            start: p.start,
            end: p.end,
        });
    }
    out
}

/// Whether a parameter name is one the lexer minted for a positional placeholder: a number,
/// where a named one starts with a letter or `_`.
pub fn is_positional(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|c| c.is_ascii_digit())
}

/// Rewrite every placeholder to the target's syntax.
pub fn rewrite(sql: &str, lexed: &Lexed, target: Target) -> Rewritten {
    rewrite_with(sql, lexed, target, &|_, rendered| rendered.to_string())
}

/// Rewrite with a hook over each rendered placeholder — a probe pins a cast on it.
pub fn rewrite_with(sql: &str, lexed: &Lexed, target: Target, decorate: &dyn Fn(&str, &str) -> String) -> Rewritten {
    let mut out = String::with_capacity(sql.len());
    let mut spans = Vec::with_capacity(lexed.placeholders.len());
    let mut order: Vec<String> = Vec::new();
    let mut last = 0;
    for p in &lexed.placeholders {
        let rendered = match target {
            Target::Dollar => {
                if is_positional(&p.name) {
                    format!("${}", p.name)
                } else {
                    format!("${}", lexed.params.iter().position(|q| q == &p.name).map_or(0, |i| i + 1))
                }
            }
            Target::Question => "?".to_string(),
            Target::Numbered => format!("?{}", lexed.params.iter().position(|q| q == &p.name).map_or(0, |i| i + 1)),
            Target::Braced => format!("{{{}}}", p.name),
        };
        let rendered = decorate(&p.name, &rendered);
        out.push_str(&sql[last..p.start]);
        let rs = out.len();
        out.push_str(&rendered);
        spans.push((p.start, p.end, rs, out.len()));
        last = p.end;
        if target == Target::Question {
            order.push(p.name.clone());
        }
    }
    out.push_str(&sql[last..]);
    if target != Target::Question {
        order = lexed.params.clone();
    }
    Rewritten { sql: out, order, spans, inserted: Vec::new() }
}

/// The first keywords of the statements SQLite and MySQL know that are not queries: DDL,
/// transaction control, session settings and the like. A first word outside this list and
/// outside the typed kinds is left to the database, whose syntax error names it (`selec`).
const NOT_QUERIES: &[&str] = &[
    "alter", "analyze", "attach", "begin", "binlog", "cache", "call", "change", "check", "checksum", "clone", "commit", "create", "deallocate",
    "describe", "desc", "detach", "do", "drop", "end", "execute", "explain", "flush", "get", "grant", "handler", "help", "import", "install",
    "kill", "load", "lock", "optimize", "pragma", "prepare", "purge", "reindex", "release", "rename", "repair", "reset", "resignal", "restart",
    "revoke", "rollback", "savepoint", "set", "show", "shutdown", "signal", "start", "stop", "truncate", "uninstall", "unlock", "use",
    "vacuum", "xa",
];

/// How a file the type check refuses (a migration or a schema file, which nothing imports, but
/// which tsconfig.json's `include` hands to the content mapper all the same) is left out of it.
const LEAVE_OUT: &str = "so if it is a migration or a schema file, leave it out of the type check with \"exclude\" in tsconfig.json, as in \"exclude\": [\"db/**\"]";

/// The error of a file that holds several statements, on every database.
pub fn several_statements() -> String {
    format!("A typed .sql file holds one statement and this one holds several, {LEAVE_OUT}.")
}

/// The database's error on a command ([`Statement::Command`]), with how to leave the file out of
/// the type check: a migration's `CREATE TABLE` prepares against a schema that already has the
/// table on SQLite, and fails there.
pub fn refused_command(message: &str) -> String {
    format!("{}, {LEAVE_OUT}.", message.trim_end().trim_end_matches('.'))
}

/// What the one statement of a typed file is.
#[derive(Debug, PartialEq, Eq)]
pub enum Statement {
    /// One of the kinds the database's analysis reads, or a first word it leaves to the database.
    Query,
    /// Any other statement, by its first keyword, lowercased: DDL, a session setting, a
    /// transaction's control, `EXPLAIN`. It is typed from the database's describe alone.
    Command(String),
}

/// Whether a span of [`Lexed::opaque`], from its start, is a comment rather than a string, a quoted
/// name or a dollar quote, each of which starts with a quote, a `$`, a `[` or an `E`.
pub fn is_comment(span: &str) -> bool {
    matches!(span.as_bytes().first(), Some(b'-' | b'/' | b'#'))
}

/// The first word of the text past whitespace and comments, lowercased.
pub fn first_word(sql: &str, lexed: &Lexed, dialect: Dialect) -> String {
    let start = past_comments(sql, lexed, dialect, 0);
    sql[start..].chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_lowercase()
}

/// The first byte at or after `i` that is neither whitespace nor inside a comment.
fn past_comments(sql: &str, lexed: &Lexed, dialect: Dialect, mut i: usize) -> usize {
    let rules = Rules::of(dialect);
    loop {
        i = rules.skip_spaces(sql.as_bytes(), i);
        match lexed.opaque.iter().find(|&&(start, _)| start == i && is_comment(&sql[start..])) {
            Some(&(_, end)) => i = end,
            None => return i,
        }
    }
}

/// Whether the text is one statement, and of which kind: a query when its first keyword (past
/// whitespace and comments) is one of `kinds`, or an opening parenthesis, and a command when it is
/// another statement's. Nothing but whitespace, semicolons and comments may follow its first
/// semicolon outside the text spans: SQLite prepares only the first statement of the text, so a
/// migration file would otherwise come back typed from its first statement alone; Postgres's
/// parse refuses several (`AnalyzeError` in `analyze.rs`).
pub fn one_statement(sql: &str, lexed: &Lexed, dialect: Dialect, kinds: &[&str]) -> Result<Statement, String> {
    let bytes = sql.as_bytes();
    // A `;` inside a placeholder is its type's, as in ClickHouse's `{e:Enum8('a;b' = 1)}`.
    let in_text = |i: usize| lexed.opaque.iter().any(|&(start, end)| start <= i && i < end) || lexed.placeholders.iter().any(|p| p.start <= i && i < p.end);
    if let Some(semicolon) = (0..bytes.len()).find(|&i| bytes[i] == b';' && !in_text(i)) {
        let mut i = semicolon + 1;
        loop {
            i = past_comments(sql, lexed, dialect, i);
            if i < bytes.len() && bytes[i] == b';' {
                i += 1;
            } else {
                break;
            }
        }
        if i < bytes.len() {
            return Err(several_statements());
        }
    }
    let word = first_word(sql, lexed, dialect);
    if kinds.contains(&word.as_str()) || sql[past_comments(sql, lexed, dialect, 0)..].starts_with('(') || !NOT_QUERIES.contains(&word.as_str()) {
        return Ok(Statement::Query);
    }
    Ok(Statement::Command(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(sql: &str, d: Dialect) -> Vec<String> {
        lex(sql, d).unwrap().params
    }

    #[test]
    fn styles() {
        assert_eq!(names("select :a, :b, :a", Dialect::Postgres), ["a", "b"]);
        assert_eq!(names("select $1, $2, $1", Dialect::Postgres), ["1", "2"]);
        assert_eq!(names("select ?, ?", Dialect::Sqlite), ["1", "2"]);
        assert_eq!(names("select ?, ?", Dialect::Mysql), ["1", "2"]);
        assert_eq!(lex("select :a, $1", Dialect::Postgres).unwrap().style, Style::Mixed);
        assert_eq!(lex("select 1", Dialect::Postgres).unwrap().style, Style::None);
    }

    #[test]
    fn skips_text_and_casts() {
        let sql = "select ':x' as a, \":y\" as b, e'\\':z' as c, $$:w$$ as d, x::int, arr[1:2], tags ? 'k' -- :c\n /* :d /* :e */ */ from t where id = :id";
        assert_eq!(names(sql, Dialect::Postgres), ["id"]);
        // A SQLite block comment does not nest: it ends at the first `*/`, whatever it holds.
        assert_eq!(names("select id /* see /* below */, :x from t", Dialect::Sqlite), ["x"]);
        assert_eq!(names("select name from t where id = /* /* */ :id -- */", Dialect::Sqlite), ["id"]);
        // One that no `*/` closes runs to the end of the text, as SQLite reads it.
        assert_eq!(names("select :x /* :y", Dialect::Sqlite), ["x"]);
        assert_eq!(lex("select 1 /* a", Dialect::Sqlite).unwrap().opaque, [(9, 13)]);
        assert_eq!(lex("select 1 /*\n", Dialect::Sqlite).unwrap().opaque, [(9, 12)]);
        // A `/*` with nothing after it is two operators to SQLite, which refuses them.
        assert_eq!(lex("select 1; /*", Dialect::Sqlite).unwrap_err().offset, 10);
        assert_eq!(names("select json_extract(j, '$.a') = ? and x = :n", Dialect::Sqlite), ["1", "n"]);
        // SQLite's bracketed name is text, whatever it holds; elsewhere a `[` opens a subscript.
        assert_eq!(names("select [a'b] as x, :p as p, [c'd?] as y, [n:m] from t", Dialect::Sqlite), ["p"]);
        assert_eq!(lex("select [a from t", Dialect::Sqlite).unwrap_err().offset, 7);
        assert_eq!(names("select `a:b`, 'x' from t where a = :a", Dialect::Mysql), ["a"]);
    }

    #[test]
    fn a_line_comment_ends_where_its_database_ends_it() {
        // Postgres ends a `--` comment at a carriage return as at a line feed, so what follows a
        // bare one is code there; MySQL, SQLite and ClickHouse read on to the line feed.
        assert_eq!(names("select $1::int as a --note\r, $2::int as b", Dialect::Postgres), ["1", "2"]);
        assert_eq!(names("select :a::int as a --note\r, :b::int as b", Dialect::Postgres), ["a", "b"]);
        assert_eq!(names("select :a -- note\r, :b\n, :c", Dialect::Mysql), ["a", "c"]);
        assert_eq!(names("select :a # note\r, :b\n, :c", Dialect::Mysql), ["a", "c"]);
        assert_eq!(names("select :a -- note\r, :b\n, :c", Dialect::Sqlite), ["a", "c"]);
        assert_eq!(names("SELECT {a:Int32} -- note\r, {b:Int32}\n, {c:Int32}", Dialect::Clickhouse), ["a", "c"]);
        // A line that ends in `\r\n` reads the same either way.
        let sql = "-- the user\r\nselect :id -- by id\r\n;\r\n-- done\r\n";
        let l = lex(sql, Dialect::Postgres).unwrap();
        assert_eq!(l.params, ["id"]);
        assert_eq!(first_word(sql, &l, Dialect::Postgres), "select");
        assert_eq!(one_statement(sql, &l, Dialect::Postgres, &["select"]), Ok(Statement::Query));
    }

    #[test]
    fn one_named_form_and_the_native_positional_one() {
        // A form the dialect would misread is an error that says what to write.
        let err = |sql: &str, d: Dialect| lex(sql, d).unwrap_err();
        assert_eq!(err("select $name", Dialect::Postgres).message, "$name is not a placeholder here; Reylo's named placeholder is :name");
        assert_eq!(err("select 1 where x = $2", Dialect::Sqlite).message, "SQLite reads $2 as a parameter that Reylo does not bind; write ?2");
        assert_eq!(err("select ?2", Dialect::Mysql).message, "MySQL has no numbered placeholder ?2; write ? for each value, or :name");
        assert_eq!(err("select $name", Dialect::Postgres).offset, 7);
        // Where the dialect reads the form as something else, it is that: MySQL's user variable and
        // `$` identifier, Postgres's `@` operator and `a$1` identifier.
        assert!(names("select @x := 1, $col, $1 from t", Dialect::Mysql).is_empty());
        assert!(names("select @ -5, a$1, b$c from t", Dialect::Postgres).is_empty());
        // A `-- @returns one` line is a comment like any other.
        assert!(names("-- @returns one\nselect 1", Dialect::Postgres).is_empty());
    }

    #[test]
    fn clickhouse_writes_a_parameter_with_its_type() {
        let l = |sql: &str| lex(sql, Dialect::Clickhouse).unwrap();
        // The placeholder is the whole of `{name:Type}`, past its type's parentheses and strings.
        let sql = "SELECT {id:UInt64}, { since : DateTime64(3, 'UTC') }, {e:Enum8('}' = 1, 'it\\'s' = 2)} FROM t";
        let found = l(sql);
        assert_eq!(found.params, ["id", "since", "e"]);
        assert_eq!(found.placeholders.iter().map(|p| &sql[p.start..p.end]).collect::<Vec<_>>(), ["{id:UInt64}", "{ since : DateTime64(3, 'UTC') }", "{e:Enum8('}' = 1, 'it\\'s' = 2)}"]);
        assert_eq!(found.style, Style::Named);
        // A name may be used again: each use carries its type.
        assert_eq!(l("SELECT {a:UInt8} + {a:UInt8}").params, ["a"]);
        assert_eq!(rewrite("SELECT {a:UInt8} + 1", &l("SELECT {a:UInt8} + 1"), Target::Braced).sql, "SELECT {a} + 1");
        // `:name` is one only where a cast gives it its type.
        assert_eq!(l("SELECT :id::UInt64, x::String FROM t WHERE n IN :ns::Array(UInt8)").params, ["id", "ns"]);
        assert!(l("SELECT c ? a :b, doc.a.:Int64, m[:k], :bare FROM t").params.is_empty());
        // Its `?` is an operator, and a `{` that opens no placeholder is the server's to refuse.
        assert!(l("SELECT a ? 1 : 2, {x}, {1:UInt8}, {y:} FROM t").params.is_empty());
        // The operator's `:` may stand before a name and a cast, which is no parameter; a
        // parameter that is its operand has no operand before it.
        assert!(l("SELECT c ? a :b::String, c ? 'x' /* or */ :d::String, c ? (d ? 1 :e::UInt8) :f::UInt8, c ? f(a) :g::String, c ? m['k'] :h::String FROM t").params.is_empty());
        assert_eq!(l("SELECT c ? :a::UInt8 : 0, c ? 1 : :b::UInt8, :c::UInt8 FROM t").params, ["a", "b", "c"]);
        // A parameter inside the operand is one: in a call, after an operator or a word that takes an operand, and in brackets.
        assert_eq!(l("SELECT 1 ? abs(:n::Int32) : 0 AS v, c ? 1 + :m::Int32 : 0, c ? a AND :o::UInt8 : 0, c ? [:p::UInt8, 2] : [], (c ? 1 : 2) + :q::UInt8").params, ["n", "m", "o", "p", "q"]);
        assert_eq!(l("SELECT c ? {r:UInt8} :s::UInt8, c ? {t:Decimal(9, 2)} + 1 : 0").params, ["r", "t"]);
        // A name with a `$` in it is one name, whatever word it ends with.
        assert!(l("SELECT 1 ? a$and :b::UInt8, 1 ? a$b$ :c::UInt8 FROM (SELECT 1 AS a$and, 2 AS b, 3 AS a$b$, 4 AS c)").params.is_empty());
        // A `$` is a character of a name, a parameter's too, and opens a heredoc only where none stands before it.
        assert_eq!(l("SELECT 1 AS a$b$, {p:UInt8} AS q, 2 AS c$b$, {x$y:UInt8}").params, ["p", "x$y"]);
        // Its text: a backslash escapes in a string and a quoted name, `#` comments before a space
        // or a `!`, block comments nest, and a heredoc quotes.
        assert!(l("SELECT 'it\\'s {a:UInt8}', `{b:UInt8}\\``, \"{c:UInt8}\\\"\" # {d:UInt8}\n #! {e:UInt8}\n /* {f:UInt8} /* {g:UInt8} */ {h:UInt8} */, $q${i:UInt8}$q$ -- {j:UInt8}").params.is_empty());
        assert_eq!(l("SELECT 1 #{k:UInt8}").params, ["k"]);
        // A heredoc's tag is ASCII letters, digits and `_`, or none, and a `$` that no tag closes opens none.
        assert!(l("SELECT $1$ {a:UInt8} $1$, $${c:UInt8}$$, $t_2${d:UInt8}$t_2$").params.is_empty());
        assert_eq!(l("SELECT 1 $ {a:UInt8} $, {b:UInt8}").params, ["a", "b"]);
        assert_eq!(l("SELECT $ x ${b:UInt8}$ x $, $本${c:UInt8}$本$").params, ["b", "c"]);
        // A `;` in a placeholder's type is the type's: the statement is one.
        let labelled = "SELECT {e:Enum8('a;b' = 1)} AS e;";
        assert_eq!(one_statement(labelled, &l(labelled), Dialect::Clickhouse, &["select"]), Ok(Statement::Query));
        // Unicode's spaces are ClickHouse's whitespace, after a semicolon and before the first word too.
        let spaced = "\u{a0}INSERT INTO t VALUES ({a:UInt8});\u{3000}";
        assert_eq!(one_statement(spaced, &l(spaced), Dialect::Clickhouse, &["insert"]), Ok(Statement::Query));
        assert_eq!(first_word(spaced, &l(spaced), Dialect::Clickhouse), "insert");
        // `//` starts a line comment, and typographic quotes quote, with no escape.
        assert_eq!(l("SELECT 1 AS a // {a:UInt8}\n, {b:UInt8} AS b").params, ["b"]);
        let quoted = "SELECT ‘{a:UInt8} it's’ AS s, “{b:UInt8}” AS n, {c:UInt8} AS c";
        assert_eq!(l(quoted).params, ["c"]);
        assert_eq!(l(quoted).opaque.iter().map(|&(s, e)| &quoted[s..e]).collect::<Vec<_>>(), ["‘{a:UInt8} it's’", "“{b:UInt8}”"]);
        assert_eq!(lex("SELECT ‘open, {a:UInt8}", Dialect::Clickhouse).unwrap_err().message, "unterminated string literal");
        assert_eq!(names("select ‘{a}’ as s, :x", Dialect::Postgres), ["x"]);
        // One statement writes its parameters one way.
        let e = lex("SELECT {a:UInt8}, :b::UInt8", Dialect::Clickhouse).unwrap_err();
        assert_eq!(e.message, ":b and {a:UInt8} are written two ways; write every parameter of a statement one way, as {name:Type} or as :name::Type");
        assert_eq!(lex("SELECT $1", Dialect::Clickhouse).unwrap_err().message, "$1 is not a placeholder on ClickHouse, which writes a parameter with its type; write {p1:Type}");
        assert_eq!(lex("SELECT 'open", Dialect::Clickhouse).unwrap_err().message, "unterminated string literal");
    }

    #[test]
    fn sqlite_named_prefixes() {
        // SQLite's `@name` and `$name` are named placeholders there, dotted like `:name`, and the
        // name drops the prefix.
        for sql in ["select @a.b, @c, @a.b", "select $a.b, $c, $a.b"] {
            let l = lex(sql, Dialect::Sqlite).unwrap();
            assert_eq!(l.params, ["a.b", "c"]);
            assert_eq!(l.style, Style::Named);
            assert_eq!(rewrite(sql, &l, Target::Numbered).sql, "select ?1, ?2, ?1");
        }
        // SQLite reads `:a` and `@a` as two parameters, so one statement keeps one prefix.
        let e = lex("select :a, @b where x = $a", Dialect::Sqlite).unwrap_err();
        assert_eq!(e.message, "@b and :a have different prefixes; write every named parameter of a statement with one prefix, here :b");
        assert_eq!(e.offset, 11);
        assert_eq!(lex("select @a, :a", Dialect::Sqlite).unwrap_err().offset, 11);
        // Named and positional still do not mix, and `a$b` and `a@b` are not placeholders.
        assert_eq!(lex("select @a, ?", Dialect::Sqlite).unwrap().style, Style::Mixed);
        assert!(names("select a$b, a@b from t", Dialect::Sqlite).is_empty());
        // Elsewhere the forms keep their meaning: Postgres `@` is an operator, MySQL `@x` a variable.
        assert!(names("select @a from t", Dialect::Postgres).is_empty());
        assert!(names("select @a, $b from t", Dialect::Mysql).is_empty());
        // `Named::Colon` takes `:name` alone.
        let colon = |sql: &str| lex_with(sql, Dialect::Sqlite, Named::Colon).unwrap_err().message;
        assert_eq!(colon("select @a.b from t"), "SQLite reads @a.b as a parameter that Reylo does not bind; write :a.b");
        assert_eq!(colon("select :x, $y"), "SQLite reads $y as a parameter that Reylo does not bind; write :y");
    }

    #[test]
    fn rewrites_and_maps_back() {
        let sql = "select * from t where id = :id and name = :name and id = :id";
        let l = lex(sql, Dialect::Postgres).unwrap();
        let r = rewrite(sql, &l, Target::Dollar);
        assert_eq!(r.sql, "select * from t where id = $1 and name = $2 and id = $1");
        assert_eq!(r.order, ["id", "name"]);
        // "name" starts at the same place in both; the third placeholder shifted.
        let third = r.sql.rfind("$1").unwrap();
        assert_eq!(r.to_original(third), sql.rfind(":id").unwrap());
        assert_eq!(r.to_original(r.sql.len()), sql.len());
        let q = rewrite(sql, &l, Target::Question);
        assert_eq!(q.sql, "select * from t where id = ? and name = ? and id = ?");
        assert_eq!(q.order, ["id", "name", "id"]);
    }

    #[test]
    fn each_parameter_once() {
        let at = |sql: &str, d: Dialect| repeated(sql, &lex(sql, d).unwrap()).into_iter().map(|r| (r.start, r.end)).collect::<Vec<_>>();
        // Every use after the first is an error at its place, in every form a dialect names.
        let sql = "select id, name from users where name like :q or email like :q";
        let r = repeated(sql, &lex(sql, Dialect::Postgres).unwrap());
        assert_eq!(r.len(), 1);
        assert_eq!(&sql[r[0].start..r[0].end], ":q");
        assert_eq!(r[0].start, sql.rfind(":q").unwrap());
        assert_eq!(
            r[0].message,
            "The parameter :q appears more than once; use each parameter once, and select it in a with to read it twice, cast to its type, as in: with input as (select :q::<type> as q) select … from input"
        );
        assert_eq!(at("select :a, :b, :a, :a, :b", Dialect::Postgres), [(15, 17), (19, 21), (23, 25)]);
        assert_eq!(at("select $1, $2, $1", Dialect::Postgres), [(15, 17)]);
        assert_eq!(at("select ?3, ?3", Dialect::Sqlite), [(11, 13)]);
        assert_eq!(at("select @a, @a", Dialect::Sqlite), [(11, 13)]);
        assert_eq!(at("select $a, $a", Dialect::Sqlite), [(11, 13)]);
        assert_eq!(at("select :a, :a", Dialect::Mysql), [(11, 13)]);
        // A bare `?` takes 1 here, so `?1` names the same parameter again.
        assert_eq!(at("select ?, ?1", Dialect::Sqlite), [(10, 12)]);
        // A dotted global is one name; the example selects it by its last segment, and a
        // position by `p` and its number.
        let sql = "select * from t where owner = :auth.user_id or author = :auth.user_id";
        assert!(repeated(sql, &lex(sql, Dialect::Postgres).unwrap())[0].message.contains("with input as (select :auth.user_id::<type> as user_id)"));
        let sql = "select $1::int + $1";
        assert!(repeated(sql, &lex(sql, Dialect::Postgres).unwrap())[0].message.starts_with("The parameter $1 appears more than once;"));
        assert!(repeated(sql, &lex(sql, Dialect::Postgres).unwrap())[0].message.contains("(select $1::<type> as p1)"));
        // A bare `?` never repeats; distinct names, text and casts are not uses.
        assert!(at("select ?, ?, ?", Dialect::Mysql).is_empty());
        assert!(at("select ?, ?", Dialect::Sqlite).is_empty());
        assert!(at("select :a, :b, ':a', x::a -- :a", Dialect::Postgres).is_empty());
        assert!(at("with input as (select :q::text as q) select 1 from t, input where a = input.q or b = input.q", Dialect::Postgres).is_empty());
    }

    #[test]
    fn errors() {
        assert!(lex("select 'abc", Dialect::Postgres).is_err());
        assert!(lex("select $x$abc", Dialect::Postgres).is_err());
        assert!(lex("select /* a", Dialect::Postgres).is_err());
        assert_eq!(lex("select 'abc", Dialect::Postgres).unwrap_err().to_string(), "unterminated string literal (at byte 7)");
        // Each error points at what is left open, the quote or the comment's start, which lies
        // inside the text: an offset at its end placed the error past the end of the file.
        let at = |sql: &str, dialect| lex(sql, dialect).unwrap_err().offset;
        assert_eq!(at("select 1, 'abc", Dialect::Sqlite), 10);
        assert_eq!(at("select 1, \"abc", Dialect::Postgres), 10);
        assert_eq!(at("select 1, `abc\n", Dialect::Sqlite), 10);
        assert_eq!(at("select 1, `abc", Dialect::Mysql), 10);
        assert_eq!(at("select 1, E'abc", Dialect::Postgres), 10);
        assert_eq!(at("select 1, $x$abc", Dialect::Postgres), 10);
        assert_eq!(at("select 1, /* a /* b */", Dialect::Postgres), 10);
        assert_eq!(at("select 1, /* a", Dialect::Mysql), 10);
    }

    #[test]
    fn dotted_names() {
        let sql = "select :a.b";
        let l = lex(sql, Dialect::Postgres).unwrap();
        assert_eq!(l.params, ["a.b"]);
        assert_eq!(l.placeholders, [Placeholder { name: "a.b".into(), start: 7, end: 11 }]);
        assert_eq!(l.style, Style::Named);
        assert_eq!(rewrite(sql, &l, Target::Dollar).sql, "select $1");
        assert_eq!(rewrite(sql, &l, Target::Numbered).sql, "select ?1");
        assert_eq!(rewrite(sql, &l, Target::Question).sql, "select ?");
        // Every segment starts like a name; the named forms agree; a cast after one is a cast.
        assert_eq!(names("select :auth.user_id, :org.name::text, :t.*, :x.1", Dialect::Postgres), ["auth.user_id", "org.name", "t", "x"]);
        // A dotted name survives the SQLite prepare only through the rewrite, and maps back.
        let sql = "select :a.b, :c";
        let l = lex(sql, Dialect::Sqlite).unwrap();
        let r = rewrite(sql, &l, Target::Numbered);
        assert_eq!(r.sql, "select ?1, ?2");
        assert_eq!(r.to_original(r.sql.find("?2").unwrap()), sql.find(":c").unwrap());
        assert_eq!(r.to_original(r.sql.len()), sql.len());
    }

    #[test]
    fn numbered_question_is_a_position() {
        let l = lex("select ?3, ?3", Dialect::Sqlite).unwrap();
        assert_eq!(l.params, ["3"]);
        assert_eq!(l.placeholders.iter().map(|p| (p.start, p.end)).collect::<Vec<_>>(), [(7, 9), (11, 13)]);
        assert_eq!(l.style, Style::Positional);
        assert_eq!(lex("select ?3, :x", Dialect::Sqlite).unwrap().style, Style::Mixed);
        // On Postgres `?` is still the operator, digits or not.
        assert!(lex("select j ? 'k', 1 ?2", Dialect::Postgres).unwrap().params.is_empty());
    }

    #[test]
    fn array_slices_are_not_placeholders() {
        for d in [Dialect::Postgres, Dialect::Sqlite, Dialect::Mysql] {
            assert!(lex("select arr[1:2], arr[2:], arr[:3], arr[1:2].x, arr[lo:hi], arr[1:n] from t", d).unwrap().params.is_empty());
        }
        // On Postgres a colon that opens a subscript is a slice with no lower bound, whatever
        // follows it: no expression starts with a colon there. Whitespace and comments may stand
        // between, and the subscript may follow a quoted name, a parenthesis, another subscript or
        // a placeholder.
        let slices = "select arr[:hi], arr[ :hi], arr[/* c */:hi], arr[:t.n], \"a\"[:hi], (f(x))[:pg_t.tgnargs], m[1][:hi], arr[1:hi], arr[:hi][:lo] from t where x = :x and y = $1";
        let l = lex(slices, Dialect::Postgres).unwrap();
        assert_eq!(l.params, ["x", "1"]);
        assert_eq!(l.style, Style::Mixed);
        assert_eq!(names("select :a[:hi], arr[:hi] from t", Dialect::Postgres), ["a"]);
        // A parameter is the subscript, or a slice's bound after its colon, as written.
        assert_eq!(names("select arr[(:i)], arr[ : :hi], arr[1 : :b], arr[:i + 0] from t", Dialect::Postgres), ["i", "hi", "b"]);
        // An array constructor's bracket follows `array` or another bracket, and holds parameters.
        assert_eq!(names("select array[:a, :b], ARRAY[[:c], [:d]], x = any(array[:e]) from t", Dialect::Postgres), ["a", "b", "c", "d", "e"]);
        // The statement the runtime's lexer is checked against reads the same here.
        let l = lex("select tags[:n], tags[ :m], (f(x))[:pg_t.tgnargs], \"q\"[:k], array[:a, :b], tags[(:c)], tags[1::int:d] from t where y = :y", Dialect::Postgres).unwrap();
        assert_eq!(l.params, ["a", "b", "c", "y"]);
        assert_eq!(l.placeholders.iter().map(|p| (p.start, p.end)).collect::<Vec<_>>(), [(66, 68), (70, 72), (81, 83), (119, 121)]);
        // MySQL keeps reading a colon after no name character as a parameter. SQLite reads `[:lo]`
        // as a quoted name, the alias of `arr`.
        assert_eq!(names("select arr[:lo] from t", Dialect::Mysql), ["lo"]);
        assert!(names("select arr[:lo] from t", Dialect::Sqlite).is_empty());
    }

    #[test]
    fn colons_that_open_nothing() {
        // `:{` and MySQL's `:=` open nothing; a quote doubled inside a string is one quote.
        assert!(names("select '{}'::jsonb, :{a}", Dialect::Postgres).is_empty());
        assert!(names("select @x := 1", Dialect::Mysql).is_empty());
        assert_eq!(names("select 'it''s :not', :yes", Dialect::Postgres), ["yes"]);
        // An upper-case `E''` string and a tagged dollar quote hide what is inside them.
        assert_eq!(names("select E'\\':nope', $tag$ :nope $tag$, :real", Dialect::Postgres), ["real"]);
    }

    #[test]
    fn postgres_names_past_ascii() {
        // Postgres reads every byte past ASCII as a letter, in a name and in a dollar quote's tag,
        // and a `$` past a name's first character as the name's, so nothing opens inside a name.
        for sql in ["select $本文$it's$本文$ as s", "select $本文$ $1 :x $本文$ as s", "select 1 as é$1", "select 1 as a$t$", "select 1 as é$$9", "select 1::a$t$ as d"] {
            assert_eq!(lex(sql, Dialect::Postgres).map(|l| l.params), Ok(Vec::new()), "{sql}");
        }
        // A colon after such a name is a slice's, as after any other.
        assert_eq!(names("select x[café:fin], café[:n] from t where id = :id", Dialect::Postgres), ["id"]);
        // A parameter's name is ASCII still, and SQLite ends a name before a colon.
        assert!(names("select :été", Dialect::Postgres).is_empty());
        assert_eq!(names("select é:a", Dialect::Sqlite), ["a"]);
    }

    #[test]
    fn each_dialect_reads_its_own_text() {
        // A vertical tab is whitespace to Postgres, so `tags\v[:n]` is a slice, as with a space.
        assert!(names("select tags\u{b}[:n] as s from (select array[1,2,3] as tags, 2 as n) t", Dialect::Postgres).is_empty());
        assert_eq!(names("select x\u{b}:y", Dialect::Postgres), ["y"]);
        // A no-break space is a character of a Postgres name, and whitespace to ClickHouse.
        assert!(names("select x\u{a0}[:n] from t", Dialect::Postgres).is_empty());
        assert_eq!(names("SELECT\u{a0}{a:UInt8},\u{3000}{\u{a0}b\u{a0}:\u{a0}UInt8}", Dialect::Clickhouse), ["a", "b"]);
        // SQLite reads `$` and every byte past ASCII as a name's, so nothing opens inside one.
        for sql in ["select 1 as a$$b", "select a$$b from (select 7 as \"a$$b\")", "select 1 as é$a", "select 1 as a$b$c"] {
            assert!(names(sql, Dialect::Sqlite).is_empty(), "{sql}");
        }
        // SQLite has no `E''` string: `e'\'` is a name and a string there, as on MySQL.
        assert_eq!(names(r"select e'\', :x as x, '\' from (select 1 as e)", Dialect::Sqlite), ["x"]);
        assert_eq!(lex(r"select e'\', :x as x, '\'", Dialect::Postgres).unwrap_err().message, "unterminated string literal");
        // A placeholder's name ends where the database ends the name, or the statement is refused.
        let refused = |sql: &str, d: Dialect| lex(sql, d).unwrap_err().message;
        assert_eq!(refused("select :a$b", Dialect::Sqlite), "SQLite reads :a$b as one parameter, and a placeholder's name is ASCII letters, digits and _ alone; rename it");
        assert_eq!(refused("select $a$b", Dialect::Sqlite), "SQLite reads $a$b as one parameter, and a placeholder's name is ASCII letters, digits and _ alone; rename it");
        assert_eq!(refused("select :a.bé", Dialect::Postgres), "Postgres reads a.bé as one name, and a placeholder's name is ASCII letters, digits and _ alone; rename the parameter");
        assert_eq!(refused("select :a$b", Dialect::Mysql), "MySQL reads a$b as one name, and a placeholder's name is ASCII letters, digits and _ alone; rename the parameter");
        // SQLite reads a vertical tab inside a run of whitespace another space starts.
        assert_eq!(names("select 1 \u{b}+ :x", Dialect::Sqlite), ["x"]);
        assert_eq!(lex("select :a$b", Dialect::Mysql).unwrap_err().offset, 7);
        // One that starts past ASCII is no placeholder here, and SQLite's engine refuses the
        // parameter SQLite reads there, at its place.
        assert!(names("select :été, @é, $é", Dialect::Sqlite).is_empty());
        assert_eq!(names("select :a::text, :b.c, :d", Dialect::Postgres), ["a", "b.c", "d"]);
    }

    #[test]
    fn the_corpus() {
        let corpus = crate::lexical::Corpus::load();
        for dialect in [Dialect::Postgres, Dialect::Sqlite, Dialect::Mysql, Dialect::Clickhouse] {
            for (case, reading) in corpus.on(dialect) {
                let at = format!("{dialect:?}, {}: {:?}", case.about, case.text);
                let lexed = match (lex(&case.text, dialect), &reading.error) {
                    (Err(e), Some(error)) => {
                        assert_eq!(&e.message, error, "{at}");
                        continue;
                    }
                    (Ok(lexed), None) => lexed,
                    (found, _) => panic!("{at}: {found:?}"),
                };
                let texts = |comments: bool| lexed.opaque.iter().map(|&(s, e)| &case.text[s..e]).filter(|t| is_comment(t) == comments).collect::<Vec<_>>();
                assert_eq!(texts(true), reading.comments, "{at}");
                assert_eq!(texts(false), reading.strings, "{at}");
                if let Some(params) = &reading.params {
                    assert_eq!(&lexed.params, params, "{at}");
                }
            }
        }
    }

    #[test]
    fn opaque_spans() {
        let sql = "select 'a''b', \"q\"\"r\", $t$ :x $t$, e'\\'' -- c :y\n/* d /* :z */ */ :w";
        let l = lex(sql, Dialect::Postgres).unwrap();
        assert_eq!(l.params, ["w"]);
        assert_eq!(l.opaque, [(7, 13), (15, 21), (23, 33), (35, 40), (41, 48), (49, 65)]);
        let text: Vec<&str> = l.opaque.iter().map(|&(s, e)| &sql[s..e]).collect();
        assert_eq!(text, ["'a''b'", "\"q\"\"r\"", "$t$ :x $t$", "e'\\''", "-- c :y", "/* d /* :z */ */"]);
        // A `-- @` line is a comment like any other; code between the spans is not listed.
        let l = lex("-- @returns one\nselect `a`, 1", Dialect::Mysql).unwrap();
        assert_eq!(l.opaque, [(0, 15), (23, 26)]);
    }

    #[test]
    fn numbered_target() {
        let sql = "select :a, :b, :a";
        let l = lex(sql, Dialect::Sqlite).unwrap();
        let r = rewrite(sql, &l, Target::Numbered);
        assert_eq!(r.sql, "select ?1, ?2, ?1");
        assert_eq!(r.order, ["a", "b"]);
        assert_eq!(r.spans, [(7, 9, 7, 9), (11, 13, 11, 13), (15, 17, 15, 17)]);
        // Positional parameters bind by number, whatever order they appear in: `?n` binds
        // `params[n - 1]`, so an author's numbering survives.
        let sql = "select ?2, ?1, ?2";
        let l = lex(sql, Dialect::Sqlite).unwrap();
        let r = rewrite(sql, &l, Target::Numbered);
        assert_eq!(r.sql, "select ?2, ?1, ?2");
        assert_eq!(r.order, ["1", "2"]);
    }

    #[test]
    fn positional_names_are_numbers() {
        // A bare `?` takes the number after the largest one so far, as SQLite numbers it.
        assert_eq!(names("select ?1, ?2, ?", Dialect::Sqlite), ["1", "2", "3"]);
        assert_eq!(names("select ?5, ?, ?2", Dialect::Sqlite), ["2", "5", "6"]);
        assert_eq!(names("select $01, $1", Dialect::Postgres), ["1"]);
        assert!(is_positional("12") && !is_positional("a1") && !is_positional(""));
        // `$2 ... $1` binds `$1` first, and the rewrite keeps the author's numbers.
        let sql = "select $2::int, $1::text";
        let l = lex(sql, Dialect::Postgres).unwrap();
        assert_eq!(l.params, ["1", "2"]);
        let r = rewrite(sql, &l, Target::Dollar);
        assert_eq!(r.sql, sql);
        assert_eq!(r.order, ["1", "2"]);
        // MySQL binds one value per `?`, in text order.
        let sql = "select ?, ?";
        let l = lex(sql, Dialect::Mysql).unwrap();
        assert_eq!(rewrite(sql, &l, Target::Question).order, ["1", "2"]);
    }

    #[test]
    fn mysql_text() {
        // By default a backslash escapes the character after it, in either quote, and a
        // double-quoted word is a string; a placeholder inside one is text.
        let sql = r#"select 'it\'s :not', 'a\\' as b, :x, "say \":no", 'a\nb' from t where id = :id"#;
        assert_eq!(names(sql, Dialect::Mysql), ["x", "id"]);
        // Postgres and SQLite read a backslash as a character, so there `'a\'` ends at its quote.
        assert_eq!(names(r"select 'C:\', :x", Dialect::Postgres), ["x"]);
        assert_eq!(names(r"select 'C:\', :x", Dialect::Sqlite), ["x"]);
        assert_eq!(lex(r"select 'C:\', :x", Dialect::Mysql).unwrap_err().message, "unterminated string literal");
        // NO_BACKSLASH_ESCAPES makes the backslash a character there too.
        let plain = MysqlText { backslash_escapes: false, ansi_quotes: false };
        assert_eq!(lex_mysql(r"select 'C:\', :x", Named::Dialect, plain).unwrap().params, ["x"]);
        assert_eq!(lex_mysql(r"select 'a\' as b, :x, 'it''s :not'", Named::Dialect, plain).unwrap().params, ["x"]);
        // Under ANSI_QUOTES a double-quoted word is a name, which escapes nothing.
        let ansi = MysqlText { backslash_escapes: true, ansi_quotes: true };
        assert_eq!(lex_mysql(r#"select "a\" from t where id = :id"#, Named::Dialect, ansi).unwrap().params, ["id"]);
        assert!(lex(r#"select "a\" from t where id = :id"#, Dialect::Mysql).is_err());
        // `#` starts a line comment; `--` does only before a space or a control character, or at the end.
        assert_eq!(names("select :a # :b\n, :c", Dialect::Mysql), ["a", "c"]);
        assert_eq!(names("select :a --:b", Dialect::Mysql), ["a", "b"]);
        assert_eq!(names("select :a -- :b\n, :c --\t:d\n, 1--", Dialect::Mysql), ["a", "c"]);
        assert_eq!(names("select :a --:b", Dialect::Postgres), ["a"]);
        assert_eq!(names("select 1 # :b", Dialect::Postgres), ["b"]);
        // A block comment ends at its first `*/`; backquotes quote a name and escape nothing.
        assert_eq!(names("select /* a /* b */ :x", Dialect::Mysql), ["x"]);
        assert_eq!(names(r"select `a\`, :x", Dialect::Mysql), ["x"]);
        // MySQL has no `E''` string: `e'\''` is an escaped quote by the default reading alone.
        assert_eq!(lex_mysql(r"select e'\', :x", Named::Dialect, plain).unwrap().params, ["x"]);
        // The spans are listed as the reading has them.
        let sql = "select 'a\\'b'':c', \"c\\\"\", `d`, :e # f\n/* g /* */";
        let l = lex(sql, Dialect::Mysql).unwrap();
        let text: Vec<&str> = l.opaque.iter().map(|&(s, e)| &sql[s..e]).collect();
        assert_eq!(text, ["'a\\'b'':c'", "\"c\\\"\"", "`d`", "# f", "/* g /* */"]);
        assert_eq!(l.params, ["e"]);
        // A comment that starts with `#` is a comment to the statement check too.
        assert_eq!(one_statement("select 1; # done", &lex("select 1; # done", Dialect::Mysql).unwrap(), Dialect::Mysql, &["select"]), Ok(Statement::Query));
        assert_eq!(MysqlText::of_mode("STRICT_TRANS_TABLES,NO_BACKSLASH_ESCAPES"), plain);
        assert_eq!(MysqlText::of_mode("REAL_AS_FLOAT,PIPES_AS_CONCAT,ANSI_QUOTES,IGNORE_SPACE,ONLY_FULL_GROUP_BY,ANSI"), ansi);
        assert_eq!(MysqlText::of_mode(""), MysqlText::default());
    }

    #[test]
    fn one_statement_of_any_kind() {
        const SQLITE: &[&str] = &["select", "with", "values", "insert", "replace", "update", "delete"];
        let check = |sql: &str| one_statement(sql, &lex(sql, Dialect::Sqlite).unwrap(), Dialect::Sqlite, SQLITE);
        assert_eq!(check("select 1"), Ok(Statement::Query));
        assert_eq!(check("-- the user\n/* by id */ SELECT id from users where id = :id;"), Ok(Statement::Query));
        assert_eq!(check("select 1;\n-- a trailing comment\n;\n"), Ok(Statement::Query));
        assert_eq!(check("select ';' as a"), Ok(Statement::Query));
        assert_eq!(check("with x as (select 1) delete from t where id in (select * from x)"), Ok(Statement::Query));
        // A statement of another kind is a command, named by its first keyword.
        assert_eq!(check("create table t (id integer primary key)"), Ok(Statement::Command("create".into())));
        assert_eq!(check("/* the schema */ PRAGMA table_info(t);"), Ok(Statement::Command("pragma".into())));
        // A migration or a schema file of several statements: the error says what a typed file
        // holds, and how to leave such a file out of the type check.
        let several = "A typed .sql file holds one statement and this one holds several, so if it is a migration or a schema file, leave it out of the type check with \"exclude\" in tsconfig.json, as in \"exclude\": [\"db/**\"].";
        assert_eq!(check("select 1; select 2").unwrap_err(), several);
        assert_eq!(check("create table t (id integer primary key);\ncreate index i on t (id);").unwrap_err(), several);
        // A misspelt keyword is a syntax error the database reports, not a statement of another kind.
        assert_eq!(check("selec id from users"), Ok(Statement::Query));
        assert_eq!(refused_command("table t already exists"), "table t already exists, so if it is a migration or a schema file, leave it out of the type check with \"exclude\" in tsconfig.json, as in \"exclude\": [\"db/**\"].");
    }
}

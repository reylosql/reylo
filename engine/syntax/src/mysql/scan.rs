//! MySQL's tokens, read as TiDB's scanner reads them (`pkg/parser/lexer.go`), for TiDB's grammar:
//! a word is a keyword by TiDB's keyword table, a builtin function's name only before `(`, and a
//! few phrases are one token. Comments, and the markers of a `/*! … */` comment whose text the
//! server runs, are trivia. Reylo's `:name` placeholder is a parameter marker, as `?` is.

use super::generated::{tok, CHAR_TOKENS, FUNCTIONS, HINTED, KEYWORDS, NAMES_BEFORE_PAREN, WINDOW_FUNCTIONS};
use crate::lalr::Lexeme;

/// How the server reads a statement: the `sql_mode` flags that change its tokens, and its version,
/// which decides the `/*!NNNNN … */` comments it runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    /// A backslash escapes the character after it in a string (no `NO_BACKSLASH_ESCAPES`).
    pub backslash_escapes: bool,
    /// `"` quotes a name (`ANSI_QUOTES`).
    pub ansi_quotes: bool,
    /// `||` concatenates (`PIPES_AS_CONCAT`).
    pub pipes_as_concat: bool,
    /// NOT binds as tightly as `!` (`HIGH_NOT_PRECEDENCE`).
    pub high_not_precedence: bool,
    /// A builtin function's name may be followed by whitespace before its `(` (`IGNORE_SPACE`).
    pub ignore_space: bool,
    /// The server's version, as `80400` writes 8.4.0.
    pub version: u32,
}

impl Default for Options {
    fn default() -> Self {
        Options { backslash_escapes: true, ansi_quotes: false, pipes_as_concat: false, high_not_precedence: false, ignore_space: false, version: 80400 }
    }
}

/// Where the scanner stopped, and why.
#[derive(Debug, Clone)]
pub struct Stop {
    pub message: String,
    pub at: u32,
}

/// A token before classification: what kind of text it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Raw {
    Eof,
    Ident,
    Quoted,
    String,
    Int,
    Float,
    Dec,
    Hex,
    Bit,
    SingleAt,
    DoubleAt,
    National,
    Hint,
    Invalid,
    /// A token the grammar names directly.
    Sym(u16),
}

/// The character sets a `_name` introducer may name, as `SHOW CHARACTER SET` lists them.
const CHARSETS: [&str; 42] = ["armscii8", "ascii", "big5", "binary", "cp1250", "cp1251", "cp1256", "cp1257", "cp850", "cp852", "cp866", "cp932", "dec8", "eucjpms", "euckr", "filename", "gb18030", "gb2312", "gbk", "geostd8", "greek", "hebrew", "hp8", "keybcs2", "koi8r", "koi8u", "latin1", "latin2", "latin5", "latin7", "macce", "macroman", "sjis", "swe7", "tis620", "ucs2", "ujis", "utf16", "utf16le", "utf32", "utf8mb3", "utf8mb4"];

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

fn is_user_var_char(c: u8) -> bool {
    is_ident_char(c) || c == b'.'
}

fn char_sym(c: u8) -> u16 {
    match CHAR_TOKENS.get(c as usize) {
        Some(&s) if s != 0 => s,
        _ => tok::invalid,
    }
}

fn lookup(table: &[(&str, u16)], upper: &str) -> Option<u16> {
    table.binary_search_by(|e| e.0.cmp(upper)).ok().map(|i| table[i].1)
}

/// The scanner's place in the text, and what it remembers between tokens.
#[derive(Clone)]
struct Scanner<'t> {
    z: &'t [u8],
    at: usize,
    opts: Options,
    /// Inside `/*! … */`, whose closing `*/` is trivia.
    in_bang: bool,
    /// The last word read was followed by `.`, so a number after it is a name.
    identifier_dot: bool,
    /// The keywords of the last three tokens, 0 where a token was none.
    last_keyword: [u16; 3],
    /// Where the last token ended, and whether it was a word: a colon right after one is a label's.
    after_word: Option<usize>,
    /// The text's first unclosed comment, which ends the text.
    unclosed: Option<usize>,
    /// Where the `/*!` comment the scanner is inside opened.
    bang_at: usize,
}

impl<'t> Scanner<'t> {
    fn peek(&self) -> u8 {
        self.z.get(self.at).copied().unwrap_or(0)
    }

    fn eof(&self) -> bool {
        self.at >= self.z.len()
    }

    fn while_(&mut self, f: impl Fn(u8) -> bool) {
        while self.at < self.z.len() && f(self.z[self.at]) {
            self.at += 1;
        }
    }

    /// The next token's kind and span, past whitespace and comments.
    fn scan(&mut self) -> (Raw, usize, usize) {
        loop {
            self.while_(is_space);
            let start = self.at;
            if self.eof() {
                return (Raw::Eof, start, start);
            }
            let rest = &self.z[start..];
            let c = rest[0];
            match c {
                b'#' => {
                    self.while_(|c| c != b'\n');
                    continue;
                }
                b'-' if rest.starts_with(b"--") && (rest.len() == 2 || is_space(rest[2])) => {
                    self.while_(|c| c != b'\n');
                    continue;
                }
                b'-' if rest.starts_with(b"->>") => return self.take(Raw::Sym(tok::juss), 3),
                b'-' if rest.starts_with(b"->") => return self.take(Raw::Sym(tok::jss), 2),
                b'/' if rest.starts_with(b"/*") => match self.comment(start) {
                    Some(hint) => return (Raw::Hint, start, hint),
                    None => continue,
                },
                b'*' if self.in_bang && rest.starts_with(b"*/") => {
                    self.in_bang = false;
                    self.at += 2;
                    continue;
                }
                b'*' => {
                    self.identifier_dot = false;
                    return self.take(Raw::Sym(char_sym(b'*')), 1);
                }
                b'@' => return self.at_sign(start),
                b'`' => return self.quoted_ident(start),
                b'\'' | b'"' => return self.string(start),
                b'0'..=b'9' => return self.number(start),
                b'.' => {
                    self.at += 1;
                    if !self.identifier_dot && self.peek().is_ascii_digit() {
                        let (raw, s, e) = self.float(start);
                        return (if raw == Raw::Ident { Raw::Invalid } else { raw }, s, e);
                    }
                    return (Raw::Sym(char_sym(b'.')), start, self.at);
                }
                b'X' | b'x' | b'B' | b'b' if rest.get(1) == Some(&b'\'') => {
                    let hex = c.eq_ignore_ascii_case(&b'x');
                    self.at += 2;
                    if hex {
                        self.while_(|c| c.is_ascii_hexdigit());
                    } else {
                        self.while_(|c| c == b'0' || c == b'1');
                    }
                    if self.peek() == b'\'' {
                        self.at += 1;
                        return (if hex { Raw::Hex } else { Raw::Bit }, start, self.at);
                    }
                    return (Raw::Invalid, start, self.at);
                }
                b'N' | b'n' if rest.get(1) == Some(&b'\'') => {
                    self.at += 1;
                    return (Raw::National, start, self.at);
                }
                c if c.is_ascii_alphabetic() || c == b'_' || c == b'$' || c >= 0x80 => {
                    self.while_(is_ident_char);
                    return (Raw::Ident, start, self.at);
                }
                _ => {}
            }
            for (op, sym) in [(&b"<=>"[..], tok::nulleq), (b"||", tok::pipes), (b"&&", tok::andand), (b"&^", tok::andnot), (b":=", tok::assignmentEq), (b">=", tok::ge), (b"<=", tok::le), (b"!=", tok::neq), (b"<>", tok::neqSynonym), (b"<<", tok::lsh), (b">>", tok::rsh), (b"\\N", tok::null)] {
                if rest.starts_with(op) {
                    return self.take(Raw::Sym(sym), op.len());
                }
            }
            // `:name`, but not a label's colon right after its word (`outer:BEGIN`).
            if c == b':' && rest.get(1).is_some_and(|&n| n.is_ascii_alphabetic() || n == b'_' || n >= 0x80) && self.after_word != Some(start) {
                self.at += 1;
                self.while_(is_ident_char);
                // `:auth.user_id`, as the analysis reads a placeholder's name; `:t.*` stops before the dot.
                while self.z.get(self.at) == Some(&b'.') && self.z.get(self.at + 1).is_some_and(|&n| n.is_ascii_alphabetic() || n == b'_') {
                    self.at += 1;
                    self.while_(is_ident_char);
                }
                self.annotation();
                return (Raw::Sym(tok::paramMarker), start, self.at);
            }
            if c == b'?' {
                self.at += 1;
                self.annotation();
                return (Raw::Sym(tok::paramMarker), start, self.at);
            }
            let sym = match c {
                b'=' => tok::eq,
                c => char_sym(c),
            };
            return self.take(Raw::Sym(sym), 1);
        }
    }

    /// Reylo's annotation of a placeholder, `::nullable(int)`, `::rows(…)`, `::int` or `::double
    /// precision`, read into the placeholder's token: MySQL has no `::`. A parenthesis left open, to
    /// the end of the text or a `;`, is left to the grammar, which reports it.
    fn annotation(&mut self) {
        let z = self.z;
        let space = |mut i: usize| {
            while i < z.len() && is_space(z[i]) {
                i += 1;
            }
            i
        };
        let i = space(self.at);
        if !z[i..].starts_with(b"::") {
            return;
        }
        let word = space(i + 2);
        let mut end = word;
        while end < z.len() && is_ident_char(z[end]) {
            end += 1;
        }
        if end == word {
            return;
        }
        // The second word of a type SQL spells in two, as the analysis reads it.
        let second = space(end);
        let mut second_end = second;
        while second_end < z.len() && is_ident_char(z[second_end]) {
            second_end += 1;
        }
        let (first, next) = (z[word..end].to_ascii_lowercase(), z[second..second_end].to_ascii_lowercase());
        if matches!((&first[..], &next[..]), (b"double", b"precision") | (b"character", b"varying") | (_, b"unsigned")) {
            end = second_end;
        }
        self.at = end;
        let open = space(end);
        if z.get(open) != Some(&b'(') {
            return;
        }
        // To the parenthesis that closes it, past quoted text.
        let (mut depth, mut j) = (0usize, open);
        while j < z.len() {
            match z[j] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        self.at = j + 1;
                        return;
                    }
                }
                q @ (b'\'' | b'"' | b'`') => {
                    j += 1;
                    while j < z.len() && z[j] != q {
                        j += if z[j] == b'\\' && q != b'`' { 2 } else { 1 };
                    }
                }
                b';' => return,
                _ => {}
            }
            j += 1;
        }
    }

    fn take(&mut self, raw: Raw, n: usize) -> (Raw, usize, usize) {
        let start = self.at;
        self.at += n;
        (raw, start, self.at)
    }

    /// A comment at `start`: its end skipped, or, for an optimizer hint after a keyword that takes
    /// one, where the hint ends. A `/*!` comment the server runs leaves its text to be read.
    fn comment(&mut self, start: usize) -> Option<usize> {
        let z = self.z;
        if z[start..].starts_with(b"/*!") {
            let (digits, wanted) = comment_version(&z[start + 3..]);
            if wanted <= self.opts.version {
                self.at = start + 3 + digits;
                self.in_bang = true;
                self.bang_at = start;
                return None;
            }
            match skipped_comment(z, start + 3 + digits) {
                Some(end) => self.at = end,
                None => {
                    self.unclosed.get_or_insert(start);
                    self.at = z.len();
                }
            }
            return None;
        }
        let hint = z.get(start + 2) == Some(&b'+') && HINTED.contains(&self.last_keyword[0]) && (self.last_keyword[1] != tok::forKwd || self.last_keyword[2] == tok::binding);
        match z[start + 2..].windows(2).position(|w| w == b"*/") {
            Some(i) => {
                self.at = start + 2 + i + 2;
                hint.then_some(self.at)
            }
            None => {
                self.unclosed.get_or_insert(start);
                self.at = z.len();
                None
            }
        }
    }

    fn at_sign(&mut self, start: usize) -> (Raw, usize, usize) {
        self.at += 1;
        match self.peek() {
            b'@' => {
                self.at += 1;
                for prefix in [&b"global."[..], b"session.", b"local."] {
                    if self.z[self.at..].len() >= prefix.len() && self.z[self.at..self.at + prefix.len()].eq_ignore_ascii_case(prefix) {
                        self.at += prefix.len();
                        break;
                    }
                }
                match self.peek() {
                    b'\'' | b'"' => {
                        let (raw, _, end) = self.string(self.at);
                        (if raw == Raw::Invalid { Raw::Invalid } else { Raw::DoubleAt }, start, end)
                    }
                    b'`' => {
                        let (raw, _, end) = self.quoted_ident(self.at);
                        (if raw == Raw::Invalid { Raw::Invalid } else { Raw::DoubleAt }, start, end)
                    }
                    c if is_user_var_char(c) => {
                        self.while_(is_user_var_char);
                        (Raw::DoubleAt, start, self.at)
                    }
                    _ => (Raw::Sym(char_sym(b'@')), start, self.at),
                }
            }
            b'\'' | b'"' => {
                let (raw, _, end) = self.string(self.at);
                (if raw == Raw::Invalid { Raw::Invalid } else { Raw::SingleAt }, start, end)
            }
            b'`' => {
                let (raw, _, end) = self.quoted_ident(self.at);
                (if raw == Raw::Invalid { Raw::Invalid } else { Raw::SingleAt }, start, end)
            }
            c if is_user_var_char(c) => {
                self.while_(is_user_var_char);
                (Raw::SingleAt, start, self.at)
            }
            _ => (Raw::SingleAt, start, self.at),
        }
    }

    fn quoted_ident(&mut self, start: usize) -> (Raw, usize, usize) {
        self.at = start + 1;
        while !self.eof() {
            let c = self.z[self.at];
            self.at += 1;
            if c == b'`' {
                if self.peek() != b'`' {
                    return (Raw::Quoted, start, self.at);
                }
                self.at += 1;
            }
        }
        (Raw::Invalid, start, self.at)
    }

    fn string(&mut self, start: usize) -> (Raw, usize, usize) {
        let quote = self.z[start];
        self.at = start + 1;
        while !self.eof() {
            let c = self.z[self.at];
            self.at += 1;
            if c == quote {
                if self.peek() != quote {
                    return (Raw::String, start, self.at);
                }
                self.at += 1;
            } else if c == b'\\' && self.opts.backslash_escapes && !(quote == b'"' && self.opts.ansi_quotes) {
                if self.eof() {
                    break;
                }
                self.at += 1;
            }
        }
        (Raw::Invalid, start, self.at)
    }

    fn number(&mut self, start: usize) -> (Raw, usize, usize) {
        if self.identifier_dot {
            self.while_(is_ident_char);
            return (Raw::Ident, start, self.at);
        }
        let ident = |s: &mut Self| {
            s.while_(is_ident_char);
            (Raw::Ident, start, s.at)
        };
        let c0 = self.z[start];
        self.at = start + 1;
        let mut raw = Raw::Int;
        if c0 == b'0' {
            match self.peek() {
                b'0'..=b'7' => {
                    self.at += 1;
                    self.while_(|c| (b'0'..=b'7').contains(&c));
                }
                b'x' | b'X' => {
                    self.at += 1;
                    let p1 = self.at;
                    self.while_(|c| c.is_ascii_hexdigit());
                    if p1 == self.at || self.peek().is_ascii_digit() {
                        return ident(self);
                    }
                    raw = Raw::Hex;
                }
                b'b' => {
                    self.at += 1;
                    let p1 = self.at;
                    self.while_(|c| c == b'0' || c == b'1');
                    if p1 == self.at || self.peek().is_ascii_digit() {
                        return ident(self);
                    }
                    raw = Raw::Bit;
                }
                b'.' => return self.float(start),
                b'B' => return ident(self),
                _ => {}
            }
        }
        // Whatever the literal began as, digits, a fraction, an exponent or a name's characters may follow.
        self.while_(|c| c.is_ascii_digit());
        let c = self.peek();
        if c == b'.' || c == b'e' || c == b'E' {
            return self.float(start);
        }
        if !self.eof() && is_ident_char(c) {
            return ident(self);
        }
        (raw, start, self.at)
    }

    fn float(&mut self, start: usize) -> (Raw, usize, usize) {
        self.at = start;
        self.while_(|c| c.is_ascii_digit());
        if self.peek() == b'.' {
            self.at += 1;
            self.while_(|c| c.is_ascii_digit());
        }
        if matches!(self.peek(), b'e' | b'E') {
            self.at += 1;
            if matches!(self.peek(), b'-' | b'+') {
                self.at += 1;
            }
            if self.peek().is_ascii_digit() {
                self.while_(|c| c.is_ascii_digit());
                return (Raw::Float, start, self.at);
            }
            // `9e` is a name; `.1e`, which no name starts like, is one token no rule takes.
            let unfinished = self.at;
            self.at = start;
            self.while_(is_ident_char);
            if self.at == start {
                self.at = unfinished;
                return (Raw::Invalid, start, self.at);
            }
            return (Raw::Ident, start, self.at);
        }
        (Raw::Dec, start, self.at)
    }

    /// A word's symbol: a charset introducer, a keyword, or a name (`handleIdent` and
    /// `isTokenIdentifier`).
    fn word(&mut self, start: usize, end: usize) -> u16 {
        let text = &self.z[start..end];
        if let Some(name) = text.strip_prefix(b"_") {
            if CHARSETS.iter().any(|c| c.as_bytes().eq_ignore_ascii_case(name)) {
                return tok::underscoreCS;
            }
        }
        if self.z.get(end) == Some(&b'.') {
            return tok::identifier;
        }
        let mut i = start;
        while i > 0 && self.z[i - 1] == b' ' {
            i -= 1;
        }
        if i > 0 && self.z[i - 1] == b'.' {
            return tok::identifier;
        }
        let upper = String::from_utf8_lossy(text).to_ascii_uppercase();
        let mut next = end;
        if self.opts.ignore_space {
            while next < self.z.len() && is_space(self.z[next]) {
                next += 1;
            }
        }
        if self.z.get(next) == Some(&b'(') {
            if let Some(sym) = lookup(&FUNCTIONS, &upper) {
                return sym;
            }
            if NAMES_BEFORE_PAREN.binary_search(&upper.as_str()).is_ok() {
                return tok::identifier;
            }
        }
        lookup(&KEYWORDS, &upper).or_else(|| lookup(&WINDOW_FUNCTIONS, &upper)).unwrap_or(tok::identifier)
    }

    /// A token's symbol from its kind (`Lex` before its phrases).
    fn symbol(&mut self, raw: Raw, start: usize, end: usize) -> (u16, bool) {
        match raw {
            Raw::Eof => (0, false),
            Raw::Ident => {
                let sym = self.word(start, end);
                (sym, sym != tok::identifier && sym != tok::underscoreCS)
            }
            Raw::Quoted => (tok::identifier, false),
            Raw::String if self.opts.ansi_quotes && self.z[start] == b'"' => (tok::identifier, false),
            Raw::String => (tok::stringLit, false),
            Raw::Int => (tok::intLit, false),
            Raw::Float => (tok::floatLit, false),
            Raw::Dec => (tok::decLit, false),
            Raw::Hex => (tok::hexLit, false),
            Raw::Bit => (tok::bitLit, false),
            Raw::SingleAt => (tok::singleAtIdentifier, false),
            Raw::DoubleAt => (tok::doubleAtIdentifier, false),
            Raw::National => (tok::underscoreCS, false),
            Raw::Hint => (tok::hintComment, false),
            Raw::Invalid => (tok::invalid, false),
            Raw::Sym(s) => (s, false),
        }
    }

    /// The symbol of the token after this one, the scanner left where it was.
    fn peek_symbols(&self, n: usize) -> [u16; 2] {
        let mut ahead = self.clone();
        let mut out = [0; 2];
        for slot in out.iter_mut().take(n) {
            let (raw, s, e) = ahead.scan();
            *slot = ahead.symbol(raw, s, e).0;
        }
        out
    }

    /// The next token (`Lex`): its symbol and span, a phrase read as one token.
    fn lex(&mut self) -> (u16, usize, usize) {
        let (mut raw, start, mut end) = self.scan();
        // Every token but the end takes at least a character, so the scanner always moves on.
        if raw != Raw::Eof && end <= start {
            end = (start + 1..=self.z.len()).find(|&i| i == self.z.len() || (self.z[i] & 0xC0) != 0x80).unwrap_or(self.z.len());
            self.at = end;
            raw = Raw::Invalid;
        }
        self.after_word = matches!(raw, Raw::Ident | Raw::Quoted).then_some(end);
        self.last_keyword = [0, self.last_keyword[0], self.last_keyword[1]];
        let (mut sym, keyword) = self.symbol(raw, start, end);
        if keyword {
            self.last_keyword[0] = sym;
        }
        if raw == Raw::Ident || raw == Raw::Quoted {
            self.identifier_dot = self.peek() == b'.';
        }
        let mut join = |s: &mut Self, n: usize, phrase: u16| {
            for _ in 0..n {
                let (_, _, e) = s.scan();
                end = e;
            }
            s.last_keyword[0] = phrase;
            phrase
        };
        if sym == tok::full && self.peek_symbols(2) == [tok::outer, tok::join] {
            self.last_keyword[0] = tok::fullJoinType;
            return (tok::fullJoinType, start, end);
        }
        if sym == tok::pipes && !self.opts.pipes_as_concat {
            sym = tok::pipesAsOr;
        }
        if sym == tok::not && self.opts.high_not_precedence {
            sym = tok::not2;
        }
        if (sym == tok::r#as || sym == tok::member) && self.peek_symbols(1)[0] == tok::of {
            sym = join(self, 1, if sym == tok::r#as { tok::asof } else { tok::memberof });
        } else if sym == tok::to {
            match self.peek_symbols(2) {
                [tok::timestampType, tok::stringLit] => sym = join(self, 1, tok::toTimestamp),
                [tok::tsoType, tok::intLit] => sym = join(self, 1, tok::toTSO),
                _ => {}
            }
        } else if sym == tok::optionally && self.peek_symbols(2) == [tok::enclosed, tok::by] {
            sym = join(self, 2, tok::optionallyEnclosedBy);
        }
        (sym, start, end)
    }
}

/// Where the trivia from `from` stops being the close of a `/*! … */` comment: past each `*/`
/// that no `/*` in it opens, which closes one whose text the server runs.
pub(crate) fn conditional_close(text: &str, from: usize) -> usize {
    let z = text.as_bytes();
    let (mut i, mut end) = (from, from);
    while i < z.len() {
        match z[i] {
            c if is_space(c) => i += 1,
            b'#' => i = z[i..].iter().position(|&c| c == b'\n').map_or(z.len(), |n| i + n),
            b'-' if z[i..].starts_with(b"--") && z.get(i + 2).is_none_or(|&c| is_space(c)) => i = z[i..].iter().position(|&c| c == b'\n').map_or(z.len(), |n| i + n),
            b'/' if z[i..].starts_with(b"/*") && !z[i..].starts_with(b"/*!") => match z[i + 2..].windows(2).position(|w| w == b"*/") {
                Some(n) => i += 2 + n + 2,
                None => return end,
            },
            b'*' if z[i..].starts_with(b"*/") => {
                i += 2;
                end = i;
            }
            _ => return end,
        }
    }
    end
}

/// Where the `/*!` comment open at `start`, the trivia from `lead` on opened it, starts: `start`
/// where none does. A skipped `/*!99999 … */` comment closes in the trivia, and opens nothing.
pub(crate) fn conditional_open(text: &str, lead: usize, start: usize) -> usize {
    let z = text.as_bytes();
    let (mut i, mut open) = (lead, start);
    while i < start {
        match z[i] {
            c if is_space(c) => i += 1,
            b'#' => i = z[i..start].iter().position(|&c| c == b'\n').map_or(start, |n| i + n),
            b'-' if z[i..].starts_with(b"--") => i = z[i..start].iter().position(|&c| c == b'\n').map_or(start, |n| i + n),
            b'/' if z[i..].starts_with(b"/*") => match z[i + 2..start].windows(2).position(|w| w == b"*/") {
                Some(n) => i += 2 + n + 2,
                None => {
                    if z[i..].starts_with(b"/*!") {
                        open = i;
                    }
                    break;
                }
            },
            _ => break,
        }
    }
    open
}

/// The `sql_mode` a script's statements set, as the server runs them one after another: a session's
/// `SET sql_mode = …` outside a routine's body changes how the statements after it are read, where
/// its value is a mode list, `DEFAULT`, or a variable a statement before it set to a mode.
#[derive(Default)]
struct ModeSwitch {
    /// The statement's tokens so far, as `(token, its text)`.
    statement: Vec<(u16, std::ops::Range<usize>)>,
    /// Whether the statement creates a routine, a trigger or an event, whose body a BEGIN opens.
    routine: bool,
    /// How deep in `BEGIN … END` bodies, and in `CASE … END` expressions, the statement is.
    bodies: u32,
    cases: u32,
    /// An `END` whose next token says what it ends.
    end: bool,
    /// User variables set to a mode the reading knows: `SET @old = @@sql_mode`.
    saved: Vec<(String, Options)>,
}

impl ModeSwitch {
    fn token(&mut self, text: &str, sym: u16, range: std::ops::Range<usize>) {
        let word = text[range.clone()].to_ascii_uppercase();
        let before = self.statement.last().map(|(_, r)| text[r.clone()].to_ascii_uppercase());
        // A word after these is a name, as `AS begin` and `t.end` are, never a keyword that opens or closes a body.
        let named = matches!(before.as_deref(), Some("AS" | "," | "(" | "." | "="));
        if std::mem::take(&mut self.end) {
            match word.as_str() {
                "IF" | "LOOP" | "WHILE" | "REPEAT" => {}
                "CASE" => self.cases = self.cases.saturating_sub(1),
                _ => self.close(),
            }
        } else if word == "CASE" {
            self.cases += 1;
        }
        match word.as_str() {
            "PROCEDURE" | "FUNCTION" | "TRIGGER" | "EVENT" if self.statement.len() < 8 && self.statement.first().is_some_and(|(_, r)| text[r.clone()].eq_ignore_ascii_case("CREATE")) => self.routine = true,
            // A transaction's BEGIN starts a statement; a body's follows a label, a routine's head, or sits in a body.
            "BEGIN" if before.as_deref() == Some(":") || ((self.routine || self.bodies > 0) && !named) => self.bodies += 1,
            "END" if !named => self.end = true,
            _ => {}
        }
        self.statement.push((sym, range));
    }

    fn close(&mut self) {
        if self.cases > 0 {
            self.cases -= 1;
        } else {
            self.bodies = self.bodies.saturating_sub(1);
        }
    }

    /// The statement ended at a `;`: the mode the next is read under.
    fn finish(&mut self, text: &str, opts: Options) -> Options {
        if std::mem::take(&mut self.end) {
            self.close();
        }
        let statement = std::mem::take(&mut self.statement);
        if self.bodies > 0 {
            return opts;
        }
        self.routine = false;
        // Only a body holds a CASE statement, so no CASE stays open past a statement outside one.
        self.cases = 0;
        if !statement.first().is_some_and(|(_, r)| text[r.clone()].eq_ignore_ascii_case("SET")) {
            return opts;
        }
        let mut opts = opts;
        // A scope word sets the scope of its assignment and of those after it.
        let mut global = false;
        let (comma, open, close) = (char_sym(b','), char_sym(b'('), char_sym(b')'));
        let (mut depth, mut from) = (0i32, 1);
        for i in 1..=statement.len() {
            let sym = statement.get(i).map(|t| t.0);
            match sym {
                Some(s) if s == open => depth += 1,
                Some(s) if s == close => depth -= 1,
                _ => {}
            }
            if sym.is_none() || (sym == Some(comma) && depth == 0) {
                let words: Vec<&str> = statement[from..i].iter().map(|(_, r)| &text[r.clone()]).collect();
                opts = self.assignment(&words, &mut global, opts);
                from = i + 1;
            }
        }
        opts
    }

    /// One `name = value` of a SET.
    fn assignment(&mut self, words: &[&str], global: &mut bool, opts: Options) -> Options {
        let words = match words.first().map(|w| w.to_ascii_uppercase()).as_deref() {
            Some("SESSION" | "LOCAL") => {
                *global = false;
                &words[1..]
            }
            Some("GLOBAL" | "PERSIST" | "PERSIST_ONLY") => {
                *global = true;
                &words[1..]
            }
            _ => words,
        };
        // A user variable set to anything holds what it held no longer.
        if let Some(name) = words.first().filter(|w| w.starts_with('@') && !w.starts_with("@@")) {
            let key = name.to_ascii_lowercase();
            self.saved.retain(|(k, _)| *k != key);
            if let [_, "=" | ":=", value] = words {
                if let Some(mode) = self.mode_of(value, opts) {
                    self.saved.push((key, mode));
                }
            }
            return opts;
        }
        let [name, eq, value] = words else { return opts };
        if *eq != "=" && *eq != ":=" {
            return opts;
        }
        let known = self.mode_of(value, opts);
        let name = name.to_ascii_lowercase();
        let (scoped, bare) = match name.strip_prefix("@@") {
            Some(rest) => match rest.split_once('.') {
                Some(("session" | "local", bare)) => (false, bare.to_string()),
                Some(_) => (true, String::new()),
                None => (*global, rest.to_string()),
            },
            None => (*global, name.clone()),
        };
        if scoped || bare.trim_matches('`') != "sql_mode" {
            return opts;
        }
        known.unwrap_or(opts)
    }

    /// The mode `value` stands for, where the reading knows it: a list of modes, `DEFAULT`, the
    /// session's own, or a variable set to one.
    fn mode_of(&self, value: &str, opts: Options) -> Option<Options> {
        let lower = value.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("@@") {
            let rest = rest.strip_prefix("session.").or(rest.strip_prefix("local.")).unwrap_or(rest);
            return (rest.trim_matches('`') == "sql_mode").then_some(opts);
        }
        if lower.starts_with('@') {
            return self.saved.iter().find(|(k, _)| *k == lower).map(|(_, o)| Options { version: opts.version, ..*o });
        }
        let modes = match value.as_bytes().first() {
            Some(q @ (b'\'' | b'"')) if value.len() >= 2 && value.ends_with(*q as char) => &value[1..value.len() - 1],
            Some(c) if c.is_ascii_alphabetic() || *c == b'_' => value,
            _ => return None,
        };
        if modes.eq_ignore_ascii_case("DEFAULT") {
            return Some(Options { version: opts.version, ..Options::default() });
        }
        let mut next = Options { version: opts.version, backslash_escapes: true, ansi_quotes: false, pipes_as_concat: false, high_not_precedence: false, ignore_space: false };
        for mode in modes.split(',').map(|m| m.trim().to_ascii_uppercase()) {
            match mode.as_str() {
                "ANSI_QUOTES" => next.ansi_quotes = true,
                "NO_BACKSLASH_ESCAPES" => next.backslash_escapes = false,
                "PIPES_AS_CONCAT" => next.pipes_as_concat = true,
                "HIGH_NOT_PRECEDENCE" => next.high_not_precedence = true,
                "IGNORE_SPACE" => next.ignore_space = true,
                "ANSI" => {
                    next.ansi_quotes = true;
                    next.pipes_as_concat = true;
                    next.ignore_space = true;
                }
                _ => {}
            }
        }
        Some(next)
    }
}

/// The tokens of `text` for the grammar, where its trivia ends, and where a comment is left open.
pub fn lexemes(text: &str, opts: Options) -> (Vec<Lexeme>, u32, Option<Stop>) {
    let mut s = Scanner { z: text.as_bytes(), at: 0, opts, in_bang: false, identifier_dot: false, last_keyword: [0; 3], after_word: None, unclosed: None, bang_at: 0 };
    let mut out = Vec::with_capacity(text.len() / 4 + 1);
    let mut lead = 0usize;
    let mut modes = ModeSwitch::default();
    loop {
        let (sym, start, end) = s.lex();
        if sym == 0 {
            break;
        }
        out.push(Lexeme { sym, lead: lead as u32, start: start as u32, end: end as u32 });
        lead = end;
        if sym == char_sym(b';') {
            s.opts = modes.finish(text, s.opts);
        } else {
            modes.token(text, sym, start..end);
        }
    }
    // A `/*!` comment the text never closes is unfinished SQL.
    let unclosed = s.unclosed.or(s.in_bang.then_some(s.bang_at));
    let stop = unclosed.map(|at| Stop { message: super::syntax_message(text, at as u32), at: at as u32 });
    (out, lead as u32, stop)
}

/// The version a conditional comment opens with, and how many digits it takes: six where whitespace follows them, else the first five, and none with fewer.
fn comment_version(rest: &[u8]) -> (usize, u32) {
    let run = rest.iter().take_while(|c| c.is_ascii_digit()).count();
    let digits = if run == 6 && rest.get(6).copied().is_some_and(is_space) {
        6
    } else if run >= 5 {
        5
    } else {
        return (0, 0);
    };
    (digits, rest[..digits].iter().fold(0, |v, c| v * 10 + (c - b'0') as u32))
}

/// Where a conditional comment the server skips ends, past one comment nested in it.
fn skipped_comment(z: &[u8], from: usize) -> Option<usize> {
    let (mut i, mut depth) = (from, 1);
    while i + 1 < z.len() {
        match &z[i..i + 2] {
            b"/*" if depth == 1 => {
                depth = 2;
                i += 2;
            }
            b"*/" => {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => i += 1,
        }
    }
    None
}

// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.
//
// Changed for reylo-sql from sqlparser-rs 0.63.0: what only its parser, its other dialects and
// its optional features used is left out, and MySQL's statements print as MySQL reads them.

//! SQL Tokenizer
//!
//! The tokens a few of the AST's nodes keep, and where a node stands in the text: its span.

use core::{cmp, fmt};

use crate::mysql::ast::keywords::{Keyword, ALL_KEYWORDS, ALL_KEYWORDS_INDEX};
use crate::mysql::ast::{DollarQuotedString, QuoteDelimitedString};

/// SQL Token enumeration
#[derive(Debug, Clone, PartialEq, PartialOrd, Eq, Ord, Hash)]

pub enum Token {
    /// An end-of-file marker, not a real token
    EOF,
    /// A keyword (like SELECT) or an optionally quoted SQL identifier
    Word(Word),
    /// An unsigned numeric literal
    Number(String, bool),
    /// A character that could not be tokenized
    Char(char),
    /// Single quoted string: i.e: 'string'
    SingleQuotedString(String),
    /// Double quoted string: i.e: "string"
    DoubleQuotedString(String),
    /// Triple single quoted strings: Example '''abc'''
    /// [BigQuery](https://cloud.google.com/bigquery/docs/reference/standard-sql/lexical#quoted_literals)
    TripleSingleQuotedString(String),
    /// Triple double quoted strings: Example """abc"""
    /// [BigQuery](https://cloud.google.com/bigquery/docs/reference/standard-sql/lexical#quoted_literals)
    TripleDoubleQuotedString(String),
    /// Dollar quoted string: i.e: $$string$$ or $tag_name$string$tag_name$
    DollarQuotedString(DollarQuotedString),
    /// Byte string literal: i.e: b'string' or B'string' (note that some backends, such as
    /// PostgreSQL, may treat this syntax as a bit string literal instead, i.e: b'10010101')
    SingleQuotedByteStringLiteral(String),
    /// Byte string literal: i.e: b"string" or B"string"
    DoubleQuotedByteStringLiteral(String),
    /// Triple single quoted literal with byte string prefix. Example `B'''abc'''`
    /// [BigQuery](https://cloud.google.com/bigquery/docs/reference/standard-sql/lexical#quoted_literals)
    TripleSingleQuotedByteStringLiteral(String),
    /// Triple double quoted literal with byte string prefix. Example `B"""abc"""`
    /// [BigQuery](https://cloud.google.com/bigquery/docs/reference/standard-sql/lexical#quoted_literals)
    TripleDoubleQuotedByteStringLiteral(String),
    /// Single quoted literal with raw string prefix. Example `R'abc'`
    /// [BigQuery](https://cloud.google.com/bigquery/docs/reference/standard-sql/lexical#quoted_literals)
    SingleQuotedRawStringLiteral(String),
    /// Double quoted literal with raw string prefix. Example `R"abc"`
    /// [BigQuery](https://cloud.google.com/bigquery/docs/reference/standard-sql/lexical#quoted_literals)
    DoubleQuotedRawStringLiteral(String),
    /// Triple single quoted literal with raw string prefix. Example `R'''abc'''`
    /// [BigQuery](https://cloud.google.com/bigquery/docs/reference/standard-sql/lexical#quoted_literals)
    TripleSingleQuotedRawStringLiteral(String),
    /// Triple double quoted literal with raw string prefix. Example `R"""abc"""`
    /// [BigQuery](https://cloud.google.com/bigquery/docs/reference/standard-sql/lexical#quoted_literals)
    TripleDoubleQuotedRawStringLiteral(String),
    /// "National" string literal: i.e: N'string'
    NationalStringLiteral(String),
    /// Quote delimited literal. Examples `Q'{ab'c}'`, `Q'|ab'c|'`, `Q'|ab|c|'`
    /// [Oracle](https://docs.oracle.com/en/database/oracle/oracle-database/21/sqlrf/Literals.html#GUID-1824CBAA-6E16-4921-B2A6-112FB02248DA)
    QuoteDelimitedStringLiteral(QuoteDelimitedString),
    /// "Nationa" quote delimited literal. Examples `NQ'{ab'c}'`, `NQ'|ab'c|'`, `NQ'|ab|c|'`
    /// [Oracle](https://docs.oracle.com/en/database/oracle/oracle-database/21/sqlrf/Literals.html#GUID-1824CBAA-6E16-4921-B2A6-112FB02248DA)
    NationalQuoteDelimitedStringLiteral(QuoteDelimitedString),
    /// "escaped" string literal, which are an extension to the SQL standard: i.e: e'first \n second' or E 'first \n second'
    EscapedStringLiteral(String),
    /// Unicode string literal: i.e: U&'first \000A second'
    UnicodeStringLiteral(String),
    /// Hexadecimal string literal: i.e.: X'deadbeef'
    HexStringLiteral(String),
    /// Comma
    Comma,
    /// Whitespace (space, tab, etc)
    Whitespace(Whitespace),
    /// Double equals sign `==`
    DoubleEq,
    /// Equality operator `=`
    Eq,
    /// Not Equals operator `<>` (or `!=` in some dialects)
    Neq,
    /// Less Than operator `<`
    Lt,
    /// Greater Than operator `>`
    Gt,
    /// Less Than Or Equals operator `<=`
    LtEq,
    /// Greater Than Or Equals operator `>=`
    GtEq,
    /// Spaceship operator <=>
    Spaceship,
    /// Plus operator `+`
    Plus,
    /// Minus operator `-`
    Minus,
    /// Multiplication operator `*`
    Mul,
    /// Division operator `/`
    Div,
    /// Integer division operator `//` in DuckDB
    DuckIntDiv,
    /// Modulo Operator `%`
    Mod,
    /// String concatenation `||`
    StringConcat,
    /// Left parenthesis `(`
    LParen,
    /// Right parenthesis `)`
    RParen,
    /// Period (used for compound identifiers or projections into nested types)
    Period,
    /// Colon `:`
    Colon,
    /// DoubleColon `::` (used for casting in PostgreSQL)
    DoubleColon,
    /// Assignment `:=` (used for keyword argument in DuckDB macros and some functions, and for variable declarations in DuckDB and Snowflake)
    Assignment,
    /// SemiColon `;` used as separator for COPY and payload
    SemiColon,
    /// Backslash `\` used in terminating the COPY payload with `\.`
    Backslash,
    /// Left bracket `[`
    LBracket,
    /// Right bracket `]`
    RBracket,
    /// Ampersand `&`
    Ampersand,
    /// Pipe `|`
    Pipe,
    /// Caret `^`
    Caret,
    /// Left brace `{`
    LBrace,
    /// Right brace `}`
    RBrace,
    /// Right Arrow `=>`
    RArrow,
    /// Sharp `#` used for PostgreSQL Bitwise XOR operator, also PostgreSQL/Redshift geometrical unary/binary operator (Number of points in path or polygon/Intersection)
    Sharp,
    /// `##` PostgreSQL/Redshift geometrical binary operator (Point of closest proximity)
    DoubleSharp,
    /// Tilde `~` used for PostgreSQL Bitwise NOT operator or case sensitive match regular expression operator
    Tilde,
    /// `~*` , a case insensitive match regular expression operator in PostgreSQL
    TildeAsterisk,
    /// `!~` , a case sensitive not match regular expression operator in PostgreSQL
    ExclamationMarkTilde,
    /// `!~*` , a case insensitive not match regular expression operator in PostgreSQL
    ExclamationMarkTildeAsterisk,
    /// `~~`, a case sensitive match pattern operator in PostgreSQL
    DoubleTilde,
    /// `~~*`, a case insensitive match pattern operator in PostgreSQL
    DoubleTildeAsterisk,
    /// `!~~`, a case sensitive not match pattern operator in PostgreSQL
    ExclamationMarkDoubleTilde,
    /// `!~~*`, a case insensitive not match pattern operator in PostgreSQL
    ExclamationMarkDoubleTildeAsterisk,
    /// `<<`, a bitwise shift left operator in PostgreSQL
    ShiftLeft,
    /// `>>`, a bitwise shift right operator in PostgreSQL
    ShiftRight,
    /// `&&`, an overlap operator in PostgreSQL
    Overlap,
    /// Exclamation Mark `!` used for PostgreSQL factorial operator
    ExclamationMark,
    /// Double Exclamation Mark `!!` used for PostgreSQL prefix factorial operator
    DoubleExclamationMark,
    /// AtSign `@` used for PostgreSQL abs operator, also PostgreSQL/Redshift geometrical unary/binary operator (Center, Contained or on)
    AtSign,
    /// `^@`, a "starts with" string operator in PostgreSQL
    CaretAt,
    /// `|/`, a square root math operator in PostgreSQL
    PGSquareRoot,
    /// `||/`, a cube root math operator in PostgreSQL
    PGCubeRoot,
    /// `?` or `$` , a prepared statement arg placeholder
    Placeholder(String),
    /// `->`, used as a operator to extract json field in PostgreSQL
    Arrow,
    /// `->>`, used as a operator to extract json field as text in PostgreSQL
    LongArrow,
    /// `#>`, extracts JSON sub-object at the specified path
    HashArrow,
    /// `@-@` PostgreSQL/Redshift geometrical unary operator (Length or circumference)
    AtDashAt,
    /// `?-` PostgreSQL/Redshift geometrical unary/binary operator (Is horizontal?/Are horizontally aligned?)
    QuestionMarkDash,
    /// `&<` PostgreSQL/Redshift geometrical binary operator (Overlaps to left?)
    AmpersandLeftAngleBracket,
    /// `&>` PostgreSQL/Redshift geometrical binary operator (Overlaps to right?)`
    AmpersandRightAngleBracket,
    /// `&<|` PostgreSQL/Redshift geometrical binary operator (Does not extend above?)`
    AmpersandLeftAngleBracketVerticalBar,
    /// `|&>` PostgreSQL/Redshift geometrical binary operator (Does not extend below?)`
    VerticalBarAmpersandRightAngleBracket,
    /// `<->` PostgreSQL/Redshift geometrical binary operator (Distance between)
    TwoWayArrow,
    /// `<^` PostgreSQL/Redshift geometrical binary operator (Is below?)
    LeftAngleBracketCaret,
    /// `>^` PostgreSQL/Redshift geometrical binary operator (Is above?)
    RightAngleBracketCaret,
    /// `?#` PostgreSQL/Redshift geometrical binary operator (Intersects or overlaps)
    QuestionMarkSharp,
    /// `?-|` PostgreSQL/Redshift geometrical binary operator (Is perpendicular?)
    QuestionMarkDashVerticalBar,
    /// `?||` PostgreSQL/Redshift geometrical binary operator (Are parallel?)
    QuestionMarkDoubleVerticalBar,
    /// `~=` PostgreSQL/Redshift geometrical binary operator (Same as)
    TildeEqual,
    /// `<<| PostgreSQL/Redshift geometrical binary operator (Is strictly below?)
    ShiftLeftVerticalBar,
    /// `|>> PostgreSQL/Redshift geometrical binary operator (Is strictly above?)
    VerticalBarShiftRight,
    /// `|> BigQuery pipe operator
    VerticalBarRightAngleBracket,
    /// `#>>`, extracts JSON sub-object at the specified path as text
    HashLongArrow,
    /// jsonb @> jsonb -> boolean: Test whether left json contains the right json
    AtArrow,
    /// jsonb <@ jsonb -> boolean: Test whether right json contains the left json
    ArrowAt,
    /// jsonb #- text[] -> jsonb: Deletes the field or array element at the specified
    /// path, where path elements can be either field keys or array indexes.
    HashMinus,
    /// jsonb @? jsonpath -> boolean: Does JSON path return any item for the specified
    /// JSON value?
    AtQuestion,
    /// jsonb @@ jsonpath → boolean: Returns the result of a JSON path predicate check
    /// for the specified JSON value. Only the first item of the result is taken into
    /// account. If the result is not Boolean, then NULL is returned.
    AtAt,
    /// jsonb ? text -> boolean: Checks whether the string exists as a top-level key within the
    /// jsonb object
    Question,
    /// jsonb ?& text[] -> boolean: Check whether all members of the text array exist as top-level
    /// keys within the jsonb object
    QuestionAnd,
    /// jsonb ?| text[] -> boolean: Check whether any member of the text array exists as top-level
    /// keys within the jsonb object
    QuestionPipe,
    /// Custom binary operator
    /// This is used to represent any custom binary operator that is not part of the SQL standard.
    /// PostgreSQL allows defining custom binary operators using CREATE OPERATOR.
    CustomBinaryOperator(String),
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Token::EOF => f.write_str("EOF"),
            Token::Word(ref w) => write!(f, "{w}"),
            Token::Number(ref n, l) => write!(f, "{}{long}", n, long = if *l { "L" } else { "" }),
            Token::Char(ref c) => write!(f, "{c}"),
            Token::SingleQuotedString(ref s) => write!(f, "'{s}'"),
            Token::TripleSingleQuotedString(ref s) => write!(f, "'''{s}'''"),
            Token::DoubleQuotedString(ref s) => write!(f, "\"{s}\""),
            Token::TripleDoubleQuotedString(ref s) => write!(f, "\"\"\"{s}\"\"\""),
            Token::DollarQuotedString(ref s) => write!(f, "{s}"),
            Token::NationalStringLiteral(ref s) => write!(f, "N'{s}'"),
            Token::QuoteDelimitedStringLiteral(ref s) => s.fmt(f),
            Token::NationalQuoteDelimitedStringLiteral(ref s) => write!(f, "N{s}"),
            Token::EscapedStringLiteral(ref s) => write!(f, "E'{s}'"),
            Token::UnicodeStringLiteral(ref s) => write!(f, "U&'{s}'"),
            Token::HexStringLiteral(ref s) => write!(f, "X'{s}'"),
            Token::SingleQuotedByteStringLiteral(ref s) => write!(f, "B'{s}'"),
            Token::TripleSingleQuotedByteStringLiteral(ref s) => write!(f, "B'''{s}'''"),
            Token::DoubleQuotedByteStringLiteral(ref s) => write!(f, "B\"{s}\""),
            Token::TripleDoubleQuotedByteStringLiteral(ref s) => write!(f, "B\"\"\"{s}\"\"\""),
            Token::SingleQuotedRawStringLiteral(ref s) => write!(f, "R'{s}'"),
            Token::DoubleQuotedRawStringLiteral(ref s) => write!(f, "R\"{s}\""),
            Token::TripleSingleQuotedRawStringLiteral(ref s) => write!(f, "R'''{s}'''"),
            Token::TripleDoubleQuotedRawStringLiteral(ref s) => write!(f, "R\"\"\"{s}\"\"\""),
            Token::Comma => f.write_str(","),
            Token::Whitespace(ws) => write!(f, "{ws}"),
            Token::DoubleEq => f.write_str("=="),
            Token::Spaceship => f.write_str("<=>"),
            Token::Eq => f.write_str("="),
            Token::Neq => f.write_str("<>"),
            Token::Lt => f.write_str("<"),
            Token::Gt => f.write_str(">"),
            Token::LtEq => f.write_str("<="),
            Token::GtEq => f.write_str(">="),
            Token::Plus => f.write_str("+"),
            Token::Minus => f.write_str("-"),
            Token::Mul => f.write_str("*"),
            Token::Div => f.write_str("/"),
            Token::DuckIntDiv => f.write_str("//"),
            Token::StringConcat => f.write_str("||"),
            Token::Mod => f.write_str("%"),
            Token::LParen => f.write_str("("),
            Token::RParen => f.write_str(")"),
            Token::Period => f.write_str("."),
            Token::Colon => f.write_str(":"),
            Token::DoubleColon => f.write_str("::"),
            Token::Assignment => f.write_str(":="),
            Token::SemiColon => f.write_str(";"),
            Token::Backslash => f.write_str("\\"),
            Token::LBracket => f.write_str("["),
            Token::RBracket => f.write_str("]"),
            Token::Ampersand => f.write_str("&"),
            Token::Caret => f.write_str("^"),
            Token::Pipe => f.write_str("|"),
            Token::LBrace => f.write_str("{"),
            Token::RBrace => f.write_str("}"),
            Token::RArrow => f.write_str("=>"),
            Token::Sharp => f.write_str("#"),
            Token::DoubleSharp => f.write_str("##"),
            Token::ExclamationMark => f.write_str("!"),
            Token::DoubleExclamationMark => f.write_str("!!"),
            Token::Tilde => f.write_str("~"),
            Token::TildeAsterisk => f.write_str("~*"),
            Token::ExclamationMarkTilde => f.write_str("!~"),
            Token::ExclamationMarkTildeAsterisk => f.write_str("!~*"),
            Token::DoubleTilde => f.write_str("~~"),
            Token::DoubleTildeAsterisk => f.write_str("~~*"),
            Token::ExclamationMarkDoubleTilde => f.write_str("!~~"),
            Token::ExclamationMarkDoubleTildeAsterisk => f.write_str("!~~*"),
            Token::AtSign => f.write_str("@"),
            Token::CaretAt => f.write_str("^@"),
            Token::ShiftLeft => f.write_str("<<"),
            Token::ShiftRight => f.write_str(">>"),
            Token::Overlap => f.write_str("&&"),
            Token::PGSquareRoot => f.write_str("|/"),
            Token::PGCubeRoot => f.write_str("||/"),
            Token::AtDashAt => f.write_str("@-@"),
            Token::QuestionMarkDash => f.write_str("?-"),
            Token::AmpersandLeftAngleBracket => f.write_str("&<"),
            Token::AmpersandRightAngleBracket => f.write_str("&>"),
            Token::AmpersandLeftAngleBracketVerticalBar => f.write_str("&<|"),
            Token::VerticalBarAmpersandRightAngleBracket => f.write_str("|&>"),
            Token::VerticalBarRightAngleBracket => f.write_str("|>"),
            Token::TwoWayArrow => f.write_str("<->"),
            Token::LeftAngleBracketCaret => f.write_str("<^"),
            Token::RightAngleBracketCaret => f.write_str(">^"),
            Token::QuestionMarkSharp => f.write_str("?#"),
            Token::QuestionMarkDashVerticalBar => f.write_str("?-|"),
            Token::QuestionMarkDoubleVerticalBar => f.write_str("?||"),
            Token::TildeEqual => f.write_str("~="),
            Token::ShiftLeftVerticalBar => f.write_str("<<|"),
            Token::VerticalBarShiftRight => f.write_str("|>>"),
            Token::Placeholder(ref s) => write!(f, "{s}"),
            Token::Arrow => write!(f, "->"),
            Token::LongArrow => write!(f, "->>"),
            Token::HashArrow => write!(f, "#>"),
            Token::HashLongArrow => write!(f, "#>>"),
            Token::AtArrow => write!(f, "@>"),
            Token::ArrowAt => write!(f, "<@"),
            Token::HashMinus => write!(f, "#-"),
            Token::AtQuestion => write!(f, "@?"),
            Token::AtAt => write!(f, "@@"),
            Token::Question => write!(f, "?"),
            Token::QuestionAnd => write!(f, "?&"),
            Token::QuestionPipe => write!(f, "?|"),
            Token::CustomBinaryOperator(s) => f.write_str(s),
        }
    }
}

impl Token {
    /// Create a `Token::Word` from an unquoted `keyword`.
    ///
    /// The lookup is case-insensitive; unknown values become `Keyword::NoKeyword`.
    pub fn make_keyword(keyword: &str) -> Self {
        Token::make_word(keyword, None)
    }

    /// Create a `Token::Word` from `word` with an optional `quote_style`.
    ///
    /// When `quote_style` is `None`, the parser attempts a case-insensitive keyword
    /// lookup and sets the `Word::keyword` accordingly.
    pub fn make_word(word: &str, quote_style: Option<char>) -> Self {
        Token::Word(Word {
            keyword: keyword_lookup(word, quote_style),
            value: word.to_string(),
            quote_style,
        })
    }
}

/// Case-insensitive keyword lookup using binary search over [`ALL_KEYWORDS`].
fn keyword_lookup(word: &str, quote_style: Option<char>) -> Keyword {
    if quote_style.is_some() {
        return Keyword::NoKeyword;
    }
    ALL_KEYWORDS
        .binary_search_by(|probe| {
            let probe = probe.as_bytes();
            let word = word.as_bytes();
            for (p, w) in probe.iter().zip(word.iter()) {
                let cmp = p.cmp(&w.to_ascii_uppercase());
                if cmp != core::cmp::Ordering::Equal {
                    return cmp;
                }
            }
            probe.len().cmp(&word.len())
        })
        .map_or(Keyword::NoKeyword, |x| ALL_KEYWORDS_INDEX[x])
}

/// A keyword (like SELECT) or an optionally quoted SQL identifier
#[derive(Debug, Clone, PartialEq, PartialOrd, Eq, Ord, Hash)]

pub struct Word {
    /// The value of the token, without the enclosing quotes, and with the
    /// escape sequences (if any) processed (TODO: escapes are not handled)
    pub value: String,
    /// An identifier can be "quoted" (&lt;delimited identifier> in ANSI parlance).
    /// The standard and most implementations allow using double quotes for this,
    /// but some implementations support other quoting styles as well (e.g. \[MS SQL])
    pub quote_style: Option<char>,
    /// If the word was not quoted and it matched one of the known keywords,
    /// this will have one of the values from dialect::keywords, otherwise empty
    pub keyword: Keyword,
}

impl fmt::Display for Word {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.quote_style {
            Some(s) if s == '"' || s == '[' || s == '`' => {
                write!(f, "{}{}{}", s, self.value, Word::matching_end_quote(s))
            }
            None => f.write_str(&self.value),
            _ => panic!("Unexpected quote_style!"),
        }
    }
}

impl Word {
    fn matching_end_quote(ch: char) -> char {
        match ch {
            '"' => '"', // ANSI and most dialects
            '[' => ']', // MS SQL
            '`' => '`', // MySQL
            _ => panic!("unexpected quoting style!"),
        }
    }
}

/// Represents whitespace in the input: spaces, newlines, tabs and comments.
#[derive(Debug, Clone, PartialEq, PartialOrd, Eq, Ord, Hash)]

pub enum Whitespace {
    /// A single space character.
    Space,
    /// A newline character.
    Newline,
    /// A tab character.
    Tab,
    /// A single-line comment (e.g. `-- comment` or `# comment`).
    /// The `comment` field contains the text, and `prefix` contains the comment prefix.
    SingleLineComment {
        /// The content of the comment (without the prefix).
        comment: String,
        /// The prefix used for the comment (for example `--` or `#`).
        prefix: String,
    },

    /// A multi-line comment (without the `/* ... */` delimiters).
    MultiLineComment(String),
}

impl fmt::Display for Whitespace {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Whitespace::Space => f.write_str(" "),
            Whitespace::Newline => f.write_str("\n"),
            Whitespace::Tab => f.write_str("\t"),
            Whitespace::SingleLineComment { prefix, comment } => writeln!(f, "{prefix}{comment}"),
            Whitespace::MultiLineComment(s) => write!(f, "/*{s}*/"),
        }
    }
}

/// Location in input string
///
/// # Create an "empty" (unknown) `Location`
/// ```ignore
/// # use sqlparser::tokenizer::Location;
/// let location = Location::empty();
/// ```
///
/// # Create a `Location` from a line and column
/// ```ignore
/// # use sqlparser::tokenizer::Location;
/// let location = Location::new(1, 1);
/// ```
///
/// # Create a `Location` from a pair
/// ```ignore
/// # use sqlparser::tokenizer::Location;
/// let location = Location::from((1, 1));
/// ```
#[derive(Eq, PartialEq, Hash, Clone, Copy, Ord, PartialOrd)]

pub struct Location {
    /// Line number, starting from 1.
    ///
    /// Note: Line 0 is used for empty spans
    pub line: u64,
    /// Line column, starting from 1.
    ///
    /// Note: Column 0 is used for empty spans
    pub column: u64,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.line == 0 {
            return Ok(());
        }
        write!(f, " at Line: {}, Column: {}", self.line, self.column)
    }
}

impl fmt::Debug for Location {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Location({},{})", self.line, self.column)
    }
}

impl Location {
    /// Return an "empty" / unknown location
    pub fn empty() -> Self {
        Self { line: 0, column: 0 }
    }

    /// Create a new `Location` for a given line and column
    pub fn new(line: u64, column: u64) -> Self {
        Self { line, column }
    }

    /// Create a new location for a given line and column
    ///
    /// Alias for [`Self::new`]
    // TODO: remove / deprecate in favor of` `new` for consistency?
    pub fn of(line: u64, column: u64) -> Self {
        Self::new(line, column)
    }

    /// Combine self and `end` into a new `Span`
    pub fn span_to(self, end: Self) -> Span {
        Span { start: self, end }
    }
}

impl From<(u64, u64)> for Location {
    fn from((line, column): (u64, u64)) -> Self {
        Self { line, column }
    }
}

/// A span represents a linear portion of the input string (start, end)
///
/// See [Spanned](crate::mysql::ast::Spanned) for more information.
#[derive(Eq, PartialEq, Hash, Clone, PartialOrd, Ord, Copy)]

pub struct Span {
    /// Start `Location` (inclusive).
    pub start: Location,
    /// End `Location` (inclusive).
    pub end: Location,
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Span({:?}..{:?})", self.start, self.end)
    }
}

impl Span {
    // An empty span (0, 0) -> (0, 0)
    // We need a const instance for pattern matching
    const EMPTY: Span = Self::empty();

    /// Create a new span from a start and end [`Location`]
    pub fn new(start: Location, end: Location) -> Span {
        Span { start, end }
    }

    /// Returns an empty span `(0, 0) -> (0, 0)`
    ///
    /// Empty spans represent no knowledge of source location
    /// See [Spanned](crate::mysql::ast::Spanned) for more information.
    pub const fn empty() -> Span {
        Span {
            start: Location { line: 0, column: 0 },
            end: Location { line: 0, column: 0 },
        }
    }

    /// Returns the smallest Span that contains both `self` and `other`
    /// If either span is [Span::empty], the other span is returned
    ///
    /// # Examples
    /// ```ignore
    /// # use sqlparser::tokenizer::{Span, Location};
    /// // line 1, column1 -> line 2, column 5
    /// let span1 = Span::new(Location::new(1, 1), Location::new(2, 5));
    /// // line 2, column 3 -> line 3, column 7
    /// let span2 = Span::new(Location::new(2, 3), Location::new(3, 7));
    /// // Union of the two is the min/max of the two spans
    /// // line 1, column 1 -> line 3, column 7
    /// let union = span1.union(&span2);
    /// assert_eq!(union, Span::new(Location::new(1, 1), Location::new(3, 7)));
    /// ```
    pub fn union(&self, other: &Span) -> Span {
        // If either span is empty, return the other
        // this prevents propagating (0, 0) through the tree
        match (self, other) {
            (&Span::EMPTY, _) => *other,
            (_, &Span::EMPTY) => *self,
            _ => Span {
                start: cmp::min(self.start, other.start),
                end: cmp::max(self.end, other.end),
            },
        }
    }

    /// Same as [Span::union] for `Option<Span>`
    ///
    /// If `other` is `None`, `self` is returned
    pub fn union_opt(&self, other: &Option<Span>) -> Span {
        match other {
            Some(other) => self.union(other),
            None => *self,
        }
    }

    /// Return the [Span::union] of all spans in the iterator
    ///
    /// If the iterator is empty, an empty span is returned
    ///
    /// # Example
    /// ```ignore
    /// # use sqlparser::tokenizer::{Span, Location};
    /// let spans = vec![
    ///     Span::new(Location::new(1, 1), Location::new(2, 5)),
    ///     Span::new(Location::new(2, 3), Location::new(3, 7)),
    ///     Span::new(Location::new(3, 1), Location::new(4, 2)),
    /// ];
    /// // line 1, column 1 -> line 4, column 2
    /// assert_eq!(
    ///   Span::union_iter(spans),
    ///   Span::new(Location::new(1, 1), Location::new(4, 2))
    /// );
    pub fn union_iter<I: IntoIterator<Item = Span>>(iter: I) -> Span {
        iter.into_iter()
            .reduce(|acc, item| acc.union(&item))
            .unwrap_or(Span::empty())
    }
}

/// Backwards compatibility struct for [`TokenWithSpan`]
#[deprecated(since = "0.53.0", note = "please use `TokenWithSpan` instead")]
pub type TokenWithLocation = TokenWithSpan;

/// A [Token] with [Span] attached to it
///
/// This is used to track the location of a token in the input string
///
/// # Examples
/// ```ignore
/// # use sqlparser::tokenizer::{Location, Span, Token, TokenWithSpan};
/// // commas @ line 1, column 10
/// let tok1 = TokenWithSpan::new(
///   Token::Comma,
///   Span::new(Location::new(1, 10), Location::new(1, 11)),
/// );
/// assert_eq!(tok1, Token::Comma); // can compare the token
///
/// // commas @ line 2, column 20
/// let tok2 = TokenWithSpan::new(
///   Token::Comma,
///   Span::new(Location::new(2, 20), Location::new(2, 21)),
/// );
/// // same token but different locations are not equal
/// assert_ne!(tok1, tok2);
/// ```
#[derive(Debug, Clone, Hash, Ord, PartialOrd, Eq, PartialEq)]

/// A `Token` together with its `Span` (location in the source).
pub struct TokenWithSpan {
    /// The token value.
    pub token: Token,
    /// The span covering the token in the input.
    pub span: Span,
}

impl TokenWithSpan {
    /// Create a new [`TokenWithSpan`] from a [`Token`] and a [`Span`]
    pub fn new(token: Token, span: Span) -> Self {
        Self { token, span }
    }

    /// Wrap a token with an empty span
    pub fn wrap(token: Token) -> Self {
        Self::new(token, Span::empty())
    }

    /// Wrap a token with a location from `start` to `end`
    pub fn at(token: Token, start: Location, end: Location) -> Self {
        Self::new(token, Span::new(start, end))
    }

    /// Return an EOF token with no location
    pub fn new_eof() -> Self {
        Self::wrap(Token::EOF)
    }
}

impl PartialEq<Token> for TokenWithSpan {
    fn eq(&self, other: &Token) -> bool {
        &self.token == other
    }
}

impl PartialEq<TokenWithSpan> for Token {
    fn eq(&self, other: &TokenWithSpan) -> bool {
        self == &other.token
    }
}

impl fmt::Display for TokenWithSpan {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.token.fmt(f)
    }
}

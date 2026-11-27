//! The ClickHouse analysis.
//!
//! The server describes the statement: `DESCRIBE (…)` answers with each result column's name and
//! type and runs nothing. A column's type is the whole of what the engine says of it, nullability
//! and nested shape included ([`crate::types`]), and a parameter's is the one its placeholder
//! declares, `{name:Type}`, or the cast after it, `:name::Type`.
//!
//! What the describe leaves open is that ClickHouse writes a parameter's value into the statement
//! as a constant before it reads the statement. A constant can decide a type: the scale of
//! `toDateTime64(x, {p:UInt8})`, the type `CAST(x, {t:String})` names, the element
//! `tupleElement(t, {i:UInt8})` reads. A constant is typed otherwise than a column, too: `x IN
//! (1, {n:UInt8})` is a UInt8 of constants and a Nullable(UInt8) of a column, and a function of a
//! `LowCardinality` column and a constant is `LowCardinality` where one of two columns is not. And
//! a constant is computed with as the statement is read, so no one value stands for a parameter:
//! `toDate('a')` fails where `toDate({day:String})` is a fine statement. So:
//!
//! - The statement's types are the ones it has as it runs, every parameter a constant. Each type
//!   has a list of values that stand for a parameter of it ([`constants`]), a string's among them
//!   the texts a statement commonly computes with: a number, a date, a time, a zone, a unit. The
//!   statement is described with each parameter's first value, then its second, and where both
//!   fail, with each later one, then with one parameter changed at a time, until it describes.
//!   Two describes that answer alike give the statement's types; two that differ show a value
//!   deciding a type, and the parameter is refused.
//! - The statement is described with each parameter a column too,
//!   `materialize(defaultValueOfTypeName('T'))`, which has the parameter's type and no value to
//!   read. ClickHouse refuses it wherever it needs a constant, which finds the parameters it reads
//!   as constants. A `LIMIT`, an `OFFSET` and the right side of an `IN` take a constant and
//!   decide no type, so a parameter there is written as one.
//! - A parameter the server reads as a constant is tried with every value of its list: the same
//!   types with another of them, and they are the statement's (`formatDateTime(t,
//!   {format:String})`, `quantile({level:Float64})(x)`, `dateDiff({unit:String}, a, b)`); other
//!   types with one, and it is refused where it stands. A value the statement fails with says
//!   nothing of its types, since the statement fails with it as it runs too.
//! - A column the statement gives no name is named by its expression's text, which holds each
//!   constant in it, a parameter's value too. One such column alone is the statement's value, and
//!   goes by no name; among others it is refused, as a name that changes from run to run.
//!
//! No list of values shows that a type holds for every value, so the run time holds the types each
//! result comes back with against the ones read here, and refuses a result that differs
//! (src/decode-clickhouse.ts). A parameter that names a column or a table, `{c:Identifier}`,
//! changes what the statement reads, and is refused.
//!
//! How many rows the statement returns is read from the server's query tree ([`crate::tree`]). A
//! write (`INSERT`, `DELETE`, `UPDATE`, `ALTER TABLE … UPDATE` or `DELETE`) returns no rows, and
//! ClickHouse describes none: an `INSERT` is checked against its table by `EXPLAIN PIPELINE`,
//! which builds the insert and writes nothing, and a `DELETE` or an `UPDATE` as the `SELECT` of
//! the rows it would change and the values it would write. An `ALTER` that does anything else
//! (`ADD COLUMN`, `FREEZE`) changes no rows, and is refused. ClickHouse converts what a write
//! gives a column to the column's type and changes a value the column cannot hold without an
//! error, so a value that depends on a parameter has to have a type its column keeps
//! ([`write`]): the server's description of the values says each one's type, and its query tree
//! which of them a parameter reaches.

use crate::api::{Analysis, Column, Diagnostic, Param, Scope};
use crate::host::{as_str, Host, Lazy};
use crate::tree::{Flow, Operands, Pick};
use crate::types::{self, Arg, Named};
use reylo_engine_core::analysis::{self, ResultTarget};
use reylo_engine_core::api::DbError;
use reylo_engine_core::dialect::Dialect;
use reylo_engine_core::lex::{self, Lexed, Placeholder, Rewritten, Target};
use reylo_engine_core::lexical::Rules;
use reylo_engine_core::types::{Cardinality, StatementKind, Type};
use serde_json::Value;

/// The settings of the session that may drop the one row an aggregate returns: the three that
/// leave it are off, `0`, by default, and the filter every result passes is empty. With them,
/// the one that leaves each shard's rows of a distributed table unmerged, which bounds nothing.
pub const ROW_SETTINGS: &str = "SELECT name, value FROM system.settings WHERE name IN ('empty_result_for_aggregation_by_empty_set', 'limit', 'offset', 'additional_result_filter', 'distributed_group_by_no_merge')";

/// The setting that has the server infer a type from a text it converts to a Dynamic, and the
/// one that has a CAST keep its value's nullability, whatever type it names.
const INFERS: &str = "cast_string_to_dynamic_use_inference";
const KEEPS_NULLABLE: &str = "cast_keep_nullable";
/// Asks what the session has the two set to.
const CONVERSION_SETTINGS: &str = "SELECT name, value FROM system.settings WHERE name IN ('cast_string_to_dynamic_use_inference', 'cast_keep_nullable')";

const DESCRIBE: &str = "DESCRIBE (\n";
const TREE: &str = "EXPLAIN QUERY TREE\n";
const PIPELINE: &str = "EXPLAIN PIPELINE\n";
const AST: &str = "EXPLAIN AST\n";
/// Asks the type the server reads a name as; the name, quoted, and `))` follow.
const TYPE_NAME: &str = "SELECT toTypeName(defaultValueOfTypeName(";

/// A describe's rows without the subcolumns a session's `describe_include_subcolumns` adds after
/// them (`t.1`, `arr.size0`, `n.null`), each marked in an eighth field: a result holds none of them.
fn own_columns(answer: Lazy<Described>) -> Lazy<Described> {
    let subcolumn = |row: &Vec<Value>| row.get(7).is_some_and(|flag| flag.as_u64() == Some(1) || flag.as_str() == Some("1") || flag.as_bool() == Some(true));
    answer.map(|described| described.map(|rows| rows.into_iter().filter(|row| !subcolumn(row)).collect()))
}

/// Where a parameter stands, as far as the analysis writes it differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    /// Anywhere a value goes.
    Value,
    /// Straight after `LIMIT`, or after the comma of `LIMIT offset, count`.
    Limit,
    /// Straight after `OFFSET`.
    Offset,
    /// The right side of an `IN`: straight after the word, or in the list in parentheses after it.
    In,
    /// Where the server takes a constant alone, and the statement describes with the same types
    /// for each value tried: found by asking the server, where the others are read off the text.
    Constant,
}

/// How each placeholder of a statement is written into a text the server is asked about, by the
/// placeholder's index: its constant of a number, or `None`, a column. One parameter is written
/// one way at each of its places.
type Choice = Vec<Option<usize>>;

/// What a describe answers with: a row a column, its name and its type's.
type Rows = Vec<Vec<Value>>;
type Described = Result<Rows, DbError>;

fn whole(sql: &str, message: impl Into<String>, code: u32, server_code: Option<String>) -> Diagnostic {
    Diagnostic { message: message.into(), start: 0, length: sql.trim_end().len().max(1), code, server_code }
}

fn at(p: &Placeholder, message: impl Into<String>, code: u32) -> Diagnostic {
    Diagnostic { message: message.into(), start: p.start, length: p.end - p.start, code, server_code: None }
}

/// The type a placeholder declares, as written: inside its braces, or in the cast after `:name`.
fn written_type<'a>(sql: &'a str, p: &Placeholder) -> Option<&'a str> {
    let text = &sql[p.start..p.end];
    if text.starts_with('{') {
        let colon = text.find(':')?;
        return Some(text[colon + 1..text.len() - 1].trim());
    }
    let after = sql[p.end..].strip_prefix("::")?;
    Some(&after[..type_end(after)])
}

/// The length of the type's name `text` starts with: a name, and its arguments in parentheses.
fn type_end(text: &str) -> usize {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
        i += 1;
    }
    if b.get(i) != Some(&b'(') {
        return i;
    }
    let mut depth = 0usize;
    while i < b.len() {
        match b[i] {
            b'\'' => {
                i += 1;
                while i < b.len() && b[i] != b'\'' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    break;
                }
            }
            _ => i += 1,
        }
    }
    i.min(b.len())
}

/// The spans a walk over the statement's code steps over whole: its strings, quoted names and
/// comments, and every placeholder but `but`, whose type may hold parentheses of its own.
fn skipped(lexed: &Lexed, but: &Placeholder) -> Vec<(usize, usize)> {
    let mut spans = lexed.opaque.clone();
    spans.extend(lexed.placeholders.iter().filter(|p| p.start != but.start).map(|p| (p.start, p.end)));
    spans.sort_unstable();
    spans
}

fn is_comment(sql: &str, start: usize) -> bool {
    lex::is_comment(&sql[start..])
}

/// ClickHouse's rules for the text around its tokens: its whitespace takes the vertical tab and
/// Unicode's spaces.
const TEXT: Rules = Rules::of(Dialect::Clickhouse);

/// The end of the code before `at`: past the whitespace and comments that stand between.
fn code_before(sql: &str, lexed: &Lexed, mut at: usize) -> usize {
    let b = sql.as_bytes();
    loop {
        while let Some(c) = sql[..at].chars().next_back().filter(|c| TEXT.space_at(b, at - c.len_utf8()) == c.len_utf8()) {
            at -= c.len_utf8();
        }
        match lexed.opaque.iter().find(|&&(start, end)| end == at && is_comment(sql, start)) {
            Some(&(start, _)) => at = start,
            None => return at,
        }
    }
}

/// The start of the code at or after `at`: past whitespace and comments.
fn code_after(sql: &str, lexed: &Lexed, mut at: usize) -> usize {
    let b = sql.as_bytes();
    loop {
        at = TEXT.skip_spaces(b, at);
        match lexed.opaque.iter().find(|&&(start, _)| start == at && is_comment(sql, start)) {
            Some(&(_, end)) => at = end,
            None => return at,
        }
    }
}

/// The word that ends at `end`, lowercased, and where it starts.
fn word_before(sql: &str, end: usize) -> (String, usize) {
    let b = sql.as_bytes();
    let mut start = end;
    while start > 0 && (b[start - 1].is_ascii_alphanumeric() || b[start - 1] == b'_') {
        start -= 1;
    }
    (sql[start..end].to_ascii_lowercase(), start)
}

/// Where the placeholder `p` stands.
fn position(sql: &str, lexed: &Lexed, p: &Placeholder) -> Position {
    let b = sql.as_bytes();
    let before = code_before(sql, lexed, p.start);
    match word_before(sql, before).0.as_str() {
        "limit" => return Position::Limit,
        "offset" => return Position::Offset,
        // `IN`, `NOT IN` and their `GLOBAL` forms all end with the word.
        "in" => return Position::In,
        _ => {}
    }
    // `LIMIT offset, count`: the count, after a comma that follows the offset's one word.
    if before > 0 && b[before - 1] == b',' {
        let offset_end = code_before(sql, lexed, before - 1);
        let offset_start = lexed.placeholders.iter().find(|q| q.end == offset_end).map_or_else(|| word_before(sql, offset_end).1, |q| q.start);
        if offset_start < offset_end && word_before(sql, code_before(sql, lexed, offset_start)).0 == "limit" {
            return Position::Limit;
        }
    }
    // The parentheses the placeholder stands in: the list of an `IN`, as in `x IN (1, {n:UInt8})`,
    // and not the subquery an `IN` reads, whose parameters are values as anywhere.
    let skip = skipped(lexed, p);
    let (mut i, mut depth, mut open) = (p.start, 0usize, None);
    while i > 0 {
        if let Some(&(start, _)) = skip.iter().find(|&&(_, end)| end == i) {
            i = start;
            continue;
        }
        i -= 1;
        match b[i] {
            b')' => depth += 1,
            b'(' if depth == 0 => {
                open = Some(i);
                break;
            }
            b'(' => depth -= 1,
            _ => {}
        }
    }
    let Some(open) = open else { return Position::Value };
    let first = code_after(sql, lexed, open + 1);
    let word = sql[first..].bytes().take_while(u8::is_ascii_alphabetic).map(|c| c.to_ascii_lowercase() as char).collect::<String>();
    if word_before(sql, code_before(sql, lexed, open)).0 == "in" && !matches!(word.as_str(), "select" | "with" | "from") {
        return Position::In;
    }
    Position::Value
}

/// The texts a statement commonly computes with as it reads a string: plain letters first, then
/// a number, a date, a time, a zone, a unit, a UUID, an address and a document, two of each, so
/// that a statement that takes one of a kind describes with another of it. The units are of both
/// kinds, those of a time and those of a date: `dateTrunc` to a month is a date, and to a day a time.
const STRINGS: [&str; 22] = [
    "a", "b", "c", "1", "2", "2001-02-03", "2004-05-06", "2001-02-03 04:05:06", "2004-05-06 07:08:09", "UTC", "Asia/Tokyo", "day", "hour", "month", "61f0c404-5cb3-11e7-907b-a6006ad3dba0", "00112233-4455-6677-8899-aabbccddeeff",
    "127.0.0.1", "10.1.2.3", "::1", "2001:db8::1", "{}", "{\"a\": 1}",
];
/// The whole numbers a parameter is tried with: the first few, where a place in a tuple or a
/// number of digits is read, then some further off.
const WHOLE: [&str; 15] = ["1", "2", "3", "0", "4", "5", "6", "7", "8", "9", "10", "16", "32", "64", "100"];
/// Within 0 and 1 first, which a level and a ratio take alone.
const FRACTIONS: [&str; 9] = ["0.5", "0.25", "0.75", "1", "0", "2", "10", "100", "-1"];

/// The constants a parameter of `ty` is written as where the server reads one: each as the
/// expression that stands in its place, and as a message shows it. The first two differ for every
/// type that has two values, so that a type or a name that changes with the value shows; a type
/// whose values are not written here has its default alone, which no describe can vary.
fn constants(ty: &Named) -> Vec<(String, String)> {
    let name = types::quoted(&ty.to_string());
    let cast = |value: &str| format!("CAST({value}, {name})");
    let of = |values: &[&str]| values.iter().map(|v| (cast(v), v.to_string())).collect::<Vec<_>>();
    let texts = |values: &[&str]| values.iter().map(|v| (cast(&types::quoted(v)), types::quoted(v))).collect::<Vec<_>>();
    let mut out = match (ty.name.as_str(), ty.args.as_slice()) {
        // A value, NULL, and its other values.
        ("Nullable", [Arg::Type(inner)]) => {
            let inner = constants(inner);
            let mut out = vec![(cast(&inner[0].0), inner[0].1.clone()), (cast("NULL"), "NULL".to_string())];
            out.extend(inner[1..].iter().map(|(sql, shown)| (cast(sql), shown.clone())));
            out
        }
        ("LowCardinality", [Arg::Type(inner)]) => constants(inner).into_iter().map(|(sql, shown)| (cast(&sql), shown)).collect(),
        // One element, none, two, the two the other way, three, and those the other way: two of
        // each length a statement may ask for.
        ("Array", [Arg::Type(inner)]) => {
            let inner = constants(inner);
            let at = |i: usize| &inner[i.min(inner.len() - 1)];
            let listed = |indexes: &[usize]| {
                let sql: Vec<&str> = indexes.iter().map(|&i| at(i).0.as_str()).collect();
                let shown: Vec<&str> = indexes.iter().map(|&i| at(i).1.as_str()).collect();
                (cast(&format!("[{}]", sql.join(", "))), format!("[{}]", shown.join(", ")))
            };
            vec![listed(&[0]), listed(&[]), listed(&[1, 0]), listed(&[0, 1]), listed(&[0, 1, 2]), listed(&[2, 1, 0])]
        }
        // Each element's first value, then each one's second, and third.
        ("Tuple", args) if !args.is_empty() && args.iter().all(|a| matches!(a, Arg::Type(_) | Arg::Member(..))) => {
            let elements: Vec<Vec<(String, String)>> = args.iter().map(|a| match a {
                Arg::Type(t) | Arg::Member(_, t) => constants(t),
                _ => Vec::new(),
            }).collect();
            (0..3)
                .map(|j| {
                    let at = |element: &Vec<(String, String)>| element[j.min(element.len() - 1)].clone();
                    let (sql, shown): (Vec<String>, Vec<String>) = elements.iter().map(at).unzip();
                    (cast(&format!("tuple({})", sql.join(", "))), format!("({})", shown.join(", ")))
                })
                .collect()
        }
        // One pair, none, and another pair.
        ("Map", [Arg::Type(key), Arg::Type(value)]) => {
            let (keys, values) = (constants(key), constants(value));
            let pair = |j: usize| (&keys[j.min(keys.len() - 1)], &values[j.min(values.len() - 1)]);
            let one = |j: usize| (cast(&format!("map({}, {})", pair(j).0 .0, pair(j).1 .0)), format!("{{{}: {}}}", pair(j).0 .1, pair(j).1 .1));
            vec![one(0), (cast("map()"), "{}".to_string()), one(1)]
        }
        ("Enum8" | "Enum16", labels) => {
            let labels: Vec<&str> = labels.iter().filter_map(|a| if let Arg::Label(label, _) = a { Some(label.as_str()) } else { None }).take(3).collect();
            texts(&labels)
        }
        ("Bool", _) => of(&["true", "false"]),
        ("String" | "FixedString", _) => texts(&STRINGS),
        ("UUID", _) => texts(&["61f0c404-5cb3-11e7-907b-a6006ad3dba0", "00112233-4455-6677-8899-aabbccddeeff", "00000000-0000-0000-0000-000000000000"]),
        ("IPv4", _) => texts(&["127.0.0.1", "10.1.2.3", "0.0.0.0"]),
        ("IPv6", _) => texts(&["::1", "2001:db8::1", "::"]),
        // A date or a time of a number: so many days, or seconds, after 1970.
        ("Date" | "Date32" | "DateTime" | "DateTime64", _) => of(&["1", "2", "3"]),
        ("Float32" | "Float64" | "BFloat16", _) => of(&FRACTIONS),
        // A number below zero for a type that holds one: a place counted from the end.
        (name, _) if ty.is_number() && !name.starts_with("UInt") => of(&[&WHOLE[..], &["-1", "-2"]].concat()),
        _ if ty.is_number() => of(&WHOLE),
        _ => Vec::new(),
    };
    // A value is tried once, and a type none of whose values is written here has its default.
    let mut seen = std::collections::HashSet::new();
    out.retain(|(sql, _)| seen.insert(sql.clone()));
    if out.is_empty() {
        out.push((format!("defaultValueOfTypeName({name})"), "its type's default value".to_string()));
    }
    out
}

/// The statement's parameters as the analysis reads them: each placeholder's type, where it
/// stands, and the constants that stand for it.
struct Parameters {
    types: Vec<Named>,
    positions: Vec<Position>,
    constants: Vec<Vec<(String, String)>>,
}

impl Parameters {
    fn of(types: Vec<Named>, positions: Vec<Position>) -> Parameters {
        let constants = types.iter().map(constants).collect();
        Parameters { types, positions, constants }
    }

    /// The placeholder at `k` as a column: a value of its type that is no constant, which the
    /// server neither reads nor computes with.
    fn column(&self, k: usize) -> String {
        format!("materialize(defaultValueOfTypeName({}))", types::quoted(&self.types[k].to_string()))
    }

    /// The placeholder's constant numbered `j`, or its last where it has fewer: the expression,
    /// and the value as a message shows it.
    fn constant(&self, k: usize, j: usize) -> &(String, String) {
        let all = &self.constants[k];
        &all[j.min(all.len() - 1)]
    }

    /// Every parameter as its constant numbered `j`: the statement as it runs.
    fn joint(&self, j: usize) -> Choice {
        vec![Some(j); self.types.len()]
    }

    /// `choice`, but each parameter that stands where a value goes as a column.
    fn columns(&self, choice: &Choice) -> Choice {
        choice.iter().enumerate().map(|(k, how)| if self.positions[k] == Position::Value { None } else { *how }).collect()
    }

    /// `choice`, but the parameter at `k`, at each of its places, written as `how`.
    fn with(&self, lexed: &Lexed, choice: &Choice, k: usize, how: Option<usize>) -> Choice {
        choice.iter().enumerate().map(|(i, own)| if lexed.placeholders[i].name == lexed.placeholders[k].name { how } else { *own }).collect()
    }

    /// Whether two choices write the parameter at `k` as two values, or one as a column.
    fn differs(&self, k: usize, a: &Choice, b: &Choice) -> bool {
        match (a[k], b[k]) {
            (Some(x), Some(y)) => self.constant(k, x).0 != self.constant(k, y).0,
            (None, None) => false,
            _ => true,
        }
    }

    /// The value a choice writes the parameter at `k` as, as a message shows it.
    fn shown(&self, k: usize, choice: &Choice) -> &str {
        choice[k].map_or("a column", |j| self.constant(k, j).1.as_str())
    }

    /// The statement `body` with each placeholder written as `choice` says, and the map from that
    /// text back to the author's. A placeholder may stand against a word (`IN{ids:Array(UInt8)}`),
    /// which what is written for it, a function's call, would read as one name with: a space
    /// parts them.
    fn text(&self, body: &str, lexed: &Lexed, choice: &Choice) -> Rewritten {
        let b = body.as_bytes();
        let word = |at: Option<&u8>| at.is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_');
        let mut out = String::with_capacity(body.len());
        let mut spans = Vec::with_capacity(lexed.placeholders.len());
        let mut last = 0;
        for (k, p) in lexed.placeholders.iter().enumerate() {
            out.push_str(&body[last..p.start]);
            let start = out.len();
            if p.start > 0 && word(b.get(p.start - 1)) {
                out.push(' ');
            }
            match choice[k] {
                Some(j) => out.push_str(&self.constant(k, j).0),
                None => out.push_str(&self.column(k)),
            }
            if word(b.get(p.end)) {
                out.push(' ');
            }
            spans.push((p.start, p.end, start, out.len()));
            last = p.end;
        }
        out.push_str(&body[last..]);
        Rewritten { sql: out, order: Vec::new(), spans, inserted: Vec::new() }
    }

    /// The server's text with each stand-in read back as the author wrote its parameter, where one
    /// parameter alone has that stand-in: the longest first, since one may hold another.
    fn authored(&self, sql: &str, lexed: &Lexed, message: &str) -> String {
        let mut stood: Vec<(String, usize)> = Vec::new();
        for k in 0..lexed.placeholders.len() {
            stood.push((self.column(k), k));
            stood.extend(self.constants[k].iter().map(|(text, _)| (text.clone(), k)));
        }
        stood.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        let mut out = message.to_string();
        for (text, k) in &stood {
            let p = &lexed.placeholders[*k];
            if !stood.iter().any(|(other, j)| other == text && lexed.placeholders[*j].name != p.name) {
                out = out.replace(text, &sql[p.start..p.end]);
            }
        }
        out
    }
}

/// The server's error as the author reads it: without the code it opens with and the version it
/// ends with, which the diagnostic carries apart.
fn message_of(e: &DbError) -> String {
    let mut text = e.message.trim();
    if let Some(rest) = text.strip_prefix("Code: ") {
        if let Some((_, after)) = rest.split_once(". ") {
            text = after;
        }
    }
    text = text.strip_prefix("DB::Exception: ").unwrap_or(text);
    if let Some(at) = text.rfind(" (version ") {
        text = &text[..at];
    }
    text.trim().to_string()
}

/// The 0-based offset in the text sent that a syntax error names: `failed at position 39`.
fn position_of(message: &str) -> Option<usize> {
    let digits: String = message.split("failed at position ").nth(1)?.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<usize>().ok()?.checked_sub(1)
}

/// A syntax error's parts: what it opens with (`Syntax error`), the token it failed at, and what
/// follows the place, which opens with the text sent from there on: `Syntax error: failed at
/// position 40 (JSON) (line 2, col 29): JSON\n). Expected one of: …`, the line and column only
/// where the text has more than one line.
fn syntax_parts(message: &str) -> Option<(&str, &str, &str)> {
    let (head, rest) = message.split_once("failed at position ")?;
    let rest = rest.trim_start_matches(|c: char| c.is_ascii_digit()).strip_prefix(" (")?;
    let (token, rest) = match (rest.find(") (line "), rest.find("): ")) {
        (Some(line), Some(colon)) if line < colon => (&rest[..line], rest[line + 2..].split_once("): ")?.1),
        (_, Some(colon)) => (&rest[..colon], &rest[colon + 3..]),
        _ => return None,
    };
    Some((head.trim_end().trim_end_matches(':'), token, rest))
}

/// The diagnostic of an error the server answered a question about `text` with, the question
/// opening with `prefix`: at the place a syntax error names, else the whole statement. A syntax
/// error's message is the server's without the place, which it counts in the text it was sent, and
/// without that text's echo: the diagnostic's own place says where.
fn server_error(sql: &str, lexed: &Lexed, parameters: &Parameters, text: &Rewritten, prefix: &str, e: &DbError) -> Diagnostic {
    let own = message_of(e);
    match position_of(&e.message).and_then(|at| at.checked_sub(prefix.len())) {
        Some(at) => {
            // An error at the end of the statement names the `)` the question closes with.
            let start = text.to_original(at.min(text.sql.len().saturating_sub(1))).min(sql.trim_end().len().saturating_sub(1));
            let after = sql.get(start..).unwrap_or_default();
            let hint = after.strip_prefix(':').filter(|rest| rest.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')).map(|rest| {
                let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                format!(" A ClickHouse parameter is written with its type: {{{name}:Type}}, or :{name}::Type.")
            });
            let (message, length) = match syntax_parts(&own) {
                Some((head, token, rest)) => {
                    let sent = format!("{}\n)", text.sql);
                    let echo = sent.get(at..).unwrap_or_default();
                    let mut cut = rest.bytes().zip(echo.bytes()).take_while(|(a, b)| a == b).count();
                    while !rest.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    let rest = rest[cut..].trim_start().trim_start_matches('.').trim_start();
                    // The token is the author's where the statement has it at that place, and not a stand-in's.
                    let length = if !token.is_empty() && after.starts_with(token) { token.len() } else { 1 };
                    (format!("{head}. {rest}"), length)
                }
                None => (own.clone(), 1),
            };
            Diagnostic { message: format!("{}{}", parameters.authored(sql, lexed, &message), hint.unwrap_or_default()), start, length, code: 6, server_code: e.code.clone() }
        }
        None => whole(sql, parameters.authored(sql, lexed, &own), 6, e.code.clone()),
    }
}

/// Whether a result column's name is one a statement can rely on: a name, as an alias or a column
/// read as it is has, and not an expression's text, which ClickHouse names an unnamed column by.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_') && chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// The length of the name `text` opens with: letters, digits and `_`, a name in backquotes or
/// double quotes, in which a backslash escapes, or one in typographic quotes, `“…”`, to its first
/// `”` (`lexical::Rules::typographic_quotes`).
fn name_length(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    if let Some(inside) = text.strip_prefix('“') {
        return inside.find('”').map(|at| '“'.len_utf8() + at + '”'.len_utf8());
    }
    match *bytes.first()? {
        quote @ (b'`' | b'"') => {
            let mut i = 1;
            while i < bytes.len() && bytes[i] != quote {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            (i < bytes.len()).then_some(i + 1)
        }
        b if b.is_ascii_alphabetic() || b == b'_' || b >= 0x80 => Some(bytes.iter().position(|b| !(b.is_ascii_alphanumeric() || *b == b'_' || *b >= 0x80)).unwrap_or(bytes.len())),
        _ => None,
    }
}

/// Whether an item of a select list is a `*`: alone, a table's (`u.*`), with its modifiers
/// (`* EXCEPT (a)`), a `COLUMNS('…')` matcher, or a tuple spread by `untuple`. Each stands for
/// any number of columns. A table's is read from the item's start, name by name, so that a `.*`
/// in a string the item holds is none.
fn is_star(item: &str) -> bool {
    let item = item.trim();
    let opens = |call: &str| item.get(..call.len()).is_some_and(|head| head.eq_ignore_ascii_case(call));
    if item.starts_with('*') || opens("columns(") || opens("untuple(") {
        return true;
    }
    let mut rest = item;
    while let Some(length) = name_length(rest) {
        rest = &rest[length..];
        if let Some(after) = rest.strip_prefix(".*") {
            return after.is_empty() || after.starts_with(char::is_whitespace);
        }
        match rest.strip_prefix('.') {
            Some(after) => rest = after,
            None => break,
        }
    }
    false
}

/// Whether an item's own text says its column's name: a column read as it is, by its name behind
/// any table's or tuple's (`u.name`, `spot.x`, `` `odd name` ``), or an alias after `AS`.
fn names_itself(item: &str) -> bool {
    let item = item.trim();
    // Names joined by dots, and nothing else.
    let mut rest = item;
    let column = loop {
        let Some(length) = name_length(rest) else { break false };
        rest = &rest[length..];
        match rest.strip_prefix('.') {
            Some(after) => rest = after,
            None => break rest.is_empty(),
        }
    };
    // `… AS name`: the item's last name, after the word.
    let alias = || {
        let bytes = item.as_bytes();
        let start = match *bytes.last()? {
            // A name in typographic quotes ends at its first `”`, so it opens at the `“` whose first `”` is the last.
            _ if item.ends_with('”') => item.match_indices('“').map(|(at, _)| at).find(|&at| name_length(&item[at..]) == Some(item.len() - at))?,
            quote @ (b'`' | b'"') => (0..bytes.len() - 1).rev().find(|&i| bytes[i] == quote && (i == 0 || bytes[i - 1] != b'\\'))?,
            _ => bytes.iter().rposition(|b| !(b.is_ascii_alphanumeric() || *b == b'_' || *b >= 0x80)).map_or(0, |i| i + 1),
        };
        (name_length(&item[start..]) == Some(item.len() - start)).then_some(())?;
        let before = item[..start].trim_end();
        let word = before.len().checked_sub(2).and_then(|at| before.get(at..))?;
        (word.eq_ignore_ascii_case("as") && before[..before.len() - 2].ends_with(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '\'' | '`' | '"' | '’' | '”'))).then_some(())
    };
    column || alias().is_some()
}

/// The statement's select list, as far as its text and its columns' names say which of its items
/// name their column. An item is named where the server's name for its column reads as one, as an
/// alias and a column's own do, or where its text says the name, as a quoted alias and a member
/// read by its path do (`items.sku`); else the server made the name up from the expression. Where
/// the text's items cannot be the columns', each column is judged by its name alone.
fn targets(sql: &str, columns: &[Column]) -> Vec<ResultTarget> {
    let by_name = || columns.iter().map(|c| if is_identifier(&c.name) { ResultTarget::Named } else { ResultTarget::Unnamed(None) }).collect::<Vec<_>>();
    let Some(items) = analysis::result_items(sql, Dialect::Clickhouse) else { return by_name() };
    let texts: Vec<&str> = items.iter().map(|&(start, length)| &sql[start..start + length]).collect();
    let stars = texts.iter().filter(|text| is_star(text)).count();
    if (stars == 0 && texts.len() != columns.len()) || texts.len() - stars > columns.len() {
        return by_name();
    }
    let first = texts.iter().position(|text| is_star(text)).unwrap_or(texts.len());
    let last = texts.iter().rposition(|text| is_star(text)).unwrap_or(0);
    texts
        .iter()
        .enumerate()
        .map(|(k, text)| {
            if is_star(text) {
                return ResultTarget::Star;
            }
            // The item's column: counted from the left before the first `*`, from the right after
            // the last, and unknown between two.
            let column = if k < first { columns.get(k) } else if k > last { columns.get(columns.len() - (texts.len() - k)) } else { None };
            match column {
                Some(column) if !is_identifier(&column.name) && !names_itself(text) => ResultTarget::Unnamed(Some(items[k])),
                _ => ResultTarget::Named,
            }
        })
        .collect()
}

pub fn analyze(sql: &str, host: &Host) -> Lazy<Analysis> {
    let lexed = match lex::lex(sql, Dialect::Clickhouse) {
        Ok(lexed) => lexed,
        Err(e) => return Some(analysis::bare(sql, &analysis::none(), Dialect::Clickhouse, vec![analysis::diag(e.message, e.offset, 1, 1)])),
    };
    let refuse = |d: Vec<Diagnostic>| Some(analysis::bare(sql, &lexed, Dialect::Clickhouse, d));
    let statement = match lex::one_statement(sql, &lexed, Dialect::Clickhouse, &["select", "with", "from", "insert", "delete", "update", "alter"]) {
        Ok(statement) => statement,
        Err(message) => return refuse(vec![whole(sql, message, 10, None)]),
    };
    let word = lex::first_word(sql, &lexed, Dialect::Clickhouse);
    // Any other first word is read as a query's, and the server's error names it (`selec`).
    let writes = matches!(word.as_str(), "insert" | "delete" | "update" | "alter");
    if let lex::Statement::Command(command) = &statement {
        return refuse(vec![whole(sql, lex::refused_command(&format!("Reylo types a ClickHouse SELECT, INSERT, DELETE or UPDATE, and {} is none of them", command.to_ascii_uppercase())), 10, None)]);
    }
    // Each placeholder's type, as it declares it: the type its text names, or, where that is no
    // name Reylo reads, the type the server reads it as, which is how an alias (`int`, `varchar`)
    // and a name in other letters (`date`) are the types they stand for.
    let texts: Vec<Option<&str>> = lexed.placeholders.iter().map(|p| written_type(sql, p).map(str::trim).filter(|text| !text.is_empty())).collect();
    let written: Vec<Option<Named>> = texts.iter().map(|text| text.and_then(types::parse)).collect();
    let named: Vec<_> = texts
        .iter()
        .zip(&written)
        .map(|(text, ty)| {
            let known = ty.as_ref().is_some_and(|ty| ty.name == "Identifier" || types::parameter(ty).is_ok());
            text.filter(|_| !known).map(|text| host.query(&format!("{TYPE_NAME}{}))", types::quoted(text)), Vec::new(), Scope::Session))
        })
        .collect();
    let mut resolved: Vec<Option<Named>> = Vec::with_capacity(named.len());
    for answer in named {
        resolved.push(match answer {
            Some(answer) => answer?.ok().and_then(|rows| rows.first().and_then(|row| row.first()).and_then(as_str).and_then(types::parse)),
            None => None,
        });
    }
    let mut declared: Vec<Named> = Vec::with_capacity(lexed.placeholders.len());
    let mut refused: Vec<Diagnostic> = Vec::new();
    for (k, p) in lexed.placeholders.iter().enumerate() {
        let text = &sql[p.start..p.end];
        let Some(ty) = resolved[k].clone().or_else(|| written[k].clone()) else {
            refused.push(at(p, format!("{text} names no type: a ClickHouse parameter is written with its type, as in {{{}:UInt64}}.", p.name), 10));
            declared.push(Named { name: "Nothing".into(), args: Vec::new() });
            continue;
        };
        if ty.name == "Identifier" {
            refused.push(at(p, format!("{text} stands for a name, so the statement's columns and their types would change with its value: write the name in the statement."), 10));
        } else if let Err(types::Unread(name)) = types::parameter(&ty) {
            refused.push(at(p, format!("Reylo binds no parameter of the type {name}: it is no ClickHouse type, or one whose values it does not write yet, as a JSON, a Dynamic or an interval is."), 10));
        } else if ty.holds("Variant") {
            // A Variant's text does not say which of its types a value is: 7 reads as a number and as a string.
            refused.push(at(p, format!("{text} is a Variant, whose value's text does not say which of its types it is: declare the parameter with the one type it has."), 10));
        } else if let Some(first) = lexed.placeholders[..k].iter().position(|q| q.name == p.name).filter(|&first| declared[first] != ty) {
            refused.push(at(p, format!("The parameter {} is declared {} here and {} at its first use: declare it with one type.", p.name, ty, declared[first]), 3));
        }
        declared.push(ty);
    }
    if !refused.is_empty() {
        return refuse(refused);
    }
    let mut parameters = Parameters::of(declared, lexed.placeholders.iter().map(|p| position(sql, &lexed, p)).collect());
    // The statement without what follows it: a `;`, and the whitespace and comments after one.
    let in_text = |i: usize| lexed.opaque.iter().any(|&(start, end)| start <= i && i < end) || lexed.placeholders.iter().any(|p| p.start <= i && i < p.end);
    let body = &sql[..sql.bytes().enumerate().find(|&(i, c)| c == b';' && !in_text(i)).map_or(sql.len(), |(i, _)| i)];
    // A format the statement's own SETTINGS names outranks the one a result is asked for in, the
    // server's answers to the questions below included, so it is read off the text.
    // A column or an alias may have the setting's name: the setting is the one a SETTINGS clause gives a value.
    let all = words(body, &lexed);
    let set = |at: usize, word: &str| word == "default_format" && body.as_bytes().get(code_after(body, &lexed, at + word.len())) == Some(&b'=');
    if let Some((start, word)) = all.iter().skip_while(|(_, word)| word != "settings").find(|(at, word)| set(*at, word)) {
        return refuse(vec![Diagnostic { message: "The statement's SETTINGS names a format for its result, default_format, and Reylo reads the one it asks for: take the setting out.".into(), start: *start, length: word.len(), code: 10, server_code: None }]);
    }
    let first = parameters.joint(0);
    let asked = parameters.text(body, &lexed, &parameters.columns(&first));
    let kind = match word.as_str() {
        "insert" => StatementKind::Insert,
        "delete" => StatementKind::Delete,
        "update" => StatementKind::Update,
        "alter" => StatementKind::Other,
        _ => StatementKind::Select,
    };
    let mut typed = analysis::bare(sql, &lexed, Dialect::Clickhouse, Vec::new());
    typed.kind = Some(kind);
    // The statement is sent as it is written, but for a type the server named, which is written
    // by that name, so that the run time reads every parameter's type by the name ClickHouse gives it.
    typed.rewritten = lex::rewrite_with(sql, &lexed, Target::Braced, &|name, _| {
        let k = lexed.placeholders.iter().position(|p| p.name == name).unwrap_or(0);
        match &resolved[k] {
            Some(_) => format!("{{{name}:{}}}", parameters.types[k]),
            None => format!("{{{name}:{}}}", written_type(sql, &lexed.placeholders[k]).unwrap_or_default()),
        }
    });
    typed.params = lexed
        .params
        .iter()
        .map(|name| {
            let ty = &parameters.types[lexed.placeholders.iter().position(|p| &p.name == name).unwrap_or(0)];
            let slot = types::parameter(ty).expect("a type the parameters were held to");
            Param { nullable: slot.nullable, ..Param::new(name.clone(), slot.ty) }
        })
        .collect();

    if writes {
        return write(sql, &lexed, &parameters, body, &asked, typed, host);
    }

    let describe = |parameters: &Parameters, choice: &Choice| own_columns(host.query(&format!("{DESCRIBE}{}\n)", parameters.text(body, &lexed, choice).sql), Vec::new(), Scope::Statement));
    // One round: the describe with each parameter a column, the query tree, the session's
    // settings, and the statement as it runs, each parameter a constant, with two values of each.
    let described = describe(&parameters, &parameters.columns(&first));
    let tree = host.query(&format!("{TREE}{}", asked.sql), Vec::new(), Scope::Statement);
    let settings = host.query(ROW_SETTINGS, Vec::new(), Scope::Session);
    let run = (!lexed.placeholders.is_empty()).then(|| (describe(&parameters, &first), describe(&parameters, &parameters.joint(1))));
    let (described, tree, settings) = (described?, tree?, settings?);
    let (rows, tree, varying) = match run {
        // No parameter: the describe is the statement's.
        None => match described {
            Ok(rows) => {
                let varying = vec![false; rows.len()];
                (rows, tree, varying)
            }
            Err(e) => return refuse(vec![server_error(sql, &lexed, &parameters, &asked, DESCRIBE, &e)]),
        },
        Some((one, two)) => {
            let (one, two) = (one?, two?);
            let ran = match as_it_runs(sql, &lexed, &parameters, body, &asked, &described, one, two, host)? {
                Ok(ran) => ran,
                Err(diagnostics) => return refuse(diagnostics),
            };
            // A parameter the server reads as a constant is written as one from here on, and each
            // with the value the statement describes with: the tree is read of that text.
            for &k in &ran.constants {
                for i in 0..lexed.placeholders.len() {
                    if lexed.placeholders[i].name == lexed.placeholders[k].name {
                        parameters.positions[i] = Position::Constant;
                    }
                }
            }
            let text = parameters.text(body, &lexed, &parameters.columns(&ran.base));
            let tree = if text.sql == asked.sql { tree } else { host.query(&format!("{TREE}{}", text.sql), Vec::new(), Scope::Statement)? };
            (ran.rows, tree, ran.varying)
        }
    };
    let mut unread = Vec::new();
    let columns: Vec<Column> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let name = row.first().and_then(as_str).unwrap_or_default().to_string();
            let described = row.get(1).and_then(as_str).unwrap_or_default();
            match types::parse(described).map(|ty| (types::slot(&ty), ty)) {
                Some((Ok(slot), ty)) => Column { described: Some(ty.to_string()), ..Column::new(name, slot.ty, slot.nullable) },
                Some((Err(types::Unread(unread_name)), _)) => {
                    // What makes a value of the type one Reylo reads.
                    let instead = match unread_name.as_str() {
                        "JSON" | "Dynamic" | "Object" => "toJSONString",
                        "AggregateFunction" => "finalizeAggregation",
                        _ => "toString",
                    };
                    unread.push(whole(sql, format!("Column {} ({name}) has the type {described}, and Reylo does not read a ClickHouse {unread_name} yet: select it as a type it reads, as {instead} gives.", i + 1), 10, None));
                    Column::new(name, Type::unknown(), true)
                }
                None => {
                    unread.push(whole(sql, format!("Column {} ({name}) has the type {described}, whose name Reylo cannot read.", i + 1), 10, None));
                    Column::new(name, Type::unknown(), true)
                }
            }
        })
        .collect();
    if !unread.is_empty() {
        return refuse(unread);
    }
    // How many rows: what the tree proves, where the session's settings leave an aggregate its row.
    // A limit a parameter gives is the caller's: the tree shows it folded from the value it was
    // read with, which is no number the statement fixes.
    let lines: Vec<&str> = tree.as_ref().map(|rows| rows.iter().filter_map(|row| row.first().and_then(as_str)).flat_map(str::lines).collect()).unwrap_or_default();
    // What a result in rows cannot carry, or a setting of the statement's own would write otherwise.
    if let Some(message) = crate::tree::unread(&lines) {
        return refuse(vec![whole(sql, message, 10, None)]);
    }
    let proven = crate::tree::rows(&lines);
    // The session's settings, each by its name: one the server does not list is not at its default.
    let session = settings.ok().unwrap_or_default();
    let value = |name: &str| session.iter().find(|row| row.first().and_then(as_str) == Some(name)).and_then(|row| row.get(1).and_then(as_str));
    let defaults = ["empty_result_for_aggregation_by_empty_set", "limit", "offset"].iter().all(|name| value(name) == Some("0")) && value("additional_result_filter") == Some("");
    let (cardinality, reason) = if value(crate::tree::UNMERGED).is_some_and(|merge| merge != "0") {
        (Cardinality::Many, Some("the session's distributed_group_by_no_merge has each shard of a distributed table answer for itself, so nothing bounds a result"))
    } else if proven.one && defaults {
        (Cardinality::One, None)
    } else if proven.one {
        (Cardinality::AtMostOne, Some("a setting of the session (empty_result_for_aggregation_by_empty_set, limit, offset or additional_result_filter) may drop the aggregate's one row"))
    } else if proven.aggregate {
        (Cardinality::AtMostOne, Some("an aggregate with no GROUP BY has one row, which the statement's HAVING, QUALIFY, OFFSET or LIMIT may drop"))
    } else if proven.at_most_one {
        (Cardinality::AtMostOne, Some("its LIMIT of 1 bounds the rows, and nothing says one is there"))
    } else {
        (Cardinality::Many, Some("ClickHouse enforces no unique key, so only a LIMIT of 1 or an aggregate with no GROUP BY bounds a result"))
    };
    typed.columns = columns;
    typed.cardinality = cardinality;
    typed.cardinality_reason = reason.map(String::from);
    typed.read_only = true;
    // ClickHouse names a column with no alias by its expression's text, which is no name to rely on.
    let targets = targets(sql, &typed.columns);
    analysis::names(sql, Dialect::Clickhouse, &mut typed, Some(&targets), &|_| None);
    // A name ClickHouse makes of an expression holds each constant in it, a parameter's value too,
    // so it changes from run to run: the one column of a statement that returns a value goes by
    // no name, and any other needs one of its own.
    if !typed.value && typed.diagnostics.is_empty() {
        let unnamed: Vec<Diagnostic> = typed
            .columns
            .iter()
            .enumerate()
            .filter(|(i, _)| varying.get(*i) == Some(&true))
            .map(|(i, column)| {
                let name = parameters.authored(sql, &lexed, &column.name);
                whole(sql, format!("Column {} is named \"{name}\" by its expression, which holds a parameter's value, so its name changes from run to run. Give it one with `as` where the statement makes it.", i + 1), analysis::UNNAMED_COLUMN, None)
            })
            .collect();
        typed.diagnostics.extend(unnamed);
    }
    Some(typed)
}

/// The types a describe answered with, in the order of its columns.
fn types_of(rows: &[Vec<Value>]) -> Vec<String> {
    rows.iter().map(|row| row.get(1).and_then(as_str).map_or_else(String::new, |ty| types::parse(ty).map_or_else(|| ty.to_string(), |ty| ty.to_string()))).collect()
}

/// How two describes of one statement differ, the parameter at `k` written as `first` in one and
/// as `second` in the other: the first column whose type changed, or the columns' count.
fn difference(first: (&[Vec<Value>], &str), second: (&[Vec<Value>], &str)) -> String {
    let (a, b) = (types_of(first.0), types_of(second.0));
    match a.iter().zip(&b).position(|(a, b)| a != b) {
        Some(i) => {
            let name = first.0[i].first().and_then(as_str).unwrap_or_default();
            format!("column {} ({name}) has the type {} with {} and {} with {}", i + 1, a[i], first.1, b[i], second.1)
        }
        None => format!("the result has {} column(s) with {} and {} with {}", a.len(), first.1, b.len(), second.1),
    }
}

/// The parameter at `k` once among those of its name, so that one parameter is asked about once.
fn once(lexed: &Lexed, indexes: impl Iterator<Item = usize>) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    for k in indexes {
        if !out.iter().any(|&j| lexed.placeholders[j].name == lexed.placeholders[k].name) {
            out.push(k);
        }
    }
    out
}

/// The refusal of a parameter the server reads as a constant: `why` says what its value changes,
/// and `error` is the server's own, where it gave one.
fn refused(sql: &str, lexed: &Lexed, parameters: &Parameters, k: usize, why: &str, error: Option<&DbError>) -> Diagnostic {
    let p = &lexed.placeholders[k];
    let quoted = error.map(|e| format!(" ({})", parameters.authored(sql, lexed, &message_of(e)).trim_end_matches('.'))).unwrap_or_default();
    at(p, format!("ClickHouse reads {} as a constant here, and {why}. Write the value in the statement.{quoted}", &sql[p.start..p.end]), 10)
}

/// The statement as it runs, every parameter a constant.
struct Ran {
    /// What its describe answers with the values of `base`.
    rows: Rows,
    /// The values the statement describes with: each parameter's constant.
    base: Choice,
    /// The parameters the server reads as constants, each by the index of its first placeholder.
    constants: Vec<usize>,
    /// Which of the columns are named by text that changes with a parameter's value.
    varying: Vec<bool>,
}

/// The statement as it runs, or the diagnostics of one that has no one type so. `column` is its
/// describe with each parameter a column, and `first` and `second` its describes with each
/// parameter's first constant and its second.
#[allow(clippy::too_many_arguments)]
fn as_it_runs(sql: &str, lexed: &Lexed, parameters: &Parameters, body: &str, asked: &Rewritten, column: &Described, first: Described, second: Described, host: &Host) -> Lazy<Result<Ran, Vec<Diagnostic>>> {
    let describe = |choice: &Choice| own_columns(host.query(&format!("{DESCRIBE}{}\n)", parameters.text(body, lexed, choice).sql), Vec::new(), Scope::Statement));
    let ones = once(lexed, 0..lexed.placeholders.len());
    let most = parameters.constants.iter().map(Vec::len).max().unwrap_or(1);
    // The same error with a column and with a constant, each stand-in read back as its parameter,
    // is the statement's own, whatever its parameters are.
    if let (Err(as_columns), Err(as_constants)) = (column, &first) {
        if as_columns.code == as_constants.code && parameters.authored(sql, lexed, &message_of(as_columns)) == parameters.authored(sql, lexed, &message_of(as_constants)) {
            return Some(Err(vec![server_error(sql, lexed, parameters, asked, DESCRIBE, as_columns)]));
        }
    }
    // Every describe of the statement as it runs, each with the values it was asked with.
    let mut tried: Vec<(Choice, Described)> = vec![(parameters.joint(0), first), (parameters.joint(1), second)];
    // Where it fails with the first two values of each parameter, a value was computed with: each
    // later value of every parameter at once, which finds what one kind of text describes.
    if tried.iter().all(|(_, answer)| answer.is_err()) {
        let later: Vec<_> = (2..most).map(|j| parameters.joint(j)).map(|choice| (describe(&choice), choice)).collect();
        for (answer, choice) in later {
            tried.push((choice, answer?));
        }
    }
    // Then one parameter changed at a time, keeping a change the statement fails otherwise with:
    // a zone here and a date there.
    if tried.iter().all(|(_, answer)| answer.is_err()) {
        let mut current = parameters.joint(0);
        let mut code = tried[0].1.as_ref().err().and_then(|e| e.code.clone());
        for _ in 0..ones.len() {
            let changes: Vec<Choice> = ones.iter().flat_map(|&k| (0..parameters.constants[k].len()).map(move |j| (k, j))).map(|(k, j)| parameters.with(lexed, &current, k, Some(j))).filter(|choice| *choice != current).collect();
            let asked: Vec<_> = changes.into_iter().map(|choice| (describe(&choice), choice)).collect();
            let mut answers = Vec::with_capacity(asked.len());
            for (answer, choice) in asked {
                answers.push((choice, answer?));
            }
            let described = answers.iter().any(|(_, answer)| answer.is_ok());
            let moved = answers.iter().find_map(|(choice, answer)| answer.as_ref().err().filter(|e| e.code != code).map(|e| (choice.clone(), e.code.clone())));
            tried.extend(answers);
            match moved {
                Some((choice, other)) if !described => {
                    current = choice;
                    code = other;
                }
                _ => break,
            }
        }
    }
    let Some((base, rows)) = tried.iter().find_map(|(choice, answer)| answer.as_ref().ok().map(|rows| (choice.clone(), rows.clone()))) else {
        let Err(error) = &tried[0].1 else { unreachable!("no describe answered") };
        return unvalued(sql, lexed, parameters, body, column, error, host).map(Err);
    };
    let types = types_of(&rows);

    // The parameters the server reads as constants: where the statement is refused with its
    // parameters columns, each one it is refused with as a column alone, or all of them where no
    // one is at fault alone.
    let values: Vec<usize> = ones.iter().copied().filter(|&k| parameters.positions[k] == Position::Value).collect();
    let constants = if column.is_ok() || values.is_empty() {
        Vec::new()
    } else {
        let together = describe(&parameters.columns(&base));
        let alone: Vec<_> = values.iter().map(|&k| describe(&parameters.with(lexed, &base, k, None))).collect();
        let together = together?;
        let mut found = Vec::new();
        for (&k, answer) in values.iter().zip(alone) {
            if answer?.is_err() {
                found.push(k);
            }
        }
        match together {
            Ok(_) => Vec::new(),
            Err(_) if found.is_empty() => values,
            Err(_) => found,
        }
    };
    // Each of those is tried with every value it has, alone, and with every parameter's value of
    // each number at once, since two of them may only describe together: two arrays of one length.
    if !constants.is_empty() {
        let mut more: Vec<Choice> = Vec::new();
        let singles = constants.iter().flat_map(|&k| (0..parameters.constants[k].len()).map(move |j| (k, j))).map(|(k, j)| parameters.with(lexed, &base, k, Some(j)));
        for choice in (0..most).map(|j| parameters.joint(j)).chain(singles) {
            if !more.contains(&choice) && !tried.iter().any(|(other, _)| *other == choice) {
                more.push(choice);
            }
        }
        let asked: Vec<_> = more.into_iter().map(|choice| (describe(&choice), choice)).collect();
        for (answer, choice) in asked {
            tried.push((choice, answer?));
        }
    }

    // A describe with other types shows a value deciding one: the parameter's, where one alone changed.
    let changed = |choice: &Choice| ones.iter().copied().filter(|&k| parameters.differs(k, choice, &base)).collect::<Vec<usize>>();
    let decides = |k: usize, choice: &Choice, other: &Rows| {
        let p = &lexed.placeholders[k];
        let why = difference((&rows, parameters.shown(k, &base)), (other, parameters.shown(k, choice)));
        if constants.contains(&k) {
            refused(sql, lexed, parameters, k, &format!("the result's types change with its value: {why}"), None)
        } else {
            at(p, format!("The result's types change with the value of {}: {why}. Write the value in the statement.", &sql[p.start..p.end]), 10)
        }
    };
    let differing: Vec<(&Choice, &Rows)> = tried.iter().filter_map(|(choice, answer)| answer.as_ref().ok().filter(|other| types_of(other) != types).map(|other| (choice, other))).collect();
    if let Some(&(choice, other)) = differing.first() {
        let mut out: Vec<Diagnostic> = Vec::new();
        let mut blamed: Vec<usize> = Vec::new();
        for &(choice, other) in &differing {
            if let [k] = changed(choice)[..] {
                if !blamed.contains(&k) {
                    blamed.push(k);
                    out.push(decides(k, choice, other));
                }
            }
        }
        if out.is_empty() {
            // Several changed at once: each alone, with the value it had there.
            let alone: Vec<_> = changed(choice).into_iter().map(|k| parameters.with(lexed, &base, k, choice[k])).map(|single| (describe(&single), single)).collect();
            for (answer, single) in alone {
                if let (Ok(with_other), [k]) = (answer?, &changed(&single)[..]) {
                    if types_of(&with_other) != types {
                        out.push(decides(*k, &single, &with_other));
                    }
                }
            }
        }
        if out.is_empty() {
            out.push(whole(sql, format!("The result's types change with the values of the statement's parameters: {}. Write the values in the statement.", difference((&rows, "one value of each"), (other, "another"))), 10, None));
        }
        return Some(Err(out));
    }
    // And a parameter the server reads as a constant holds its types only where another value of
    // it describes with them. A value the statement fails with says nothing of its types.
    let mut unshown = Vec::new();
    for &k in &constants {
        if tried.iter().any(|(choice, answer)| answer.is_ok() && parameters.differs(k, choice, &base)) {
            continue;
        }
        if parameters.constants[k].len() < 2 {
            unshown.push(refused(sql, lexed, parameters, k, &format!("Reylo has one value of the type {} to describe the statement with, which cannot show that the result's types hold for every value", parameters.types[k]), None));
            continue;
        }
        let failed = tried.iter().find_map(|(choice, answer)| answer.as_ref().err().filter(|_| changed(choice) == [k]).map(|e| (parameters.shown(k, choice), e)));
        let (shown, error) = match failed {
            Some((shown, e)) => (shown, Some(e)),
            None => (parameters.constant(k, 1).1.as_str(), None),
        };
        unshown.push(refused(sql, lexed, parameters, k, &format!("the statement fails with each other value tried in its place, as with {shown}, so nothing shows that the result's types hold for every value"), error));
    }
    if !unshown.is_empty() {
        return Some(Err(unshown));
    }

    // A column's name that another value changes, or that holds the text standing for a parameter.
    let name = |rows: &Rows, i: usize| rows.get(i).and_then(|row| row.first()).and_then(as_str).unwrap_or_default().to_string();
    let varying = (0..rows.len())
        .map(|i| {
            let own = name(&rows, i);
            tried.iter().any(|(_, answer)| matches!(answer, Ok(other) if other.len() == rows.len() && name(other, i) != own)) || ones.iter().any(|&k| base[k].is_some_and(|j| own.contains(parameters.constant(k, j).0.as_str())))
        })
        .collect();
    Some(Ok(Ran { rows, base, constants, varying }))
}

/// Why the statement describes with no value tried for its parameters: the diagnostics of each
/// parameter at fault, found by writing it alone as its first constant among columns. The server
/// computes with the one the statement then fails with, or fails otherwise with than as columns;
/// where none is at fault alone, the error is the statement's, as it fails with each first value.
fn unvalued(sql: &str, lexed: &Lexed, parameters: &Parameters, body: &str, column: &Described, error: &DbError, host: &Host) -> Lazy<Vec<Diagnostic>> {
    let ones = once(lexed, 0..lexed.placeholders.len());
    let first = parameters.joint(0);
    let columns = parameters.columns(&first);
    let alone: Vec<_> = ones.iter().map(|&k| host.query(&format!("{DESCRIBE}{}\n)", parameters.text(body, lexed, &parameters.with(lexed, &columns, k, Some(0))).sql), Vec::new(), Scope::Statement)).collect();
    let mut out = Vec::new();
    for (&k, answer) in ones.iter().zip(alone) {
        let p = &lexed.placeholders[k];
        let shown = &parameters.constant(k, 0).1;
        match (column, answer?) {
            // A column there, and a value the statement converts as it is read: a text that is no date.
            (Ok(_), Err(e)) => {
                let quoted = parameters.authored(sql, lexed, &message_of(&e));
                out.push(at(p, format!("ClickHouse computes with {} as it reads the statement, and the statement fails with each value tried in its place, as with {shown}. Declare the parameter as the type the statement converts it to, or write the value in the statement. ({})", &sql[p.start..p.end], quoted.trim_end_matches('.')), 10));
            }
            // A constant there alone, and one that names what the statement reads: a type's name.
            (Err(as_columns), Err(e)) if e.code != as_columns.code => out.push(refused(sql, lexed, parameters, k, &format!("the statement fails with each value tried in its place, as with {shown}, so its value is part of what the statement says"), Some(&e))),
            _ => {}
        }
    }
    if out.is_empty() {
        out.push(server_error(sql, lexed, parameters, &parameters.text(body, lexed, &first), DESCRIBE, error));
    }
    Some(out)
}

/// The rows of an `INSERT … VALUES` that are each checked: a statement of more is checked by its first.
const ROWS_CHECKED: usize = 32;

/// Where the refusal of a value a write computes points.
#[derive(Clone, Copy)]
enum Spot {
    /// At the one parameter the value holds.
    Parameter(usize),
    /// At the value, from where it starts to where it ends: it holds several parameters, or a
    /// name one goes by.
    Value(usize, usize),
    /// At the statement, whose text does not say which of its values it is.
    Statement,
}

/// A value a write gives a column that depends on a parameter.
struct Held {
    /// The column, by its name and its type, where the statement and its table say it.
    column: Option<(String, String)>,
    /// The value's type: the one its parameter declares, or the one the server describes it with.
    ty: Option<Named>,
    /// The parameter the value is, where it is one alone.
    alone: Option<usize>,
    at: Spot,
    /// Whether the type is the value's whatever its parameters hold: not so where the server
    /// read a parameter as a constant and no conversion in the statement names the type.
    fixed: bool,
    /// Whether that is so by a conversion in the statement alone, where the server read a
    /// parameter as a constant.
    converted: bool,
    /// The type of each parameter the value is computed from, where the server's tree says each
    /// and the server read none of them as a constant.
    on: Option<Vec<Named>>,
    /// What the value is computed from, where arithmetic alone computes it.
    operands: Option<Operands>,
    /// The values the value is one of, where it is a choice between values.
    choices: Option<Vec<Pick>>,
}

/// What the server says of one value a write gives a column.
#[derive(Clone, PartialEq)]
struct Told {
    ty: String,
    /// The parameters the value depends on.
    flow: Flow,
    /// What it is computed from, where arithmetic alone computes it.
    operands: Option<Operands>,
    /// The values it is one of, where it is a choice between values.
    choices: Option<Vec<Pick>>,
}

/// The analysis of a write, which returns no rows. Any write is parsed. An INSERT is built, and
/// not run, by `EXPLAIN PIPELINE`, which checks its table and columns and the SELECT it reads;
/// the server builds an `INSERT … VALUES` without reading its rows, so each row is built as the
/// SELECT of its values. A DELETE or an UPDATE is described as the SELECT it reads as
/// ([`as_select`]), which checks its table, the rows it names and the values it writes, and one
/// that does not read as a SELECT is refused, since nothing would check it. A write returns no
/// types for a parameter's value to change, so it stands where the server takes it with its
/// parameters columns, or constants of any of the values tried: a constant is computed with as
/// the statement is read.
///
/// A value that depends on a parameter has to have a type its column keeps every value of
/// ([`types::kept`]): ClickHouse converts what a statement writes to the column's type, and
/// changes a value the column cannot hold without an error. The server says which values depend
/// on one and what type each has: the values are described as a SELECT's columns, and the
/// SELECT's tree shows where each comes from ([`crate::tree::flows`]). An INSERT's SELECT is that
/// SELECT, an UPDATE's is the list of the values it assigns, and the values of VALUES that
/// compute with a parameter are one's list, a parameter alone having the type it declares. The
/// column is the table's own, by its description: a name in the statement may be an alias's.
/// A value whose column the text does not say, or whose type the server does not, is refused. A
/// value of constants and columns alone is ClickHouse's to convert, and so is one the statement
/// converts itself, whose type is the one it names.
///
/// A number of a type the column does not keep still stands where no declaration would change
/// that: arithmetic operators alone compute it, from parameters, columns and constants whose
/// types the column each keeps ([`crate::tree::operands`]), and it is a number of the column's
/// kind ([`types::of_one_arithmetic`]). What the column is given is then ClickHouse's arithmetic
/// on values the column holds: a sum is of a wider type than what it adds. A value anything else
/// computes is not such a number, one a parameter only chooses or a hash, and neither is one of
/// another kind, a float in a column of whole numbers, nor one that may be NULL where the column
/// holds none. And a text that is one of an enum's labels whichever its parameters choose, an
/// `if` between two of them, is the label ([`crate::tree::choices`]).
///
/// Where the server takes the statement only with a parameter a constant, it has read that
/// parameter's value, and a type may be another with another value, one no value tried here
/// shows: a tuple's element by its number. There, a value that depends on a parameter has to be
/// a conversion to a type the statement names ([`crate::tree::conversions`]).
fn write(sql: &str, lexed: &Lexed, parameters: &Parameters, body: &str, asked: &Rewritten, mut typed: Analysis, host: &Host) -> Lazy<Analysis> {
    let refuse = |d: Vec<Diagnostic>| Some(analysis::bare(sql, lexed, Dialect::Clickhouse, d));
    let insert = typed.kind == Some(StatementKind::Insert);
    let code = code(body, lexed);
    // What an INSERT writes, as far as the text says.
    let written = if insert { inserted(body, lexed, &code) } else { None };
    // An insert of rows in a format reads them from the request's body, which a parameter is not.
    if let Some(start) = written.as_ref().and_then(|w| w.format) {
        return refuse(vec![Diagnostic { message: "An INSERT … FORMAT takes its rows from the request's body, which a typed statement does not have: write the rows as VALUES, or as a SELECT, with parameters.".into(), start, length: "format".len(), code: 10, server_code: None }]);
    }
    // A partition's id is a string of the statement's text, where the server takes a parameter
    // alone and nothing a check can write for one.
    let names_partition = |p: &&Placeholder| {
        let (id, start) = word_before(body, code_before(body, lexed, p.start));
        id == "id" && word_before(body, code_before(body, lexed, start)).0 == "partition"
    };
    if let Some(p) = lexed.placeholders.iter().find(names_partition) {
        return refuse(vec![at(p, format!("Reylo types no parameter as a partition's id, which the server reads from the statement's text where a write is checked. Write the id in the statement in place of {}, or name the partition by its key's value, as IN PARTITION {{day:Date}} does.", &sql[p.start..p.end]), 10)]);
    }
    // The server's tree of a statement writes each name as it is, and what follows a line break
    // in one would read as a line of the tree's own. An escape may stand for one, and the engine
    // reads a name's escapes no further than a backslash's and a quote's.
    let unread = |&&(start, end): &&(usize, usize)| {
        let b = body.as_bytes();
        if !matches!(b.get(start), Some(b'`' | b'"')) || end > b.len() {
            return false;
        }
        let mut i = start + 1;
        while i + 1 < end {
            match b[i] {
                c if c < b' ' => return true,
                b'\\' if !matches!(b[i + 1], b'\\' | b'`' | b'"' | b'\'') => return true,
                b'\\' => i += 2,
                _ => i += 1,
            }
        }
        false
    };
    if let Some(&(start, end)) = lexed.opaque.iter().find(unread) {
        return refuse(vec![Diagnostic { message: "This name holds a control character or an escape, and the server's description of the statement writes a name as it is, a line break included, so Reylo cannot read what the statement writes. Write the name without it.".into(), start, length: end - start, code: 10, server_code: None }]);
    }
    let ask = |text: String| host.query(&text, Vec::new(), Scope::Statement);
    let most = parameters.constants.iter().map(Vec::len).max().unwrap_or(1);
    let first = parameters.joint(0);
    let columns = parameters.columns(&first);
    let select = if insert { None } else { as_select(body, lexed, &code, true) };
    let values = written.as_ref().and_then(|w| w.rows.as_ref());
    // What the server is asked of the statement with its parameters written one way: of an
    // INSERT … VALUES, each of `rows` as the SELECT of its values, built into the table, or, where
    // `by_column` says VALUES reads a value by its column's type, described alone, and then the
    // statement whole, for its table and its columns.
    let questions = |choice: &Choice, rows: &[(usize, usize)], by_column: bool| -> Vec<String> {
        let text = parameters.text(body, lexed, choice);
        let row = |&(start, end): &(usize, usize)| &text.sql[moved(&text, start)..moved(&text, end)];
        match (&select, values) {
            (_, Some(_)) if !rows.is_empty() && by_column => rows.iter().map(|at| format!("{DESCRIBE}SELECT {}\n)", row(at))).chain([format!("{PIPELINE}{}", text.sql)]).collect(),
            (_, Some((values, _))) if !rows.is_empty() => rows.iter().map(|at| format!("{PIPELINE}{}SELECT {}", &text.sql[..moved(&text, *values)], row(at))).collect(),
            _ if insert => vec![format!("{PIPELINE}{}", text.sql)],
            (Some((edits, _)), _) => vec![format!("{DESCRIBE}{}\n)", edited(&text, edits))],
            (None, _) => Vec::new(),
        }
    };
    let asked_as = |choice: &Choice, rows: &[(usize, usize)], by_column: bool| -> Vec<Lazy<Described>> { questions(choice, rows, by_column).into_iter().map(ask).collect() };
    // The values the write gives its columns, as a SELECT's columns: an INSERT's own SELECT,
    // from where it starts, where it holds a parameter;
    let select_from = written.as_ref().and_then(|w| w.select).filter(|&start| lexed.placeholders.iter().any(|p| p.start >= start));
    // the values of VALUES that compute with a parameter, each by its place in its row, where it
    // starts and where it ends;
    let mut computed: Vec<(usize, usize, usize)> = Vec::new();
    for &(place, _, start, end) in written.iter().flat_map(|w| &w.valued) {
        if computed.last() != Some(&(place, start, end)) && alone(body, lexed, start, end).is_none() {
            computed.push((place, start, end));
        }
    }
    // and the values an UPDATE assigns, each as it is.
    let assigned = if insert || lexed.placeholders.is_empty() { None } else { as_select(body, lexed, &code, false).filter(|(_, assignments)| !assignments.is_empty()) };
    let updated_table = if assigned.is_some() { updated(body, lexed, &code) } else { None };
    // An `UPDATE … SET`, which ClickHouse reads otherwise than an `ALTER TABLE … UPDATE`.
    let lightweight = words(body, lexed).first().is_some_and(|(_, word)| word == "update");
    // The SETTINGS clause that ends it: the last such word outside every parenthesis that a
    // setting's name and its `=` follow, which a column named as the clause is does not have.
    let update_settings = top_words(body, lexed, &code).into_iter().rev().map(|(at, word)| (at, word)).find(|(at, word)| {
        let name = code_after(body, lexed, at + word.len());
        word == "settings" && name_length(&body[name..]).is_some_and(|length| body.as_bytes().get(code_after(body, lexed, name + length)) == Some(&b'='))
    });
    let update_settings = update_settings.map(|(at, _)| at).filter(|_| lightweight);
    let flowing = |choice: &Choice| -> Option<String> {
        let text = parameters.text(body, lexed, choice);
        // The settings an INSERT names before its rows are the rows' too: a setting may change a value's type.
        let settings = written.as_ref().and_then(|w| w.settings).map(|(start, end)| text.sql[moved(&text, start)..moved(&text, end)].trim().to_string());
        if let Some(start) = select_from {
            let select = &text.sql[moved(&text, start)..];
            Some(match settings {
                Some(settings) => format!("SELECT * FROM (\n{select}\n) {settings}"),
                None => select.to_string(),
            })
        } else if !computed.is_empty() {
            // Each value in parentheses on lines of its own, which a comment at its end leaves it,
            // and under no name of the engine's: a name over a value takes the place of its own.
            let listed: Vec<String> = computed.iter().map(|&(_, start, end)| format!("(\n{}\n)", &text.sql[moved(&text, start)..moved(&text, end)])).collect();
            Some(format!("SELECT\n{}{}", listed.join(",\n"), settings.map(|settings| format!("\n{settings}")).unwrap_or_default()))
        } else {
            let (edits, assignments) = assigned.as_ref()?;
            Some(match updated_table.as_ref().filter(|_| lightweight) {
                // A lightweight UPDATE's values see the names of its other values and of its table,
                // and none its WHERE gives: they are described without it.
                Some(table) => {
                    let listed: Vec<String> = assignments.iter().map(|&(_, equals, cut)| format!("(\n{}\n)", &text.sql[moved(&text, equals + 1)..moved(&text, cut)])).collect();
                    // Its own settings are the values' too, from the word to the statement's end.
                    let settings = update_settings.map(|at| format!("\n{}", &text.sql[moved(&text, at)..])).unwrap_or_default();
                    format!("SELECT\n{}\nFROM {table}{settings}", listed.join(",\n"))
                }
                None => edited(&text, edits),
            })
        }
    };
    // What the server is asked of that SELECT: its columns' types, and its tree, which says the
    // columns that depend on a parameter, and on which.
    let flow = |choice: &Choice| -> Option<(Lazy<Described>, Lazy<Described>)> {
        let select = flowing(choice)?;
        Some((own_columns(ask(format!("{DESCRIBE}{select}\n)"))), ask(format!("{TREE}{select}"))))
    };
    let flowed = flow(&columns);
    // The session's settings that change what a conversion gives: asked where one may bear on a value.
    let conversions = (!lexed.placeholders.is_empty()).then(|| host.query(CONVERSION_SETTINGS, Vec::new(), Scope::Session));
    let parsed = ask(format!("{AST}{}", asked.sql));
    // The table's columns: of an INSERT … VALUES, whose rows VALUES reads by them, and of any
    // write that may give a column a value that depends on a parameter.
    let table = written.as_ref().and_then(|w| w.table.as_ref()).filter(|_| values.is_some() || select_from.is_some()).or(updated_table.as_ref());
    let table = table.map(|table| own_columns(ask(format!("DESCRIBE TABLE {table}"))));
    // Whether the text reads as the SELECT it is checked as.
    let reads = select.as_ref().map(|(edits, _)| ask(format!("{AST}{}", edited(&parameters.text(body, lexed, &columns), edits))));
    // Asked with the rest, where which rows are asked does not wait on the table's columns.
    let early = values.is_none().then(|| (asked_as(&columns, &[], false), asked_as(&first, &[], false)));
    let parsed = match parsed? {
        Ok(rows) => rows,
        Err(e) => return refuse(vec![server_error(sql, lexed, parameters, asked, AST, &e)]),
    };
    // An ALTER's commands, as the server's parse names them: one that changes no rows changes the
    // table, or a part of it on disk, which is no statement of an application's data.
    if typed.kind == Some(StatementKind::Other) {
        let lines: Vec<&str> = parsed.iter().filter_map(|row| row.first().and_then(as_str)).flat_map(str::lines).collect();
        let commands: Vec<&str> = lines.iter().filter_map(|line| line.trim_start().strip_prefix("AlterCommand ")).map(|rest| rest.split_whitespace().next().unwrap_or_default()).collect();
        let other = commands.iter().find(|command| !matches!(**command, "UPDATE" | "DELETE"));
        if !lines.first().is_some_and(|line| line.starts_with("AlterQuery")) || commands.is_empty() || other.is_some() {
            let what = other.map_or_else(|| "this one is none".to_string(), |command| format!("{} is neither", command.replace('_', " ")));
            return refuse(vec![whole(sql, lex::refused_command(&format!("Reylo types an ALTER TABLE that changes rows, with UPDATE or DELETE, and {what}")), 10, None)]);
        }
        // Each command is checked as a SELECT of its own, which several in one statement are not.
        if commands.len() > 1 {
            return refuse(vec![whole(sql, format!("Reylo types an ALTER TABLE of one command, an UPDATE or a DELETE, and this one has {}: write each as a statement of its own.", commands.len()), 10, None)]);
        }
    }
    if !insert {
        let unread = |why: String| refuse(vec![whole(sql, format!("Reylo checks a DELETE or an UPDATE as the SELECT of the rows it changes and the values it writes, and it cannot read this one as that SELECT{why}, so nothing would check it. Write it as DELETE FROM t WHERE …, UPDATE t SET … WHERE … or ALTER TABLE t UPDATE … WHERE …."), 10, None)]);
        match reads {
            None => return unread(String::new()),
            Some(answer) => {
                if let Err(e) = answer? {
                    return unread(format!(" ({})", message_of(&e).trim_end_matches('.')));
                }
            }
        }
    }
    // An INSERT whose rows the text does not show holds parameters nothing would hold to their columns.
    if insert && written.is_none() && !lexed.placeholders.is_empty() {
        return refuse(vec![whole(sql, "Reylo cannot read what this INSERT writes, so nothing would hold its parameters to their columns. Write its rows as VALUES, or as a SELECT.", 10, None)]);
    }
    let described_table: Option<Described> = match table {
        Some(answer) => Some(answer?),
        None => None,
    };
    // A table the server does not describe is its error to give, which the statement's own check does.
    let unknown_table = matches!(described_table, Some(Err(_)));
    let of_table: Option<Rows> = described_table.and_then(Result::ok);
    // A table function's rows go where no description says the columns.
    let function = written.as_ref().is_some_and(|w| w.table.is_none());
    let text = |row: &Vec<Value>, i: usize| row.get(i).and_then(as_str).unwrap_or_default().to_string();
    let named = |name: &String| of_table.as_ref()?.iter().find(|row| text(row, 0) == *name).map(|row| (text(row, 0), text(row, 1)));
    // The columns an INSERT writes, in the order of its values, each by its name and its type:
    // none where the text and the table's description do not say them.
    let into: Option<Vec<(String, String)>> = written.as_ref().and_then(|w| match &w.listed {
        Listed::Names(names) => names.iter().map(|name| named(name)).collect(),
        // With no list, every column but the ones the table computes itself, in its order: a
        // description without each column's kind, as a session's describe_compact_output gives, says no order.
        Listed::Table => of_table.as_ref().filter(|rows| rows.iter().all(|row| row.len() > 2)).map(|rows| rows.iter().filter(|row| !matches!(text(row, 2).as_str(), "MATERIALIZED" | "ALIAS" | "EPHEMERAL")).map(|row| (text(row, 0), text(row, 1))).collect()),
        Listed::Unread => None,
    });
    let holds = |described: &str, names: &[&str]| types::parse(described).is_some_and(|ty| names.iter().any(|name| ty.holds(name)));
    // VALUES reads a value for a column that holds a Dynamic by its text where the value is a
    // parameter alone or opens with a parenthesis or a bracket, and stores a placeholder's own
    // text for the parameter's value: `({d:UInt64} + 1)` is kept as that text. Where the text
    // does not say the value's column, any column of the table that holds one may be it.
    if let Some(w) = written.as_ref().filter(|w| w.rows.is_some() && !function && !unknown_table) {
        let anywhere = of_table.as_ref().is_none_or(|rows| rows.iter().any(|row| holds(&text(row, 1), &["Dynamic"])));
        let column = |place: usize| match &into {
            Some(into) => into.get(place).filter(|(_, described)| holds(described, &["Dynamic"])).map(|column| Some(column.clone())),
            None => anywhere.then_some(None),
        };
        let mut stored: Vec<Diagnostic> = w.valued.iter().filter(|&&(_, _, start, end)| read_as_text(body, lexed, &code, start, end)).filter_map(|&(place, k, _, _)| column(place).map(|column| not_computed(sql, lexed, k, column.as_ref()))).collect();
        if !stored.is_empty() {
            stored.sort_by_key(|d| d.start);
            return refuse(stored);
        }
    }
    // What the server says of the values written with the parameters written one way: each
    // one's type, the parameters it depends on and the values it chooses between, and, of a
    // `whole` answer, whether it is a conversion to a type the statement names. None where the
    // server refuses the SELECT, or answers with what the engine does not read.
    type Flows = (Vec<Told>, Option<Vec<bool>>);
    let read = |choice: &Choice, described: &Described, tree: &Described, whole: bool| -> Option<Flows> {
        let rows = described.as_ref().ok()?;
        // A column's name with a line break in it parts a line of the tree.
        if rows.iter().any(|row| text(row, 0).chars().any(char::is_control)) {
            return None;
        }
        let lines: Option<Vec<&str>> = match tree {
            Ok(tree) => Some(tree.iter().filter_map(|row| row.first().and_then(as_str)).flat_map(str::lines).filter(|line| !line.trim().is_empty()).collect()),
            // Each value of VALUES asked of is one the text says depends on a parameter, so a
            // server that gives no tree leaves it held by its own type alone.
            Err(_) if !computed.is_empty() && !whole => None,
            Err(_) => return None,
        };
        // A parameter written as a constant is a conversion to its type, as the tree names it.
        let constants: Vec<String> = (0..choice.len()).filter(|&k| choice[k].is_some()).map(|k| parameters.types[k].to_string()).collect();
        let constant = |ty: &str| types::parse(ty).is_none_or(|ty| constants.contains(&ty.to_string()));
        let told = lines.as_ref().and_then(|lines| crate::tree::flows(lines, &constant));
        let flows: Vec<Flow> = if computed.is_empty() {
            told?
        } else {
            // Held whatever the tree says of it, and by its own type alone where the tree says no parameter's.
            let held = Flow { of: Vec::new(), unknown: true };
            match told.filter(|flows| flows.len() == rows.len()) {
                Some(flows) => flows.into_iter().map(|flow| if flow.depends() { flow } else { held.clone() }).collect(),
                None => vec![held; rows.len()],
            }
        };
        let converted = match (whole, &lines) {
            (true, Some(lines)) => Some(crate::tree::conversions(lines).filter(|converted| converted.len() == rows.len())?),
            (true, None) => return None,
            (false, _) => None,
        };
        let chosen = lines.as_ref().map(|lines| crate::tree::choices(lines)).filter(|chosen| chosen.len() == rows.len()).unwrap_or_else(|| vec![None; rows.len()]);
        let computed_from = lines.as_ref().map(|lines| crate::tree::operands(lines, &constant)).filter(|operands| operands.len() == rows.len()).unwrap_or_else(|| vec![None; rows.len()]);
        (flows.len() == rows.len()).then(|| (rows.iter().zip(flows).zip(chosen).zip(computed_from).map(|(((row, flow), choices), operands)| Told { ty: text(row, 1), flow, operands, choices }).collect(), converted))
    };
    // Why the server's answer says nothing, where it does not.
    let mut untyped: Option<Option<DbError>> = None;
    let mut answers: Vec<Flows> = Vec::new();
    if let Some((described, tree)) = flowed {
        let (described, tree) = (described?, tree?);
        match read(&columns, &described, &tree, false) {
            Some(found) => answers.push(found),
            None => {
                // The server takes a parameter as a constant alone: the SELECT is asked with each
                // constant tried, of every parameter at once, and each answer is held.
                let asks: Vec<(Choice, Option<(Lazy<Described>, Lazy<Described>)>)> = (0..most)
                    .map(|j| {
                        let choice = parameters.joint(j);
                        let asked = flow(&choice);
                        (choice, asked)
                    })
                    .collect();
                for (choice, asked) in asks {
                    let Some((described, tree)) = asked else { continue };
                    let (described, tree) = (described?, tree?);
                    if let Some(answer) = read(&choice, &described, &tree, true).filter(|answer| !answers.contains(answer)) {
                        answers.push(answer);
                    }
                }
                if answers.is_empty() {
                    untyped = Some(described.err().or(tree.err()));
                }
            }
        }
    }
    // Each value that depends on a parameter, and the column it is written to: none where the
    // text and the table's description do not say it.
    let mut held: Vec<Held> = Vec::new();
    // A value past the last column is one too many, which the server's own check says.
    let column_at = |place: usize| -> Option<Option<(String, String)>> {
        match &into {
            Some(into) => into.get(place).map(|column| Some(column.clone())),
            None => Some(None),
        }
    };
    // A parameter alone in a row of VALUES has the type it declares.
    for &(place, k) in written.iter().flat_map(|w| &w.written) {
        held.extend(column_at(place).map(|column| Held { column, ty: Some(parameters.types[k].clone()), alone: Some(k), at: Spot::Parameter(k), fixed: true, converted: false, on: None, operands: None, choices: None }));
    }
    // The parameters a stretch of the statement's text holds.
    let inside = |start: usize, end: usize| -> Vec<usize> { (0..lexed.placeholders.len()).filter(|&k| start <= lexed.placeholders[k].start && lexed.placeholders[k].end <= end).collect() };
    // Where a value's refusal points: at its one parameter, or at the value.
    let spot = |start: usize, end: usize| {
        match inside(start, end)[..] {
            [k] => Spot::Parameter(k),
            _ => Spot::Value(code_after(body, lexed, start), text_end(body, lexed, &code, start, end)),
        }
    };
    for (flows, converted) in &answers {
        let fixed = |i: usize| converted.as_ref().is_none_or(|converted| converted[i]);
        let by_conversion = converted.is_some();
        // `written` is the stretch of the text that is the value, where the text says it: each
        // parameter it holds is one the value is computed from, whatever the tree says of it.
        let value = |column: Option<(String, String)>, told: &Told, alone: Option<usize>, at: Spot, i: usize, written: Option<(usize, usize)>| Held {
            column,
            ty: types::parse(&told.ty),
            alone,
            at,
            fixed: fixed(i),
            converted: by_conversion && fixed(i),
            // A parameter the server read as a constant may have decided more than a value.
            on: if by_conversion || told.flow.unknown {
                None
            } else {
                let of: Option<Vec<Named>> = told.flow.of.iter().map(|ty| types::parse(ty)).collect();
                of.map(|of| of.into_iter().chain(written.iter().flat_map(|&(start, end)| inside(start, end)).map(|k| parameters.types[k].clone())).collect())
            },
            operands: told.operands.clone(),
            choices: told.choices.clone(),
        };
        if let Some(start) = select_from {
            let at = match spot(start, body.len()) {
                Spot::Parameter(k) => Spot::Parameter(k),
                _ => Spot::Statement,
            };
            for (place, told) in flows.iter().enumerate().filter(|(_, told)| told.flow.depends()) {
                held.extend(column_at(place).map(|column| value(column, told, None, at, place, None)));
            }
        } else if !computed.is_empty() {
            if flows.len() != computed.len() {
                untyped = Some(None);
            }
            for (i, (&(place, start, end), told)) in computed.iter().zip(flows).enumerate() {
                held.extend(column_at(place).map(|column| value(column, told, None, spot(start, end), i, Some((start, end)))));
            }
        } else if let Some((_, assignments)) = &assigned {
            if flows.len() != assignments.len() {
                untyped = Some(None);
            }
            for (i, (&(start, equals, cut), told)) in assignments.iter().zip(flows).enumerate() {
                if told.flow.depends() {
                    // The column is the table's own, by the name the assignment gives it.
                    let column = column_name(plain(body, lexed, start, equals).trim()).and_then(|name| named(&name));
                    held.push(value(column, told, alone(body, lexed, equals + 1, cut), spot(equals + 1, cut), i, Some((equals + 1, cut))));
                }
            }
        }
    }
    let typed_column = |column: &Option<(String, String)>| column.as_ref().and_then(|(name, described)| Some((name.clone(), types::parse(described)?)));
    // A text that goes to a Dynamic is the text where the server infers no type from one. A
    // session whose settings the engine cannot read may infer.
    let text_to_dynamic = |ty: &Named, column: &Named| column.holds("Dynamic") && (ty.holds("String") || ty.holds("FixedString"));
    let infers = held.iter().any(|value| matches!((typed_column(&value.column), &value.ty), (Some((_, column)), Some(ty)) if text_to_dynamic(ty, &column)));
    // And a conversion names its value's type where a CAST keeps no nullability of its own.
    let converts = held.iter().any(|value| value.converted);
    // A setting is on where the statement names it, quoted or not, whatever it sets it to, or
    // where the session has it so: one the engine cannot ask may have.
    let session: Option<Described> = match conversions.filter(|_| infers || converts) {
        Some(answer) => Some(answer?),
        None => None,
    };
    let on = |setting: &str| {
        body.contains(setting)
            || session.as_ref().is_some_and(|answer| match answer {
                Ok(rows) => rows.iter().any(|row| text(row, 0) == setting && text(row, 1) != "0"),
                Err(_) => true,
            })
    };
    let (inferred, keeps_nullable) = (infers && on(INFERS), converts && on(KEEPS_NULLABLE));
    // A value of a type the column does not keep that stands all the same, where it is of the
    // type `ty`: a number of the column's kind that arithmetic alone computes, from operands and
    // parameters whose types the column each keeps; and a text that is one of an enum's labels
    // whichever its parameters choose, each a label the statement writes or a value of an enum
    // the column keeps. Neither where the value may be NULL and the column holds none.
    let stands = |value: &Held, ty: &Named, column: &Named| {
        if types::never_null(ty).is_some() && !types::takes_null(column) {
            return false;
        }
        let kept = |of: &String| types::parse(of).is_some_and(|of| types::kept(&of, column));
        let by_arithmetic = value.operands.as_ref().is_some_and(|operands| operands.parameters.iter().chain(&operands.others).all(kept));
        let by_parameters = types::of_one_arithmetic(ty, column) && by_arithmetic && value.on.as_ref().is_some_and(|on| !on.is_empty() && on.iter().all(|of| types::kept(of, column)));
        let labels = |pick: &Pick| match pick {
            Pick::Text(text) => types::labelled(std::slice::from_ref(text), column),
            // A NULL is one the column holds, or no part of a type that has none.
            Pick::Null => true,
            Pick::Typed(of) => types::labelled(&[], column) && types::parse(of).is_some_and(|of| types::kept(&of, column)),
        };
        let by_labels = types::plain(ty).bare().name == "String" && value.choices.as_ref().is_some_and(|picks| picks.iter().all(labels));
        by_parameters || by_labels
    };
    let mut unkept: Vec<Diagnostic> = Vec::new();
    for value in &held {
        if let (Some((name, column)), Some(ty)) = (typed_column(&value.column), &value.ty) {
            if !value.fixed || (value.converted && keeps_nullable) {
                unkept.push(not_fixed(sql, lexed, value.at, &name, &column, value.fixed));
            } else if inferred && text_to_dynamic(ty, &column) {
                let to = format!("the column {name}, of the type {column}. {INFERS_TEXT}");
                unkept.push(match value.alone {
                    Some(k) => at(&lexed.placeholders[k], format!("{} is written as it is to {to}", declaration(sql, &lexed.placeholders[k]).0), 10),
                    None => refused_value(sql, lexed, value.at, Some(ty), &to),
                });
            } else if !types::kept(ty, &column) && !stands(value, ty, &column) {
                // Whether all the column does not keep of the value is its NULL.
                let null_alone = !types::takes_null(&column) && types::never_null(ty).is_some_and(|rest| types::kept(&rest, &column) || stands(value, &rest, &column));
                unkept.push(match value.alone {
                    Some(k) => not_kept(sql, lexed, parameters, k, &name, &column, insert),
                    None => not_kept_value(sql, lexed, value.at, ty, value.on.as_deref(), value.operands.as_ref(), null_alone, &name, &column, insert),
                });
            }
        }
    }
    if !unkept.is_empty() {
        unkept.sort_by_key(|d| d.start);
        unkept.dedup_by(|a, b| a.start == b.start && a.message == b.message);
        return refuse(unkept);
    }
    // The rows of VALUES asked as SELECTs. Where a column holds a Variant or a Dynamic, VALUES
    // reads a value for it by the column's type and a SELECT by the value's own: a 5 that VALUES
    // writes as the Variant's UInt64 is a UInt8 the SELECT's conversion refuses. Such a row's
    // values are described, which checks what each computes, and not built into the table.
    let by_type = |described: &str| holds(described, &["Variant", "Dynamic"]);
    let by_column = match &into {
        Some(into) => into.iter().any(|(_, described)| by_type(described)),
        None => of_table.as_ref().is_some_and(|rows| rows.iter().any(|row| by_type(&text(row, 1)))),
    };
    let rows: Vec<(usize, usize)> = values.map(|(_, rows)| rows.iter().copied().filter(|&(start, end)| reads_as_select(body, lexed, &code, start, end)).take(ROWS_CHECKED).collect()).unwrap_or_default();
    let (as_columns, as_first) = early.unwrap_or_else(|| (asked_as(&columns, &rows, by_column), asked_as(&first, &rows, by_column)));
    // Each question is one the server takes with the parameters written one way or the other.
    let mut failed: Vec<(usize, DbError)> = Vec::new();
    let count = as_columns.len();
    for (i, (one, other)) in as_columns.into_iter().zip(as_first).enumerate() {
        if let (Err(e), Err(_)) = (one?, other?) {
            failed.push((i, e));
        }
    }
    if !failed.is_empty() {
        // A value may have been computed with: each later value of every parameter at once.
        let later: Vec<Vec<Lazy<Described>>> = (1..most).map(|j| asked_as(&parameters.joint(j), &rows, by_column)).collect();
        let mut taken = vec![false; count];
        for answers in later {
            for (i, answer) in answers.into_iter().enumerate() {
                taken[i] |= answer?.is_ok();
            }
        }
        if let Some((i, e)) = failed.iter().find(|(i, _)| !taken[*i]) {
            let message = parameters.authored(sql, lexed, &message_of(e));
            return refuse(vec![match rows.get(*i) {
                // A row of VALUES, at the row: its `(` to its `)`.
                Some(&(start, end)) => Diagnostic { message, start: start - 1, length: end + 2 - start, code: 6, server_code: e.code.clone() },
                None if insert => server_error(sql, lexed, parameters, asked, PIPELINE, e),
                None => whole(sql, message, 6, e.code.clone()),
            }]);
        }
    }
    // A statement the server takes. A value it writes with a parameter is held by nothing where
    // the text does not say its column,
    let mut unplaced: Vec<Diagnostic> = held.iter().filter(|value| typed_column(&value.column).is_none()).map(|value| match value.alone {
        Some(k) => not_placed(sql, lexed, k, function),
        None => not_placed_value(sql, lexed, value.at, function),
    }).collect();
    if !unplaced.is_empty() {
        unplaced.sort_by_key(|d| d.start);
        unplaced.dedup_by(|a, b| a.start == b.start && a.message == b.message);
        return refuse(unplaced);
    }
    // or where the server does not say its type.
    if held.iter().any(|value| value.ty.is_none()) {
        untyped.get_or_insert(None);
    }
    if let Some(error) = untyped {
        let why = error.map(|e| format!(" ({})", parameters.authored(sql, lexed, &message_of(&e)).trim_end_matches('.'))).unwrap_or_default();
        return refuse(vec![whole(sql, format!("Reylo cannot read the type of a value this statement writes with a parameter{why}, so nothing would hold the value to its column."), 10, None)]);
    }
    typed.cardinality = Cardinality::None;
    Some(typed)
}

/// What ClickHouse does with a value of a type its column does not keep.
const CONVERTS: &str = "ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error.";
/// What it does with a NULL written to a column that holds none, in an INSERT or in an UPDATE.
fn nulls(insert: bool) -> &'static str {
    if insert {
        "The column holds no NULL, and an INSERT writes the column's default value in place of one, with no error unless a setting asks for one."
    } else {
        "The column holds no NULL, and an UPDATE that writes one fails."
    }
}
/// What it does with a text written to a Dynamic where its setting says to infer a type.
const INFERS_TEXT: &str = "ClickHouse reads a text written to a Dynamic as whichever type the text reads as where the setting cast_string_to_dynamic_use_inference is on, as it is for this statement, so the column would not hold the text as it was given: 123 is stored as a number. Turn the setting off, or give the value a type that is no text.";
/// What it does with a text written to a Variant of more than text.
const READS_TEXT: &str = "ClickHouse reads a text written to a Variant as whichever of its types the text reads as, so the column would not hold the text as it was given: 123 is stored as a number where the Variant has one.";
/// A column the statement's text does not name, and one of a table function, for a refusal.
const UNNAMED: &str = "a column the statement's text does not name, so nothing holds it to that column's type. Write the table and each column by its name.";
const OF_FUNCTION: &str = "a table function's column, and Reylo reads no table function's columns, so nothing holds it to that column's type. Insert into a table.";

/// A parameter as its statement writes it, the cast that declares a `:name`'s type with it, and
/// as the same statement would write it with another type.
fn declaration<'a>(sql: &'a str, p: &'a Placeholder) -> (String, impl Fn(&str) -> String + 'a) {
    let colon = sql[p.start..].starts_with(':');
    let written = match written_type(sql, p).filter(|_| colon) {
        Some(ty) => format!("{}::{ty}", &sql[p.start..p.end]),
        None => sql[p.start..p.end].to_string(),
    };
    (written, move |ty: &str| if colon { format!(":{}::{ty}", p.name) } else { format!("{{{}:{ty}}}", p.name) })
}

/// The refusal of a parameter written as it is to a column that does not keep its type's every value.
fn not_kept(sql: &str, lexed: &Lexed, parameters: &Parameters, k: usize, name: &str, column: &Named, insert: bool) -> Diagnostic {
    let p = &lexed.placeholders[k];
    let (written, declared) = declaration(sql, p);
    // The type a parameter declares for the column: the column's, without what only a column has.
    let into = types::plain(column);
    let text = matches!(types::plain(&parameters.types[k]).bare().name.as_str(), "String" | "FixedString");
    // All the column does not keep of the parameter is its NULL. Nothing has the type Nothing
    // but NULL, so it is no type a value that is never NULL has.
    let never_null = types::never_null(&parameters.types[k]).filter(|rest| rest.name != "Nothing" && !types::takes_null(column) && types::kept(rest, column));
    let why = if let Some(never_null) = never_null {
        format!("{} Declare the parameter as a type that is never NULL, {}, or write what a NULL becomes in the statement, as ifNull({written}, …) does.", nulls(insert), declared(&never_null.to_string()))
    } else if into.name == "Variant" && text {
        READS_TEXT.to_string()
    } else if into.name == "Variant" {
        format!("{CONVERTS} Declare the parameter as one of the types the column holds.")
    } else if into.holds("Variant") {
        // A Variant within an array, a tuple or a map: no parameter declares one, and a conversion to one reads a text as any of its types.
        format!("{CONVERTS} Declare the parameter as the column's type with one of the Variant's types in its place, other than a text.")
    } else if types::parameter(&into).is_err() {
        // A type no parameter declares: a JSON.
        format!("{CONVERTS} No parameter declares that type: convert the value in the statement, as CAST({written}, {}) does.", types::quoted(&into.to_string()))
    } else {
        format!("{CONVERTS} Declare the parameter as the column's type, {}.", declared(&into.to_string()))
    };
    at(p, format!("{written} is written as it is to the column {name}, of the type {column}. {why}"), 10)
}

/// The refusal of a value the statement computes with a parameter, at its spot: `value` is the
/// value's type, where the refusal names it, and `to` what the value is written to and why that
/// is refused, with `it` for the value.
fn refused_value(sql: &str, lexed: &Lexed, spot: Spot, value: Option<&Named>, to: &str) -> Diagnostic {
    let of = value.map(|ty| format!(" of the type {ty}")).unwrap_or_default();
    match spot {
        Spot::Parameter(k) => {
            let p = &lexed.placeholders[k];
            at(p, format!("{} is in a value{of} that is written to {to}", declaration(sql, p).0), 10)
        }
        Spot::Value(start, end) => Diagnostic { message: format!("This value{of} depends on a parameter and is written to {to}"), start, length: end.saturating_sub(start).max(1), code: 10, server_code: None },
        Spot::Statement => whole(sql, format!("The statement writes a value{of} that depends on a parameter to {to}"), 10, None),
    }
}

/// The refusal of a value the statement computes with a parameter, written to a column that does
/// not keep its type's every value.
///
/// `on` is the type of each parameter the value is computed from, where the server says each,
/// and `operands` what it is computed from, where arithmetic alone computes it. Declaring the
/// parameters as types the column keeps is a way out where the value's type follows theirs, and
/// a conversion in the statement the one way otherwise. `null_alone` says that all the column
/// does not keep of the value is its NULL.
fn not_kept_value(sql: &str, lexed: &Lexed, spot: Spot, value: &Named, on: Option<&[Named]>, operands: Option<&Operands>, null_alone: bool, name: &str, column: &Named, insert: bool) -> Diagnostic {
    let into = types::plain(column);
    let text = matches!(types::plain(value).bare().name.as_str(), "String" | "FixedString");
    let others = |holds: &dyn Fn(&Named) -> bool| operands.is_some_and(|operands| operands.others.iter().all(|of| types::parse(of).is_some_and(|of| holds(&of))));
    // A parameter whose type the value follows: one of the value's own type, which the value is
    // or passes on, and a number in a number of the column's kind that arithmetic computes from
    // it and from what the column keeps.
    let summed = types::of_one_arithmetic(value, column) && others(&|of| types::kept(of, column));
    let follows = |of: &Named| types::plain(of) == types::plain(value) || (summed && matches!(types::plain(of).bare(), of if of.name == "Bool" || of.is_number()));
    // Each parameter of a type the column does not keep is one a declaration changes the value with.
    let declared = on.is_some_and(|on| {
        let other: Vec<&Named> = on.iter().filter(|of| !types::kept(of, column)).collect();
        !other.is_empty() && other.into_iter().all(follows)
    });
    let why = if null_alone {
        // A NULL is a parameter's own where arithmetic alone computes the value, from nothing
        // else that may be NULL: declaring each parameter as a type that has none leaves none.
        let own = on.is_some_and(|on| on.iter().any(|of| types::never_null(of).is_some())) && others(&|of| types::never_null(of).is_none());
        let declare = if own { "Declare each parameter it is computed from as a type that is never NULL, or write" } else { "Write" };
        format!("{} {declare} what a NULL becomes in the statement, as ifNull(…, …) does.", nulls(insert))
    } else if into.name == "Variant" && text {
        READS_TEXT.to_string()
    } else if into.holds("Variant") {
        format!("{CONVERTS} Give the value one of the types the column holds, other than a text: declare its parameters so, or convert it in the statement.")
    } else if types::parameter(&into).is_err() {
        // A type no parameter declares, and one a conversion may not make: a JSON, an aggregate function's state.
        format!("{CONVERTS} Give the value the column's type in the statement: no parameter declares that type.")
    } else if declared {
        format!("{CONVERTS} Give each parameter the value is computed from a type the column keeps, or convert the value in the statement, as CAST(… AS {into}) does.")
    } else {
        format!("{CONVERTS} Convert the value in the statement, as CAST(… AS {into}) does.")
    };
    refused_value(sql, lexed, spot, Some(value), &format!("the column {name}, of the type {column}. {why}"))
}

/// The refusal of a value the statement computes with a parameter where the server read a
/// parameter as a constant, and no conversion in the statement names the value's type.
fn not_fixed(sql: &str, lexed: &Lexed, spot: Spot, name: &str, column: &Named, converted: bool) -> Diagnostic {
    let into = types::plain(column);
    let fix = if converted {
        // A conversion that names the type, where a setting has one keep its value's nullability.
        "A CAST gives a value one type only where the setting cast_keep_nullable is off, and it is on for this statement: turn it off.".to_string()
    } else {
        format!("Give the value one type in the statement, as CAST(… AS {into}) does.")
    };
    refused_value(sql, lexed, spot, None, &format!("the column {name}, of the type {column}. ClickHouse reads a parameter of this statement as a constant, and the type of a value computed with one may change with the parameter's value. {fix}"))
}

/// The refusal of a parameter written as it is where the statement's text does not say to which
/// column, or says a table function's.
fn not_placed(sql: &str, lexed: &Lexed, k: usize, function: bool) -> Diagnostic {
    let p = &lexed.placeholders[k];
    let (written, _) = declaration(sql, p);
    at(p, format!("{written} is written as it is to {}", if function { OF_FUNCTION } else { UNNAMED }), 10)
}

/// The refusal of a value the statement computes with a parameter where its text does not say
/// which column the value is written to, or says a table function's.
fn not_placed_value(sql: &str, lexed: &Lexed, spot: Spot, function: bool) -> Diagnostic {
    refused_value(sql, lexed, spot, None, if function { OF_FUNCTION } else { UNNAMED })
}

/// The refusal of a parameter in a value that VALUES may read by its text for a column that
/// holds a Dynamic: `column` is the column's name and type, where the statement's text says it.
fn not_computed(sql: &str, lexed: &Lexed, k: usize, column: Option<&(String, String)>) -> Diagnostic {
    let p = &lexed.placeholders[k];
    let (written, _) = declaration(sql, p);
    let to = column.map_or_else(|| "a column the statement's text does not name, which may hold a Dynamic".to_string(), |(name, described)| format!("the column {name}, of the type {described}"));
    at(p, format!("{written} is in a value that VALUES writes to {to}. ClickHouse's VALUES reads a value for a Dynamic by its text where the value is a parameter alone or opens with a parenthesis or a bracket, and stores the placeholder's own text in place of the parameter's value. Write the row as an INSERT … SELECT, which computes each value."), 10)
}

/// A stretch of the statement without its comments, which stand for nothing.
fn plain(body: &str, lexed: &Lexed, from: usize, to: usize) -> String {
    let mut out = String::new();
    let mut at = from;
    while at < to {
        let comment = lexed.opaque.iter().find(|&&(start, end)| start <= at && at < end && is_comment(body, start));
        let next = comment.map_or(to, |&(_, end)| end.min(to));
        let until = lexed.opaque.iter().filter(|&&(start, _)| start > at && start < to && is_comment(body, start)).map(|&(start, _)| start).min().unwrap_or(to);
        match comment {
            Some(_) => {
                out.push(' ');
                at = next;
            }
            None => {
                out.push_str(&body[at..until]);
                at = until;
            }
        }
    }
    out
}

/// The parameter a stretch of the statement is, where it is one alone: in parentheses or bare,
/// with the cast that declares a `:name`'s type, and the alias a SELECT's list gives it. A
/// comment beside it changes nothing. A cast after a `{name:Type}` is the statement's own
/// conversion of it, and what a statement converts is no parameter alone.
fn alone(body: &str, lexed: &Lexed, start: usize, end: usize) -> Option<usize> {
    let inside: Vec<usize> = (0..lexed.placeholders.len()).filter(|&k| start <= lexed.placeholders[k].start && lexed.placeholders[k].end <= end).collect();
    let [k] = inside[..] else { return None };
    let p = &lexed.placeholders[k];
    let (before, after) = (plain(body, lexed, start, p.start), plain(body, lexed, p.end, end));
    let (mut before, mut rest) = (before.trim(), after.trim());
    if body[p.start..].starts_with(':') {
        if let Some(ty) = rest.strip_prefix("::") {
            rest = ty[type_end(ty)..].trim_start();
        }
    }
    loop {
        if let (Some(outside), Some(after)) = (before.strip_suffix('('), rest.strip_prefix(')')) {
            (before, rest) = (outside.trim_end(), after.trim_start());
            continue;
        }
        // An alias, after AS or bare: a name with nothing after it but a parenthesis that closes.
        let named = match rest.get(..2) {
            Some(word) if word.eq_ignore_ascii_case("as") && rest[2..].starts_with(char::is_whitespace) => rest[2..].trim_start(),
            _ => rest,
        };
        match name_length(named).map(|length| named[length..].trim_start()) {
            Some(after) if after.is_empty() || after.starts_with(')') => rest = after,
            _ => break,
        }
    }
    (before.is_empty() && rest.is_empty()).then_some(k)
}

/// The table a statement names at `start`: names joined by dots, each plain or quoted, with the
/// spaces and comments ClickHouse takes around a dot read past, and where it ends.
fn table_at(body: &str, lexed: &Lexed, start: usize) -> Option<(String, usize)> {
    let b = body.as_bytes();
    let mut parts: Vec<&str> = Vec::new();
    let mut at = start;
    loop {
        let end = at + name_length(&body[at..])?;
        parts.push(&body[at..end]);
        let dot = code_after(body, lexed, end);
        if b.get(dot) != Some(&b'.') {
            return Some((parts.join("."), end));
        }
        at = code_after(body, lexed, dot + 1);
    }
}

/// The table an UPDATE or an ALTER TABLE … UPDATE writes, as the statement names it.
fn updated(body: &str, lexed: &Lexed, code: &[(usize, u8, usize)]) -> Option<String> {
    let top = top_words(body, lexed, code);
    let word = |i: usize| top.get(i).map(|(_, word)| word.as_str());
    let named = match (word(0)?, word(1), word(2)) {
        ("update", ..) => 0,
        ("alter", Some("table"), _) => 1,
        ("alter", Some("temporary"), Some("table")) => 2,
        _ => return None,
    };
    table_at(body, lexed, code_after(body, lexed, top[named].0 + top[named].1.len())).map(|(table, _)| table)
}

/// A name as the table's description gives it: without the quotes it is written in, a quote or
/// a backslash after a backslash, and a quote written twice, as the one it stands for. A write
/// whose names hold any other escape is refused before one is read ([`write`]).
fn unquoted(name: &str) -> String {
    let Some(quote) = name.chars().next().filter(|c| matches!(c, '`' | '"')) else { return name.to_string() };
    let mut out = String::with_capacity(name.len());
    let mut chars = name[1..name.len() - 1].chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.extend(chars.next()),
            c if c == quote && chars.peek() == Some(&quote) => {
                chars.next();
                out.push(quote);
            }
            c => out.push(c),
        }
    }
    out
}

/// A column's name as the table's description gives it, where `text` is one and no more: each of
/// its parts without its quotes, a Nested column's (`items.sku`) joined by their dot.
fn column_name(text: &str) -> Option<String> {
    let mut parts = Vec::new();
    let mut rest = text;
    loop {
        let length = name_length(rest)?;
        parts.push(unquoted(&rest[..length]));
        rest = &rest[length..];
        match rest.strip_prefix('.') {
            Some(after) => rest = after,
            None => break,
        }
    }
    rest.is_empty().then(|| parts.join("."))
}

/// The statement's code between two places.
fn between(code: &[(usize, u8, usize)], from: usize, to: usize) -> &[(usize, u8, usize)] {
    &code[code.partition_point(|&(at, _, _)| at < from)..code.partition_point(|&(at, _, _)| at < to)]
}

/// Whether a row of VALUES, from past its `(` to its `)`, reads as the SELECT of its values. One
/// that writes a column's default by the word `DEFAULT` does not, and neither does one that
/// writes a map in braces (`{'a': 1}`), which VALUES reads and a SELECT does not.
fn reads_as_select(body: &str, lexed: &Lexed, code: &[(usize, u8, usize)], start: usize, end: usize) -> bool {
    let row = between(code, start, end);
    if row.iter().any(|&(_, byte, _)| byte == b'{') {
        return false;
    }
    let mut cuts: Vec<usize> = row.iter().filter(|&&(_, byte, depth)| byte == b',' && depth == 1).map(|&(at, _, _)| at).collect();
    cuts.push(end);
    let mut from = start;
    !cuts.into_iter().any(|cut| {
        let item = plain(body, lexed, from, cut);
        from = cut + 1;
        item.trim().eq_ignore_ascii_case("default")
    })
}

/// The columns an INSERT lists.
enum Listed {
    /// None: the table's own, in their order.
    Table,
    /// Each by its name, as the table's description gives it.
    Names(Vec<String>),
    /// What is more than names: a `*`, with its modifiers or without, or a `COLUMNS('…')` matcher.
    Unread,
}

/// What an INSERT writes, as far as its text says.
struct Inserted {
    /// The table, as the statement names it: none where the rows go to a table function.
    table: Option<String>,
    listed: Listed,
    /// Where `VALUES` starts, and each row after it, from past its `(` to its `)`: none of an
    /// `INSERT … SELECT`.
    rows: Option<(usize, Vec<(usize, usize)>)>,
    /// Each value of a row of VALUES that is a parameter alone: its place in its row, and the
    /// parameter's index.
    written: Vec<(usize, usize)>,
    /// Every parameter in a value of a row of VALUES, alone or within what the value computes:
    /// the value's place in its row, the parameter's index, and where the value starts and ends.
    valued: Vec<(usize, usize, usize, usize)>,
    /// Where the word `FORMAT` is, of an INSERT that reads its rows in one.
    format: Option<usize>,
    /// The `SETTINGS` clause an INSERT names before its rows, from the word to where the rows
    /// start: the settings its values are computed with.
    settings: Option<(usize, usize)>,
    /// Where the SELECT starts that an `INSERT … SELECT` reads its rows from, a parenthesis
    /// around it included. What it writes is the server's to say ([`crate::tree::flows`]).
    select: Option<usize>,
}

/// The words outside every parenthesis, lowercased, each with where it starts.
fn top_words(body: &str, lexed: &Lexed, code: &[(usize, u8, usize)]) -> Vec<(usize, String)> {
    words(body, lexed).into_iter().filter(|(start, _)| code.binary_search_by_key(start, |&(at, _, _)| at).is_ok_and(|i| code[i].2 == 0)).collect()
}

/// What an INSERT writes ([`Inserted`]): its table, the columns it lists, and where its rows
/// come from. `None` where the statement opens otherwise, or names nothing its rows come from.
fn inserted(body: &str, lexed: &Lexed, code: &[(usize, u8, usize)]) -> Option<Inserted> {
    let b = body.as_bytes();
    let top = top_words(body, lexed, code);
    let word = |i: usize| top.get(i).map(|(_, word)| word.as_str());
    if word(0)? != "insert" || word(1)? != "into" {
        return None;
    }
    // The parenthesis that closes the one at `open`.
    let closes = |open: usize| between(code, open + 1, body.len()).iter().find(|&&(_, byte, depth)| byte == b')' && depth == 0).map(|&(at, _, _)| at);
    let named = if word(2) == Some("table") { 2 } else { 1 };
    let target = code_after(body, lexed, top[named].0 + top[named].1.len());
    // A table function's rows go where no table's description says the columns: its call is read past.
    let function = top.get(named + 1).filter(|(at, word)| *at == target && word == "function").is_some();
    let (table, end) = if function {
        let open = between(code, target, body.len()).iter().find(|&&(_, byte, depth)| byte == b'(' && depth == 0)?.0;
        (None, closes(open)? + 1)
    } else {
        let (table, end) = table_at(body, lexed, target)?;
        (Some(table), end)
    };
    // The list of columns after it: a parenthesis that what the rows come from follows. One that
    // nothing follows, or a set operation, is a SELECT in parentheses.
    let after = code_after(body, lexed, end);
    let list = Some(after).filter(|&open| b.get(open) == Some(&b'(')).and_then(closes).filter(|&close| {
        let next = code_after(body, lexed, close + 1);
        b.get(next) == Some(&b'(') || top.iter().any(|(at, word)| *at == next && matches!(word.as_str(), "values" | "select" | "with" | "from" | "format" | "settings"))
    });
    let listed = match (list, &table) {
        (_, None) => Listed::Unread,
        (None, _) => Listed::Table,
        // Each a name, between the commas of the list's own level.
        (Some(close), _) => {
            let mut cuts: Vec<usize> = between(code, after + 1, close).iter().filter(|&&(_, byte, depth)| byte == b',' && depth == 1).map(|&(at, _, _)| at).collect();
            cuts.push(close);
            let mut start = after + 1;
            let mut names = Some(Vec::with_capacity(cuts.len()));
            for cut in cuts {
                let name = plain(body, lexed, start, cut);
                // A comma after the last name is one ClickHouse takes.
                if !(cut == close && name.trim().is_empty() && start > after + 1) {
                    names = names.and_then(|mut names: Vec<String>| {
                        names.push(column_name(name.trim())?);
                        Some(names)
                    });
                }
                start = cut + 1;
            }
            names.map_or(Listed::Unread, Listed::Names)
        }
    };
    // What the rows come from: the first of these words past the list, outside every
    // parenthesis, or a parenthesis that stands before any of them, which holds a SELECT.
    let from = list.map_or(after, |close| close + 1);
    let source = top.iter().find(|(at, word)| *at >= from && matches!(word.as_str(), "values" | "select" | "with" | "from" | "format"));
    let paren = between(code, from, body.len()).iter().find(|&&(_, byte, depth)| byte == b'(' && depth == 0).map(|&(at, _, _)| at);
    let source = source.filter(|(at, _)| paren.is_none_or(|paren| paren > *at)).map(|(at, word)| (*at, word.as_str()));
    let rows_at = source.map(|(at, _)| at).or(paren);
    let settings = top.iter().find(|(at, word)| *at >= from && word == "settings").map(|(at, _)| *at).zip(rows_at).filter(|(at, rows)| at < rows);
    let none = Inserted { table: None, listed: Listed::Unread, rows: None, written: Vec::new(), valued: Vec::new(), format: None, settings, select: None };
    match source {
        Some((at, "format")) => Some(Inserted { table, listed, format: Some(at), ..none }),
        Some((at, "values")) => {
            // Each row's values, a level inside its parentheses, and each parameter of a value:
            // the placeholders stand in the text's order.
            let from = at + "values".len();
            let (mut rows, mut written, mut valued) = (Vec::new(), Vec::new(), Vec::new());
            let mut next = lexed.placeholders.partition_point(|p| p.start < from);
            let (mut place, mut item, mut open) = (0usize, from, from);
            for &(at, byte, depth) in between(code, from, body.len()) {
                let ends = match (byte, depth) {
                    (b'(', 0) => {
                        (place, item, open) = (0, at + 1, at + 1);
                        continue;
                    }
                    (b',', 1) | (b')', 0) => byte == b')',
                    _ => continue,
                };
                written.extend(alone(body, lexed, item, at).map(|k| (place, k)));
                while lexed.placeholders.get(next).is_some_and(|p| p.end <= at) {
                    valued.push((place, next, item, at));
                    next += 1;
                }
                if ends {
                    rows.push((open, at));
                } else {
                    (place, item) = (place + 1, at + 1);
                }
            }
            Some(Inserted { table, listed, rows: Some((at, rows)), written, valued, ..none })
        }
        found => Some(Inserted { table, listed, select: Some(found.map(|(at, _)| at).into_iter().chain(paren).min()?), ..none }),
    }
}

/// Whether a value of a row of VALUES, between two places, is one that VALUES may read by its
/// text for a column that holds a Dynamic: a parameter alone, or a value that opens with a
/// parenthesis, a bracket or a brace. A call, a cast and an operator after a first operand are
/// computed, and what they give is stored.
fn read_as_text(body: &str, lexed: &Lexed, code: &[(usize, u8, usize)], start: usize, end: usize) -> bool {
    if alone(body, lexed, start, end).is_some() {
        return true;
    }
    let Some(&(open, first, _)) = between(code, start, end).iter().find(|(_, byte, _)| !byte.is_ascii_whitespace()) else { return false };
    // Nothing that is no code stands before it: a string, a quoted name or a parameter.
    let before = lexed.opaque.iter().copied().filter(|&(from, _)| !is_comment(body, from)).chain(lexed.placeholders.iter().map(|p| (p.start, p.end))).any(|(from, _)| from >= start && from < open);
    matches!(first, b'(' | b'[' | b'{') && !before
}

/// Each assignment between two places: where it starts, its `=`, the first of its own level, and
/// where it ends, at the comma after it or at `to`.
fn assignments(code: &[(usize, u8, usize)], from: usize, to: usize) -> Vec<(usize, usize, usize)> {
    let list = between(code, from, to);
    let mut cuts: Vec<usize> = list.iter().filter(|&&(_, byte, depth)| byte == b',' && depth == 0).map(|&(at, _, _)| at).collect();
    cuts.push(to);
    let mut start = from;
    let mut out = Vec::with_capacity(cuts.len());
    for cut in cuts {
        out.extend(between(code, start, cut).iter().find(|&&(_, byte, depth)| byte == b'=' && depth == 0).map(|&(equals, _, _)| (start, equals, cut)));
        start = cut + 1;
    }
    out
}

/// The statement's strings, quoted names, comments and placeholders, which are no code, in order.
fn not_code(lexed: &Lexed) -> Vec<(usize, usize)> {
    let mut spans: Vec<(usize, usize)> = lexed.opaque.iter().copied().chain(lexed.placeholders.iter().map(|p| (p.start, p.end))).collect();
    spans.sort_unstable();
    spans
}

/// The statement's code a byte at a time, each with its place and how many parentheses, brackets
/// and braces it stands in: its strings, quoted names, comments and placeholders left out.
fn code(body: &str, lexed: &Lexed) -> Vec<(usize, u8, usize)> {
    let b = body.as_bytes();
    let spans = not_code(lexed);
    let mut out = Vec::with_capacity(b.len());
    let (mut i, mut depth, mut next) = (0, 0usize, 0);
    while i < b.len() {
        while next < spans.len() && spans[next].1 <= i {
            next += 1;
        }
        if next < spans.len() && spans[next].0 <= i {
            i = spans[next].1;
            continue;
        }
        match b[i] {
            b'(' | b'[' | b'{' => {
                out.push((i, b[i], depth));
                depth += 1;
            }
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                out.push((i, b[i], depth));
            }
            byte => out.push((i, byte, depth)),
        }
        i += 1;
    }
    out
}

/// Where a stretch of the statement's last byte that is no space and no comment's ends.
fn text_end(body: &str, lexed: &Lexed, code: &[(usize, u8, usize)], from: usize, to: usize) -> usize {
    let last = between(code, from, to).iter().rev().find(|(_, byte, _)| !byte.is_ascii_whitespace()).map(|&(at, _, _)| at + 1);
    let held = lexed.placeholders.iter().map(|p| (p.start, p.end)).chain(lexed.opaque.iter().copied().filter(|&(start, _)| !is_comment(body, start))).filter(|&(start, end)| start >= from && end <= to).map(|(_, end)| end).max();
    last.max(held).unwrap_or(to)
}

/// The SELECT a DELETE or an UPDATE reads as, as the edits of its text that make it one, each a
/// place, a length and what stands there. `DELETE FROM t WHERE c` is `SELECT 1 FROM t WHERE c`.
/// `UPDATE t SET a = x, b = y WHERE c` is `FROM t SELECT a , CAST(x, toTypeName(a)), b , CAST(y,
/// toTypeName(b)) WHERE c`, which names each column written and converts each value to its
/// column's type, as the UPDATE does, so that a value the column does not take is refused. `ALTER
/// TABLE t UPDATE … WHERE c` and `ALTER TABLE t DELETE WHERE c` read the same way. `ON CLUSTER c`
/// says where the statement runs, which a SELECT does not, so it is left out; `IN PARTITION p
/// WHERE c` keeps the rows to a partition, and reads as `WHERE (p) IS NOT NULL AND c`, which
/// computes the partition's expression too. With them, each assignment: where it starts, its
/// `=`, and where it ends. Where `converted` is not set, each value stands alone and as it is,
/// `SELECT x, y`, for the type it has of its own: a column's name beside it may be an alias's.
/// `None` where the statement opens otherwise.
#[allow(clippy::type_complexity)]
fn as_select(body: &str, lexed: &Lexed, code: &[(usize, u8, usize)], converted: bool) -> Option<(Vec<(usize, usize, String)>, Vec<(usize, usize, usize)>)> {
    // The words outside every parenthesis, lowercased, each with where it starts.
    let mut words: Vec<(usize, String)> = Vec::new();
    let mut last = None;
    for &(at, byte, depth) in code {
        if depth > 0 || !(byte.is_ascii_alphanumeric() || byte == b'_') {
            last = None;
            continue;
        }
        match words.last_mut() {
            Some((_, word)) if last == Some(at.wrapping_sub(1)) => word.push(byte.to_ascii_lowercase() as char),
            _ => words.push((at, (byte.to_ascii_lowercase() as char).to_string())),
        }
        last = Some(at);
    }
    let word = |i: usize| words.get(i).map(|(_, word)| word.as_str());
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    // ON CLUSTER and its cluster's name, a plain one or a quoted one.
    if let Some(on) = (0..words.len()).find(|&i| words[i].1 == "on" && word(i + 1) == Some("cluster")) {
        let name = code_after(body, lexed, words[on + 1].0 + "cluster".len());
        let quoted = lexed.opaque.iter().find(|&&(start, _)| start == name).map(|&(_, end)| end);
        let end = quoted.or_else(|| name_length(&body[name..]).map(|length| name + length))?;
        edits.push((words[on].0, end - words[on].0, String::new()));
    }
    // IN PARTITION, with the ID before a partition's name where it is written, and the WHERE after it.
    if let Some(part) = (0..words.len()).find(|&i| words[i].1 == "in" && word(i + 1) == Some("partition")) {
        let by_id = word(part + 2) == Some("id") && lexed.opaque.iter().any(|&(start, _)| start == code_after(body, lexed, words[part + 2].0 + "id".len()));
        let last = &words[part + 1 + usize::from(by_id)];
        let condition = words.iter().skip(part + 2 + usize::from(by_id)).find(|(_, word)| word == "where")?;
        edits.push((words[part].0, last.0 + last.1.len() - words[part].0, "WHERE (".into()));
        edits.push((condition.0, "where".len(), ") IS NOT NULL AND".into()));
    }
    // The word the assignments follow, where the statement has them.
    let set = match (word(0)?, word(1), word(2)) {
        ("delete", ..) => {
            edits.push((words[0].0, "delete".len(), "SELECT 1".into()));
            return Some((edits, Vec::new()));
        }
        ("update", ..) => {
            edits.push((words[0].0, "update".len(), "FROM".into()));
            let set = words.iter().position(|(_, word)| word == "set")?;
            edits.push((words[set].0, "set".len(), "SELECT".into()));
            set
        }
        ("alter", Some("table"), _) | ("alter", Some("temporary"), Some("table")) => {
            let table = if word(1) == Some("table") { 1 } else { 2 };
            edits.push((words[0].0, words[table].0 + "table".len() - words[0].0, "FROM".into()));
            let command = words.iter().skip(table + 1).position(|(_, word)| word == "update" || word == "delete")? + table + 1;
            if words[command].1 == "delete" {
                edits.push((words[command].0, "delete".len(), "SELECT 1".into()));
                return Some((edits, Vec::new()));
            }
            edits.push((words[command].0, "update".len(), "SELECT".into()));
            command
        }
        _ => return None,
    };
    // Each assignment's `=` as the comma between the column and its value, and the value
    // converted to the type of the column it names.
    let from = words[set].0 + words[set].1.len();
    let to = (set + 1..words.len()).find(|&i| words[i].1 == "where" || (words[i].1 == "in" && word(i + 1) == Some("partition"))).map_or(body.len(), |i| words[i].0);
    let assignments = assignments(code, from, to);
    for &(start, equals, cut) in &assignments {
        if converted {
            edits.push((equals, 1, ", CAST(".into()));
            edits.push((text_end(body, lexed, code, equals + 1, cut), 0, format!(", toTypeName({}))", plain(body, lexed, start, equals).trim())));
        } else {
            edits.push((start, equals + 1 - start, String::new()));
        }
    }
    Some((edits, assignments))
}

/// Where a place of the author's text that stands in no placeholder is in a rewritten statement:
/// the placeholders before it are each as long as what stands for them.
fn moved(text: &Rewritten, at: usize) -> usize {
    text.spans.iter().take_while(|&&(_, end, _, _)| end <= at).last().map_or(at, |&(_, end, _, rewritten_end)| rewritten_end + (at - end))
}

/// A rewritten statement with `edits` made, each at a place of the author's text that stands in
/// no placeholder and holds none. Of two at one place, the later is written before the earlier.
fn edited(text: &Rewritten, edits: &[(usize, usize, String)]) -> String {
    let mut ordered: Vec<&(usize, usize, String)> = edits.iter().collect();
    ordered.sort_by_key(|edit| std::cmp::Reverse(edit.0));
    let mut out = text.sql.clone();
    for (at, length, with) in ordered {
        let start = moved(text, *at);
        out.replace_range(start..start + length, with);
    }
    out
}

/// The words of the statement's code, lowercased, each with where it starts: its strings, quoted
/// names, comments and placeholders left out.
fn words(body: &str, lexed: &Lexed) -> Vec<(usize, String)> {
    let b = body.as_bytes();
    let spans = not_code(lexed);
    let mut out = Vec::new();
    let (mut i, mut next) = (0, 0);
    while i < b.len() {
        while next < spans.len() && spans[next].1 <= i {
            next += 1;
        }
        if next < spans.len() && spans[next].0 <= i {
            i = spans[next].1;
        } else if b[i].is_ascii_alphabetic() || b[i] == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push((start, body[start..i].to_ascii_lowercase()));
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Answer, Answers, Need};
    use serde_json::{json, Value};

    /// The analysis of `sql`, each query answered by `answer`.
    fn run(sql: &str, answer: &dyn Fn(&str) -> Result<Vec<Vec<Value>>, DbError>) -> Analysis {
        let mut answers = Answers::new();
        loop {
            let host = Host::new(&answers);
            if let Some(analysis) = analyze(sql, &host) {
                return analysis;
            }
            for need in host.needs() {
                let Need::Query { id, sql, .. } = need else { panic!("ClickHouse asks no describe") };
                answers.insert(id, match answer(&sql) {
                    Ok(rows) => Answer::Rows { rows },
                    Err(error) => Answer::Error { error },
                });
            }
        }
    }

    fn error(message: &str, code: &str) -> DbError {
        DbError { message: message.into(), code: Some(code.into()), position: None }
    }

    const DEFAULTS: fn() -> Vec<Vec<Value>> = || vec![vec![json!("additional_result_filter"), json!("")], vec![json!("empty_result_for_aggregation_by_empty_set"), json!("0")], vec![json!("limit"), json!("0")], vec![json!("offset"), json!("0")]];

    /// A server that describes every statement as `columns` and gives `tree` as its query tree.
    fn server<'a>(columns: &'a [(&'a str, &'a str)], tree: &'a str) -> impl Fn(&str) -> Result<Vec<Vec<Value>>, DbError> + 'a {
        move |sql| {
            if sql.starts_with(DESCRIBE) {
                Ok(columns.iter().map(|(name, ty)| vec![json!(name), json!(ty)]).collect())
            } else if sql.starts_with(TREE) {
                Ok(tree.lines().map(|line| vec![json!(line)]).collect())
            } else if sql.starts_with(TYPE_NAME) {
                Err(error("Code: 50. DB::Exception: Unknown data type family: nope. (UNKNOWN_TYPE) (version 26.9.23.15 (official build))", "50"))
            } else {
                Ok(DEFAULTS())
            }
        }
    }

    #[test]
    fn a_statement_is_typed_from_its_describe_and_its_parameters_from_their_declarations() {
        let sql = "SELECT id, email, tags FROM users WHERE id = {id:UInt64} AND created > {since:Nullable(DateTime64(3, 'UTC'))}";
        let a = run(sql, &server(&[("id", "UInt64"), ("email", "Nullable(String)"), ("tags", "Array(String)")], "QUERY id: 0"));
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        assert_eq!(a.kind, Some(StatementKind::Select));
        assert_eq!(a.cardinality, Cardinality::Many);
        assert_eq!(a.columns.iter().map(|c| (c.name.as_str(), c.nullable, c.described.as_deref())).collect::<Vec<_>>(), [("id", false, Some("UInt64")), ("email", true, Some("Nullable(String)")), ("tags", false, Some("Array(String)"))]);
        assert_eq!(a.params.iter().map(|p| (p.name.as_str(), p.nullable, p.ty.label.as_deref())).collect::<Vec<_>>(), [("id", false, Some("UInt64")), ("since", true, Some("DateTime64"))]);
        // A statement written ClickHouse's way is sent as it is written.
        assert_eq!(a.rewritten.sql, sql);
        assert_eq!(a.rewritten.order, ["id", "since"]);
        assert!(a.read_only);
    }

    #[test]
    fn the_describe_is_asked_with_each_parameter_as_a_column_but_where_the_text_says_a_constant() {
        let asked = std::cell::RefCell::new(Vec::new());
        let sql = "SELECT max(age) AS q FROM users WHERE name = {name:String} AND id IN ({one:UInt8}, 5) AND id NOT IN {ids:Array(UInt64)} AND id IN (SELECT id FROM t WHERE a = {a:Nullable(UInt8)}) -- why\n LIMIT {skip:UInt64}, {count:UInt64};\n";
        let a = run(sql, &|sent| {
            asked.borrow_mut().push(sent.to_string());
            server(&[("q", "UInt8")], "QUERY id: 0")(sent)
        });
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        let describes: Vec<String> = asked.borrow().iter().filter(|s| s.starts_with(DESCRIBE)).cloned().collect();
        // A column where a value goes, the subquery of an IN included; a constant after LIMIT and on the right of an IN.
        assert_eq!(
            describes[0],
            "DESCRIBE (\nSELECT max(age) AS q FROM users WHERE name = materialize(defaultValueOfTypeName('String')) AND id IN (CAST(1, 'UInt8'), 5) AND id NOT IN CAST([CAST(1, 'UInt64')], 'Array(UInt64)') AND id IN (SELECT id FROM t WHERE a = materialize(defaultValueOfTypeName('Nullable(UInt8)'))) -- why\n LIMIT CAST(1, 'UInt64'), CAST(1, 'UInt64')\n)"
        );
        // Then the statement as it runs, each parameter a constant, with two values of each.
        assert_eq!(
            describes[1],
            "DESCRIBE (\nSELECT max(age) AS q FROM users WHERE name = CAST('a', 'String') AND id IN (CAST(1, 'UInt8'), 5) AND id NOT IN CAST([CAST(1, 'UInt64')], 'Array(UInt64)') AND id IN (SELECT id FROM t WHERE a = CAST(CAST(1, 'UInt8'), 'Nullable(UInt8)')) -- why\n LIMIT CAST(1, 'UInt64'), CAST(1, 'UInt64')\n)"
        );
        assert_eq!(
            describes[2],
            "DESCRIBE (\nSELECT max(age) AS q FROM users WHERE name = CAST('b', 'String') AND id IN (CAST(2, 'UInt8'), 5) AND id NOT IN CAST([], 'Array(UInt64)') AND id IN (SELECT id FROM t WHERE a = CAST(NULL, 'Nullable(UInt8)')) -- why\n LIMIT CAST(2, 'UInt64'), CAST(2, 'UInt64')\n)"
        );
        assert_eq!(describes.len(), 3);
        // A limit a parameter gives bounds nothing.
        assert_eq!(a.cardinality, Cardinality::Many);
    }

    #[test]
    fn a_statement_has_the_types_it_runs_with_where_a_constant_is_typed_otherwise_than_a_column() {
        // ClickHouse types `x IN (constants)` a UInt8, and a Nullable(UInt8) where one of them is a column.
        let by_text = |sent: &str| match sent.strip_prefix(DESCRIBE) {
            Some(text) if text.contains("materialize(") => Ok(vec![vec![json!("hit"), json!("Nullable(UInt8)")]]),
            Some(_) => Ok(vec![vec![json!("hit"), json!("UInt8")]]),
            None => server(&[], "QUERY id: 0")(sent),
        };
        let a = run("SELECT (id, age) IN ((1, {age:UInt8})) AS hit FROM users", &by_text);
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        assert_eq!((a.columns[0].nullable, a.columns[0].described.as_deref()), (false, Some("UInt8")));
        // Two values of a parameter that give two types: the statement has no one type.
        let sql = "SELECT pick(age, {n:UInt8}) AS hit FROM users";
        let b = run(sql, &|sent| match sent.strip_prefix(DESCRIBE) {
            Some(text) if text.contains("CAST(2, 'UInt8')") => Ok(vec![vec![json!("hit"), json!("String")]]),
            Some(_) => Ok(vec![vec![json!("hit"), json!("UInt8")]]),
            None => server(&[], "QUERY id: 0")(sent),
        });
        assert_eq!(b.diagnostics.len(), 1);
        assert_eq!(&sql[b.diagnostics[0].start..b.diagnostics[0].start + b.diagnostics[0].length], "{n:UInt8}");
        assert_eq!(b.diagnostics[0].message, "The result's types change with the value of {n:UInt8}: column 1 (hit) has the type UInt8 with 1 and String with 2. Write the value in the statement.");
    }

    /// A describe that fails, as the server does with a text it cannot compute with.
    fn unparsed(name: &str, code: &str) -> Result<Vec<Vec<Value>>, DbError> {
        Err(error(&format!("Code: {code}. DB::Exception: Cannot parse it. ({name}) (version 26.9)"), code))
    }

    #[test]
    fn a_statement_is_described_with_values_it_computes_with() {
        // A function of a LowCardinality column and a constant is LowCardinality, and of two columns
        // is not: the types are the ones the statement runs with, a date standing for the string a
        // date is made of, which no letter is.
        let asked = std::cell::RefCell::new(Vec::new());
        let sql = "SELECT concat(plan, {suffix:String}) AS label FROM users WHERE created >= toDateTime({since:String})";
        let a = run(sql, &|sent| {
            asked.borrow_mut().push(sent.to_string());
            match sent.strip_prefix(DESCRIBE) {
                Some(text) if text.contains("materialize(") => Ok(vec![vec![json!("label"), json!("String")]]),
                Some(text) if text.contains("toDateTime(CAST('2001-02-03 04:05:06', 'String'))") || text.contains("toDateTime(CAST('2004-05-06 07:08:09', 'String'))") => Ok(vec![vec![json!("label"), json!("LowCardinality(String)")]]),
                Some(_) => unparsed("CANNOT_PARSE_DATETIME", "41"),
                None => server(&[], "QUERY id: 0")(sent),
            }
        });
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        assert_eq!(a.columns[0].described.as_deref(), Some("LowCardinality(String)"));
        // No parameter is one the server reads as a constant, so none is asked about alone.
        assert_eq!(asked.borrow().iter().filter(|s| s.starts_with(DESCRIBE)).count(), 1 + STRINGS.len());

        // Two parameters that each take a text of their own kind: one is changed at a time, and a
        // change the statement fails otherwise with is kept.
        let sql = "SELECT toDate(created, {zone:String}) AS day FROM users WHERE created >= {since:String}";
        let b = run(sql, &|sent| match sent.strip_prefix(DESCRIBE) {
            Some(text) if text.contains("materialize(") => Err(error("Code: 43. DB::Exception: Illegal type of argument. (ILLEGAL_TYPE_OF_ARGUMENT) (version 26.9)", "43")),
            Some(text) if !["'UTC'", "'Asia/Tokyo'"].iter().any(|zone| text.contains(&format!("toDate(created, CAST({zone}, 'String'))"))) => unparsed("BAD_ARGUMENTS", "36"),
            Some(text) if !["'2001-02-03 04:05:06'", "'2004-05-06 07:08:09'"].iter().any(|time| text.contains(&format!(">= CAST({time}, 'String')"))) => unparsed("CANNOT_PARSE_DATETIME", "41"),
            Some(_) => Ok(vec![vec![json!("day"), json!("Date")]]),
            None => server(&[], "QUERY id: 0\n  LIMIT\n    CONSTANT id: 4, constant_value: UInt64_1, constant_value_type: UInt64\n      EXPRESSION\n        FUNCTION id: 5, function_name: CAST, function_type: ordinary, result_type: UInt64")(sent),
        });
        assert!(b.diagnostics.is_empty(), "{:?}", b.diagnostics);
        assert_eq!(b.columns[0].described.as_deref(), Some("Date"));
        // Both are constants to the server, so a limit one of them is folded into is the caller's.
        assert_eq!(b.cardinality, Cardinality::Many);

        // A text the statement computes with, and none of the values tried is one: the parameter is refused where it stands.
        let sql = "SELECT parse({code:String}) AS n FROM users WHERE id = {id:UInt64}";
        let c = run(sql, &|sent| match sent.strip_prefix(DESCRIBE) {
            Some(text) if text.contains("parse(materialize(") => Ok(vec![vec![json!("n"), json!("UInt8")]]),
            Some(_) => Err(error("Code: 6. DB::Exception: Cannot parse the code: In scope SELECT parse(CAST('a', 'String')). (CANNOT_PARSE_TEXT) (version 26.9)", "6")),
            None => server(&[], "QUERY id: 0")(sent),
        });
        assert_eq!(c.diagnostics.len(), 1, "{:?}", c.diagnostics);
        assert_eq!(&sql[c.diagnostics[0].start..c.diagnostics[0].start + c.diagnostics[0].length], "{code:String}");
        assert_eq!(c.diagnostics[0].message, "ClickHouse computes with {code:String} as it reads the statement, and the statement fails with each value tried in its place, as with 'a'. Declare the parameter as the type the statement converts it to, or write the value in the statement. (Cannot parse the code: In scope SELECT parse({code:String}). (CANNOT_PARSE_TEXT))");
    }

    #[test]
    fn a_column_named_by_a_parameter_s_value_is_the_statement_s_value_or_refused() {
        // ClickHouse names a column with no alias by its expression, each constant in it written out.
        let named = |sent: &str| match sent.strip_prefix(DESCRIBE) {
            Some(text) => {
                let value = text.split("CAST(").nth(1).and_then(|rest| rest.split(',').next()).unwrap_or("?").to_string();
                let name = if text.contains("materialize(") { "plus(id, materialize(defaultValueOfTypeName('UInt32')))".to_string() } else { format!("plus(id, CAST({value}, 'UInt32'))") };
                Ok(if text.contains("SELECT *") { vec![vec![json!("id"), json!("UInt32")], vec![json!(name), json!("UInt64")]] } else { vec![vec![json!(name), json!("UInt64")]] })
            }
            None => server(&[], "QUERY id: 0")(sent),
        };
        // One such column alone goes by no name: the statement returns its values.
        let a = run("SELECT id + {x:UInt32} FROM users", &named);
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        assert!(a.value);
        // Among others, its name would change from run to run.
        let b = run("SELECT * FROM (SELECT id, id + {x:UInt32} FROM users)", &named);
        assert_eq!(b.diagnostics.len(), 1, "{:?}", b.diagnostics);
        assert_eq!(b.diagnostics[0].message, "Column 2 is named \"plus(id, {x:UInt32})\" by its expression, which holds a parameter's value, so its name changes from run to run. Give it one with `as` where the statement makes it.");
    }

    #[test]
    fn a_parameter_written_with_a_cast_is_sent_the_server_s_way() {
        let a = run("SELECT id FROM users WHERE id = :id::UInt64 AND name IN :names::Array(String)", &server(&[("id", "UInt64")], "QUERY id: 0"));
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        assert_eq!(a.rewritten.sql, "SELECT id FROM users WHERE id = {id:UInt64}::UInt64 AND name IN {names:Array(String)}::Array(String)");
        assert_eq!(a.params.iter().map(|p| p.ty.kind.name()).collect::<Vec<_>>(), ["integer", "array"]);
    }

    #[test]
    fn a_type_written_by_another_name_is_the_type_the_server_reads_it_as() {
        let sql = "SELECT id FROM users WHERE id = {id:int} AND name IN {names:Array(varchar)} AND age > {age:UInt8} AND id < {id:Int32} AND age < {most:TINYINT UNSIGNED}";
        let a = run(sql, &|sent| match sent.strip_prefix(TYPE_NAME) {
            Some("'int'))") => Ok(vec![vec![json!("Int32")]]),
            Some("'Array(varchar)'))") => Ok(vec![vec![json!("Array(String)")]]),
            Some("'TINYINT UNSIGNED'))") => Ok(vec![vec![json!("UInt8")]]),
            _ => server(&[("id", "UInt64")], "QUERY id: 0")(sent),
        });
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        assert_eq!(a.params.iter().map(|p| (p.name.as_str(), p.ty.label.as_deref())).collect::<Vec<_>>(), [("id", Some("Int32")), ("names", None), ("age", Some("UInt8")), ("most", Some("UInt8"))]);
        // The statement is sent with the names the server gave, which the run time reads the types by.
        assert_eq!(a.rewritten.sql, "SELECT id FROM users WHERE id = {id:Int32} AND name IN {names:Array(String)} AND age > {age:UInt8} AND id < {id:Int32} AND age < {most:UInt8}");
    }

    #[test]
    fn a_parameter_may_repeat_with_one_type_and_no_other() {
        let a = run("SELECT {n:UInt8} AS a, {n:UInt8} AS b", &server(&[("a", "UInt8"), ("b", "UInt8")], "QUERY id: 0"));
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        assert_eq!(a.params.len(), 1);
        let b = run("SELECT {n:UInt8} AS a, {n:String} AS b", &server(&[], "QUERY id: 0"));
        assert_eq!(b.diagnostics.iter().map(|d| (d.message.as_str(), d.code)).collect::<Vec<_>>(), [("The parameter n is declared String here and UInt8 at its first use: declare it with one type.", 3)]);
    }

    #[test]
    fn a_parameter_that_names_or_holds_what_cannot_be_typed_is_refused() {
        let refused = |sql: &str| run(sql, &server(&[], "QUERY id: 0")).diagnostics.into_iter().map(|d| (d.message, sql[d.start..d.start + d.length].to_string())).collect::<Vec<_>>();
        assert_eq!(refused("SELECT {c:Identifier} FROM users")[0].1, "{c:Identifier}");
        assert!(refused("SELECT {c:Identifier} FROM users")[0].0.contains("stands for a name"));
        assert!(refused("SELECT {doc:JSON} AS d")[0].0.starts_with("Reylo binds no parameter of the type JSON"));
        assert!(refused("SELECT {n:UInt46} AS d")[0].0.starts_with("Reylo binds no parameter of the type UInt46"));
        assert!(refused("SELECT {v:Array(Variant(String, UInt8))} AS d")[0].0.contains("is a Variant"));
    }

    /// A server that takes a column nowhere `needs` names, and describes `columns(text)` elsewhere.
    fn constant_only<'a>(needs: &'a str, columns: &'a dyn Fn(&str) -> Result<Vec<Vec<Value>>, DbError>) -> impl Fn(&str) -> Result<Vec<Vec<Value>>, DbError> + 'a {
        move |sent| match sent.strip_prefix(DESCRIBE) {
            Some(text) if text.contains(&format!("{needs}materialize(")) => Err(error(&format!("Code: 44. DB::Exception: The argument must be a constant: In scope {}. (ILLEGAL_COLUMN) (version 26.9.23.15 (official build))", text.trim_end_matches("\n)")), "44")),
            Some(text) => columns(text),
            None => server(&[], "QUERY id: 0\n  LIMIT\n    CONSTANT id: 4, constant_value: UInt64_1, constant_value_type: UInt64\n      EXPRESSION\n        FUNCTION id: 5, function_name: CAST, function_type: ordinary, result_type: UInt64")(sent),
        }
    }

    #[test]
    fn a_parameter_the_server_reads_as_a_constant_is_one_where_its_values_leave_the_types_alone() {
        let asked = std::cell::RefCell::new(Vec::new());
        let sql = "SELECT formatDateTime(created, {format:String}) AS shown, quantile({level:Float64})(age) AS q FROM users WHERE id = {id:UInt64}";
        let fixed = |_: &str| Ok(vec![vec![json!("shown"), json!("String")], vec![json!("q"), json!("Float64")]]);
        let by_format = constant_only("formatDateTime(created, ", &fixed);
        let a = run(sql, &|sent| {
            asked.borrow_mut().push(sent.to_string());
            if sent.contains("quantile(materialize(") {
                return Err(error("Code: 36. DB::Exception: Parameters to aggregate functions must be literals. (BAD_ARGUMENTS) (version 26.9.23.15 (official build))", "36"));
            }
            by_format(sent)
        });
        assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
        assert_eq!(a.columns.iter().map(|c| c.described.as_deref().unwrap()).collect::<Vec<_>>(), ["String", "Float64"]);
        // Each of the two is tried with every value of its list, alone, in the statement as it runs.
        for value in ["CAST('a', 'String')) AS shown, quantile(CAST(0.5, 'Float64'))", "CAST('b', 'String')) AS shown, quantile(CAST(0.5, 'Float64'))", "CAST('UTC', 'String')) AS shown, quantile(CAST(0.5, 'Float64'))", "CAST('a', 'String')) AS shown, quantile(CAST(0.25, 'Float64'))", "CAST('a', 'String')) AS shown, quantile(CAST(-1, 'Float64'))"] {
            assert!(asked.borrow().iter().any(|s| s.starts_with(DESCRIBE) && s.contains(value) && s.contains("id = CAST(1, 'UInt64')")), "{value}");
        }
        // The tree is read with the two written as constants, and the third parameter a column.
        assert!(asked.borrow().iter().any(|s| s.starts_with(TREE) && s.contains("CAST('a', 'String')) AS shown, quantile(CAST(0.5, 'Float64'))") && s.contains("id = materialize(")));
        // A limit such a parameter is folded into is the caller's: the tree shows what it was folded from.
        assert_eq!(a.cardinality, Cardinality::Many);
    }

    #[test]
    fn a_parameter_the_server_reads_as_a_constant_is_refused_where_its_value_changes_a_type() {
        let sql = "SELECT toDateTime64(created, {scale:UInt8}) AS t FROM users WHERE id = {id:UInt64}";
        let by_scale = |text: &str| {
            let scale = text.split("toDateTime64(created, CAST(").nth(1).and_then(|rest| rest.split(',').next()).unwrap_or("?");
            Ok(vec![vec![json!("t"), json!(format!("DateTime64({scale})"))]])
        };
        let a = run(sql, &constant_only("toDateTime64(created, ", &by_scale));
        assert_eq!(a.diagnostics.len(), 1);
        let d = &a.diagnostics[0];
        assert_eq!(&sql[d.start..d.start + d.length], "{scale:UInt8}");
        assert_eq!(d.message, "ClickHouse reads {scale:UInt8} as a constant here, and the result's types change with its value: column 1 (t) has the type DateTime64(1) with 1 and DateTime64(2) with 2. Write the value in the statement.");
        // A value the statement fails with says nothing of its types, where another describes as the first.
        let sql = "SELECT tupleElement(pair, {at:UInt8}) AS side FROM users";
        let failing_from = |from: u8| {
            move |text: &str| match (from..=3).any(|at| text.contains(&format!("CAST({at}, 'UInt8')"))) {
                true => Err(error("Code: 10. DB::Exception: Tuple doesn't have element with that index. (NOT_FOUND_COLUMN_IN_BLOCK) (version 26.9.23.15 (official build))", "10")),
                false => Ok(vec![vec![json!("side"), json!("UInt8")]]),
            }
        };
        let b = run(sql, &constant_only("tupleElement(pair, ", &failing_from(3)));
        assert!(b.diagnostics.is_empty(), "{:?}", b.diagnostics);
        // A place in the tuple past the first values tried is tried too, and one counted from its end.
        let sql = "SELECT tupleElement(spot, {at:Int8}) AS side FROM users";
        let by_place = |text: &str| Ok(vec![vec![json!("side"), json!(if text.contains("CAST(4, 'Int8')") || text.contains("CAST(-1, 'Int8')") { "String" } else { "Float64" })]]);
        let last = run(sql, &constant_only("tupleElement(spot, ", &by_place));
        assert_eq!(last.diagnostics.len(), 1, "{:?}", last.diagnostics);
        assert_eq!(last.diagnostics[0].message, "ClickHouse reads {at:Int8} as a constant here, and the result's types change with its value: column 1 (side) has the type Float64 with 1 and String with 4. Write the value in the statement.");
        // A unit of a date gives a date where a unit of a time gives a time: both kinds are tried.
        let sql = "SELECT dateTrunc({unit:String}, created) AS start FROM users";
        let by_unit = |text: &str| match text.split("dateTrunc(CAST('").nth(1).and_then(|rest| rest.split('\'').next()) {
            Some("day" | "hour") => Ok(vec![vec![json!("start"), json!("DateTime64(0, 'UTC')")]]),
            Some("month") => Ok(vec![vec![json!("start"), json!("Date32")]]),
            _ => unparsed("BAD_ARGUMENTS", "36"),
        };
        let unit = run(sql, &constant_only("dateTrunc(", &by_unit));
        assert_eq!(unit.diagnostics.len(), 1, "{:?}", unit.diagnostics);
        assert_eq!(unit.diagnostics[0].message, "ClickHouse reads {unit:String} as a constant here, and the result's types change with its value: column 1 (start) has the type DateTime64(0, 'UTC') with 'day' and Date32 with 'month'. Write the value in the statement.");
        // Where no other value describes, nothing shows that the types hold.
        let sql = "SELECT tupleElement(pair, {at:UInt8}) AS side FROM users";
        let failing_from = |_: u8| move |text: &str| match text.contains("CAST(1, 'UInt8')") {
            true => Ok(vec![vec![json!("side"), json!("UInt8")]]),
            false => Err(error("Code: 10. DB::Exception: Tuple doesn't have element with that index. (NOT_FOUND_COLUMN_IN_BLOCK) (version 26.9.23.15 (official build))", "10")),
        };
        let b = run(sql, &constant_only("tupleElement(pair, ", &failing_from(2)));
        assert_eq!(b.diagnostics.len(), 1);
        assert_eq!(b.diagnostics[0].message, "ClickHouse reads {at:UInt8} as a constant here, and the statement fails with each other value tried in its place, as with 2, so nothing shows that the result's types hold for every value. Write the value in the statement. (Tuple doesn't have element with that index. (NOT_FOUND_COLUMN_IN_BLOCK))");
        // A type Reylo has one value of cannot be varied.
        let sql = "SELECT toString(created, {zone:Enum8('UTC' = 1)}) AS shown FROM users";
        let c = run(sql, &constant_only("toString(created, ", &|_| Ok(vec![vec![json!("shown"), json!("String")]])));
        assert_eq!(c.diagnostics.len(), 1);
        assert!(c.diagnostics[0].message.starts_with("ClickHouse reads {zone:Enum8('UTC' = 1)} as a constant here, and Reylo has one value of the type Enum8('UTC' = 1) to describe the statement with"), "{}", c.diagnostics[0].message);
    }

    #[test]
    fn a_parameter_read_as_a_constant_is_found_where_its_first_constant_is_refused_too() {
        let cast = "Code: 43. DB::Exception: Second argument to CAST must be a constant string describing type. Instead there is non-constant column of type String: In scope SELECT CAST(age, materialize(defaultValueOfTypeName('String'))) AS a FROM users. (ILLEGAL_TYPE_OF_ARGUMENT) (version 26.9.23.15 (official build))";
        let unknown_type = "Code: 50. DB::Exception: Unknown data type family: a: In scope SELECT CAST(age, CAST('a', 'String')) AS a FROM users. (UNKNOWN_TYPE) (version 26.9.23.15 (official build))";
        let answer = |sent: &str| {
            if !sent.starts_with(DESCRIBE) {
                server(&[], "QUERY id: 0")(sent)
            } else if sent.contains("CAST(age, materialize(") {
                Err(error(cast, "43"))
            } else {
                Err(error(unknown_type, "50"))
            }
        };
        let sql = "SELECT CAST(age, {type:String}) AS a FROM users WHERE id = {id:UInt64}";
        let a = run(sql, &answer);
        assert_eq!(a.diagnostics.len(), 1);
        let d = &a.diagnostics[0];
        assert_eq!(&sql[d.start..d.start + d.length], "{type:String}");
        assert_eq!(d.message, "ClickHouse reads {type:String} as a constant here, and the statement fails with each value tried in its place, as with 'a', so its value is part of what the statement says. Write the value in the statement. (Unknown data type family: a: In scope SELECT CAST(age, {type:String}) AS a FROM users. (UNKNOWN_TYPE))");
        // A statement that fails the same way whatever its parameters are is wrong by itself.
        let unknown = "Code: 47. DB::Exception: Unknown expression identifier `nope`. In scope SELECT nope FROM users. (UNKNOWN_IDENTIFIER) (version 26.9.23.15 (official build))";
        let b = run("SELECT nope FROM users WHERE id = {id:UInt64}", &|sent| if sent.starts_with(DESCRIBE) { Err(error(unknown, "47")) } else { server(&[], "QUERY id: 0")(sent) });
        assert_eq!((b.diagnostics[0].start, b.diagnostics[0].code), (0, 6));
        assert!(b.diagnostics[0].message.starts_with("Unknown expression identifier `nope`"), "{}", b.diagnostics[0].message);
    }

    #[test]
    fn the_server_s_error_is_the_statement_s_at_the_place_it_names() {
        let sql = "SELECT id FROM users WHERE";
        let a = run(sql, &|sent| {
            if sent.starts_with(DESCRIBE) {
                Err(error("Code: 62. DB::Exception: Syntax error: failed at position 39 ()) (line 3, col 1): ). Expected one of: expression. (SYNTAX_ERROR) (version 26.9.23.15 (official build))", "62"))
            } else {
                server(&[], "QUERY id: 0")(sent)
            }
        });
        let d = &a.diagnostics[0];
        assert_eq!((d.start, d.length, d.code, d.server_code.as_deref()), (sql.len() - 1, 1, 6, Some("62")));
        // The place is the diagnostic's own: the message leaves out the server's, counted in the text it was sent.
        assert_eq!(d.message, "Syntax error. Expected one of: expression. (SYNTAX_ERROR)");
        let sql = "SELECT id FROM users FORMAT JSON\n";
        let a = run(sql, &|sent| {
            if sent.starts_with(DESCRIBE) {
                Err(error("Code: 62. DB::Exception: Syntax error: failed at position 40 (JSON) (line 2, col 29): JSON\n\n). Expected one of: token sequence, Dot. (SYNTAX_ERROR) (version 26.9.23.15 (official build))", "62"))
            } else {
                server(&[], "QUERY id: 0")(sent)
            }
        });
        let d = &a.diagnostics[0];
        assert_eq!((&sql[d.start..d.start + d.length], d.message.as_str()), ("JSON", "Syntax error. Expected one of: token sequence, Dot. (SYNTAX_ERROR)"));
        assert_eq!(syntax_parts("Syntax error: failed at position 8 ()): ). Expected one of: expression"), Some(("Syntax error", ")", "). Expected one of: expression")));
        let unknown = run("SELECT nope FROM users", &|sent| if sent.starts_with(DESCRIBE) { Err(error("Code: 47. DB::Exception: Unknown expression identifier `nope`. In scope SELECT nope FROM users. (UNKNOWN_IDENTIFIER) (version 26.9.23.15 (official build))", "47")) } else { server(&[], "QUERY id: 0")(sent) });
        assert_eq!(unknown.diagnostics[0].message, "Unknown expression identifier `nope`. In scope SELECT nope FROM users. (UNKNOWN_IDENTIFIER)");
        assert_eq!(unknown.diagnostics[0].start, 0);
    }

    #[test]
    fn the_rows_are_what_the_query_tree_proves() {
        let aggregate = "QUERY id: 0\n  PROJECTION\n    LIST id: 1, nodes: 1\n      FUNCTION id: 2, function_name: count, function_type: aggregate, result_type: UInt64\n  JOIN TREE\n    TABLE id: 3, table_name: app.users";
        let one = run("SELECT count() AS n FROM users", &server(&[("n", "UInt64")], aggregate));
        assert_eq!(one.cardinality, Cardinality::One);
        let limited = format!("{}\n  LIMIT\n    CONSTANT id: 4, constant_value: UInt64_1, constant_value_type: UInt64", aggregate.replace("function_type: aggregate", "function_type: ordinary"));
        assert_eq!(run("SELECT id AS n FROM users LIMIT 1", &server(&[("n", "UInt64")], &limited)).cardinality, Cardinality::AtMostOne);
        // A parameter as the limit is the caller's: the tree shows the value it was read with, and under it what that was folded from.
        let given = format!("{limited}\n      EXPRESSION\n        FUNCTION id: 5, function_name: CAST, function_type: ordinary, result_type: UInt64");
        assert_eq!(run("SELECT id AS n FROM users LIMIT {n:UInt64}", &server(&[("n", "UInt64")], &given)).cardinality, Cardinality::Many);
        // A session that drops an aggregate's row over an empty input leaves at most one.
        let with = |name: &'static str, value: &'static str| move |sent: &str| if sent == ROW_SETTINGS { Ok(DEFAULTS().into_iter().map(|row| if row[0] == name { vec![json!(name), json!(value)] } else { row }).collect()) } else { server(&[("n", "UInt64")], aggregate)(sent) };
        assert_eq!(run("SELECT count() AS n FROM users", &with("empty_result_for_aggregation_by_empty_set", "1")).cardinality, Cardinality::AtMostOne);
        // So does one that filters every result, which the aggregate's row may not pass.
        assert_eq!(run("SELECT count() AS n FROM users", &with("additional_result_filter", "n > 5")).cardinality, Cardinality::AtMostOne);
    }

    #[test]
    fn a_result_of_more_than_rows_is_refused() {
        let a = run("SELECT plan, count() AS n FROM users GROUP BY plan WITH TOTALS", &server(&[("plan", "String"), ("n", "UInt64")], "QUERY id: 0, is_group_by_with_totals: 1\n  PROJECTION COLUMNS"));
        assert_eq!(a.diagnostics.len(), 1);
        assert!(a.diagnostics[0].message.starts_with("WITH TOTALS returns its totals beside the rows, and a typed result is its rows alone"), "{}", a.diagnostics[0].message);
        // A format the statement names is the one the server would answer every question in, so it is read off the text.
        let sql = "SELECT 'default_format' AS n SETTINGS default_format = 'JSONEachRow'";
        let b = run(sql, &|_| Err(error("unread", "1")));
        assert_eq!(b.diagnostics[0].message, "The statement's SETTINGS names a format for its result, default_format, and Reylo reads the one it asks for: take the setting out.");
        assert_eq!(b.diagnostics[0].start, sql.rfind("default_format").unwrap());
        // A column or an alias of the setting's name is no setting.
        let named = run("SELECT default_format FROM formats WHERE default_format = 'x'", &server(&[("default_format", "String")], "QUERY id: 0"));
        assert!(named.diagnostics.is_empty(), "{:?}", named.diagnostics);
        let c = run("SELECT 1 AS n SETTINGS extremes = 1", &server(&[("n", "UInt8")], "QUERY id: 0\n  PROJECTION COLUMNS\n  SETTINGS extremes=1"));
        assert!(c.diagnostics[0].message.starts_with("The statement's SETTINGS turns on extremes"), "{}", c.diagnostics[0].message);
    }

    #[test]
    fn a_column_s_name_is_an_alias_or_a_column_s_own() {
        let value = run("SELECT count() FROM users", &server(&[("count()", "UInt64")], "QUERY id: 0"));
        assert!(value.value && value.diagnostics.is_empty());
        let sql = "SELECT id, age + 1 FROM users";
        let unnamed = run(sql, &server(&[("id", "UInt64"), ("plus(age, 1)", "UInt16")], "QUERY id: 0"));
        assert_eq!(unnamed.diagnostics.len(), 1);
        assert_eq!((unnamed.diagnostics[0].code, &sql[unnamed.diagnostics[0].start..unnamed.diagnostics[0].start + unnamed.diagnostics[0].length]), (analysis::UNNAMED_COLUMN, "age + 1"));
        // A name that reads as no identifier is still the statement's where its text says it: a
        // member read by its path, a quoted column or alias, and whatever a `*` stands for.
        let named = |sql: &str, columns: &[(&str, &str)]| run(sql, &server(columns, "QUERY id: 0")).diagnostics;
        assert!(named("SELECT items.sku, o.`odd name`, total * 2 AS `twice the total`, \"q\" FROM orders o", &[("items.sku", "Array(String)"), ("odd name", "UInt8"), ("twice the total", "UInt64"), ("q", "UInt8")]).is_empty());
        assert!(named("SELECT * FROM orders", &[("items.sku", "Array(String)"), ("odd name", "UInt8")]).is_empty());
        assert!(named("SELECT id, o.* EXCEPT (id), COLUMNS('^t'), total AS t FROM orders o", &[("id", "UInt64"), ("items.sku", "Array(String)"), ("total", "UInt64"), ("t", "UInt64")]).is_empty());
        assert_eq!(named("SELECT *, total * 2, id FROM orders", &[("odd name", "UInt8"), ("multiply(total, 2)", "UInt64"), ("id", "UInt64")]).len(), 1);
        assert!(is_star("*") && is_star("t.*") && is_star("db.`t`.* EXCEPT (a)") && is_star("* EXCEPT (a)") && is_star("COLUMNS('a')") && is_star("untuple(pair)") && !is_star("a * b") && !is_star("count(*)") && !is_star("t.*x"));
        // A `.*` in a string an item holds is no table's star.
        assert!(!is_star("match(name, 'a.* b')") && !is_star("'t.*'"));
        assert_eq!(named("SELECT id, match(name, 'a.* b') FROM users", &[("id", "UInt64"), ("match(name, 'a.* b')", "UInt8")]).len(), 1);
        assert!(names_itself("a.b.c") && names_itself("`a b`") && names_itself("f(x) AS `a b`") && names_itself("x as y") && names_itself("(1)AS y"));
        // A name in typographic quotes, which ClickHouse reads as it reads one in backquotes.
        assert!(names_itself("“a b”") && names_itself("1 AS “a b”") && names_itself("‘x’ AS “a b”") && !names_itself("1 + “a”"));
        assert!(!names_itself("a + b") && !names_itself("f(x)") && !names_itself("pair.1") && !names_itself("CAST(x AS String)") && !names_itself("x IS NULL"));
    }

    #[test]
    fn a_type_the_run_time_does_not_read_is_refused_with_what_to_write() {
        let a = run("SELECT doc FROM events", &server(&[("doc", "JSON")], "QUERY id: 0"));
        assert_eq!(a.diagnostics[0].message, "Column 1 (doc) has the type JSON, and Reylo does not read a ClickHouse JSON yet: select it as a type it reads, as toJSONString gives.");
        let b = run("SELECT state, days FROM events", &server(&[("state", "AggregateFunction(sum, UInt8)"), ("days", "IntervalDay")], "QUERY id: 0"));
        assert!(b.diagnostics[0].message.ends_with("as finalizeAggregation gives.") && b.diagnostics[1].message.ends_with("as toString gives."), "{:?}", b.diagnostics);
    }

    /// A value's lines in a tree's projection, the value numbered `i`: one computed from a
    /// stand-in of each of `on`'s types, a parameter's own stand-in where it is of the value's
    /// type alone, or a table's column where it has none.
    fn computed_from(i: usize, ty: &str, on: &[&str]) -> String {
        computed_with(i, "plus", ty, on)
    }

    /// The same, the function that computes the value from its stand-ins by its name.
    fn computed_with(i: usize, name: &str, ty: &str, on: &[&str]) -> String {
        let id = (i + 1) * 10;
        let stand_in = |id: usize, ty: &str, indent: usize| format!("{:indent$}FUNCTION id: {id}, function_name: materialize, function_type: ordinary, result_type: {ty}", "");
        match on {
            [] => format!("      COLUMN id: {id}, column_name: c{i}, result_type: {ty}, source_id: 3"),
            [own] if *own == ty => stand_in(id, ty, 6),
            _ => {
                let stand_ins: Vec<String> = on.iter().enumerate().map(|(k, of)| stand_in(id + 2 + k, of, 12)).collect();
                format!("      FUNCTION id: {id}, function_name: {name}, function_type: ordinary, result_type: {ty}\n        ARGUMENTS\n          LIST id: {}, nodes: {}\n{}", id + 1, on.len(), stand_ins.join("\n"))
            }
        }
    }

    /// A value's lines in a tree's projection, the value numbered `i`: an `if` of the type `ty`
    /// between two results, by a stand-in of the type `on`. A result is a node's text after its
    /// id: a text's constant where it is a word alone.
    fn chosen(i: usize, ty: &str, on: &str, results: [&str; 2]) -> String {
        let id = (i + 1) * 10;
        let result = |k: usize| match results[k] {
            word if word.chars().all(|c| c.is_ascii_alphanumeric()) => format!("            CONSTANT id: {}, constant_value: '{word}', constant_value_type: String", id + 3 + k),
            node => format!("            {}", node.replacen("id: #", &format!("id: {}", id + 3 + k), 1)),
        };
        format!("      FUNCTION id: {id}, function_name: if, function_type: ordinary, result_type: {ty}\n        ARGUMENTS\n          LIST id: {}, nodes: 3\n            FUNCTION id: {}, function_name: materialize, function_type: ordinary, result_type: {on}\n{}\n{}", id + 1, id + 2, result(0), result(1))
    }

    /// What a server answers of the values a write gives its columns, each by its type and its
    /// lines in the tree's projection: the describe of their SELECT, a row a value, and its tree.
    /// `None` of any other question.
    fn told(values: &[(&str, String)], sent: &str) -> Option<Vec<Vec<Value>>> {
        if sent.starts_with(TREE) {
            let names: String = values.iter().enumerate().map(|(i, (ty, _))| format!("\n    c{i} {ty}")).collect();
            let items: String = values.iter().map(|(_, item)| format!("\n{item}")).collect();
            let tree = format!("QUERY id: 0\n  PROJECTION COLUMNS{names}\n  PROJECTION\n    LIST id: 1, nodes: {}{items}\n  JOIN TREE\n    TABLE id: 3, table_name: t", values.len());
            Some(tree.lines().map(|line| vec![json!(line)]).collect())
        } else if sent.starts_with(DESCRIBE) && !sent.contains("toTypeName(") {
            Some(values.iter().enumerate().map(|(i, (ty, _))| vec![json!(format!("c{i}")), json!(ty)]).collect())
        } else {
            None
        }
    }

    /// The same, each value by its type and whether it depends on a parameter: one that does is
    /// a parameter's stand-in, and any other a table's column.
    fn written(values: &[(&str, bool)], sent: &str) -> Option<Vec<Vec<Value>>> {
        let values: Vec<(&str, String)> = values.iter().enumerate().map(|(i, (ty, depends))| (*ty, computed_from(i, ty, if *depends { std::slice::from_ref(ty) } else { &[] }))).collect();
        told(&values, sent)
    }

    #[test]
    fn a_write_returns_no_rows_and_is_checked_as_far_as_the_server_checks_one() {
        let asked = std::cell::RefCell::new(Vec::new());
        let orders = |sent: &str| -> Result<Vec<Vec<Value>>, DbError> {
            Ok(if sent == "DESCRIBE TABLE orders" { vec![vec![json!("id"), json!("UInt64"), json!("")], vec![json!("note"), json!("Nullable(String)"), json!("")], vec![json!("total"), json!("Decimal(18, 2)"), json!("")]] } else { Vec::new() })
        };
        let ok = |sent: &str| {
            asked.borrow_mut().push(sent.to_string());
            orders(sent)
        };
        let insert = run("INSERT INTO orders (id, note) VALUES ({id:UInt64}, {note:Nullable(String)})", &ok);
        assert!(insert.diagnostics.is_empty(), "{:?}", insert.diagnostics);
        assert_eq!((insert.kind, insert.cardinality, insert.columns.len()), (Some(StatementKind::Insert), Cardinality::None, 0));
        assert_eq!(insert.params.iter().map(|p| p.nullable).collect::<Vec<_>>(), [false, true]);
        assert!(asked.borrow().iter().any(|s| s.starts_with(PIPELINE)) && asked.borrow().iter().any(|s| s.starts_with(AST)));
        asked.borrow_mut().clear();
        let delete = run("DELETE FROM orders WHERE id = {id:UInt64}", &ok);
        assert_eq!((delete.kind, delete.cardinality), (Some(StatementKind::Delete), Cardinality::None));
        assert!(asked.borrow().iter().any(|s| s == "DESCRIBE (\nSELECT 1 FROM orders WHERE id = materialize(defaultValueOfTypeName('UInt64'))\n)"), "{:?}", asked.borrow());
        let refused = run("DELETE FROM orders WHERE nope = 1", &|sent| if sent.starts_with(DESCRIBE) { Err(error("Code: 47. DB::Exception: Unknown expression identifier `nope`. (UNKNOWN_IDENTIFIER) (version 26.9)", "47")) } else { Ok(Vec::new()) });
        assert_eq!(refused.diagnostics[0].message, "Unknown expression identifier `nope`. (UNKNOWN_IDENTIFIER)");
        // An UPDATE reads as the SELECT of each column it writes and each value it computes, converted to the column's type.
        asked.borrow_mut().clear();
        let assigns = |sent: &str| {
            asked.borrow_mut().push(sent.to_string());
            written(&[("Nullable(String)", true), ("Decimal(18, 2)", false)], sent).map_or_else(|| orders(sent), Ok)
        };
        let update = run("UPDATE orders SET note = {note:Nullable(String)}, total = round(total, 2) + 1 WHERE id IN (SELECT id FROM gone WHERE at = 1) AND id = {id:UInt64}", &assigns);
        assert_eq!((update.kind, update.cardinality, update.diagnostics.len()), (Some(StatementKind::Update), Cardinality::None, 0), "{:?}", update.diagnostics);
        // And as the list of its values as they are, for the type each has and where it comes from; the
        // columns are the table's own, by its description. A lightweight UPDATE's values see no name its
        // WHERE gives, so they are described without it.
        assert!(asked.borrow().iter().any(|s| s == "DESCRIBE TABLE orders"), "{:?}", asked.borrow());
        for question in [DESCRIBE, TREE] {
            let select = "SELECT\n(\n materialize(defaultValueOfTypeName('Nullable(String)'))\n),\n(\n round(total, 2) + 1 \n)\nFROM orders";
            assert!(asked.borrow().iter().any(|s| s.strip_prefix(question).is_some_and(|rest| rest.trim_end_matches("\n)") == select)), "{:?}", asked.borrow());
        }
        assert!(asked.borrow().iter().any(|s| s == "DESCRIBE (\nFROM orders SELECT note , CAST( materialize(defaultValueOfTypeName('Nullable(String)')), toTypeName(note)), total , CAST( round(total, 2) + 1, toTypeName(total)) WHERE id IN (SELECT id FROM gone WHERE at = 1) AND id = materialize(defaultValueOfTypeName('UInt64'))\n)"), "{:?}", asked.borrow());
        // An ALTER is one of rows where each of its commands is, as the server's parse names them.
        let log = &asked;
        let parse = |commands: &'static str| move |sent: &str| {
            log.borrow_mut().push(sent.to_string());
            match sent.strip_prefix(AST) {
                Some(text) if text.starts_with("ALTER") => Ok(format!("AlterQuery  orders (children 2)\n ExpressionList (children 1)\n{commands}").lines().map(|line| vec![json!(line)]).collect()),
                _ => written(&[("Nullable(String)", true)], sent).map_or_else(|| orders(sent), Ok),
            }
        };
        asked.borrow_mut().clear();
        let altered = run("ALTER TABLE orders UPDATE note = {note:Nullable(String)} WHERE id = {id:UInt64}", &parse("  AlterCommand UPDATE (children 2)"));
        assert_eq!((altered.cardinality, altered.diagnostics.len()), (Cardinality::None, 0), "{:?}", altered.diagnostics);
        // An ALTER's values see the names its WHERE gives, and are described with it.
        assert!(asked.borrow().iter().any(|s| s == "EXPLAIN QUERY TREE\nFROM orders SELECT materialize(defaultValueOfTypeName('Nullable(String)')) WHERE id = materialize(defaultValueOfTypeName('UInt64'))"), "{:?}", asked.borrow());
        assert!(asked.borrow().iter().any(|s| s == "DESCRIBE (\nFROM orders SELECT note , CAST( materialize(defaultValueOfTypeName('Nullable(String)')), toTypeName(note)) WHERE id = materialize(defaultValueOfTypeName('UInt64'))\n)"), "{:?}", asked.borrow());
        asked.borrow_mut().clear();
        let cut = run("ALTER TABLE orders DELETE WHERE id = {id:UInt64}", &parse("  AlterCommand DELETE (children 1)"));
        assert!(cut.diagnostics.is_empty(), "{:?}", cut.diagnostics);
        assert!(asked.borrow().iter().any(|s| s == "DESCRIBE (\nFROM orders SELECT 1 WHERE id = materialize(defaultValueOfTypeName('UInt64'))\n)"), "{:?}", asked.borrow());
        let added = run("ALTER TABLE orders ADD COLUMN z UInt8", &parse("  AlterCommand ADD_COLUMN (children 1)"));
        assert!(added.diagnostics[0].message.starts_with("Reylo types an ALTER TABLE that changes rows, with UPDATE or DELETE, and ADD COLUMN is neither, so if it is a migration"), "{}", added.diagnostics[0].message);
        let frozen = run("ALTER TABLE orders UPDATE note = '' WHERE 1, FREEZE", &parse("  AlterCommand UPDATE (children 2)\n  AlterCommand FREEZE_ALL"));
        assert!(frozen.diagnostics[0].message.contains("and FREEZE ALL is neither"), "{}", frozen.diagnostics[0].message);
        // A value a statement computes with as it is read is tried with each of a parameter's, and the statement refused where none is taken.
        let dated = run("UPDATE orders SET note = '' WHERE created >= {since:String}", &|sent| match sent.strip_prefix(DESCRIBE).or(sent.strip_prefix(TREE)) {
            // The values it writes hold no parameter, and are described whatever the WHERE's stands as.
            Some(text) if text.contains("'2001-02-03 04:05:06'") || text.starts_with("SELECT\n(") => Ok(written(&[("String", false)], sent).unwrap_or_default()),
            Some(_) => unparsed("CANNOT_PARSE_DATETIME", "41"),
            None => Ok(Vec::new()),
        });
        assert!(dated.diagnostics.is_empty(), "{:?}", dated.diagnostics);
        let missing = run("UPDATE orders SET nope = {note:String} WHERE id = 1", &|sent| if sent.starts_with(DESCRIBE) { Err(error("Code: 47. DB::Exception: Unknown expression identifier `nope`. (UNKNOWN_IDENTIFIER) (version 26.9)", "47")) } else { Ok(Vec::new()) });
        assert_eq!(missing.diagnostics[0].message, "Unknown expression identifier `nope`. (UNKNOWN_IDENTIFIER)");
        // A parameter written to a column as it is declares a type the column keeps every value of.
        let lines = |sent: &str| match sent {
            "DESCRIBE TABLE shop.`order lines`" => Ok(vec![vec![json!("id"), json!("UInt8"), json!("")], vec![json!("total"), json!("Decimal(9, 2)"), json!("")], vec![json!("seen"), json!("UInt8"), json!("MATERIALIZED")], vec![json!("note"), json!("LowCardinality(String)"), json!("DEFAULT")]]),
            _ => Ok(Vec::new()),
        };
        // The same table, and the types of the values a statement computes with a parameter.
        let computes = |types: &'static [&'static str]| move |sent: &str| match sent.strip_prefix(DESCRIBE).or(sent.strip_prefix(TREE)) {
            Some(select) if select.starts_with("SELECT\n(") => Ok(written(&types.iter().map(|ty| (*ty, true)).collect::<Vec<_>>(), sent).unwrap_or_default()),
            _ => lines(sent),
        };
        let table = computes(&[]);
        let sql = "INSERT INTO shop.`order lines` VALUES ({id:UInt64}, {total:Decimal(18, 4)}, {note:Nullable(String)}), (1 + {id:UInt64}, 2, 'x')";
        let wide = run(sql, &computes(&["UInt64"]));
        assert_eq!(wide.diagnostics.iter().map(|d| &sql[d.start..d.start + d.length]).collect::<Vec<_>>(), ["{id:UInt64}", "{total:Decimal(18, 4)}", "{note:Nullable(String)}", "{id:UInt64}"]);
        assert_eq!(wide.diagnostics[0].message, "{id:UInt64} is written as it is to the column id, of the type UInt8. ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error. Declare the parameter as the column's type, {id:UInt8}.");
        // A NULL is what a column that holds none does not keep: an INSERT writes its default value in place of one.
        assert_eq!(wide.diagnostics[2].message, "{note:Nullable(String)} is written as it is to the column note, of the type LowCardinality(String). The column holds no NULL, and an INSERT writes the column's default value in place of one, with no error unless a setting asks for one. Declare the parameter as a type that is never NULL, {note:String}, or write what a NULL becomes in the statement, as ifNull({note:Nullable(String)}, …) does.");
        // A value the statement computes with a parameter has the type the server describes it with.
        assert_eq!(wide.diagnostics[3].message, "{id:UInt64} is in a value of the type UInt64 that is written to the column id, of the type UInt8. ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error. Give each parameter the value is computed from a type the column keeps, or convert the value in the statement, as CAST(… AS UInt8) does.");
        // By the columns the statement lists, in its order; a cast is the parameter's type too.
        let listed = run("INSERT INTO shop.`order lines` (note, id) VALUES (:note::String, :id::UInt16)", &table);
        assert_eq!(listed.diagnostics.len(), 1, "{:?}", listed.diagnostics);
        assert!(listed.diagnostics[0].message.starts_with(":id::UInt16 is written as it is to the column id, of the type UInt8.") && listed.diagnostics[0].message.ends_with("Declare the parameter as the column's type, :id::UInt8."), "{}", listed.diagnostics[0].message);
        // A comment beside a parameter, or in the list of columns, is no part of what is written.
        let sql = "INSERT INTO shop.`order lines` (note /* its text */, id -- the key\n) VALUES (/* the note */ {note:String}, {id:UInt16} /* too wide */)";
        let commented = run(sql, &table);
        assert_eq!(commented.diagnostics.iter().map(|d| &sql[d.start..d.start + d.length]).collect::<Vec<_>>(), ["{id:UInt16}"]);
        let described = std::cell::RefCell::new(Vec::new());
        let fits = run("INSERT INTO shop.`order lines` (id, total, note) VALUES ({id:UInt8}, {total:Decimal(9, 2)}, {note:String}), ({id:UInt8}, toDecimal32({total:Decimal(9, 2)}, 2), concat({note:String}, 'x') -- joined\n)", &|sent| {
            described.borrow_mut().push(sent.to_string());
            computes(&["Decimal(9, 2)", "String"])(sent)
        });
        assert!(fits.diagnostics.is_empty(), "{:?}", fits.diagnostics);
        // The values that compute with a parameter are described as one SELECT's columns, each on lines of its own.
        assert!(described.borrow().iter().any(|s| s == "DESCRIBE (\nSELECT\n(\n toDecimal32(materialize(defaultValueOfTypeName('Decimal(9, 2)')), 2)\n),\n(\n concat(materialize(defaultValueOfTypeName('String')), 'x') -- joined\n\n)\n)"), "{:?}", described.borrow());
        let format = run("INSERT INTO orders FORMAT JSONEachRow", &ok);
        assert!(format.diagnostics[0].message.starts_with("An INSERT … FORMAT takes its rows"));
        let ddl = run("CREATE TABLE t (id UInt64) ENGINE = MergeTree ORDER BY id", &ok);
        assert!(ddl.diagnostics[0].message.starts_with("Reylo types a ClickHouse SELECT, INSERT, DELETE or UPDATE, and CREATE is none of them"), "{}", ddl.diagnostics[0].message);
    }

    #[test]
    fn a_value_that_depends_on_a_parameter_is_held_to_its_column() {
        let column = |name: &str, ty: &str| vec![json!(name), json!(ty), json!("")];
        // The table, and of the values a statement gives its columns what the server says: `values`.
        let server = |values: &'static [(&'static str, bool)]| move |sent: &str| match sent {
            "DESCRIBE TABLE u" => Ok(vec![column("id", "UInt8"), column("a", "UInt8"), column("at", "DateTime64(3)"), column("n.a", "Array(UInt8)"), column("format", "String"), column("d", "Dynamic"), column("v", "Variant(UInt64, String)"), column("j", "JSON")]),
            _ => Ok(written(values, sent).unwrap_or_default()),
        };
        let refused = |sql: &str, values: &'static [(&'static str, bool)]| run(sql, &server(values)).diagnostics.iter().map(|d| (sql[d.start..d.start + d.length].to_string(), d.message.clone())).collect::<Vec<_>>();
        let held = |sql: &str, values: &'static [(&'static str, bool)]| refused(sql, values).into_iter().map(|(at, _)| at).collect::<Vec<_>>();
        // A parameter alone has the type it declares: in parentheses it is the same parameter, and a narrower whole number is kept.
        assert_eq!(held("INSERT INTO u (id, a) VALUES (({i:UInt8}), ({x:UInt64}))", &[]), ["{x:UInt64}"]);
        // A column's name may be a setting's or a Nested column's part, and a comma may follow the last.
        assert_eq!(held("INSERT INTO u (id, format, n.a) VALUES ({i:UInt8}, {f:String}, {xs:Array(UInt64)})", &[]), ["{xs:Array(UInt64)}"]);
        assert_eq!(held("INSERT INTO u (id, a,) VALUES ({i:UInt8}, {x:UInt64})", &[]), ["{x:UInt64}"]);
        // A value the statement computes with a parameter has the type the server describes it with,
        // which a conversion in the statement names: an array, a tuple and a sum are each a value's.
        assert!(held("INSERT INTO u (id, a) VALUES ({i:UInt8}, {x:UInt64}::UInt8), ({j:UInt8}, CAST({y:UInt64}, 'UInt8'))", &[("UInt8", true), ("UInt8", true)]).is_empty());
        assert!(held("INSERT INTO u (id, a) VALUES (:i::UInt8, :x::UInt64::UInt8)", &[("UInt8", true)]).is_empty());
        let sum = refused("INSERT INTO u (id, a, n.a) VALUES ({i:UInt8}, {x:UInt64} + 0, [{y:UInt8}, {z:UInt64}])", &[("UInt64", true), ("Array(UInt64)", true)]);
        // A value of one parameter is refused at it, and one of several at the value.
        assert_eq!(sum.iter().map(|(at, _)| at.as_str()).collect::<Vec<_>>(), ["{x:UInt64}", "[{y:UInt8}, {z:UInt64}]"]);
        assert_eq!(sum[1].1, "This value of the type Array(UInt64) depends on a parameter and is written to the column n.a, of the type Array(UInt8). ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error. Convert the value in the statement, as CAST(… AS Array(UInt8)) does.");
        // An UPDATE's assignments: each value's type as the server describes it, what its tree says of the
        // value, and the column the table's description gives the name the assignment writes.
        assert_eq!(held("UPDATE u SET a = {x:UInt64}, at = {t:DateTime64(6)} WHERE id = {i:UInt64}", &[("UInt64", true), ("DateTime64(6)", true)]), ["{x:UInt64}", "{t:DateTime64(6)}"]);
        assert!(held("UPDATE u SET a = {x:UInt8}, at = now64(3) WHERE id = {i:UInt64}", &[("UInt8", true), ("DateTime64(3)", false)]).is_empty());
        // A name in the statement may be an alias's, which the server reads before the column's: the column is the table's.
        assert_eq!(held("UPDATE u SET a = {x:UInt64} WHERE (toUInt64(id) AS a) > 0", &[("UInt64", true)]), ["{x:UInt64}"]);
        assert_eq!(refused("UPDATE u SET nope = {x:UInt8} WHERE id = 1", &[("UInt8", true)]), [("{x:UInt8}".to_string(), "{x:UInt8} is written as it is to a column the statement's text does not name, so nothing holds it to that column's type. Write the table and each column by its name.".to_string())]);
        // A value of columns and constants alone is ClickHouse's to convert, whatever keeps the rows it is written to.
        assert!(held("UPDATE u SET a = id * 1000 WHERE id = {i:UInt64}", &[("UInt16", false)]).is_empty());
        // One a statement computes is held as one alone is, and one that reaches its column through a name is refused at the name.
        let computed = refused("UPDATE u SET a = a + {x:UInt64} WHERE id = 1", &[("UInt64", true)]);
        assert_eq!(computed, [("{x:UInt64}".to_string(), "{x:UInt64} is in a value of the type UInt64 that is written to the column a, of the type UInt8. ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error. Give each parameter the value is computed from a type the column keeps, or convert the value in the statement, as CAST(… AS UInt8) does.".to_string())]);
        let sql = "UPDATE u SET at = now64(3), a = late WHERE ({x:UInt64} AS late) > 0";
        let named = refused(sql, &[("DateTime64(3)", false), ("UInt64", true)]);
        assert_eq!(named, [("late".to_string(), "This value of the type UInt64 depends on a parameter and is written to the column a, of the type UInt8. ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error. Give each parameter the value is computed from a type the column keeps, or convert the value in the statement, as CAST(… AS UInt8) does.".to_string())]);
        // And an ALTER's.
        let parse = |sent: &str| match sent.strip_prefix(AST) {
            Some(text) if text.starts_with("ALTER") => Ok("AlterQuery  u (children 2)\n ExpressionList (children 1)\n  AlterCommand UPDATE (children 2)".lines().map(|line| vec![json!(line)]).collect()),
            _ => server(&[("UInt64", true)])(sent),
        };
        let altered = run("ALTER TABLE u UPDATE a = {x:UInt64} WHERE id = 1", &parse);
        assert_eq!(altered.diagnostics[0].message, "{x:UInt64} is written as it is to the column a, of the type UInt8. ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error. Declare the parameter as the column's type, {x:UInt8}.");
        // VALUES reads a value for a Dynamic by its text where the value is a parameter alone or opens with a
        // parenthesis or a bracket, and stores the placeholder's own text: what a call, a cast or an operator computes is stored.
        assert_eq!(refused("INSERT INTO u (id, d) VALUES ({i:UInt8}, {x:UInt64})", &[])[0].1, "{x:UInt64} is in a value that VALUES writes to the column d, of the type Dynamic. ClickHouse's VALUES reads a value for a Dynamic by its text where the value is a parameter alone or opens with a parenthesis or a bracket, and stores the placeholder's own text in place of the parameter's value. Write the row as an INSERT … SELECT, which computes each value.");
        assert_eq!(held("INSERT INTO u (id, d) VALUES ({i:UInt8}, ({x:UInt64} + 1)), ({i:UInt8}, [{s:String}]), ({i:UInt8}, (1) + {y:UInt8})", &[("UInt64", true), ("Array(String)", true), ("UInt16", true)]), ["{x:UInt64}", "{s:String}", "{y:UInt8}"]);
        // A Dynamic holds a value of any type as that type, so what a statement computes for one is kept.
        assert!(held("INSERT INTO u (id, d) VALUES ({i:UInt8}, CAST({x:UInt64}, 'Dynamic')), ({i:UInt8}, {x:UInt64}::Dynamic), ({i:UInt8}, 1 + {y:UInt64})", &[("Dynamic", true), ("Dynamic", true), ("UInt64", true)]).is_empty());
        // A type no parameter declares takes a value the statement gives that type.
        assert!(refused("INSERT INTO u (id, j) VALUES ({i:UInt8}, {x:String})", &[])[0].1.ends_with("No parameter declares that type: convert the value in the statement, as CAST({x:String}, 'JSON') does."));
        assert!(refused("INSERT INTO u (id, j) VALUES ({i:UInt8}, concat({x:String}, ''))", &[("String", true)])[0].1.ends_with("to the column j, of the type JSON. ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error. Give the value the column's type in the statement: no parameter declares that type."));
        // Where the text does not say the value's column, any column of the table may be the one that holds a Dynamic.
        assert!(refused("INSERT INTO u (* EXCEPT (at)) VALUES (2, 3, [{x:UInt64}])", &[("Array(UInt64)", true)])[0].1.starts_with("{x:UInt64} is in a value that VALUES writes to a column the statement's text does not name, which may hold a Dynamic."));
        // A Variant takes a value of one of its types, and a text only where it is of text alone.
        assert!(refused("INSERT INTO u (id, v) VALUES ({i:UInt8}, {x:UInt8})", &[])[0].1.ends_with("Declare the parameter as one of the types the column holds."));
        assert!(held("INSERT INTO u (id, v) VALUES ({i:UInt8}, {x:UInt64})", &[]).is_empty());
        assert!(held("INSERT INTO u (id, v) VALUES ({i:UInt8}, {x:UInt64} + 1)", &[("UInt64", true)]).is_empty());
        assert!(refused("INSERT INTO u (id, v) VALUES ({i:UInt8}, concat({s:String}, 'x'))", &[("String", true)])[0].1.ends_with("123 is stored as a number where the Variant has one."));
        assert!(refused("INSERT INTO u (id, v) VALUES ({i:UInt8}, {x:UInt8} + 1)", &[("UInt16", true)])[0].1.ends_with("Give the value one of the types the column holds, other than a text: declare its parameters so, or convert it in the statement."));
        // Where the text does not say which column a value is written to, nothing holds it: in a list
        // of columns that is more than names, and to a table function.
        assert_eq!(held("INSERT INTO u (COLUMNS('^(id|a)$')) VALUES ({i:UInt8}, {x:UInt8})", &[]), ["{i:UInt8}", "{x:UInt8}"]);
        assert_eq!(held("INSERT INTO FUNCTION remote('other', db.u) (id) VALUES ({i:UInt8})", &[]), ["{i:UInt8}"]);
        assert_eq!(refused("INSERT INTO FUNCTION remote('other', db.u) (id) VALUES ({i:UInt8})", &[])[0].1, "{i:UInt8} is written as it is to a table function's column, and Reylo reads no table function's columns, so nothing holds it to that column's type. Insert into a table.");
        assert_eq!(refused("INSERT INTO FUNCTION remote('other', db.u) (id) VALUES ({i:UInt8} + 0)", &[("UInt8", true)]), [("{i:UInt8}".to_string(), "{i:UInt8} is in a value that is written to a table function's column, and Reylo reads no table function's columns, so nothing holds it to that column's type. Insert into a table.".to_string())]);
        // A table is named with its database, whatever stands around the dot, and a quoted name as the table's description writes it.
        let asked = std::cell::RefCell::new(Vec::new());
        let logged = |sent: &str| {
            asked.borrow_mut().push(sent.to_string());
            Ok(Vec::new())
        };
        run("INSERT INTO app . /* the table */ u (id) VALUES ({i:UInt8})", &logged);
        assert!(asked.borrow().iter().any(|s| s == "DESCRIBE TABLE app.u"), "{:?}", asked.borrow());
        assert_eq!((unquoted("`x\\\\y`"), unquoted("`a``b`"), unquoted("\"a\\\"b\""), unquoted("plain")), ("x\\y".to_string(), "a`b".to_string(), "a\"b".to_string(), "plain".to_string()));
        // The settings an INSERT names before its rows are those its values are computed with.
        asked.borrow_mut().clear();
        run("INSERT INTO u (id, a) SETTINGS async_insert = 1 VALUES ({i:UInt8}, {x:UInt8} + 0)", &logged);
        assert!(asked.borrow().iter().any(|s| s == "DESCRIBE (\nSELECT\n(\n materialize(defaultValueOfTypeName('UInt8')) + 0\n)\nSETTINGS async_insert = 1\n)"), "{:?}", asked.borrow());
        asked.borrow_mut().clear();
        run("INSERT INTO u (id, a) SETTINGS async_insert = 1 SELECT 1, {x:UInt8}", &logged);
        assert!(asked.borrow().iter().any(|s| s == "EXPLAIN QUERY TREE\nSELECT * FROM (\nSELECT 1, materialize(defaultValueOfTypeName('UInt8'))\n) SETTINGS async_insert = 1"), "{:?}", asked.borrow());
        // A name with a line break in it would break the server's description of the statement.
        let sql = "INSERT INTO u (id, a) SELECT 1 AS `one\\n      CONSTANT id: 9`, {x:UInt8}";
        assert_eq!(refused(sql, &[]), [("`one\\n      CONSTANT id: 9`".to_string(), "This name holds a control character or an escape, and the server's description of the statement writes a name as it is, a line break included, so Reylo cannot read what the statement writes. Write the name without it.".to_string())]);
        // So is any escape but a backslash's or a quote's, which may stand for one and names another column than it reads as.
        assert_eq!(held("INSERT INTO u (id, `a\\x41`) VALUES ({i:UInt8}, {x:UInt8})", &[]), ["`a\\x41`"]);
        assert!(held("INSERT INTO u (id, `a`) VALUES ({i:UInt8}, {x:UInt8})", &[]).is_empty());
        // A text in a Dynamic is the text where the server infers no type from one: the session's setting, or the statement's.
        let inferring = |on: &'static str| move |sent: &str| if sent == CONVERSION_SETTINGS { Ok(vec![vec![json!(KEEPS_NULLABLE), json!("0")], vec![json!(INFERS), json!(on)]]) } else { server(&[("UInt8", false), ("String", true)])(sent) };
        let sql = "INSERT INTO u (id, d) SELECT 1, {s:String}";
        assert!(run(sql, &inferring("0")).diagnostics.is_empty());
        assert_eq!(run(sql, &inferring("1")).diagnostics.iter().map(|d| d.message.as_str()).collect::<Vec<_>>(), ["{s:String} is in a value of the type String that is written to the column d, of the type Dynamic. ClickHouse reads a text written to a Dynamic as whichever type the text reads as where the setting cast_string_to_dynamic_use_inference is on, as it is for this statement, so the column would not hold the text as it was given: 123 is stored as a number. Turn the setting off, or give the value a type that is no text."]);
        assert_eq!(run("INSERT INTO u (id, d) SELECT 1, {s:String} SETTINGS cast_string_to_dynamic_use_inference = 1", &inferring("0")).diagnostics.len(), 1);
        assert_eq!(run("INSERT INTO u (id, d) SELECT 1, {s:String} SETTINGS `cast_string_to_dynamic_use_inference` = 1", &inferring("0")).diagnostics.len(), 1);
        assert!(run("INSERT INTO u (id, d) SELECT 1, {n:UInt64}", &|sent: &str| if sent == CONVERSION_SETTINGS { Ok(vec![vec![json!(INFERS), json!("1")]]) } else { server(&[("UInt8", false), ("UInt64", true)])(sent) }).diagnostics.is_empty());
        // A lightweight UPDATE's own settings are those its values are described with: one may change a value's type, or which name a value reads.
        let asked = std::cell::RefCell::new(Vec::new());
        run("UPDATE u SET a = {x:UInt8} + 0 WHERE settings = 1 AND id = {i:UInt64} SETTINGS prefer_column_name_to_alias = 1, max_threads = 2", &|sent: &str| {
            asked.borrow_mut().push(sent.to_string());
            server(&[("UInt8", true)])(sent)
        });
        assert!(asked.borrow().iter().any(|s| s == "EXPLAIN QUERY TREE\nSELECT\n(\n materialize(defaultValueOfTypeName('UInt8')) + 0 \n)\nFROM u\nSETTINGS prefer_column_name_to_alias = 1, max_threads = 2"), "{:?}", asked.borrow());
        // A table the server does not know is the server's error, not a column the text does not name.
        let unknown = |sent: &str| if sent.starts_with("DESCRIBE TABLE") || sent.starts_with(PIPELINE) { Err(error("Code: 60. DB::Exception: Unknown table expression identifier 'nope'. (UNKNOWN_TABLE) (version 26.9)", "60")) } else { Ok(Vec::new()) };
        assert_eq!(run("INSERT INTO nope (id) VALUES ({i:UInt8})", &unknown).diagnostics.iter().map(|d| d.message.as_str()).collect::<Vec<_>>(), ["Unknown table expression identifier 'nope'. (UNKNOWN_TABLE)"]);
        // A value whose type the server does not say is held by nothing: here, one it describes as nothing.
        let sql = "INSERT INTO u (id, a) VALUES ({i:UInt8}, {x:UInt8} + 0)";
        assert_eq!(refused(sql, &[]), [(sql.to_string(), "Reylo cannot read the type of a value this statement writes with a parameter, so nothing would hold the value to its column.".to_string())]);
        // A table described without each column's kind, as a session's describe_compact_output gives it, says no order of its own.
        let compact = |sent: &str| match sent {
            "DESCRIBE TABLE u" => Ok(vec![vec![json!("id"), json!("UInt8")], vec![json!("a"), json!("UInt8")]]),
            _ => Ok(Vec::new()),
        };
        let unplaced = run("INSERT INTO u VALUES ({i:UInt8}, {x:UInt8})", &compact);
        assert_eq!(unplaced.diagnostics.len(), 2);
        assert_eq!(unplaced.diagnostics[0].message, "{i:UInt8} is written as it is to a column the statement's text does not name, so nothing holds it to that column's type. Write the table and each column by its name.");
        assert!(run("INSERT INTO u (id, a) VALUES ({i:UInt8}, {x:UInt8})", &compact).diagnostics.is_empty());
    }

    #[test]
    fn a_value_of_another_type_stands_where_no_declaration_would_change_that() {
        let column = |name: &str, ty: &str| vec![json!(name), json!(ty), json!("")];
        let server = |values: Vec<(&'static str, String)>| {
            move |sent: &str| match sent {
                "DESCRIBE TABLE t" => Ok(vec![column("id", "UInt8"), column("n", "UInt8"), column("role", "Enum8('new' = 1, 'paid' = 2)"), column("v", "Variant(UInt64, String)"), column("name", "String"), column("was", "Nullable(Enum8('new' = 1, 'paid' = 2))")]),
                _ => Ok(told(&values, sent).unwrap_or_default()),
            }
        };
        let refused = |sql: &str, values: Vec<(&'static str, String)>| run(sql, &server(values)).diagnostics.iter().map(|d| d.message.clone()).collect::<Vec<_>>();
        let converts = "ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error.";
        // A sum is wider than what it adds. Where arithmetic alone computes it, from what the column
        // keeps the type of, and it is a number of the column's kind, the sum is ClickHouse's arithmetic.
        assert!(refused("UPDATE t SET n = n + {by:UInt8} WHERE id = 1", vec![("UInt16", computed_from(0, "UInt16", &["UInt8"]))]).is_empty());
        assert!(refused("INSERT INTO t (id, n) VALUES ({i:UInt8}, {a:UInt8} * {b:UInt8})", vec![("UInt16", computed_from(0, "UInt16", &["UInt8", "UInt8"]))]).is_empty());
        assert!(refused("INSERT INTO t (id, n) SELECT 1, {a:UInt8} + 1", vec![("UInt8", computed_from(0, "UInt8", &[])), ("UInt16", computed_from(1, "UInt16", &["UInt8"]))]).is_empty());
        // A parameter of a type the column does not keep is one a declaration changes.
        assert_eq!(refused("INSERT INTO t (id, n) VALUES ({i:UInt8}, {a:UInt8} * {b:UInt16})", vec![("UInt32", computed_from(0, "UInt32", &["UInt8", "UInt16"]))]), [format!("This value of the type UInt32 depends on a parameter and is written to the column n, of the type UInt8. {converts} Give each parameter the value is computed from a type the column keeps, or convert the value in the statement, as CAST(… AS UInt8) does.")]);
        // A parameter the value passes on is one a declaration changes it with, whatever its kind.
        let passed = refused("INSERT INTO t (id, name) SELECT 1, {at:DateTime} FROM t WHERE id = {i:UInt8}", vec![("UInt8", computed_from(0, "UInt8", &[])), ("DateTime", computed_from(1, "DateTime", &["DateTime"]))]);
        assert_eq!(passed, [format!("The statement writes a value of the type DateTime that depends on a parameter to the column name, of the type String. {converts} Give each parameter the value is computed from a type the column keeps, or convert the value in the statement, as CAST(… AS String) does.")]);
        // A parameter that could not be of the column's kind is none a declaration changes: the statement converts the value.
        assert_eq!(refused("UPDATE t SET n = length({s:String}) WHERE id = 1", vec![("UInt64", computed_with(0, "length", "UInt64", &["String"]))]), [format!("{{s:String}} is in a value of the type UInt64 that is written to the column n, of the type UInt8. {converts} Convert the value in the statement, as CAST(… AS UInt8) does.")]);
        // What anything but arithmetic computes is no sum, whatever its parameters are: a hash, and a value a parameter only chooses.
        for (sql, name) in [("UPDATE t SET n = cityHash64({a:UInt8}) WHERE id = 1", "cityHash64"), ("UPDATE t SET n = if({a:UInt8} = 1, 1000, 0) WHERE id = 1", "if")] {
            assert_eq!(refused(sql, vec![("UInt64", computed_with(0, name, "UInt64", &["UInt8"]))]), [format!("{{a:UInt8}} is in a value of the type UInt64 that is written to the column n, of the type UInt8. {converts} Convert the value in the statement, as CAST(… AS UInt8) does.")], "{sql}");
        }
        // And neither is a sum with a column or a constant of a type the column does not keep.
        let sum = |operand: &str| format!("      FUNCTION id: 10, function_name: plus, function_type: ordinary, result_type: UInt64\n        ARGUMENTS\n          LIST id: 11, nodes: 2\n            {operand}\n            FUNCTION id: 13, function_name: materialize, function_type: ordinary, result_type: UInt8");
        assert!(refused("UPDATE t SET n = id + {by:UInt8} WHERE id = 1", vec![("UInt64", sum("COLUMN id: 12, column_name: id, result_type: UInt8, source_id: 3"))]).is_empty());
        for operand in ["COLUMN id: 12, column_name: big, result_type: UInt64, source_id: 3", "CONSTANT id: 12, constant_value: UInt64_1000, constant_value_type: UInt16"] {
            assert_eq!(refused("UPDATE t SET n = big + {by:UInt8} WHERE id = 1", vec![("UInt64", sum(operand))]), [format!("{{by:UInt8}} is in a value of the type UInt64 that is written to the column n, of the type UInt8. {converts} Convert the value in the statement, as CAST(… AS UInt8) does.")], "{operand}");
        }
        // A parameter the value's own text holds is one it is computed from, whatever the tree says of it.
        assert_eq!(refused("INSERT INTO t (id, n) VALUES ({i:UInt8}, {a:UInt8} * {b:UInt16})", vec![("UInt32", computed_from(0, "UInt32", &["UInt8"]))]).len(), 1);
        assert_eq!(refused("UPDATE t SET n = n + {a:UInt8} + {b:UInt16} WHERE id = 1", vec![("UInt32", computed_from(0, "UInt32", &["UInt8"]))]).len(), 1);
        // A value of a source the engine does not know may be of any parameter: its own type is all that holds it.
        let unknown = "      COLUMN id: 10, column_name: number, result_type: UInt64, source_id: 99".to_string();
        assert_eq!(refused("INSERT INTO t (id, n) SELECT 1, number FROM numbers({n:UInt8})", vec![("UInt8", computed_from(0, "UInt8", &[])), ("UInt64", unknown.replace("id: 10", "id: 20"))]), [format!("{{n:UInt8}} is in a value of the type UInt64 that is written to the column n, of the type UInt8. {converts} Convert the value in the statement, as CAST(… AS UInt8) does.")]);
        // A Variant reads a text by what it says, whatever the text was computed from, and VALUES writes a
        // number that is none of its types as a text: only a value of one of its types is kept.
        let text = refused("INSERT INTO t (id, v) VALUES ({i:UInt8}, toString({x:UInt64}))", vec![("String", computed_from(0, "String", &["UInt64"]))]);
        assert!(text.len() == 1 && text[0].ends_with("123 is stored as a number where the Variant has one."), "{text:?}");
        let signed = refused("INSERT INTO t (id, v) VALUES ({i:UInt8}, {a:UInt64} - {b:UInt64})", vec![("Int64", computed_from(0, "Int64", &["UInt64", "UInt64"]))]);
        assert!(signed.len() == 1 && signed[0].ends_with("Give the value one of the types the column holds, other than a text: declare its parameters so, or convert it in the statement."), "{signed:?}");
        // A value of another kind is no number the column's arithmetic gives: a text the column would read, a float in a column of whole numbers.
        for (sql, name, ty) in [("INSERT INTO t (id, n) VALUES ({i:UInt8}, concat(toString({a:UInt8}), toString({b:UInt8})))", "concat", "String"), ("INSERT INTO t (id, n) VALUES ({i:UInt8}, {a:UInt8} / {b:UInt8})", "divide", "Float64")] {
            assert_eq!(refused(sql, vec![(ty, computed_with(0, name, ty, &["UInt8", "UInt8"]))]), [format!("This value of the type {ty} depends on a parameter and is written to the column n, of the type UInt8. {converts} Convert the value in the statement, as CAST(… AS UInt8) does.")], "{sql}");
        }
        // A text that is one of the enum's labels whichever its parameters choose is the label.
        for sql in ["INSERT INTO t (id, role) VALUES ({i:UInt8}, if({paid:Bool}, 'paid', 'new'))", "UPDATE t SET role = if({paid:Bool}, 'paid', 'new') WHERE id = 1"] {
            assert!(refused(sql, vec![("String", chosen(0, "String", "Bool", ["paid", "new"]))]).is_empty(), "{sql}");
        }
        // So is a value of an enum the column keeps, as the column's own is, and a NULL where the column holds one.
        let own = "COLUMN id: #, column_name: role, result_type: Enum8('new' = 1, 'paid' = 2), source_id: 3";
        assert!(refused("UPDATE t SET role = if({paid:Bool}, 'paid', role) WHERE id = 1", vec![("String", chosen(0, "String", "Bool", ["paid", own]))]).is_empty());
        let null = "CONSTANT id: #, constant_value: NULL, constant_value_type: Nullable(Nothing)";
        assert!(refused("UPDATE t SET was = if({paid:Bool}, 'paid', NULL) WHERE id = 1", vec![("Nullable(String)", chosen(0, "Nullable(String)", "Bool", ["paid", null]))]).is_empty());
        let unheld = refused("UPDATE t SET role = if({paid:Bool}, 'paid', NULL) WHERE id = 1", vec![("Nullable(String)", chosen(0, "Nullable(String)", "Bool", ["paid", null]))]);
        assert_eq!(unheld, ["{paid:Bool} is in a value of the type Nullable(String) that is written to the column role, of the type Enum8('new' = 1, 'paid' = 2). The column holds no NULL, and an UPDATE that writes one fails. Write what a NULL becomes in the statement, as ifNull(…, …) does."]);
        // One that may be no label is a text, which ClickHouse reads as a label's number where it is one.
        let gone = refused("UPDATE t SET role = if({paid:Bool}, 'paid', '2') WHERE id = 1", vec![("String", chosen(0, "String", "Bool", ["paid", "2"]))]);
        assert_eq!(gone, [format!("{{paid:Bool}} is in a value of the type String that is written to the column role, of the type Enum8('new' = 1, 'paid' = 2). {converts} Convert the value in the statement, as CAST(… AS Enum8('new' = 1, 'paid' = 2)) does.")]);
        let other = "COLUMN id: #, column_name: name, result_type: String, source_id: 3";
        assert_eq!(refused("UPDATE t SET role = if({paid:Bool}, 'paid', name) WHERE id = 1", vec![("String", chosen(0, "String", "Bool", ["paid", other]))]).len(), 1);
        assert_eq!(refused("UPDATE t SET role = concat({s:String}, '') WHERE id = 1", vec![("String", computed_from(0, "String", &["String"]))]).len(), 1);
        // A column that holds no NULL: an INSERT writes its default value in place of one, and an UPDATE fails.
        let null = "{s:Nullable(String)} is written as it is to the column name, of the type String. The column holds no NULL, and";
        let declare = "Declare the parameter as a type that is never NULL, {s:String}, or write what a NULL becomes in the statement, as ifNull({s:Nullable(String)}, …) does.";
        assert_eq!(refused("INSERT INTO t (id, name) VALUES ({i:UInt8}, {s:Nullable(String)})", Vec::new()), [format!("{null} an INSERT writes the column's default value in place of one, with no error unless a setting asks for one. {declare}")]);
        assert_eq!(refused("UPDATE t SET name = {s:Nullable(String)} WHERE id = 1", vec![("Nullable(String)", computed_from(0, "Nullable(String)", &["Nullable(String)"]))]), [format!("{null} an UPDATE that writes one fails. {declare}")]);
        // A value computed from a parameter that may be NULL is one a declaration changes where arithmetic
        // alone computes it, and a NULL the statement makes is its own, whatever its parameters are.
        assert_eq!(refused("UPDATE t SET name = nullIf({s:Nullable(String)}, {none:String}) WHERE id = 1", vec![("Nullable(String)", computed_with(0, "nullIf", "Nullable(String)", &["Nullable(String)", "String"]))]), ["This value of the type Nullable(String) depends on a parameter and is written to the column name, of the type String. The column holds no NULL, and an UPDATE that writes one fails. Write what a NULL becomes in the statement, as ifNull(…, …) does."]);
        assert_eq!(refused("UPDATE t SET n = abs({x:Nullable(Int8)}) WHERE id = 1", vec![("Nullable(UInt8)", computed_from(0, "Nullable(UInt8)", &["Nullable(Int8)"]))]), ["{x:Nullable(Int8)} is in a value of the type Nullable(UInt8) that is written to the column n, of the type UInt8. The column holds no NULL, and an UPDATE that writes one fails. Declare each parameter it is computed from as a type that is never NULL, or write what a NULL becomes in the statement, as ifNull(…, …) does."]);
        // A value that may be NULL is held as one whatever its parameters are: a sum that may be NULL too, whose NULL is all the column does not keep.
        let written = "The column holds no NULL, and an INSERT writes the column's default value in place of one, with no error unless a setting asks for one. Write what a NULL becomes in the statement, as ifNull(…, …) does.";
        assert_eq!(refused("INSERT INTO t (id, n) VALUES ({i:UInt8}, nullIf({x:UInt8}, 0))", vec![("Nullable(UInt8)", computed_from(0, "Nullable(UInt8)", &["UInt8"]))]), [format!("{{x:UInt8}} is in a value of the type Nullable(UInt8) that is written to the column n, of the type UInt8. {written}")]);
        assert_eq!(refused("INSERT INTO t (id, n) VALUES ({i:UInt8}, nullIf({x:UInt8} + 1, 0))", vec![("Nullable(UInt16)", computed_from(0, "Nullable(UInt16)", &["UInt8"]))]), [format!("{{x:UInt8}} is in a value of the type Nullable(UInt16) that is written to the column n, of the type UInt8. {written}")]);
        let unknown = "      COLUMN id: 20, column_name: number, result_type: Nullable(UInt8), source_id: 99".to_string();
        assert_eq!(refused("INSERT INTO t (id, n) SELECT 1, nullIf(toUInt8(number), 0) FROM numbers({n:UInt8})", vec![("UInt8", computed_from(0, "UInt8", &[])), ("Nullable(UInt8)", unknown)]), [format!("{{n:UInt8}} is in a value of the type Nullable(UInt8) that is written to the column n, of the type UInt8. {written}")]);
    }

    #[test]
    fn what_an_insert_s_select_writes_is_held_by_what_the_server_says_of_it() {
        let column = |name: &str, ty: &str| vec![json!(name), json!(ty), json!("")];
        let asked = std::cell::RefCell::new(Vec::new());
        let server = |values: &'static [(&'static str, bool)]| {
            let asked = &asked;
            move |sent: &str| {
                asked.borrow_mut().push(sent.to_string());
                match sent {
                    "DESCRIBE TABLE u" => Ok(vec![column("id", "UInt8"), column("a", "UInt8"), column("at", "DateTime64(3)"), column("d", "Dynamic"), column("j", "JSON")]),
                    _ => Ok(written(values, sent).unwrap_or_default()),
                }
            }
        };
        let refused = |sql: &str, values: &'static [(&'static str, bool)]| run(sql, &server(values)).diagnostics.iter().map(|d| (sql[d.start..d.start + d.length].to_string(), d.message.clone())).collect::<Vec<_>>();
        let kept = "ClickHouse converts a value of another type to the column's, and changes one the column cannot hold without an error. Give each parameter the value is computed from a type the column keeps, or convert the value in the statement, as CAST(… AS UInt8) does.";
        // The SELECT is asked as it stands, a parenthesis around it and all, with each parameter a column: its
        // describe says each value's type, and its tree which of them depend on a parameter.
        let sql = "INSERT INTO u SELECT {i:UInt8} AS id, {x:UInt64} total, now64(3) FROM numbers(1) UNION ALL (SELECT 1, {y:Int16}, now64(3))";
        assert_eq!(refused(sql, &[("UInt8", true), ("UInt64", true), ("DateTime64(3)", false)]), [(sql.to_string(), format!("The statement writes a value of the type UInt64 that depends on a parameter to the column a, of the type UInt8. {kept}"))]);
        for question in [DESCRIBE, TREE] {
            let select = "SELECT materialize(defaultValueOfTypeName('UInt8')) AS id, materialize(defaultValueOfTypeName('UInt64')) total, now64(3) FROM numbers(1) UNION ALL (SELECT 1, materialize(defaultValueOfTypeName('Int16')), now64(3))";
            assert!(asked.borrow().iter().any(|s| s.strip_prefix(question).is_some_and(|rest| rest.trim_end_matches("\n)") == select)), "{:?}", asked.borrow());
        }
        // The one parameter of a SELECT is the one a value depends on.
        assert_eq!(refused("INSERT INTO u (id, a) (SELECT 1, :x::UInt64 + 0)", &[("UInt8", false), ("UInt64", true)]), [(":x".to_string(), format!(":x::UInt64 is in a value of the type UInt64 that is written to the column a, of the type UInt8. {kept}"))]);
        // A value the statement converts has the type it names, and a parameter that only keeps rows is in no value.
        assert!(refused("INSERT INTO u (id, a) WITH {x:UInt64} AS wide SELECT 1, CAST(wide AS UInt8)", &[("UInt8", false), ("UInt8", true)]).is_empty());
        assert!(refused("INSERT INTO u (id, a) SELECT id, total FROM orders WHERE id = {i:UInt64} LIMIT {n:UInt64}", &[("UInt64", false), ("Decimal(18, 2)", false)]).is_empty());
        // A Dynamic keeps a value of any type, and a column whose type no parameter declares takes a value the statement gives it.
        assert!(refused("INSERT INTO u (id, d) SELECT 1, {x:UInt64}", &[("UInt8", false), ("UInt64", true)]).is_empty());
        let json = refused("INSERT INTO u (id, j) SELECT 1, {x:String}", &[("UInt8", false), ("String", true)]);
        assert_eq!(json.len(), 1);
        assert!(json[0].1.ends_with("Give the value the column's type in the statement: no parameter declares that type."), "{}", json[0].1);
        assert!(refused("INSERT INTO u (id, j) SELECT 1, CAST({x:String}, 'JSON')", &[("UInt8", false), ("JSON", true)]).is_empty());
        // More values than columns is the server's to refuse, and a column the text does not say holds nothing.
        assert!(refused("INSERT INTO u (id) SELECT 1, {x:UInt64}", &[("UInt8", false), ("UInt64", true)]).is_empty());
        assert_eq!(refused("INSERT INTO FUNCTION remote('other', db.u) SELECT {x:UInt8}", &[("UInt8", true)]), [("{x:UInt8}".to_string(), "{x:UInt8} is in a value that is written to a table function's column, and Reylo reads no table function's columns, so nothing holds it to that column's type. Insert into a table.".to_string())]);
        // A tree the engine does not read says nothing of the values, so nothing would hold them.
        let unread = |sent: &str| if sent.starts_with(TREE) { Ok(vec![vec![json!("Expression")]]) } else { server(&[("UInt8", true)])(sent) };
        let sql = "INSERT INTO u (id) SELECT {x:UInt8}";
        assert_eq!(run(sql, &unread).diagnostics.iter().map(|d| d.message.as_str()).collect::<Vec<_>>(), ["Reylo cannot read the type of a value this statement writes with a parameter, so nothing would hold the value to its column."]);
        // A parameter the server takes as a constant alone: it has read the parameter's value, so a type may be
        // another with another value. The SELECT is asked with each constant tried, and a value is held
        // where a conversion in the statement names its type, each type an answer gives being one the column keeps.
        let constant = |top: &'static str, types: [&'static str; 2], named: bool| move |sent: &str| match sent.strip_prefix(DESCRIBE).or(sent.strip_prefix(TREE)) {
            Some(text) if text.contains("materialize(") => Err(error("Code: 44. DB::Exception: Illegal column. (ILLEGAL_COLUMN) (version 26.9)", "44")),
            // The value's tree: a function at its top, whose last argument is the type a CAST names, written or computed.
            Some(_) if sent.starts_with(TREE) => Ok(format!(
                "QUERY id: 0\n  PROJECTION COLUMNS\n    s UInt8\n  PROJECTION\n    LIST id: 1, nodes: 1\n      CONSTANT id: 2, constant_value: UInt64_1, constant_value_type: UInt8\n        EXPRESSION\n          FUNCTION id: 3, function_name: {top}, function_type: ordinary, result_type: UInt8\n            ARGUMENTS\n              LIST id: 4, nodes: 2\n                CONSTANT id: 6, constant_value: UInt64_1, constant_value_type: UInt8\n                  EXPRESSION\n                    FUNCTION id: 7, function_name: CAST, function_type: ordinary, result_type: UInt8\n                CONSTANT id: 8, constant_value: 'UInt8', constant_value_type: String{}",
                if named { "" } else { "\n                  EXPRESSION\n                    FUNCTION id: 9, function_name: if, function_type: ordinary, result_type: String" }
            )
            .lines()
            .map(|line| vec![json!(line)])
            .collect()),
            Some(text) => Ok(vec![vec![json!("s"), json!(types[usize::from(text.contains("CAST(2, 'UInt8')"))])]]),
            None => server(&[])(sent),
        };
        let messages = |sql: &str, answer: &dyn Fn(&str) -> Result<Vec<Vec<Value>>, DbError>| run(sql, answer).diagnostics.iter().map(|d| d.message.clone()).collect::<Vec<_>>();
        let unfixed = "{n:UInt8} is in a value that is written to the column id, of the type UInt8. ClickHouse reads a parameter of this statement as a constant, and the type of a value computed with one may change with the parameter's value. Give the value one type in the statement, as CAST(… AS UInt8) does.";
        assert_eq!(messages("INSERT INTO u (id) SELECT tupleElement((1, 2), {n:UInt8})", &constant("tupleElement", ["UInt8", "UInt8"], true)), [unfixed]);
        assert!(messages("INSERT INTO u (id) SELECT CAST(tupleElement((1, 2), {n:UInt8}) AS UInt8)", &constant("CAST", ["UInt8", "UInt8"], true)).is_empty());
        // A CAST to a type the statement computes names no one type.
        assert_eq!(messages("INSERT INTO u (id) SELECT CAST(1, if({n:UInt8} = 12, 'UInt64', 'UInt8'))", &constant("CAST", ["UInt8", "UInt8"], false)), [unfixed]);
        // A conversion may still name types that differ, as one to a DateTime keeps its value's zone: each is held.
        assert!(messages("INSERT INTO u (at) SELECT CAST(toTimeZone(now(), {n:UInt8}) AS DateTime)", &constant("_CAST", ["DateTime('UTC')", "DateTime('Asia/Tokyo')"], true)).is_empty());
        // And a CAST names its value's type only where the server has it keep no nullability of its value's.
        let keeping = |sent: &str| if sent == CONVERSION_SETTINGS { Ok(vec![vec![json!(KEEPS_NULLABLE), json!("1")]]) } else { constant("CAST", ["UInt8", "UInt8"], true)(sent) };
        assert_eq!(messages("INSERT INTO u (id) SELECT CAST(tupleElement((1, 2), {n:UInt8}) AS UInt8)", &keeping), ["{n:UInt8} is in a value that is written to the column id, of the type UInt8. ClickHouse reads a parameter of this statement as a constant, and the type of a value computed with one may change with the parameter's value. A CAST gives a value one type only where the setting cast_keep_nullable is off, and it is on for this statement: turn it off."]);
        assert_eq!(messages("INSERT INTO u (id) SELECT CAST(tupleElement((1, 2), {n:UInt8}) AS UInt8) SETTINGS cast_keep_nullable = 1", &constant("CAST", ["UInt8", "UInt8"], true)).len(), 1);
        let either = messages("INSERT INTO u (id) SELECT CAST(x AS {n:UInt8})", &constant("CAST", ["UInt8", "UInt16"], true));
        assert_eq!(either.len(), 1, "{either:?}");
        assert!(either[0].starts_with("{n:UInt8} is in a value of the type UInt16 that is written to the column id, of the type UInt8."), "{}", either[0]);
        // A SELECT with no parameter writes nothing that depends on one, and is asked nothing of its values.
        asked.borrow_mut().clear();
        assert!(refused("INSERT INTO u (id, a) SELECT 1000, 2", &[]).is_empty());
        assert!(!asked.borrow().iter().any(|s| s.starts_with(TREE) || s.starts_with(DESCRIBE)), "{:?}", asked.borrow());
        // An INSERT whose rows the text does not show is refused where it holds a parameter.
        let hidden = refused("INSERT INTO u SETTINGS max_threads = {n:UInt64}", &[]);
        assert_eq!(hidden.iter().map(|(_, message)| message.as_str()).collect::<Vec<_>>(), ["Reylo cannot read what this INSERT writes, so nothing would hold its parameters to their columns. Write its rows as VALUES, or as a SELECT."]);
    }

    #[test]
    fn each_row_of_values_is_checked_as_the_select_of_its_values() {
        let asked = std::cell::RefCell::new(Vec::new());
        let server = |sent: &str| {
            asked.borrow_mut().push(sent.to_string());
            if sent.starts_with(PIPELINE) && sent.contains("nonexistent") {
                Err(error("Code: 47. DB::Exception: Unknown expression or function identifier `nonexistent`. (UNKNOWN_IDENTIFIER) (version 26.9)", "47"))
            } else {
                Ok(Vec::new())
            }
        };
        let sql = "INSERT INTO t (id, u) VALUES ({id:UInt8}, generateUUIDv4()), ({id:UInt8}, DEFAULT), (3, {'a': 1}), (2, nonexistent + 1)";
        let refused = run(sql, &server);
        assert_eq!(refused.diagnostics.iter().map(|d| (&sql[d.start..d.start + d.length], d.message.as_str())).collect::<Vec<_>>(), [("(2, nonexistent + 1)", "Unknown expression or function identifier `nonexistent`. (UNKNOWN_IDENTIFIER)")]);
        // The first row as a SELECT, its parameter a column. A row that writes a DEFAULT, or a map
        // as VALUES alone reads one, is no SELECT, and is asked nothing.
        let sent = asked.borrow();
        assert!(sent.iter().any(|s| s == "EXPLAIN PIPELINE\nINSERT INTO t (id, u) SELECT materialize(defaultValueOfTypeName('UInt8')), generateUUIDv4()"), "{sent:?}");
        assert!(!sent.iter().any(|s| s.starts_with(PIPELINE) && (s.contains("DEFAULT") || s.contains("'a'"))), "{sent:?}");
        drop(sent);
        // VALUES reads a value for a Variant by the column's type, and a SELECT by the value's own: a 5 is
        // the Variant's UInt64 to the one and a UInt8 to the other. So a row for such a table is described
        // as the SELECT of its values, which checks what each computes, and is not built into the table.
        asked.borrow_mut().clear();
        let variant = |sent: &str| {
            asked.borrow_mut().push(sent.to_string());
            match sent {
                "DESCRIBE TABLE t" => Ok(vec![vec![json!("id"), json!("UInt8"), json!("")], vec![json!("v"), json!("Variant(String, UInt64)"), json!("")]]),
                _ if sent.starts_with(PIPELINE) && sent.contains("SELECT") => Err(error("Code: 70. DB::Exception: Cannot convert. (CANNOT_CONVERT_TYPE) (version 26.9)", "70")),
                _ if sent.starts_with(DESCRIBE) && sent.contains("nonexistent") => Err(error("Code: 47. DB::Exception: Unknown expression or function identifier `nonexistent`. (UNKNOWN_IDENTIFIER) (version 26.9)", "47")),
                _ => Ok(Vec::new()),
            }
        };
        let read = run("INSERT INTO t (id, v) VALUES (1, 5), ({id:UInt8}, 'x')", &variant);
        assert!(read.diagnostics.is_empty(), "{:?}", read.diagnostics);
        assert!(asked.borrow().iter().any(|s| s == "EXPLAIN PIPELINE\nINSERT INTO t (id, v) VALUES (1, 5), (materialize(defaultValueOfTypeName('UInt8')), 'x')"), "{:?}", asked.borrow());
        assert!(asked.borrow().iter().any(|s| s == "DESCRIBE (\nSELECT 1, 5\n)") && asked.borrow().iter().any(|s| s == "DESCRIBE (\nSELECT materialize(defaultValueOfTypeName('UInt8')), 'x'\n)"), "{:?}", asked.borrow());
        let sql = "INSERT INTO t (id, v) VALUES (1, 5), (2, nonexistent + 1)";
        let unknown = run(sql, &variant);
        assert_eq!(unknown.diagnostics.iter().map(|d| &sql[d.start..d.start + d.length]).collect::<Vec<_>>(), ["(2, nonexistent + 1)"]);
    }

    #[test]
    fn a_long_list_of_rows_and_a_word_against_a_placeholder_are_read() {
        // A column named as a clause is, a table named as one, and rows by the thousand.
        let table = |sent: &str| match sent {
            "DESCRIBE TABLE format" => Ok(vec![vec![json!("id"), json!("UInt32"), json!("")], vec![json!("values"), json!("String"), json!("")]]),
            _ => Ok(Vec::new()),
        };
        let rows: String = (0..4000).map(|i| format!("({i}, 'a (b), c')")).collect::<Vec<_>>().join(", ");
        let sql = format!("INSERT INTO format (id, values) VALUES {rows}, ({{id:UInt64}}, {{s:String}})");
        let long = run(&sql, &table);
        assert_eq!(long.diagnostics.iter().map(|d| &sql[d.start..d.start + d.length]).collect::<Vec<_>>(), ["{id:UInt64}"]);
        // What stands for a placeholder is parted from a word it is written against.
        let asked = std::cell::RefCell::new(Vec::new());
        let ok = |sent: &str| {
            asked.borrow_mut().push(sent.to_string());
            server(&[("id", "UInt64")], "QUERY id: 0")(sent)
        };
        let against = run("SELECT id FROM users WHERE id IN{ids:Array(UInt64)}AND age > {age:UInt8}", &ok);
        assert!(against.diagnostics.is_empty(), "{:?}", against.diagnostics);
        let sent = asked.borrow();
        assert!(sent.iter().any(|s| s.contains(" IN CAST(") || s.contains(" IN materialize(")) && !sent.iter().any(|s| s.contains("INCAST") || s.contains("INmaterialize") || s.contains(")AND")), "{sent:?}");
    }

    #[test]
    fn a_write_that_reads_as_no_select_is_refused_and_a_cluster_or_a_partition_is_read_past() {
        let asked = std::cell::RefCell::new(Vec::new());
        let ok = |sent: &str| {
            asked.borrow_mut().push(sent.to_string());
            Ok(Vec::new())
        };
        let described = |sql: &str| {
            asked.borrow_mut().clear();
            let analysis = run(sql, &ok);
            assert!(analysis.diagnostics.is_empty(), "{:?}", analysis.diagnostics);
            asked.borrow().iter().find(|s| s.starts_with(DESCRIBE)).cloned().unwrap_or_default()
        };
        assert_eq!(described("DELETE FROM t ON CLUSTER main IN PARTITION tuple() WHERE id = {id:UInt64}"), "DESCRIBE (\nSELECT 1 FROM t  WHERE ( tuple() ) IS NOT NULL AND id = materialize(defaultValueOfTypeName('UInt64'))\n)");
        // Each value an UPDATE writes is converted to its column's type, as the UPDATE converts it.
        assert_eq!(described("UPDATE t ON CLUSTER 'c' SET a = 1 IN PARTITION ID '2024' WHERE id = 1"), "DESCRIBE (\nFROM t  SELECT a , CAST( 1, toTypeName(a)) WHERE ( '2024' ) IS NOT NULL AND id = 1\n)");
        // A partition's id is the statement's to write: no check can stand a value in for a parameter there.
        let by_parameter = "DELETE FROM t IN PARTITION ID {part:String} WHERE id = 1";
        let refused_id = run(by_parameter, &ok);
        assert_eq!(refused_id.diagnostics.iter().map(|d| (&by_parameter[d.start..d.start + d.length], d.message.as_str())).collect::<Vec<_>>(), [("{part:String}", "Reylo types no parameter as a partition's id, which the server reads from the statement's text where a write is checked. Write the id in the statement in place of {part:String}, or name the partition by its key's value, as IN PARTITION {day:Date} does.")]);
        assert_eq!(described("UPDATE t SET a = b + 1 -- one more\n, `n.x` = [1] /* all of it */ WHERE id = 1"), "DESCRIBE (\nFROM t SELECT a , CAST( b + 1, toTypeName(a)) -- one more\n, `n.x` , CAST( [1], toTypeName(`n.x`)) /* all of it */ WHERE id = 1\n)");
        // A statement the edits make no SELECT of is refused, with what the server says of that SELECT.
        let unparsed = |sent: &str| if sent.starts_with(AST) && sent.contains("SELECT") { Err(error("Code: 62. DB::Exception: Syntax error: failed at position 20 (x). (SYNTAX_ERROR) (version 26.9)", "62")) } else { Ok(Vec::new()) };
        let refused = run("DELETE FROM t WHERE id = 1 SETTINGS x", &unparsed);
        assert!(refused.diagnostics[0].message.starts_with("Reylo checks a DELETE or an UPDATE as the SELECT of the rows it changes and the values it writes, and it cannot read this one as that SELECT (Syntax error: failed at position 20 (x). (SYNTAX_ERROR)), so nothing would check it."), "{}", refused.diagnostics[0].message);
        // An ALTER of several commands, each of which would be a SELECT of its own; a temporary table's reads as any other's.
        let parse = |commands: &'static str| move |sent: &str| match sent.strip_prefix(AST) {
            Some(text) if text.starts_with("ALTER") => Ok(format!("AlterQuery  t (children 2)\n ExpressionList (children 1)\n{commands}").lines().map(|line| vec![json!(line)]).collect()),
            _ => ok(sent),
        };
        let several = run("ALTER TABLE t UPDATE a = 1 WHERE 1, DELETE WHERE 0", &parse("  AlterCommand UPDATE (children 2)\n  AlterCommand DELETE (children 1)"));
        assert_eq!(several.diagnostics[0].message, "Reylo types an ALTER TABLE of one command, an UPDATE or a DELETE, and this one has 2: write each as a statement of its own.");
        asked.borrow_mut().clear();
        let temporary = run("ALTER TEMPORARY TABLE t UPDATE a = 1 WHERE id = 1", &parse("  AlterCommand UPDATE (children 2)"));
        assert!(temporary.diagnostics.is_empty(), "{:?}", temporary.diagnostics);
        assert!(asked.borrow().iter().any(|s| s == "DESCRIBE (\nFROM t SELECT a , CAST( 1, toTypeName(a)) WHERE id = 1\n)"), "{:?}", asked.borrow());
    }

    #[test]
    fn a_describe_s_subcolumns_are_no_columns_of_the_result() {
        // What a session's describe_include_subcolumns adds after a statement's own columns.
        let described = |sent: &str| {
            if sent.starts_with(DESCRIBE) {
                let row = |name: &str, ty: &str, subcolumn: u8| vec![json!(name), json!(ty), json!(""), json!(""), json!(""), json!(""), json!(""), json!(subcolumn)];
                Ok(vec![row("t", "Tuple(UInt8, String)", 0), row("n", "Nullable(UInt8)", 0), row("t.1", "UInt8", 1), row("t.2", "String", 1), row("n.null", "UInt8", 1)])
            } else {
                server(&[], "QUERY id: 0")(sent)
            }
        };
        let typed = run("SELECT t, n FROM x", &described);
        assert_eq!(typed.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["t", "n"], "{:?}", typed.diagnostics);
    }
}

//! The prose written in comments before a statement: its summary, as an editor's hover shows it
//! (the format's `comment`, `spec/index.ts`).

use crate::dialect::Dialect;
use crate::lexical::Rules;

/// The comments before a statement, as prose, and the byte offset where they end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leading {
    pub prose: String,
    pub end: usize,
}

/// The prose of the comments before a statement, where it has any: an analysis's `comment`, which
/// every host's analysis carries ([`crate::api::call`], [`crate::replay`]).
pub fn prose(sql: &str, dialect: Dialect) -> Option<String> {
    let prose = leading(sql, dialect).prose;
    (!prose.is_empty()).then_some(prose)
}

/// The `--` lines and `/* … */` blocks before the statement's first token, markers stripped and
/// lines kept: a blank line between two comments, or a change from one form to the other, starts a
/// paragraph. Read as the dialect reads comments and whitespace ([`Rules`]): Postgres and
/// ClickHouse nest block comments, Postgres ends a `--` one at a carriage return as at a line
/// feed, MySQL's `--` is one only before a space or a control character and `#` starts one too,
/// as it does on ClickHouse before a space or a `!`, and MySQL's `/*!` and `/*+` are code to the
/// server, so they end the prose. A no-break space before a comment is a name's character but on
/// ClickHouse, so the comment stands after the statement's first token.
pub fn leading(sql: &str, dialect: Dialect) -> Leading {
    let rules = Rules::of(dialect);
    let bytes = sql.as_bytes();
    let mut paragraphs: Vec<Vec<String>> = Vec::new();
    let mut last: Option<bool> = None; // Some(true) after a line comment, Some(false) after a block
    let mut i = 0;
    let mut end = 0;
    loop {
        let at = rules.skip_spaces(bytes, i);
        // A blank line between two comments starts a paragraph: two line ends, `\r\n` being one,
        // and a bare carriage return one where it ends a line comment.
        let gap = &sql[i..at];
        let breaks = if rules.carriage_return_ends_comment { gap.matches('\n').count() + gap.matches('\r').count() - gap.matches("\r\n").count() } else { gap.matches('\n').count() };
        let blank_before = breaks > 1;
        let (ch, next) = (bytes.get(at).copied(), bytes.get(at + 1).copied());
        let mysql = dialect == Dialect::Mysql;
        if rules.line_comment_at(bytes, at) {
            let stop = rules.line_comment_end(bytes, at);
            let text = &sql[at + if ch == Some(b'#') { 1 } else { 2 }..stop];
            if last != Some(true) || blank_before || paragraphs.is_empty() {
                paragraphs.push(Vec::new());
            }
            paragraphs.last_mut().expect("a paragraph").push(text.to_string());
            last = Some(true);
            i = stop;
            end = stop;
            continue;
        }
        if ch == Some(b'/') && next == Some(b'*') && !(mysql && matches!(bytes.get(at + 2), Some(b'!' | b'+'))) {
            // One that nothing closes is prose to the end of the text.
            let closed = rules.block_comment_end(bytes, at).filter(|&j| j >= at + 4 && sql[..j].ends_with("*/"));
            let j = closed.unwrap_or(bytes.len());
            let text = &sql[at + 2..if closed.is_some() { j - 2 } else { j }];
            paragraphs.push(block(text));
            last = Some(false);
            i = j;
            end = j;
            continue;
        }
        break;
    }
    let prose = paragraphs.into_iter().map(dedent).filter(|p| !p.is_empty()).map(|p| p.join("\n")).collect::<Vec<_>>().join("\n\n");
    Leading { prose, end }
}

/// A block comment's lines, without a `/**`'s extra stars or the `*` that starts each line of one
/// written as JSDoc is. A line ends at `\r\n`, `\n` or a bare `\r`: inside a comment a line end
/// means nothing to any database, so each is read as an editor shows it.
fn block(text: &str) -> Vec<String> {
    let text = text.trim_start_matches('*').replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    let rest: Vec<&&str> = lines[1..].iter().filter(|l| !l.trim().is_empty()).collect();
    let starred = |l: &str| l.trim_start().starts_with('*');
    if !rest.is_empty() && rest.iter().all(|l| starred(l)) {
        let mut out = vec![lines[0].to_string()];
        for l in &lines[1..] {
            let trimmed = l.trim_start();
            out.push(match trimmed.strip_prefix('*') {
                Some(after) if !after.starts_with('/') => after.strip_prefix(' ').unwrap_or(after).to_string(),
                _ => l.to_string(),
            });
        }
        return out;
    }
    lines.into_iter().map(str::to_string).collect()
}

/// Lines without their trailing space, the leading and trailing blank ones, and the indentation
/// they share.
fn dedent(lines: Vec<String>) -> Vec<String> {
    let mut trimmed: Vec<String> = lines.iter().map(|l| l.trim_end().to_string()).collect();
    while trimmed.first().is_some_and(String::is_empty) {
        trimmed.remove(0);
    }
    while trimmed.last().is_some_and(String::is_empty) {
        trimmed.pop();
    }
    let indent_of = |l: &str| l.len() - l.trim_start_matches([' ', '\t']).len();
    let indent = trimmed.iter().filter(|l| !l.is_empty()).map(|l| indent_of(l)).min().unwrap_or(0);
    trimmed.iter().map(|l| l[indent.min(indent_of(l))..].to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prose(sql: &str, dialect: Dialect) -> String {
        leading(sql, dialect).prose
    }

    #[test]
    fn line_and_block_comments_become_paragraphs() {
        assert_eq!(prose("-- One user,\n--   by id.\nselect 1", Dialect::Postgres), "One user,\n  by id.");
        assert_eq!(prose("-- a\n\n-- b\nselect 1", Dialect::Postgres), "a\n\nb");
        assert_eq!(prose("/**\n * Lists users.\n *\n * Newest first.\n */\nselect 1", Dialect::Postgres), "Lists users.\n\nNewest first.");
        assert_eq!(prose("-- a\n/* b */ select 1", Dialect::Postgres), "a\n\nb");
        assert_eq!(prose("select 1 -- not before", Dialect::Postgres), "");
        assert_eq!(leading("-- a\nselect 1", Dialect::Postgres).end, 4);
        // Postgres ends a `--` comment at a carriage return as at a line feed.
        assert_eq!(prose("-- One user,\r--   by id.\rselect 1", Dialect::Postgres), "One user,\n  by id.");
        assert_eq!(leading("-- a\rselect 1", Dialect::Postgres).end, 4);
        assert_eq!(prose("-- a\r\n\r\n-- b\r\nselect 1", Dialect::Postgres), "a\n\nb");
        assert_eq!(prose("-- a\r\r-- b\rselect 1", Dialect::Postgres), "a\n\nb");
        assert_eq!(prose("-- a\r\n-- b\r\nselect 1", Dialect::Postgres), "a\nb");
    }

    #[test]
    fn each_dialect_reads_its_own_comments() {
        assert_eq!(prose("/* outer /* inner */ still */ select 1", Dialect::Postgres), "outer /* inner */ still");
        assert_eq!(prose("/* outer /* inner */ select 1", Dialect::Sqlite), "outer /* inner");
        assert_eq!(prose("# hash\n--x\nselect 1", Dialect::Mysql), "hash");
        assert_eq!(prose("-- dash\n/*! code */ select 1", Dialect::Mysql), "dash");
        assert_eq!(prose("# not a comment\nselect 1", Dialect::Postgres), "");
    }

    #[test]
    fn the_corpus() {
        let corpus = crate::lexical::Corpus::load();
        for dialect in [Dialect::Postgres, Dialect::Sqlite, Dialect::Mysql, Dialect::Clickhouse] {
            for (case, reading) in corpus.on(dialect) {
                if let Some(expected) = &reading.prose {
                    assert_eq!(&prose(&case.text, dialect), expected, "{dialect:?}, {}: {:?}", case.about, case.text);
                }
            }
        }
    }
}

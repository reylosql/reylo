//! Lossless SQL syntax trees. Every byte of a statement's text belongs to one token of its tree or
//! to the whitespace and comments before one, so a tree prints back the text it was read from, and
//! an invalid statement still has a tree, with what could not be read under error nodes.

pub mod lalr;
pub mod lemon;
pub mod mysql;
pub mod postgres;
pub mod sqlite;
pub mod tolerant;
pub mod tree;

pub use tree::{Node, NodeId, SyntaxError, Tree, ERROR_KIND};

/// A database whose SQL this crate reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Dialect {
    Postgres,
    Sqlite,
    Mysql,
    ClickHouse,
    SqlServer,
}

/// Reads `text` into a lossless tree: every byte belongs to a token or the trivia before one, and text a dialect can't read stays in the tree, under an error node or with an error beside it. Postgres and SQLite follow the database's own grammar, so their errors are its syntax errors, and MySQL follows TiDB's grammar of it, with MySQL's message. ClickHouse and SQL Server are read for their tokens, statements, clauses and parentheses, so their errors are only what doesn't balance or end.
pub fn parse(dialect: Dialect, text: &str) -> Tree<'_> {
    match dialect {
        Dialect::Postgres => postgres::parse_tree(text),
        Dialect::Sqlite => sqlite::parse_tree(text),
        Dialect::Mysql => mysql::parse_tree(text),
        Dialect::ClickHouse => tolerant::parse(tolerant::Flavor::ClickHouse, text),
        Dialect::SqlServer => tolerant::parse(tolerant::Flavor::SqlServer, text),
    }
}

/// Where each statement of `tree` starts and ends, its closing `;` left out.
pub fn statements(dialect: Dialect, tree: &Tree<'_>) -> Vec<(u32, u32)> {
    let name = match dialect {
        Dialect::Postgres => "toplevel_stmt",
        Dialect::Sqlite => "ecmd",
        Dialect::Mysql => "Statement",
        Dialect::ClickHouse | Dialect::SqlServer => "statement",
    };
    let mut out: Vec<(u32, u32)> = Vec::new();
    // Deleted tokens no statement has taken yet: where they start and end.
    let mut pending: Option<(u32, u32)> = None;
    // Where each written `;` starts, read once, when a deleted token first needs it.
    let mut ends: Option<Vec<u32>> = None;
    let mut stack = vec![tree.root];
    while let Some(id) = stack.pop() {
        let n = tree.node(id);
        // A token a repair deleted belongs to the statement it sits in: the one before it, up to that statement's `;`, else the next.
        if n.kind == ERROR_KIND && n.leaf {
            match (out.last_mut(), pending) {
                (Some(last), None) if !after_end(tree, &mut ends, last.1, n.start) => last.1 = n.end,
                // A `;` between them makes the earlier deletions a statement of their own.
                (_, Some((start, end))) if after_end(tree, &mut ends, end, n.start) => {
                    out.push((start, end));
                    pending = Some((n.start, n.end));
                }
                (_, Some((start, _))) => pending = Some((start, n.end)),
                _ => pending = Some((n.start, n.end)),
            }
            continue;
        }
        if n.kind == ERROR_KIND || (!n.leaf && kind_name(dialect, n.kind) == name) {
            // Where its last token before any statement end stops.
            let is_end = |kind: u16| match dialect {
                Dialect::Postgres | Dialect::Sqlite | Dialect::Mysql => false,
                _ => kind == tolerant::SEMI,
            };
            let last = tree.leaves_of(id).map(|l| tree.node(l)).filter(|l| l.end > l.start && !is_end(l.kind)).last();
            if let Some(last) = last {
                // A `/*! … */` comment's close, which trivia holds, belongs to the statement it closes in, and so does
                // its opener, where the comment holds the statement whole.
                let last_end = if dialect == Dialect::Mysql { mysql::scan::conditional_close(tree.text, last.end as usize) as u32 } else { last.end };
                let first = tree.leaves_of(id).map(|l| tree.node(l)).find(|l| l.end > l.start);
                let n_start = match first {
                    Some(f) if last_end > last.end => mysql::scan::conditional_open(tree.text, f.lead as usize, f.start as usize) as u32,
                    _ => n.start,
                };
                let text = &tree.text[n_start as usize..last_end as usize];
                let kept = text.trim_end_matches(|c: char| c == ';' || c.is_whitespace()).len() as u32;
                if kept > 0 {
                    let start = match pending.take() {
                        Some((start, end)) if after_end(tree, &mut ends, end, n_start) => {
                            out.push((start, end));
                            n_start
                        }
                        Some((start, _)) => start,
                        None => n_start,
                    };
                    out.push((start, n_start + kept));
                }
            }
        } else {
            stack.extend(tree.children(id).iter().rev());
        }
    }
    if let Some(span) = pending {
        out.push(span);
    }
    out
}

/// Whether a statement end, a leaf whose text is `;`, sits between `from` and `to`. `ends` holds where each starts, in order, once it has been read.
fn after_end(tree: &Tree<'_>, ends: &mut Option<Vec<u32>>, from: u32, to: u32) -> bool {
    let ends = ends.get_or_insert_with(|| tree.leaves().map(|l| tree.node(l)).filter(|l| &tree.text[l.start as usize..l.end as usize] == ";").map(|l| l.start).collect());
    let i = ends.partition_point(|at| *at < from);
    ends.get(i).is_some_and(|at| *at < to)
}

/// The dialect's keywords, as its lexer spells them.
pub fn keywords(dialect: Dialect) -> Vec<&'static str> {
    match dialect {
        Dialect::Postgres => postgres::keywords().collect(),
        Dialect::Sqlite => sqlite::generated::KEYWORDS.iter().map(|k| k.0).collect(),
        Dialect::Mysql => mysql::keywords().collect(),
        Dialect::ClickHouse | Dialect::SqlServer => tolerant::KEYWORDS.to_vec(),
    }
}

/// The words `dialect` won't read as a bare name, which a name spelled like one needs quotes for.
pub fn reserved_words(dialect: Dialect) -> Vec<&'static str> {
    match dialect {
        Dialect::Postgres => postgres::generated::KEYWORDS.iter().filter(|k| k.2 != 0).map(|k| k.0).collect(),
        Dialect::Sqlite => sqlite::generated::KEYWORDS.iter().filter(|k| sqlite::generated::YY_FALLBACK[k.1 as usize] == 0).map(|k| k.0).collect(),
        Dialect::Mysql => mysql::generated::RESERVED.to_vec(),
        Dialect::ClickHouse | Dialect::SqlServer => tolerant::KEYWORDS.to_vec(),
    }
}

/// The tokens an error says the grammar would have taken, spelled for a reader: a keyword in capitals, punctuation as written, and a class of token by what it is, as "a name". Tokens no text spells, and repeats, are left out.
pub fn expected(dialect: Dialect, error: &SyntaxError) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for kind in &error.expected {
        let name = kind_name(dialect, *kind);
        let spelled = match (dialect, name) {
            (_, "$end" | "END_OF_FILE") => Some("end of input".to_string()),
            (Dialect::Postgres, "IDENT") | (Dialect::Sqlite, "ID") => Some("a name".to_string()),
            (Dialect::Postgres, "SCONST") | (Dialect::Sqlite, "STRING") => Some("a string".to_string()),
            (Dialect::Postgres, "ICONST" | "FCONST") | (Dialect::Sqlite, "INTEGER" | "FLOAT") => Some("a number".to_string()),
            (Dialect::Postgres, "PARAM") | (Dialect::Sqlite, "VARIABLE") => Some("a parameter".to_string()),
            (Dialect::Postgres, "Op") => Some("an operator".to_string()),
            (Dialect::Postgres, "TYPECAST") => Some("::".to_string()),
            (Dialect::Postgres, "DOT_DOT") => Some("..".to_string()),
            (Dialect::Postgres, "COLON_EQUALS") => Some(":=".to_string()),
            (Dialect::Postgres, "EQUALS_GREATER") => Some("=>".to_string()),
            (Dialect::Postgres, "LESS_EQUALS") => Some("<=".to_string()),
            (Dialect::Postgres, "GREATER_EQUALS") => Some(">=".to_string()),
            (Dialect::Postgres, "NOT_EQUALS") => Some("<>".to_string()),
            // Spellings the scanner makes of other tokens, a mode a caller sets, and comments.
            (Dialect::Postgres, n) if n.starts_with("MODE_") || matches!(n, "UIDENT" | "USCONST" | "BCONST" | "XCONST" | "SQL_COMMENT" | "C_COMMENT") => None,
            (_, n) if n.len() == 3 && n.starts_with('\'') && n.ends_with('\'') => Some(n[1..2].to_string()),
            (Dialect::Postgres, n) if n.chars().all(|c| c.is_ascii_uppercase() || c == '_') => Some(n.trim_end_matches("_P").trim_end_matches("_LA").to_string()),
            (Dialect::Sqlite, n) => sqlite::spelled(n),
            (Dialect::Mysql, _) => mysql::spelled(*kind),
            _ => None,
        };
        if let Some(s) = spelled.filter(|s| !out.contains(s)) {
            out.push(s);
        }
    }
    out
}

/// The name of a node's kind in `dialect`'s trees.
pub fn kind_name(dialect: Dialect, kind: u16) -> &'static str {
    match dialect {
        Dialect::Postgres => postgres::kind_name(kind),
        Dialect::Sqlite => sqlite::kind_name(kind),
        Dialect::Mysql => mysql::kind_name(kind),
        Dialect::ClickHouse | Dialect::SqlServer => tolerant::kind_name(kind),
    }
}

/// The hash of this crate's sources (`build.rs`), which an engine's fingerprint combines with its own.
#[doc(hidden)]
pub const SOURCE_HASH: &str = env!("REYLO_SQL_HASH");

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(dialect: Dialect, text: &str) -> Vec<&str> {
        statements(dialect, &parse(dialect, text)).into_iter().map(|(a, b)| &text[a as usize..b as usize]).collect()
    }

    #[test]
    fn a_statement_keeps_the_close_of_a_conditional_comment() {
        assert_eq!(spans(Dialect::Mysql, "SELECT 1 /*! + 2 */; select 3"), ["SELECT 1 /*! + 2 */", "select 3"]);
        assert_eq!(spans(Dialect::Mysql, "/*!80000 SELECT 1 */; select 3"), ["/*!80000 SELECT 1 */", "select 3"]);
        assert_eq!(spans(Dialect::Mysql, "-- note\n/*!99999 skipped */ /*!80000 SELECT 1 */;"), ["/*!80000 SELECT 1 */"]);
        assert_eq!(spans(Dialect::Mysql, "/* note */ SELECT 1;"), ["SELECT 1"]);
    }

    #[test]
    fn what_a_postgres_action_refuses_is_an_error_of_the_tree() {
        let text = "select 1 fetch first row with ties";
        let tree = parse(Dialect::Postgres, text);
        assert_eq!(tree.print(), text);
        assert_eq!(tree.errors[0].message, "WITH TIES cannot be specified without ORDER BY clause");
    }

    #[test]
    fn a_refusal_reached_before_a_syntax_error_comes_first() {
        let text = "SELECT 2 FETCH FIRST ROW WITH TIES ); SELECT 3;";
        assert_eq!(parse(Dialect::Postgres, text).errors[0].message, "WITH TIES cannot be specified without ORDER BY clause");
        assert_eq!(spans(Dialect::Postgres, text), ["SELECT 2 FETCH FIRST ROW WITH TIES )", "SELECT 3"]);
        let deep = format!("select {}1{})", "(".repeat(3000), ")".repeat(3000));
        assert!(!parse(Dialect::Postgres, &deep).errors.is_empty());
    }

    #[test]
    fn the_parser_stack_limit_is_one_error_at_its_token() {
        let text = format!("select {}1", "(".repeat(12_000));
        let tree = parse(Dialect::Postgres, &text);
        assert_eq!(tree.errors.len(), 1);
        assert_eq!(tree.errors[0].message, "memory exhausted at or near \"(\"");
        assert_eq!(tree.errors[0].end - tree.errors[0].start, 1);
    }

    /// The kinds of the inner nodes of `text`'s tree, and its errors' messages with what each expected.
    fn repaired(dialect: Dialect, text: &str) -> (Tree<'_>, Vec<(String, Vec<String>)>) {
        let tree = parse(dialect, text);
        assert_eq!(tree.print(), text);
        let errors = tree.errors.iter().map(|e| (e.message.clone(), expected(dialect, e))).collect();
        (tree, errors)
    }

    fn has_error_node(tree: &Tree<'_>) -> bool {
        tree.nodes.iter().any(|n| n.kind == ERROR_KIND && !n.leaf)
    }

    #[test]
    fn a_statement_is_repaired_where_one_token_is_missing_or_extra() {
        for (dialect, text) in [
            (Dialect::Postgres, "select a, from t; select 2"),
            (Dialect::Postgres, "select (a + 1 from t where b = 2"),
            (Dialect::Postgres, "select a from t where where b = 1"),
            (Dialect::Sqlite, "select a, from t; select 2"),
            (Dialect::Sqlite, "select a from t where where b = 1"),
        ] {
            let (tree, errors) = repaired(dialect, text);
            assert_eq!(errors.len(), 1, "{text}: {errors:?}");
            // The statement keeps its structure past the error: no stretch of it goes under an error node.
            assert!(!has_error_node(&tree), "{text}");
            assert_eq!(statements(dialect, &tree).len(), text.matches(';').count() + 1, "{text}");
        }
    }

    #[test]
    fn a_repair_stays_in_its_own_statement() {
        for dialect in [Dialect::Postgres, Dialect::Sqlite] {
            assert_eq!(spans(dialect, "select 1; ) select 2;"), ["select 1", ") select 2"], "{dialect:?}");
            assert_eq!(spans(dialect, "select 1; ); select 2;"), ["select 1", ")", "select 2"], "{dialect:?}");
            let text = "select (select 1 limit 1 union select 2; select 3;";
            let (tree, errors) = repaired(dialect, text);
            assert_eq!(errors.len(), 1, "{dialect:?}: {errors:?}");
            assert_eq!(spans(dialect, text).last(), Some(&"select 3"), "{dialect:?}");
            let last = tree.children(tree.root).last().map(|id| tree.node(*id).kind);
            assert_ne!(last, Some(ERROR_KIND), "{dialect:?}");
        }
    }

    #[test]
    fn deep_valid_sql_parses_in_linear_time() {
        for (dialect, depth) in [(Dialect::Sqlite, 64_000), (Dialect::Postgres, 3_000)] {
            let text = format!("select {}1{}", "(1+".repeat(depth), ")".repeat(depth));
            let started = std::time::Instant::now();
            let tree = parse(dialect, &text);
            assert!(tree.errors.is_empty(), "{dialect:?}");
            assert!(started.elapsed().as_secs() < 3, "{dialect:?}: {:?}", started.elapsed());
        }
    }

    #[test]
    fn an_error_says_what_would_have_fit() {
        let (_, errors) = repaired(Dialect::Postgres, "select a from t order");
        assert_eq!(errors[0].0, "syntax error at end of input");
        assert_eq!(errors[0].1, ["BY"]);
        let (_, errors) = repaired(Dialect::Postgres, "select a from");
        assert!(errors[0].1.contains(&"a name".to_string()), "{errors:?}");
        let (_, errors) = repaired(Dialect::Sqlite, "select a from t order");
        assert_eq!(errors[0].1, ["BY"]);
    }

    #[test]
    fn text_that_is_mostly_not_sql_stays_a_tree_and_stays_fast() {
        let junk = "select ) ( from from , where ; ".repeat(5_000);
        for dialect in [Dialect::Postgres, Dialect::Sqlite] {
            let started = std::time::Instant::now();
            assert_eq!(parse(dialect, &junk).print(), junk);
            assert!(started.elapsed().as_secs() < 5, "{dialect:?}");
        }
    }

    #[test]
    fn a_statement_with_an_error_keeps_all_its_text() {
        assert_eq!(spans(Dialect::Postgres, "select 1; select u. from users u"), ["select 1", "select u. from users u"]);
        assert_eq!(spans(Dialect::Postgres, "select 1; select ); select 2;"), ["select 1", "select )", "select 2"]);
        assert_eq!(spans(Dialect::Postgres, "select );"), ["select )"]);
    }

    #[test]
    fn an_error_in_one_statement_hides_no_refusal_in_another() {
        let text = "select 1 fetch first row with ties; select ); select 2 fetch first row with ties";
        let tree = parse(Dialect::Postgres, text);
        assert_eq!(tree.print(), text);
        let messages: Vec<&str> = tree.errors.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(messages, ["WITH TIES cannot be specified without ORDER BY clause", "syntax error at or near \")\"", "WITH TIES cannot be specified without ORDER BY clause"]);
    }
}

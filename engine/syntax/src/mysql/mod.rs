//! MySQL's syntax: the tables Bison generates from TiDB's grammar (`gen/mysql.py`), read with a port
//! of TiDB's scanner. MySQL's own grammar is GPL, so nothing here comes from it.

pub mod ast;
mod build;
#[path = "generated/tokens.rs"]
pub mod generated;
pub mod scan;

use crate::lalr::{self, Lexeme, NoActions, Tables};
use crate::tree::{SyntaxError, Tree, ERROR_KIND};
use generated::{tok, FINAL, LAST, NAMES, NTOKENS, PACT_NINF, SEGMENTS, TABLE_NINF};
pub use scan::Options;
use std::sync::OnceLock;

static TABLE_BYTES: &[u8] = include_bytes!("generated/tables.bin");

/// The grammar's tables.
pub fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| Tables::read(TABLE_BYTES, &SEGMENTS, PACT_NINF, TABLE_NINF, FINAL, LAST, NTOKENS))
}

pub fn kind_name(kind: u16) -> &'static str {
    if kind == ERROR_KIND {
        return "error";
    }
    NAMES.get(kind as usize).copied().unwrap_or("?")
}

/// The grammar's symbols for a list of finished statements.
fn statement_list() -> &'static [u16] {
    static LIST: OnceLock<[u16; 2]> = OnceLock::new();
    LIST.get_or_init(|| ["StatementList", "Start"].map(|name| NAMES.iter().position(|n| *n == name).expect("the grammar names its statement list") as u16))
}

/// The tree of `text`, read as a server with the default `sql_mode` reads it.
pub fn parse_tree(text: &str) -> Tree<'_> {
    parse_tree_with(text, Options::default())
}

/// The tree of `text`, read under `opts`. What the scanner or the grammar refuses is under error
/// nodes, and listed in the tree's `errors`, in MySQL's words.
pub fn parse_tree_with(text: &str, opts: Options) -> Tree<'_> {
    let (mut lexemes, mut tail, stop) = scan::lexemes(text, opts);
    if let Some(stop) = &stop {
        // Everything from the unclosed comment on is one token that no rule takes.
        lexemes.push(Lexeme { sym: lalr::SCAN_STOP, lead: tail, start: stop.at, end: text.len() as u32 });
        tail = text.len() as u32;
    }
    let semicolon = generated::CHAR_TOKENS[b';' as usize];
    let mut parsed = lalr::parse(tables(), text, &lexemes, tail, semicolon, statement_list(), &mut NoActions);
    for e in &mut parsed.tree.errors {
        e.message = if e.message.is_empty() { syntax_message(text, e.start) } else { format!("{} near '{}'", e.message, near(text, e.start)) };
    }
    if let Some(stop) = stop {
        parsed.tree.errors.retain(|o| o.start != stop.at);
        parsed.tree.errors.push(SyntaxError { message: stop.message, start: stop.at, end: text.len() as u32, expected: Vec::new() });
        parsed.tree.errors.sort_by_key(|e| e.start);
    }
    parsed.tree
}

/// Why a text has no AST: the grammar refuses it, it holds a construct the AST has no node for, or
/// it nests deeper than the bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

impl ParseError {
    /// Whether the grammar reads the text, and only the [`Bounds`] the parse was given refuse it.
    pub fn past_bounds(&self) -> bool {
        self.0 == build::TOO_DEEP || self.0 == build::TOO_MANY_OPERATORS
    }
}

/// How far a statement's AST may go, so that a walk of it, and its drop, stay within a stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    /// How deeply expressions nest in other than operators (a call's arguments, a parenthesized
    /// operand, a CASE), and queries and tables: a subquery spends two levels.
    pub depth: usize,
    /// How many operators the whole statement has, each a level of the tree its chain makes.
    pub operators: usize,
}

/// The statements of `text` as the AST, read under `opts`, within `bounds`. The build recurses as
/// deeply as the statement nests, so it runs on a stack of its own, as Postgres's parser does.
pub fn parse(text: &str, opts: Options, bounds: Bounds) -> Result<Vec<ast::Statement>, ParseError> {
    crate::postgres::on_deep_stack(|| {
        let tree = parse_tree_with(text, opts);
        build::B::new(&tree, opts.backslash_escapes, bounds).statements().map_err(ParseError)
    })
}

/// An expression alone as the AST, such as a table's CHECK clause, read under `opts`. Its spans
/// count from a `SELECT ` written before it.
pub fn parse_expr(text: &str, opts: Options, bounds: Bounds) -> Result<ast::Expr, ParseError> {
    use ast::{SelectItem, SetExpr, Statement};
    let mut statements = parse(&format!("SELECT {text}"), opts, bounds)?;
    if let (Some(Statement::Query(q)), true) = (statements.pop(), statements.is_empty()) {
        if let SetExpr::Select(s) = *q.body {
            if let ([SelectItem::UnnamedExpr(_)], true) = (s.projection.as_slice(), s.from.is_empty()) {
                if let Some(SelectItem::UnnamedExpr(e)) = s.projection.into_iter().next() {
                    return Ok(e);
                }
            }
        }
    }
    Err(ParseError("not one expression".into()))
}

/// The text MySQL's message quotes from where a statement goes wrong: the rest of the statement,
/// at most 80 characters.
fn near(text: &str, at: u32) -> String {
    let rest = &text[(at as usize).min(text.len())..];
    let end = rest.find(';').unwrap_or(rest.len());
    rest[..end].chars().take(80).collect()
}

/// MySQL's words for a syntax error at byte `at` of `text` (`ER_PARSE_ERROR`), with the line of
/// the statement it falls in.
pub fn syntax_message(text: &str, at: u32) -> String {
    let at = (at as usize).min(text.len());
    let start = text[..at].rfind(';').map_or(0, |i| i + 1);
    let start = start + text[start..at].len() - text[start..at].trim_start().len();
    let line = 1 + text[start..at].matches('\n').count();
    format!("You have an error in your SQL syntax; check the manual that corresponds to your MySQL server version for the right syntax to use near '{}' at line {line}", near(text, at as u32))
}

/// MySQL's keywords, in capitals.
pub fn keywords() -> impl Iterator<Item = &'static str> {
    generated::KEYWORDS.iter().map(|k| k.0)
}

/// A token's spelling for a reader of an error's expected tokens, where it has one.
pub(crate) fn spelled(kind: u16) -> Option<String> {
    let name = kind_name(kind);
    Some(match kind {
        0 => "end of input".into(),
        tok::identifier => "a name".into(),
        tok::stringLit => "a string".into(),
        tok::intLit | tok::decLit | tok::floatLit | tok::hexLit | tok::bitLit => "a number".into(),
        tok::paramMarker => "?".into(),
        tok::singleAtIdentifier | tok::doubleAtIdentifier => "a variable".into(),
        tok::invalid | tok::hintComment | tok::underscoreCS => return None,
        _ if name.len() > 2 && name.starts_with('"') && name.ends_with('"') => name[1..name.len() - 1].to_string(),
        _ if name.len() == 3 && name.starts_with('\'') => name[1..2].to_string(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn errors(text: &str) -> Vec<String> {
        parse_tree(text).errors.iter().map(|e| e.message.clone()).collect()
    }

    #[test]
    fn reads_what_mysql_reads_and_prints_it_back() {
        for text in [
            "select a, b from t where c = 1 order by a desc limit 10",
            "SELECT /*+ MAX_EXECUTION_TIME(1000) */ * FROM t AS OF TIMESTAMP '2024-01-01'",
            "insert into t (a) values (1), (2) on duplicate key update a = values(a)",
            "select `select`, \"x\" from `t` where j->>'$.a' = 'b' and x member of (j)",
            "select count(*), substr(a, 1) from t group by a with rollup",
            "select 1 /*!80000 + 2 */; select @x, @@session.sql_mode",
            "create table t (id int primary key auto_increment, name varchar(10) not null default '') engine=InnoDB",
        ] {
            let tree = parse_tree(text);
            assert_eq!(tree.print(), text);
            assert!(tree.errors.is_empty(), "{text}: {:?}", tree.errors);
        }
    }

    #[test]
    fn refuses_in_mysql_words() {
        assert_eq!(errors("select from where"), ["You have an error in your SQL syntax; check the manual that corresponds to your MySQL server version for the right syntax to use near 'from where' at line 1"]);
        assert_eq!(errors("select 1;\nselect\n  2 2"), ["You have an error in your SQL syntax; check the manual that corresponds to your MySQL server version for the right syntax to use near '2' at line 2"]);
        assert_eq!(errors("select 'abc").len(), 1);
        assert_eq!(errors("select 1 /* open").len(), 1);
    }

    fn spans(text: &str) -> Vec<&str> {
        crate::statements(crate::Dialect::Mysql, &parse_tree(text)).into_iter().map(|(a, b)| &text[a as usize..b as usize]).collect()
    }

    #[test]
    fn splits_statements_as_the_grammar_reads_them() {
        assert_eq!(spans("select ';'; select \"a;b\"; -- c;\nselect `d;`"), ["select ';'", "select \"a;b\"", "select `d;`"]);
        assert_eq!(spans("begin; select 1; commit").len(), 3);
        assert_eq!(spans("create table t (`begin` int); select 1").len(), 2);
        assert_eq!(spans("SELECT /*!800000+1 */;"), ["SELECT /*!800000+1 */"]);
        assert_eq!(spans("/*!99999 /* note */ */ SELECT 2;"), ["SELECT 2"]);
        assert_eq!(spans("SELECT 1 /*!99999 + 2 */; select 3"), ["SELECT 1", "select 3"]);
        for two in [
            "create procedure p() begin select 1; if a then select 2; end if; end; select 3",
            "create procedure p(x int) begin case x when 1 then select 1; else select 2; end case; end; select 3",
            "create procedure p() begin a:begin select 1; end a; select 2; end; select 3",
            "/*!50003 create procedure p() begin select 1; end */; select 2",
            "create procedure p() begin if 1 then select 1; end /*!80000 if */; select 2; end; select 3",
            "create procedure p() begin select 1; end /*!99999 if */; select 3",
            "create table `begin` (a int); select 1",
            "select 1 --2; select 3",
        ] {
            assert_eq!(spans(two).len(), 2, "{two}");
        }
        for unfinished in ["/*!99999 /* note */ SELECT 2;", "/*!80000 /* note */ SELECT 2;", "/*!80000 SELECT '*/';"] {
            let tree = parse_tree(unfinished);
            assert_eq!(tree.print(), unfinished);
            assert!(!tree.errors.is_empty(), "{unfinished}");
        }
    }

    #[test]
    fn reads_reylos_placeholders_and_labels() {
        for text in ["select * from t where id = :id and name = :name limit :n", "create procedure p() body:begin leave body; end"] {
            let tree = parse_tree(text);
            assert_eq!(tree.print(), text);
            assert!(tree.errors.is_empty(), "{text}: {:?}", tree.errors);
        }
    }

    #[test]
    fn an_unfinished_token_still_moves_on() {
        for text in ["select .1e", "select .1e+", "select 1e", "select 0x", "select `a", "select 'a", "select @", "select :"] {
            let tree = parse_tree(text);
            assert_eq!(tree.print(), text);
        }
    }

    #[test]
    fn reads_reylos_annotated_placeholders() {
        for text in ["select :id::nullable(int)", "insert into t (a, b) select * from json_table(:items::rows(a int not null, b? varchar(10)) ...", "select :q :: nullable(varchar(40)) as q", "select ?::int, :doc::nullable(json('{\"type\": \"object\"}'))"] {
            let tree = parse_tree(text);
            assert_eq!(tree.print(), text);
        }
        for text in ["select :id::nullable(int)", "select :q :: nullable(varchar(40)) as q", "insert into events (id) values (:id::int)", "select :person.name as name, :id::double precision as id", "select :n::int unsigned from t"] {
            assert!(parse_tree(text).errors.is_empty(), "{text}");
        }
    }

    #[test]
    fn an_unfinished_annotation_leaves_the_next_statement_to_read() {
        let text = "select :p::nullable(int; select from where;";
        let tree = parse_tree(text);
        assert_eq!(tree.print(), text);
        assert_eq!(spans(text).len(), 2);
        let second = text.find("from").unwrap() as u32;
        assert!(tree.errors.iter().any(|e| e.start < second), "{:?}", tree.errors);
        assert!(tree.errors.iter().any(|e| e.start >= second), "{:?}", tree.errors);
    }

    #[test]
    fn a_version_comment_newer_than_the_server_is_a_comment() {
        assert!(parse_tree("select 1 /*!99999 nonsense here */").errors.is_empty());
        assert!(!parse_tree("select 1 /*!80000 nonsense here */").errors.is_empty());
    }

    const BOUNDS: Bounds = Bounds { depth: 200, operators: 500 };

    #[test]
    fn only_a_statement_past_the_bounds_is_past_them() {
        let refused = |text: String| parse(&text, Options::default(), BOUNDS).unwrap_err();
        assert!(refused(format!("select {}a{} as x", "coalesce(".repeat(250), ", b)".repeat(250))).past_bounds());
        assert!(refused(format!("select {} as x", vec!["a"; 502].join(" + "))).past_bounds());
        assert!(!refused("select from where".to_string()).past_bounds());
    }

    fn printed(text: &str) -> String {
        parse(text, Options::default(), BOUNDS).unwrap_or_else(|e| panic!("{text}: {e}")).iter().map(|s| s.to_string()).collect::<Vec<_>>().join("; ")
    }

    #[test]
    fn builds_the_ast_mysql_reads() {
        for (text, read) in [
            ("select a, b + 1 as c from t join u on t.id = u.id where x > ? order by a limit 2", "SELECT a, b + 1 AS c FROM t JOIN u ON t.id = u.id WHERE x > ? ORDER BY a LIMIT 2"),
            ("insert into t (a) values (1), (?) on duplicate key update a = values(a)", "INSERT INTO t (a) VALUES (1), (?) ON DUPLICATE KEY UPDATE a = values(a)"),
            ("update t1, t2 set t1.a = t2.b where t1.id = t2.id", "UPDATE t1 CROSS JOIN t2 SET t1.a = t2.b WHERE t1.id = t2.id"),
            ("delete from t where a = ? order by b limit 1", "DELETE FROM t WHERE a = ? ORDER BY b LIMIT 1"),
            ("with c as (select 1 as a) select a from c union all select 2", "WITH c AS (SELECT 1 AS a) SELECT a FROM c UNION ALL SELECT 2"),
            ("select * from json_table(?, '$[*]' columns (id int path '$.id', n varchar(10) path '$.n' null on empty)) as j", "SELECT * FROM JSON_TABLE(?, '$[*]' COLUMNS(id INT PATH '$.id', n VARCHAR(10) PATH '$.n' NULL ON EMPTY)) AS j"),
            // MySQL's precedence: `^` binds tighter than `*`, and a join without ON is the left operand of the joins after it.
            ("select 2 * 3 ^ 1", "SELECT 2 * 3 ^ 1"),
            ("select a, count(*) from t group by rollup(a)", "SELECT a, count(*) FROM t GROUP BY a WITH ROLLUP"),
            // A column's responses, in either order.
            ("select * from json_table(?, '$[*]' columns (x int path '$.v' default '1' on error default '2' on empty)) as j", "SELECT * FROM JSON_TABLE(?, '$[*]' COLUMNS(x INT PATH '$.v' DEFAULT '2' ON EMPTY DEFAULT '1' ON ERROR)) AS j"),
            ("select * from a join b right join c on x", "SELECT * FROM a JOIN b RIGHT JOIN c ON x"),
        ] {
            assert_eq!(printed(text), read);
        }
        assert_eq!(printed("select sql_no_cache sql_cache(1)"), "SELECT SQL_NO_CACHE sql_cache(1)");
        assert_eq!(printed("select sql_cache /* c */ (1)"), "SELECT sql_cache(1)");
        let read = parse("select 2 * 3 ^ 1", Options::default(), BOUNDS);
        let Ok([ast::Statement::Query(q)]) = read.as_deref() else { panic!() };
        let ast::SetExpr::Select(s) = q.body.as_ref() else { panic!() };
        assert!(matches!(&s.projection[0], ast::SelectItem::UnnamedExpr(ast::Expr::BinaryOp { op: ast::BinaryOperator::Multiply, .. })));
    }

    #[test]
    fn a_placeholder_stands_where_it_is_written() {
        let text = "select a from t\nwhere b = ? and c in (?, :d)";
        let debug = format!("{:?}", parse(text, Options::default(), BOUNDS).unwrap());
        for (placeholder, at) in [("?", "Location(2,11)"), ("?", "Location(2,23)"), (":d", "Location(2,26)")] {
            assert!(debug.contains(&format!("Placeholder(\"{placeholder}\"), span: Span({at}")), "{placeholder} at {at}: {debug}");
        }
        // A column counts characters, past a long line's run of them.
        let text = format!("select a from t\nwhere b in ({}?)", "'é', ".repeat(100));
        let debug = format!("{:?}", parse(&text, Options::default(), BOUNDS).unwrap());
        assert!(debug.contains("Placeholder(\"?\"), span: Span(Location(2,513)"), "{debug}");
    }

    #[test]
    fn refuses_past_its_bounds() {
        let sum = |n: usize| format!("select {} as x", vec!["a"; n].join(" + "));
        assert!(parse(&sum(500), Options::default(), BOUNDS).is_ok());
        assert!(parse(&sum(502), Options::default(), BOUNDS).is_err());
        let unions = |n: usize| vec!["select 1"; n + 1].join(" union all ");
        assert!(parse(&unions(500), Options::default(), BOUNDS).is_ok());
        assert!(parse(&unions(501), Options::default(), BOUNDS).is_err());
        let calls = |n: usize| format!("select {}a{} as x", "abs(".repeat(n), ")".repeat(n));
        assert!(parse(&calls(150), Options::default(), BOUNDS).is_ok());
        assert!(parse(&calls(250), Options::default(), BOUNDS).is_err());
        assert!(parse("select from where", Options::default(), BOUNDS).is_err());
    }

    #[test]
    fn a_word_only_tidb_knows_names_a_function() {
        for text in ["select sql_cache(1)", "select sql_cache /* c */ (1)", "select sql_cache /*+ c */ (1)", "select sql_cache /*!99999 ignored */ (1)", "select sql_cache /*!80000 */ (1)", "select plan (1), background(2)", "select approx_count_distinct (id) from users", "select approx_percentile(id, 0.9) from users", "select translate (a) from t", "select sequence() from t"] {
            for opts in [Options::default(), Options { ignore_space: true, ..Options::default() }] {
                assert!(parse_tree_with(text, opts).errors.is_empty(), "{text}");
                assert!(parse(text, opts, BOUNDS).is_ok(), "{text}");
            }
        }
        // A word a rule reads before its parenthesis stays the keyword, through the rules between.
        for text in ["create table t (v vector(3))", "create table t (v vector (3))"] {
            assert!(parse_tree(text).errors.is_empty(), "{text}");
        }
    }

    #[test]
    fn json_value_takes_on_empty_before_on_error_alone() {
        assert!(parse_tree("select json_value('{}', '$.x' returning signed null on empty null on error)").errors.is_empty());
        assert!(!parse_tree("select json_value('{}', '$.x' returning signed null on error null on empty)").errors.is_empty());
        assert!(parse_tree("select * from json_table('[]', '$[*]' columns (x int path '$.v' null on error null on empty)) as j").errors.is_empty());
    }

    #[test]
    fn sql_mode_changes_the_tokens() {
        let ansi = Options { ansi_quotes: true, ..Options::default() };
        assert!(parse_tree_with("select \"a\" from \"t\"", ansi).errors.is_empty());
        assert!(parse_tree_with("select \"a\\\" from t", ansi).errors.is_empty());
        assert!(!parse_tree("select a from \"t\"").errors.is_empty());
        for script in [
            "SET SESSION sql_mode = '';\nSELECT \"a\" \"b\" \"c\";\nSET SESSION sql_mode = 'ANSI_QUOTES';\nSELECT \"i\" FROM (SELECT 1 AS i) AS \"t\";",
            "/*!40101 SET @OLD_SQL_MODE=@@SQL_MODE, SQL_MODE='NO_BACKSLASH_ESCAPES,ANSI' */;\nselect \"a\", 'b\\' from \"t\";\n/*!40101 SET SQL_MODE=@OLD_SQL_MODE */;\nselect \"x\", 'y\\'z'",
            "create procedure p() begin set sql_mode = 'ANSI_QUOTES'; begin select 1; end; select case when 1 then 2 end; end; select \"a\" \"b\" \"c\"",
            "set @@session.sql_mode = ANSI_QUOTES; select \"a\" from \"t\"; set sql_mode = default; select \"a\"",
            "SET SESSION sql_mode = '';\nSELECT 1 AS begin;\nSET SESSION sql_mode = 'ANSI_QUOTES';\nSELECT i FROM (SELECT 1 AS i) AS \"t\";",
            "SET SESSION sql_mode = '';\nSET GLOBAL sort_buffer_size = @@GLOBAL.sort_buffer_size, sql_mode = 'ANSI_QUOTES';\nSELECT \"a\" \"b\" \"c\";",
            "SET SESSION sql_mode = 'ANSI_QUOTES';\nSET @old = @@sql_mode;\nSET @old = '';\nSET sql_mode = @old;\nSELECT \"a\" \"b\" \"c\";",
            "SET SESSION sql_mode = '';\nSET GLOBAL sort_buffer_size = @@GLOBAL.sort_buffer_size, @@sql_mode = 'ANSI_QUOTES';\nSELECT i FROM (SELECT 1 AS i) AS \"t\";",
            "SET @a = '';\nSET @a = 'ANSI_QUOTES', sql_mode = @a;\nSELECT \"a\" \"b\" \"c\";",
            "SET sql_mode = 'ANSI_QUOTES';\nSET @old = @@sql_mode;\nSET @old = @old;\nSET sql_mode = '';\nSET sql_mode = @old;\nSELECT \"i\" FROM (SELECT 1 AS i) AS \"t\";",
            "SET sql_mode = 'ANSI_QUOTES';\nSET @old = @@sql_mode, @old = concat('', 'NO_BACKSLASH_ESCAPES');\nSET sql_mode = '';\nSET sql_mode = @old;\nSELECT \"a\" \"b\" \"c\";",
        ] {
            assert!(parse_tree(script).errors.is_empty(), "{script}: {:?}", parse_tree(script).errors);
        }
        assert!(!parse_tree("set global sql_mode = 'ANSI_QUOTES'; select a from \"t\"").errors.is_empty());
        let no_escapes = Options { backslash_escapes: false, ..Options::default() };
        assert!(parse_tree_with(r"select 'a\', 'b'", no_escapes).errors.is_empty());
        assert!(!parse_tree(r"select 'a\'").errors.is_empty());
    }
}

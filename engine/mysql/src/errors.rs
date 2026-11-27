//! The errors a MySQL statement may raise, where it runs only the server's own code
//! ([`reylo_engine_core::errors`]): no trigger fires on a table it writes, and it calls no stored
//! function, itself or through a view it reads. Its errors then name only the keys, foreign keys,
//! checks and columns of the tables it writes and reads, or a column it makes itself, which is all
//! the run time reads from a MySQL message (src/query-error.ts).

use reylo_engine_core::types::{Capabilities, StatementKind};
use crate::host::Host;
use reylo_engine_core::api::{Analysis, Scope};
use reylo_engine_core::dialect::Dialect;
use reylo_engine_core::errors::{called, has_word, Errors};
use reylo_engine_core::fresh::Tables;
use reylo_engine_core::host::Lazy;
use reylo_engine_core::lex::{self, MysqlText};
use serde_json::Value;

/// `{written}` and `{read}` are lists of string literals, the tables the statement writes and reads;
/// `{called}` the words it calls as functions, which a stored function of any schema may answer to,
/// since a call may name the schema, but for Reylo Migrate's shadows (`reylo_shadow_…`), each named
/// at random and dropped by the one check that made it, other than the session's own; `{cascades}` whether a row it writes may change a key
/// another table's foreign key refers to; `{own}` whether it holds a RECURSIVE WITH query, or a
/// JSON_TABLE whose columns [`request`] cannot read, and `{decimal}` whether it casts to DECIMAL (or DEC). One row `('open', why)` for each trigger and stored
/// function that may run, for each written table whose triggers the session cannot see (it holds no
/// TRIGGER privilege there, and `information_schema` shows a table's triggers only to one that
/// does), for each view whose tables it cannot see and each view of a table of another schema, and
/// one `('name', code, field, name)` for each name an error may carry.
/// What a write reaches: the written tables, a view's tables, and, where `{cascades}`, the tables a
/// foreign key's action writes in turn.
/// A value error names the column a value does not fit: a column of those tables, or one the
/// statement makes itself, which no catalog lists. MySQL types a RECURSIVE WITH query's columns by
/// its anchors alone, so a recursive member's value may not fit one, and a JSON_TABLE's columns by
/// their declarations, which a value fails under ERROR ON ERROR (MySQL refuses a DEFAULT that does
/// not fit before the statement runs). It names a cast to DECIMAL out of range by its select item,
/// an alias or the cast's own text. The statement's own JSON_TABLE columns [`request`] reads off its
/// text, as a `rows(…)` parameter's expansion writes them too; where the statement holds a
/// RECURSIVE WITH query, or a view it reads holds one or a JSON_TABLE with ERROR ON ERROR, or
/// whose definition this session may not read, no value error's column is narrowed, and where
/// either casts to DECIMAL, no out-of-range value's.
pub(crate) const ERRORS_QUERY: &str = "with recursive w(t) as ( \
      select table_name from information_schema.tables where table_schema = database() and table_name in ({written}) \
      union select u.table_name from information_schema.view_table_usage u join w on u.view_name = w.t where u.view_schema = database() and u.table_schema = database() \
      union select r.table_name from information_schema.referential_constraints r join w on r.referenced_table_name = w.t \
        where r.constraint_schema = database() and {cascades} and (r.delete_rule in ('CASCADE', 'SET NULL', 'SET DEFAULT') or r.update_rule in ('CASCADE', 'SET NULL', 'SET DEFAULT'))), \
    r(t) as (select table_name from information_schema.tables where table_schema = database() and table_name in ({read}) \
      union select u.table_name from information_schema.view_table_usage u join r on u.view_name = r.t where u.view_schema = database() and u.table_schema = database()), \
    rel(t) as (select t from w union select t from r) \
    select 'open' as kind, 'trigger' as code, cast(g.trigger_name as char) as field, cast(null as char) as name from information_schema.triggers g join w on g.event_object_table = w.t where g.event_object_schema = database() \
    union all select 'open', 'function', cast(p.routine_name as char), null from information_schema.routines p where p.routine_type = 'FUNCTION' and (p.routine_schema = database() or left(p.routine_schema, 13) <> 'reylo_shadow_') and lower(p.routine_name) in ({called}) \
    union all select 'open', 'view', cast(v.table_name as char), null from information_schema.view_routine_usage v join rel on v.table_name = rel.t where v.table_schema = database() \
    union all select 'open', 'hidden triggers', cast(w.t as char), null from w where not exists ( \
      select 1 from information_schema.user_privileges p where p.privilege_type = 'TRIGGER' and p.grantee = concat('''', substring_index(current_user(), '@', 1), '''@''', substring_index(current_user(), '@', -1), '''') \
      union all select 1 from information_schema.schema_privileges p where p.privilege_type = 'TRIGGER' and p.table_schema = database() and p.grantee = concat('''', substring_index(current_user(), '@', 1), '''@''', substring_index(current_user(), '@', -1), '''') \
      union all select 1 from information_schema.table_privileges p where p.privilege_type = 'TRIGGER' and p.table_schema = database() and p.table_name = w.t and p.grantee = concat('''', substring_index(current_user(), '@', 1), '''@''', substring_index(current_user(), '@', -1), '''')) \
    union all select 'open', 'hidden view', cast(r.t as char), null from r join information_schema.tables t on t.table_schema = database() and t.table_name = r.t \
      where t.table_type = 'VIEW' and not exists (select 1 from information_schema.view_table_usage u where u.view_schema = database() and u.view_name = r.t) \
    union all select 'open', 'view of another schema', cast(u.view_name as char), null from information_schema.view_table_usage u join rel on u.view_name = rel.t \
      where u.view_schema = database() and u.table_schema <> database() \
    union all select 'name', k.code, k.field, k.name from ( \
      select 'ER_DUP_ENTRY' as code, 'key' as field, cast(concat(s.table_name, '.', s.index_name) as char) as name from information_schema.statistics s join w on s.table_name = w.t where s.table_schema = database() and s.non_unique = 0 \
      union all select 'ER_DUP_ENTRY_WITH_KEY_NAME', 'key', cast(concat(s.table_name, '.', s.index_name) as char) from information_schema.statistics s join w on s.table_name = w.t where s.table_schema = database() and s.non_unique = 0 \
      union all select 'ER_NO_REFERENCED_ROW_2', 'constraint', cast(c.constraint_name as char) from information_schema.referential_constraints c join w on c.table_name = w.t where c.constraint_schema = database() \
      union all select 'ER_NO_REFERENCED_ROW_2', 'table', cast(c.table_name as char) from information_schema.referential_constraints c join w on c.table_name = w.t where c.constraint_schema = database() \
      union all select 'ER_ROW_IS_REFERENCED_2', 'constraint', cast(c.constraint_name as char) from information_schema.referential_constraints c join w on c.referenced_table_name = w.t where c.constraint_schema = database() and {cascades} \
      union all select 'ER_ROW_IS_REFERENCED_2', 'table', cast(c.table_name as char) from information_schema.referential_constraints c join w on c.referenced_table_name = w.t where c.constraint_schema = database() and {cascades} \
      union all select 'ER_CHECK_CONSTRAINT_VIOLATED', 'constraint', cast(c.constraint_name as char) from information_schema.table_constraints c join w on c.table_name = w.t where c.table_schema = database() and c.constraint_type = 'CHECK' \
      union all select n.code, 'column', cast(c.column_name as char) from information_schema.columns c join w on c.table_name = w.t \
        cross join (select 'ER_BAD_NULL_ERROR' as code union all select 'ER_NO_DEFAULT_FOR_FIELD') n where c.table_schema = database() and c.is_nullable = 'NO' \
      union all select n.code, 'column', cast(c.column_name as char) from information_schema.columns c join rel on c.table_name = rel.t \
        cross join (select 'ER_WARN_DATA_OUT_OF_RANGE' as code union all select 'WARN_DATA_TRUNCATED' union all select 'ER_TRUNCATED_WRONG_VALUE' \
          union all select 'ER_TRUNCATED_WRONG_VALUE_FOR_FIELD' union all select 'ER_DATA_TOO_LONG') n where c.table_schema = database() \
        and not {own} and not exists (select 1 from information_schema.views v join rel on v.table_name = rel.t where v.table_schema = database() \
          and (coalesce(v.view_definition, '') = '' or v.view_definition like '%recursive%' or v.view_definition like '%json_table%error on error%')) \
        and not (n.code = 'ER_WARN_DATA_OUT_OF_RANGE' and ({decimal} or exists ( \
          select 1 from information_schema.views v join rel on v.table_name = rel.t where v.table_schema = database() and v.view_definition like '%decimal%'))) \
    ) k";

/// A string literal every MySQL session reads as written: a quote doubled. A name with a backslash
/// is left out, since a session reads one by its mode, and so is one with a `?`, since a host writes
/// a value at each `?` of a catalog query ([`unwritable`]).
fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Whether a name can't be written into a catalog query as a literal ([`literal`]).
pub(crate) fn unwritable(name: &str) -> bool {
    name.contains('\\') || name.contains('?')
}

pub(crate) fn list<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let literals: Vec<String> = names.map(literal).collect();
    if literals.is_empty() { "null".to_string() } else { literals.join(", ") }
}

/// The value errors whose column [`ERRORS_QUERY`] lists from the catalog.
const VALUE_ERRORS: [&str; 5] = ["ER_WARN_DATA_OUT_OF_RANGE", "WARN_DATA_TRUNCATED", "ER_TRUNCATED_WRONG_VALUE", "ER_TRUNCATED_WRONG_VALUE_FOR_FIELD", "ER_DATA_TOO_LONG"];

/// [`ERRORS_QUERY`] for a statement of `kind` that reads and writes `tables`, with the columns of
/// its own a value error may name beside the catalog's ([`crate::mysql_collation::erroring_columns`]),
/// or `None` where it is neither a query nor a write (a CALL), or writes what the parse cannot name
/// (a table of another schema), or, `by_schema`, reads a table named by its schema, which the
/// catalog's walk does not reach: a view there may call a stored function, or name a column of
/// its own.
fn request(kind: Option<StatementKind>, tables: &Tables, sql: &str, by_schema: bool) -> Option<(String, Vec<String>)> {
    let unqualified = |t: &String| t.rsplit('.').next().unwrap_or(t).to_string();
    let written: Vec<String> = tables.write.iter().map(unqualified).collect();
    let read: Vec<String> = tables.read.iter().map(unqualified).collect();
    if by_schema || (kind != Some(StatementKind::Select) && written.is_empty()) || written.iter().chain(&read).any(|t| unwritable(t)) {
        return None;
    }
    let called = called(sql, Dialect::Mysql);
    // An INSERT changes no key but through ON DUPLICATE KEY UPDATE or as a REPLACE, which deletes.
    let cascades = !written.is_empty() && (kind != Some(StatementKind::Insert) || has_word(sql, Dialect::Mysql, &["update", "replace"]));
    let columns = if has_word(sql, Dialect::Mysql, &["json_table"]) {
        lex::lex(sql, Dialect::Mysql).ok().and_then(|lexed| crate::mysql_collation::erroring_columns(sql, &lexed.opaque))
    } else {
        Some(Vec::new())
    };
    let own = has_word(sql, Dialect::Mysql, &["recursive"]) || columns.is_none();
    let query = ERRORS_QUERY
        .replace("{written}", &list(written.iter().map(String::as_str)))
        .replace("{read}", &list(read.iter().map(String::as_str)))
        .replace("{called}", &list(called.iter().map(String::as_str)))
        .replace("{cascades}", if cascades { "true" } else { "false" })
        .replace("{own}", if own { "true" } else { "false" })
        .replace("{decimal}", if has_word(sql, Dialect::Mysql, &["decimal", "dec"]) { "true" } else { "false" });
    Some((query, columns.unwrap_or_default()))
}

/// Asks [`ERRORS_QUERY`] in the statement's first round, beside its other needs, since it waits on
/// nothing but the parse: asked only once the analysis is done, it would take a round of its own.
pub fn prefetch(kind: Option<StatementKind>, tables: &Tables, sql: &str, by_schema: bool, host: &Host) {
    if let Some((query, _)) = request(kind, tables, sql, by_schema) {
        let _ = host.query(&query, Vec::new(), Scope::Statement);
    }
}

/// The names each error the statement may raise can carry: `Some(None)` where it may run a trigger
/// or a stored function, or where [`request`] has no query to ask.
pub fn errors(analysis: &Analysis, host: &Host) -> Lazy<Option<Errors>> {
    let Some(tables) = &analysis.tables else { return Some(None) };
    if !scanned_as_read(&analysis.rewritten.sql, analysis.sql_mode) {
        return Some(None);
    }
    // The analysis says it reads a table named by its schema as capabilities whose tables are unknown.
    let by_schema = analysis.capabilities.as_ref().is_some_and(Capabilities::tables_unknown);
    let Some((query, columns)) = request(analysis.kind, tables, &analysis.rewritten.sql, by_schema) else { return Some(None) };
    let rows = host.query(&query, Vec::new(), Scope::Statement)?;
    let Ok(rows) = rows else { return Some(None) };
    Some(narrowed(&rows).map(|mut errors| {
        // A value error the catalog's columns narrow may name the statement's own too; one it
        // left out stays unnarrowed.
        let codes: Vec<&str> = VALUE_ERRORS.into_iter().filter(|code| errors.0.contains_key(*code)).collect();
        for code in codes {
            for column in &columns {
                errors.add(code, "column", column);
            }
        }
        errors
    }))
}

/// Whether the word scans, which read strings as the default `sql_mode` does, read `sql` as its
/// session does, which reads it by `text` where the modes read it otherwise: where the session ends
/// a string elsewhere (at a backslash, under NO_BACKSLASH_ESCAPES), a word the scans take for text
/// may be a call, a cast or a RECURSIVE, and the reverse.
pub(crate) fn scanned_as_read(sql: &str, text: Option<MysqlText>) -> bool {
    let spans = |text| lex::lex_mysql(sql, lex::Named::Dialect, text).map(|l| l.opaque).ok();
    match text {
        Some(text) => spans(text) == spans(MysqlText::default()),
        None => true,
    }
}

/// The built-ins that change something: a lock, the session's last insert id.
const CHANGES_SOMETHING: [&str; 4] = ["get_lock", "release_lock", "release_all_locks", "last_insert_id"];

/// One row per view a read of `{read}` reaches, the tables of a view of another schema among what it
/// reaches: its definition as the server normalizes it, NULL where this session may not read it or
/// it is of another schema, which [`ERRORS_QUERY`] does not look into.
pub(crate) const VIEWS_QUERY: &str = "with recursive r(s, t) as ( \
      select table_schema, table_name from information_schema.tables where table_schema = database() and table_name in ({read}) \
      union select u.table_schema, u.table_name from information_schema.view_table_usage u join r on u.view_schema = r.s and u.view_name = r.t) \
    select case when v.table_schema = database() then cast(v.view_definition as char) end from r \
    join information_schema.views v on v.table_schema = r.s and v.table_name = r.t";

/// Whether the engine finds nothing in the statement that could write: a SELECT that runs no stored function, as
/// [`errors`] says, with no locking clause, no `INTO` and no variable assigned, that calls none of
/// the built-ins that change something, itself or through a view, and reads nothing of another
/// schema by name.
pub fn read_only(analysis: &Analysis, host: &Host) -> Lazy<bool> {
    let sql = &analysis.rewritten.sql;
    let Some(tables) = &analysis.tables else { return Some(false) };
    let changes = |sql: &str| called(sql, Dialect::Mysql).iter().any(|f| CHANGES_SOMETHING.contains(&f.as_str()));
    if analysis.kind != Some(StatementKind::Select) || analysis.errors.is_none() || has_word(sql, Dialect::Mysql, &["for", "lock", "into"]) || sql.contains(":=") || changes(sql) {
        return Some(false);
    }
    if tables.read.iter().any(|t| t.contains('.') || unwritable(t)) {
        return Some(false);
    }
    if tables.read.is_empty() {
        return Some(true);
    }
    let Ok(views) = host.query(&VIEWS_QUERY.replace("{read}", &list(tables.read.iter().map(String::as_str))), Vec::new(), Scope::Statement)? else { return Some(false) };
    Some(views.iter().all(|row| row.first().and_then(Value::as_str).is_some_and(|definition| !definition.is_empty() && !changes(definition))))
}

/// What [`ERRORS_QUERY`]'s rows say: nothing where one opens the statement.
fn narrowed(rows: &[Vec<Value>]) -> Option<Errors> {
    let text = |row: &[Value], k: usize| row.get(k).and_then(Value::as_str).map(str::to_string);
    let mut errors = Errors::default();
    for row in rows {
        match text(row, 0).as_deref() {
            Some("name") => {
                if let (Some(code), Some(field), Some(name)) = (text(row, 1), text(row, 2), text(row, 3)) {
                    errors.add(&code, &field, &name);
                }
            }
            _ => return None,
        }
    }
    Some(errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(sql: &str) -> (bool, Vec<String>) {
        let tables = Tables { write: vec!["events".to_string()], ..Default::default() };
        let (query, columns) = request(Some(StatementKind::Insert), &tables, sql, false).unwrap();
        (query.contains("and not true and not exists"), columns)
    }

    #[test]
    fn a_json_tables_columns_with_error_on_error_are_listed_and_a_recursive_with_query_leaves_the_column_open() {
        // A `rows(…)` parameter's expansion, in insert position and as a derived table.
        let insert = "insert into events (`id`, `ts`) SELECT `events`.`id`, `events`.`ts` FROM JSON_TABLE(?, '$[*]' COLUMNS (`reylo.n` FOR ORDINALITY, `id` bigint PATH '$.\"id\"' ERROR ON ERROR, `ts` datetime(6) PATH '$.\"ts\"' ERROR ON ERROR)) AS `events` ORDER BY `events`.`reylo.n`";
        assert_eq!(asked(insert), (false, vec!["id".to_string(), "ts".to_string()]));
        let derived = "insert into events (id, note) select r.id, r.day from (SELECT `reylo.j`.`id` AS `id`, `reylo.j`.`day` AS `day` FROM JSON_TABLE(?, '$[*]' COLUMNS (`id` bigint PATH '$.\"id\"' ERROR ON ERROR, `day` date PATH '$.\"day\"' ERROR ON ERROR)) AS `reylo.j`) AS r";
        assert_eq!(asked(derived), (false, vec!["id".to_string(), "day".to_string()]));
        // Only a column that errors names itself; NESTED ones too, by the name as written.
        let nested = "insert into events (id) select j.`My ``id``` from json_table(?, '$[*]' columns (a int path '$.a' null on error, b int path '$.b', nested path '$.c[*]' columns (`My ``id``` int path '$' null on empty error on error), d int path '$.d' error on empty)) j";
        assert_eq!(asked(nested), (false, vec!["My `id`".to_string()]));
        assert_eq!(asked("insert into events (id) select x from json_table(?, '$[*]' columns (x int path '$' null on error)) j"), (false, vec![]));
        // A name the scan cannot read, and a recursive WITH query, leave every value error's column open.
        assert!(asked("insert into events (id) select j.a$b from json_table(?, '$[*]' columns (a$b int path '$' error on error)) j").0);
        assert!(asked("insert into events (id) with recursive r (x) as (select 1 union all select x + 1 from r where x < 3) select x from r").0);
        assert_eq!(asked("insert into events (id) values (?)"), (false, vec![]));
    }

    #[test]
    fn a_statement_that_reads_a_table_named_by_its_schema_is_not_narrowed() {
        let tables = Tables { read: vec!["users".to_string()], ..Default::default() };
        assert!(request(Some(StatementKind::Select), &tables, "select * from users, app.taxed", true).is_none());
        assert!(request(Some(StatementKind::Select), &tables, "select * from users", false).is_some());
    }

    #[test]
    fn the_scans_hold_only_where_the_session_ends_each_string_where_the_default_mode_does() {
        let no_escapes = MysqlText { backslash_escapes: false, ansi_quotes: false };
        let ansi = MysqlText { backslash_escapes: true, ansi_quotes: true };
        let sql = "insert into posts (id, title) select id + length('\\') - 1, cast(age * 1000 as decimal(4,0)) as score from users -- the user's score";
        assert!(!scanned_as_read(sql, Some(no_escapes)));
        assert!(scanned_as_read(sql, None));
        assert!(scanned_as_read("select \"email\" from users", Some(ansi)));
    }
}

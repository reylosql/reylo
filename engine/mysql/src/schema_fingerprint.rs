//! The schema's fingerprint: one query whose answer changes when anything an analysis reads
//! changes, so that an analysis kept by it (`reylo/.cache/`, which the type check, the build steps
//! and a Rust build share) is current while the answer is. Every host runs this one text
//! (`reylo-engine`'s `mysql::schema`), with each value as mysql2 reads it, and the test below holds
//! it to the engine's catalog queries.
//!
//! It holds the server's settings that change how a statement reads; the columns with their full
//! types and defaults, indexes, views and what each reads, routines with their arguments,
//! triggers, foreign keys and constraints, each CHECK with its expression and whether it is
//! enforced, which a JSON column is typed by; and the session's account's privileges, which the
//! server checks a statement against and the errors a write may raise read
//! (`errors::ERRORS_QUERY`). A statement may read a table, a view or a function of any
//! database on the server (`other.t`, `other.f()`), so each database's are in it but the server's
//! own and Reylo Migrate's shadows (`reylo_shadow_…`, made and dropped by each check, which no
//! statement names), unless the shadow is the session's own, as a dry run's is for its type check:
//! the session's database's with no database's name, so that a database built the same way under
//! another name, beside the same others, has the same fingerprint, and any other's with its own.
//! `information_schema` lists only what the session's account may reach, so an account granted
//! only the databases its statements read sees no other.

use std::sync::LazyLock;

/// The query. Its one row is the server's settings that change how a statement reads, and how many
/// lines there are with two sums of parts of their SHA-256s.
pub static QUERY: LazyLock<String> = LazyLock::new(query);

/// Whether `schema` is one a statement may read: the session's own, or any but the server's and
/// Reylo Migrate's other shadows.
fn own(schema: &str) -> String {
    format!("({schema} = database() or {schema} not in ('mysql', 'sys', 'information_schema', 'performance_schema') and left({schema}, 13) <> 'reylo_shadow_')")
}

/// `schema` as a line names it: none for the session's own.
fn within(schema: &str) -> String {
    format!("if({schema} = database(), '', {schema})")
}

/// The session's account, as `information_schema`'s privilege tables write a grantee.
const GRANTEE: &str = "concat('''', substring_index(current_user(), '@', 1), '''@''', substring_index(current_user(), '@', -1), '''')";

/// One line: its tag, then each field, a NUL apart, which no name holds.
fn line(tag: &str, fields: &[&str]) -> String {
    format!("concat_ws(char(0), '{tag}', {})", fields.join(", "))
}

fn query() -> String {
    let rows = [
        // Columns, with their full types (a length, a precision and scale, fractional seconds, a
        // character set) and defaults, which a structured parameter's optional keys read.
        format!(
            "select {} as x from information_schema.columns where {}",
            line("c", &[&within("table_schema"), "table_name", "column_name", "ordinal_position", "column_type", "coalesce(character_set_name, '')", "coalesce(collation_name, '')", "is_nullable", "column_key", "extra", "if(column_default is null, '', concat('=', column_default))", "coalesce(generation_expression, '')", "sha2(column_comment, 256)"]),
            own("table_schema")
        ),
        format!("select {} from information_schema.statistics where {}", line("s", &[&within("table_schema"), "table_name", "index_name", "non_unique", "seq_in_index", "coalesce(column_name, '')", "coalesce(expression, '')"]), own("table_schema")),
        // Views, and what each reads, which an account without SHOW VIEW sees no definition of.
        format!("select {} from information_schema.views where {}", line("v", &[&within("table_schema"), "table_name", "sha2(coalesce(view_definition, ''), 256)"]), own("table_schema")),
        format!("select {} from information_schema.view_table_usage where {}", line("w", &[&within("view_schema"), "view_name", &within("table_schema"), "table_name"]), own("view_schema")),
        format!("select {} from information_schema.view_routine_usage where {}", line("o", &[&within("table_schema"), "table_name", &within("specific_schema"), "specific_name"]), own("table_schema")),
        // Stored routines with their arguments.
        format!("select {} from information_schema.routines where {}", line("r", &[&within("routine_schema"), "routine_name", "routine_type", "coalesce(dtd_identifier, '')", "sha2(coalesce(routine_definition, ''), 256)"]), own("routine_schema")),
        format!(
            "select {} from information_schema.parameters where {} and ordinal_position > 0",
            line("a", &[&within("specific_schema"), "specific_name", "routine_type", "ordinal_position", "coalesce(parameter_mode, '')", "coalesce(parameter_name, '')", "dtd_identifier", "coalesce(character_set_name, '')"]),
            own("specific_schema")
        ),
        // What the errors a statement may raise are read from: triggers, foreign keys, and
        // constraints with each CHECK's expression and whether it is enforced, which a JSON column
        // is typed by too.
        format!("select {} from information_schema.triggers where {}", line("g", &[&within("event_object_schema"), "event_object_table", "trigger_name", "event_manipulation", "action_timing"]), own("event_object_schema")),
        format!("select {} from information_schema.referential_constraints where {}", line("f", &[&within("constraint_schema"), "table_name", "constraint_name", "referenced_table_name", "update_rule", "delete_rule"]), own("constraint_schema")),
        format!("select {} from information_schema.table_constraints where {}", line("k", &[&within("table_schema"), "table_name", "constraint_name", "constraint_type", "enforced"]), own("table_schema")),
        format!("select {} from information_schema.check_constraints where {}", line("h", &[&within("constraint_schema"), "constraint_name", "sha2(check_clause, 256)"]), own("constraint_schema")),
        // The session's account's privileges, which the server checks a statement against when it
        // prepares it, and without TRIGGER on a written table hides the table's triggers.
        format!("select {} from information_schema.user_privileges where grantee = {GRANTEE}", line("u", &["privilege_type"])),
        format!("select {} from information_schema.schema_privileges where grantee = {GRANTEE}", line("u", &[&within("table_schema"), "privilege_type"])),
        format!("select {} from information_schema.table_privileges where grantee = {GRANTEE}", line("u", &[&within("table_schema"), "table_name", "privilege_type"])),
        format!("select {} from information_schema.column_privileges where grantee = {GRANTEE}", line("u", &[&within("table_schema"), "table_name", "column_name", "privilege_type"])),
    ];
    // The server's version; the session's `sql_mode`, which changes how a statement reads; the
    // database's character set and collation, which a structured parameter is expanded by; whether
    // a table's name is matched by its case; and the digits a division adds to its result's scale.
    format!(
        "select concat_ws(' ', @@version, @@sql_mode, @@character_set_database, @@collation_database, @@lower_case_table_names, @@div_precision_increment, count(*), sum(cast(conv(substr(h, 1, 15), 16, 10) as unsigned)), bit_xor(cast(conv(substr(h, 16, 16), 16, 10) as unsigned))) from (select sha2(x, 256) as h from (\n    {}\n  ) r) t",
        rows.join("\n    union all ")
    )
}

#[cfg(test)]
mod tests {
    /// How the fingerprint and the engine leave out Reylo Migrate's shadows other than the session's own.
    const SHADOWS: &str = "<> 'reylo_shadow_'";

    /// The text of every query the engine's sources define as a `…_QUERY` constant, by its name,
    /// and core's queries of a table's columns and the server, which the engine asks too.
    fn queries() -> Vec<(String, String)> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&root).unwrap().flatten() {
            if entry.file_name() == "schema_fingerprint.rs" {
                continue;
            }
            let source = std::fs::read_to_string(entry.path()).unwrap();
            for (i, _) in source.match_indices("const ") {
                let rest = &source[i + "const ".len()..];
                let name: String = rest.chars().take_while(|c| c.is_ascii_uppercase() || *c == '_').collect();
                let Some(text) = rest[name.len()..].strip_prefix(": &str = \"") else { continue };
                if !name.ends_with("_QUERY") {
                    continue;
                }
                let mut end = 0;
                let bytes = text.as_bytes();
                while end < bytes.len() && bytes[end] != b'"' {
                    end += if bytes[end] == b'\\' { 2 } else { 1 };
                }
                out.push((format!("{}:{name}", entry.file_name().to_string_lossy()), text[..end].to_string()));
            }
        }
        use reylo_engine_core::structured::{MARIADB_TABLE_QUERY, MYSQL_SERVER_QUERY, MYSQL_TABLE_QUERY};
        out.extend([("MYSQL_TABLE_QUERY", MYSQL_TABLE_QUERY), ("MARIADB_TABLE_QUERY", MARIADB_TABLE_QUERY), ("MYSQL_SERVER_QUERY", MYSQL_SERVER_QUERY)].map(|(n, q)| (n.to_string(), q.to_string())));
        out
    }

    /// The columns of the `information_schema` tables the engine reads.
    const COLUMNS: &str = "table_catalog table_schema table_name column_name ordinal_position column_default is_nullable data_type character_maximum_length character_octet_length numeric_precision numeric_scale datetime_precision character_set_name collation_name column_type column_key extra privileges column_comment generation_expression srs_id \
        table_type engine row_format table_rows auto_increment table_collation create_options table_comment \
        non_unique index_schema index_name seq_in_index sub_part index_type index_comment is_visible expression \
        view_definition check_option is_updatable definer security_type \
        specific_name routine_schema routine_name routine_type dtd_identifier routine_body routine_definition is_deterministic sql_data_access routine_comment specific_schema parameter_mode parameter_name \
        trigger_schema trigger_name event_manipulation event_object_schema event_object_table action_order action_condition action_statement action_orientation action_timing \
        constraint_schema constraint_name unique_constraint_schema unique_constraint_name match_option update_rule delete_rule referenced_table_name constraint_type enforced check_clause \
        grantee privilege_type is_grantable view_schema view_name is_default pad_attribute";

    /// The columns the engine reads that the fingerprint need not, each with why.
    const UNFINGERPRINTED: &[(&str, &str)] = &[
        ("data_type", "a column's type without its modifiers, which column_type writes"),
        ("table_type", "whether a table is a view, which the views rows say"),
        ("auto_increment", "a word of extra, which the columns rows hold"),
    ];

    /// Every column of `information_schema` a query of the engine's names, the fingerprint names
    /// too, or [`UNFINGERPRINTED`] says why it need not: a column the engine starts reading would
    /// otherwise leave an entry current after a change to it.
    #[test]
    fn the_schema_fingerprint_reads_every_column_the_engine_reads() {
        let words = |text: &str| -> std::collections::BTreeSet<String> { text.to_lowercase().split(|c: char| !c.is_ascii_alphanumeric() && c != '_').filter(|w| !w.is_empty()).map(String::from).collect() };
        let fingerprint = words(&super::QUERY);
        let columns: Vec<&str> = COLUMNS.split_whitespace().collect();
        let mut missed = Vec::new();
        let queries = queries();
        assert!(queries.len() > 10, "the engine's queries are found");
        for (name, sql) in &queries {
            for word in words(sql) {
                if columns.contains(&word.as_str()) && !fingerprint.contains(&word) && !UNFINGERPRINTED.iter().any(|(c, _)| *c == word) {
                    missed.push(format!("{name} reads {word}"));
                }
            }
        }
        assert!(missed.is_empty(), "the schema's fingerprint reads none of these, and UNFINGERPRINTED says nothing of them: {missed:?}");
        for (column, _) in UNFINGERPRINTED {
            assert!(queries.iter().any(|(_, sql)| words(sql).contains(*column)) && !fingerprint.contains(*column), "{column} is no longer read by the engine alone: take it off UNFINGERPRINTED");
        }
    }

    /// Every catalog a query of the engine's reads (`information_schema.…`), the fingerprint reads
    /// too, or one whose every change moves a row the fingerprint does read, named here with why.
    #[test]
    fn the_schema_fingerprint_reads_everything_the_engine_reads() {
        const COVERED: &[(&str, &str)] = &[
            ("tables", "a table's name and kind, which the columns and views rows carry"),
            ("collations", "the server's own, which its version carries"),
        ];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let fingerprint = super::QUERY.to_lowercase();
        let mut missed = std::collections::BTreeSet::new();
        // The engine's sources, and core's queries of a table's columns for a structured parameter.
        let sources = std::fs::read_dir(&root).unwrap().flatten().filter(|entry| entry.file_name() != "schema_fingerprint.rs").map(|entry| std::fs::read_to_string(entry.path()).unwrap());
        let core = [reylo_engine_core::structured::MYSQL_TABLE_QUERY, reylo_engine_core::structured::MARIADB_TABLE_QUERY].map(String::from);
        for source in sources.chain(core) {
            let source = source.to_lowercase();
            for (i, _) in source.match_indices("information_schema.") {
                let catalog: String = source[i + "information_schema.".len()..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                if !catalog.is_empty() && !fingerprint.contains(&format!("information_schema.{catalog}")) && !COVERED.iter().any(|(c, _)| *c == catalog) {
                    missed.insert(catalog);
                }
            }
        }
        assert!(missed.is_empty(), "the engine reads these, which the schema's fingerprint does not: {missed:?}");
    }

    /// Every system variable a query of the engine's reads, the fingerprint reads too.
    #[test]
    fn the_schema_fingerprint_reads_every_variable_the_engine_reads() {
        for sql in [crate::mysql::SQL_MODE_QUERY, reylo_engine_core::structured::MYSQL_SERVER_QUERY] {
            for (i, _) in sql.match_indices("@@") {
                let name: String = sql[i + 2..].trim_start_matches("session.").chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                assert!(super::QUERY.contains(&format!("@@{name}")), "the engine reads @@{name}, which the schema's fingerprint does not");
            }
        }
    }

    /// The engine's lookups across databases leave out the shadows the fingerprint leaves out, so a
    /// migration check's shadow can't change an analysis without moving the fingerprint.
    #[test]
    fn the_engine_and_the_fingerprint_leave_out_the_same_shadows() {
        for (name, sql) in [("fingerprint", super::QUERY.as_str()), ("VIEWS_QUERY", crate::mysql::VIEWS_QUERY), ("ERRORS_QUERY", crate::errors::ERRORS_QUERY)] {
            assert!(sql.contains(SHADOWS), "{name} reads Reylo Migrate's shadows");
        }
    }
}

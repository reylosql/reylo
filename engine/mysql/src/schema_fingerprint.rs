//! The schema's fingerprint: one query whose answer changes when anything an analysis reads
//! changes, so that an analysis kept by it (`reylo/.cache/`, which the type check, the build steps
//! and a Rust build share) is current while the answer is. Every host runs this one text
//! (`reylo-engine`'s `mysql::schema`), with each value as mysql2 reads it, and the test below holds
//! it to the engine's catalog queries.
//!
//! It holds the server's version and the session's `sql_mode`, which change how a statement reads,
//! and the database's character set and collation, which a structured parameter is expanded by;
//! the columns with their full types (a length, a precision and scale, fractional seconds, a
//! character set), indexes, views, routines with their arguments, triggers, foreign keys and
//! constraints, each CHECK with its expression and whether it is enforced, which a JSON column is
//! typed by; and the session's account's TRIGGER grants, read as the errors a write may raise read
//! them (`errors::ERRORS_QUERY`). A statement may read a table, a view or a function of any
//! database on the server (`other.t`, `other.f()`), so each database's are in it but the server's
//! own and Reylo Migrate's shadows (`reylo_shadow_…`, made and dropped by each check, which no
//! statement names), unless the shadow is the session's own, as a dry run's is for its type check:
//! the session's database's with no database's name, so that a database built the same way under
//! another name, beside the same others, has the same fingerprint, and any other's with its own.
//! `information_schema` lists only what the session's account may reach, so an account granted
//! only the databases its statements read sees no other.

/// The query. Its one row is the server's version, the session's `sql_mode`, the database's
/// character set and collation, and how many lines there are with two sums of their CRC-32s.
pub const QUERY: &str = "select concat_ws(' ', @@version, @@sql_mode, @@character_set_database, @@collation_database, count(*), sum(crc32(x)), bit_xor(crc32(x))) from (
    select concat_ws(' ', 'c', if(table_schema = database(), '', table_schema), table_name, column_name, ordinal_position, column_type, coalesce(character_set_name, ''), is_nullable, column_key, extra, coalesce(generation_expression, ''), sha2(column_comment, 256)) as x from information_schema.columns where (table_schema = database() or table_schema not in ('mysql', 'sys', 'information_schema', 'performance_schema') and left(table_schema, 13) <> 'reylo_shadow_')
    union all select concat_ws(' ', 's', if(table_schema = database(), '', table_schema), table_name, index_name, non_unique, seq_in_index, column_name) from information_schema.statistics where (table_schema = database() or table_schema not in ('mysql', 'sys', 'information_schema', 'performance_schema') and left(table_schema, 13) <> 'reylo_shadow_')
    union all select concat_ws(' ', 'v', if(table_schema = database(), '', table_schema), table_name, sha2(view_definition, 256)) from information_schema.views where (table_schema = database() or table_schema not in ('mysql', 'sys', 'information_schema', 'performance_schema') and left(table_schema, 13) <> 'reylo_shadow_')
    union all select concat_ws(' ', 'r', if(routine_schema = database(), '', routine_schema), routine_name, routine_type, coalesce(dtd_identifier, ''), sha2(coalesce(routine_definition, ''), 256)) from information_schema.routines where (routine_schema = database() or routine_schema not in ('mysql', 'sys', 'information_schema', 'performance_schema') and left(routine_schema, 13) <> 'reylo_shadow_')
    union all select concat_ws(' ', 'a', if(specific_schema = database(), '', specific_schema), specific_name, routine_type, ordinal_position, coalesce(parameter_mode, ''), coalesce(parameter_name, ''), dtd_identifier, coalesce(character_set_name, '')) from information_schema.parameters where (specific_schema = database() or specific_schema not in ('mysql', 'sys', 'information_schema', 'performance_schema') and left(specific_schema, 13) <> 'reylo_shadow_') and ordinal_position > 0
    union all select concat_ws(' ', 'g', if(event_object_schema = database(), '', event_object_schema), event_object_table, trigger_name, event_manipulation, action_timing) from information_schema.triggers where (event_object_schema = database() or event_object_schema not in ('mysql', 'sys', 'information_schema', 'performance_schema') and left(event_object_schema, 13) <> 'reylo_shadow_')
    union all select concat_ws(' ', 'f', if(constraint_schema = database(), '', constraint_schema), table_name, constraint_name, referenced_table_name, update_rule, delete_rule) from information_schema.referential_constraints where (constraint_schema = database() or constraint_schema not in ('mysql', 'sys', 'information_schema', 'performance_schema') and left(constraint_schema, 13) <> 'reylo_shadow_')
    union all select concat_ws(' ', 'k', if(table_schema = database(), '', table_schema), table_name, constraint_name, constraint_type, enforced) from information_schema.table_constraints where (table_schema = database() or table_schema not in ('mysql', 'sys', 'information_schema', 'performance_schema') and left(table_schema, 13) <> 'reylo_shadow_')
    union all select concat_ws(' ', 'h', if(constraint_schema = database(), '', constraint_schema), constraint_name, sha2(check_clause, 256)) from information_schema.check_constraints where (constraint_schema = database() or constraint_schema not in ('mysql', 'sys', 'information_schema', 'performance_schema') and left(constraint_schema, 13) <> 'reylo_shadow_')
    union all select concat_ws(' ', 'u', privilege_type) from information_schema.user_privileges where grantee = concat('''', substring_index(current_user(), '@', 1), '''@''', substring_index(current_user(), '@', -1), '''') and privilege_type = 'TRIGGER'
    union all select concat_ws(' ', 'u', '', privilege_type) from information_schema.schema_privileges where grantee = concat('''', substring_index(current_user(), '@', 1), '''@''', substring_index(current_user(), '@', -1), '''') and privilege_type = 'TRIGGER' and table_schema = database()
    union all select concat_ws(' ', 'u', '', table_name, privilege_type) from information_schema.table_privileges where grantee = concat('''', substring_index(current_user(), '@', 1), '''@''', substring_index(current_user(), '@', -1), '''') and privilege_type = 'TRIGGER' and table_schema = database()
  ) t";

#[cfg(test)]
mod tests {
    /// How the fingerprint and the engine leave out Reylo Migrate's shadows other than the session's own.
    const SHADOWS: &str = "<> 'reylo_shadow_'";

    /// Every catalog a query of the engine's reads (`information_schema.…`), the fingerprint reads
    /// too, or one whose every change moves a row the fingerprint does read, named here with why.
    #[test]
    fn the_schema_fingerprint_reads_everything_the_engine_reads() {
        const COVERED: &[(&str, &str)] = &[
            ("tables", "a table's name and kind, which the columns and views rows carry"),
            ("view_table_usage", "what a view's definition names, which the views row hashes"),
            ("view_routine_usage", "what a view's definition names, which the views row hashes"),
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
        for (name, sql) in [("fingerprint", super::QUERY), ("VIEWS_QUERY", crate::mysql::VIEWS_QUERY), ("ERRORS_QUERY", crate::errors::ERRORS_QUERY)] {
            assert!(sql.contains(SHADOWS), "{name} reads Reylo Migrate's shadows");
        }
    }
}

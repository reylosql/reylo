//! The schema's fingerprint: what an analysis reads of a database file, so that an analysis kept by
//! it (`reylo/.cache/`, which the type check, the build steps and a Rust build share) is current
//! while it is. A file's whole schema is its `sqlite_schema` table, which every pragma the engine
//! reads it through (`pragma_table_xinfo`, `pragma_foreign_key_list`) reads in turn; the functions
//! of the SQLite that reads the file (`sqlite::FUNCTIONS_QUERY`) are the host's library's, not the
//! file's, and name the analyses beside it instead (the program's fingerprint, engine/native). The
//! test below holds it to the engine's catalog queries.

/// Every object of the file's schema, with the statement that made it.
pub const QUERY: &str = "select type, name, tbl_name, coalesce(sql, '') from sqlite_schema order by 1, 2, 3, 4";

/// The count SQLite moves with every change to a file's schema and never moves back: read with
/// [`QUERY`], in one transaction, and again once an analysis ends, it tells a schema changed and
/// changed back while a statement was analyzed from one that never changed.
pub const STAMP_QUERY: &str = "select schema_version from pragma_schema_version";

#[cfg(test)]
mod tests {
    /// Every query the engine asks its host, by its name.
    fn queries() -> [(&'static str, &'static str); 5] {
        [
            ("DDL_QUERY", crate::sqlite::DDL_QUERY),
            ("FUNCTIONS_QUERY", crate::sqlite::FUNCTIONS_QUERY),
            ("SCHEMA_QUERY", crate::capabilities::SCHEMA_QUERY),
            ("ERRORS_QUERY", crate::errors::ERRORS_QUERY),
            ("SQLITE_TABLE_QUERY", reylo_engine_core::structured::SQLITE_TABLE_QUERY),
        ]
    }

    /// The catalogs that are the file's schema: `sqlite_schema`, which [`super::QUERY`] reads whole,
    /// and the pragmas that read it in turn.
    const OF_THE_SCHEMA: &[&str] = &["sqlite_schema", "pragma_table_xinfo", "pragma_table_info", "pragma_foreign_key_list", "pragma_index_list", "pragma_index_info", "pragma_index_xinfo", "pragma_table_list"];
    /// What the engine reads besides the file's schema, each with why the fingerprint need not.
    const ELSEWHERE: &[(&str, &str)] = &[("pragma_function_list", "the functions of the host's SQLite, which name its analyses beside the schema (engine/native's sqlite::cache_fingerprint)")];

    /// Each catalog a query of the engine's names (`sqlite_…` and `pragma_…`) is the file's schema,
    /// or one of [`ELSEWHERE`].
    #[test]
    fn the_schema_fingerprint_reads_everything_the_engine_reads() {
        let mut unread = Vec::new();
        for (name, sql) in queries() {
            for (i, _) in sql.match_indices("pragma_").chain(sql.match_indices("sqlite_")) {
                let catalog: String = sql[i..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                if !OF_THE_SCHEMA.contains(&catalog.as_str()) && !ELSEWHERE.iter().any(|(n, _)| *n == catalog) {
                    unread.push(format!("{name} reads {catalog}"));
                }
            }
        }
        unread.dedup();
        assert!(unread.is_empty(), "neither the schema's fingerprint nor the program's covers these: {unread:?}");
        assert!(super::QUERY.contains("from sqlite_schema"));
    }

    /// Every query the engine's sources define is one [`queries`] holds, so that a new one is held to the fingerprint.
    #[test]
    fn every_query_the_engine_defines_is_checked() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let checked = queries().map(|(name, _)| name);
        for entry in std::fs::read_dir(&root).unwrap().flatten() {
            if entry.file_name() == "schema_fingerprint.rs" {
                continue;
            }
            for line in std::fs::read_to_string(entry.path()).unwrap().lines() {
                let Some(rest) = line.split("const ").nth(1) else { continue };
                let name: String = rest.chars().take_while(|c| c.is_ascii_uppercase() || *c == '_').collect();
                if name.ends_with("_QUERY") && rest[name.len()..].trim_start().starts_with(':') {
                    assert!(checked.contains(&name.as_str()), "add {name} to queries()");
                }
            }
        }
    }
}

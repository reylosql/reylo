//! The schema's fingerprint: one query whose answer changes when anything an analysis reads
//! changes, so that an analysis kept by it (`reylo/.cache/`) is current while the answer is.
//!
//! It holds every column, table (a view's query with it), function and dictionary a user made,
//! the server's configuration's included (an executable function, a dictionary read from a file),
//! each hashed field by field, since a name may hold any character, a dot or a space included, and
//! the server's version and the settings the session changed with them, since one of those can
//! change a statement's types (`join_use_nulls`). A statement may read a table of any database, so each
//! database's are in it but the server's own: the connection's with no database's name, so that a
//! database built the same way under another name has the same fingerprint, and any other's with
//! its own.

/// The query: one row, the server's version and how many lines there are with two sums of their
/// hashes, then how many executable functions the server's configuration defines, then the
/// server's UUID, which is no part of the fingerprint: it names the replica that answered. With
/// `signatures`, it reads each one's arguments and result too, from `system.user_defined_functions`:
/// a table an account may read only by its own grant, so it is asked for only where the first
/// answer counts such a function.
pub fn query(signatures: bool) -> String {
    let mut rows = ROWS.to_vec();
    if signatures {
        rows.push("SELECT cityHash64('x', name, toString(type), return_type, toString(argument_types), toString(argument_names)) FROM system.user_defined_functions");
    }
    format!("SELECT concat(version(), ' ', toString(count()), ' ', toString(sum(x)), ' ', toString(groupBitXor(x))), (SELECT count() FROM system.functions WHERE origin = 'ExecutableUserDefined'), toString(serverUUID()) FROM (\n    {}\n  )", rows.join("\n    UNION ALL "))
}

/// Whether a server of `version` (`26.8.1.1`) lists an executable function's signature, which it
/// does from 26.2.
pub fn lists_signatures(version: &str) -> bool {
    at_least(version, (26, 2))
}

/// Whether `version` is `oldest` or later.
fn at_least(version: &str, oldest: (u32, u32)) -> bool {
    let mut parts = version.split('.').map(|part| part.parse::<u32>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0)) >= oldest
}

/// The lines every server has.
const ROWS: &[&str] = &[
    "SELECT cityHash64('c', database = currentDatabase(), if(database = currentDatabase(), '', database), table, name, position, type, default_kind, default_expression, comment) AS x FROM system.columns WHERE database NOT IN ('system', 'INFORMATION_SCHEMA', 'information_schema')",
    "SELECT cityHash64('t', database = currentDatabase(), if(database = currentDatabase(), '', database), name, engine, sorting_key, as_select) FROM system.tables WHERE database NOT IN ('system', 'INFORMATION_SCHEMA', 'information_schema')",
    "SELECT cityHash64('f', name, toString(origin), create_query) FROM system.functions WHERE origin <> 'System'",
    "SELECT cityHash64('y', database = currentDatabase(), if(database = currentDatabase(), '', database), name, toString(`key.names`), toString(`key.types`), toString(`attribute.names`), toString(`attribute.types`)) FROM system.dictionaries",
    "SELECT cityHash64('s', name, value) FROM system.settings WHERE changed",
];

#[cfg(test)]
mod tests {
    /// An executable function's signature is read where the server lists it.
    #[test]
    fn an_executable_functions_signature_is_read_where_the_server_lists_it() {
        assert!(!super::lists_signatures("25.11.2.24"));
        assert!(super::lists_signatures("26.2.1.1") && super::lists_signatures("26.8.48.11"));
        assert!(!super::query(false).contains("system.user_defined_functions"));
        assert!(super::query(true).contains("system.user_defined_functions"));
    }

    /// Every catalog a query of the engine's reads (`system.…`), the fingerprint reads too, or one
    /// named here with why it need not.
    #[test]
    fn the_schema_fingerprint_reads_everything_the_engine_reads() {
        const COVERED: &[(&str, &str)] = &[("one", "a constant table of one row")];
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut missed = std::collections::BTreeSet::new();
        for entry in std::fs::read_dir(&root).unwrap().flatten() {
            if entry.file_name() == "schema_fingerprint.rs" {
                continue;
            }
            let source = std::fs::read_to_string(entry.path()).unwrap();
            for (i, _) in source.match_indices("system.") {
                let catalog: String = source[i + "system.".len()..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                if !catalog.is_empty() && !super::query(true).contains(&format!("system.{catalog}")) && !COVERED.iter().any(|(c, _)| *c == catalog) {
                    missed.insert(catalog);
                }
            }
        }
        assert!(missed.is_empty(), "the engine reads these, which the schema's fingerprint does not: {missed:?}");
    }
}

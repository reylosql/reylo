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

/// The query. Its one row is the server's version, and how many lines there are with two sums of
/// their hashes.
pub const QUERY: &str = "SELECT concat(version(), ' ', toString(count()), ' ', toString(sum(x)), ' ', toString(groupBitXor(x))) FROM (
    SELECT cityHash64('c', database = currentDatabase(), if(database = currentDatabase(), '', database), table, name, position, type, default_kind, default_expression, comment) AS x FROM system.columns WHERE database NOT IN ('system', 'INFORMATION_SCHEMA', 'information_schema')
    UNION ALL SELECT cityHash64('t', database = currentDatabase(), if(database = currentDatabase(), '', database), name, engine, sorting_key, as_select) FROM system.tables WHERE database NOT IN ('system', 'INFORMATION_SCHEMA', 'information_schema')
    UNION ALL SELECT cityHash64('f', name, toString(origin), create_query) FROM system.functions WHERE origin <> 'System'
    UNION ALL SELECT cityHash64('y', database = currentDatabase(), if(database = currentDatabase(), '', database), name, toString(`key.names`), toString(`key.types`), toString(`attribute.names`), toString(`attribute.types`)) FROM system.dictionaries
    UNION ALL SELECT cityHash64('s', name, value) FROM system.settings WHERE changed
  )";

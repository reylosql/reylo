//! The schema's fingerprint: one query whose answer changes when anything an analysis reads
//! changes, so that an analysis kept by it (`reylo/.cache/`, which the type check, the build steps
//! and a Rust build share) is current while the answer is. Every host runs this one text
//! (`reylo-engine`'s `mysql::schema`), with each value as mysql2 reads it.
//!
//! It holds the server's version and the session's `sql_mode`, which change how a statement reads;
//! the database's columns with their full types (a length, a precision and scale, fractional
//! seconds, a character set), indexes, views, routines, triggers, foreign keys and constraints, and
//! each CHECK with its expression and whether it is enforced, which a JSON column is typed by; and
//! every other database's stored functions, which a statement may call by its database's name.

/// The query. Its one row is the server's version, the session's `sql_mode`, and how many lines
/// there are with two sums of their CRC-32s.
pub const QUERY: &str = "select concat_ws(' ', @@version, @@sql_mode, count(*), sum(crc32(x)), bit_xor(crc32(x))) from (
    select concat_ws(' ', 'c', table_name, column_name, ordinal_position, column_type, coalesce(character_set_name, ''), is_nullable, column_key, extra, coalesce(generation_expression, ''), sha2(column_comment, 256)) as x from information_schema.columns where table_schema = database()
    union all select concat_ws(' ', 's', table_name, index_name, non_unique, seq_in_index, column_name) from information_schema.statistics where table_schema = database()
    union all select concat_ws(' ', 'v', table_name, sha2(view_definition, 256)) from information_schema.views where table_schema = database()
    union all select concat_ws(' ', 'r', routine_name, routine_type, coalesce(dtd_identifier, ''), sha2(coalesce(routine_definition, ''), 256)) from information_schema.routines where routine_schema = database()
    union all select concat_ws(' ', 'p', routine_schema, routine_name) from information_schema.routines where routine_schema <> database() and routine_type = 'FUNCTION'
    union all select concat_ws(' ', 'g', event_object_table, trigger_name, event_manipulation, action_timing) from information_schema.triggers where event_object_schema = database()
    union all select concat_ws(' ', 'f', table_name, constraint_name, referenced_table_name, update_rule, delete_rule) from information_schema.referential_constraints where constraint_schema = database()
    union all select concat_ws(' ', 'k', table_name, constraint_name, constraint_type, enforced) from information_schema.table_constraints where table_schema = database()
    union all select concat_ws(' ', 'h', constraint_name, sha2(check_clause, 256)) from information_schema.check_constraints where constraint_schema = database()
  ) t";

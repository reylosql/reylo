//! The objects of `pg_catalog` the engine's catalog queries read, which the session's role must be
//! allowed to use. A host that is refused a catalog query for a privilege (42501) asks the server
//! whether the role may use each of them (`reylo-engine`'s Postgres client): where it may, the
//! refusal was of a schema the role may not use, which the engine reads as an answer; where it may
//! not, the analysis cannot be made. The test in `catalog_queries.rs` holds the queries to them.

/// The functions the queries call, each in a form that is an exact match of a built-in taking no
/// polymorphic argument. A query that needs another adds it here once it is checked to be so:
/// `unnest`, `array_agg`, `cardinality`, `array_length`, `to_jsonb` and the like take any type, and
/// an overload of narrower types created in `pg_catalog` would be the closer match.
pub const FUNCTIONS: [&str; 36] = [
    "bit_or",
    "bool_or",
    "count",
    "current_setting",
    "float4out",
    "format_type",
    "generate_series",
    "has_schema_privilege",
    "int2out",
    "int4out",
    "max",
    "md5",
    "oidout",
    "pg_get_constraintdef",
    "pg_function_is_visible",
    "pg_get_expr",
    "pg_get_function_sqlbody",
    "pg_get_indexdef",
    "pg_get_ruledef",
    "pg_get_serial_sequence",
    "pg_get_viewdef",
    "pg_operator_is_visible",
    "quote_literal",
    "regclassout",
    "regcollationout",
    "regnamespaceout",
    "regoperatorout",
    "regprocedureout",
    "regtypeout",
    "regexp_matches",
    "replace",
    "string_agg",
    "textcat",
    "textin",
    "to_number",
    "to_regclass",
];

/// The tables the queries read.
pub const TABLES: [&str; 26] = [
    "pg_aggregate",
    "pg_am",
    "pg_amop",
    "pg_attrdef",
    "pg_attribute",
    "pg_cast",
    "pg_class",
    "pg_collation",
    "pg_constraint",
    "pg_depend",
    "pg_description",
    "pg_enum",
    "pg_extension",
    "pg_index",
    "pg_inherits",
    "pg_language",
    "pg_namespace",
    "pg_opclass",
    "pg_operator",
    "pg_opfamily",
    "pg_policy",
    "pg_proc",
    "pg_range",
    "pg_rewrite",
    "pg_trigger",
    "pg_type",
];

//! The schema's fingerprint: one query whose answer changes when anything an analysis reads
//! changes, so that an analysis kept by it (`reylo/.cache/`, which the type check, the build steps
//! and a Rust build share) is current while the answer is. Every host runs this one text
//! (`reylo-engine`'s `postgres::schema`), and the test in `catalog_queries.rs` holds it to the
//! engine's catalog queries: each catalog, column, setting and catalog function they read is read
//! here too, or named there with the reason it need not be.
//!
//! It names every operator, function, type and table as `pg_catalog`'s, and converts a value to
//! text only by a built-in cast (a name, a `"char"`, a boolean) or by its type's output function
//! (`textin(oidout(…))`), as the engine's catalog queries do: under a search path that puts a
//! user's schema before `pg_catalog`, a user's `||` that answers NULL would leave every row NULL,
//! and the fingerprint would never move. Each row is the text the plain `||` of each field makes.
//! The server's own `pg_get_viewdef` and `pg_get_ruledef` read a rule through a query of bare
//! operators, which a user's `=` of oids answers too: a view's or a rule's row is then NULL, and
//! left out.
//!
//! An object is in it by its name, which a database built by the same migrations shares, where its
//! oid differs: each `reg` type's output writes the name, schema-qualified where the search path
//! does not reach it. Objects of the server's own (`pg_catalog`'s and `information_schema`'s below
//! the first oid a user's object takes) are left out; a user's or an extension's object is in it
//! wherever it was made, `pg_catalog` included, as every catalog query reads it there too.

use std::sync::LazyLock;

/// The query. Its one row is the MD5 of every line, in order.
pub static QUERY: LazyLock<String> = LazyLock::new(query);

/// The schemas among `$1` the session's role may not use, as the fingerprint's `j` rows count them:
/// a host asks it to tell a catalog query refused for a name in such a schema from one refused for
/// the catalog itself.
pub const UNUSABLE_SCHEMAS_QUERY: &str = "select n.nspname::pg_catalog.text from pg_catalog.pg_namespace n where n.nspname operator(pg_catalog.=) any ($1::pg_catalog.name[]) and not pg_catalog.has_schema_privilege(n.oid, 'USAGE'::pg_catalog.text)";

/// A temporary schema holds one session's objects, which another session's statement names only by
/// that session's number, and a TOAST schema none that a statement names.
const NOT_TEMP: &str = "n.nspname operator(pg_catalog.!~) '^pg_(toast|temp)'::pg_catalog.text";
/// The first oid a user's object takes.
const USER: &str = "'16384'::pg_catalog.oid";
const PG_CLASS: &str = "'pg_catalog.pg_class'::pg_catalog.regclass::pg_catalog.oid";
const PG_PROC: &str = "'pg_catalog.pg_proc'::pg_catalog.regclass::pg_catalog.oid";
const PG_EXTENSION: &str = "'pg_catalog.pg_extension'::pg_catalog.regclass::pg_catalog.oid";

/// Whether the object `oid`, in the schema `n`, is a user's or an extension's: outside the server's
/// own schemas, or made in one of them after the server's own objects were.
fn user(oid: &str) -> String {
    format!("(not (n.nspname operator(pg_catalog.=) any ('{{pg_catalog,information_schema}}'::pg_catalog.name[])) or {oid} operator(pg_catalog.>=) {USER}) and {NOT_TEMP}")
}

fn cat(parts: &[String]) -> String {
    parts.join(" operator(pg_catalog.||) ")
}

fn lit(s: &str) -> String {
    format!("'{s}'::pg_catalog.text")
}

/// One line: its tag, then each field, a space apart.
fn line(tag: &str, fields: &[String]) -> String {
    let mut parts = vec![lit(&format!("{tag} "))];
    for (i, field) in fields.iter().enumerate() {
        if i > 0 {
            parts.push(lit(" "));
        }
        parts.push(field.clone());
    }
    cat(&parts)
}

fn out(ty: &str, value: &str) -> String {
    format!("pg_catalog.textin(pg_catalog.{ty}out({value}))")
}

/// An object by its name, through its `reg` type's output.
fn named(ty: &str, oid: &str) -> String {
    out(ty, &format!("{oid}::pg_catalog.{ty}"))
}

fn relation(oid: &str) -> String {
    named("regclass", oid)
}

fn ty(oid: &str) -> String {
    named("regtype", oid)
}

fn txt(value: &str) -> String {
    format!("{value}::pg_catalog.text")
}

fn md5(value: &str) -> String {
    format!("pg_catalog.md5({value})")
}

fn or(value: &str) -> String {
    format!("coalesce({value}, ''::pg_catalog.text)")
}

fn eq(a: &str, b: &str) -> String {
    format!("{a} operator(pg_catalog.=) {b}")
}

fn setting(name: &str) -> String {
    format!("pg_catalog.current_setting('{name}'::pg_catalog.text)")
}

/// The relation `alias.column` names, and its schema as `n`.
fn rel(alias: &str, column: &str) -> String {
    format!("join pg_catalog.pg_class c on {} join pg_catalog.pg_namespace n on {}", eq("c.oid", &format!("{alias}.{column}")), eq("n.oid", "c.relnamespace"))
}

fn query() -> String {
    let rows = [
        // What the session itself adds to an analysis: the server's version, which the catalog
        // queries are chosen by; the search path that resolves each name; whether a backslash in a
        // string is a character or an escape, and whether `\'` may be written, which decide how the
        // server reads a statement, and the first of which the CHECKs are read under alone; and the
        // session's DateStyle, whose style the engine's host reports an error for, naming the
        // setting (engine/native).
        format!(
            "select {} as x",
            line("v", &[setting("server_version_num"), setting("search_path"), setting("standard_conforming_strings"), setting("backslash_quote"), setting("DateStyle")])
        ),
        // Relations, their columns with their type modifiers (a `varchar(n)`'s size, a
        // `numeric(p,s)`'s precision) and whether each is an identity or a generated one,
        // inheritance, constraints, indexes, row triggers and rules: nullability and the row count
        // read them.
        format!("select {} from pg_catalog.pg_class c join pg_catalog.pg_namespace n on {} where {}", line("r", &[cat(&[txt("n.nspname"), lit("."), txt("c.relname")]), txt("c.relkind")]), eq("n.oid", "c.relnamespace"), user("c.oid")),
        format!(
            "select {} from pg_catalog.pg_attribute a {} where a.attnum operator(pg_catalog.>) 0 and not a.attisdropped and {}",
            line("a", &[relation("a.attrelid"), out("int2", "a.attnum"), txt("a.attname"), ty("a.atttypid"), out("int4", "a.atttypmod"), txt("a.attnotnull"), named("regcollation", "a.attcollation"), txt("a.attidentity"), txt("a.attgenerated")]),
            rel("a", "attrelid"),
            user("c.oid")
        ),
        format!("select {} from pg_catalog.pg_inherits h {} where {}", line("h", &[relation("h.inhrelid"), relation("h.inhparent"), out("int4", "h.inhseqno")]), rel("h", "inhrelid"), user("c.oid")),
        format!(
            "select {} from pg_catalog.pg_constraint o join pg_catalog.pg_namespace n on {} where {}",
            line("c", &[relation("o.conrelid"), ty("o.contypid"), txt("o.conname"), "pg_catalog.pg_get_constraintdef(o.oid)".into()]),
            eq("n.oid", "o.connamespace"),
            user("o.oid")
        ),
        format!("select {} from pg_catalog.pg_index i {} where {}", line("i", &[relation("i.indexrelid"), txt("i.indisvalid"), "pg_catalog.pg_get_indexdef(i.indexrelid)".into()]), rel("i", "indrelid"), user("c.oid")),
        format!(
            "select {} from pg_catalog.pg_trigger g {} where not g.tgisinternal and {}",
            line("g", &[relation("g.tgrelid"), txt("g.tgname"), out("int2", "g.tgtype"), txt("g.tgenabled"), named("regprocedure", "g.tgfoid")]),
            rel("g", "tgrelid"),
            user("c.oid")
        ),
        format!(
            "select {} from pg_catalog.pg_rewrite w {} where w.rulename operator(pg_catalog.<>) '_RETURN'::pg_catalog.name and {}",
            line("u", &[relation("w.ev_class"), txt("w.rulename"), txt("w.ev_type"), txt("w.ev_enabled"), md5("pg_catalog.pg_get_ruledef(w.oid)")]),
            rel("w", "ev_class"),
            user("c.oid")
        ),
        // A user's operators and casts, which the trust check reads.
        format!("select {} from pg_catalog.pg_operator o where o.oid operator(pg_catalog.>=) {USER}", line("o", &[named("regoperator", "o.oid"), named("regprocedure", "o.oprcode::pg_catalog.oid")])),
        // Enum labels, and a user's collations with whether each is deterministic: a key's equality
        // and a CHECK's narrowing read it.
        format!("select {} from pg_catalog.pg_enum e", line("e", &[ty("e.enumtypid"), out("float4", "e.enumsortorder"), txt("e.enumlabel")])),
        format!(
            "select {} from pg_catalog.pg_collation l where l.oid operator(pg_catalog.>=) {USER}",
            line("l", &[named("regcollation", "l.oid"), txt("l.collprovider"), txt("l.collisdeterministic"), or(&txt("l.collcollate"))])
        ),
        // Types: a domain with its base type's modifier, collation, NOT NULL and default; a base
        // type's length, its category and whether it is preferred in it (which operator a call
        // resolves to), and the delimiter its array's text is read by; and a range's subtype and
        // multirange.
        format!(
            "select {} from pg_catalog.pg_type t join pg_catalog.pg_namespace n on {} where {}",
            line(
                "t",
                &[
                    ty("t.oid"),
                    txt("t.typname"),
                    txt("t.typtype"),
                    ty("t.typbasetype"),
                    out("int4", "t.typtypmod"),
                    named("regcollation", "t.typcollation"),
                    txt("t.typnotnull"),
                    md5(&or("t.typdefault")),
                    out("int2", "t.typlen"),
                    txt("t.typcategory"),
                    txt("t.typispreferred"),
                    txt("t.typdelim"),
                ]
            ),
            eq("n.oid", "t.typnamespace"),
            user("t.oid")
        ),
        format!("select {} from pg_catalog.pg_range g where g.rngtypid operator(pg_catalog.>=) {USER}", line("b", &[ty("g.rngtypid"), ty("g.rngsubtype"), ty("g.rngmultitypid")])),
        // Functions with their arguments' names, their result columns, their defaults, volatility,
        // strictness, kind and variadic argument; their language, which decides whether a body is
        // read; and whether one runs as its owner or has a planner support function, either of which
        // keeps its body unread.
        format!(
            "select {} from pg_catalog.pg_proc p join pg_catalog.pg_namespace n on {} join pg_catalog.pg_language l on {} where {}",
            line(
                "f",
                &[
                    named("regprocedure", "p.oid"),
                    ty("p.prorettype"),
                    txt("p.proretset"),
                    md5(&or("p.prosrc")),
                    md5(&or("pg_catalog.pg_get_function_sqlbody(p.oid)")),
                    or(&txt("p.proconfig")),
                    md5(&or("pg_catalog.pg_get_expr(p.proargdefaults, 0::pg_catalog.oid)")),
                    txt("p.prokind"),
                    txt("p.provolatile"),
                    txt("p.proisstrict"),
                    out("int2", "p.pronargdefaults"),
                    ty("p.provariadic"),
                    or(&txt("p.proargnames")),
                    or(&txt("p.proargmodes")),
                    or(&txt("p.proallargtypes::pg_catalog.regtype[]")),
                    txt("l.lanname"),
                    txt("p.prosecdef"),
                    named("regprocedure", "p.prosupport::pg_catalog.oid"),
                ]
            ),
            eq("n.oid", "p.pronamespace"),
            eq("l.oid", "p.prolang"),
            user("p.oid")
        ),
        // The functions each aggregate runs, with their volatility and language: whether a call may
        // write.
        format!(
            "select {} from pg_catalog.pg_aggregate s join pg_catalog.pg_proc p on {} join pg_catalog.pg_namespace n on {} join pg_catalog.pg_proc f on f.oid operator(pg_catalog.=) any (array[{}]) join pg_catalog.pg_language l on {} where {}",
            line("s", &[named("regprocedure", "s.aggfnoid::pg_catalog.oid"), named("regprocedure", "f.oid"), txt("f.provolatile"), txt("l.lanname")]),
            eq("p.oid", "s.aggfnoid::pg_catalog.oid"),
            eq("n.oid", "p.pronamespace"),
            ["aggtransfn", "aggfinalfn", "aggcombinefn", "aggmtransfn", "aggminvtransfn", "aggmfinalfn"].map(|f| format!("s.{f}::pg_catalog.oid")).join(", "),
            eq("l.oid", "f.prolang"),
            user("p.oid")
        ),
        format!(
            "select {} from pg_catalog.pg_cast k where k.oid operator(pg_catalog.>=) {USER}",
            line("k", &[ty("k.castsource"), ty("k.casttarget"), named("regprocedure", "k.castfunc"), txt("k.castcontext"), txt("k.castmethod")])
        ),
        // View definitions.
        format!(
            "select {} from pg_catalog.pg_class c join pg_catalog.pg_namespace n on {} where c.relkind operator(pg_catalog.=) any ('{{v,m}}'::pg_catalog.\"char\"[]) and {}",
            line("w", &[relation("c.oid"), md5("pg_catalog.pg_get_viewdef(c.oid)")]),
            eq("n.oid", "c.relnamespace"),
            user("c.oid")
        ),
        // What the errors a statement may raise are read from: defaults, policies and operator
        // classes, with the family each is of and whether it is its type's default.
        format!("select {} from pg_catalog.pg_attrdef d {} where {}", line("x", &[relation("d.adrelid"), out("int2", "d.adnum"), md5("pg_catalog.pg_get_expr(d.adbin, d.adrelid)")]), rel("d", "adrelid"), user("c.oid")),
        format!(
            "select {} from pg_catalog.pg_policy p {} where {}",
            line("p", &[relation("p.polrelid"), txt("p.polname"), txt("p.polcmd"), txt("p.polpermissive"), md5(&cat(&[or("pg_catalog.pg_get_expr(p.polqual, p.polrelid)"), lit(" "), or("pg_catalog.pg_get_expr(p.polwithcheck, p.polrelid)")]))]),
            rel("p", "polrelid"),
            user("c.oid")
        ),
        format!(
            "select {} from pg_catalog.pg_opclass q join pg_catalog.pg_am m on {} join pg_catalog.pg_opfamily f on {} where q.oid operator(pg_catalog.>=) {USER}",
            line("q", &[named("regnamespace", "q.opcnamespace"), txt("q.opcname"), txt("m.amname"), ty("q.opcintype"), txt("q.opcdefault"), named("regnamespace", "f.opfnamespace"), txt("f.opfname")]),
            eq("m.oid", "q.opcmethod"),
            eq("f.oid", "q.opcfamily")
        ),
        // The operators of a user's operator family, or added to a built-in one: a unique key pins
        // one row only where its family's equality is its type's.
        format!(
            "select {} from pg_catalog.pg_amop m join pg_catalog.pg_opfamily f on {} join pg_catalog.pg_am a on {} where m.oid operator(pg_catalog.>=) {USER}",
            line("m", &[named("regnamespace", "f.opfnamespace"), txt("f.opfname"), txt("a.amname"), out("int2", "m.amopstrategy"), ty("m.amoplefttype"), ty("m.amoprighttype"), named("regoperator", "m.amopopr::pg_catalog.oid"), txt("m.amoppurpose")]),
            eq("f.oid", "m.amopfamily"),
            eq("a.oid", "f.opfmethod")
        ),
        // Column comments: a field's doc comment.
        format!(
            "select {} from pg_catalog.pg_description d {} where {} and d.objsubid operator(pg_catalog.>) 0 and {}",
            line("d", &[relation("d.objoid"), out("int4", "d.objsubid"), md5("d.description")]),
            rel("d", "objoid"),
            eq("d.classoid", PG_CLASS),
            user("c.oid")
        ),
        // Extensions, the server's own plpgsql among them, and the relations and functions each
        // owns: the tables an extension's code may read, and whether a CHECK's function is the JSON
        // Schema validator it is typed by.
        format!("select {} from pg_catalog.pg_extension e", line("n", &[txt("e.extname"), txt("e.extversion"), named("regnamespace", "e.extnamespace")])),
        format!(
            "select {} from pg_catalog.pg_depend d join pg_catalog.pg_extension e on {} where {} and {} and d.classid operator(pg_catalog.=) any (array[{PG_CLASS}, {PG_PROC}])",
            line("y", &[txt("e.extname"), format!("case when {} then {} else {} end", eq("d.classid", PG_CLASS), relation("d.objid"), named("regprocedure", "d.objid"))]),
            eq("e.oid", "d.refobjid"),
            eq("d.refclassid", PG_EXTENSION),
            eq("d.deptype", "'e'::pg_catalog.\"char\"")
        ),
        // The sequences a column owns, an identity's or one `owned by` it, which an insert may advance.
        format!(
            "select {} from pg_catalog.pg_depend d join pg_catalog.pg_class s on {} where {} and {} and d.deptype operator(pg_catalog.=) any ('{{a,i}}'::pg_catalog.\"char\"[]) and {}",
            line("z", &[relation("d.objid"), relation("d.refobjid"), out("int4", "d.refobjsubid"), txt("d.deptype")]),
            eq("s.oid", "d.objid"),
            eq("d.classid", PG_CLASS),
            eq("d.refclassid", PG_CLASS),
            eq("s.relkind", "'S'::pg_catalog.\"char\"")
        ),
        // The schemas the session's role may not use: a name in one resolves to nothing, and a
        // catalog query that looks it up fails.
        format!(
            "select {} from pg_catalog.pg_namespace n where not pg_catalog.has_schema_privilege(n.oid, 'USAGE'::pg_catalog.text) and {}",
            line("j", &[txt("n.nspname")]),
            user("n.oid")
        ),
    ];
    format!(
        "select pg_catalog.md5(coalesce(pg_catalog.string_agg(t.x, E'\\n'::pg_catalog.text order by t.x), ''::pg_catalog.text)) from (\n    {}\n  ) t",
        rows.join("\n    union all ")
    )
}

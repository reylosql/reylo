//! The locks a Postgres migration holds on tables that already have rows. A statement that checks
//! every row, builds an index or copies a table while it holds a lock that blocks writes stops the
//! app's writes to that table for as long as it runs, which on a large table is an outage. Most such
//! changes have a shape that doesn't: a constraint added `not valid` and validated in a later
//! transaction (validation blocks no writes), a `not null` proven by a validated check first, an
//! index built `concurrently` in a file of its own. An index the plan replaces is dropped only once
//! its replacement is built, in one transaction with what takes the new one in its place, so that
//! the table is never without it. `lock_safe` writes a plan in those shapes, as several files;
//! `run_watched` runs a file on a shadow and says which of its statements still hold such a lock,
//! reading the parse tree for the scans and the tables' storage for the rewrites, so that whether a
//! statement copies a table is Postgres's own answer, not a list of rules.

use crate::commands::{run, Migration};
use crate::db::Db;
use crate::diff::postgres::quote_ident;
use crate::diff::Plan;
use crate::{Dialect, Error};
use syntax::postgres::protobuf::{node::Node as N, AlterTableType as At, ConstrType, ObjectType, RangeVar};
use std::collections::{BTreeMap, BTreeSet};

/// What a plan finds in the database it changes, for `lock_safe`.
#[derive(Clone, Debug, Default)]
pub struct Existing {
    /// Each plain table and materialized view (not a partitioned table, where neither `not valid`
    /// foreign keys nor concurrent index builds are taken on every supported version), under the
    /// name a statement on the search path writes it with and under its qualified name, to its
    /// qualified name.
    pub tables: BTreeMap<String, String>,
    /// Each index of those, under both names.
    pub indexes: BTreeMap<String, Index>,
    /// The name of every relation, as the server stores it: a temporary name is one none holds.
    pub names: BTreeSet<String>,
}

/// An index of a table that has rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Index {
    /// Its table's qualified name.
    pub table: String,
    /// Its name, as the server stores it.
    pub name: String,
    /// The kind of constraint it is the index of: `p`, `u` or `x`, or a space for none.
    pub key: char,
}

/// What waits for a table's last build, each a statement of the plan.
#[derive(Default)]
struct Waits {
    /// The foreign keys on a key it drops.
    keys: Vec<usize>,
    /// The drops of its indexes and keys.
    drops: Vec<usize>,
    /// What names an index whose finish waits.
    named: Vec<usize>,
    /// The foreign keys added back.
    back: Vec<usize>,
}

impl Existing {
    /// The tables named, each under that name alone, with no index.
    pub fn of(tables: &[&str]) -> Existing {
        Existing { tables: tables.iter().map(|t| (t.to_string(), t.to_string())).collect(), names: tables.iter().map(|t| crate::sql::last_written(t)).collect(), ..Existing::default() }
    }

    /// With an index named `index` (as a statement names it) on `table`, of a constraint of the
    /// kind `key` (`p`, `u` or `x`) or a space for none.
    pub fn with_index(mut self, index: &str, table: &str, key: char) -> Existing {
        let name = crate::sql::last_written(index);
        self.indexes.insert(index.to_string(), Index { table: table.to_string(), name: name.clone(), key });
        self.names.insert(name);
        self
    }
}

/// The tables and materialized views of `db`, their indexes, and every relation's name.
pub async fn existing(db: &mut Db) -> Result<Existing, Error> {
    const OWN: &str = r"n.nspname not in ('pg_catalog', 'information_schema') and n.nspname not like 'pg\_%'";
    let tables = db
        .rows(&format!(
            "select c.oid::regclass::text as shown, format('%I.%I', n.nspname, c.relname) as qualified
            from pg_class c join pg_namespace n on n.oid = c.relnamespace
            where c.relkind in ('r', 'm') and {OWN}"
        ))
        .await?;
    let indexes = db
        .rows(&format!(
            "select i.indexrelid::regclass::text as shown, format('%I.%I', n.nspname, c.relname) as qualified, c.relname as stored, format('%I.%I', tn.nspname, t.relname) as table_name,
              coalesce((select co.contype::text from pg_constraint co where co.conindid = i.indexrelid and co.conrelid = i.indrelid and co.contype in ('p', 'u', 'x')), ' ') as key
            from pg_index i join pg_class c on c.oid = i.indexrelid join pg_namespace n on n.oid = c.relnamespace
              join pg_class t on t.oid = i.indrelid join pg_namespace tn on tn.oid = t.relnamespace
            where t.relkind in ('r', 'm') and {OWN}"
        ))
        .await?;
    let names = db.rows(&format!("select c.relname as stored from pg_class c join pg_namespace n on n.oid = c.relnamespace where {OWN}")).await?;
    let mut out = Existing { names: names.iter().map(|r| r.text("stored").to_string()).collect(), ..Existing::default() };
    for r in &tables {
        for name in [r.text("shown"), r.text("qualified")] {
            out.tables.insert(name.to_string(), r.text("qualified").to_string());
        }
    }
    for r in &indexes {
        for name in [r.text("shown"), r.text("qualified")] {
            out.indexes.insert(name.to_string(), Index { table: r.text("table_name").to_string(), name: r.text("stored").to_string(), key: r.text("key").chars().next().unwrap_or(' ') });
        }
    }
    Ok(out)
}

/// A relation as a statement names it: `users`, `billing.invoices`, each part quoted as needed.
fn named(r: &RangeVar) -> String {
    if r.schemaname.is_empty() {
        quote_ident(&r.relname)
    } else {
        format!("{}.{}", quote_ident(&r.schemaname), quote_ident(&r.relname))
    }
}

/// A name a list of parts makes (`drop index billing.invoices_due`), each quoted as needed.
fn named_parts(parts: &[syntax::postgres::protobuf::Node]) -> Option<String> {
    let parts: Option<Vec<String>> = parts
        .iter()
        .map(|p| match &p.node {
            Some(N::String(s)) => Some(quote_ident(&s.sval)),
            _ => None,
        })
        .collect();
    parts.filter(|p| !p.is_empty()).map(|p| p.join("."))
}

/// The one statement `sql` holds, parsed.
fn parsed(sql: &str) -> Option<N> {
    let result = syntax::postgres::parse(sql, 0).ok()?;
    let [raw] = result.protobuf.stmts.as_slice() else { return None };
    raw.stmt.as_ref()?.node.clone()
}

/// The columns of a constraint's key, where each is a plain column.
fn key_columns(keys: &[syntax::postgres::protobuf::Node]) -> Option<Vec<String>> {
    keys.iter()
        .map(|k| match &k.node {
            Some(N::String(s)) => Some(quote_ident(&s.sval)),
            _ => None,
        })
        .collect()
}

/// The name of the check that proves a column holds no null before `set not null` reads it: the
/// column's, cut where a name would pass Postgres's 63 bytes.
fn not_null_check(column: &str) -> String {
    const SUFFIX: &str = "_not_null";
    format!("{}{SUFFIX}", cut(column, 63 - SUFFIX.len()))
}

/// `name` cut to at most `bytes` bytes, on a character's boundary.
fn cut(name: &str, bytes: usize) -> &str {
    let mut end = name.len().min(bytes);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

/// Whether a boolean option is on: written bare (`concurrently`), or with a value Postgres reads as
/// true (`true`, `on`, `1`, `yes`), never `false`, `off` or `0`.
fn option_on(d: &syntax::postgres::protobuf::DefElem) -> bool {
    match d.arg.as_ref().and_then(|a| a.node.as_ref()) {
        None => true,
        Some(N::Boolean(b)) => b.boolval,
        Some(N::Integer(i)) => i.ival != 0,
        Some(N::String(s)) => matches!(s.sval.to_ascii_lowercase().as_str(), "true" | "on" | "yes" | "1" | "t" | "y"),
        _ => false,
    }
}

/// Whether a parse tree's enum field holds `value`.
fn is<T: Into<i32>>(field: i32, value: T) -> bool {
    field == value.into()
}

/// The columns, as (table as the statement names it, column), that a statement of `plan` needs not
/// null when it runs: one given an identity, and the key of a primary key added to a table. Their
/// `set not null` can't wait for the last file.
fn needs_not_null(trees: &[Option<N>]) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for tree in trees {
        let Some(N::AlterTableStmt(a)) = tree else { continue };
        let Some(rel) = a.relation.as_ref().map(named) else { continue };
        for cmd in &a.cmds {
            let Some(N::AlterTableCmd(cmd)) = &cmd.node else { continue };
            if is(cmd.subtype, At::AtAddIdentity) {
                out.insert((rel.clone(), cmd.name.clone()));
            } else if is(cmd.subtype, At::AtAddInherit) {
                // A table that takes a parent must have its not nulls already: every column of it.
                out.insert((rel.clone(), "*".to_string()));
            } else if is(cmd.subtype, At::AtAttachPartition) {
                if let Some(N::PartitionCmd(p)) = cmd.def.as_ref().and_then(|d| d.node.as_ref()) {
                    if let Some(part) = p.name.as_ref() {
                        out.insert((named(part), "*".to_string()));
                    }
                }
            } else if is(cmd.subtype, At::AtAddConstraint) {
                if let Some(N::Constraint(c)) = cmd.def.as_ref().and_then(|d| d.node.as_ref()) {
                    if is(c.contype, ConstrType::ConstrPrimary) {
                        out.extend(c.keys.iter().filter_map(|k| match &k.node {
                            Some(N::String(s)) => Some((rel.clone(), s.sval.clone())),
                            _ => None,
                        }));
                    }
                }
            }
        }
    }
    out
}

/// What one statement of a plan becomes in the lock-safe shape. A table's columns a later statement
/// needs not null are `(table, column)`, or `(table, "*")` for every column of a table that joins a
/// parent, which must have its not nulls as it joins.
enum Shape {
    /// Kept as it is, in the file being written.
    Kept,
    /// In the file being written; then, in the last file, a validation (a scan that blocks no
    /// writes) and what finishes the change once every validation has run (`set not null`, which
    /// holds a lock that blocks reads and writes until the file commits).
    Later(String, String, Vec<String>),
    /// An index built concurrently in a file of its own, then what takes it as the plan's in the
    /// file after.
    Built(Build),
}

/// An index the plan builds on a table that has rows.
struct Build {
    /// The table, by its qualified name.
    table: String,
    /// The index's name, as the server stores it: the constraint's, for a key.
    name: String,
    what: Built,
}

enum Built {
    /// `create index`, as the plan writes it, and the schema its table is named in (`billing.`).
    Index { statement: String, schema: String },
    /// A unique constraint or a primary key: its index, then the constraint added over it.
    Key { rel: String, primary: bool, columns: Vec<String>, including: Vec<String>, nulls_not_distinct: bool },
}

/// `create [unique] index [if not exists] <name> on`, its name apart.
static NAMED: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r#"(?is)^(\s*create\s+(?:unique\s+)?index)((?:\s|/\*.*?\*/)+(?:if\s+not\s+exists(?:\s|/\*.*?\*/)+)?)("(?:[^"]|"")+"|[^\s"(/]+)((?:\s|/\*.*?\*/)+on\s)"#).expect("a valid pattern"));

/// A name as the server stores it: unquoted, folded to lower case.
fn stored(name: &str) -> String {
    match name.strip_prefix('"').and_then(|n| n.strip_suffix('"')) {
        Some(inner) => inner.replace("\"\"", "\""),
        None => name.to_lowercase(),
    }
}

impl Build {
    /// Whether it is a primary key's.
    fn primary(&self) -> bool {
        matches!(self.what, Built::Key { primary: true, .. })
    }

    /// Whether it can be built under another name than its own.
    fn renamable(&self) -> bool {
        match &self.what {
            Built::Index { statement, .. } => NAMED.captures(statement).is_some_and(|c| stored(&c[3]) == self.name),
            Built::Key { .. } => true,
        }
    }

    /// The statement that builds it concurrently, under the name `under`.
    fn create(&self, under: &str) -> String {
        match &self.what {
            Built::Index { statement, .. } => {
                let statement = statement.trim_end();
                match NAMED.captures(statement).filter(|_| under != self.name) {
                    Some(c) => format!("{} concurrently{}{}{}{}", &c[1], &c[2], quote_ident(under), &c[4], &statement[c.get(0).map_or(0, |m| m.end())..]),
                    // Under its own name, the statement as the plan writes it.
                    None => {
                        static CREATE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)^(\s*create\s+(?:unique\s+)?index)\b").expect("a valid pattern"));
                        CREATE.replace(statement, "$1 concurrently").into_owned()
                    }
                }
            }
            Built::Key { rel, columns, including, nulls_not_distinct, .. } => {
                let include = if including.is_empty() { String::new() } else { format!(" include ({})", including.join(", ")) };
                format!("create unique index concurrently {} on {rel} ({}){include}{}", quote_ident(under), columns.join(", "), if *nulls_not_distinct { " nulls not distinct" } else { "" })
            }
        }
    }

    /// What takes the index built under `under` as the plan's: the constraint added over it, which
    /// takes the constraint's name, or the index renamed to its own.
    fn finish(&self, under: &str) -> Vec<String> {
        match &self.what {
            Built::Index { schema, .. } if under != self.name => vec![format!("alter index {schema}{} rename to {}", quote_ident(under), quote_ident(&self.name))],
            Built::Index { .. } => vec![],
            Built::Key { rel, primary, .. } => {
                vec![format!("alter table {rel} add constraint {} {} using index {}", quote_ident(&self.name), if *primary { "primary key" } else { "unique" }, quote_ident(under))]
            }
        }
    }
}

/// `statement` (whose parse tree is `tree`) in the shape that holds no lock that blocks writes to a
/// table in `tables` while it reads every row.
fn shape(statement: &str, tree: Option<&N>, tables: &BTreeMap<String, String>, needs_not_null: &BTreeSet<(String, String)>) -> Shape {
    let kept = || Shape::Kept;
    let Some(node) = tree else { return kept() };
    match node {
        N::AlterTableStmt(a) => {
            let Some(rel) = a.relation.as_ref().map(named) else { return kept() };
            let Some(table) = tables.get(&rel) else { return kept() };
            let [cmd] = a.cmds.as_slice() else { return kept() };
            let Some(N::AlterTableCmd(cmd)) = &cmd.node else { return kept() };
            if is(cmd.subtype, At::AtAddConstraint) {
                let Some(N::Constraint(c)) = cmd.def.as_ref().and_then(|d| d.node.as_ref()) else { return kept() };
                if c.conname.is_empty() || c.skip_validation {
                    return kept();
                }
                // Added without reading a row, then validated later, which blocks no writes.
                if is(c.contype, ConstrType::ConstrCheck) || is(c.contype, ConstrType::ConstrForeign) {
                    return Shape::Later(format!("{} not valid", statement.trim_end()), format!("alter table {rel} validate constraint {}", quote_ident(&c.conname)), vec![]);
                }
                // Its index built concurrently, then taken as the constraint's: an index takes no
                // other storage parameters or tablespace in that place. Not a deferrable key's, whose
                // index would check each row at once until the key takes it, refusing writes its
                // deferral lets through.
                let plain = c.options.is_empty() && c.indexname.is_empty() && c.indexspace.is_empty() && c.access_method.is_empty() && !c.without_overlaps && c.where_clause.is_none() && !c.deferrable && !c.initdeferred;
                let primary = is(c.contype, ConstrType::ConstrPrimary);
                if (is(c.contype, ConstrType::ConstrUnique) || primary) && plain {
                    let (Some(columns), Some(including)) = (key_columns(&c.keys), key_columns(&c.including)) else { return kept() };
                    return Shape::Built(Build { table: table.clone(), name: c.conname.clone(), what: Built::Key { rel, primary, columns, including, nulls_not_distinct: c.nulls_not_distinct } });
                }
                return kept();
            }
            // A validated check that the column is not null lets `set not null` skip its scan.
            // Unless a later statement needs the column not null, which an identity and a primary
            // key do: then it stays where it is, and the lint says what it holds.
            if is(cmd.subtype, At::AtSetNotNull) && !cmd.name.is_empty() && !needs_not_null.contains(&(rel.clone(), cmd.name.clone())) && !needs_not_null.contains(&(rel.clone(), "*".to_string())) {
                let column = quote_ident(&cmd.name);
                let check = quote_ident(&not_null_check(&cmd.name));
                return Shape::Later(
                    format!("alter table {rel} add constraint {check} check ({column} is not null) not valid"),
                    format!("alter table {rel} validate constraint {check}"),
                    vec![statement.trim_end().to_string(), format!("alter table {rel} drop constraint {check}")],
                );
            }
            kept()
        }
        N::IndexStmt(i) if !i.concurrent => {
            let Some(relation) = i.relation.as_ref() else { return kept() };
            let Some(table) = tables.get(&named(relation)) else { return kept() };
            static CREATE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)^\s*create\s+(?:unique\s+)?index\b").expect("a valid pattern"));
            if !CREATE.is_match(statement) {
                return kept();
            }
            let schema = if relation.schemaname.is_empty() { String::new() } else { format!("{}.", quote_ident(&relation.schemaname)) };
            Shape::Built(Build { table: table.clone(), name: i.idxname.clone(), what: Built::Index { statement: statement.to_string(), schema } })
        }
        _ => kept(),
    }
}

/// The index a statement drops on a table in `tables`: `drop index`, or a constraint dropped whose
/// index is named for it (a unique constraint, a primary key or an exclusion constraint).
fn dropped_index(tree: Option<&N>, existing: &Existing, tables: &BTreeMap<String, String>) -> Option<Index> {
    match tree? {
        N::DropStmt(d) if is(d.remove_type, ObjectType::ObjectIndex) && !d.concurrent => {
            let [object] = d.objects.as_slice() else { return None };
            let Some(N::List(parts)) = &object.node else { return None };
            let index = existing.indexes.get(&named_parts(&parts.items)?)?;
            tables.values().any(|t| *t == index.table).then(|| index.clone())
        }
        N::AlterTableStmt(a) => {
            let table = tables.get(&named(a.relation.as_ref()?))?;
            let [cmd] = a.cmds.as_slice() else { return None };
            let Some(N::AlterTableCmd(cmd)) = &cmd.node else { return None };
            if !is(cmd.subtype, At::AtDropConstraint) {
                return None;
            }
            existing.indexes.values().find(|i| i.table == *table && i.name == cmd.name && i.key != ' ').cloned()
        }
        _ => None,
    }
}

/// The foreign key a statement drops, as (its table, its name, the statement the plan adds it again
/// in, the table it references), where the plan adds it again later.
fn dropped_foreign_key(trees: &[Option<N>], k: usize, existing: &Existing, tables: &BTreeMap<String, String>) -> Option<(String, String, usize, String)> {
    let Some(N::AlterTableStmt(a)) = trees[k].as_ref() else { return None };
    let table = tables.get(&named(a.relation.as_ref()?))?;
    let [cmd] = a.cmds.as_slice() else { return None };
    let Some(N::AlterTableCmd(cmd)) = &cmd.node else { return None };
    if !is(cmd.subtype, At::AtDropConstraint) || existing.indexes.values().any(|i| i.table == *table && i.name == cmd.name) {
        return None;
    }
    trees.iter().enumerate().skip(k + 1).find_map(|(m, tree)| {
        let Some(N::AlterTableStmt(b)) = tree else { return None };
        (tables.get(&named(b.relation.as_ref()?)) == Some(table)).then_some(())?;
        let [added] = b.cmds.as_slice() else { return None };
        let Some(N::AlterTableCmd(added)) = &added.node else { return None };
        let Some(N::Constraint(c)) = added.def.as_ref().and_then(|d| d.node.as_ref()) else { return None };
        if !is(added.subtype, At::AtAddConstraint) || !is(c.contype, ConstrType::ConstrForeign) || c.conname != cmd.name {
            return None;
        }
        Some((table.clone(), cmd.name.clone(), m, tables.get(&named(c.pktable.as_ref()?))?.clone()))
    })
}

/// Whether a statement does something to `table` that its index or constraint `name`, kept a while
/// longer, would change: drops or retypes a column, rewrites the table, moves, renames or drops it,
/// or renames `name` or another constraint to it. A column renamed takes its indexes with it.
fn disturbs(tree: Option<&N>, table: &str, name: &str, tables: &BTreeMap<String, String>) -> bool {
    let names = |r: Option<&RangeVar>| r.and_then(|r| tables.get(&named(r))).is_some_and(|t| t == table);
    match tree {
        Some(N::AlterTableStmt(a)) if names(a.relation.as_ref()) => a.cmds.iter().any(|cmd| {
            let Some(N::AlterTableCmd(cmd)) = &cmd.node else { return true };
            // Clustered on, or the replica identity: another index is no change to this one.
            if is(cmd.subtype, At::AtClusterOn) {
                return cmd.name == name;
            }
            if let Some(N::ReplicaIdentityStmt(r)) = cmd.def.as_ref().and_then(|d| d.node.as_ref()) {
                return r.name == name;
            }
            ![
                At::AtDropCluster,
                At::AtAlterConstraint,
                At::AtAddColumn,
                At::AtColumnDefault,
                At::AtDropNotNull,
                At::AtSetNotNull,
                At::AtSetStatistics,
                At::AtSetOptions,
                At::AtResetOptions,
                At::AtSetStorage,
                At::AtSetCompression,
                At::AtAddConstraint,
                At::AtValidateConstraint,
                At::AtDropConstraint,
                At::AtChangeOwner,
                At::AtSetRelOptions,
                At::AtResetRelOptions,
                At::AtEnableTrig,
                At::AtEnableAlwaysTrig,
                At::AtEnableReplicaTrig,
                At::AtDisableTrig,
                At::AtEnableRowSecurity,
                At::AtDisableRowSecurity,
                At::AtForceRowSecurity,
                At::AtNoForceRowSecurity,
                At::AtAddIdentity,
                At::AtSetIdentity,
                At::AtDropIdentity,
            ]
            .into_iter()
            .any(|ok| is(cmd.subtype, ok))
        }),
        Some(N::DropStmt(d)) => d.objects.iter().any(|o| matches!(&o.node, Some(N::List(parts)) if named_parts(&parts.items).and_then(|n| tables.get(&n)).is_some_and(|t| t == table))),
        Some(N::RenameStmt(r)) => names(r.relation.as_ref()) && !(is(r.rename_type, ObjectType::ObjectColumn) || (is(r.rename_type, ObjectType::ObjectTabconstraint) && r.subname != name && r.newname != name)),
        Some(N::AlterObjectSchemaStmt(o)) => names(o.relation.as_ref()),
        _ => false,
    }
}

/// The names of the relations and constraints a statement makes or renames to.
fn names_made(tree: Option<&N>) -> Vec<String> {
    let rel = |r: &Option<RangeVar>| r.as_ref().map(|r| r.relname.clone());
    let constraint = |n: &syntax::postgres::protobuf::Node| match &n.node {
        Some(N::Constraint(c)) if !c.conname.is_empty() => Some(c.conname.clone()),
        _ => None,
    };
    match tree {
        Some(N::IndexStmt(i)) => vec![i.idxname.clone()],
        Some(N::CreateStmt(c)) => rel(&c.relation)
            .into_iter()
            .chain(c.table_elts.iter().flat_map(|e| match &e.node {
                Some(N::ColumnDef(d)) => d.constraints.iter().filter_map(constraint).collect(),
                _ => constraint(e).into_iter().collect::<Vec<_>>(),
            }))
            .chain(c.constraints.iter().filter_map(constraint))
            .collect(),
        Some(N::CreateSeqStmt(c)) => rel(&c.sequence).into_iter().collect(),
        Some(N::ViewStmt(v)) => rel(&v.view).into_iter().collect(),
        Some(N::CompositeTypeStmt(c)) => rel(&c.typevar).into_iter().collect(),
        Some(N::CreateTableAsStmt(c)) => c.into.as_ref().and_then(|i| i.rel.as_ref()).map(|r| r.relname.clone()).into_iter().collect(),
        Some(N::RenameStmt(r)) => vec![r.newname.clone()],
        Some(N::AlterTableStmt(a)) => a
            .cmds
            .iter()
            .filter_map(|c| match &c.node {
                Some(N::AlterTableCmd(c)) if is(c.subtype, At::AtAddConstraint) => c.def.as_deref().and_then(constraint),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

/// Whether a statement names the index `name`: alters, renames or comments on it, clusters its
/// table on it, makes it the replica identity, or adds a constraint over it.
fn names_index(tree: Option<&N>, name: &str) -> bool {
    match tree {
        Some(N::AlterTableStmt(a)) => {
            a.relation.as_ref().is_some_and(|r| r.relname == name)
                || a.cmds.iter().any(|c| match &c.node {
                    Some(N::AlterTableCmd(c)) => {
                        (is(c.subtype, At::AtClusterOn) && c.name == name)
                            || match c.def.as_ref().and_then(|d| d.node.as_ref()) {
                                Some(N::ReplicaIdentityStmt(r)) => r.name == name,
                                Some(N::Constraint(k)) => k.indexname == name,
                                _ => false,
                            }
                    }
                    _ => false,
                })
        }
        Some(N::CommentStmt(c)) if is(c.objtype, ObjectType::ObjectIndex) => {
            matches!(c.object.as_deref().and_then(|o| o.node.as_ref()), Some(N::List(l)) if l.items.last().is_some_and(|i| matches!(&i.node, Some(N::String(s)) if s.sval == name)))
        }
        Some(N::RenameStmt(r)) => is(r.rename_type, ObjectType::ObjectIndex) && r.relation.as_ref().is_some_and(|x| x.relname == name),
        _ => false,
    }
}

/// A name for an index built while the one it replaces is still there: the index's own with
/// `_new` after it, or `_new2` and on where one of those is taken, cut to fit 63 bytes.
fn temporary(name: &str, taken: &mut BTreeSet<String>) -> String {
    let mut n = 1;
    loop {
        let suffix = if n == 1 { "_new".to_string() } else { format!("_new{n}") };
        let candidate = format!("{}{suffix}", cut(name, 63 - suffix.len()));
        if taken.insert(candidate.clone()) {
            return candidate;
        }
        n += 1;
    }
}

/// `plan`'s statements as files that hold no lock blocking writes to a table in `existing` (one
/// that already has rows) while they read every row: where any statement does, the files in order,
/// comments kept with the statement they come before; `None` where the plan holds no such lock.
/// An index (or a key's) the plan drops on a table it builds another on goes once that one is
/// built: where the new one takes its name, the new one is built under a temporary name and takes
/// it in the same transaction as the drop.
pub fn lock_safe(plan: &Plan, existing: &Existing) -> Option<Vec<Vec<String>>> {
    // A statement's tree is cloned and dropped here, a level at a time.
    syntax::postgres::on_deep_stack(|| plan_lock_safe(plan, existing, true))
}

/// `lock_safe`, but with each index the plan drops dropped where the plan has it, before what
/// replaces it is built: for a plan whose replacements, built first, don't reach its target.
pub fn lock_safe_dropping_first(plan: &Plan, existing: &Existing) -> Option<Vec<Vec<String>>> {
    syntax::postgres::on_deep_stack(|| plan_lock_safe(plan, existing, false))
}

fn plan_lock_safe(plan: &Plan, existing: &Existing, build_first: bool) -> Option<Vec<Vec<String>>> {
    let trees: Vec<Option<N>> = plan.statements.iter().map(|s| if s.starts_with("--") || s == crate::diff::COMMIT { None } else { parsed(s) }).collect();
    // A table or materialized view the plan makes (again, perhaps partitioned now) holds no rows the
    // plan reads under a lock: none of it is the database's as it stands.
    let made: BTreeSet<String> = trees
        .iter()
        .filter_map(|t| match t {
            Some(N::CreateStmt(c)) => c.relation.as_ref().map(named),
            Some(N::CreateTableAsStmt(c)) => c.into.as_ref().and_then(|i| i.rel.as_ref()).map(named),
            _ => None,
        })
        .map(|name| existing.tables.get(&name).cloned().unwrap_or(name))
        .collect();
    let tables: BTreeMap<String, String> = existing.tables.iter().filter(|(_, t)| !made.contains(*t)).map(|(k, v)| (k.clone(), v.clone())).collect();
    let needs_not_null = needs_not_null(&trees);
    let mut shapes: Vec<Option<Shape>> = plan.statements.iter().zip(&trees).map(|(s, t)| (!s.starts_with("--") && s != crate::diff::COMMIT).then(|| shape(s, t.as_ref(), &tables, &needs_not_null))).collect();

    // Each table's drops of an index or a key wait for its last build, which they then follow in one
    // transaction: the foreign keys on the keys it drops, the drops, what takes each new index in
    // the place of one dropped (renamed from a temporary name, or a key added over it), what names
    // those, then the foreign keys back. A build that takes a dropped one's name is built under a
    // temporary one; an exclusion constraint is never built concurrently, so its drop stays.
    let mut waits: BTreeMap<usize, Waits> = BTreeMap::new();
    // The statements that wait for a build, emitted there and not where the plan has them.
    let mut deferred: BTreeSet<usize> = BTreeSet::new();
    // The builds whose finish waits for their table's last build, by that build.
    let mut held: BTreeMap<usize, usize> = BTreeMap::new();
    let mut temporary_names: BTreeMap<usize, String> = BTreeMap::new();
    if build_first {
        let builds: Vec<(usize, &Build)> = shapes.iter().enumerate().filter_map(|(k, s)| match s {
            Some(Shape::Built(b)) => Some((k, b)),
            _ => None,
        }).collect();
        let mut taken = existing.names.clone();
        taken.extend(trees.iter().flat_map(|t| names_made(t.as_ref())));
        let drops: Vec<(usize, Index)> = trees.iter().enumerate().filter_map(|(k, t)| dropped_index(t.as_ref(), existing, &tables).map(|i| (k, i))).collect();
        for (k, index) in drops.iter().filter(|(_, i)| i.key != 'x') {
            let Some(at) = builds.iter().filter(|(j, b)| j > k && b.table == index.table).map(|(j, _)| *j).last() else { continue };
            let on: Vec<&(usize, &Build)> = builds.iter().filter(|(j, b)| j > k && *j <= at && b.table == index.table).collect();
            let renamed: Vec<usize> = on.iter().filter(|(_, b)| b.name == index.name).map(|(j, _)| *j).collect();
            if on.iter().any(|(j, b)| renamed.contains(j) && (!b.renamable() || temporary_names.contains_key(j))) {
                continue;
            }
            // Nothing between may need it gone: a column it covers dropped, its name taken. What
            // names the new one after it is built follows the swap.
            let follows = |m: usize| renamed.first().is_some_and(|j| m > *j) && names_index(trees[m].as_ref(), &index.name);
            if (k + 1..at).any(|m| !follows(m) && disturbs(trees[m].as_ref(), &index.table, &index.name, &tables)) || (k + 1..=at).any(|m| !renamed.contains(&m) && names_made(trees[m].as_ref()).contains(&index.name)) {
                continue;
            }
            deferred.insert(*k);
            waits.entry(at).or_default().drops.push(*k);
            for j in renamed {
                temporary_names.insert(j, temporary(&index.name, &mut taken));
                held.insert(j, at);
            }
            // A primary key is added once the old one is gone.
            if index.key == 'p' {
                held.extend(on.iter().filter(|(_, b)| b.primary()).map(|(j, _)| (*j, at)));
            }
        }
        // What names an index whose finish waits, between its build and that wait, follows it.
        for (&j, &at) in &held {
            let Some(name) = builds.iter().find(|(b, _)| *b == j).map(|(_, b)| b.name.clone()) else { continue };
            for m in j + 1..at {
                if !deferred.contains(&m) && names_index(trees[m].as_ref(), &name) {
                    deferred.insert(m);
                    waits.entry(at).or_default().named.push(m);
                }
            }
        }
        // A foreign key on a key the plan drops goes in the same transaction, where every drop on
        // the key's table waits, and comes back there once its key is in place.
        for k in 0..trees.len() {
            let Some((table, name, added, referenced)) = dropped_foreign_key(&trees, k, existing, &tables) else { continue };
            let on: Vec<&(usize, Index)> = drops.iter().filter(|(d, i)| i.table == referenced && *d > k).collect();
            let Some(at) = waits.iter().find(|(_, w)| on.iter().any(|(d, _)| w.drops.contains(d))).map(|(at, _)| *at) else { continue };
            if on.iter().any(|(d, _)| !deferred.contains(d)) || (k + 1..at).any(|m| disturbs(trees[m].as_ref(), &table, &name, &tables)) {
                continue;
            }
            deferred.insert(k);
            let w = waits.entry(at).or_default();
            w.keys.push(k);
            // Back in that transaction, but where a statement between them changes either table.
            let between = if added < at { added + 1..at } else { at + 1..added };
            if !between.into_iter().any(|m| disturbs(trees[m].as_ref(), &table, &name, &tables) || disturbs(trees[m].as_ref(), &referenced, &name, &tables)) {
                deferred.insert(added);
                w.back.push(added);
            }
        }
    }

    let mut files: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    // The last file: every validation first, then what finishes the changes, so that no
    // validation reads a table under the lock a finishing statement before it holds.
    let (mut validations, mut finishes): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    let mut comments: Vec<String> = Vec::new();
    let mut changed = false;
    // The statements that wait for a build, each with the comments above it, and the finishes.
    let mut stash: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut held_finishes: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    let mut emitted: BTreeSet<usize> = BTreeSet::new();
    // The session's settings in force (`set check_function_bodies = off` around functions). Each
    // file is whole on its own: one that ends with a setting still in force resets it, and the
    // file after a lone index build sets it again, since a deploy cut off between the files starts
    // the next run there, in a new session.
    let mut settings: Vec<String> = Vec::new();
    let resets = |settings: &[String]| -> Vec<String> { settings.iter().map(|s| format!("reset {}", setting_name(s))).collect() };
    for (k, statement) in plan.statements.iter().enumerate() {
        if statement.starts_with("--") {
            comments.push(statement.clone());
            continue;
        }
        // Where the plan commits partway (an enum's label added before what uses it), a file ends.
        if statement == crate::diff::COMMIT {
            changed = true;
            current.append(&mut comments);
            if does_something(&current) {
                current.extend(resets(&settings));
                files.push(std::mem::take(&mut current));
            } else {
                current.retain(|s| s.starts_with("--"));
            }
            current.extend(settings.iter().cloned());
            continue;
        }
        let lower = statement.trim_start().to_ascii_lowercase();
        if lower.starts_with("set ") && !lower.starts_with("set local ") {
            let name = setting_name(statement);
            settings.retain(|s| setting_name(s) != name);
            settings.push(statement.clone());
        } else if let Some(name) = lower.strip_prefix("reset ") {
            settings.retain(|s| setting_name(s) != name.trim());
        }
        if emitted.contains(&k) {
            current.append(&mut comments);
            continue;
        }
        if deferred.contains(&k) {
            changed = true;
            let mut with = std::mem::take(&mut comments);
            with.push(statement.clone());
            stash.insert(k, with);
            continue;
        }
        match shapes[k].take() {
            None | Some(Shape::Kept) => {
                current.append(&mut comments);
                current.push(statement.clone());
            }
            Some(Shape::Later(s, validation, finish)) => {
                changed = true;
                current.append(&mut comments);
                current.push(s);
                validations.push(validation);
                finishes.extend(finish);
            }
            Some(Shape::Built(build)) => {
                changed = true;
                // What came before goes in a file of its own, unless it is only settings, which
                // the file after this one repeats.
                if does_something(&current) {
                    current.extend(resets(&settings));
                    files.push(std::mem::take(&mut current));
                } else {
                    current.retain(|s| s.starts_with("--"));
                }
                let under = temporary_names.get(&k).cloned().unwrap_or_else(|| build.name.clone());
                let mut alone = std::mem::take(&mut current);
                alone.append(&mut comments);
                alone.push(build.create(&under));
                files.push(alone);
                current.extend(settings.iter().cloned());
                let mut own = build.finish(&under);
                if let Some(&at) = held.get(&k).filter(|&&at| at != k) {
                    held_finishes.entry(at).or_default().append(&mut own);
                }
                let Some(w) = waits.remove(&k) else {
                    current.extend(own);
                    continue;
                };
                for d in w.keys.iter().chain(&w.drops) {
                    current.extend(stash.remove(d).unwrap_or_default());
                }
                current.extend(held_finishes.remove(&k).unwrap_or_default());
                current.extend(own);
                for d in &w.named {
                    current.extend(stash.remove(d).unwrap_or_default());
                }
                // Each foreign key back, in the shape it has where it stands.
                for d in &w.back {
                    emitted.insert(*d);
                    let text = stash.remove(d).unwrap_or_else(|| vec![plan.statements[*d].clone()]);
                    match shapes[*d].take() {
                        Some(Shape::Later(s, validation, finish)) => {
                            current.extend(text.iter().filter(|t| t.starts_with("--")).cloned());
                            current.push(s);
                            validations.push(validation);
                            finishes.extend(finish);
                        }
                        _ => current.extend(text),
                    }
                }
            }
        }
    }
    current.append(&mut comments);
    if !changed {
        return None;
    }
    if does_something(&current) {
        files.push(current);
    }
    if !validations.is_empty() {
        validations.extend(finishes);
        files.push(validations);
    }
    Some(files)
}

/// The name a `set` statement sets, in lowercase: `check_function_bodies`.
fn setting_name(statement: &str) -> String {
    let lower = statement.trim_start().to_ascii_lowercase();
    let rest = lower.strip_prefix("set ").unwrap_or(&lower).trim_start();
    let rest = rest.strip_prefix("session ").unwrap_or(rest).trim_start();
    rest.split(['=', ' ']).next().unwrap_or_default().to_string()
}

/// Whether statements change anything: more than comments and the session's settings.
fn does_something(statements: &[String]) -> bool {
    statements.iter().any(|s| {
        let lower = s.trim_start().to_ascii_lowercase();
        !s.starts_with("--") && !lower.starts_with("set ") && !lower.starts_with("reset ")
    })
}

/// The files' texts, as `reylo migrate new` writes them: what the plan loses as comments at the top
/// of the first that drops anything, then each statement.
pub fn file_texts(plan: &Plan, files: &[Vec<String>]) -> Vec<String> {
    // What the plan loses heads the first file past its commits, which drops it: the files before
    // each hold only an enum's new labels.
    let commits = plan.statements.iter().filter(|s| *s == crate::diff::COMMIT).count();
    let losing = commits.min(files.len().saturating_sub(1));
    files
        .iter()
        .enumerate()
        .map(|(k, statements)| {
            let mut head = Plan { statements: statements.clone(), ..Plan::default() };
            if k == losing {
                head.losses = plan.losses.clone();
                head.narrowings = plan.narrowings.clone();
            }
            head.sql()
        })
        .collect()
}

/// A lock a statement holds on a table that already has rows, for as long as it reads them all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hazard {
    /// The table, as the database names it.
    pub table: String,
    /// What it does, as a sentence's predicate: `rewrites users, …`.
    pub what: String,
}

/// Runs a MySQL file as `deploy` runs it, saying where its `alter table`, on a table that was there
/// before it ran, blocks writes to it while it reads every row. That is MySQL's own answer: the
/// statement runs first with `lock=none`, which MySQL refuses where the change can't let writes
/// through (a column's type changed, which copies the table, a table partitioned anew, partitions
/// reorganized), and otherwise makes as it is. A file that holds its changes under `lock tables`
/// blocks reads and writes for as long as each runs, however MySQL makes it: each is tried with
/// `algorithm=instant` on an empty copy of its table, which MySQL answers as it would the table,
/// and one it can't make by changing the definition alone (it rebuilds or reads the table) blocks.
/// The file then runs as written.
async fn mysql_watched(db: &mut Db, m: &Migration) -> Result<Vec<Hazard>, Error> {
    let statements = crate::sql::split_statements(&m.sql, Dialect::Mysql);
    let there = mysql_tables(db).await?;
    if statements.len() > 2 && crate::sql::locks_tables(&statements[0]) {
        // A name no table or view of the database has.
        let taken: BTreeSet<String> = db.rows("select table_name as t from information_schema.tables where table_schema = database()").await?.iter().map(|r| r.text("t").to_lowercase()).collect();
        let copy = (0..).map(|n| format!("__reylo_lock_probe_{n}")).find(|c| !taken.contains(c)).expect("a free name");
        let mut hazards = Vec::new();
        for s in statements.iter().filter(|s| crate::sql::alters_table(s)) {
            let Some((table, tried)) = with_option(&s.text, Some(&copy), "algorithm=instant") else { continue };
            if !there.contains(&table.to_lowercase()) {
                continue;
            }
            // Where no copy can be made to try it on, it is taken to block.
            let refused = match db.exec(&format!("create table {copy} like `{}`", table.replace('`', "``"))).await {
                Ok(()) => {
                    let refused = matches!(db.script(&tried).await, Err(e) if e.0.contains("ALGORITHM=INSTANT is not supported"));
                    db.exec(&format!("drop table {copy}")).await?;
                    refused
                }
                Err(_) => true,
            };
            if refused {
                hazards.push(Hazard { what: mysql_hazard(&s.text, &table, true), table });
            }
        }
        run(db, m).await?;
        return Ok(hazards);
    }
    let tried = match statements.as_slice() {
        [one] if crate::sql::alters_table(one) => with_option(&one.text, None, "lock=none"),
        _ => None,
    };
    let Some((table, tried)) = tried.filter(|(table, _)| there.contains(&table.to_lowercase())) else {
        run(db, m).await?;
        return Ok(vec![]);
    };
    let written = &statements[0].text;
    // A statement that names its own lock takes the last it names: a lock that blocks writes is
    // the author's, said as it is; `lock=default` is tried as `lock=none` in its place.
    let tried = match named_lock(written) {
        Some((lock, ..)) if lock == "SHARED" || lock == "EXCLUSIVE" => {
            run(db, m).await?;
            return Ok(vec![Hazard { what: mysql_hazard(written, &table, lock == "EXCLUSIVE"), table }]);
        }
        Some((lock, start, end)) if lock == "DEFAULT" => format!("{}none{}", &written[..start], &written[end..]),
        Some(_) => written.clone(),
        None => tried,
    };
    match db.script(&tried).await {
        Ok(()) => Ok(vec![]),
        Err(e) if e.0.contains("LOCK=NONE is not supported") => {
            run(db, m).await?;
            Ok(vec![Hazard { what: mysql_hazard(written, &table, false), table }])
        }
        // A statement that fails for its own reasons fails the same way as written.
        Err(_) => run(db, m).await.map(|()| vec![]),
    }
}

/// The last `lock` clause an `alter table` names itself, uppercase, and where its value is written.
fn named_lock(statement: &str) -> Option<(String, usize, usize)> {
    let tokens = crate::sql::tokenize(statement, Dialect::Mysql);
    let mut depth = 0;
    let mut found = None;
    for (k, t) in tokens.iter().enumerate() {
        match t.text.as_str() {
            "(" => depth += 1,
            ")" => depth -= 1,
            _ => {}
        }
        if depth == 0 && k > 2 && t.upper() == "LOCK" && (k == 3 || tokens[k - 1].text == ",") {
            let at = if tokens.get(k + 1).is_some_and(|n| n.text == "=") { k + 2 } else { k + 1 };
            if let Some(value) = tokens.get(at) {
                found = Some((value.upper(), value.start, value.end));
            }
        }
    }
    found
}

/// The tables MySQL statements make (`create table`, not `if not exists`), each name unquoted and in
/// lowercase: a change to one of them after, in the same migration or in a pending one, reads no
/// rows.
pub(crate) fn mysql_tables_made<'a>(sqls: impl IntoIterator<Item = &'a str>) -> BTreeSet<String> {
    let mut made = BTreeSet::new();
    for sql in sqls {
        for statement in crate::sql::split_statements(sql, Dialect::Mysql) {
            let t = &statement.tokens;
            let words: Vec<String> = t.iter().take(5).map(crate::sql::Token::upper).collect();
            // One `if not exists` may find a table with rows, and make nothing.
            let at = match words.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
                ["CREATE", "TABLE", "IF", ..] => continue,
                ["CREATE", "TABLE", ..] => 2,
                _ => continue,
            };
            if let Some(name) = t.get(at) {
                made.insert(unquoted(name).to_lowercase());
            }
        }
    }
    made
}

/// A MySQL name token as MySQL reads it: a backquoted one without its quotes, a doubled backtick
/// in it read as one.
fn unquoted(name: &crate::sql::Token) -> String {
    match name.text.strip_prefix('`').and_then(|t| t.strip_suffix('`')) {
        Some(inner) => inner.replace("``", "`"),
        None => name.text.clone(),
    }
}

/// The base tables of a MySQL database, each name in lowercase.
pub(crate) async fn mysql_tables(db: &mut Db) -> Result<BTreeSet<String>, Error> {
    Ok(db.rows("select table_name as t from information_schema.tables where table_schema = database() and table_type = 'BASE TABLE'").await?.iter().map(|r| r.text("t").to_lowercase()).collect())
}

/// What a MySQL `alter table` that blocks writes does to `table`, as a sentence's predicate; under
/// `lock tables` (`reads`), it blocks reads as well.
fn mysql_hazard(statement: &str, table: &str, reads: bool) -> String {
    let blocks = if reads { "reads and writes" } else { "writes" };
    let lower = statement.to_ascii_lowercase();
    if let Some(at) = lower.find("reorganize partition ") {
        let names = statement[at + "reorganize partition ".len()..].split(" into ").next().unwrap_or_default().trim();
        return format!("reorganizes {table}'s partitions {names}, copying their rows while it blocks {blocks} to it");
    }
    if lower.contains(" partition by ") || lower.contains(" remove partitioning") {
        return format!("partitions {table} anew, copying every row while it blocks {blocks} to it");
    }
    if lower.contains(" modify column ") || lower.contains(" change column ") || lower.contains(" convert to ") {
        return format!("rewrites {table}, copying every row while it blocks {blocks} to it");
    }
    format!("blocks {blocks} to {table} while it reads every row")
}

/// A MySQL `alter table` with `option` (`lock=none`) among its clauses, on the table `on` names
/// where it names one, and the table it alters, unquoted: the option goes first, and before a
/// partitioning clause with no comma, as MySQL's grammar has it.
fn with_option(statement: &str, on: Option<&str>, option: &str) -> Option<(String, String)> {
    let tokens = crate::sql::tokenize(statement, Dialect::Mysql);
    let (name, next) = match tokens.as_slice() {
        [alter, table, name, next, ..] if alter.upper() == "ALTER" && table.upper() == "TABLE" => (name, next),
        _ => return None,
    };
    // A qualified name (`app.users`) is refused by the shadow check before it runs.
    if next.text == "." {
        return None;
    }
    let table = unquoted(name);
    let options = matches!(next.upper().as_str(), "PARTITION" | "REMOVE");
    let head = format!("{}{}{}", &statement[..name.start], on.unwrap_or(&name.text), &statement[name.end..next.start]);
    Some((table, format!("{head}{option}{} {}", if options { "" } else { "," }, &statement[next.start..])))
}

/// The check constraints that prove a column holds no null: (table, column) to (name, validated).
async fn not_null_proofs(db: &mut Db) -> Result<BTreeMap<(String, String), (String, bool)>, Error> {
    static PROOF: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r#"^CHECK \(\(?("(?:[^"]|"")+"|[a-z_][a-z0-9_$]*) IS NOT NULL\)?\)(?: NOT VALID)?$"#).expect("a valid pattern"));
    let rows = db.rows("select conrelid::regclass::text as t, conname as name, convalidated as valid, pg_get_constraintdef(oid) as def from pg_constraint where contype = 'c'").await?;
    let mut out = BTreeMap::new();
    for r in rows {
        if let Some(c) = PROOF.captures(r.text("def")) {
            let column = c[1].trim_matches('"').replace("\"\"", "\"");
            out.insert((r.text("t").to_string(), column), (r.text("name").to_string(), r.flag("valid")));
        }
    }
    Ok(out)
}

/// A relation's storage, as `pg_class` records it.
struct Stored {
    oid: String,
    file: String,
    /// `r`, `m` or `p` for a table, `i` for an index.
    kind: String,
    /// An index's table; a table's own name.
    table: String,
}

/// The storage of each table, materialized view, partitioned table and index of those, by the name
/// the database gives it. A table copied (a type change that rewrites it, a volatile default,
/// `vacuum full`) gets new storage, and so does an index built again (a type or collation change of
/// a column it covers, which may leave the table's own storage as it was); a partitioned table has
/// none, and is here so that a statement that names it is read as naming a table.
async fn storage(db: &mut Db) -> Result<BTreeMap<String, Stored>, Error> {
    let rows = db
        .rows(
            r"select c.oid::regclass::text as t, c.oid::text as oid, c.relfilenode::text as file, c.relkind::text as kind,
              coalesce((select x.indrelid::regclass::text from pg_index x where x.indexrelid = c.oid), c.oid::regclass::text) as owner
            from pg_class c join pg_namespace n on n.oid = c.relnamespace
            where c.relkind in ('r', 'm', 'p', 'i') and n.nspname not in ('pg_catalog', 'information_schema') and n.nspname not like 'pg\_%'",
        )
        .await?;
    Ok(rows.iter().map(|r| (r.text("t").to_string(), Stored { oid: r.text("oid").to_string(), file: r.text("file").to_string(), kind: r.text("kind").to_string(), table: r.text("owner").to_string() })).collect())
}

/// Runs `m` as `deploy` runs it, saying each lock it holds on a table that was there before it ran
/// while it reads every row: the parse tree for a scan (a validated constraint, an index built
/// without `concurrently`, a `set not null` no validated check proves, a validation after an
/// earlier statement of the file took a lock that blocks writes, which it holds until the file
/// commits), and the storage of tables and their indexes for a copy or a rebuild. On MySQL, as the
/// server answers ([`mysql_watched`]); on SQLite it runs the file and says nothing.
pub(crate) async fn run_watched(db: &mut Db, m: &Migration) -> Result<Vec<Hazard>, Error> {
    if db.dialect() == Dialect::Mysql {
        return mysql_watched(db, m).await;
    }
    if db.dialect() != Dialect::Postgres {
        run(db, m).await?;
        return Ok(vec![]);
    }
    let before = storage(db).await?;
    let proofs = not_null_proofs(db).await?;
    // A statement names a table as the search path finds it; the database's own name for it is
    // what the hazard gives.
    let tables: BTreeSet<String> = before.iter().filter(|(_, s)| s.kind != "i").map(|(name, _)| name.clone()).collect();
    let resolve = |name: &str| -> Option<String> { tables.contains(name).then(|| name.to_string()).or_else(|| tables.iter().find(|k| k.rsplit_once('.').is_some_and(|(_, t)| t == name)).cloned()) };
    let mut hazards: Vec<Hazard> = Vec::new();
    let mut truncated: BTreeSet<String> = BTreeSet::new();
    let mut validated: BTreeSet<(String, String)> = BTreeSet::new();
    // The tables an earlier statement of the file locked against writes, until it commits.
    let mut locked: BTreeSet<String> = BTreeSet::new();
    // The indexes the file makes by name, which may be ones it dropped first, and those it builds
    // again concurrently, which blocks no writes; and the tables whose every index it so rebuilds.
    let mut made: BTreeSet<String> = BTreeSet::new();
    let mut reindexed: BTreeSet<String> = BTreeSet::new();
    // The columns an earlier statement of the file made not null, by table; and each primary key
    // added over an index, as (table, index, those columns then), whose key columns not yet not
    // null it checks every row of.
    let mut nulled: BTreeSet<(String, String)> = BTreeSet::new();
    let mut keyed: Vec<(String, String, BTreeSet<String>)> = Vec::new();
    let mut add = |table: &str, what: String| {
        let hazard = Hazard { table: table.to_string(), what };
        if !hazards.contains(&hazard) {
            hazards.push(hazard);
        }
    };
    // Its tree is read and dropped a level at a time.
    syntax::postgres::on_deep_stack(|| {
        if let Ok(result) = syntax::postgres::parse(&m.sql, 0) {
        for raw in &result.protobuf.stmts {
            match raw.stmt.as_ref().and_then(|s| s.node.as_ref()) {
                Some(N::AlterTableStmt(a)) => {
                    let Some(table) = a.relation.as_ref().and_then(|r| resolve(&named(r))) else { continue };
                    let mut locks = false;
                    for cmd in &a.cmds {
                        let Some(N::AlterTableCmd(cmd)) = &cmd.node else { continue };
                        if is(cmd.subtype, At::AtValidateConstraint) {
                            // A validation alone blocks no writes, but under a lock the file
                            // already holds it reads every row with writes blocked.
                            if locked.contains(&table) {
                                add(&table, format!("checks every row of {table} while an earlier statement's lock blocks writes to it"));
                            }
                            validated.insert((table.clone(), cmd.name.clone()));
                            continue;
                        }
                        locks = true;
                        if is(cmd.subtype, At::AtAddConstraint) {
                            let Some(N::Constraint(c)) = cmd.def.as_ref().and_then(|d| d.node.as_ref()) else { continue };
                            if is(c.contype, ConstrType::ConstrCheck) && !c.skip_validation {
                                add(&table, format!("checks every row of {table} while it blocks reads and writes to it"));
                            } else if is(c.contype, ConstrType::ConstrForeign) && !c.skip_validation {
                                add(&table, format!("checks every row of {table} while it blocks writes to it"));
                            } else if (is(c.contype, ConstrType::ConstrUnique) || is(c.contype, ConstrType::ConstrPrimary) || is(c.contype, ConstrType::ConstrExclusion)) && c.indexname.is_empty() {
                                add(&table, format!("builds an index on {table} while it blocks reads and writes to it"));
                            } else if !c.indexname.is_empty() {
                                // The index takes the constraint's name.
                                made.insert(c.conname.clone());
                                if is(c.contype, ConstrType::ConstrPrimary) {
                                    let set = nulled.iter().filter(|(t, _)| *t == table).map(|(_, column)| column.clone()).collect();
                                    keyed.push((table.clone(), c.indexname.clone(), set));
                                }
                            }
                        } else if is(cmd.subtype, At::AtSetNotNull) {
                            nulled.insert((table.clone(), cmd.name.clone()));
                            let proven = proofs.get(&(table.clone(), cmd.name.clone())).is_some_and(|(name, valid)| *valid || validated.contains(&(table.clone(), name.clone())));
                            if !proven {
                                add(&table, format!("checks every row of {table} for a null in {} while it blocks reads and writes to it", quote_ident(&cmd.name)));
                            }
                        }
                    }
                    if locks {
                        locked.insert(table);
                    }
                }
                Some(N::IndexStmt(i)) if !i.concurrent => {
                    made.insert(i.idxname.clone());
                    if let Some(table) = i.relation.as_ref().and_then(|r| resolve(&named(r))) {
                        add(&table, format!("builds an index on {table} while it blocks writes to it"));
                        locked.insert(table);
                    }
                }
                Some(N::ReindexStmt(r)) if r.params.iter().any(|p| matches!(&p.node, Some(N::DefElem(d)) if d.defname == "concurrently" && option_on(d))) => {
                    let Some(rel) = &r.relation else { continue };
                    if is(r.kind, syntax::postgres::protobuf::ReindexObjectType::ReindexObjectIndex) {
                        made.insert(rel.relname.clone());
                    } else if is(r.kind, syntax::postgres::protobuf::ReindexObjectType::ReindexObjectTable) {
                        reindexed.extend(resolve(&named(rel)));
                    }
                }
                Some(N::RenameStmt(r)) if is(r.rename_type, ObjectType::ObjectIndex) => {
                    made.insert(r.newname.clone());
                }
                Some(N::TruncateStmt(t)) => {
                    for r in &t.relations {
                        if let Some(N::RangeVar(r)) = &r.node {
                            truncated.extend(resolve(&named(r)));
                        }
                    }
                }
                _ => {}
            }
        }
        }
    });
    for (table, index, set) in keyed {
        let literal = |s: &str| format!("'{}'", s.replace('\'', "''"));
        let open = db
            .rows(&format!(
                "select a.attname as name from pg_index i join pg_class c on c.oid = i.indexrelid join pg_attribute a on a.attrelid = i.indrelid and a.attnum = any((i.indkey::int2[])[0:i.indnkeyatts - 1])
                where i.indrelid = to_regclass({}) and c.relname = {} and not a.attnotnull order by a.attnum",
                literal(&table),
                literal(&index)
            ))
            .await?;
        for column in open.iter().map(|r| r.text("name").to_string()).filter(|c| !set.contains(c)) {
            let hazard = Hazard { table: table.clone(), what: format!("checks every row of {table} for a null in {} while it blocks reads and writes to it", quote_ident(&column)) };
            if !hazards.contains(&hazard) {
                hazards.push(hazard);
            }
        }
    }
    run(db, m).await?;
    // A table renamed keeps its storage; one given new storage was copied, but for a truncate,
    // which empties it at once. An index given new storage on a table that wasn't copied was
    // built again, reading every row while its table's lock blocks reads and writes.
    let after = storage(db).await?;
    let moved = |s: &Stored| after.values().any(|a| a.oid == s.oid && a.file != s.file);
    let mut copied: BTreeSet<String> = BTreeSet::new();
    for (table, s) in before.iter().filter(|(_, s)| s.kind == "r" || s.kind == "m") {
        if !truncated.contains(table) && moved(s) {
            copied.insert(table.clone());
            hazards.retain(|h| &h.table != table);
            hazards.push(Hazard { table: table.clone(), what: format!("rewrites {table}, copying every row while it blocks reads and writes to it") });
        }
    }
    // Postgres builds an index again under its name with a new oid, so an index is followed by its
    // name: there with other storage, where the file didn't make one of that name itself.
    let rebuilt = |name: &str, s: &Stored| after.get(name).is_some_and(|a| a.kind == "i" && (a.oid != s.oid || a.file != s.file));
    for (index, s) in before.iter().filter(|(_, s)| s.kind == "i") {
        let bare = index.rsplit_once('.').map_or(index.as_str(), |(_, n)| n).trim_matches('"');
        if !truncated.contains(&s.table) && !copied.contains(&s.table) && !reindexed.contains(&s.table) && !made.contains(bare) && rebuilt(index, s) {
            let hazard = Hazard { table: s.table.clone(), what: format!("builds {index} again, reading every row of {} while it blocks reads and writes to it", s.table) };
            if !hazards.contains(&hazard) {
                hazards.push(hazard);
            }
        }
    }
    Ok(hazards)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(statements: &[&str]) -> Plan {
        Plan { statements: statements.iter().map(|s| s.to_string()).collect(), ..Plan::default() }
    }

    fn existing(names: &[&str]) -> Existing {
        Existing::of(names)
    }

    fn strings(statements: &[&str]) -> Vec<String> {
        statements.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn asks_mysql_to_change_a_table_letting_writes_through() {
        let tried = |s: &str| with_option(s, None, "lock=none").map(|(t, s)| format!("{t}: {s}"));
        assert_eq!(tried("alter table people modify column mood enum('ok') not null").as_deref(), Some("people: alter table people lock=none, modify column mood enum('ok') not null"));
        assert_eq!(tried("alter table readings\n  modify column level tinyint,\n  modify column code varchar(3)").as_deref(), Some("readings: alter table readings\n  lock=none, modify column level tinyint,\n  modify column code varchar(3)"));
        // A partitioning clause takes the option before it with no comma.
        assert_eq!(tried("alter table m partition by hash (id) partitions 4").as_deref(), Some("m: alter table m lock=none partition by hash (id) partitions 4"));
        assert_eq!(tried("alter table `order` remove partitioning").as_deref(), Some("order: alter table `order` lock=none remove partitioning"));
        assert_eq!(tried("alter table `a``` modify column n bigint").as_deref(), Some("a`: alter table `a``` lock=none, modify column n bigint"));
        assert_eq!(mysql_tables_made(["create table `a``` (id int); create table if not exists b (id int); create table C (id int)"]), ["a`".to_string(), "c".to_string()].into_iter().collect());
        assert_eq!(tried("alter table m add partition (partition p2 values less than (3))").as_deref(), Some("m: alter table m lock=none, add partition (partition p2 values less than (3))"));
        assert_eq!(tried("create table t (id int)"), None);
        assert_eq!(tried("alter table app.users add column bio text"), None);
        // On a copy of the table.
        assert_eq!(with_option("alter table `order` rename column n to m", Some("probe"), "algorithm=instant").map(|(t, s)| format!("{t}: {s}")).as_deref(), Some("order: alter table probe algorithm=instant, rename column n to m"));
        assert_eq!(mysql_hazard("alter table m reorganize partition p1, pmax into (partition p2 values less than (3), partition pmax values less than maxvalue)", "m", false), "reorganizes m's partitions p1, pmax, copying their rows while it blocks writes to it");
        assert_eq!(mysql_hazard("alter table m partition by hash (id) partitions 4", "m", false), "partitions m anew, copying every row while it blocks writes to it");
        assert_eq!(mysql_hazard("alter table p modify column n bigint", "p", true), "rewrites p, copying every row while it blocks reads and writes to it");
        assert_eq!(mysql_hazard("alter table p add fulltext index f (body)", "p", false), "blocks writes to p while it reads every row");
        assert_eq!(named_lock("alter table p modify column n bigint, lock = shared").map(|(l, ..)| l).as_deref(), Some("SHARED"));
        assert_eq!(named_lock("alter table p lock=exclusive, modify column n bigint, lock=default").map(|(l, s, e)| format!("{l} {s} {e}")).as_deref(), Some("DEFAULT 59 66"));
        assert_eq!(named_lock("alter table p add column lock int"), None);
    }

    #[test]
    fn leaves_a_plan_with_nothing_to_lock_alone() {
        assert_eq!(lock_safe(&plan(&["create table t (id int)", "alter table users add column bio text"]), &existing(&["users"])), None);
        // A new table has no rows to read.
        assert_eq!(lock_safe(&plan(&["create table t (id int)", "create index t_id on t (id)", "alter table t add constraint t_pos check (id > 0)"]), &existing(&["users"])), None);
    }

    #[test]
    fn validates_constraints_in_a_later_file() {
        let files = lock_safe(&plan(&["alter table users add constraint n_pos check (n > 0)", "alter table posts add constraint posts_user_id_fkey foreign key (user_id) references users(id)"]), &existing(&["users", "posts"])).unwrap();
        assert_eq!(
            files,
            vec![
                vec!["alter table users add constraint n_pos check (n > 0) not valid".to_string(), "alter table posts add constraint posts_user_id_fkey foreign key (user_id) references users(id) not valid".to_string()],
                vec!["alter table users validate constraint n_pos".to_string(), "alter table posts validate constraint posts_user_id_fkey".to_string()],
            ]
        );
    }

    #[test]
    fn proves_not_null_with_a_check_first() {
        let files = lock_safe(&plan(&["alter table users alter column email set not null"]), &existing(&["users"])).unwrap();
        assert_eq!(
            files,
            vec![
                vec!["alter table users add constraint email_not_null check (email is not null) not valid".to_string()],
                vec!["alter table users validate constraint email_not_null".to_string(), "alter table users alter column email set not null".to_string(), "alter table users drop constraint email_not_null".to_string()],
            ]
        );
    }

    #[test]
    fn validates_everything_before_any_column_is_made_not_null() {
        let files = lock_safe(&plan(&["alter table users alter column a set not null", "alter table users alter column b set not null"]), &existing(&["users"])).unwrap();
        assert_eq!(
            files[1],
            vec![
                "alter table users validate constraint a_not_null".to_string(),
                "alter table users validate constraint b_not_null".to_string(),
                "alter table users alter column a set not null".to_string(),
                "alter table users drop constraint a_not_null".to_string(),
                "alter table users alter column b set not null".to_string(),
                "alter table users drop constraint b_not_null".to_string(),
            ]
        );
    }

    #[test]
    fn keeps_a_not_null_an_identity_or_a_primary_key_needs() {
        // Adding an identity, or a primary key, refuses a column not yet not null.
        assert_eq!(lock_safe(&plan(&["alter table v alter column r set not null", "alter table v alter column r add generated always as identity"]), &existing(&["v"])), None);
        // The key's index is built concurrently after it, all the same.
        let files = lock_safe(&plan(&["alter table v alter column r set not null", "alter table v add constraint v_pkey primary key (r)"]), &existing(&["v"])).unwrap();
        assert_eq!(files[0], strings(&["alter table v alter column r set not null"]));
        // Another column's is still proven first.
        let files = lock_safe(&plan(&["alter table v alter column r set not null", "alter table v alter column r add generated always as identity", "alter table v alter column s set not null"]), &existing(&["v"])).unwrap();
        assert_eq!(files[0], vec!["alter table v alter column r set not null".to_string(), "alter table v alter column r add generated always as identity".to_string(), "alter table v add constraint s_not_null check (s is not null) not valid".to_string()]);
    }

    #[test]
    fn builds_indexes_concurrently_alone() {
        let files = lock_safe(&plan(&["alter table users add column code text", "-- the index", "create unique index users_code_key on users (code)", "alter table users add constraint users_email_key unique (email)", "comment on table users is 'x'"]), &existing(&["users"])).unwrap();
        assert_eq!(
            files,
            vec![
                vec!["alter table users add column code text".to_string()],
                vec!["-- the index".to_string(), "create unique index concurrently users_code_key on users (code)".to_string()],
                vec!["create unique index concurrently users_email_key on users (email)".to_string()],
                vec!["alter table users add constraint users_email_key unique using index users_email_key".to_string(), "comment on table users is 'x'".to_string()],
            ]
        );
    }

    #[test]
    fn reads_an_index_on_a_deeply_nested_expression() {
        let sum = vec!["n"; 3501].join(" + ");
        let index = format!("create index users_sum on users (({sum}))");
        let files = lock_safe(&plan(&[index.as_str(), "alter table users add constraint n_pos check (n > 0)"]), &existing(&["users"])).expect("a lock-safe plan");
        let all = files.concat();
        assert!(all.iter().any(|s| s.starts_with("create index concurrently users_sum")));
        assert!(all.iter().any(|s| s.contains("n_pos check (n > 0) not valid")));
    }

    #[test]
    fn repeats_session_settings_after_a_lone_file() {
        let files = lock_safe(&plan(&["set check_function_bodies = off", "create index i on users (n)", "create function f() returns int language sql as 'select 1'", "reset check_function_bodies"]), &existing(&["users"])).unwrap();
        assert_eq!(
            files,
            vec![
                vec!["create index concurrently i on users (n)".to_string()],
                vec!["set check_function_bodies = off".to_string(), "create function f() returns int language sql as 'select 1'".to_string(), "reset check_function_bodies".to_string()],
            ]
        );
    }

    #[test]
    fn resets_a_setting_a_file_leaves_in_force() {
        let files = lock_safe(&plan(&["set check_function_bodies = off", "create function f() returns int language sql as 'select 1'", "create index i on users (n)", "reset check_function_bodies"]), &existing(&["users"])).unwrap();
        assert_eq!(
            files,
            vec![
                vec!["set check_function_bodies = off".to_string(), "create function f() returns int language sql as 'select 1'".to_string(), "reset check_function_bodies".to_string()],
                vec!["create index concurrently i on users (n)".to_string()],
            ]
        );
    }

    #[test]
    fn builds_a_key_concurrently_alone() {
        let files = lock_safe(
            &plan(&["alter table teams add constraint teams_code_key unique nulls not distinct (code) include (name)", "alter table teams add constraint teams_pkey primary key (id)"]),
            &existing(&["teams"]),
        )
        .unwrap();
        assert_eq!(
            files,
            vec![
                strings(&["create unique index concurrently teams_code_key on teams (code) include (name) nulls not distinct"]),
                strings(&["alter table teams add constraint teams_code_key unique using index teams_code_key"]),
                strings(&["create unique index concurrently teams_pkey on teams (id)"]),
                strings(&["alter table teams add constraint teams_pkey primary key using index teams_pkey"]),
            ]
        );
        // A deferrable key is added as it is: an index built first would check each row at once.
        for deferrable in ["alter table teams add constraint teams_name_key unique (name) deferrable", "alter table teams add constraint teams_pkey primary key (id) initially deferred"] {
            assert_eq!(lock_safe(&plan(&[deferrable]), &existing(&["teams"])), None, "{deferrable}");
        }
    }

    #[test]
    fn builds_an_index_on_a_materialized_view_concurrently() {
        let files = lock_safe(&plan(&["create index room_totals_total on room_totals (total desc)"]), &existing(&["room_totals"])).unwrap();
        assert_eq!(files, vec![strings(&["create index concurrently room_totals_total on room_totals (total desc)"])]);
        // One the plan makes again has no rows the plan reads under a lock.
        let remade = ["drop materialized view room_totals", "create materialized view room_totals as select 1 as total", "create index room_totals_total on room_totals (total desc)"];
        assert_eq!(lock_safe(&plan(&remade), &existing(&["room_totals"])), None);
    }

    #[test]
    fn builds_a_changed_index_before_dropping_the_old_one() {
        let at = existing(&["users"]).with_index("users_email_lower", "users", ' ');
        let files = lock_safe(&plan(&["drop index users_email_lower", "create unique index users_email_lower on users (lower(trim(email)))"]), &at).unwrap();
        assert_eq!(
            files,
            vec![
                strings(&["create unique index concurrently users_email_lower_new on users (lower(trim(email)))"]),
                strings(&["drop index users_email_lower", "alter index users_email_lower_new rename to users_email_lower"]),
            ]
        );
        // A schema the table is named in is the index's.
        let at = existing(&["billing.invoices"]).with_index("billing.invoices_due", "billing.invoices", ' ');
        let files = lock_safe(&plan(&["drop index billing.invoices_due", "create index invoices_due on billing.invoices (due desc)"]), &at).unwrap();
        assert_eq!(files[1], strings(&["drop index billing.invoices_due", "alter index billing.invoices_due_new rename to invoices_due"]));
    }

    #[test]
    fn takes_a_temporary_name_no_relation_holds() {
        let at = existing(&["users", "users_n_new"]).with_index("users_n", "users", ' ');
        let files = lock_safe(&plan(&["drop index users_n", "create index users_n on users (n desc)"]), &at).unwrap();
        assert_eq!(files[0], strings(&["create index concurrently users_n_new2 on users (n desc)"]));
        // Cut to fit 63 bytes.
        let long = "i".repeat(63);
        let at = existing(&["users"]).with_index(&long, "users", ' ');
        let files = lock_safe(&plan(&[&format!("drop index {long}"), &format!("create index {long} on users (n desc)")]), &at).unwrap();
        assert_eq!(files[0], vec![format!("create index concurrently {}_new on users (n desc)", "i".repeat(59))]);
    }

    #[test]
    fn builds_a_changed_key_before_dropping_the_old_one() {
        let at = existing(&["invites"]).with_index("invites_email_team", "invites", 'u');
        let files = lock_safe(&plan(&["alter table invites drop constraint invites_email_team", "alter table invites add constraint invites_email_team unique (email, team_id)"]), &at).unwrap();
        assert_eq!(
            files,
            vec![
                strings(&["create unique index concurrently invites_email_team_new on invites (email, team_id)"]),
                strings(&["alter table invites drop constraint invites_email_team", "alter table invites add constraint invites_email_team unique using index invites_email_team_new"]),
            ]
        );
        // Under another name, the old one goes once the new one is in place.
        let at = existing(&["t"]).with_index("t_org_email_key", "t", 'u');
        let files = lock_safe(&plan(&["alter table t drop constraint t_org_email_key", "alter table t add constraint t_email_key unique (email)"]), &at).unwrap();
        assert_eq!(files, vec![strings(&["create unique index concurrently t_email_key on t (email)"]), strings(&["alter table t drop constraint t_org_email_key", "alter table t add constraint t_email_key unique using index t_email_key"])]);
        // An index made a constraint, and a constraint made an index.
        let at = existing(&["t"]).with_index("t_n_key", "t", ' ');
        let files = lock_safe(&plan(&["drop index t_n_key", "alter table t add constraint t_n_key unique (n)"]), &at).unwrap();
        assert_eq!(files[1], strings(&["drop index t_n_key", "alter table t add constraint t_n_key unique using index t_n_key_new"]));
        let at = existing(&["t"]).with_index("t_n_key", "t", 'u');
        let files = lock_safe(&plan(&["alter table t drop constraint t_n_key", "create unique index t_n_key on t (n)"]), &at).unwrap();
        assert_eq!(files[1], strings(&["alter table t drop constraint t_n_key", "alter index t_n_key_new rename to t_n_key"]));
    }

    #[test]
    fn drops_a_replaced_index_once_its_tables_last_build_is_done() {
        let at = existing(&["t", "u"]).with_index("t_a", "t", ' ');
        let files = lock_safe(&plan(&["drop index t_a", "alter table t add column c int", "create index t_b on t (b)", "create index t_c on t (c)", "create index u_x on u (x)"]), &at).unwrap();
        assert_eq!(
            files,
            vec![
                strings(&["alter table t add column c int"]),
                strings(&["create index concurrently t_b on t (b)"]),
                strings(&["create index concurrently t_c on t (c)"]),
                strings(&["drop index t_a"]),
                strings(&["create index concurrently u_x on u (x)"]),
            ]
        );
    }

    #[test]
    fn keeps_every_index_a_table_has_until_its_last_build() {
        // A name given to a new definition while the old one moves to another name.
        let at = existing(&["accounts"]).with_index("accounts_a", "accounts", ' ');
        let files = lock_safe(&plan(&["drop index accounts_a", "create unique index accounts_a on accounts (handle)", "create unique index accounts_z on accounts (email)"]), &at).unwrap();
        assert_eq!(
            files,
            vec![
                strings(&["create unique index concurrently accounts_a_new on accounts (handle)"]),
                strings(&["create unique index concurrently accounts_z on accounts (email)"]),
                strings(&["drop index accounts_a", "alter index accounts_a_new rename to accounts_a"]),
            ]
        );
        // A primary key moved to another column, the old key column kept unique and referenced.
        let at = existing(&["posts", "comments"]).with_index("posts_pkey", "posts", 'p');
        let files = lock_safe(
            &plan(&[
                "alter table comments drop constraint comments_post_slug_fkey",
                "alter table posts drop constraint posts_pkey",
                "alter table posts add constraint posts_pkey primary key (id)",
                "alter table posts add constraint posts_slug_key unique (slug)",
                "alter table comments add constraint comments_post_slug_fkey foreign key (post_slug) references posts(slug)",
            ]),
            &at,
        )
        .unwrap();
        assert_eq!(
            files,
            vec![
                strings(&["create unique index concurrently posts_pkey_new on posts (id)"]),
                strings(&["create unique index concurrently posts_slug_key on posts (slug)"]),
                strings(&[
                    "alter table comments drop constraint comments_post_slug_fkey",
                    "alter table posts drop constraint posts_pkey",
                    "alter table posts add constraint posts_pkey primary key using index posts_pkey_new",
                    "alter table posts add constraint posts_slug_key unique using index posts_slug_key",
                    "alter table comments add constraint comments_post_slug_fkey foreign key (post_slug) references posts(slug) not valid",
                ]),
                strings(&["alter table comments validate constraint comments_post_slug_fkey"]),
            ]
        );
        // A primary key renamed: the new one is added once the old one is gone.
        let at = existing(&["users"]).with_index("users_pk", "users", 'p');
        let files = lock_safe(&plan(&["alter table users drop constraint users_pk", "alter table users add constraint users_pkey primary key (uid)", "create index users_email on users (email)"]), &at).unwrap();
        assert_eq!(files[2], strings(&["alter table users drop constraint users_pk", "alter table users add constraint users_pkey primary key using index users_pkey"]));
    }

    #[test]
    fn moves_what_names_a_swapped_index_after_the_swap() {
        let at = existing(&["t"]).with_index("t_a", "t", ' ').with_index("t_b", "t", ' ');
        let files = lock_safe(
            &plan(&["drop index t_b", "drop index t_a", "create index t_a on t (a, id)", "alter table t cluster on t_a", "comment on index t_a is 'hot'", "create unique index t_b on t (b) include (a)"]),
            &at,
        )
        .unwrap();
        assert_eq!(
            files,
            vec![
                strings(&["create index concurrently t_a_new on t (a, id)"]),
                strings(&["create unique index concurrently t_b_new on t (b) include (a)"]),
                strings(&["drop index t_b", "drop index t_a", "alter index t_a_new rename to t_a", "alter index t_b_new rename to t_b", "alter table t cluster on t_a", "comment on index t_a is 'hot'"]),
            ]
        );
    }

    #[test]
    fn keeps_a_foreign_key_back_with_its_key() {
        let at = existing(&["t", "u", "tb"]).with_index("t_code_key", "t", 'u');
        let files = lock_safe(
            &plan(&[
                "alter table u drop constraint u_code_fkey",
                "alter table t drop constraint t_code_key",
                "alter table t add constraint t_code_key unique (code) include (region)",
                "alter table tb add constraint tb_name_key unique (name)",
                "alter table u add constraint u_code_fkey foreign key (code) references t(code)",
            ]),
            &at,
        )
        .unwrap();
        assert_eq!(
            files,
            vec![
                strings(&["create unique index concurrently t_code_key_new on t (code) include (region)"]),
                strings(&[
                    "alter table u drop constraint u_code_fkey",
                    "alter table t drop constraint t_code_key",
                    "alter table t add constraint t_code_key unique using index t_code_key_new",
                    "alter table u add constraint u_code_fkey foreign key (code) references t(code) not valid",
                ]),
                strings(&["create unique index concurrently tb_name_key on tb (name)"]),
                strings(&["alter table tb add constraint tb_name_key unique using index tb_name_key"]),
                strings(&["alter table u validate constraint u_code_fkey"]),
            ]
        );
    }

    #[test]
    fn takes_a_temporary_name_nothing_the_plan_makes_holds() {
        let at = existing(&["t"]).with_index("t_a", "t", ' ');
        let files = lock_safe(&plan(&["drop index t_a", "create sequence t_a_new", "create index t_a on t (a, b)"]), &at).unwrap();
        assert_eq!(files[1], strings(&["create index concurrently t_a_new2 on t (a, b)"]));
        let files = lock_safe(&plan(&["drop index t_a", "alter table t add constraint t_a_new unique (b)", "create index t_a on t (a, b)"]), &at).unwrap();
        assert!(files.concat().contains(&"create index concurrently t_a_new2 on t (a, b)".to_string()), "{files:?}");
        // A comment where the name stands.
        let files = lock_safe(&plan(&["drop index t_a", "create index /* hot path */ t_a on t (a, b)"]), &at).unwrap();
        assert_eq!(files[0], strings(&["create index concurrently /* hot path */ t_a_new on t (a, b)"]));
    }

    #[test]
    fn drops_an_exclusion_constraint_where_the_plan_has_it() {
        let at = existing(&["bookings"]).with_index("bookings_excl", "bookings", 'x');
        let files = lock_safe(&plan(&["alter table bookings drop constraint bookings_excl", "alter table bookings add constraint bookings_excl exclude using gist (room with =)", "create index bookings_room on bookings (room)"]), &at).unwrap();
        assert_eq!(files[0], strings(&["alter table bookings drop constraint bookings_excl", "alter table bookings add constraint bookings_excl exclude using gist (room with =)"]));
    }

    #[test]
    fn drops_first_where_a_statement_between_changes_the_table() {
        // A column dropped or retyped may take the old index with it, or build it again.
        let at = existing(&["t"]).with_index("t_a", "t", ' ');
        for between in ["alter table t drop column n", "alter table t alter column n type bigint", "alter table t rename to t2"] {
            let files = lock_safe(&plan(&["drop index t_a", between, "create index t_a on t (m)"]), &at).unwrap();
            assert_eq!(files, vec![strings(&["drop index t_a", between]), strings(&["create index concurrently t_a on t (m)"])], "{between}");
        }
        // As the shape that drops first writes it.
        let files = lock_safe_dropping_first(&plan(&["drop index t_a", "create index t_a on t (m)"]), &at).unwrap();
        assert_eq!(files, vec![strings(&["drop index t_a"]), strings(&["create index concurrently t_a on t (m)"])]);
    }

    #[test]
    fn builds_first_around_a_column_renamed_under_the_index() {
        let at = existing(&["posts"]).with_index("posts_pkey", "posts", 'p');
        let files = lock_safe(
            &plan(&[
                "alter table posts drop constraint posts_pkey",
                "alter table posts rename column pid to post_id",
                "alter table posts rename constraint posts_pid_not_null to posts_post_id_not_null",
                "alter table posts add constraint posts_pkey primary key (post_id)",
            ]),
            &at,
        )
        .unwrap();
        assert_eq!(
            files,
            vec![
                strings(&["alter table posts rename column pid to post_id", "alter table posts rename constraint posts_pid_not_null to posts_post_id_not_null"]),
                strings(&["create unique index concurrently posts_pkey_new on posts (post_id)"]),
                strings(&["alter table posts drop constraint posts_pkey", "alter table posts add constraint posts_pkey primary key using index posts_pkey_new"]),
            ]
        );
        // A constraint renamed to the name the drop takes is no longer the one it dropped.
        let files = lock_safe(&plan(&["alter table posts drop constraint posts_pkey", "alter table posts rename constraint posts_old to posts_pkey", "create index posts_pkey on posts (n)"]), &at).unwrap();
        assert_eq!(files[0], strings(&["alter table posts drop constraint posts_pkey", "alter table posts rename constraint posts_old to posts_pkey"]));
    }

    #[test]
    fn drops_a_foreign_key_on_a_replaced_key_with_it() {
        let at = existing(&["t", "u"]).with_index("t_code_key", "t", 'u');
        let files = lock_safe(
            &plan(&[
                "alter table u drop constraint u_code_fkey",
                "alter table t drop constraint t_code_key",
                "alter table t add constraint t_code_key unique nulls not distinct (code)",
                "alter table u add constraint u_code_fkey foreign key (code) references t(code)",
            ]),
            &at,
        )
        .unwrap();
        assert_eq!(
            files,
            vec![
                strings(&["create unique index concurrently t_code_key_new on t (code) nulls not distinct"]),
                strings(&[
                    "alter table u drop constraint u_code_fkey",
                    "alter table t drop constraint t_code_key",
                    "alter table t add constraint t_code_key unique using index t_code_key_new",
                    "alter table u add constraint u_code_fkey foreign key (code) references t(code) not valid",
                ]),
                strings(&["alter table u validate constraint u_code_fkey"]),
            ]
        );
    }

    #[test]
    fn keeps_what_has_no_lock_safe_shape() {
        // With options the index can't carry into the constraint.
        assert_eq!(lock_safe(&plan(&["alter table users add constraint k unique (email) with (fillfactor = 70)"]), &existing(&["users"])), None);
        assert_eq!(lock_safe(&plan(&["alter table users add constraint k unique (email) using index tablespace fast"]), &existing(&["users"])), None);
        // Already not valid, or already concurrent.
        assert_eq!(lock_safe(&plan(&["alter table users add constraint c check (n > 0) not valid", "create index concurrently i on users (n)"]), &existing(&["users"])), None);
        // Qualified names match the table's qualified name.
        assert!(lock_safe(&plan(&["create index i on billing.invoices (n)"]), &existing(&["billing.invoices"])).is_some());
    }

    #[test]
    fn reads_whether_an_option_is_on() {
        let concurrently = |sql: &str| match parsed(sql) {
            Some(N::ReindexStmt(r)) => r.params.iter().any(|p| matches!(&p.node, Some(N::DefElem(d)) if d.defname == "concurrently" && option_on(d))),
            _ => panic!("{sql}"),
        };
        assert!(concurrently("reindex index concurrently users_n"));
        assert!(concurrently("reindex (concurrently) index users_n"));
        assert!(concurrently("reindex (concurrently true) table users"));
        assert!(!concurrently("reindex (concurrently false) index users_n"));
        assert!(!concurrently("reindex (concurrently off) table users"));
        assert!(!concurrently("reindex (concurrently 0) index users_n"));
        assert!(!concurrently("reindex index users_n"));
    }

    #[test]
    fn not_null_check_names_fit() {
        let long = "c".repeat(70);
        assert!(not_null_check(&long).len() <= 63);
        assert_eq!(not_null_check("email"), "email_not_null");
    }
}

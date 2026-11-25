//! The commands of `reylo migrate` and `reylo db`, and `reylo run`. Each takes the project's settings and a log it
//! writes its lines to, and answers the process's exit code; an `Err` is a refusal to start (a
//! setting, a file or a connection), which the command line prints.

use crate::catalog::{catalog, difference, HISTORY_TABLE};
use crate::db::{shown_url, Breach, Breaks, Db, Ran, Server};
use crate::lock;
#[cfg(feature = "cli")]
use crate::replay::{Shadows, Step};
use crate::sql::{atomicity_problem, changes_data, runs_alone, split_statements, unreplayable};
use crate::{time, Dialect, Error};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;

/// A project's `migrate` settings, resolved.
#[derive(Clone, Debug)]
pub struct Options {
    pub url: String,
    /// A server to make shadow databases on, where the target's refuses `create database`.
    pub shadow_url: Option<String>,
    pub dir: PathBuf,
    pub schema: PathBuf,
    /// The lock's file, `reylo/reylo.lock`.
    pub lock: PathBuf,
    /// The cache of replays and the SQLite lock files, `reylo/.cache/`.
    pub cache: PathBuf,
    /// The project's directory: a relative SQLite path is its.
    pub root: PathBuf,
    /// The project's files as a build holds them, read in place of `dir`, `schema` and `lock`.
    pub embedded: Option<std::sync::Arc<Embedded>>,
}

/// A project's migrations as a build holds them, which `reylo::migrate!()` compiles into a Rust
/// app: each file of the migrations directory with its text, the lock's text and `schema.sql`'s,
/// read in place of the files, so that deploy and status check what the build holds as they check
/// the files.
#[derive(Clone, Debug, Default)]
pub struct Embedded {
    /// Each entry of the migrations directory, by its name, and a migration's text: any other
    /// entry's is empty, since deploy and status name it as a stray.
    pub files: Vec<(String, String)>,
    pub lock: Option<String>,
    pub schema: Option<String>,
}

impl Options {
    /// The migrations and the files beside them that aren't migrations, from the build where it
    /// holds them, else from `dir`.
    fn migrations(&self) -> Result<(Vec<Migration>, Vec<String>), Error> {
        match &self.embedded {
            Some(e) => Ok(migrations_of(&self.dir, e.files.iter().map(|(name, sql)| (name.clone(), Some(sql.clone()))))),
            None => read_migrations(&self.dir),
        }
    }

    /// Whether a SQLite project runs on libSQL's engine: its target is a libSQL server's (Turso's),
    /// or its migrations or schema.sql use SQL of libSQL's own (a vector index, `alter table …
    /// alter column`), which Reylo's SQLite refuses, so that its development file opens there too.
    pub fn libsql_engine(&self) -> bool {
        if crate::libsql::is_url(&self.url) {
            return true;
        }
        let files = self.migrations().map(|(files, _)| files).unwrap_or_default();
        files.iter().any(|m| crate::libsql::uses_own_sql(&m.sql)) || self.schema_text().is_some_and(|s| crate::libsql::uses_own_sql(&s))
    }

    fn lock_state(&self) -> lock::State {
        match &self.embedded {
            Some(e) => e.lock.as_deref().map_or(lock::State::Missing, lock::of_text),
            None => lock::read(&self.lock),
        }
    }

    fn schema_text(&self) -> Option<String> {
        match &self.embedded {
            Some(e) => e.schema.clone(),
            None => std::fs::read_to_string(&self.schema).ok(),
        }
    }

    /// Whether the migrations directory exists: always, for the files a build holds, since
    /// `reylo::migrate!()` refuses to build without one.
    fn dir_exists(&self) -> bool {
        self.embedded.is_some() || self.dir.exists()
    }

    fn schema_exists(&self) -> bool {
        self.embedded.as_ref().map_or_else(|| self.schema.exists(), |e| e.schema.is_some())
    }
}

/// What a host's run of deploy or status gave: the status the command exits with, the lines it
/// printed, its refusals and its result, each as `--json` gives them.
#[derive(Clone, Debug)]
pub struct HostRun {
    pub code: i32,
    pub lines: Vec<String>,
    pub problems: Vec<serde_json::Value>,
    pub result: Option<serde_json::Value>,
}

/// `reylo migrate deploy` on a database the host connects, with the files `opts` holds. The run's
/// lock is released before it answers, since the host's connection outlives the run.
pub async fn deploy_on_host(db: &mut Db, opts: &Options) -> Result<HostRun, Error> {
    let mut lines = Vec::new();
    let (code, held) = HOST_RUN
        .scope(Default::default(), async {
            let code = deploy_on(db, opts, &mut |line: &str| lines.push(line.to_string())).await;
            (code, HOST_RUN.with(std::cell::RefCell::take))
        })
        .await;
    let unlocked = unlock(db).await;
    let code = code?;
    unlocked?;
    Ok(HostRun { code, lines, problems: held.problems.iter().map(Problem::json).collect(), result: held.result })
}

/// `reylo migrate status` on a database the host connects, with the files `opts` holds.
pub async fn status_on_host(db: &mut Db, opts: &Options) -> Result<HostRun, Error> {
    let mut lines = Vec::new();
    let (code, held) = HOST_RUN
        .scope(Default::default(), async {
            let code = status_on(db, opts, &mut |line: &str| lines.push(line.to_string())).await;
            (code, HOST_RUN.with(std::cell::RefCell::take))
        })
        .await;
    Ok(HostRun { code: code?, lines, problems: held.problems.iter().map(Problem::json).collect(), result: held.result })
}

/// The host's runs are `Send`, so an app can spawn one, or await it in a handler.
#[allow(dead_code)]
fn host_runs_are_send(db: &'static mut Db, other: &'static mut Db, opts: &'static Options) {
    fn send<T: Send>(_: T) {}
    send(deploy_on_host(db, opts));
    send(status_on_host(other, opts));
}

/// What one host's run gives beside its lines, kept with the run's own task, so that runs in one
/// process at once (an app's tests, each on a database of its own) never read each other's.
#[derive(Default)]
struct HostState {
    problems: Vec<Problem>,
    result: Option<serde_json::Value>,
}

tokio::task_local! {
    static HOST_RUN: std::cell::RefCell<HostState>;
}

/// Releases the lock [`lock`] took on a connection that stays open after the run.
async fn unlock(db: &mut Db) -> Result<(), Error> {
    match db.dialect() {
        Dialect::Postgres => db.exec("select pg_advisory_unlock(hashtextextended('reylo_migrate', 0))").await,
        Dialect::Mysql => db.exec("do release_lock(concat('reylo_migrate_', left(sha2(coalesce(database(), ''), 256), 32)))").await,
        // The file's lock is released with the file, which the run dropped.
        Dialect::Sqlite => Ok(()),
    }
}

pub type Log<'a> = &'a mut (dyn FnMut(&str) + Send);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Migration {
    pub name: String,
    pub time: String,
    pub path: PathBuf,
    pub sql: String,
}

#[derive(Clone, Debug)]
struct HistoryRow {
    name: String,
    sql: String,
    applied: bool,
    /// The live schema's fingerprint right after the run that applied it ([`fingerprint`]), on the
    /// last row of each run.
    fingerprint: Option<String>,
}

const INIT: &str = "init.sql";
static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{14})_([a-z0-9_]+)\.sql$").expect("a valid pattern"));

static COLOR: AtomicBool = AtomicBool::new(false);

/// Whether lines carry ANSI colors: the command line turns it on for a terminal.
/// Whether the command runs with `--json`: each refusal it prints is kept for the JSON it ends with.
static JSON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static REPORTED: std::sync::Mutex<Vec<Problem>> = std::sync::Mutex::new(Vec::new());
/// What a command gives `--json` beside its refusals: `status`'s rows, for one.
static RESULT: std::sync::Mutex<Option<serde_json::Value>> = std::sync::Mutex::new(None);

pub fn set_json(on: bool) {
    JSON.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// The refusals printed since the last call, as `--json` gives them.
pub fn reported() -> Vec<serde_json::Value> {
    std::mem::take(&mut *REPORTED.lock().unwrap_or_else(|e| e.into_inner())).iter().map(Problem::json).collect()
}

/// What the command gave `--json` beside its refusals, if anything.
pub fn result() -> Option<serde_json::Value> {
    RESULT.lock().unwrap_or_else(|e| e.into_inner()).take()
}

fn set_result(value: serde_json::Value) {
    if HOST_RUN.try_with(|run| run.borrow_mut().result = Some(value.clone())).is_ok() {
        return;
    }
    if JSON.load(std::sync::atomic::Ordering::Relaxed) {
        *RESULT.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
    }
}

pub fn set_color(on: bool) {
    COLOR.store(on, Ordering::Relaxed);
}

/// Whether lines carry ANSI colors.
pub fn color() -> bool {
    COLOR.load(Ordering::Relaxed)
}

/// Text in a terminal style, or as it is where colour is off. What each mark and colour means is in
/// AGENTS.md, under Terminal output.
pub fn paint(style: &str, text: &str) -> String {
    if !COLOR.load(Ordering::Relaxed) {
        return text.to_string();
    }
    let code = match style {
        "red" => "31",
        "green" => "32",
        "yellow" => "33",
        "dim" => "2",
        "cyan" => "36",
        "bold red" => "1;31",
        "bold" => "1",
        // A diff's words removed and added within a line: light text on a dark red or green.
        "removed" => "38;5;217;48;5;52",
        "added" => "38;5;157;48;5;22",
        _ => "0",
    };
    format!("\x1b[{code}m{text}\x1b[0m")
}

/// A note: something to know, and maybe act on, that is neither done (✓) nor a risk (!).
fn note(text: impl AsRef<str>) -> String {
    paint("dim", &format!("» {}", text.as_ref()))
}

/// What to do, as a refusal's fixes are shown: each lead, indented by `indent`, names the act, and
/// the commands that do it follow on lines of their own, two spaces further in, cyan.
fn steps(indent: usize, steps: &[(&str, &[&str])]) -> String {
    let pad = " ".repeat(indent);
    let mut out: Vec<String> = Vec::new();
    for (lead, commands) in steps {
        out.push(paint("dim", &format!("{pad}{lead}:")));
        out.extend(commands.iter().map(|c| format!("{pad}  {}", paint("cyan", c))));
    }
    out.join("\n")
}

/// A line with what to do about it under it.
fn then(line: impl AsRef<str>, what: &[(&str, &[&str])]) -> String {
    format!("{}\n{}", line.as_ref(), steps(2, what))
}

fn normalized(sql: &str) -> String {
    sql.replace("\r\n", "\n").replace('\r', "\n").trim_end().to_string()
}

/// A path as the person running the command reads it: relative to where they are.
fn shown(path: &Path) -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    match path.strip_prefix(&cwd) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
        // A path in the project reads as `reylo/schema.sql` on every platform, as git and the docs write it.
        Ok(rel) if cfg!(windows) => rel.to_string_lossy().replace('\\', "/"),
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

/// The migration files of `dir`, in time order (`init.sql` first), and the files that are not migrations.
pub fn read_migrations(dir: &Path) -> Result<(Vec<Migration>, Vec<String>), Error> {
    if !dir.exists() {
        return Ok((vec![], vec![]));
    }
    let mut named = Vec::new();
    for entry in std::fs::read_dir(dir)?.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        let migration = name == INIT || NAME.is_match(&name);
        named.push((name.clone(), if migration { Some(std::fs::read_to_string(dir.join(&name))?) } else { None }));
    }
    Ok(migrations_of(dir, named.into_iter()))
}

/// The migrations of `dir` from its files' names and texts (a text read for each file that is a
/// migration), and the names of those that aren't.
fn migrations_of(dir: &Path, named: impl Iterator<Item = (String, Option<String>)>) -> (Vec<Migration>, Vec<String>) {
    let mut named: Vec<(String, Option<String>)> = named.collect();
    named.sort_by(|a, b| a.0.cmp(&b.0));
    let mut files = Vec::new();
    let mut strays = Vec::new();
    for (name, sql) in named {
        let path = dir.join(&name);
        if name == INIT {
            files.push(Migration { sql: sql.unwrap_or_default(), name, time: "00000000000000".into(), path });
        } else if let Some(m) = NAME.captures(&name) {
            let time = m[1].to_string();
            files.push(Migration { sql: sql.unwrap_or_default(), name, time, path });
        } else if !name.starts_with('.') {
            strays.push(name);
        }
    }
    files.sort_by(|a, b| a.time.cmp(&b.time).then(a.name.cmp(&b.name)));
    (files, strays)
}

// ---------------------------------------------------------------------------------------------
// The history table.

fn history_ddl(d: Dialect) -> String {
    match d {
        Dialect::Postgres => format!("create table if not exists {HISTORY_TABLE} (name text primary key, sql text not null, applied_at timestamptz, fingerprint text, catalog text)"),
        Dialect::Mysql => format!("create table if not exists {HISTORY_TABLE} (name varchar(255) primary key, `sql` longtext not null, applied_at datetime(6) null, fingerprint text null, catalog longtext null) engine=InnoDB"),
        Dialect::Sqlite => format!("create table if not exists {HISTORY_TABLE} (name text primary key, sql text not null, applied_at text, fingerprint text, catalog text)"),
    }
}

/// The time a row is applied at: now, and after every row already there, so that rows written in
/// one run read back in the order they were written. On SQLite, whose clock has milliseconds, a tie
/// is broken by the row's rowid instead ([`read_history`]).
fn now_sql(d: Dialect) -> String {
    match d {
        Dialect::Postgres => format!("greatest(clock_timestamp(), (select max(applied_at) + interval '1 microsecond' from {HISTORY_TABLE}))"),
        Dialect::Mysql => format!("greatest(now(6), coalesce((select max(h.applied_at) + interval 1 microsecond from (select applied_at from {HISTORY_TABLE}) h), now(6)))"),
        Dialect::Sqlite => "strftime('%Y-%m-%d %H:%M:%f', 'now')".into(),
    }
}

fn sql_col(d: Dialect) -> &'static str {
    if d == Dialect::Mysql {
        "`sql`"
    } else {
        "sql"
    }
}

async fn has_history(db: &mut Db) -> Result<bool, Error> {
    let sql = match db.dialect() {
        Dialect::Postgres => format!("select to_regclass('{HISTORY_TABLE}') is not null as ok"),
        Dialect::Mysql => format!("select count(*) > 0 as ok from information_schema.tables where table_schema = database() and table_name = '{HISTORY_TABLE}'"),
        Dialect::Sqlite => format!("select count(*) > 0 as ok from sqlite_schema where name = '{HISTORY_TABLE}'"),
    };
    Ok(db.rows(&sql).await?.first().is_some_and(|r| r.flag("ok")))
}

async fn read_history(db: &mut Db) -> Result<Vec<HistoryRow>, Error> {
    if !has_history(db).await? {
        return Ok(vec![]);
    }
    let d = db.dialect();
    // Rows are written each after the last (`now_sql`); on SQLite, whose clock has milliseconds, rows
    // written in one run tie, and are read in the order they were written, by rowid.
    let applied_at = if d == Dialect::Postgres { "applied_at::text" } else { "applied_at" };
    let tie = if d == Dialect::Sqlite { "rowid, " } else { "" };
    // A table an earlier build made has no fingerprint column until the next run adds it.
    let fingerprint = if has_fingerprint(db).await? { "fingerprint" } else { "null as fingerprint" };
    let rows = db
        .rows(&format!(
            "select name, {} as body, {applied_at} as applied_at, {fingerprint} from {HISTORY_TABLE} order by applied_at is null, applied_at, {tie}case when name = '{INIT}' then '' else name end",
            sql_col(d)
        ))
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| HistoryRow { name: r.text("name").into(), sql: r.text("body").into(), applied: r.get("applied_at").is_some(), fingerprint: r.get("fingerprint").map(str::to_string) })
        .collect())
}

async fn has_fingerprint(db: &mut Db) -> Result<bool, Error> {
    has_column(db, "fingerprint").await
}

/// Whether the history table has `column`, which a table an earlier build made may lack.
async fn has_column(db: &mut Db, column: &str) -> Result<bool, Error> {
    let sql = match db.dialect() {
        Dialect::Postgres => format!("select count(*) > 0 as ok from pg_attribute where attrelid = to_regclass('{HISTORY_TABLE}') and attname = '{column}' and not attisdropped"),
        Dialect::Mysql => format!("select count(*) > 0 as ok from information_schema.columns where table_schema = database() and table_name = '{HISTORY_TABLE}' and column_name = '{column}'"),
        Dialect::Sqlite => format!("select count(*) > 0 as ok from pragma_table_info('{HISTORY_TABLE}') where name = '{column}'"),
    };
    Ok(db.rows(&sql).await?.first().is_some_and(|r| r.flag("ok")))
}

/// Makes the history table, or adds what an earlier build's table lacks. On Postgres it is made
/// with no privilege but its owner's: on a database whose default privileges grant on every new
/// table (to an app's role, or to `anon` and `authenticated` on Supabase), a table made after them
/// would take those grants, and a role the app connects as could rewrite which migrations ran. A
/// grant made on it later, by hand, is kept.
async fn ensure_history(db: &mut Db) -> Result<(), Error> {
    let d = db.dialect();
    if d == Dialect::Postgres && !has_history(db).await? {
        db.exec("begin").await?;
        let made = async {
            db.exec(&history_ddl(d)).await?;
            let granted = db
                .rows(&format!(
                    "select distinct case when a.grantee = 0 then 'public' else quote_ident(r.rolname) end as grantee from pg_class c cross join aclexplode(c.relacl) a left join pg_roles r on r.oid = a.grantee where c.oid = to_regclass('{HISTORY_TABLE}') and a.grantee <> c.relowner"
                ))
                .await?;
            for r in granted {
                db.exec(&format!("revoke all on table {HISTORY_TABLE} from {}", r.text("grantee"))).await?;
            }
            db.exec("commit").await
        }
        .await;
        if made.is_err() {
            let _ = db.exec("rollback").await;
        }
        made?;
    }
    db.exec(&history_ddl(d)).await?;
    if !has_fingerprint(db).await? {
        db.exec(&format!("alter table {HISTORY_TABLE} add column fingerprint text")).await?;
    }
    if !has_column(db, "catalog").await? {
        db.exec(&format!("alter table {HISTORY_TABLE} add column catalog {}", if d == Dialect::Mysql { "longtext null" } else { "text" })).await?;
    }
    Ok(())
}

/// The lines a fingerprint is the digest of: what deploy shows of a database changed since. On
/// MySQL, grants aside: which of them an account sees turns on the account (only its own, where
/// it may not read the privilege tables), and a fingerprint is the database's, whoever reads it.
fn fingerprinted(lines: &[String], d: Dialect) -> Vec<String> {
    let lines = crate::catalog::without_children(&crate::catalog::without_comments(lines));
    if d != Dialect::Mysql {
        return lines;
    }
    lines.into_iter().filter(|l| !l.starts_with("grant ") && l != crate::catalog::GRANTS_UNREAD).collect()
}

/// Keeps the lines `name`'s fingerprint is of, on its row alone, so that a refusal can show what
/// changed since with no shadow. Nothing where the table has no column for them.
async fn store_catalog(db: &mut Db, name: &str, lines: &[String]) -> Result<(), Error> {
    if !has_column(db, "catalog").await? {
        return Ok(());
    }
    // As a JSON array: a line may hold newlines of its own (a function's definition).
    let text = serde_json::to_string(&fingerprinted(lines, db.dialect())).unwrap_or_default();
    db.exec_with(&format!("update {HISTORY_TABLE} set catalog = null where name <> {} and catalog is not null", db.param(1)), &[&name]).await?;
    db.exec_with(&format!("update {HISTORY_TABLE} set catalog = {} where name = {}", db.param(1), db.param(2)), &[&text, &name]).await
}

/// The lines stored with `name`'s fingerprint, where they were.
async fn stored_catalog(db: &mut Db, name: &str) -> Result<Option<Vec<String>>, Error> {
    if !has_column(db, "catalog").await? {
        return Ok(None);
    }
    let rows = db.rows(&format!("select catalog from {HISTORY_TABLE} where name = {}", crate::db::literal(name))).await?;
    // A JSON array, or from a run before that, the lines joined by newlines.
    Ok(rows.first().and_then(|r| r.get("catalog")).map(|text| match serde_json::from_str::<Vec<String>>(text) {
        Ok(lines) => lines,
        Err(_) if text.is_empty() => vec![],
        Err(_) => text.split('\n').map(str::to_string).collect(),
    }))
}

/// The live schema's fingerprint, stored on `name`'s row with the lines it is of.
async fn store_fingerprint(db: &mut Db, name: &str) -> Result<(), Error> {
    let lines = catalog(db).await?;
    let value = fingerprint_of(db, &lines).await?;
    set_fingerprint(db, name, &value).await?;
    store_catalog(db, name, &lines).await
}

/// The live schema's fingerprint, from catalog lines already read from `db`: a digest of the catalog
/// as the comparator reads it, with the server's version and the catalog reader's, which both
/// change what the catalog reads as. A database whose fingerprint equals the one its last run stored
/// has not been changed since, its comments aside.
async fn fingerprint_of(db: &mut Db, lines: &[String]) -> Result<String, Error> {
    // The server's major version: a minor release fixes bugs and leaves the catalog's text alone, so
    // that an upgrade a managed host makes on its own leaves the fingerprint comparable.
    let version = match db.dialect() {
        Dialect::Postgres => "select (current_setting('server_version_num')::int / 10000)::text as v",
        Dialect::Mysql => "select substring_index(version(), '.', 2) as v",
        Dialect::Sqlite => "select sqlite_version() as v",
    };
    let version = db.rows(version).await?.first().map(|r| r.text("v").to_string()).unwrap_or_default();
    let mut digest = crate::cache::Sha256::new();
    // A child table may have been made at run time, as data: only a replay can tell.
    for line in fingerprinted(lines, db.dialect()) {
        digest.part(line.as_bytes());
    }
    Ok(format!("{}:{version}:{}", crate::cache::reader_id(), crate::cache::hex(&digest.finish())))
}

async fn set_fingerprint(db: &mut Db, name: &str, value: &str) -> Result<(), Error> {
    db.exec_with(&format!("update {HISTORY_TABLE} set fingerprint = {} where name = {}", db.param(1), db.param(2)), &[&value, &name]).await
}

/// Waits until no other run of `reylo migrate` works on this database, so that two that start at
/// once (two instances of an app, each migrating as it starts) neither decide alike nor run alike:
/// a session lock on Postgres and MySQL, held until the connection closes. SQLite's own locks would
/// stop the app's queries for the length of the check, so a SQLite database is locked through a
/// file named after its path in the user's temporary directory, which every checkout and worktree
/// of theirs that opens the database shares, held while the answer lives. A libSQL server (Turso's)
/// has no lock a session holds either: its database is locked the same way, through a file named
/// after its URL, which holds runs on this machine apart. Two runs on different machines at once are
/// held apart by the server's own write lock and the history, whose row each migration writes in
/// its own transaction: one of the two fails, changing nothing.
async fn lock(db: &mut Db, opts: &Options) -> Result<Option<std::fs::File>, Error> {
    match db.dialect() {
        // Asked for in short statements, with the waits between them outside any: a session blocked
        // in `pg_advisory_lock` holds its statement's snapshot all the while, and the run holding the
        // lock, building an index concurrently, waits for every snapshot older than its own to end,
        // which is a deadlock that Postgres ends by failing one of the two. The session's
        // `lock_timeout` and `statement_timeout` still bound the whole wait, as they bound a blocking
        // one, and a pause stays well inside `idle_session_timeout`, which ends a session idle longer.
        Dialect::Postgres => {
            let settings = db.rows("select name, setting, current_setting(name) as shown from pg_settings where name in ('lock_timeout', 'statement_timeout', 'idle_session_timeout')").await?;
            let ms = |name: &str| settings.iter().find(|r| r.text("name") == name).and_then(|r| r.text("setting").parse::<u64>().ok()).filter(|&v| v > 0);
            let limit = ["lock_timeout", "statement_timeout"].into_iter().filter_map(|name| ms(name).map(|v| (v, name))).min();
            let longest = std::time::Duration::from_millis(ms("idle_session_timeout").map_or(1000, |v| (v / 2).clamp(1, 1000)));
            let started = std::time::Instant::now();
            let mut pause = std::time::Duration::from_millis(50).min(longest);
            loop {
                if db.rows("select pg_try_advisory_lock(hashtextextended('reylo_migrate', 0)) as ok").await?.first().is_some_and(|r| r.flag("ok")) {
                    return Ok(None);
                }
                let mut wait = pause;
                if let Some((limit, name)) = limit {
                    let left = std::time::Duration::from_millis(limit).saturating_sub(started.elapsed());
                    if left.is_zero() {
                        let shown = settings.iter().find(|r| r.text("name") == name).map(|r| r.text("shown").to_string()).unwrap_or_default();
                        return Err(Error(format!("another run of reylo migrate held this database past this session's {name} of {shown}; try again once it finishes")));
                    }
                    wait = wait.min(left);
                }
                tokio::time::sleep(wait).await;
                pause = (pause * 2).min(longest);
            }
        }
        // A lock of this database's alone (its name is at most 64 characters), which answers 0
        // once the wait runs out. Named through sha2: MySQL 9.7 has md5 and sha1 only as a component.
        // The answer is read by position: Vitess (PlanetScale's MySQL) runs a lock function apart
        // and answers it without the column's alias.
        Dialect::Mysql => {
            let rows = db.rows("select get_lock(concat('reylo_migrate_', left(sha2(coalesce(database(), ''), 256), 32)), 600)").await?;
            if rows.first().and_then(|r| r.first()) != Some("1") {
                return Err(Error("another run of reylo migrate held this database for ten minutes; try again once it finishes".into()));
            }
            Ok(None)
        }
        Dialect::Sqlite => {
            let canonical = if crate::libsql::is_url(&opts.url) {
                crate::libsql::identity(&opts.url).to_ascii_lowercase()
            } else {
                let path = crate::db::sqlite_path(&opts.url, &opts.root);
                if path == ":memory:" {
                    return Ok(None);
                }
                std::fs::canonicalize(&path).map(|p| p.to_string_lossy().into_owned()).unwrap_or(path)
            };
            let mut digest = crate::cache::Sha256::new();
            digest.part(canonical.as_bytes());
            let dir = std::env::temp_dir().join(format!("reylo-migrate-locks{}", user_suffix()));
            std::fs::create_dir_all(&dir).map_err(|e| Error(format!("cannot make {}: {e}", dir.display())))?;
            let path = dir.join(crate::cache::hex(&digest.finish()[..8]));
            let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&path).map_err(|e| Error(format!("cannot open {}: {e}", path.display())))?;
            // Waited for on the runtime's blocking pool, not the run's own thread: an app's runs share
            // it with others, as two deploys on one runtime do, and the one holding the lock must go
            // on to release it.
            let held = tokio::task::spawn_blocking(move || file.lock().map(|()| file)).await.map_err(|e| Error(format!("waiting for the run's lock: {e}")))?;
            Ok(Some(held?))
        }
    }
}

/// What names a user's own directory of run locks in a temporary directory every user shares, as
/// Linux's `/tmp` is: their id. macOS and Windows give each user a temporary directory of their own.
fn user_suffix() -> String {
    #[cfg(unix)]
    {
        // SAFETY: getuid has no preconditions and cannot fail.
        format!("-{}", unsafe { libc::getuid() })
    }
    #[cfg(not(unix))]
    {
        String::new()
    }
}

/// What a row's fingerprint says while its run hasn't read it yet: written in the migration's own
/// transaction, so that a run cut off before it reads one leaves a row the next deploy refuses
/// rather than one with nothing to compare. The read follows the commit, so the migration's locks
/// are not held for it, and comes once a run of migrations ends ([`Runs`]). A row the run passed
/// keeps its mark: it says nothing of the schema now, should a rewind make the row the newest again.
const PENDING: &str = "pending";

/// How a run stamps the schema in a row's pending mark: [`PENDING`], and after a colon a digest of
/// what a query reads, taken in the migration's own transaction, so that a run cut off before it read
/// the fingerprint can go on where the stamp is still the schema's ([`unchanged_since_cut_off`]). The
/// query reads no more than a few thousand catalog rows, and needs nothing a server may be built
/// without; MySQL commits each statement, so the schema is read before each migration there ([`Runs`]),
/// and its mark is bare.
struct SchemaStamp(Option<String>);

impl SchemaStamp {
    /// The query, for `db`. On Postgres, the count and the sum of the row versions in each catalog of
    /// the database's own that the session may read, but for statistics, comments and large objects,
    /// which change with no change to the schema, and the rows of temporary tables' schemas, which stay
    /// once a session has made one; of the tables, their columns that a change to the schema sets,
    /// since a truncate or a refresh gives a table new storage in its row; and the names of the roles
    /// the database's objects name. Any statement that changes the schema writes a row of one of them.
    /// On SQLite, the schema's statements, which a vacuum leaves as they were.
    async fn of(db: &mut Db) -> Result<SchemaStamp, Error> {
        match db.dialect() {
            Dialect::Postgres => {
                let temp = "nspname ~ ''^pg_(toast_)?temp_''";
                let sql = format!(
                    "select string_agg(format('select %L || '':'' || count(*) || '':'' || coalesce(sum(%s), 0) as s from %s%s', c.relname, \
                     case when c.relname = 'pg_class' then 'hashtext(row(oid, relname, relnamespace, reltype, reloftype, relowner, relam, reltablespace, relkind, relpersistence, relispartition, relrowsecurity, relforcerowsecurity, relreplident, relacl, reloptions, relnatts, relchecks, relhasrules, relhastriggers)::text)::bigint' else 'xmin::text::bigint' end, \
                     c.oid::regclass, \
                     case c.relname when 'pg_namespace' then ' where not ({temp})' when 'pg_class' then ' where relnamespace not in (select oid from pg_namespace where {temp})' else '' end), \
                     ' union all ' order by c.relname) as q \
                     from pg_class c where c.relnamespace = 'pg_catalog'::regnamespace and c.relkind = 'r' and not c.relisshared \
                     and c.relname not in ('pg_statistic', 'pg_statistic_ext_data', 'pg_description', 'pg_largeobject', 'pg_largeobject_metadata') and has_table_privilege(c.oid, 'select')"
                );
                let union = db.rows(&sql).await?.first().map(|r| r.text("q").to_string()).unwrap_or_default();
                let roles = "select 'role:' || r.oid || ':' || r.rolname as s from pg_roles r where r.oid in (select d.refobjid from pg_shdepend d join pg_database b on b.oid = d.dbid where b.datname = current_database() union select datdba from pg_database where datname = current_database())";
                let stamp = SchemaStamp(Some(if union.is_empty() { roles.to_string() } else { format!("{union} union all {roles}") }));
                // Read once here, outside any migration's transaction: a catalog the session can't
                // read (a server may revoke `pg_shdepend` from everyone) leaves a bare mark rather than
                // failing the migration it is read in.
                Ok(if stamp.mark(db).await.is_ok() { stamp } else { SchemaStamp(None) })
            }
            Dialect::Sqlite => Ok(SchemaStamp(Some("select type || char(1) || name || char(1) || tbl_name || char(1) || coalesce(sql, '') as s from sqlite_schema".into()))),
            Dialect::Mysql => Ok(SchemaStamp(None)),
        }
    }

    /// The pending mark of the schema now.
    async fn mark(&self, db: &mut Db) -> Result<String, Error> {
        let Some(sql) = &self.0 else { return Ok(PENDING.to_string()) };
        let mut rows: Vec<String> = db.rows(sql).await?.iter().map(|r| r.text("s").to_string()).collect();
        rows.sort();
        let mut digest = crate::cache::Sha256::new();
        for row in &rows {
            digest.part(row.as_bytes());
        }
        Ok(format!("{PENDING}:{}", crate::cache::hex(&digest.finish())))
    }
}

/// The pending mark a run would write on `db` now: what a run cut off leaves, for the scenarios to
/// stand for one.
#[doc(hidden)]
pub async fn pending_mark(db: &mut Db) -> Result<String, Error> {
    SchemaStamp::of(db).await?.mark(db).await
}

/// Whether a fingerprint is a pending mark ([`PENDING`]).
fn is_pending(fingerprint: &str) -> bool {
    fingerprint.split_once(':').map_or(fingerprint, |(mark, _)| mark) == PENDING
}

/// Whether the schema is the one the migration of the pending mark `stored` left: its stamp is the
/// schema's now, so nothing has changed it since that migration committed, and a fingerprint read now
/// is the one its run would have stored.
async fn unchanged_since_cut_off(db: &mut Db, stored: &str) -> bool {
    if !stored.contains(':') {
        return false;
    }
    // A stamp that can't be read shows nothing.
    match SchemaStamp::of(db).await {
        Ok(stamp) => stamp.mark(db).await.is_ok_and(|now| now == stored),
        Err(_) => false,
    }
}

/// The migration's row: applied now, or marked started; with the pending mark of the schema it left,
/// where its run reads the fingerprint.
async fn insert_row(db: &mut Db, m: &Migration, applied: bool, mark: Option<&str>) -> Result<(), Error> {
    let d = db.dialect();
    let at = if applied { now_sql(d) } else { "null".to_string() };
    let stamp = if mark.is_some() { db.param(3) } else { "null".to_string() };
    let sql = format!("insert into {HISTORY_TABLE} (name, {}, applied_at, fingerprint) values ({}, {}, {at}, {stamp})", sql_col(d), db.param(1), db.param(2));
    match mark {
        Some(mark) => db.exec_with(&sql, &[&m.name, &m.sql, &mark]).await,
        None => db.exec_with(&sql, &[&m.name, &m.sql]).await,
    }
}

// ---------------------------------------------------------------------------------------------
// Running a migration: in one transaction with its history row, or, for a file that cannot run in
// one, alone, with a row marked started before it and finished after it. Either way the row is
// applied marked pending, and gets the fingerprint of the schema the run left once the run ends
// ([`Runs`]). No run cut off between the two leaves an applied row with nothing to compare.

/// Runs a migration and records nothing of it, as a shadow's replay and the check of its locks do.
pub(crate) async fn run(db: &mut Db, m: &Migration) -> Result<(), Error> {
    run_undone(db, m, None).await.map_err(|f| f.error)
}

/// How a migration failed: its error, whether nothing of it is known to be in place, and the row
/// the table already held that broke a constraint, where that is what failed it.
struct Failure {
    error: Error,
    undone: bool,
    breach: Option<Breach>,
}

/// Migrations run one after another, each recorded, with the fingerprint of the schema they leave
/// read once, onto the newest row, rather than after each: only the newest row's is ever compared,
/// nothing but the run changes the schema while it holds the lock, and each read is of the whole
/// catalog, so a read after each made a long history run from empty, as a new database's does, cost
/// its length times its schema. A run cut off before the read leaves its newest row pending, whose
/// mark's stamp lets the next run go on where nothing has changed the schema since ([`SchemaStamp`]).
/// A migration that may leave part of itself when it fails has the fingerprint read before it, so
/// that the next run sees what it left as a change; after one that fails and leaves nothing, the
/// schema is the one the migrations before it left, and is read then.
struct Runs {
    /// The newest row the run wrote, while it is marked pending.
    unread: Option<String>,
    /// How its pending marks stamp the schema, read once the run first needs it.
    stamp: Option<SchemaStamp>,
}

impl Runs {
    fn new() -> Runs {
        Runs { unread: None, stamp: None }
    }

    /// Runs `m` and records it; a failure says whether nothing of it is in place, as [`run_undone`]'s
    /// does.
    async fn run(&mut self, db: &mut Db, m: &Migration) -> Result<(), Failure> {
        if may_leave_part(&m.sql, db.dialect()) {
            self.finish(db).await;
        }
        if self.stamp.is_none() {
            self.stamp = Some(SchemaStamp::of(db).await.unwrap_or(SchemaStamp(None)));
        }
        let result = run_undone(db, m, self.stamp.as_ref()).await;
        match result {
            Ok(()) => self.unread = Some(m.name.clone()),
            Err(Failure { undone: true, .. }) => self.finish(db).await,
            // What it may have left must not be read as the schema the rows before it left: they stay
            // pending, as a run cut off leaves them.
            Err(_) => self.unread = None,
        }
        result
    }

    /// Stores the fingerprint of the schema the run left on its newest row. A fingerprint that can't
    /// be read leaves the row pending, which the next deploy refuses, rather than reading as a
    /// migration that failed.
    async fn finish(&mut self, db: &mut Db) {
        if let Some(name) = self.unread.take() {
            let _ = store_fingerprint(db, &name).await;
        }
    }
}

/// Runs a migration, with its row where `record` says; a failure says whether nothing of it is known to be
/// in place: never on MySQL, which commits each DDL statement, but for one statement whose rows broke
/// a constraint, which the server undoes whole; nor where a statement run outside a transaction may
/// have left part of itself, or finished before its row could be written.
async fn run_undone(db: &mut Db, m: &Migration, record: Option<&SchemaStamp>) -> Result<(), Failure> {
    let undone = db.dialect() != Dialect::Mysql;
    let mut tries = 1;
    loop {
        match run_once(db, m, record).await {
            Ok(()) => return Ok(()),
            Err(Ran::Failed(error)) => return Err(Failure { error, undone, breach: None }),
            Err(Ran::Broke(error, breach)) => return Err(Failure { error, undone: undone || split_statements(&m.sql, Dialect::Mysql).len() == 1, breach: Some(breach) }),
            Err(Ran::Partly(error)) => return Err(Failure { error, undone: false, breach: None }),
            // A statement that gave up waiting for a lock changed nothing: the file runs again, after
            // a pause that grows, until the last try.
            Err(Ran::Waited(_)) if tries < LOCK_TRIES => {
                tokio::time::sleep(std::time::Duration::from_secs(LOCK_PAUSES[(tries - 1).min(LOCK_PAUSES.len() - 1)])).await;
                tries += 1;
            }
            Err(Ran::Waited(e)) => return Err(Failure { error: Error(format!("{}: {e}, {LOCK_TRIES} times over {} seconds{}", m.name, LOCK_PAUSES.iter().take(LOCK_TRIES - 1).sum::<u64>(), holders(db).await)), undone, breach: None }),
        }
    }
}

/// How long a statement of a migration waits for a lock before it gives up: while it waits, every
/// query after it on that table waits behind it, so a short wait, tried again, blocks the app for
/// moments rather than for as long as the longest transaction holding the table runs. A migration
/// sets its own (`set local lock_timeout`) to wait longer.
const LOCK_TIMEOUT_MS: u64 = 500;

/// How many times a migration runs whose statement gave up waiting for a lock, and the pause before
/// each try after the first, in seconds.
const LOCK_TRIES: usize = 10;
const LOCK_PAUSES: [u64; 9] = [1, 2, 4, 8, 10, 10, 10, 10, 10];

/// The transactions open on the database, oldest first, which a migration's lock likely waited for.
async fn holders(db: &mut Db) -> String {
    let sql = match db.dialect() {
        Dialect::Postgres => {
            "select pid::text as id, extract(epoch from now() - xact_start)::bigint::text as secs, state, left(regexp_replace(coalesce(query, ''), '\\s+', ' ', 'g'), 80) as q
            from pg_stat_activity where datname = current_database() and pid <> pg_backend_pid() and xact_start is not null order by xact_start limit 3"
        }
        Dialect::Mysql => "select trx_mysql_thread_id as id, timestampdiff(second, trx_started, now()) as secs, trx_state as state, left(coalesce(trx_query, ''), 80) as q from information_schema.innodb_trx order by trx_started limit 3",
        Dialect::Sqlite => return String::new(),
    };
    let Ok(rows) = db.rows(sql).await else { return String::new() };
    if rows.is_empty() {
        return String::new();
    }
    let open: Vec<String> = rows.iter().map(|r| format!("{} open {} s ({}){}", r.text("id"), r.text("secs"), r.text("state"), if r.text("q").is_empty() { String::new() } else { format!(": {}", r.text("q")) })).collect();
    format!(". The oldest open transactions, by session: {}", open.join("; "))
}

/// Sets how long the session's statements wait for a lock: a transaction's own on Postgres
/// (`set local`, which its commit or rollback ends), the session's on MySQL until `reset_lock_wait`.
async fn limit_lock_wait(db: &mut Db) -> Result<(), Error> {
    match db {
        #[cfg(feature = "cli")]
        Db::Postgres(_) => db.exec(&format!("set local lock_timeout = '{LOCK_TIMEOUT_MS}ms'")).await,
        // MySQL counts whole seconds, at least one.
        #[cfg(feature = "cli")]
        Db::Mysql(_) => db.exec("set session lock_wait_timeout = 1").await,
        _ => Ok(()),
    }
}

async fn reset_lock_wait(db: &mut Db) -> Result<(), Error> {
    match db {
        #[cfg(feature = "cli")]
        // The server's own value, which `= default` would set: Vitess (PlanetScale's MySQL) refuses
        // that ("unsupported: DEFAULT for @@lock_wait_timeout"), and takes a number.
        Db::Mysql(_) => {
            let rows = db.rows("select @@global.lock_wait_timeout").await?;
            let seconds = rows.first().and_then(|r| r.first()).and_then(|v| v.parse::<u64>().ok()).ok_or_else(|| Error("cannot read the server's lock_wait_timeout".into()))?;
            db.exec(&format!("set session lock_wait_timeout = {seconds}")).await
        }
        _ => Ok(()),
    }
}

async fn run_once(db: &mut Db, m: &Migration, record: Option<&SchemaStamp>) -> Result<(), Ran> {
    let d = db.dialect();
    if runs_alone(&m.sql, d) {
        if record.is_some() {
            insert_row(db, m, false, None).await?;
        }
        // A failed `create index concurrently` (or `reindex concurrently`) leaves an invalid index
        // behind; the ones this statement left are dropped, so the file can run again as written.
        let invalid = async |db: &mut Db| -> Result<Vec<String>, Error> {
            if d != Dialect::Postgres {
                return Ok(vec![]);
            }
            Ok(db.rows("select indexrelid::regclass::text as i from pg_index where not indisvalid").await?.iter().map(|r| r.text("i").to_string()).collect())
        };
        let before = invalid(db).await?;
        // On Postgres such a statement is built concurrently, whose waits block no one, so it waits
        // as long as it must; MySQL's DDL waits for its lock as the app's queries queue behind it.
        if d == Dialect::Mysql {
            limit_lock_wait(db).await?;
        }
        let result = db.script_or_wait(&m.sql).await;
        let reset = if d == Dialect::Mysql { reset_lock_wait(db).await } else { Ok(()) };
        if let Err(mut e) = result {
            // Duplicates that failed a unique index's build are listed by its definition, read before
            // the invalid index it left is dropped.
            if let Ran::Broke(_, b) = &mut e {
                if b.kind == Breaks::Unique && d == Dialect::Postgres {
                    b.rows = duplicates_query(db, b).await;
                }
            }
            let mut dropped = true;
            match invalid(db).await {
                Ok(now) => {
                    for index in now.iter().filter(|i| !before.contains(i)) {
                        dropped &= db.exec(&format!("drop index concurrently if exists {index}")).await.is_ok();
                    }
                }
                Err(_) => dropped = false,
            }
            if record.is_some() {
                let _ = db.exec_with(&format!("delete from {HISTORY_TABLE} where name = {}", db.param(1)), &[&m.name]).await;
            }
            // A statement that gave up waiting for a lock changed nothing, and SQLite's run whole or
            // not at all; on Postgres, an index build leaves nothing once its invalid index is
            // dropped, and any other may leave part of itself, as a detach left pending.
            // On MySQL, one statement whose rows broke a constraint is undone whole: a DDL statement
            // is atomic, and a row's is rolled back at the statement.
            let whole = d == Dialect::Sqlite || (dropped && crate::sql::builds_index_alone(&m.sql, d));
            return Err(match e {
                Ran::Failed(e) if whole => Ran::Failed(e),
                Ran::Broke(e, b) if whole || (d == Dialect::Mysql && split_statements(&m.sql, d).len() == 1) => Ran::Broke(e, b),
                Ran::Failed(e) | Ran::Partly(e) | Ran::Broke(e, _) => Ran::Partly(e),
                waited => waited,
            });
        }
        // The statement is in place from here, so a failure is no longer one that left nothing.
        reset.map_err(Ran::Partly)?;
        if let Some(stamp) = record {
            // Outside a transaction, a stamp that can't be read leaves a bare mark, which the next run
            // refuses until the fingerprint is stored.
            let mark = stamp.mark(db).await.unwrap_or_else(|_| PENDING.to_string());
            db.exec_with(&format!("update {HISTORY_TABLE} set applied_at = {}, fingerprint = {} where name = {}", now_sql(d), db.param(1), db.param(2)), &[&mark, &m.name]).await.map_err(Ran::Partly)?;
        }
        return Ok(());
    }
    // SQLite cannot turn foreign keys off inside a transaction, and a table rebuilt in a migration
    // (create, copy, drop, rename) must not cascade its drop: they are off for the transaction and
    // checked before it commits.
    if d == Dialect::Sqlite {
        db.exec("pragma foreign_keys = off").await?;
    }
    db.exec(if d == Dialect::Mysql { "start transaction" } else { "begin" }).await?;
    let result = async {
        if d == Dialect::Postgres {
            limit_lock_wait(db).await?;
        }
        db.script_or_wait(&m.sql).await?;
        if d == Dialect::Sqlite {
            let broken = db.rows("pragma foreign_key_check").await?;
            if !broken.is_empty() {
                return Err(Ran::Failed(Error(format!("{} rows break a foreign key after this migration", broken.len()))));
            }
            crate::catalog::keep_turso_sequences(db).await?;
        }
        if let Some(stamp) = record {
            let mark = stamp.mark(db).await?;
            insert_row(db, m, true, Some(&mark)).await?;
        }
        db.exec("commit").await?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = db.exec("rollback").await;
    }
    let on = if d == Dialect::Sqlite { db.exec("pragma foreign_keys = on").await } else { Ok(()) };
    result?;
    on.map_err(Ran::Partly)
}

// ---------------------------------------------------------------------------------------------
// The check.

/// How the schema `a` differs from `b`, as a diff from one to the other under headers that name
/// them, set off by a blank line on each side: `-` what only `a` has, `+` what only `b` has.
fn describe_diff(label_a: &str, a: &[String], label_b: &str, b: &[String]) -> Vec<String> {
    const MAX: usize = 20;
    let (only_a, only_b) = difference(a, b);
    let (only_a, only_b) = (without_default_grants(&only_a), without_default_grants(&only_b));
    let (only_a, only_b, mut changes) = in_place(only_a.iter().collect(), only_b.iter().collect(), "    ");
    changes.extend(only_a.iter().take(MAX).map(|l| paint("red", &format!("    - {}", readable(l)))));
    changes.extend(only_b.iter().take(MAX).map(|l| paint("green", &format!("    + {}", readable(l)))));
    let more = only_a.len().saturating_sub(MAX) + only_b.len().saturating_sub(MAX);
    if more > 0 {
        changes.push(format!("    … and {more} more"));
    }
    [String::new(), paint("dim", &format!("    --- {label_a}")), paint("dim", &format!("    +++ {label_b}"))].into_iter().chain(changes).chain([String::new()]).collect()
}

/// One side of a diff without the privileges an object it adds or drops has by default (Postgres's
/// `USAGE` on a type for every role, `EXECUTE` on a function): they come and go with the object. A
/// grant that changes on an object both sides have is kept.
fn without_default_grants(lines: &[String]) -> Vec<String> {
    static DEFAULT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^grant (?:type|domain|language|function|procedure) (?:[^\s.(]+\.)?([^\s.(]+)\S* PUBLIC:(?:USAGE|EXECUTE)$").expect("a valid pattern"));
    let comes_with = |line: &String| {
        let Some(c) = DEFAULT.captures(line) else { return false };
        let name = Regex::new(&format!(r"[\s.]{}\b", regex::escape(&c[1]))).expect("a valid pattern");
        lines.iter().any(|other| !other.starts_with("grant ") && name.is_match(other))
    };
    lines.iter().filter(|l| !comes_with(l)).cloned().collect()
}

/// A comparison line as a person reads it: `public.` left out, and an index or a constraint as SQL
/// writes it, its keywords in lowercase, rather than as the catalog prints it. An index reads
/// `index <name> on <table> (<columns>)` on every database, and SQLite's index, view or trigger
/// names itself once, rather than before its statement and in it.
fn readable(line: &str) -> String {
    static INDEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^index CREATE (UNIQUE )?INDEX (\S+) ON (?:ONLY )?(\S+) USING (\w+) (.*)$").expect("a valid pattern"));
    static MYSQL_INDEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^index ([^.\s]+)\.(\S+) (unique )?(\w+)((?: invisible)?(?: comment "(?:[^"\\]|\\.)*")?) (\(.*\))$"#).expect("a valid pattern"));
    static SQLITE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(index|view|trigger) \S+ create (unique )?(?:temp |temporary )?(?:index|view|trigger) (?:if not exists )?(\S+) (.*)$").expect("a valid pattern"));
    static CONSTRAINT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^constraint (\S+) (\S+) (.*)$").expect("a valid pattern"));
    static POSTGRES_DEFINITION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:PRIMARY KEY|UNIQUE|CHECK|FOREIGN KEY|EXCLUDE|NOT NULL)\b").expect("a valid pattern"));
    static SQLITE_CONSTRAINT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^constraint (\S+) ((?:primary key|unique|check|foreign key|constraint)\b.*)$").expect("a valid pattern"));
    static PUBLIC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(^|[\s(,]|::)(?:public|"public")\."#).expect("a valid pattern"));
    let line = PUBLIC.replace_all(line, "$1").into_owned();
    if let Some(c) = INDEX.captures(&line) {
        let using = if &c[4] == "btree" { String::new() } else { format!("using {} ", &c[4]) };
        return format!("{}index {} on {} {using}{}", if c.get(1).is_some() { "unique " } else { "" }, &c[2], &c[3], lowercase_keywords(&c[5]));
    }
    if let Some(c) = MYSQL_INDEX.captures(&line) {
        let using = if c[4].eq_ignore_ascii_case("btree") { String::new() } else { format!("using {} ", c[4].to_ascii_lowercase()) };
        return format!("{}index {} on {} {using}{}{}", c.get(3).map_or("", |m| m.as_str()), &c[2], &c[1], &c[6], &c[5]);
    }
    if let Some(c) = SQLITE.captures(&line) {
        return format!("{}{} {} {}", c.get(2).map_or("", |m| m.as_str()), &c[1], &c[3], &c[4]);
    }
    // A function by its signature and what it returns, not its body, which Postgres writes over
    // several lines; a trigger by its name and table, as a constraint is.
    if let Some(routine) = routine(&line) {
        return routine;
    }
    if let Some(rest) = line.strip_prefix("trigger CREATE ") {
        let (constraint, rest) = rest.strip_prefix("CONSTRAINT ").map_or(("", rest), |rest| ("constraint ", rest));
        // The table is the name after the first ON outside quotes, which a column's quoted name may hold.
        if let Some((name, rest)) = rest.strip_prefix("TRIGGER ").map(first_name) {
            if let Some((when, after)) = outside_quotes(rest.trim_start(), " ON ") {
                let (table, rest) = first_name(after);
                return format!("{constraint}trigger {name} on {table}: {} {}", lowercase_keywords(when), lowercase_keywords(rest.trim_start()));
            }
        }
    }
    // Postgres's, whose definition is in capitals, names its constraint, whatever the name; SQLite's,
    // in lowercase, names none unless its definition does.
    let named = CONSTRAINT.captures(&line).filter(|c| POSTGRES_DEFINITION.is_match(&c[3]));
    if let Some(c) = SQLITE_CONSTRAINT.captures(&line).filter(|_| named.is_none()) {
        return format!("constraint on {}: {}", &c[1], &c[2]);
    }
    if let Some(c) = CONSTRAINT.captures(&line) {
        // Postgres's definition of a constraint not validated ends in its own NOT VALID, which the
        // catalog line marks again.
        let def = c[3].strip_suffix(" not valid").filter(|d| d.ends_with(" NOT VALID")).unwrap_or(&c[3]);
        return format!("constraint {} on {}: {}", &c[2], &c[1], lowercase_keywords(def));
    }
    line
}

/// A Postgres function or procedure as `pg_get_functiondef` writes it, by its name, its arguments
/// whole (to the parenthesis that closes them, outside quotes, where a name or a default's string
/// may hold a line break, shown as `↵`) and what it returns. None for any other line, or one whose arguments don't close.
fn routine(line: &str) -> Option<String> {
    routine_parts(line).map(|(_, shown)| shown)
}

/// A routine's signature and what it returns as `pg_get_functiondef` writes them, which tells two
/// routines apart, and as `routine` shows them.
fn routine_parts(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("function CREATE OR REPLACE ")?;
    let (kind, rest) = rest.strip_prefix("FUNCTION ").map(|r| ("function", r)).or_else(|| rest.strip_prefix("PROCEDURE ").map(|r| ("procedure", r)))?;
    let (mut depth, mut quote) = (0, None::<char>);
    let mut end = None;
    for (k, c) in rest.char_indices() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => {
                depth -= 1;
                if depth == 0 {
                    end = Some(k + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    let raw_returns = rest[end..].strip_prefix("\n RETURNS ").and_then(|r| r.lines().next());
    let signature = lowercase_keywords(&rest[..end]).replace('\n', "↵");
    let returns = raw_returns.map(|r| format!(" returns {}", lowercase_keywords(r))).unwrap_or_default();
    Some((format!("{kind} {}\n{}", &rest[..end], raw_returns.unwrap_or_default()), format!("{kind} {signature}{returns}")))
}

/// The name `text` starts with, a qualified one whole and a quoted part with its spaces, and what
/// follows it.
fn first_name(text: &str) -> (&str, &str) {
    let mut quoted = false;
    let end = text.char_indices().find(|&(_, c)| {
        if c == '"' {
            quoted = !quoted;
        }
        c == ' ' && !quoted
    });
    end.map_or((text, ""), |(k, _)| (&text[..k], &text[k..]))
}

/// `text` split at the first `pattern` outside double quotes.
fn outside_quotes<'a>(text: &'a str, pattern: &str) -> Option<(&'a str, &'a str)> {
    let mut quoted = false;
    for (k, c) in text.char_indices() {
        if c == '"' {
            quoted = !quoted;
        } else if !quoted && text[k..].starts_with(pattern) {
            return Some((&text[..k], &text[k + pattern.len()..]));
        }
    }
    None
}

/// SQL's keywords in lowercase, outside its quotes: `PRIMARY KEY (id)` is `primary key (id)`.
fn lowercase_keywords(sql: &str) -> String {
    let mut out = String::new();
    let mut quote: Option<char> = None;
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if word.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
            out.push_str(&word.to_lowercase());
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for ch in sql.chars() {
        match quote {
            Some(q) => {
                out.push(ch);
                if ch == q {
                    quote = None;
                }
            }
            None if ch == '\'' || ch == '"' => {
                flush(&mut word, &mut out);
                quote = Some(ch);
                out.push(ch);
            }
            None if ch.is_ascii_alphanumeric() || ch == '_' => word.push(ch),
            None => {
                flush(&mut word, &mut out);
                out.push(ch);
            }
        }
    }
    flush(&mut word, &mut out);
    out
}

/// What changed from `before` to `after`, a line each, readable: `+` what `after` has, `-` what it
/// lost. An index a constraint of the same name makes (a primary key's, a unique constraint's) goes
/// with it.
fn changed_lines(before: &[String], after: &[String], d: Dialect) -> Vec<String> {
    let (gone, came) = difference(before, after);
    listed(&gone, &came, before, after, d)
}

/// The schema at each step of drafting a migration, as `catalog` reads it: before it, after its
/// tables moved to another schema and after its tables renamed (where it does either), and after it.
#[cfg(feature = "cli")]
struct Steps<'a> {
    /// The database's dialect: Postgres's lines may hold dollar-quoted bodies, and MySQL's name a
    /// table unquoted.
    dialect: Dialect,
    before: &'a [String],
    moved: Option<&'a [String]>,
    renamed: Option<&'a [String]>,
    /// The schema once the column renames that ran first ran, where any did.
    columns: Option<&'a [String]>,
    after: &'a [String],
}

/// What a migration changes, as `changed_lines` writes it: the difference from the schema before it
/// to the schema after it. Each table and column it renames (`tables` and `columns`, each the old id
/// and the new) shows as one line changed in place (`~`) rather than as a drop and an add, and what
/// only took a renamed table's new name is left out: a line the table renames themselves replaced
/// with the same object under the new names (a column, the same column of the renamed table), where
/// both lines are the endpoints'. Whatever else the renames did, such as a view they dropped,
/// stays listed.
#[cfg(feature = "cli")]
fn summary_lines(steps: &Steps<'_>, tables: &[(String, String)], columns: &[(String, String)]) -> Vec<String> {
    let (mut gone, mut came) = difference(steps.before, steps.after);
    let mut out: Vec<String> = tables.iter().map(|(old, new)| changed_line(&format!("table {}", shown_name(old)), &format!("table {}", shown_name(new)))).collect();
    // What the table renames replaced, each line before them with the line they left in its place.
    let mut moved_with: Vec<(String, String)> = Vec::new();
    if let Some(renamed) = steps.renamed {
        let (r_gone, r_came) = difference(steps.moved.unwrap_or(steps.before), renamed);
        // Each renamed table's name as the catalog writes it, schema and all, and as a name starts.
        let names: Vec<(String, String)> = tables.iter().map(|(old, new)| (shown_id(old), shown_id(new))).collect();
        let bare: Vec<(String, String)> = names.iter().map(|(old, new)| (crate::sql::last_written(old), crate::sql::last_written(new))).collect();
        // A column's line names it under its table, whose name alone the rename changes: the same
        // column, whatever its definition became, as the rename of a sequence named after the
        // table changes a default.
        let retabled = |line: &str| -> Option<String> {
            let name = split_head(line).0.strip_prefix("column ")?;
            let column = crate::sql::last_written(name);
            let table = name.strip_suffix(column.as_str())?.strip_suffix('.')?;
            names.iter().find(|(old, _)| old == table).map(|(_, new)| format!("column {new}.{column}"))
        };
        // Any other line: every renamed table's name replaced where it stands whole, and a word
        // outside parentheses, such as a constraint's or an index's name, may take a renamed
        // table's new name where it starts with the old, as the renames name what they follow. A
        // column list never changes.
        let same_other = |g: &str, c: &str| {
            let (g, c) = (renamed_in(g, &names, steps.dialect == Dialect::Postgres), c);
            let (x, y) = (top_words(&g), top_words(c));
            x.len() == y.len() && x.iter().zip(&y).all(|(a, b)| a == b || (!a.contains('(') && bare.iter().any(|(old, new)| crate::diff::swap_name(a, old, new) == *b)))
        };
        for g in &r_gone {
            // A view is the same view by its name, which a table rename never changes, whatever the
            // rename changed in its query.
            let same = |c: &String| match g.split(' ').next() {
                Some("column") => retabled(g).is_some_and(|h| split_head(c).0 == h),
                Some("view") => split_head(g).0 == split_head(c).0,
                _ => same_other(g, c),
            };
            if let Some(c) = r_came.iter().find(|c| same(c) && !moved_with.iter().any(|(_, t)| t == *c)) {
                moved_with.push((g.clone(), c.clone()));
            }
        }
    }
    for (g, c) in &moved_with {
        if let (Some(i), Some(j)) = (gone.iter().position(|l| l == g), came.iter().position(|l| l == c)) {
            gone.remove(i);
            came.remove(j);
        }
    }
    let is = |line: &str, name: &str| readable(line).strip_prefix(&format!("column {name}")).is_some_and(|rest| rest.is_empty() || rest.starts_with(' '));
    for (old, new) in columns {
        let (old, new) = (shown_name(old), shown_name(new));
        // The old column as the schema before the migration has it: under its table's old name where
        // its table was renamed too.
        let before = |line: &String| is(line, &old) || moved_with.iter().any(|(g, c)| g == line && is(c, &old));
        // Only both lines together make a rename.
        if let (Some(g), Some(c)) = (gone.iter().position(before), came.iter().position(|l| is(l, &new))) {
            let (a, b) = (gone.remove(g), came.remove(c));
            let a = moved_with.iter().find(|(g, _)| *g == a).map_or(a.clone(), |(_, c)| c.clone());
            out.push(changed_line(&readable(&a), &readable(&b)));
        }
    }
    // What only took a renamed column's new name (a key, an index or a view over it), as the renames
    // that ran first left it and as it still is after, is left out: the server renamed it with the
    // column. A line is matched by its text with the column's new name in it, a view by its name.
    if let Some(renamed_columns) = steps.columns {
        let (r_gone, r_came) = difference(steps.renamed.or(steps.moved).unwrap_or(steps.before), renamed_columns);
        let names: Vec<(String, String)> = columns.iter().map(|(old, new)| (crate::sql::last_written(old), crate::sql::last_written(new))).collect();
        let swapped = |line: &str| names.iter().fold(line.to_string(), |text, (old, new)| swap_word(&text, old, new));
        // A line as the table renames left it.
        let at = |g: &String| moved_with.iter().find(|(m, _)| m == g).map_or(g.clone(), |(_, c)| c.clone());
        let mut pairs: Vec<(String, String)> = Vec::new();
        for by_name in [false, true] {
            let open: Vec<String> = gone.iter().filter(|g| r_gone.contains(&at(g)) && !pairs.iter().any(|(p, _)| p == *g)).cloned().collect();
            for g in &open {
                let (line, renamed) = (at(g), swapped(&at(g)));
                let same = |c: &&String| if by_name { line.starts_with("view ") && split_head(c).0 == split_head(&line).0 } else { **c == renamed };
                if let Some(c) = came.iter().filter(|c| r_came.contains(c) && !pairs.iter().any(|(_, t)| t == *c)).find(same) {
                    pairs.push((g.clone(), c.clone()));
                }
            }
        }
        gone.retain(|g| !pairs.iter().any(|(p, _)| p == g));
        came.retain(|c| !pairs.iter().any(|(_, t)| t == c));
    }
    out.extend(listed(&gone, &came, steps.before, steps.after, steps.dialect));
    out
}

/// `text` with the name `old` (a column's) written `new` wherever it stands whole.
fn swap_word(text: &str, old: &str, new: &str) -> String {
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let (mut out, mut rest, mut before) = (String::with_capacity(text.len()), text, None::<char>);
    while let Some(c) = rest.chars().next() {
        if !old.is_empty() && rest.starts_with(old) && !before.is_some_and(ident) && !rest[old.len()..].chars().next().is_some_and(ident) {
            out.push_str(new);
            before = new.chars().last();
            rest = &rest[old.len()..];
            continue;
        }
        out.push(c);
        before = Some(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

/// `line` with each of `names` (an old name and its new, as the catalog writes them) replaced where
/// it stands as a whole name, all at once: `public.orders` in `public.orders(id)` and
/// `public.orders.title`, and as a relation constant, `'public.orders'::regclass`, but not in
/// `public.orders_archive`, inside a quoted name, `"public.orders"`, or inside a string,
/// `'public.orders'`, or on Postgres a dollar-quoted body, whose text a rename never changes.
#[cfg(feature = "cli")]
fn renamed_in(line: &str, names: &[(String, String)], postgres: bool) -> String {
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let (mut out, mut at, mut prev) = (String::new(), 0, None::<char>);
    while let Some(c) = line[at..].chars().next() {
        let rest = &line[at..];
        if prev.is_none_or(|p| !ident(p) && p != '.') {
            if let Some((old, new)) = names.iter().find(|(old, _)| rest.starts_with(old.as_str()) && rest[old.len()..].chars().next().is_none_or(|n| !ident(n))) {
                out.push_str(new);
                at += old.len();
                prev = old.chars().last();
                continue;
            }
        }
        // A dollar-quoted body, `$$ … $$` or `$tag$ … $tag$`, is source text Postgres keeps as
        // written: whole, to its closing tag. Elsewhere a `$` may be part of a name.
        if postgres && c == '$' && prev.is_none_or(|p| !ident(p)) {
            let tag_end = rest[1..].find(|t: char| !(t.is_ascii_alphanumeric() || t == '_')).map(|k| k + 1);
            if let Some(k) = tag_end.filter(|k| rest[*k..].starts_with('$') && rest[1..*k].chars().next().is_none_or(|t| !t.is_ascii_digit())) {
                let tag = &rest[..=k];
                let end = rest[tag.len()..].find(tag).map_or(rest.len(), |at| tag.len() + at + tag.len());
                out.push_str(&rest[..end]);
                at += end;
                prev = Some('$');
                continue;
            }
        }
        if c == '\'' || c == '"' || c == '`' {
            // A quoted name or a string, whole, to its closing quote (a doubled quote stays inside).
            let mut end = 1;
            loop {
                match rest[end..].find(c) {
                    Some(k) if rest[end + k + 1..].starts_with(c) => end += k + 2,
                    Some(k) => {
                        end += k + 1;
                        break;
                    }
                    None => {
                        end = rest.len();
                        break;
                    }
                }
            }
            let span = &rest[..end];
            // A relation constant is the relation itself, which Postgres prints by its new name.
            let relation = (c == '\'' && rest[end..].starts_with("::regclass")).then(|| &span[1..span.len().saturating_sub(1)]);
            match relation.and_then(|r| names.iter().find(|(old, _)| old == r)) {
                Some((_, new)) => out.push_str(&format!("'{new}'")),
                None => out.push_str(span),
            }
            at += end;
            prev = Some(c);
            continue;
        }
        out.push(c);
        at += c.len_utf8();
        prev = Some(c);
    }
    out
}

/// A line's words, split at spaces outside parentheses and quotes: `FOREIGN KEY (a, b)` is three.
#[cfg(feature = "cli")]
fn top_words(line: &str) -> Vec<&str> {
    let (mut words, mut start, mut depth, mut quote) = (Vec::new(), 0, 0i32, None::<char>);
    for (k, c) in line.char_indices() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"' | '`') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, ' ') if depth == 0 => {
                if k > start {
                    words.push(&line[start..k]);
                }
                start = k + 1;
            }
            _ => {}
        }
    }
    if start < line.len() {
        words.push(&line[start..]);
    }
    words
}

/// The lines of `changed_lines`, from what went and what came, of the whole schemas `before` and
/// `after` in dialect `d`.
fn listed(gone: &[String], came: &[String], before: &[String], after: &[String], d: Dialect) -> Vec<String> {
    let mysql = d == Dialect::Mysql;
    const MAX: usize = 20;
    let (gone, came) = (without_default_grants(gone), without_default_grants(came));
    let constraint_names = |lines: &[String]| -> Vec<String> { lines.iter().filter_map(|l| l.strip_prefix("constraint ")).filter_map(|rest| rest.split(' ').nth(1)).map(str::to_string).collect() };
    let made_by_constraint = |line: &str, names: &[String]| names.iter().any(|n| line.starts_with("index ") && line.contains(&format!(" INDEX {n} ON ")));
    let (gone_names, came_names) = (constraint_names(&gone), constraint_names(&came));
    // A SQLite virtual table's own tables (FTS5's `_data`, an R-tree's `_node`) come and go with it,
    // which is listed for them.
    let shadows = if d == Dialect::Sqlite { shadow_tables(before.iter().chain(after)) } else { Vec::new() };
    let own = |line: &str| shadows.iter().any(|t| table_name(line, false).as_ref() == Some(t) || line.starts_with(&format!("column {t}.")) || line.starts_with(&format!("constraint {t} ")));
    let gone: Vec<&String> = gone.iter().filter(|l| !made_by_constraint(l, &gone_names) && !own(l)).collect();
    let came: Vec<&String> = came.iter().filter(|l| !made_by_constraint(l, &came_names) && !own(l)).collect();
    // A table one side has and the other doesn't, by its name across each whole schema, came or
    // went; one both have may have changed its own line, as row level security does.
    // A name two of one side's tables read as is no one table's, and groups nothing.
    let names = |lines: &[String]| -> Vec<String> { lines.iter().filter_map(|l| table_name(&readable(l), mysql)).collect() };
    let (had, has) = (names(before), names(after));
    let tables: Vec<String> = had.iter().chain(&has).cloned().collect();
    let once = |name: &String, side: &[String]| side.iter().filter(|t| *t == name).count() == 1;
    let added: Vec<String> = has.iter().filter(|t| !had.contains(t) && once(t, &has)).cloned().collect();
    let dropped: Vec<String> = had.iter().filter(|t| !has.contains(t) && once(t, &had)).cloned().collect();
    // A line both sides read the same, as a function whose body alone changed does, changed in place.
    let (gone, came, mut out) = in_place(gone, came, "  ");
    let (came_out, more_came) = marked(&came, "green", '+', MAX, &added, &tables, mysql);
    out.extend(came_out);
    let (gone, more_gone) = marked(&gone, "red", '-', MAX, &dropped, &tables, mysql);
    out.extend(gone);
    let more = more_came + more_gone;
    if more > 0 {
        out.push(format!("  … and {more} more"));
    }
    out
}

/// What went and what came, without each function or procedure both sides have by the same
/// signature (its body alone changed), and each of those as one line changed in place, `indent` in.
/// Every other line is left where it is.
fn in_place<'a>(gone: Vec<&'a String>, came: Vec<&'a String>, indent: &str) -> (Vec<&'a String>, Vec<&'a String>, Vec<String>) {
    let mut by_signature: std::collections::HashMap<String, Vec<usize>> = std::collections::HashMap::new();
    for (k, c) in came.iter().enumerate() {
        if let Some((signature, _)) = routine_parts(c) {
            by_signature.entry(signature).or_default().push(k);
        }
    }
    let mut taken = vec![false; came.len()];
    let mut kept = Vec::new();
    let mut changed = Vec::new();
    for g in gone {
        let found = routine_parts(g).and_then(|(signature, _)| by_signature.get_mut(&signature).and_then(|ks| (!ks.is_empty()).then(|| ks.remove(0))));
        match found {
            Some(k) => {
                taken[k] = true;
                changed.push(format!("{indent}{} {}", paint("yellow", "~"), readable(g)));
            }
            None => kept.push(g),
        }
    }
    (kept, came.into_iter().zip(taken).filter(|(_, t)| !t).map(|(c, _)| c).collect(), changed)
}

/// The name of the table a readable `table …` line is, quotes and all: `"order items"` whole. On
/// MySQL, which writes a name unquoted, all before the table's engine and collation; its
/// `table options` line, a table's settings, is no table.
/// The tables SQLite's virtual tables in the catalog's `lines` keep for themselves, by their names:
/// FTS3, FTS4 and FTS5's and an R-tree's, each named after its table.
fn shadow_tables<'a>(lines: impl Iterator<Item = &'a String> + Clone) -> Vec<String> {
    let mut out = Vec::new();
    for line in lines.clone() {
        let Some((name, module)) = table_name(line, false).zip(line.split(" using ").nth(1)) else { continue };
        // A full-text table whose text is another's, or nowhere, keeps no `_content` of its own:
        // a table of that name is the project's.
        let content = lines.clone().any(|l| l.starts_with(&format!("column {name}.content =")));
        let suffixes: &[&str] = match module.split(' ').next().unwrap_or_default() {
            "fts5" => &["data", "idx", "content", "docsize", "config"],
            "fts3" | "fts4" => &["content", "segments", "segdir", "docsize", "stat"],
            "rtree" | "rtree_i32" | "geopoly" => &["node", "parent", "rowid"],
            _ => &[],
        };
        out.extend(suffixes.iter().filter(|s| !(content && **s == "content")).map(|s| format!("{name}_{s}")));
    }
    out
}

fn table_name(line: &str, mysql: bool) -> Option<String> {
    static MYSQL_TABLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^table (.+) engine=\S+ collate=\S+").expect("a valid pattern"));
    if mysql {
        return MYSQL_TABLE.captures(line).map(|c| c[1].to_string());
    }
    let rest = line.strip_prefix("table ")?;
    let mut quote = None;
    let end = rest
        .char_indices()
        .find(|&(_, c)| {
            match (quote, c) {
                (Some(q), _) if c == q => quote = None,
                (None, '"' | '`') => quote = Some(c),
                (None, ' ') => return true,
                _ => {}
            }
            false
        })
        .map_or(rest.len(), |(k, _)| k);
    Some(rest[..end].to_string())
}

/// The table a readable line belongs to, of `tables` (every table on either side), and the line as
/// that table's own, without its name: a column, a constraint, an index, and MySQL's foreign keys
/// and checks. None where no table, or more than one, could own it, as `t` and `t.archive` could
/// both own SQLite's `column t.archive.label`.
fn owner(line: &str, tables: &[String], mysql: bool) -> Option<(String, String)> {
    // A table's own line is no other's part, as MySQL's `options orders` is no setting of `orders`.
    if table_name(line, mysql).is_some() {
        return None;
    }
    let of = |table: &String| -> Option<String> {
        if let Some(rest) = line.strip_prefix("column ") {
            return rest.strip_prefix(&format!("{table}.")).map(str::to_string);
        }
        if let Some(rest) = line.strip_prefix("trigger ") {
            return rest.split_once(&format!(" on {table}: ")).filter(|(name, _)| !name.contains(' ')).map(|(name, def)| format!("trigger {name}: {def}"));
        }
        if let Some(rest) = line.strip_prefix("constraint ") {
            return rest.split_once(&format!(" on {table}: ")).filter(|(name, _)| !name.contains(' ')).map(|(_, def)| def.to_string()).or_else(|| rest.strip_prefix(&format!("on {table}: ")).map(str::to_string));
        }
        // MySQL's foreign keys, checks and a table's settings, each a line of its own.
        for kind in ["foreign key", "check", "table options"].into_iter().filter(|_| mysql) {
            if let Some(rest) = line.strip_prefix(&format!("{kind} {table} ")) {
                return Some(format!("{} {rest}", kind.trim_start_matches("table ")));
            }
        }
        let (kind, rest) = line.strip_prefix("unique index ").map(|rest| ("unique index", rest)).or_else(|| line.strip_prefix("index ").map(|rest| ("index", rest)))?;
        let (name, rest) = rest.split_once(' ')?;
        rest.strip_prefix(&format!("on {table} ")).map(|def| format!("{kind} {name} {def}"))
    };
    let mut owners = tables.iter().filter_map(|t| of(t).map(|own| (t.clone(), own)));
    let first = owners.next()?;
    owners.all(|(t, _)| t == first.0).then_some(first)
}

/// What came or went, `mark` before each, readable: each table of `grouped` (those that came or
/// went whole) one line, its columns, constraints and indexes indented under it rather than each
/// on a line of its own. At most `max` of them, and how many more there are.
fn marked(lines: &[&String], style: &str, mark: char, max: usize, grouped: &[String], tables: &[String], mysql: bool) -> (Vec<String>, usize) {
    let lines: Vec<String> = lines.iter().map(|l| readable(l)).collect();
    // A view's own line, its name alone, says nothing its definition's line doesn't.
    // A materialized view's definition line reads `view <name> as …`: it takes its kind back from
    // the view's own line, which then says nothing more, as a view's own line doesn't.
    let materialized: Vec<String> = lines.iter().filter_map(|l| l.strip_prefix("materialized view ")).filter(|name| lines.iter().any(|d| d.starts_with(&format!("view {name} as ")))).map(str::to_string).collect();
    let view = |l: &String| l.starts_with("view ") && lines.iter().any(|d| d.starts_with(&format!("{l} as ")));
    let lines: Vec<String> = lines
        .iter()
        .filter(|l| !view(l) && !materialized.iter().any(|name| **l == format!("materialized view {name}")))
        .map(|l| if materialized.iter().any(|name| l.starts_with(&format!("view {name} as "))) { format!("materialized {l}") } else { l.clone() })
        .collect();
    // Only a table whose own line is listed here has its parts under it: one a rename took out of the
    // list keeps them as lines of their own.
    let headed: Vec<String> = lines.iter().filter_map(|l| table_name(l, mysql)).filter(|t| grouped.contains(t)).collect();
    let owned: Vec<Option<(String, String)>> = lines.iter().map(|l| owner(l, tables, mysql).filter(|(t, _)| headed.contains(t))).collect();
    let mut out = Vec::new();
    let mut shown = 0;
    let mut total = 0;
    for line in lines.iter().zip(&owned).filter(|(_, o)| o.is_none()).map(|(l, _)| l) {
        total += 1;
        if shown == max {
            continue;
        }
        shown += 1;
        out.push(paint(style, &format!("  {mark} {line}")));
        if let Some(table) = table_name(line, mysql).filter(|t| headed.contains(t)) {
            out.extend(owned.iter().flatten().filter(|(t, _)| *t == table).map(|(_, own)| paint(style, &format!("      {own}"))));
        }
    }
    (out, total - shown)
}

/// Prints the refusals of a command that changes something, and says that it changed nothing.
fn refuse(problems: &[Problem], log: Log<'_>) -> i32 {
    print_problems(problems, log);
    log("");
    log(&paint("dim", "Nothing was changed."));
    1
}

/// What every command calls a database changed by hand since its last migration.
const CHANGED_OUTSIDE: &str = "This database was changed outside its migrations";

/// One thing a check refuses: its heading, and the lines under it, the SQL first and what to run last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// What kind of refusal it is, which stays the same from one version to the next: for a program
    /// reading `--json`, where the title's words may change.
    pub code: &'static str,
    pub title: String,
    pub lines: Vec<String>,
    /// The parts of `lines`, as `--json` gives them.
    pub detail: Vec<String>,
    pub sql: Option<String>,
    pub notes: Vec<String>,
    pub fixes: Vec<(String, Vec<String>)>,
    fixed: bool,
    /// Whether it is the lock's drafts, which some commands pass over.
    drafts: bool,
    /// What each command it suggests states, without `--blocking`: for `migrate new` to try, and to
    /// add `--blocking` to where the migration it writes would need it.
    readings: Vec<Choices>,
    /// What a `blocks-writes` refusal's migration does that `--blocking` accepts, each as a
    /// sentence's predicate: `rewrites users, …`.
    hazards: Vec<String>,
}

impl Problem {
    fn new(code: &'static str, title: impl Into<String>) -> Problem {
        Problem { code, title: title.into(), lines: vec![], detail: vec![], sql: None, notes: vec![], fixes: vec![], fixed: false, drafts: false, readings: vec![], hazards: vec![] }
    }

    /// The refusal as `--json` gives it.
    pub fn json(&self) -> serde_json::Value {
        let mut out = serde_json::json!({ "code": self.code, "title": self.title });
        let detail: Vec<String> = self.detail.iter().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        if !detail.is_empty() {
            out["detail"] = detail.into();
        }
        if let Some(sql) = &self.sql {
            out["sql"] = sql.clone().into();
        }
        if !self.notes.is_empty() {
            out["notes"] = self.notes.clone().into();
        }
        if !self.fixes.is_empty() {
            out["fixes"] = self.fixes.iter().map(|(what, commands)| serde_json::json!({ "what": what, "commands": commands })).collect();
        }
        out
    }

    fn is_drafts(&self) -> bool {
        self.drafts
    }

    /// SQL under the heading, set off by blank lines: what differs, never a fix to run.
    fn sql(mut self, sql: &str) -> Problem {
        self.sql = Some(sql.trim_end().to_string());
        self.lines.push(String::new());
        self.lines.extend(sql.trim_end().lines().map(|l| format!("    {l}")));
        self.lines.push(String::new());
        self
    }

    fn note(mut self, text: impl AsRef<str>) -> Problem {
        self.notes.push(text.as_ref().to_string());
        self.lines.push(format!("  {}", text.as_ref()));
        self
    }

    /// What to do, then each command that does it on a line of its own.
    fn fix(mut self, what: impl AsRef<str>, commands: &[&str]) -> Problem {
        if !self.fixed && self.lines.last().is_none_or(|l| !l.is_empty()) {
            self.lines.push(String::new());
        }
        self.fixed = true;
        self.fixes.push((what.as_ref().to_string(), commands.iter().map(|c| c.to_string()).collect()));
        self.lines.push(paint("dim", &format!("  {}:", what.as_ref())));
        self.lines.extend(commands.iter().map(|c| format!("    {}", paint("cyan", c))));
        self
    }

    /// `fix`, with the command that states `reading`, which it records; with `--blocking` where
    /// `asked` knows the migration that reading writes holds a lock that needs it.
    fn offer(mut self, what: impl AsRef<str>, asked: &Asked, reading: Choices) -> Problem {
        let command = command_line(asked, &asked.blocking(&reading), &[], &[], false);
        self.readings.push(reading);
        self.fix(what, &[&command])
    }

    fn lines(mut self, lines: impl IntoIterator<Item = String>) -> Problem {
        let lines: Vec<String> = lines.into_iter().collect();
        self.detail.extend(lines.iter().cloned());
        self.lines.extend(lines);
        self
    }
}

#[derive(Debug, Default)]
pub struct Plan {
    pub problems: Vec<Problem>,
    /// Pending migrations recorded without running: lone statements a cut-off run left, which landed.
    pub records: Vec<Migration>,
    /// Migrations to run, in order.
    pub pending: Vec<Migration>,
    /// Whether the live schema is the one Reylo's last run on this database left, by the
    /// fingerprint that run stored; None where no run stored one, or another server version or
    /// catalog reader read it.
    pub unchanged: Option<bool>,
    /// The newest row, where a run was cut off before it stored its fingerprint and the schema is as
    /// its migration left it: deploy stores the fingerprint it lacks.
    pub resumed: Option<String>,
    /// The records that are lone statements cut off before their row was finished, which landed.
    pub landed: Vec<String>,
    /// The invalid indexes such statements left, which `deploy` drops before running them again.
    pub leftovers: Vec<Leftover>,
    /// The migrations the database ran, in its order: with no lock, a pending one that sorts before
    /// any of them must be shown to end where name order does.
    pub ran: Vec<Migration>,
}

/// An index a lone `create index concurrently` (or `reindex concurrently`) left invalid when it was
/// cut off.
#[derive(Debug, Clone, Default)]
pub struct Leftover {
    /// The migration whose row is marked started.
    pub migration: String,
    /// The index, schema-qualified.
    pub index: String,
    /// Its catalog line, up to its `(invalid)` mark.
    line: String,
}

/// The invalid indexes on a Postgres database that the started rows' statements left: found as the
/// server found what each names, on its own table.
async fn leftovers(db: &mut Db, history: &[HistoryRow]) -> Result<Vec<Leftover>, Error> {
    let started: Vec<&HistoryRow> = history.iter().filter(|r| !r.applied).collect();
    if db.dialect() != Dialect::Postgres || started.is_empty() {
        return Ok(vec![]);
    }
    const NAME: &str = r#"(?:"(?:[^"]|"")*"|[^\s(".;]+)(?:\.(?:"(?:[^"]|"")*"|[^\s(".;]+))*"#;
    let create = regex::Regex::new(&format!(r"(?is)^create\s+(?:unique\s+)?index\s+concurrently\s+(?:if\s+not\s+exists\s+)?(?:({NAME})\s+)?on\s+(?:only\s+)?({NAME})")).unwrap();
    let drop = regex::Regex::new(&format!(r"(?is)^drop\s+index\s+concurrently\s+(?:if\s+exists\s+)?({NAME})")).unwrap();
    let reindex = regex::Regex::new(&format!(r"(?is)^reindex\s+(?:\([^)]*\)\s*)?(index|table)\s+concurrently\s+({NAME})")).unwrap();
    let literal = |s: &str| format!("'{}'", s.replace('\'', "''"));
    // A name as the server stores it: unquoted folded to lower case.
    let stored = |name: &str| match name.strip_prefix('"').and_then(|n| n.strip_suffix('"')) {
        Some(inner) => inner.replace("\"\"", "\""),
        None => name.to_lowercase(),
    };
    let mut out = Vec::new();
    for m in started {
        // The one statement, without the comments above it.
        let Some(statement) = split_statements(&m.sql, Dialect::Postgres).into_iter().next() else { continue };
        let sql = statement.text.trim().trim_end_matches(';').trim();
        let found = if let Some(c) = create.captures(sql) {
            // An index is made in its table's schema, and named without one.
            let on = format!("i.indrelid = to_regclass({})", literal(&c[2]));
            match c.get(1) {
                Some(name) => format!("{on} and c.relname = {}", literal(&stored(name.as_str()))),
                None => on,
            }
        } else if let Some(c) = drop.captures(sql) {
            format!("c.oid = to_regclass({})", literal(&c[1]))
        } else if let Some(c) = reindex.captures(sql) {
            // A concurrent reindex builds <index>_ccnew beside each index, and swaps the old one to <index>_ccold.
            let copies = |base: &str| format!("(c.relname like {} or c.relname like {})", literal(&format!("{}\\_ccnew%", base.replace('_', "\\_"))), literal(&format!("{}\\_ccold%", base.replace('_', "\\_"))));
            if c[1].eq_ignore_ascii_case("index") {
                let bare = stored(&crate::sql::last_written(&c[2]));
                format!("i.indrelid = (select x.indrelid from pg_index x where x.indexrelid = to_regclass({})) and {}", literal(&c[2]), copies(&bare))
            } else {
                format!("i.indrelid = to_regclass({}) and {}", literal(&c[2]), copies("%"))
            }
        } else {
            continue;
        };
        for r in db
            .rows(&format!("select format('%I.%I', n.nspname, c.relname) as qualified, pg_get_indexdef(c.oid) as def from pg_index i join pg_class c on c.oid = i.indexrelid join pg_namespace n on n.oid = c.relnamespace where not i.indisvalid and {found}"))
            .await?
        {
            out.push(Leftover { migration: m.name.clone(), index: r.text("qualified").to_string(), line: format!("index {} (invalid)", r.text("def")) });
        }
    }
    Ok(out)
}

async fn drop_leftovers(db: &mut Db, leftovers: &[Leftover], log: Log<'_>) -> Result<(), Error> {
    for x in leftovers {
        db.exec(&format!("drop index concurrently if exists {}", x.index)).await?;
        log(&note(format!("Dropped {}, which {} left invalid when it was cut off", x.index, x.migration)));
    }
    Ok(())
}

impl Plan {
    pub fn ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Whether two texts are the same statements, comments and whitespace aside; on MySQL, an executable
/// comment's version too, which says whether its text runs.
fn same_statements(a: &str, b: &str, d: Dialect) -> bool {
    let tokens = |sql: &str| split_statements(sql, d).into_iter().map(|s| s.tokens.into_iter().map(|t| t.text).collect::<Vec<_>>()).collect::<Vec<_>>();
    tokens(a) == tokens(b) && crate::sql::gates(a, d) == crate::sql::gates(b, d)
}

/// The statements of a migration that change data, as their first words: such a migration always
/// runs, since no comparison of schemas can see whether its rows were written.
fn data_statements(sql: &str, d: Dialect) -> Vec<String> {
    let statements = split_statements(sql, d);
    statements.iter().enumerate().filter(|(k, s)| changes_data(s, d) && !copies_a_rebuild(&statements, *k)).map(|(_, s)| format!("{}…", s.text.split_whitespace().take(3).collect::<Vec<_>>().join(" ").to_lowercase())).collect()
}

/// Whether `reylo run` applies `sql` as deploy runs a migration: whole or not at all, and touching
/// no rows deploy would keep. On Postgres a script runs as one transaction (a lone statement that
/// can't run in one, as a migration's must be); elsewhere one statement does. And SQLite runs it with
/// foreign keys on, where a migration runs with them off and checks them before it commits, so a
/// table dropped there takes its children's rows with it: only what touches no rows through them,
/// anything made, or an index, a view or a trigger dropped.
fn runs_as_a_migration(sql: &str, d: Dialect) -> bool {
    let statements = split_statements(sql, d);
    let word = |s: &crate::sql::Statement, k: usize| s.tokens.get(k).map(crate::sql::Token::upper).unwrap_or_default();
    match d {
        Dialect::Postgres => !statements.iter().any(|s| word(s, 0) == "COMMIT"),
        Dialect::Mysql => statements.len() == 1,
        Dialect::Sqlite => statements.len() == 1 && statements.iter().all(|s| word(s, 0) == "CREATE" || (word(s, 0) == "DROP" && matches!(word(s, 1).as_str(), "INDEX" | "VIEW" | "TRIGGER"))),
    }
}

/// The statements of a migration that write rows, as [`data_statements`] gives them, with the
/// schema changes that fill what they make ([`fills`]). What they write turns on the rows and
/// objects they read, so it can change with the order they run in, and no schema shows it was
/// written.
fn writes_rows(sql: &str, d: Dialect) -> Vec<String> {
    let mut out = data_statements(sql, d);
    out.extend(split_statements(sql, d).iter().filter(|s| fills(s, d)).map(|s| format!("{}…", s.text.split_whitespace().take(3).collect::<Vec<_>>().join(" ").to_lowercase())));
    out
}

/// The statements of a migration whose rows can turn out differently run before or after another
/// migration: those that write rows ([`writes_rows`]), and the schema changes that compute a value
/// for every row a table already has ([`fills_existing`]). Nothing moves past one of these.
fn order_sensitive(sql: &str, d: Dialect) -> Vec<String> {
    let mut out = writes_rows(sql, d);
    out.extend(split_statements(sql, d).iter().filter(|s| fills_existing(s)).map(|s| format!("{}…", s.text.split_whitespace().take(3).collect::<Vec<_>>().join(" ").to_lowercase())));
    out
}

/// Whether a schema change computes a value for each row a table already has from an expression
/// that can call what other migrations define, and stores it: a column added with a default that
/// calls something or generated and stored, a column retyped with `using`, and an index whose keys
/// or predicate call something. What it stores turns on what those calls do when it runs, which no
/// schema shows, and an index scan reads its keys as they were stored.
fn fills_existing(statement: &crate::sql::Statement) -> bool {
    use crate::sql::TokenKind;
    let t = &statement.tokens;
    let words: Vec<String> = t.iter().map(crate::sql::Token::upper).collect();
    // A name and a parenthesis, but for a keyword's that Postgres lets name no function, reserved
    // or a column's alone (`where (…)`, `check (…)`), and the table a reference names. A name after
    // a schema's is a function's, whatever it is spelled. `like`, `ilike`, `similar` and `is` can
    // name one too: after an operand (`email like (…)`, `email not like (…)`) each is an operator.
    const NO_CALL: &[&str] = &["WHERE", "AND", "OR", "NOT", "IN", "EXISTS", "ANY", "ALL", "SOME", "BETWEEN", "CASE", "WHEN", "THEN", "ELSE", "CHECK", "ROW", "FROM", "DISTINCT", "TO", "AS", "ON", "USING", "SELECT", "ARRAY"];
    const INFIX: &[&str] = &["LIKE", "ILIKE", "SIMILAR", "IS"];
    let ends_operand = |j: usize| match t[j].kind {
        TokenKind::Quoted | TokenKind::Str | TokenKind::Number => true,
        // A name after a table's is a column's, whatever it is spelled.
        TokenKind::Word => (j > 0 && t[j - 1].text == ".") || (!NO_CALL.contains(&words[j].as_str()) && !INFIX.contains(&words[j].as_str()) && words[j] != "DEFAULT"),
        TokenKind::Symbol => t[j].text == ")",
    };
    let infix = |i: usize| {
        if !INFIX.contains(&words[i].as_str()) || i == 0 {
            return false;
        }
        let j = if words[i - 1] == "NOT" && i > 1 { i - 2 } else { i - 1 };
        ends_operand(j)
    };
    let call = |i: usize| {
        i > 0
            && t[i].text == "("
            && t[i].kind == TokenKind::Symbol
            && (t[i - 1].kind == TokenKind::Quoted || (t[i - 1].kind == TokenKind::Word && ((!NO_CALL.contains(&words[i - 1].as_str()) && !infix(i - 1)) || (i > 1 && t[i - 2].text == "."))))
            && !(i > 1 && words[i - 2] == "REFERENCES")
    };
    if words.first().map(String::as_str) == Some("CREATE") && words.iter().take(4).any(|w| w == "INDEX") {
        // The key list, the first parenthesis after the table's name (and its access method), and
        // the predicate after `where`.
        let Some(on) = words.iter().position(|w| w == "ON") else { return false };
        let Some(open) = (on + 1..t.len()).find(|&i| t[i].text == "(" && t[i].kind == TokenKind::Symbol) else { return false };
        let mut depth = 0usize;
        let mut close = t.len();
        for (i, token) in t.iter().enumerate().skip(open) {
            match token.text.as_str() {
                "(" if token.kind == TokenKind::Symbol => depth += 1,
                ")" if token.kind == TokenKind::Symbol => {
                    depth -= 1;
                    if depth == 0 {
                        close = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let predicate = (close..t.len()).find(|&i| words[i] == "WHERE").unwrap_or(t.len());
        return (open + 1..close).chain(predicate..t.len()).any(call);
    }
    if words.first().map(String::as_str) != Some("ALTER") || words.get(1).map(String::as_str) != Some("TABLE") {
        return false;
    }
    let has = |w: &str| words.iter().any(|x| x == w);
    if !has("ADD") && !has("USING") {
        return false;
    }
    if (has("ADD") && has("STORED")) || (has("TYPE") && has("USING")) {
        return true;
    }
    // A call in a default, up to the end of its clause.
    words.iter().enumerate().filter(|(_, w)| *w == "DEFAULT").any(|(k, _)| {
        let mut depth = 0usize;
        for i in k + 1..t.len() {
            match t[i].text.as_str() {
                "(" if i > k + 1 && call(i) => return true,
                "(" if t[i].kind == TokenKind::Symbol => depth += 1,
                ")" if t[i].kind == TokenKind::Symbol => depth = depth.saturating_sub(1),
                "," if t[i].kind == TokenKind::Symbol && depth == 0 => return false,
                _ => {}
            }
        }
        false
    })
}

/// Whether a statement makes what a query fills, read from its words, never a string's or a
/// comment's: a table made from a query (`create table … as select`, and MySQL's `create table …
/// select` and `create table … (select …)`, the query outside any parentheses but its own), and a
/// materialized view, made or refreshed. The parenthesis is MySQL's alone, where a query's words are
/// reserved: SQLite takes `with` for a column's name.
fn fills(statement: &crate::sql::Statement, d: Dialect) -> bool {
    let words: Vec<String> = statement.tokens.iter().map(crate::sql::Token::upper).collect();
    let word = |k: usize| words.get(k).map(String::as_str).unwrap_or_default();
    let head: Vec<&str> = (0..6).map(word).collect();
    if head[..3] == ["REFRESH", "MATERIALIZED", "VIEW"] {
        return true;
    }
    if head[0] != "CREATE" {
        return false;
    }
    if head.windows(2).any(|w| w == ["MATERIALIZED", "VIEW"]) {
        return true;
    }
    let Some(table) = head.iter().position(|w| *w == "TABLE") else { return false };
    let query = |w: &str| matches!(w, "SELECT" | "WITH" | "VALUES" | "TABLE");
    // The first word in a parenthesis at the top, past any more of them: a column's name, or a
    // query's first word, as MySQL takes `create table t (select …)` with no `as`.
    let opens = |k: usize| (k + 1..words.len()).find(|&n| statement.tokens[n].text != "(").is_some_and(|n| query(word(n)));
    let mut depth = 0usize;
    for (k, t) in statement.tokens.iter().enumerate().skip(table + 1) {
        match t.text.as_str() {
            "(" if t.kind == crate::sql::TokenKind::Symbol && d == Dialect::Mysql && depth == 0 && opens(k) => return true,
            "(" if t.kind == crate::sql::TokenKind::Symbol => depth += 1,
            ")" if t.kind == crate::sql::TokenKind::Symbol => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            _ if word(k) == "SELECT" => return true,
            _ if word(k) == "AS" && opens(k) => return true,
            _ => {}
        }
    }
    false
}

/// Whether statement `k` of a file is a rebuild's own copy of a table's rows, which a database the
/// change is already in needs no more: `insert into t (…) select … from s`, nothing after, where the
/// file makes `t` before it and drops `s` after it (SQLite's copy into the table that takes the old
/// one's name), and SQLite's AUTOINCREMENT counter of a table the file makes.
fn copies_a_rebuild(statements: &[crate::sql::Statement], k: usize) -> bool {
    use crate::sql::{Token, TokenKind};
    let part = |t: &Token| match t.kind {
        TokenKind::Quoted => t.text[1..t.text.len() - 1].replace("\"\"", "\"").replace("``", "`"),
        _ => t.text.to_lowercase(),
    };
    // The dotted name at `at`, and the index after it.
    let name_at = |tokens: &[Token], mut at: usize| -> Option<(String, usize)> {
        let mut parts = vec![part(tokens.get(at).filter(|t| matches!(t.kind, TokenKind::Word | TokenKind::Quoted))?)];
        while tokens.get(at + 1).is_some_and(|t| t.text == ".") && tokens.get(at + 2).is_some_and(|t| matches!(t.kind, TokenKind::Word | TokenKind::Quoted)) {
            parts.push(part(&tokens[at + 2]));
            at += 2;
        }
        Some((parts.join("."), at + 1))
    };
    // The table a statement makes or drops: `create [temp[orary]] table [if not exists] <name>`, `drop table [if exists] <name>`.
    let table_of = |s: &crate::sql::Statement, verb: &str| -> Option<String> {
        let t = &s.tokens;
        if t.first().map(Token::upper).as_deref() != Some(verb) {
            return None;
        }
        let at = t.iter().take(4).position(|x| x.upper() == "TABLE")? + 1;
        let at = if t.get(at).map(Token::upper).as_deref() == Some("IF") { at + if verb == "CREATE" { 3 } else { 2 } } else { at };
        name_at(t, at).map(|(n, _)| n)
    };
    let made_before = |name: &str| statements[..k].iter().any(|s| table_of(s, "CREATE").as_deref() == Some(name));
    let dropped_after = |name: &str| statements[k + 1..].iter().any(|s| table_of(s, "DROP").as_deref() == Some(name));
    let t = &statements[k].tokens;
    let words: Vec<String> = t.iter().take(3).map(|x| x.upper()).collect();
    if words.len() < 2 || (words[0] != "INSERT" && words[0] != "DELETE") {
        return false;
    }
    let Some((target, mut at)) = name_at(t, 2) else { return false };
    if target == "sqlite_sequence" {
        // The counter of the table the file makes, named as a string.
        return t.iter().filter(|x| x.kind == TokenKind::Str).any(|x| made_before(&x.text[1..x.text.len() - 1].replace("''", "'")));
    }
    if words[0] != "INSERT" || !made_before(&target) {
        return false;
    }
    // The columns the insert names.
    let mut listed: &[Token] = &[];
    if t.get(at).is_some_and(|x| x.text == "(") {
        let close = t[at..].iter().position(|x| x.text == ")").map_or(t.len(), |p| at + p);
        listed = &t[at + 1..close];
        at = close + 1;
    }
    if t.get(at).map(Token::upper).as_deref() != Some("SELECT") {
        return false;
    }
    let Some(from) = t[at..].iter().position(|x| x.upper() == "FROM").map(|p| at + p) else { return false };
    // Columns as they are, by name: a copy that computes a value (`lower(body)`, `current_timestamp`,
    // `null`) writes rows. A word that is a value is no column's name, unless quoted, or named bare in
    // the insert's own list too, as SQLite's rebuild writes a column it reads as `true`.
    const VALUES: [&str; 19] = ["NULL", "DEFAULT", "TRUE", "FALSE", "CURRENT_DATE", "CURRENT_TIME", "CURRENT_TIMESTAMP", "LOCALTIME", "LOCALTIMESTAMP", "UTC_DATE", "UTC_TIME", "UTC_TIMESTAMP", "CURRENT_USER", "SESSION_USER", "SYSTEM_USER", "USER", "CURRENT_ROLE", "CURRENT_CATALOG", "CURRENT_SCHEMA"];
    let in_list = |x: &Token| listed.iter().any(|l| l.kind == TokenKind::Word && l.text.eq_ignore_ascii_case(&x.text));
    let column = |x: &Token| x.kind == TokenKind::Quoted || (x.kind == TokenKind::Word && (!VALUES.contains(&x.upper().as_str()) || in_list(x)));
    let picked = &t[at + 1..from];
    let bare = !picked.is_empty() && picked.iter().enumerate().all(|(k, x)| if k % 2 == 0 { column(x) } else { x.text == "," }) && picked.len() % 2 == 1;
    bare && matches!(name_at(t, from + 1), Some((source, end)) if end == t.len() && dropped_after(&source))
}

/// The lines of `old` and `new` as a unified diff without its hunk headers: unchanged lines more
/// than one away from a change are left out.
fn line_diff(old: &str, new: &str) -> Vec<String> {
    let a: Vec<&str> = old.trim_end().lines().collect();
    let b: Vec<&str> = new.trim_end().lines().collect();
    let (n, m) = (a.len(), b.len());
    if n.saturating_mul(m) > 4_000_000 {
        return vec!["    (the texts differ; too long to show line by line)".into()];
    }
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let mut ops: Vec<(char, &str)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            ops.push((' ', a[i]));
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
            ops.push(('-', a[i]));
            i += 1;
        } else {
            ops.push(('+', b[j]));
            j += 1;
        }
    }
    let changed = |k: usize| ops.get(k).is_some_and(|(op, _)| *op != ' ');
    let mut out = vec![paint("dim", "    --- as applied"), paint("dim", "    +++ on disk")];
    let mut skipped = false;
    for (k, (op, text)) in ops.iter().enumerate() {
        if *op == ' ' && !(k > 0 && changed(k - 1)) && !changed(k + 1) {
            if !skipped {
                out.push(paint("dim", "    …"));
                skipped = true;
            }
            continue;
        }
        skipped = false;
        let line = format!("    {op} {text}");
        out.push(match op {
            '-' => paint("red", &line),
            '+' => paint("green", &line),
            _ => line,
        });
    }
    out
}

/// How an entry of the lock stands against the migration files.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Link {
    /// Its file, chained to the entry before it.
    Ok,
    /// No file of its name.
    Missing,
    /// Its file's text isn't what was locked.
    Edited,
    /// Locked after another entry than the one before it (`after`): two branches' lines merged.
    Moved { after: Option<String> },
}

/// Each entry's link, in the lock's order.
fn links(l: &lock::Lock, files: &[Migration]) -> Vec<Link> {
    let mut out = Vec::new();
    for (k, e) in l.entries.iter().enumerate() {
        let Some(f) = files.iter().find(|f| f.name == e.name) else {
            out.push(Link::Missing);
            continue;
        };
        let text = normalized(&f.sql);
        let prev = if k == 0 { String::new() } else { l.entries[k - 1].sum.clone() };
        if lock::checksum(&prev, &e.name, &text) == e.sum {
            out.push(Link::Ok);
            continue;
        }
        // Chained after an earlier entry, or as the first: moved there from another branch's lock.
        let parents = std::iter::once((None, String::new())).chain(l.entries[..k.saturating_sub(1)].iter().map(|p| (Some(p.name.clone()), p.sum.clone())));
        match parents.filter(|(_, sum)| *sum != prev).find(|(_, sum)| lock::checksum(sum, &e.name, &text) == e.sum) {
            Some((after, _)) => out.push(Link::Moved { after }),
            None => out.push(Link::Edited),
        }
    }
    out
}

/// The project's lock, or what keeps it from being read. A project with no migrations
/// needs none yet.
fn read_lock(opts: &Options) -> Result<lock::Lock, Problem> {
    let path = shown(&opts.lock);
    match opts.lock_state() {
        lock::State::Lock(l) => Ok(l),
        // No lock: the history isn't linear, and the migrations run in name order.
        lock::State::Missing => Ok(lock::Lock::default()),
        lock::State::Conflicted => Err(Problem::new("lock-conflict", "The lock has a merge conflict: another branch added migrations too".to_string())
            .note("Keep main's lines and delete this branch's (git checkout --theirs while merging main in, --ours while rebasing).")
            .fix("Then run this branch's migrations after main's, which locks them again", &["reylo migrate dev"])),
        lock::State::Invalid(e) => Err(Problem::new("lock-unreadable", format!("Reylo can't read {path} as a lock")).note(unreadable_why(&e)).fix("Write it again from what a database applied", &["reylo migrate history pull"])),
        lock::State::Unreadable(why) => Err(unreadable_lock_problem(opts, &why)),
        lock::State::Newer(version) => Err(newer_lock_problem(opts, version)),
    }
}

/// The refusal of a lock a newer Reylo wrote: read by this one's rules, every checksum in it would
/// look edited, and written again, it would lose what the newer version keeps.
fn newer_lock_problem(opts: &Options, version: u64) -> Problem {
    Problem::new("lock-newer", format!("A newer Reylo wrote {}", shown(&opts.lock)))
        .note(format!("It is a lock of version {version}, and Reylo {} reads version {} and older.", env!("CARGO_PKG_VERSION"), lock::VERSION))
        .note("Upgrade Reylo to read it.")
}

/// The refusal of a lock that is there and can't be read: read as none, it would turn its checksums
/// off and deploy its drafts.
fn unreadable_lock_problem(opts: &Options, why: &str) -> Problem {
    Problem::new("lock-unreadable", format!("Reylo can't read {}: {why}", shown(&opts.lock))).note("Read as no lock, it would run the migrations in name order, drafts and all, with no checksum to hold them to.")
}

/// Why a lock doesn't read, as a person can act on it: the line the YAML parser stopped at.
fn unreadable_why(error: &str) -> String {
    static LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"line (\d+)").expect("a valid pattern"));
    match LINE.captures(error) {
        Some(c) => format!("Its line {} doesn't parse.", &c[1]),
        None => format!("It doesn't parse: {error}."),
    }
}

/// The problems of the lock against the migration files, for a command that runs
/// what it lists as it is (deploy, its dry run): an entry with no file, a file edited after it was locked,
/// lines merged out of their order, and drafts.
fn lock_problems(opts: &Options, files: &[Migration]) -> Vec<Problem> {
    if lock_free(opts) {
        return vec![];
    }
    let l = match read_lock(opts) {
        Ok(l) => l,
        Err(p) => return vec![p],
    };
    let mut problems = Vec::new();
    for (e, link) in l.entries.iter().zip(links(&l, files)) {
        match link {
            Link::Ok => {}
            Link::Missing => problems.push(Problem::new("lock-lists-missing-file", format!("The lock lists {}, which isn't in {}", e.name, shown(&opts.dir))).fix("Write it back from a database that ran it", &["reylo migrate history pull"])),
            Link::Edited => problems.push(
                Problem::new("locked-migration-edited", format!("Migration {} changed after it was locked", e.name))
                    .note("A database that ran the old text refuses it: if one did, put the change in a new migration instead.")
                    .fix("If no database ran the old text (it failed everywhere), lock it as it now reads", &["reylo migrate dev"])
                    .fix("To take back the text a database ran", &["reylo migrate history pull"]),
            ),
            Link::Moved { after } => problems.push(
                Problem::new("lock-out-of-order", format!("The lock was merged out of order at {}", e.name))
                    .note(format!("It was locked {}. Keep main's lines, in main's order, and delete this branch's.", after.map_or("first".to_string(), |a| format!("right after {a}"))))
                    .fix("Then run this branch's migrations after main's, which locks them again", &["reylo migrate dev"]),
            ),
        }
    }
    let drafts: Vec<&str> = files.iter().filter(|f| !l.entries.iter().any(|e| e.name == f.name)).map(|f| f.name.as_str()).collect();
    if !drafts.is_empty() {
        let one = drafts.len() == 1;
        let title = if one { format!("The lock doesn't list {} yet", drafts[0]) } else { format!("The lock doesn't list {} migrations yet", drafts.len()) };
        let listed: Vec<String> = if one { vec![] } else { drafts.iter().map(|d| format!("    {d}")).collect() };
        let mut problem = Problem::new("migration-draft", title).lines(listed).fix(format!("Run {} in development, which locks {}", if one { "it" } else { "them" }, if one { "it" } else { "them" }), &["reylo migrate dev"]);
        problem.drafts = true;
        problems.push(problem);
    }
    problems
}

/// The lock under its name before 0.8.0, beside the lock or in its place: never read, and refused by
/// every command that reads or writes the lock. A build holds no such file: `reylo::migrate!()`
/// refuses it when it compiles.
fn old_lock(opts: &Options) -> Option<Problem> {
    let old = opts.lock.with_file_name(lock::OLD_LOCK);
    if opts.embedded.is_some() || !old.exists() {
        return None;
    }
    let (was, is) = (shown(&old), shown(&opts.lock));
    Some(if opts.lock.exists() {
        Problem::new("lock-old-name", format!("Both {was} and {is} are here"))
            .note(format!("{is} is the lock, and {was} its name before 0.8.0, which nothing reads."))
            .fix(format!("Keep {is} and delete {was}"), &[&format!("git rm {was}")])
    } else {
        Problem::new("lock-old-name", format!("The lock's old name is {was}: it is {is} now"))
            .note("Nothing reads it under that name, and read as no lock, it would run the migrations in name order.")
            .fix("Rename it", &[&format!("git mv {was} {is}")])
    })
}

/// The refusal of a project that still holds the lock under its old name ([`old_lock`]), or whose
/// lock is there and can't be read, or was written by a newer Reylo: every command that reads or
/// writes the lock makes it first, so that none writes over what it can't read.
fn refuse_lock_file(opts: &Options, log: Log<'_>) -> Option<i32> {
    let unreadable = || match opts.lock_state() {
        lock::State::Unreadable(why) => Some(unreadable_lock_problem(opts, &why)),
        lock::State::Newer(version) => Some(newer_lock_problem(opts, version)),
        _ => None,
    };
    old_lock(opts).or_else(unreadable).map(|p| refuse(&[p], log))
}

/// Whether the project keeps no lock: its history isn't linear, and its migrations run in name order.
fn lock_free(opts: &Options) -> bool {
    opts.lock_state() == lock::State::Missing
}

/// Where a project with no lock has migrations that sort before one this database already ran: their
/// replay after what it ran, and the replay of every migration in name order, as a fresh database runs
/// them, must leave the same schema (tables' columns compared as a set, as everywhere). None where
/// there are none, or they do.
#[cfg(feature = "cli")]
async fn late_problem(shadows: &mut Shadows<'_>, dir: &Path, ran: &[Migration], pending: &[Migration], all: &[Migration]) -> Result<Option<Problem>, Error> {
    let Some(last) = ran.iter().map(|m| name_order(&m.name)).max() else { return Ok(None) };
    let late: Vec<&Migration> = pending.iter().filter(|m| name_order(&m.name) < last).collect();
    // A database that already ran its migrations out of name order (as a lock, since deleted, had
    // them) is held to name order too, pending or not.
    let in_order = ran.windows(2).all(|w| name_order(&w[0].name) <= name_order(&w[1].name));
    let Some(first) = late.first() else {
        if in_order {
            return Ok(None);
        }
        let here = shadows.lane();
        shadows.push(here, ran.iter().chain(pending).map(Step::of));
        let fresh = shadows.lane();
        let mut by_name = all.to_vec();
        by_name.sort_by(|a, b| name_order(&a.name).cmp(&name_order(&b.name)));
        shadows.push(fresh, by_name.iter().map(Step::of));
        return Ok(match (shadows.lines(here).await?, shadows.lines(fresh).await?) {
            (Ok(a), Ok(b)) if crate::catalog::same_schema(&a, &b) => None,
            (Ok(a), Ok(b)) => Some(
                Problem::new("history-out-of-order", "This database ran its migrations in another order than a fresh database runs them, and the two end differently")
                    .lines(describe_diff("this database", &crate::catalog::without_comments(&a), "a fresh database", &crate::catalog::without_comments(&b)))
                    .note("Elsewhere, write a migration that leaves both with the same schema.")
                    .fix("On a development database, rebuild it in name order", &["reylo db reset"]),
            ),
            (Err((m, e)), _) | (_, Err((m, e))) => Some(Problem::new("replay-fails", format!("Replaying {m} in a shadow database fails")).note(e)),
        });
    };
    let after = ran.iter().filter(|m| name_order(&m.name) > name_order(&first.name)).map(|m| m.name.clone()).collect::<Vec<_>>();
    let here = shadows.lane();
    shadows.push(here, ran.iter().chain(pending).map(Step::of));
    let fresh = shadows.lane();
    let mut by_name = all.to_vec();
    by_name.sort_by(|a, b| name_order(&a.name).cmp(&name_order(&b.name)));
    shadows.push(fresh, by_name.iter().map(Step::of));
    let names = late.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", ");
    let one = late.len() == 1;
    // Each late migration under a name after every migration that isn't pending (what this database
    // ran, and what a run cut off already applied, which may be what it needs), in their order, and
    // before every other pending one, which may need it: the order the replay above ran. Between
    // the two no name is one a file has. Where they leave no room, or cross, no command is printed.
    let done = all.iter().map(|m| m.name.as_str()).filter(|n| !pending.iter().any(|p| p.name == *n));
    let last_ran = done.max_by(|a, b| name_order(a).cmp(&name_order(b))).unwrap_or_default();
    let next = pending.iter().map(|m| m.name.as_str()).filter(|n| !late.iter().any(|l| l.name == *n)).min_by(|a, b| name_order(a).cmp(&name_order(b)));
    let floor = last_ran.split_once('_').and_then(|(at, _)| time::seconds(at)).map_or_else(time::now_seconds, |s| s + 1);
    let renamed: Vec<String> = late
        .iter()
        .enumerate()
        .map(|(k, m)| {
            let rest = match m.name.split_once('_') {
                Some((at, rest)) if time::seconds(at).is_some() => rest,
                _ => m.name.as_str(),
            };
            format!("{}_{rest}", time::stamp(floor + k as i64))
        })
        .collect();
    let fits = renamed.iter().all(|n| name_order(n) > name_order(last_ran) && next.is_none_or(|next| name_order(n) < name_order(next)));
    let folder = shown(dir);
    let moves: Vec<String> = late.iter().zip(&renamed).map(|(m, n)| format!("mv {} {}", shell_word(&format!("{folder}/{}", m.name)), shell_word(&format!("{folder}/{n}")))).collect();
    let lead = format!("Give {} a later name, so that every database runs {} after {}", if one { "it" } else { "them" }, if one { "it" } else { "them" }, after.join(", "));
    let diff = |a: &[String], b: &[String]| describe_diff("this database", &crate::catalog::without_comments(a), "a fresh database", &crate::catalog::without_comments(b));
    let problem = match (shadows.lines(here).await?, shadows.lines(fresh).await?) {
        (Ok(a), Ok(b)) if crate::catalog::same_schema(&a, &b) => return Ok(None),
        (Ok(a), Ok(b)) => Problem::new("late-migration-differs", format!("{} {names} {} before {}, which this database already ran", if one { "Migration" } else { "Migrations" }, if one { "sorts" } else { "sort" }, after.join(", ")))
            .note("Run in name order, as on a fresh database, the migrations end differently:")
            .lines(diff(&a, &b)),
        (Err((m, e)), _) => Problem::new("late-migration-fails", format!("Migration {m} fails when run after what this database already ran")).note(e),
        (_, Err((m, e))) => Problem::new("late-migration-fails", format!("Migration {m} fails when the migrations run in name order, as a fresh database runs them")).note(e),
    };
    if !fits {
        let it = if one { "it" } else { "them" };
        let names = if one { "a name that sorts" } else { "names that sort" };
        let note = match next {
            Some(next) if name_order(next) <= name_order(last_ran) => format!("No name sorts both after {last_ran}, which a run cut off already applied, and before {next}, which is pending: put these migrations in the order they need by hand."),
            Some(next) => format!("Give {it} {names} after {last_ran} and before {next}, so that every database runs {it} after {}.", after.join(", ")),
            None => format!("Give {it} {names} after {last_ran}, so that every database runs {it} after {}.", after.join(", ")),
        };
        return Ok(Some(problem.note(note)));
    }
    let commands: Vec<&str> = moves.iter().map(String::as_str).collect();
    Ok(Some(problem.fix(lead, &commands)))
}

/// The refusal of a database that applied its migrations in another order than the lock lists, `order`
/// being the lock's (`run_order`). Every database runs the lock's order, and nothing else; with no
/// lock, a late one is proven at deploy, so there is none. Where the lock, its chain intact, runs a
/// migration this database hasn't run ahead of all it ran since (`history push` moved it there, as
/// another database ran it, or a merge kept main's lines), a database development commands don't
/// run on takes its change too and records both: `history push` records them only where they make
/// what the lock's order makes. Where the one ahead writes rows, which no schema shows ran, that way
/// isn't offered.
fn lock_order_problem(opts: &Options, applied: &[&HistoryRow], order: &[Migration], d: Dialect) -> Option<Problem> {
    if lock_free(opts) {
        return None;
    }
    let k = applied.iter().zip(order).position(|(r, f)| r.name != f.name)?;
    let problem = Problem::new("history-out-of-order", format!("This database applied {} where {} lists {}", applied[k].name, shown(&opts.lock), order[k].name));
    let (ahead, since) = (&order[k], &applied[k..]);
    let moved = !applied.iter().any(|r| r.name == ahead.name) && since.len() < order.len() - k && since.iter().zip(&order[k + 1..]).all(|(r, f)| r.name == f.name);
    // One that ran migrations the lock lists, in another order (the lock moved one after others, as
    // `history push` moves one a database skipped): where it ran every one the lock lists up to the
    // last it ran, it takes the lock's order, which `history push` records where the two end alike;
    // where it lacks some of them that write no rows, it makes their change too, then records them.
    let reach = applied.iter().map(|r| order.iter().position(|f| f.name == r.name)).collect::<Option<Vec<usize>>>().and_then(|at| at.into_iter().max());
    let lacking: Option<Vec<&Migration>> = reach.map(|p| order[..=p].iter().filter(|f| !applied.iter().any(|r| r.name == f.name)).collect());
    if lacking.as_ref().is_some_and(Vec::is_empty) {
        return Some(
            problem
                .note("Every database runs the migrations in the lock's order, and this one ran the same migrations in another.")
                .fix("On a development database, record the lock's order, running nothing", &["reylo migrate history push"])
                .fix("On any other, do the same", &["reylo migrate history push --url <url>"]),
        );
    }
    // Their files may not run as written after what it ran since (a column they change renamed), and
    // deploy replays nothing to tell: the change is made by hand, and `history push` checks it.
    if let Some(lacking) = lacking.filter(|l| !moved && l.iter().all(|m| writes_rows(&m.sql, d).is_empty())) {
        let names: Vec<String> = lacking.iter().map(|m| m.name.clone()).collect();
        let one = names.len() == 1;
        return Some(
            problem
                .note(format!("Every database runs the migrations in the lock's order, and this one ran them in another, without {}.", and_list(&names)))
                .fix("On a development database, apply them in the lock's order", &["reylo migrate dev"])
                .fix(format!("On any other, make the change {} {} there by hand, then record them all, running nothing", and_list(&names), if one { "makes" } else { "make" }), &["reylo migrate history push --url <url>"]),
        );
    }
    if !moved || !writes_rows(&ahead.sql, d).is_empty() {
        return Some(
            problem
                .note("Every database runs the migrations in the lock's order. Where one ran them otherwise, the lock was merged keeping a branch's lines: keep main's.")
                .fix("On a development database, apply them in the lock's order", &["reylo migrate dev"]),
        );
    }
    let ran = and_list(&since.iter().map(|r| r.name.clone()).collect::<Vec<_>>());
    let problem = problem.note(format!("Every database runs the migrations in the lock's order, and this one ran {ran} without {}, which the lock runs first.", ahead.name)).fix("On a development database, apply them in the lock's order", &["reylo migrate dev"]);
    let record = "reylo migrate history push --url <url>";
    if runs_as_a_migration(&ahead.sql, d) {
        let file = shell_word(&format!("{}/{}", shown(&opts.dir), ahead.name));
        Some(problem.fix("On any other, make its change there too, then record both, running nothing", &[&format!("reylo run --url <url> --file {file}"), record]))
    } else {
        Some(problem.fix(format!("On any other, make its change there by hand, as {} makes it, then record both, running nothing", ahead.name), &[record]))
    }
}

/// Where a migration falls in name order: init.sql, which holds the schema a project started
/// from, first; then the rest by name, which starts with the time each was written.
fn name_order(name: &str) -> (bool, &str) {
    (name != INIT, name)
}

/// The order the migrations run in: the lock's, then its drafts by name.
fn run_order(opts: &Options, files: &[Migration]) -> Vec<Migration> {
    let Ok(l) = read_lock(opts) else { return files.to_vec() };
    let mut out: Vec<Migration> = l.entries.iter().filter_map(|e| files.iter().find(|f| f.name == e.name).cloned()).collect();
    out.extend(files.iter().filter(|f| !l.entries.iter().any(|e| e.name == f.name)).cloned());
    out
}

/// Checks the target against the migration files; changes nothing. With a `server` (`reylo migrate
/// check`) it replays them in shadow databases, against the live schema and schema.sql; with none
/// (`reylo migrate deploy`) it replays nothing, and trusts the fingerprint Reylo's last run stored: a
/// database that no longer has it is refused, and what the fingerprint can't speak to is `--dry-run`'s.
pub async fn check(db: &mut Db, server: Option<&mut Server>, opts: &Options) -> Result<Plan, Error> {
    let d = db.dialect();
    let (files, strays) = opts.migrations()?;
    let mut problems: Vec<Problem> = stray_problem(opts, &files, &strays).into_iter().collect();
    // What schema.sql declares is held against the migrations in development and CI, not at deploy;
    // a project without one keeps its schema in its migrations alone.
    let schema_sql = if server.is_some() { opts.schema_text() } else { None };
    // At deploy, with no shadow, schema.sql is held to the lock's checksum alone (status refuses
    // the same), after whatever else deploy refuses.
    let stamped = (server.is_none() && stamp(opts, opts.schema_text().as_deref()) == Stamp::Changed).then(|| stamp_problem(opts));
    let with_stamp = |mut problems: Vec<Problem>| {
        problems.extend(stamped.clone());
        problems
    };
    let history = read_history(db).await?;
    // A migrations directory that isn't there, on a database that ran migrations, is one refusal: a
    // typo in dir, not every migration taken away.
    if let Some(problem) = missing_dir(opts, &history) {
        return Ok(Plan { problems: vec![problem], ..Plan::default() });
    }
    // A migration this database ran, edited since, is refused as what this database ran, not twice.
    let ran_edited: Vec<String> = history.iter().filter(|r| r.applied && files.iter().any(|f| f.name == r.name && !same_statements(&f.sql, &r.sql, d))).map(|r| r.name.clone()).collect();
    problems.extend(lock_problems(opts, &files).into_iter().filter(|p| !(p.code == "locked-migration-edited" && ran_edited.iter().any(|n| p.title.starts_with(&format!("Migration {n} "))))));
    let files = run_order(opts, &files);
    // A row marked started (a lone statement cut off before its row was finished) counts as pending:
    // whether it landed is read from the live database below, as for any pending migration.
    let applied: Vec<&HistoryRow> = history.iter().filter(|r| r.applied).collect();
    let by_name = |name: &str| files.iter().find(|f| f.name == name);
    let dir = shown(&opts.dir);
    for r in &applied {
        match by_name(&r.name) {
            None => problems.push(missing_problem(&r.name, &r.sql, &dir)),
            // An edit to comments or spacing alone changes nothing that ran.
            Some(f) if !same_statements(&f.sql, &r.sql, d) => problems.push(edited_problem(&r.name, &r.sql, &f.sql)),
            Some(_) => {}
        }
    }
    let applied_files: Vec<Migration> = applied.iter().filter_map(|r| by_name(&r.name).cloned()).collect();
    if problems.is_empty() {
        problems.extend(lock_order_problem(opts, &applied, &files, d));
    }
    // Nothing is recorded without running but what a cut-off run of Reylo's own left (below).
    let records: Vec<Migration> = Vec::new();
    let pending: Vec<Migration> = files.iter().filter(|f| !applied_files.iter().any(|a| a.name == f.name) && !records.contains(f)).cloned().collect();
    // init.sql, which `init` writes, holds the whole schema and runs only on an empty database.
    for p in pending.iter().filter(|f| f.name != INIT) {
        if let Some(why) = atomicity_problem(&p.sql, d) {
            problems.push(Problem::new("not-atomic", format!("Migration {} can't run atomically", p.name)).note(why));
        }
    }
    if server.is_some() {
        problems.extend(replay_problems(opts, &files, schema_sql.as_deref(), d));
        problems.extend(unreadable_grants(db, opts, &files, schema_sql.as_deref()).await?);
    } else {
        // A deploy replays nothing, but runs no migration a shadow can't replay: once it ran, every
        // command that replays the migrations, status and the dry run among them, would refuse it.
        for p in &pending {
            problems.extend(replay_problem(&format!("Migration {}", p.name), &p.sql, d).map(|problem| problem.note("A deploy needs no shadow, but once it ran this, every command that replays the migrations would refuse it.")));
        }
    }
    if !problems.is_empty() {
        return Ok(Plan { problems: with_stamp(problems), ..Plan::default() });
    }

    // An index a cut-off `create index concurrently` left invalid is dropped before its migration
    // runs again, so it is compared as gone.
    let leftovers = leftovers(db, &history).await?;
    let live: Vec<String> = catalog(db).await?.into_iter().filter(|l| !leftovers.iter().any(|x| l.starts_with(&x.line))).collect();
    let stored = applied.last().and_then(|r| r.fingerprint.clone());
    // A run cut off after a migration committed, before it read the schema the migration left: one
    // that left the schema as it is goes on.
    let unread = stored.as_deref().is_some_and(is_pending);
    let resumed = match stored.as_deref() {
        Some(stored) if unread => unchanged_since_cut_off(db, stored).await,
        _ => false,
    };
    if unread && !resumed && server.is_none() {
        let problem = Problem::new("fingerprint-cut-off", format!("A run was cut off after {} applied, before it stored this database's fingerprint", applied.last().map(|r| r.name.as_str()).unwrap_or_default()))
            .note("So nothing shows that its schema is as that migration left it.")
            .fix("See whether it is as its migrations make it", &["reylo migrate deploy --dry-run"])
            .fix("Then store its fingerprint, running nothing", &["reylo migrate history push"]);
        return Ok(Plan { problems: with_stamp(vec![problem]), ..Plan::default() });
    }
    let resumed = resumed.then(|| applied.last().map(|r| r.name.clone())).flatten();
    let unchanged = match stored {
        Some(_) if resumed.is_some() => Some(true),
        Some(_) if unread => Some(false),
        Some(stored) => {
            let now = fingerprint_of(db, &live).await?;
            // Only a fingerprint read the same way says anything: the reader and the server's version.
            let how = |f: &str| f.rsplit_once(':').map(|(how, _)| how.to_string());
            (how(&stored) == how(&now)).then_some(stored == now)
        }
        None => None,
    };
    let Some(server) = server else {
        // No replay: the fingerprint says whether the database is as Reylo's last run left it.
        if unchanged == Some(false) {
            let mut problem = Problem::new("database-changed", CHANGED_OUTSIDE);
            if let Some(stored) = match applied.last() {
                Some(last) => stored_catalog(db, &last.name).await?,
                None => None,
            } {
                problem = problem.lines(std::iter::once(String::new()).chain(changed_lines(&stored, &fingerprinted(&live, db.dialect()), db.dialect())).chain([String::new()]));
            }
            let problem = problem
                .fix("If a migration makes this change, record it here, running nothing", &["reylo migrate history push"])
                .fix("If none does, write one and run it in development, then record it here", &["reylo migrate new <name>", "reylo migrate dev", "reylo migrate history push"])
                .fix("To see which migrations it matches", &["reylo migrate deploy --dry-run"]);
            return Ok(Plan { problems: with_stamp(vec![problem]), ..Plan::default() });
        }
        // A first run is on an empty database: there is no history to say what else this one holds.
        if history.is_empty() && holds_a_schema(&live) {
            let problem = Problem::new("schema-without-history", "This database has a schema, and no migration history")
                .fix("Where the migrations describe it, record them, running nothing", &["reylo migrate history push"])
                .fix("Where they don't, start the project's migrations from it", &["reylo migrate init"]);
            return Ok(Plan { problems: with_stamp(vec![problem]), ..Plan::default() });
        }
        return Ok(Plan { problems: with_stamp(vec![]), pending, unchanged, resumed, leftovers, ran: applied_files.clone(), ..Plan::default() });
    };
    // Without the `cli` feature there is no server to make shadows on.
    #[cfg(not(feature = "cli"))]
    match *server {}
    #[cfg(feature = "cli")]
    {
        let texts: Vec<&str> = files.iter().map(|f| f.sql.as_str()).chain(history.iter().map(|r| r.sql.as_str())).chain(schema_sql.as_deref()).collect();
        let mut shadows = Shadows::new(server, &opts.url, &opts.root, &opts.cache, &texts).await;
        // A lone statement cut off before its row was finished did run, or did not: the database says
        // which, and one that landed is recorded, as the row it started says it ran.
        let started: Vec<String> = history.iter().filter(|r| !r.applied).map(|r| r.name.clone()).collect();
        let mut run = Run { live: &live, url: &opts.url, schema_sql: schema_sql.as_deref(), schema: shown(&opts.schema), applied_files: &applied_files, records, pending, started, landed: vec![], d };
        let result = check_in_shadows(&mut shadows, db, &mut run).await;
        shadows.finish().await;
        let problems = result?;
        Ok(Plan { problems, records: run.records, pending: run.pending, unchanged, resumed, landed: run.landed, leftovers, ran: applied_files.clone() })
    }
}

/// What a check holds the shadows against.
struct Run<'a> {
    live: &'a [String],
    /// The target's URL, which a refusal's commands name it by.
    url: &'a str,
    schema_sql: Option<&'a str>,
    /// schema.sql's path, as a refusal names it.
    schema: String,
    applied_files: &'a [Migration],
    records: Vec<Migration>,
    pending: Vec<Migration>,
    /// Migrations whose row is marked started: a lone statement cut off before its row was finished.
    started: Vec<String>,
    landed: Vec<String>,
    d: Dialect,
}

#[cfg(feature = "cli")]
async fn check_in_shadows(shadows: &mut Shadows<'_>, target: &mut Db, run: &mut Run<'_>) -> Result<Vec<Problem>, Error> {
    let landed: Vec<Migration> = run.pending.iter().filter(|m| run.started.contains(&m.name)).cloned().collect();
    if !landed.is_empty() {
        let lane = shadows.lane();
        shadows.push(lane, run.applied_files.iter().chain(&run.records).chain(&landed).map(Step::of));
        if let Ok(Ok(lines)) = shadows.lines(lane).await {
            if crate::catalog::same_schema(run.live, &lines) {
                run.pending.retain(|m| !landed.contains(m));
                run.landed = landed.iter().map(|m| m.name.clone()).collect();
                run.records.extend(landed);
            }
        }
    }
    let run = &*run;
    // B: the target's history, as the files read now, then compared with the live target. With
    // migrations pending, the target's history is where the next check starts, so it is kept.
    let b_lane = shadows.lane();
    shadows.push(b_lane, run.applied_files.iter().chain(&run.records).map(Step::of));
    if !run.pending.is_empty() {
        shadows.mark(b_lane);
    }
    let checkpoint = match shadows.lines(b_lane).await? {
        Ok(lines) => lines,
        Err((m, error)) => return Ok(vec![Problem::new("replay-fails", format!("Replaying {m} in a shadow database fails")).note(error)]),
    };
    if !crate::catalog::same_schema(run.live, &checkpoint) {
        return drift(shadows, target, run, &checkpoint).await;
    }
    shadows.push(b_lane, run.pending.iter().map(Step::of));
    let b = match shadows.lines(b_lane).await? {
        Ok(lines) => lines,
        Err((m, error)) => return Ok(vec![Problem::new("history-replay-fails", format!("Replaying this database's history fails at {m}")).note(error)]),
    };

    let Some(schema_sql) = run.schema_sql else { return Ok(vec![]) };
    schema_problems(shadows, b_lane, &b, schema_sql, &run.schema).await
}

/// schema.sql held against `b`, what the migrations in `b_lane` make: where they differ, the SQL that
/// takes one to the other.
#[cfg(feature = "cli")]
async fn schema_problems(shadows: &mut Shadows<'_>, b_lane: usize, b: &[String], schema_sql: &str, schema: &str) -> Result<Vec<Problem>, Error> {
    let mut problems = Vec::new();
    let a_lane = shadows.lane();
    shadows.push(a_lane, [Step::script("schema.sql", schema_sql)]);
    let a = match shadows.lines(a_lane).await? {
        Ok(lines) => lines,
        Err((_, error)) => {
            problems.push(Problem::new("schema-sql-fails", format!("On an empty database, {schema} fails")).note(error));
            return Ok(problems);
        }
    };
    if a != b {
        let problem = match sides(&a, b) {
            (true, false) => Problem::new("schema-sql-ahead", "The schema is ahead of the migrations").sql(&lane_plan(shadows, b_lane, a_lane, Some(schema_sql)).await.unwrap_or_default()).fix("To write the changes to a migration file", &["reylo migrate new <name>"]),
            (false, true) => Problem::new("migrations-ahead", format!("The migrations make what {schema} doesn't declare")).sql(&lane_plan(shadows, a_lane, b_lane, None).await.unwrap_or_default()).note(format!("Add it to {schema}, or take it out of the migration that makes it.")),
            _ => Problem::new("schema-sql-disagrees", format!("The migrations and {schema} disagree"))
                .sql(&lane_plan(shadows, b_lane, a_lane, Some(schema_sql)).await.unwrap_or_default())
                .note(format!("Change {schema} or the migrations so that they agree."))
                .fix(format!("Then write what {schema} has and no migration makes"), &["reylo migrate new <name>"]),
        };
        problems.push(problem);
    }
    Ok(problems)
}

/// The refusal for a database that differs from its history: what changed, as SQL from what the
/// history makes to what the database has, and the ways out. Where the next pending migrations,
/// none of which changes data, make exactly the change, recording them is named.
#[cfg(feature = "cli")]
async fn drift(shadows: &mut Shadows<'_>, target: &mut Db, run: &Run<'_>, checkpoint: &[String]) -> Result<Vec<Problem>, Error> {
    // Partitions made at run time are data, and comments aren't compared: no part of what differs.
    let checkpoint = crate::catalog::without_comments(&crate::catalog::without_missing_children(checkpoint, run.live));
    let live = crate::catalog::without_comments(&crate::catalog::without_extra_children(run.live, &checkpoint));
    let (only_history, only_live) = difference(&checkpoint, &live);
    if !run.records.is_empty() {
        let names = run.records.iter().map(|m| m.name.as_str()).collect::<Vec<_>>().join(", ");
        let without = shadows.lane();
        shadows.push(without, run.applied_files.iter().map(Step::of));
        if let Ok(Ok(lines)) = shadows.lines(without).await {
            if crate::catalog::without_comments(&lines) == live {
                return Ok(vec![Problem::new("cannot-record", format!("This database doesn't have what {names} make, so they can't be recorded without running")).fix("Run them instead", &["reylo migrate deploy"])]);
            }
        }
    }
    let mut hint = Vec::new();
    for j in 1..=run.pending.len().min(3) {
        if !run.pending[..j].iter().all(|m| data_statements(&m.sql, run.d).is_empty()) {
            break;
        }
        let lane = shadows.lane();
        shadows.push(lane, run.applied_files.iter().chain(&run.records).chain(&run.pending[..j]).map(Step::of));
        if let Ok(Ok(lines)) = shadows.lines(lane).await {
            if crate::catalog::without_comments(&lines) == live {
                hint = run.pending[..j].iter().map(|m| m.name.clone()).collect();
                break;
            }
        }
    }
    // A later migration that makes exactly this change, as history push moves it ahead of the rest.
    let mut ahead = None;
    if hint.is_empty() {
        let ran: Vec<Migration> = run.applied_files.iter().chain(&run.records).cloned().collect();
        if let Some(j) = behind(shadows, &ran, &run.pending, &live).await? {
            let (it, rest) = (&run.pending[j], &run.pending[..j]);
            if let Some(why) = unmovable(shadows, &ran, &run.pending, j, run.d).await? {
                return Ok(vec![behind_problem(shadows, target, run.url, &ran, it, rest, &live, Some(why)).await]);
            }
            ahead = Some((it.name.clone(), names_of(rest)));
        }
    }
    let prefix = shadows.lane();
    shadows.push(prefix, run.applied_files.iter().chain(&run.records).map(Step::of));
    let mut sql = String::new();
    if let Ok(Ok(())) = shadows.open(prefix).await {
        shadows.taint(prefix);
        sql = plan_text(shadows.db(prefix), target, None).await.unwrap_or_default();
    }
    let mut problem = Problem::new("database-drift", CHANGED_OUTSIDE);
    problem = if sql.is_empty() { problem.lines(std::iter::once(String::new()).chain(changed_lines(&checkpoint, &live, run.d)).chain([String::new()])) } else { problem.sql(&sql) };
    if !hint.is_empty() {
        problem = problem.fix(format!("{} {} exactly this change: record it here, running nothing", hint.join(" and "), if hint.len() == 1 { "makes" } else { "make" }), &["reylo migrate history push"]);
    } else if let Some((it, rest)) = ahead {
        problem = problem.fix(format!("Migration {it} makes exactly this change: record it here, running nothing, and deploy runs {rest} after it"), &["reylo migrate history push"]);
    } else {
        problem = keep_fixes(problem, run.schema_sql.is_some().then_some(run.schema.as_str()), only_live.is_empty(), only_history.is_empty());
    }
    Ok(vec![problem])
}

/// Where the database has, on top of `ran`, exactly what a pending migration other than the next
/// makes: a change made outside the migrations, written since as one that the lock runs after others
/// this database hasn't run. It is recorded where the order can move to run it first, as this
/// database did. The index in `pending` of the newest such, of the last three.
#[cfg(feature = "cli")]
async fn behind(shadows: &mut Shadows<'_>, ran: &[Migration], pending: &[Migration], live: &[String]) -> Result<Option<usize>, Error> {
    for j in (1..pending.len()).rev().take(3) {
        let lane = shadows.lane();
        shadows.push(lane, ran.iter().chain([&pending[j]]).map(Step::of));
        if let Ok(lines) = shadows.lines(lane).await? {
            let made = crate::catalog::without_comments(&lines);
            if crate::catalog::without_extra_children(live, &made) == made {
                return Ok(Some(j));
            }
        }
    }
    Ok(None)
}

/// Why `pending[j]`, which makes all this database has on top of `ran`, can't be recorded there,
/// ahead of `pending[..j]`; None where it can. What writes rows ([`writes_rows`]) is never moved
/// past, nor recorded by its schema alone: no schema shows that its statements ran, or that they run
/// the same on the rows after the change. And the two orders must end at the same schema.
#[cfg(feature = "cli")]
async fn unmovable(shadows: &mut Shadows<'_>, ran: &[Migration], pending: &[Migration], j: usize, d: Dialect) -> Result<Option<String>, Error> {
    let (it, ahead) = (&pending[j], &pending[..j]);
    if !writes_rows(&it.sql, d).is_empty() {
        return Ok(Some("Its data statements can't be told to have run here, so it isn't recorded without running.".into()));
    }
    if !order_sensitive(&it.sql, d).is_empty() {
        return Ok(Some("It fills the rows a table has, which can turn out differently ahead of them, so it isn't recorded without running.".into()));
    }
    let changing: Vec<Migration> = ahead.iter().filter(|m| !order_sensitive(&m.sql, d).is_empty()).cloned().collect();
    if !changing.is_empty() {
        let one = changing.len() == 1;
        return Ok(Some(format!("{} {} {} data, which can run differently after this change, so it can't move ahead of {}.", if one { "Migration" } else { "Migrations" }, names_of(&changing), if one { "changes" } else { "change" }, if one { "it" } else { "them" })));
    }
    let reordered: Vec<Migration> = ran.iter().chain([it]).chain(ahead).chain(&pending[j + 1..]).cloned().collect();
    let original: Vec<Migration> = ran.iter().chain(pending).cloned().collect();
    if !ends_alike(shadows, &original, &reordered, ran.len() + j + 1).await? {
        return Ok(Some(format!("Run ahead of {}, the migrations end differently, so it can't move there.", names_of(ahead))));
    }
    Ok(None)
}

/// Whether two orders of the migrations both replay, and reach the same schema at the end of the
/// first `block` of each, where they hold the same migrations, and at their ends: a migration after
/// the block that fills rows sees the same schema either way, though a later one could make the
/// two ends alike again. Both are replays, so every line is compared, a partition one has and the
/// other lacks included: the next insert lands in it or not.
#[cfg(feature = "cli")]
async fn ends_alike(shadows: &mut Shadows<'_>, a: &[Migration], b: &[Migration], block: usize) -> Result<bool, Error> {
    let (x, y) = (shadows.lane(), shadows.lane());
    for (from, to) in [(0, block), (block, a.len())] {
        shadows.push(x, a[from..to].iter().map(Step::of));
        shadows.push(y, b[from..to].iter().map(Step::of));
        let alike = match (shadows.lines(x).await?, shadows.lines(y).await?) {
            (Ok(p), Ok(q)) => crate::catalog::same(&crate::catalog::without_comments(&p), &crate::catalog::without_comments(&q)),
            _ => false,
        };
        if !alike {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The migrations' names, as a sentence lists them.
#[cfg(feature = "cli")]
fn names_of(migrations: &[Migration]) -> String {
    and_list(&migrations.iter().map(|m| m.name.clone()).collect::<Vec<_>>())
}

/// A run of migrations, as a sentence names it: each by name, and a long one by its ends.
#[cfg(feature = "cli")]
fn run_of(migrations: &[Migration]) -> String {
    match migrations {
        [first, .., last] if migrations.len() > 3 => format!("the {} migrations from {} to {}", migrations.len(), first.name, last.name),
        _ => names_of(migrations),
    }
}

/// Where the database has what `order` makes without one migration, `order[k]`, and with those
/// after it up to `order[m]`: a database that skipped one, as another tool or a hand can leave it.
/// It is at or after `from`, where the history's run ends, and is looked for only among the three
/// whose change accounts for the most of what the database and some later prefix of `order` differ
/// by, as `stages` (the catalog after each prefix) shows, the latest first among equals: a few
/// replays, and never one per migration, which decide. A line can hold what two migrations change
/// (a column's default and its nullability), so a change need not account for all of it. With the
/// longest run after it, but for the migrations at its end that change data and nothing a schema
/// shows, which can't be told to have run.
#[cfg(feature = "cli")]
async fn skipped(shadows: &mut Shadows<'_>, order: &[Migration], stages: &[Vec<String>], from: usize, live: &[String], d: Dialect) -> Result<Option<(usize, usize)>, Error> {
    if stages.len() <= order.len() {
        return Ok(None);
    }
    let apart: Vec<(Vec<String>, Vec<String>)> = stages.iter().map(|stage| difference(stage, &crate::catalog::without_extra_children(live, stage))).collect();
    // How much of the difference at its best later prefix a migration's change accounts for.
    let share = |k: usize| -> f64 {
        let (gone, came) = difference(&stages[k], &stages[k + 1]);
        let (gone, came): (std::collections::BTreeSet<&String>, std::collections::BTreeSet<&String>) = (gone.iter().collect(), came.iter().collect());
        apart[k + 2..]
            .iter()
            .filter(|(only_stage, only_live)| !(only_stage.is_empty() && only_live.is_empty()))
            .map(|(only_stage, only_live)| (only_stage.iter().filter(|l| came.contains(l)).count() + only_live.iter().filter(|l| gone.contains(l)).count()) as f64 / (only_stage.len() + only_live.len()) as f64)
            .fold(0.0, f64::max)
    };
    let mut ranked: Vec<(f64, usize)> = (from..order.len().saturating_sub(1)).map(|k| (share(k), k)).filter(|(s, _)| *s > 0.0).collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)));
    let candidates: Vec<usize> = ranked.into_iter().take(3).map(|(_, k)| k).collect();
    for k in candidates {
        let lane = shadows.lane();
        shadows.push(lane, order[..k].iter().map(Step::of));
        let mut hits: Vec<usize> = Vec::new();
        for m in k + 1..order.len() {
            shadows.push(lane, [Step::of(&order[m])]);
            let Ok(lines) = shadows.lines(lane).await? else { break };
            let made = crate::catalog::without_comments(&lines);
            if crate::catalog::without_extra_children(live, &made) == made {
                hits.push(m + 1);
            }
        }
        if let Some(mut m) = hits.last().copied() {
            while m > k + 2 && hits.contains(&(m - 1)) && !data_statements(&order[m - 1].sql, d).is_empty() {
                m -= 1;
            }
            return Ok(Some((k, m)));
        }
    }
    Ok(None)
}

/// `order` with `order[k]` moved after `order[k + 1..m]`.
#[cfg(feature = "cli")]
fn moved_after(order: &[Migration], k: usize, m: usize) -> Vec<Migration> {
    order[..k].iter().chain(&order[k + 1..m]).chain([&order[k]]).chain(&order[m..]).cloned().collect()
}

/// Why `order[k]`, which a database skipped, can't move after `order[k + 1..m]`, which it ran, and
/// whether its change can be made on that database and both recorded in their order instead; None
/// where it can move. As with [`unmovable`], nothing that writes rows moves past a change, and the
/// two orders must end at the same schema.
#[cfg(feature = "cli")]
async fn unskippable(shadows: &mut Shadows<'_>, order: &[Migration], k: usize, m: usize, d: Dialect) -> Result<Option<(String, bool)>, Error> {
    let (it, past) = (&order[k], &order[k + 1..m]);
    let alike = ends_alike(shadows, order, &moved_after(order, k, m), m).await?;
    let them = if past.len() == 1 { "it" } else { "them" };
    if !order_sensitive(&it.sql, d).is_empty() {
        return Ok(Some((format!("It changes data, which can run differently after {}, so it can't move after {them}.", run_of(past)), false)));
    }
    let changing: Vec<Migration> = past.iter().filter(|m| !order_sensitive(&m.sql, d).is_empty()).cloned().collect();
    if !changing.is_empty() {
        let one = changing.len() == 1;
        return Ok(Some((format!("{} {} {} data, which can run differently without it, so it can't move after {them}.", if one { "Migration" } else { "Migrations" }, names_of(&changing), if one { "changes" } else { "change" }), alike)));
    }
    if !alike {
        return Ok(Some((format!("Run after {}, the migrations end differently, so it can't move there.", run_of(past)), false)));
    }
    Ok(None)
}

/// The refusal of a database that has what the migrations make without `it`, after `past`, where
/// it can't move after them (`why`): what `past` wrote or stored ran without `it`, so making its
/// change now leaves those as they were, and no command can say what they would be. They are
/// redone by hand, and the migrations recorded in their order.
#[cfg(feature = "cli")]
fn skipped_problem(it: &Migration, past: &[Migration], why: &str, d: Dialect) -> Problem {
    let changing: Vec<Migration> = past.iter().filter(|m| !order_sensitive(&m.sql, d).is_empty()).cloned().collect();
    Problem::new("history-push-skipped", format!("This database has what the migrations make without {}", it.name))
        .note(format!("It ran {} without it. {why}", run_of(past)))
        .fix(format!("Make the change {} makes here, then redo what {} wrote or built as {} would after it, and record them all, running nothing", it.name, names_of(&changing), if changing.len() == 1 { "it" } else { "they" }), &["reylo migrate history push --url <url>"])
}

/// The refusal for [`behind`] where the order can't move: `it` makes the change, after `ahead`,
/// which this database hasn't run, on top of `ran`, what its history says it ran, which is where
/// deploy starts; `why` says why `it` can't run first. The way out keeps the order: the change
/// undone, so that deploy runs `ahead`, then `it`, as every database does. The
/// lead names the database at `url`, as every command shows one, with no credential or option, and
/// both commands take its URL in `--url`, since a command without it reaches the project's own.
/// The undo is a command only where `reylo run` applies it whole or not at all: on Postgres, which
/// runs a script as one transaction, and a lone statement elsewhere. A SQLite rebuild needs the
/// foreign keys off that a migration runs with.
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "cli")]
async fn behind_problem(shadows: &mut Shadows<'_>, target: &mut Db, url: &str, ran: &[Migration], it: &Migration, ahead: &[Migration], live: &[String], why: Option<String>) -> Problem {
    let names = names_of(ahead);
    let problem = Problem::new("later-migration-in-place", format!("Migration {} makes this change, but runs after {names}, which this database hasn't run", it.name));
    let problem = match why {
        Some(why) => problem.note(why),
        None => problem,
    };
    let problem = match ran.last() {
        Some(last) => problem.note(format!("It ran the migrations through {}, and then had this change made outside them:", last.name)),
        None => problem.note("It ran none of the migrations, and then had this change made outside them:"),
    };
    let lane = shadows.lane();
    shadows.push(lane, ran.iter().map(Step::of));
    let made = match shadows.lines(lane).await {
        Ok(Ok(lines)) => crate::catalog::without_comments(&lines),
        _ => vec![],
    };
    let (mut change, mut sql) = (None, None);
    if let Ok(Ok(())) = shadows.open(lane).await {
        shadows.taint(lane);
        change = scoped_change(target, shadows.db(lane), &[]).await;
        if change.is_none() {
            sql = plan_text(shadows.db(lane), target, None).await;
        }
    }
    let (sql, undo, lost) = match change {
        Some((sql, undo, lost)) => (Some(sql), Some(undo), lost),
        None => (sql, None, vec![]),
    };
    let problem = match sql {
        Some(sql) => problem.sql(&sql),
        None => problem.lines(std::iter::once(String::new()).chain(changed_lines(&made, live, target.dialect())).chain([String::new()])),
    };
    let problem = if lost.is_empty() { problem } else { problem.note(format!("Undoing it loses what {} {}.", and_list(&lost), if lost.len() == 1 { "holds" } else { "hold" })) };
    // What `it` writes may have been written here too, with the change: the undo can't take it back,
    // and deploy writes it again.
    let writes = writes_rows(&it.sql, target.dialect());
    let problem = if writes.is_empty() { problem } else { problem.note(format!("The undo reverses the schema alone: where {} also ran here, reverse what {} wrote first, or deploy writes it again.", and_list(&writes), if writes.len() == 1 { "it" } else { "they" })) };
    let order = format!("{names} first, and then {}", it.name);
    let on = shown_url(url);
    let deploy = "reylo migrate deploy --url <url>";
    let d = target.dialect();
    match undo {
        Some(undo) if runs_as_a_migration(&undo.join("\n"), d) => problem.note("Give both commands the same URL as this one.").fix(format!("Undo it on {on}, so that deploy runs {order}"), &[format!("reylo run --url <url> {}", shell_word(&undo.join(" "))).as_str(), deploy]),
        // An undo reylo run wouldn't run as a migration does is no script to hand over either: on
        // SQLite a rebuild copied as written has none of a migration's safeguards (its transaction,
        // foreign keys off and checked before it commits). The change it reverses is shown above.
        _ => problem.note(format!("Undo it on {on} by hand, then give deploy the same URL as this command.")).fix(format!("Then deploy, which runs {order}"), &[deploy]),
    }
}

/// The ways to keep a change made by hand on a database no development command runs on: written as
/// a migration in development, then recorded on it. `removal` and `addition` say what the change
/// is, for what to do to schema.sql, where the project has one.
fn keep_fixes(problem: Problem, schema: Option<&str>, removal: bool, addition: bool) -> Problem {
    let keep = match schema {
        Some(schema) if addition => format!("To keep it, add it to {schema} and write its migration in development"),
        Some(schema) if removal => format!("To keep it, take it out of {schema} and write its migration in development"),
        Some(schema) => format!("To keep it, make {schema} match and write its migration in development"),
        None => "To keep it, make the same change in development and write its migration there".to_string(),
    };
    problem.fix(keep, &["reylo migrate new <name>", "reylo migrate dev"]).fix("Then record it here, running nothing", &["reylo migrate history push"])
}

/// The SQL that takes the schema of `from` to that of `to`, as the diff engine plans it, written as
/// `create` writes it (from `declared`, the text that built `to`, where there is one); None where it
/// plans nothing for this database or finds no difference.
#[cfg(feature = "cli")]
async fn plan_text(from: &mut Db, to: &mut Db, declared: Option<&str>) -> Option<String> {
    let mut target = crate::diff::read(to).await.ok()?;
    if let (Some(sql), Dialect::Postgres) = (declared, to.dialect()) {
        crate::diff::postgres::declare(to, &mut target, sql).await.ok()?;
    }
    let current = crate::diff::read(from).await.ok()?;
    // Partitions the database made at run time are data, not a difference.
    crate::diff::without_extra_children(&mut target, &current);
    let alterer = crate::diff::alterer(from).await.ok()?;
    let mut plan = crate::diff::plan(&current, &target, alterer.as_ref());
    style(from, &mut plan, &target).await.ok()?;
    (!plan.statements.is_empty()).then(|| plan.sql())
}

/// The plan written as `schema.sql` is: on Postgres, in lowercase, with the names in the first
/// schema of the search_path of `db`, the database it runs on, unqualified, in its statements and
/// in the comments that name what it loses or renames.
#[cfg(feature = "cli")]
async fn style(db: &mut Db, plan: &mut crate::diff::Plan, target: &crate::diff::Schema) -> Result<(), Error> {
    if db.dialect() != Dialect::Postgres {
        return Ok(());
    }
    crate::diff::postgres::style(db, plan, &target.verbatim).await
}

/// `plan_text` between two lanes' databases, which the reads leave as no replay to keep.
#[cfg(feature = "cli")]
async fn lane_plan(shadows: &mut Shadows<'_>, from: usize, to: usize, declared: Option<&str>) -> Option<String> {
    for lane in [from, to] {
        if !matches!(shadows.open(lane).await, Ok(Ok(()))) {
            return None;
        }
        shadows.taint(lane);
    }
    let mut target = crate::diff::read(shadows.db(to)).await.ok()?;
    if let (Some(sql), Dialect::Postgres) = (declared, shadows.db(to).dialect()) {
        crate::diff::postgres::declare(shadows.db(to), &mut target, sql).await.ok()?;
    }
    let current = crate::diff::read(shadows.db(from)).await.ok()?;
    let alterer = crate::diff::alterer(shadows.db(from)).await.ok()?;
    let mut plan = crate::diff::plan(&current, &target, alterer.as_ref());
    style(shadows.db(from), &mut plan, &target).await.ok()?;
    (!plan.statements.is_empty()).then(|| plan.sql())
}

// ---------------------------------------------------------------------------------------------
// The commands.

/// Whether the project's own SQL sets default privileges, so that shadows must not take the target's.
fn sets_default_privileges(opts: &Options) -> bool {
    static PRIVILEGES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\balter\s+default\s+privileges\b").expect("a valid pattern"));
    let schema = opts.schema_text().unwrap_or_default();
    PRIVILEGES.is_match(&schema) || opts.migrations().map(|(files, _)| files.iter().any(|f| PRIVILEGES.is_match(&f.sql))).unwrap_or(false)
}

/// The extensions the project's own files create, which its shadows leave to them.
fn created_extensions(opts: &Options) -> Vec<String> {
    let mut texts = vec![opts.schema_text().unwrap_or_default()];
    texts.extend(opts.migrations().map(|(files, _)| files.into_iter().map(|f| f.sql).collect::<Vec<_>>()).unwrap_or_default());
    texts.iter().flat_map(|t| extensions_in(t)).collect()
}

/// The schemas the project's own files create.
fn created_schemas(opts: &Options) -> Vec<String> {
    static SCHEMA: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)\bcreate\s+schema\s+(?:if\s+not\s+exists\s+)?"?([A-Za-z0-9_$-]+)"?"#).expect("a valid pattern"));
    let mut texts = vec![opts.schema_text().unwrap_or_default()];
    texts.extend(opts.migrations().map(|(files, _)| files.into_iter().map(|f| f.sql).collect::<Vec<_>>()).unwrap_or_default());
    texts
        .iter()
        .flat_map(|t| {
            let uncommented = crate::sql::tokenize(t, Dialect::Postgres).iter().map(|token| token.text.clone()).collect::<Vec<_>>().join(" ");
            SCHEMA.captures_iter(&uncommented).map(|c| c[1].to_string()).filter(|s| !s.eq_ignore_ascii_case("authorization")).collect::<Vec<_>>()
        })
        .collect()
}

/// The extensions a text creates.
fn extensions_in(text: &str) -> Vec<String> {
    static EXTENSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)\bcreate\s+extension\s+(?:if\s+not\s+exists\s+)?"?([A-Za-z0-9_-]+)"?"#).expect("a valid pattern"));
    // What a comment says creates nothing: matched in the text with comments taken out.
    let uncommented = crate::sql::tokenize(text, Dialect::Postgres).iter().map(|token| token.text.clone()).collect::<Vec<_>>().join(" ");
    EXTENSION.captures_iter(&uncommented).map(|c| c[1].to_lowercase()).collect()
}

/// The target and its shadows' server, or the refusal of a target whose relations of the project's
/// are out of the role's reach ([`out_of_reach`]), as the code to exit with.
#[cfg(feature = "cli")]
async fn open(opts: &Options, log: Log<'_>) -> Result<Result<(Db, Server, usize), i32>, Error> {
    let mut db = Db::open(opts).await?;
    match out_of_reach(&mut db, &opts.url).await {
        Ok(problems) if problems.is_empty() => Ok(Ok(with_shadows(db, opts).await?)),
        Ok(problems) => {
            db.close().await;
            Ok(Err(refuse(&problems, log)))
        }
        Err(e) => {
            db.close().await;
            Err(e)
        }
    }
}

/// The refusal of the relations in a schema of the project's that a superuser the connected role may
/// not act as owns, one per owner ([`crate::catalog::out_of_reach_relations`]): they are the project's,
/// which every comparison reads, but no migration the role runs may change them and no shadow it makes
/// owns them as they are owned here, so a command that replays or plans refuses them rather than
/// leaving them out. The fix gives them to the role, run as their owner.
#[cfg(feature = "cli")]
async fn out_of_reach(db: &mut Db, url: &str) -> Result<Vec<Problem>, Error> {
    let relations = crate::catalog::out_of_reach_relations(db).await?;
    if relations.is_empty() {
        return Ok(vec![]);
    }
    let me = db.rows("select current_user::text as u, quote_ident(current_user) as q").await?;
    let (role, quoted) = me.first().map(|r| (r.text("u").to_string(), r.text("q").to_string())).unwrap_or_default();
    let mut owners: Vec<&str> = relations.iter().map(|(o, ..)| o.as_str()).collect();
    owners.dedup();
    Ok(owners
        .into_iter()
        .map(|owner| {
            let theirs: Vec<&(String, String, String, String)> = relations.iter().filter(|(o, ..)| o == owner).collect();
            let one = theirs.len() == 1;
            let kinds: Vec<String> = ["table", "view", "materialized view", "sequence", "foreign table"]
                .into_iter()
                .filter(|k| theirs.iter().any(|(_, kind, ..)| kind == k))
                .map(|k| if one { format!("a {k}") } else { format!("{k}s") })
                .collect();
            let names: Vec<&str> = theirs.iter().map(|(_, _, name, _)| name.as_str()).collect();
            // Each named with its schema, `public` too: the statements run as the owner, whose
            // `search_path` (`"$user", public`) may find another relation of the name first.
            let statements: Vec<String> = theirs.iter().map(|(_, kind, _, qualified)| shell_word(&format!("alter {kind} {qualified} owner to {quoted}"))).collect();
            let command = format!("reylo run --url {} {}", shell_word(&as_user(url, owner)), statements.join(" "));
            Problem::new("owner-out-of-reach", format!("{owner} owns {} here that {role} can't act as: {}", and_list(&kinds), names.join(", ")))
                .fix(format!("Make {role} {} owner, connected as {owner}, then start again", if one { "its" } else { "their" }), &[&command])
        })
        .collect())
}

/// `url` with `user` in place of its user, and no password: the one to connect as `user`.
#[cfg(feature = "cli")]
fn as_user(url: &str, user: &str) -> String {
    let (base, query) = url.split_once('?').map_or((url, None), |(b, q)| (b, Some(q)));
    let kept: Vec<&str> = query.into_iter().flat_map(|q| q.split('&')).filter(|kv| !matches!(kv.split('=').next(), Some("user" | "password"))).collect();
    let Some((scheme, rest)) = base.split_once("://") else { return format!("<{user}'s URL>") };
    let end = rest.find('/').unwrap_or(rest.len());
    let host = rest[..end].rsplit_once('@').map_or(&rest[..end], |(_, host)| host);
    let user: String = user.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    let base = format!("{scheme}://{user}@{host}{}", &rest[end..]);
    if kept.is_empty() { base } else { format!("{base}?{}", kept.join("&")) }
}
#[cfg(feature = "cli")]
async fn with_shadows(mut db: Db, opts: &Options) -> Result<(Db, Server, usize), Error> {
    let mut server = shadow_server(&mut db, opts).await?;
    let swept = server.sweep().await?;
    Ok((db, server, swept))
}

/// The server the shadows of `db` are made on, set up as the project's files say.
#[cfg(feature = "cli")]
async fn shadow_server(db: &mut Db, opts: &Options) -> Result<Server, Error> {
    let mut server = Server::open(db, &opts.url, opts.shadow_url.as_deref()).await?;
    server.copy_default_privileges(!sets_default_privileges(opts));
    server.leave_extensions(created_extensions(opts));
    server.leave_schemas(created_schemas(opts));
    Ok(server)
}

/// Deploy's one shadow, in a project with no lock: where a pending migration sorts before one the
/// database ran, the two orders are replayed, and a difference refuses the deploy.
async fn late_on(db: &mut Db, opts: &Options, plan: &Plan) -> Result<Option<Problem>, Error> {
    let last = plan.ran.iter().map(|m| name_order(&m.name)).max();
    if !lock_free(opts) || !plan.pending.iter().any(|m| last.is_some_and(|last| name_order(&m.name) < last)) {
        return Ok(None);
    }
    // A host's connection has no URL to make shadows from, and without the `cli` feature there are
    // none to replay a late migration in: one with no lock is deployed from the command line.
    let unchecked = || Problem::new("late-migration-unchecked", "A migration sorts before one this database already ran, and this project keeps no lock").fix("Deploy it from the command line, which checks it in a shadow database", &["reylo migrate deploy"]);
    #[cfg(not(feature = "cli"))]
    {
        let _ = db;
        Ok(Some(unchecked()))
    }
    #[cfg(feature = "cli")]
    {
        // An app's build may have the feature on, through another crate of its workspace.
        if matches!(db, Db::Host(_)) {
            return Ok(Some(unchecked()));
        }
        let (files, _) = opts.migrations()?;
        // The replay takes every file, those this database ran too, which the check before this
        // one didn't hold to a shadow, and the plan's texts as that check read them.
        let replayed: Vec<Migration> = files.iter().chain(&plan.ran).chain(&plan.pending).cloned().collect();
        if let Some(problem) = replay_problems(opts, &replayed, None, db.dialect()).into_iter().next() {
            return Ok(Some(problem));
        }
        let mut server = shadow_server(db, opts).await?;
        let texts: Vec<&str> = files.iter().map(|f| f.sql.as_str()).collect();
        let mut shadows = Shadows::new(&mut server, &opts.url, &opts.root, &opts.cache, &texts).await;
        let problem = late_problem(&mut shadows, &opts.dir, &plan.ran, &plan.pending, &files).await;
        shadows.finish().await;
        server.close().await;
        problem
    }
}

fn print_problems(problems: &[Problem], log: Log<'_>) {
    let hosted = HOST_RUN.try_with(|run| run.borrow_mut().problems.extend(problems.iter().cloned())).is_ok();
    if !hosted && JSON.load(std::sync::atomic::Ordering::Relaxed) {
        REPORTED.lock().unwrap_or_else(|e| e.into_inner()).extend(problems.iter().cloned());
    }
    for (k, p) in problems.iter().enumerate() {
        if k > 0 {
            log("");
        }
        log(&paint("bold red", &format!("✗ {}", p.title)));
        for line in &p.lines {
            log(line);
        }
    }
}

/// What a run prints: a heading, once there is something to put under it, and one aligned row per
/// migration.
struct Rows {
    heading: String,
    width: usize,
    opened: bool,
}

impl Rows {
    fn new<'a>(heading: String, names: impl Iterator<Item = &'a str>) -> Rows {
        Rows { heading, width: names.map(|n| n.chars().count()).max().unwrap_or(0), opened: false }
    }

    fn open(&mut self, log: Log<'_>) {
        if !self.opened {
            if !self.heading.is_empty() {
                log(&self.heading);
            }
            self.opened = true;
        }
    }

    fn row(&mut self, log: Log<'_>, mark: &str, name: &str, what: &str) {
        self.open(log);
        log(&format!("  {mark} {name:<width$}  {what}", width = self.width));
    }
}

/// The refusal for a migrations directory that isn't there, on a database that ran migrations: a
/// typo in `dir`, which would otherwise read as every migration taken away.
fn missing_dir(opts: &Options, history: &[HistoryRow]) -> Option<Problem> {
    let ran = history.iter().filter(|r| r.applied).count();
    (!opts.dir_exists() && ran > 0).then(|| {
        Problem::new("migrations-dir-missing", format!("There's no migrations directory at {}", shown(&opts.dir))).note(format!("This database ran {ran} migration{} from it: check dir in .reylo.yaml.", if ran == 1 { "" } else { "s" }))
    })
}

/// A migration a database ran whose file is gone: the text it ran is the way back.
fn missing_problem(name: &str, ran: &str, dir: &str) -> Problem {
    Problem::new("applied-migration-missing", format!("This database applied {name}, which isn't in {dir}"))
        .sql(ran)
        .fix("Write it back as it ran", &["reylo migrate history pull"])
        .fix("On a development database, undo it instead", &["reylo migrate dev"])
}

/// A migration edited after a database ran it.
fn edited_problem(name: &str, ran: &str, now: &str) -> Problem {
    Problem::new("applied-migration-edited", format!("Migration {name} changed after this database applied it"))
        .lines(std::iter::once(String::new()).chain(line_diff(ran, now)).chain([String::new()]))
        .note("Write the change as a new migration.")
        .fix("Take back the text this database ran", &["reylo migrate history pull"])
        .fix("On a development database, run it again as it now reads", &["reylo migrate dev"])
}

/// Whether a database's catalog lines hold more than an empty database's do.
fn holds_a_schema(lines: &[String]) -> bool {
    const EMPTY: [&str; 6] = ["schema ", "grant schema ", "comment schema ", "comment language ", "extension ", crate::catalog::DEFAULT_PRIVILEGES];
    // A schema's owner too: a new database's `public` is `pg_database_owner`'s.
    lines.iter().any(|l| !EMPTY.iter().any(|kind| l.starts_with(kind)) && !(l.starts_with("owner ") && l.ends_with(" (schema)")))
}

/// Whether a database holds anything init and introspect write: a schema, or a grant on a schema or a
/// default privilege that a new database on its server lacks. A new database's `public` grants
/// PUBLIC `USAGE`, and `CREATE` too before Postgres 15. On a platform's server, a new database may
/// grant the platform's roles more (Supabase grants its API's roles every new table), so a database
/// with only those still starts empty there.
#[cfg(feature = "cli")]
fn holds_sql(lines: &[String], new: &NewDatabase) -> bool {
    let platform = |grantee: &str| new.platform && crate::catalog::PLATFORM_ROLES.contains(&grantee);
    holds_a_schema(lines)
        || lines.iter().any(|l| {
            if let Some(rest) = l.strip_prefix("grant schema ") {
                let name = crate::catalog::name_at(rest);
                let given = |grantee: &str, privilege: &str| name == "public" && grantee == "PUBLIC" && (privilege == "USAGE" || (privilege == "CREATE" && new.public_create));
                rest[name.len()..].trim().split(',').filter_map(|g| g.rsplit_once(':')).any(|(grantee, privilege)| !given(grantee, privilege) && !platform(grantee))
            } else if l.starts_with(crate::catalog::DEFAULT_PRIVILEGES) {
                let tail = l.trim_end_matches(" with grant option");
                tail.rsplit_once(" to ").or_else(|| tail.rsplit_once(" from ")).is_none_or(|(_, grantee)| !platform(grantee))
            } else {
                false
            }
        })
}

/// What a new database on a server has that [`holds_sql`] needs to know.
#[cfg(feature = "cli")]
#[derive(Default)]
struct NewDatabase {
    /// Whether the server is a platform's, as Supabase's is.
    platform: bool,
    /// Whether a new database's `public` grants PUBLIC `CREATE`, as before Postgres 15.
    public_create: bool,
}

#[cfg(feature = "cli")]
async fn new_database(db: &mut Db) -> Result<NewDatabase, Error> {
    if db.dialect() != Dialect::Postgres {
        return Ok(NewDatabase::default());
    }
    let rows = db.rows(&format!("select ({})::text as platform, (current_setting('server_version_num')::int < 150000)::text as public_create", crate::catalog::platform_names())).await?;
    let r = rows.first().ok_or_else(|| Error("no answer".into()))?;
    Ok(NewDatabase { platform: r.text("platform") == "true", public_create: r.text("public_create") == "true" })
}

/// Whether a migration that fails can leave part of what it did: on MySQL, whose statements commit as
/// they go, and a statement that runs outside a transaction.
fn may_leave_part(sql: &str, d: Dialect) -> bool {
    d == Dialect::Mysql || runs_alone(sql, d)
}

/// `reylo migrate deploy`: runs what is pending on a database that is as Reylo's last run left it, and
/// refuses one that isn't, running nothing. It needs no shadow database: `--dry-run` is what replays.
#[cfg(feature = "cli")]
pub async fn deploy(opts: &Options, log: Log<'_>) -> Result<i32, Error> {
    if let Some(code) = refuse_lock_file(opts, log) {
        return Ok(code);
    }
    let mut db = Db::open(opts).await?;
    let result = deploy_on(&mut db, opts, log).await;
    db.close().await;
    result
}

/// What the lock says of schema.sql, with no database: whether its checksum is the one the lock
/// holds from the last time the migrations were shown to reach it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stamp {
    /// The project has no schema.sql: its migrations are its schema.
    NoSchema,
    /// No lock to hold a checksum.
    NoLock,
    /// A lock that can't be read (a merge conflict in it), or that disagrees with the files, which its
    /// own refusal names.
    Unreadable,
    /// The lock holds none: the migrations weren't shown to reach schema.sql when it was written.
    Unproven,
    Proven,
    /// schema.sql changed since: a change no migration makes yet, or formatting alone.
    Changed,
}

fn stamp(opts: &Options, schema_sql: Option<&str>) -> Stamp {
    let Some(sql) = schema_sql else { return Stamp::NoSchema };
    let l = match opts.lock_state() {
        lock::State::Lock(l) => l,
        lock::State::Missing => return Stamp::NoLock,
        _ => return Stamp::Unreadable,
    };
    let last = l.entries.last().map(|e| e.sum.clone()).unwrap_or_default();
    match l.schema {
        Some(sum) if sum == lock::schema_sum(&last, sql) => Stamp::Proven,
        Some(_) => Stamp::Changed,
        None => Stamp::Unproven,
    }
}

/// The refusal of a schema.sql whose text isn't the one the lock's checksum is of.
fn stamp_problem(opts: &Options) -> Problem {
    Problem::new("schema-sql-changed", "The schema changed since the lock was written")
        .note(format!("The lock holds a checksum of {} from when the migrations last matched it.", shown(&opts.schema)))
        .fix("If the schema changed, write its migration and run it", &["reylo migrate new <name>", "reylo migrate dev"])
        .fix("If only formatting or comments changed, check it again", &["reylo migrate dev"])
}

/// What `status` and `lock` say of the lock's checksum of schema.sql.
fn stamp_line(opts: &Options, stamped: Stamp) -> String {
    match stamped {
        Stamp::NoSchema => schema_line(opts),
        Stamp::Proven => format!("{} The migrations match {}", paint("green", "✓"), shown(&opts.schema)),
        Stamp::Changed => then(note("The schema changed since the lock was written"), &[("To write the changes to a migration file and run it", &["reylo migrate new <name>", "reylo migrate dev"])]),
        Stamp::NoLock => then(note(format!("With no lock to hold its checksum, {} is checked against the migrations by a dry run", shown(&opts.schema))), &[("To check it", &["reylo migrate deploy --dry-run"])]),
        Stamp::Unreadable => note(format!("Once the lock agrees with the files, {} is checked against it", shown(&opts.schema))),
        Stamp::Unproven => then(note(format!("The lock holds no checksum of {} yet", shown(&opts.schema))), &[("To write one, once the migrations match it", &["reylo migrate dev"])]),
    }
}

/// What a check says of the declared schema: that the migrations reach it, or, where the project
/// has none, that its migrations are its schema.
fn schema_line(opts: &Options) -> String {
    if opts.schema_exists() {
        format!("{} The migrations match {}", paint("green", "✓"), shown(&opts.schema))
    } else {
        note(format!("With no {}, the migrations are the schema", shown(&opts.schema)))
    }
}

/// What Reylo reads by name alone or not at all, listed under `heading` where there is any.
fn unmodeled_note(heading: &str, objects: &[String], log: Log<'_>) {
    if objects.is_empty() {
        return;
    }
    log(&format!("{} {heading}:", paint("yellow", "!")));
    for o in objects {
        log(&format!("    {o}"));
    }
}

/// What opening the shadows' server found: the shadows a run left behind, and what every shadow
/// leaves out of the target.
#[cfg(feature = "cli")]
fn swept_note(server: &Server, swept: usize, log: Log<'_>) {
    if swept > 0 {
        log(&note(format!("Dropped {swept} shadow database{} a run left behind", if swept == 1 { "" } else { "s" })));
    }
    left_out_note(server, log);
}

#[cfg(feature = "cli")]
fn left_out_note(server: &Server, log: Log<'_>) {
    if let Some(note) = server.note() {
        log(&paint("dim", note));
    }
}

async fn deploy_on(db: &mut Db, opts: &Options, log: Log<'_>) -> Result<i32, Error> {
    let _held = lock(db, opts).await?;
    let mut plan = check(db, None, opts).await?;
    if plan.ok() {
        plan.problems.extend(late_on(db, opts, &plan).await?);
    }
    if !plan.ok() {
        return Ok(refuse(&plan.problems, log));
    }
    let history = read_history(db).await?;
    if plan.unchanged.is_none() && history.iter().any(|r| r.applied) {
        log(&uncompared_line());
    }
    // A run cut off before it stored its fingerprint, with the schema as it left it: this one stores it.
    let mut runs = Runs { unread: plan.resumed.clone(), ..Runs::new() };
    if plan.pending.is_empty() {
        // A fingerprint that can't be compared is stored again, for the next run to compare.
        if plan.unchanged.is_none() {
            if let Some(last) = history.iter().rfind(|r| r.applied) {
                ensure_history(db).await?;
                store_fingerprint(db, &last.name).await?;
            }
        }
        runs.finish(db).await;
        log(&format!("{} Up to date", paint("green", "✓")));
        return Ok(0);
    }
    ensure_history(db).await?;
    drop_leftovers(db, &plan.leftovers, log).await?;
    let started: Vec<&str> = history.iter().filter(|r| !r.applied).map(|r| r.name.as_str()).collect();
    // Rows marked started belong to pending migrations: they are written again below.
    db.exec(&format!("delete from {HISTORY_TABLE} where applied_at is null")).await?;
    let mut rows = Rows::new(format!("Applying to {}", shown_url(&opts.url)), plan.pending.iter().map(|m| m.name.as_str()));
    let d = db.dialect();
    let mut code = 0;
    let mut applied: Vec<serde_json::Value> = Vec::new();
    for m in &plan.pending {
        let at = std::time::Instant::now();
        match runs.run(db, m).await {
            Ok(()) => {
                let ms = at.elapsed().as_millis();
                rows.row(log, &paint("green", "✓"), &m.name, &format!("applied {}", paint("dim", &format!("· {ms} ms"))));
                applied.push(serde_json::json!({ "name": m.name, "state": "applied", "ms": ms as u64 }));
            }
            Err(f) => {
                rows.row(log, &paint("bold red", "✗"), &m.name, "failed");
                applied.push(serde_json::json!({ "name": m.name, "state": "failed", "error": f.error.0 }));
                log("");
                let started = started.contains(&m.name.as_str());
                let ran: Vec<String> = history.iter().filter(|r| r.applied).map(|r| r.name.clone()).chain(applied.iter().filter(|a| a["state"] == "applied").filter_map(|a| a["name"].as_str().map(String::from))).collect();
                let problem = match if started { None } else { rows_failure(db, opts, m, &f, &ran, &plan.pending).await } {
                    Some(problem) => problem,
                    None => deploy_failure(m, &f.error, d, started, f.undone),
                };
                print_problems(&[problem], log);
                code = 1;
                break;
            }
        }
    }
    runs.finish(db).await;
    set_result(serde_json::json!({ "migrations": applied }));
    if code == 0 {
        log(&format!("{} Applied {} migration{}", paint("green", "✓"), plan.pending.len(), if plan.pending.len() == 1 { "" } else { "s" }));
    }
    Ok(code)
}

/// A failed migration's row and its error; on MySQL, or for a statement that runs alone, what of it
/// may be in place.
fn failed(rows: &mut Rows, m: &Migration, e: &Error, d: Dialect, log: Log<'_>) {
    let partly = may_leave_part(&m.sql, d);
    rows.row(log, &paint("bold red", "✗"), &m.name, if partly { "failed" } else { "failed, and nothing of it was applied" });
    log(&format!("      {e}"));
    if partly && d == Dialect::Mysql {
        log("  MySQL commits each statement, so its first statements may be in place");
        log(&steps(4, &[("See which are", &["reylo migrate deploy --dry-run"])]));
    }
}

/// A key of a query: a column's name as written, or an expression in parentheses.
fn key_term(key: &str) -> String {
    let plain = key.starts_with('"') && key.ends_with('"') || key.starts_with('`') && key.ends_with('`') || (!key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'$'));
    if plain {
        key.to_string()
    } else {
        format!("({key})")
    }
}

/// The query that lists the values more than one row holds of a unique key: `keys` on `table`, the
/// rows a partial index covers (`filter`), and where NULLs are distinct, only rows with no NULL key,
/// as the index compares them.
fn duplicates_of(table: &str, keys: &[String], filter: Option<&str>, nulls_distinct: bool) -> String {
    let terms: Vec<String> = keys.iter().map(|k| key_term(k)).collect();
    let mut wheres: Vec<String> = if nulls_distinct { terms.iter().map(|t| format!("{t} is not null")).collect() } else { vec![] };
    wheres.extend(filter.map(|f| format!("({f})")));
    let at = if wheres.is_empty() { String::new() } else { format!(" where {}", wheres.join(" and ")) };
    format!("select {}, count(*) from {table}{at} group by {} having count(*) > 1;", terms.join(", "), terms.join(", "))
}

/// The query that lists the rows of `table` whose `cols` match no row of `parent` on `refs`: those a
/// foreign key holds, every column set (MATCH SIMPLE) or any (MATCH FULL).
fn orphans_of(table: &str, cols: &[String], parent: &str, refs: &[String], full: bool) -> String {
    let set: Vec<String> = cols.iter().map(|c| format!("c.{c} is not null")).collect();
    let set = if full && set.len() > 1 { format!("({})", set.join(" or ")) } else { set.join(" and ") };
    let on: Vec<String> = cols.iter().zip(refs).map(|(c, r)| format!("p.{r} = c.{c}")).collect();
    format!("select c.* from {table} as c where {set} and not exists (select 1 from {parent} as p where {});", on.join(" and "))
}

/// The duplicates that failed a Postgres unique index's build, as a query, read off the index the
/// build left invalid: its keys, each with the collation it compares by where that isn't its
/// column's (a nondeterministic one takes values as equal that the column's doesn't), its predicate,
/// and whether its NULLs are distinct. Read before the index is dropped.
async fn duplicates_query(db: &mut Db, b: &Breach) -> Option<String> {
    // A key whose operator class isn't its type's own compares by an equality `group by` doesn't
    // use (`text_ops` on a `citext` column or expression), so its duplicates can't be listed. The
    // index's own column has each key's type, an expression's too; a type with no default class
    // of its own (`varchar`) takes another's, by the same equality. A domain's key is left out: its
    // equality is its base type's, which may be one with a class of its own (a domain over `citext`).
    let sql = format!(
        "select i.indrelid::regclass::text as t, pg_get_expr(i.indpred, i.indrelid, true) as pred, coalesce((to_jsonb(i) ->> 'indnullsnotdistinct')::boolean, false) as nnd,
           not exists (select 1 from generate_series(1, i.indnkeyatts) k join pg_opclass o on o.oid = i.indclass[k - 1]
               join pg_attribute ia on ia.attrelid = i.indexrelid and ia.attnum = k
               join pg_type ty on ty.oid = ia.atttypid
             where not o.opcdefault or ty.typtype = 'd' or (o.opcintype <> ia.atttypid and exists (select 1 from pg_opclass t where t.opcmethod = o.opcmethod and t.opcdefault and t.opcintype = ia.atttypid))) as own,
           (select string_agg(pg_get_indexdef(i.indexrelid, k, true) || coalesce((select ' collate ' || quote_ident(n.nspname) || '.' || quote_ident(co.collname) from pg_collation co join pg_namespace n on n.oid = co.collnamespace
               where co.oid = i.indcollation[k - 1] and co.oid <> coalesce((select a.attcollation from pg_attribute a where a.attrelid = i.indrelid and a.attnum = i.indkey[k - 1] and i.indkey[k - 1] > 0), 100)), ''), chr(31) order by k)
             from generate_series(1, i.indnkeyatts) k) as keys
         from pg_index i join pg_class c on c.oid = i.indexrelid where i.indrelid = to_regclass({}) and c.relname = {}",
        crate::db::literal(&b.table),
        crate::db::literal(&b.name)
    );
    let rows = db.rows(&sql).await.ok()?;
    let r = rows.first()?;
    if !r.flag("own") {
        return None;
    }
    let keys: Vec<String> = r.get("keys")?.split('\u{1f}').map(String::from).collect();
    Some(duplicates_of(r.text("t"), &keys, r.get("pred"), !r.flag("nnd")))
}

/// What Postgres's catalog says of the constraint a row broke: the query that lists the rows that
/// break it, and whether it is in place unchecked against the rows already there (`not valid`), as
/// the first of a change's two migrations leaves it. None where the constraint isn't in the catalog.
async fn pg_constraint_rows(db: &mut Db, b: &Breach) -> Option<ConstraintRows> {
    let attnames = |key: &str, rel: &str| format!("(select string_agg(quote_ident(a.attname), chr(31) order by k.i) from unnest(c.{key}) with ordinality k(n, i) join pg_attribute a on a.attrelid = c.{rel} and a.attnum = k.n)");
    let sql = format!(
        "select c.contype as kind, c.confmatchtype as matching, c.convalidated as valid, c.conrelid::regclass::text as t, c.confrelid::regclass::text as parent,
           pg_get_expr(c.conbin, c.conrelid, true) as expr, {} as cols, {} as refs, coalesce((to_jsonb(c) ->> 'conperiod')::boolean, false) as period
         from pg_constraint c where c.conrelid = to_regclass({}) and c.conname = {}",
        attnames("conkey", "conrelid"),
        attnames("confkey", "confrelid"),
        crate::db::literal(&b.table),
        crate::db::literal(&b.name)
    );
    let rows = db.rows(&sql).await.ok()?;
    let r = rows.first()?;
    let split = |v: Option<&str>| -> Vec<String> { v.map(|v| v.split('\u{1f}').map(String::from).collect()).unwrap_or_default() };
    let (table, cols) = (r.text("t"), split(r.get("cols")));
    let query = match r.text("kind") {
        "c" => r.get("expr").map(|e| format!("select * from {table} where not ({e});")),
        // A temporal foreign key (`period`) matches by range, which no equality says.
        "f" if !r.flag("period") => Some(orphans_of(table, &cols, r.text("parent"), &split(r.get("refs")), r.text("matching") == "f")),
        "n" => cols.first().map(|c| format!("select * from {table} where {c} is null;")),
        _ => None,
    };
    Some(ConstraintRows { query, unchecked: !r.flag("valid"), column: if r.text("kind") == "n" { cols.first().cloned() } else { None } })
}

/// What [`pg_constraint_rows`] read: the query, whether the constraint is `not valid`, and a NOT
/// NULL's column.
struct ConstraintRows {
    query: Option<String>,
    unchecked: bool,
    column: Option<String>,
}

/// Whether a migration changes the rows it then checks: it adds a column (whose default or NULLs a
/// constraint of the same file may refuse) or changes a column's type (whose `using` rewrites each
/// value). A constraint such a file fails on may be failed by the file itself, not by the rows the
/// table held.
fn alters_rows(sql: &str, d: Dialect) -> bool {
    const NOT_A_COLUMN: &[&str] = &["CONSTRAINT", "UNIQUE", "PRIMARY", "FOREIGN", "CHECK", "INDEX", "KEY", "FULLTEXT", "SPATIAL", "EXCLUDE", "PARTITION"];
    split_statements(sql, d).iter().any(|s| {
        let words: Vec<String> = s.tokens.iter().map(|t| if t.kind == crate::sql::TokenKind::Symbol { t.text.clone() } else { t.upper() }).collect();
        if words.first().map(String::as_str) != Some("ALTER") || words.get(1).map(String::as_str) != Some("TABLE") {
            return false;
        }
        // Each action of the statement, split at its commas outside parentheses.
        let mut actions: Vec<Vec<&str>> = vec![vec![]];
        let mut depth = 0i32;
        for w in &words[2..] {
            match w.as_str() {
                "(" => depth += 1,
                ")" => depth -= 1,
                "," if depth == 0 => {
                    actions.push(vec![]);
                    continue;
                }
                _ => {}
            }
            actions.last_mut().unwrap().push(w.as_str());
        }
        actions.iter().any(|a| {
            let at = |w: &str| a.iter().position(|x| *x == w);
            match (at("ADD"), at("ALTER"), at("MODIFY"), at("CHANGE")) {
                (_, _, Some(_), _) | (_, _, _, Some(_)) => true,
                (Some(k), _, _, _) => {
                    let next = a.iter().skip(k + 1).find(|w| !matches!(**w, "COLUMN" | "IF" | "NOT" | "EXISTS"));
                    next.is_some_and(|w| !NOT_A_COLUMN.contains(w))
                }
                (_, Some(k), _, _) => a.iter().skip(k + 1).any(|w| *w == "TYPE"),
                _ => false,
            }
        })
    })
}

/// A constraint or an index a migration drops: its name as the server keeps it, and as written,
/// with the table a constraint (or a MySQL index) is dropped from, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Dropped {
    name: String,
    written: String,
    table: Option<String>,
    /// Whether it is an index (`drop index`, MySQL's `drop key`), else a constraint.
    index: bool,
}

/// The constraints and indexes a migration drops and doesn't make again: a changed `check` is
/// dropped and added under its own name.
fn dropped_by(sql: &str, d: Dialect) -> Vec<Dropped> {
    let (mut dropped, made) = dropped_and_made(sql, d);
    dropped.retain(|x| !made.contains(&x.name));
    dropped
}

/// The constraints and indexes a migration drops, and the names of those it makes.
fn dropped_and_made(sql: &str, d: Dialect) -> (Vec<Dropped>, Vec<String>) {
    let (mut dropped, mut made) = (Vec::new(), Vec::new());
    for s in split_statements(sql, d) {
        let t = &s.tokens;
        let up: Vec<String> = t.iter().map(|x| x.upper()).collect();
        let skip = |mut k: usize| {
            while matches!(up.get(k).map(String::as_str), Some("CONCURRENTLY" | "IF" | "NOT" | "EXISTS" | "ONLY")) {
                k += 1;
            }
            k
        };
        // A dotted name from token `k`, as written: its tokens' offsets count from the file's start.
        let written = |k: usize| -> Option<String> {
            let mut end = k;
            while t.get(end + 1).is_some_and(|x| x.text == ".") && t.get(end + 2).is_some() {
                end += 2;
            }
            Some(sql[t.get(k)?.start..t[end].end].to_string())
        };
        // A name as the server keeps it, near enough to say it: a quoted one as written, an unquoted
        // one with its ASCII letters lowered. The catalog, not this, says whether one is there.
        let kept = |k: usize| {
            t.get(k).map(|x| match x.kind {
                crate::sql::TokenKind::Quoted if d == Dialect::Postgres => x.text.trim_matches('"').replace("\"\"", "\""),
                _ => x.text.trim_matches(['"', '`']).to_ascii_lowercase(),
            })
        };
        // Where a drop is looked for after: an `alter table`'s table; for a `drop index`, MySQL's
        // table after `on`, and on Postgres and SQLite none, the index's name being enough. A drop
        // in any other statement (`alter domain`) isn't read.
        let head = (up.first().map(String::as_str), up.get(1).map(String::as_str));
        let (reads, table) = match head {
            (Some("ALTER"), Some("TABLE")) => (true, written(skip(2))),
            (Some("DROP"), Some("INDEX")) if d == Dialect::Mysql => (true, up.iter().position(|w| w == "ON").and_then(|k| written(k + 1))),
            (Some("DROP"), Some("INDEX")) => (true, None),
            _ => (false, None),
        };
        for k in 0..up.len() {
            let what = up.get(k + 1).map(String::as_str);
            if up[k] == "DROP" && matches!(what, Some("INDEX" | "CONSTRAINT" | "CHECK" | "KEY")) {
                if !reads {
                    continue;
                }
                let at = skip(k + 2);
                if let (Some(name), Some(written)) = (kept(at), written(at)) {
                    // Postgres's `drop index` names the index alone, schema-qualified where it is.
                    let name = if table.is_none() { kept(at + if t.get(at + 1).is_some_and(|x| x.text == ".") { 2 } else { 0 }).unwrap_or(name) } else { name };
                    dropped.push(Dropped { name, written, table: table.clone(), index: matches!(what, Some("INDEX" | "KEY")) });
                }
            } else if (up[k] == "ADD" && matches!(what, Some("CONSTRAINT"))) || (up[k] == "INDEX" && k >= 1 && matches!(up[k - 1].as_str(), "CREATE" | "UNIQUE")) {
                made.extend(kept(skip(if up[k] == "ADD" { k + 2 } else { k + 1 })));
            } else if up[k] == "UNIQUE" && matches!(what, Some("KEY" | "INDEX")) {
                made.extend(kept(skip(k + 2)));
            }
        }
    }
    (dropped, made)
}

/// The query that lists the rows a MySQL statement found breaking the constraint it adds, read off
/// the statement, since the server undid it: the `check`'s expression, the foreign key's columns and
/// the table they reference, or the unique key's columns (plain columns only: a prefix or an
/// expression compares otherwise). None where the statement says it otherwise.
fn mysql_rows_query(sql: &str, b: &Breach) -> Option<String> {
    use crate::sql::{Token, TokenKind};
    let [statement] = split_statements(sql, Dialect::Mysql).try_into().ok()?;
    // Each token's offsets count from the start of the file.
    let (t, text) = (&statement.tokens, sql);
    let bare = |k: &Token| k.text.trim_matches('`').to_string();
    let word = |k: usize, w: &str| t.get(k).is_some_and(|x| x.upper() == w);
    let symbol = |k: usize, s: &str| t.get(k).is_some_and(|x| x.kind == TokenKind::Symbol && x.text == s);
    // A dotted name from token `k`: its text as written, and the token after it.
    let name_at = |k: usize| -> Option<(String, usize)> {
        let mut end = k;
        while symbol(end + 1, ".") && t.get(end + 2).is_some() {
            end += 2;
        }
        Some((text[t.get(k)?.start..t[end].end].to_string(), end + 1))
    };
    // The tokens inside the parentheses opening at `k`, and the token after them.
    let inside = |k: usize| -> Option<(usize, usize)> {
        if !symbol(k, "(") {
            return None;
        }
        let mut depth = 0;
        for j in k..t.len() {
            if symbol(j, "(") {
                depth += 1;
            } else if symbol(j, ")") {
                depth -= 1;
                if depth == 0 {
                    return Some((k + 1, j));
                }
            }
        }
        None
    };
    // A list of plain column names, as written.
    let columns = |(from, to): (usize, usize)| -> Option<Vec<String>> {
        let items: Vec<&[Token]> = t[from..to].split(|x| x.kind == TokenKind::Symbol && x.text == ",").collect();
        items.iter().map(|i| match i {
            [one] if matches!(one.kind, TokenKind::Word | TokenKind::Quoted) => Some(one.text.clone()),
            _ => None,
        }).collect()
    };
    let table = if word(0, "ALTER") && word(1, "TABLE") { name_at(2)?.0 } else { t.iter().position(|x| x.upper() == "ON").and_then(|k| name_at(k + 1)).map(|(n, _)| n)? };
    let named = |k: usize| t.get(k).is_some_and(|x| bare(x) == b.name);
    match b.kind {
        Breaks::Check => {
            let k = (0..t.len()).find(|&k| word(k, "CONSTRAINT") && named(k + 1) && word(k + 2, "CHECK"))?;
            let (from, to) = inside(k + 3)?;
            Some(format!("select * from {table} where not ({});", &text[t[from].start..t[to - 1].end]))
        }
        Breaks::ForeignKey => {
            let k = (0..t.len()).find(|&k| word(k, "CONSTRAINT") && named(k + 1) && word(k + 2, "FOREIGN") && word(k + 3, "KEY"))?;
            let open = (k + 4..t.len()).find(|&j| symbol(j, "("))?;
            let (from, to) = inside(open)?;
            let cols = columns((from, to))?;
            if !word(to + 1, "REFERENCES") {
                return None;
            }
            let (parent, at) = name_at(to + 2)?;
            let refs = columns(inside(at)?)?;
            (cols.len() == refs.len()).then(|| orphans_of(&table, &cols, &parent, &refs, false))
        }
        Breaks::Unique => {
            // The key by its name (`constraint <name> unique`, `unique key <name>`, `create unique
            // index <name>`), else the statement's one unique key, which MySQL names after its column.
            let uniques: Vec<usize> = (0..t.len()).filter(|&k| word(k, "UNIQUE")).collect();
            let by_name = uniques.iter().copied().find(|&k| (k >= 1 && named(k - 1)) || named(k + 1) || named(k + 2));
            let k = by_name.or(if uniques.len() == 1 { uniques.first().copied() } else { None })?;
            let open = (k + 1..t.len()).find(|&j| symbol(j, "("))?;
            // MySQL's unique key leaves NULLs distinct.
            Some(duplicates_of(&table, &columns(inside(open)?)?, None, true))
        }
        Breaks::NotNull => None,
    }
}

/// The migrations of one change `migrate new` wrote as several files, `<name>`, `<name>_2`, …, each a
/// second after the one before: those of `m`'s change among `names`, and which of them come before it.
/// A name that ends in `_<n>` reads two ways, as a change's first file and as its nth, so a change
/// whose own name ends so (`tighten_2`, then `tighten_2_2`) is found too.
fn same_change<'a>(m: &str, names: impl IntoIterator<Item = &'a str>) -> (Vec<String>, Vec<String>) {
    // Each reading: the time of the change's first file, its name, and this file's place in it.
    let readings = |name: &str| -> Vec<(i64, String, i64)> {
        let Some(c) = NAME.captures(name) else { return vec![] };
        let Some(at) = time::seconds(&c[1]) else { return vec![] };
        let stem = c[2].to_string();
        let mut out = vec![(at, stem.clone(), 1)];
        if let Some((base, n)) = stem.rsplit_once('_') {
            if let Ok(n) = n.parse::<i64>() {
                if n >= 2 {
                    out.push((at - (n - 1), base.to_string(), n));
                }
            }
        }
        out
    };
    let mine = readings(m);
    let (mut before, mut after) = (Vec::new(), Vec::new());
    for name in names {
        let place = readings(name).into_iter().find_map(|(first, base, k)| mine.iter().find(|(f, b, n)| *f == first && *b == base && *n != k).map(|(_, _, n)| k < *n));
        match place {
            Some(true) => before.push(name.to_string()),
            Some(false) => after.push(name.to_string()),
            None => {}
        }
    }
    (before, after)
}

/// The tables (and domains) a migration's statements change or index, by their last name part,
/// lowercased and unquoted: what a file of one change shares with the others.
fn tables_of(sql: &str, d: Dialect) -> Vec<String> {
    let mut out = Vec::new();
    for s in split_statements(sql, d) {
        let up: Vec<String> = s.tokens.iter().map(|t| t.upper()).collect();
        let at = match (up.first().map(String::as_str), up.get(1).map(String::as_str)) {
            (Some("ALTER"), Some("TABLE" | "DOMAIN" | "INDEX")) => Some(2),
            (Some("CREATE"), _) => up.iter().position(|w| w == "ON").map(|k| k + 1),
            (Some("DROP"), Some("INDEX")) => Some(2),
            _ => None,
        };
        let Some(mut k) = at else { continue };
        while matches!(up.get(k).map(String::as_str), Some("ONLY" | "IF" | "EXISTS" | "CONCURRENTLY")) {
            k += 1;
        }
        // A dotted name's last part.
        while s.tokens.get(k + 1).is_some_and(|t| t.text == ".") && s.tokens.get(k + 2).is_some() {
            k += 2;
        }
        out.extend(s.tokens.get(k).map(|t| t.text.trim_matches(['"', '`']).to_ascii_lowercase()));
    }
    out
}

/// Whether two migrations change a table, an index or a constraint in common, as the files of one
/// change do: Postgres's `drop index` names no table, only the index the next file builds again.
fn related(a: &str, b: &str, d: Dialect) -> bool {
    let objects = |sql: &str| -> Vec<String> {
        let (dropped, made) = dropped_and_made(sql, d);
        tables_of(sql, d).into_iter().chain(dropped.iter().map(|x| &x.name).chain(&made).map(|n| n.to_ascii_lowercase())).collect()
    };
    let theirs = objects(b);
    objects(a).iter().any(|o| theirs.contains(o))
}

/// The constraints a pending Postgres migration validates that are in place unchecked (`not valid`),
/// as the first of a change's two migrations leaves them: each one's table and name.
async fn unchecked_by(db: &mut Db, m: &Migration) -> Vec<(String, String)> {
    if db.dialect() != Dialect::Postgres {
        return vec![];
    }
    let mut out = Vec::new();
    for s in split_statements(&m.sql, Dialect::Postgres) {
        let t = &s.tokens;
        let words: Vec<String> = t.iter().map(|x| x.upper()).collect();
        if words.first().map(String::as_str) != Some("ALTER") || words.get(1).map(String::as_str) != Some("TABLE") {
            continue;
        }
        let Some(v) = words.iter().position(|w| w == "VALIDATE") else { continue };
        if words.get(v + 1).map(String::as_str) != Some("CONSTRAINT") || t.get(v + 2).is_none() {
            continue;
        }
        // The table's name as written: the tokens between `alter table [only] [if exists]` and `validate`.
        let from = 2 + words[2..v].iter().take_while(|w| matches!(w.as_str(), "ONLY" | "IF" | "EXISTS")).count();
        if from >= v {
            continue;
        }
        let table = &m.sql[t[from].start..t[v - 1].end];
        let name = t[v + 2].text.trim_matches('"').replace("\"\"", "\"");
        let sql = format!("select c.conrelid::regclass::text as t from pg_constraint c where c.conrelid = to_regclass({}) and c.conname = {} and not c.convalidated", crate::db::literal(table), crate::db::literal(&name));
        if let Some(r) = db.rows(&sql).await.ok().and_then(|rows| rows.into_iter().next()) {
            out.push((r.text("t").to_string(), name));
        }
    }
    out
}

/// Of `dropped`, the names of those the database doesn't have: what a migration dropped is said to
/// be gone only where it is, since a later migration may have made it again. Each is looked for
/// where its statement dropped it, a constraint on its own table, and its name read as the server
/// reads it, so that a namesake elsewhere doesn't stand in for it.
async fn absent(db: &mut Db, dropped: Vec<Dropped>) -> Vec<String> {
    let mut out = Vec::new();
    for x in dropped {
        let (written, name) = (crate::db::literal(&x.written), crate::db::literal(&x.name));
        let sql = match (db.dialect(), &x.table) {
            (Dialect::Postgres, Some(table)) => format!("select not exists (select 1 from pg_constraint where conrelid = to_regclass({}) and conname = (parse_ident({written}))[1]) as gone", crate::db::literal(table)),
            (Dialect::Postgres, None) => format!("select to_regclass({written}) is null as gone"),
            // An index in the table's indexes, a constraint in its constraints: a `check` that shares an
            // index's name doesn't stand in for it.
            (Dialect::Mysql, Some(table)) => {
                let table = crate::db::literal(table.rsplit('.').next().unwrap_or(table).trim_matches('`'));
                if x.index {
                    format!("select not exists (select 1 from information_schema.statistics where table_schema = database() and table_name = {table} and index_name = {name}) as gone")
                } else {
                    format!("select not exists (select 1 from information_schema.table_constraints where constraint_schema = database() and table_name = {table} and constraint_name = {name}) as gone")
                }
            }
            (Dialect::Sqlite, _) => format!("select not exists (select 1 from sqlite_schema where name = {name}) as gone"),
            // A drop read with no table to look in isn't said.
            (Dialect::Mysql, None) => continue,
        };
        if db.rows(&sql).await.ok().and_then(|rows| rows.first().map(|r| r.flag("gone"))) == Some(true) {
            out.push(x.name);
        }
    }
    out
}

/// What `status` says of a change `migrate new` wrote as several migrations that this database ran
/// only part of: which of them ran and which haven't, and on Postgres, what a constraint the first
/// added unchecked holds until the rest run.
async fn partway_lines(db: &mut Db, ran: &[&HistoryRow], pending: &[&Migration]) -> (Vec<String>, Vec<serde_json::Value>) {
    let mut out = Vec::new();
    let mut json = Vec::new();
    for f in pending {
        let (before, _) = same_change(&f.name, ran.iter().map(|r| r.name.as_str()));
        let (earlier, after) = same_change(&f.name, pending.iter().map(|p| p.name.as_str()));
        // Said once for each change, at the first of its migrations still to run.
        if before.is_empty() || !earlier.is_empty() {
            continue;
        }
        let rest: Vec<String> = std::iter::once(f.name.clone()).chain(after).collect();
        out.push(note(format!("This database ran part of one change: {}, and not {}", before.join(", "), rest.join(", "))));
        let unchecked = unchecked_by(db, f).await;
        for (table, name) in &unchecked {
            out.push(note(format!("Until it runs, rows written to {table} are held to {name}, and the rows already there aren't")));
        }
        let dropped: Vec<Dropped> = ran.iter().filter(|r| before.contains(&r.name)).flat_map(|r| dropped_by(&r.sql, db.dialect())).collect();
        let dropped = absent(db, dropped).await;
        if !dropped.is_empty() {
            out.push(note(format!("Until it runs, {} {} dropped and not yet replaced", dropped.join(", "), if dropped.len() == 1 { "is" } else { "are" })));
        }
        json.push(serde_json::json!({ "ran": before, "pending": rest, "unchecked": unchecked.iter().map(|(t, c)| serde_json::json!({ "table": t, "constraint": c })).collect::<Vec<_>>(), "dropped": dropped }));
    }
    (out, json)
}

/// A migration that failed on rows the table already held, which break the constraint or unique
/// index it adds or checks: said so, with what of its change is in place, the query that lists the
/// rows, and the two ways on. Fixing the rows lets the same migration run; changing the constraint
/// takes replacing this migration and the rest of its change, since a new migration runs after them,
/// which only a database that hasn't run them can do. None for any other failure, and for a
/// migration that writes rows, whose own may have broken it.
async fn rows_failure(db: &mut Db, opts: &Options, m: &Migration, f: &Failure, ran: &[String], pending: &[Migration]) -> Option<Problem> {
    let d = db.dialect();
    let b = f.breach.as_ref()?;
    if !f.undone || !writes_rows(&m.sql, d).is_empty() || alters_rows(&m.sql, d) {
        return None;
    }
    // The table as Postgres writes it in a query on this database: schema-qualified only off the
    // search path.
    let table = match d {
        Dialect::Postgres => db.rows(&format!("select to_regclass({})::text as t", crate::db::literal(&b.table))).await.ok().and_then(|rows| rows.first().and_then(|r| r.get("t").map(String::from))).unwrap_or_else(|| b.table.clone()),
        _ if b.table.is_empty() => "the table".to_string(),
        _ => b.table.clone(),
    };
    // A constraint the migration makes, as a `check` dropped and added under its own name, is in the
    // catalog after the rollback as it was before: its definition there isn't the one that failed,
    // so a migration that makes any is read by none.
    // Any it makes, rather than one by the name the server gave, which the server folds by its own
    // rules (an unquoted `État` is `état` on a Latin-1 database).
    let made = !dropped_and_made(&m.sql, d).1.is_empty();
    let catalog = if d == Dialect::Postgres && !made { pg_constraint_rows(db, b).await } else { None };
    let (query, unchecked, column) = match d {
        Dialect::Postgres => match catalog {
            Some(c) => (c.query.or_else(|| b.rows.clone()), c.unchecked, c.column.or_else(|| b.column.clone())),
            // A NOT NULL a statement set, rather than validated, is named by its column.
            None if b.kind == Breaks::NotNull => {
                let column = match &b.column {
                    Some(c) => db.rows(&format!("select quote_ident({}) as q", crate::db::literal(c))).await.ok().and_then(|rows| rows.first().map(|r| r.text("q").to_string())),
                    None => None,
                };
                (column.as_ref().map(|c| format!("select * from {table} where {c} is null;")), false, column)
            }
            None => (b.rows.clone(), false, None),
        },
        Dialect::Mysql => (mysql_rows_query(&m.sql, b), false, None),
        Dialect::Sqlite => return None,
    };
    // The migrations of the same change, by their names and the tables they share with it.
    let files = opts.migrations().map(|(files, _)| files).unwrap_or_default();
    // What is applied is said by the names `migrate new` gives a change's files; one it splits across
    // tables shares no object between them. A file offered for deletion must share one too.
    let (before, _) = same_change(&m.name, ran.iter().map(String::as_str));
    let (_, after) = same_change(&m.name, pending.iter().filter(|p| related(&m.sql, &p.sql, d)).map(|p| p.name.as_str()));
    // An index built under a name of its own, which a later migration of the change renames to the
    // one it replaces, is named as schema.sql names it.
    let renamed = pending.iter().filter(|p| after.contains(&p.name)).flat_map(|p| split_statements(&p.sql, d)).find_map(|s| {
        let w: Vec<String> = s.tokens.iter().map(|t| t.text.trim_matches('"').to_string()).collect();
        let up: Vec<String> = s.tokens.iter().map(|t| t.upper()).collect();
        let at = up.iter().position(|u| u == "RENAME")?;
        (up.first().map(String::as_str) == Some("ALTER") && up.get(1).map(String::as_str) == Some("INDEX") && up.get(at + 1).map(String::as_str) == Some("TO") && w.get(at - 1).is_some_and(|n| *n == b.name || n.ends_with(&format!(".{}", b.name)))).then(|| w.get(at + 2).cloned()).flatten()
    });
    let name = renamed.unwrap_or_else(|| b.name.clone());
    // What the rows break, as the title and the way on name it: a NOT NULL by its column.
    let (title, what) = match (&column, b.kind) {
        (Some(column), Breaks::NotNull) => (format!("The migration {} failed on rows with nulls in {table}.{column}", m.name), format!("{table}.{column}")),
        _ => (format!("The migration {} failed on rows that break {name}", m.name), name.clone()),
    };
    let mut problem = Problem::new("rows-break-constraint", title).note(&f.error.0);
    let earlier = match before.as_slice() {
        [] => None,
        [one] => Some(format!("The migration before it, {one},")),
        many => Some(format!("The migrations before it, {},", many.join(", "))),
    };
    // The migrations before it, as the files have them, to say what they did.
    let texts: Vec<&str> = files.iter().filter(|f| before.contains(&f.name)).map(|f| f.sql.as_str()).collect();
    let added = texts.iter().any(|sql| sql.contains(&b.name));
    if unchecked {
        problem = problem.note(match &earlier {
            Some(earlier) if added => format!("{earlier} added {} without checking the rows already in {table}: rows written from now on are held to it, and those aren't.", b.name),
            _ => format!("The constraint {} is in place without checking the rows already in {table}: rows written from now on are held to it, and those aren't.", b.name),
        });
    } else if let Some(earlier) = &earlier {
        let dropped = absent(db, texts.iter().flat_map(|sql| dropped_by(sql, d)).collect()).await;
        let is = if before.len() == 1 { "is" } else { "are" };
        problem = problem.note(match dropped.as_slice() {
            [] => format!("{earlier} of the same change {is} applied."),
            names => format!("{earlier} of the same change {is} applied, so {} {} dropped and not yet replaced.", names.join(", "), if names.len() == 1 { "is" } else { "are" }),
        });
    }
    problem = problem.note("Nothing was recorded, so the next deploy runs it again.");
    problem = match &query {
        Some(query) => problem.fix("List the rows that break it", &[query]).fix("Fix them here, then deploy again", &["reylo migrate deploy"]),
        None => problem.fix("Fix the rows that break it here, then deploy again", &["reylo migrate deploy"]),
    };
    let replaced: Vec<String> = std::iter::once(m.name.clone()).chain(after).collect();
    let (them, files) = if replaced.len() == 1 { ("it", replaced[0].clone()) } else { ("them", replaced.join(" and ")) };
    let lead = if opts.schema_exists() {
        format!("If the rows are right and only development ran {files}, change {what} in {} and delete {them}, then take {them} off development and write the replacement", shown(&opts.schema))
    } else {
        format!("If the rows are right and only development ran {files}, delete {them}, then take {them} off development and write the replacement")
    };
    Some(problem.fix(lead, &["reylo migrate dev", "reylo migrate new <name>", "reylo migrate dev"]))
}

/// A migration deploy ran that failed: its error, what of it may be in place, and what to do next.
/// Where nothing of it is in place ([`run_undone`]), nor of its row, it is said to have rolled back,
/// and to be fixed and run again; elsewhere, where its change or its row may be in place, the dry run
/// says which.
fn deploy_failure(m: &Migration, e: &Error, d: Dialect, started: bool, undone: bool) -> Problem {
    let problem = Problem::new("migration-failed", format!("The migration {} failed", m.name)).note(&e.0);
    if started {
        return problem
            .note("A run before this one was cut off here, so its change may be in place.")
            .fix("See whether it is", &["reylo migrate deploy --dry-run"])
            .fix("Record it if it is, running nothing", &["reylo migrate history push"]);
    }
    if !undone {
        let why = if d == Dialect::Mysql { "MySQL commits each statement, so its first statements may be in place" } else { "It runs outside a transaction, so its change may be in place" };
        return problem.note(format!("{why}.")).fix("See what is", &["reylo migrate deploy --dry-run"]);
    }
    Problem { title: format!("The migration {} failed and was rolled back", m.name), ..problem }
        .note("Nothing was recorded, so the next deploy runs it again.")
        .fix("Fix the file, lock the fix and deploy again", &["reylo migrate dev", "reylo migrate deploy"])
        .fix("If another database already ran it, write a new migration instead", &["reylo migrate new <name>"])
}

/// What a command says where the fingerprint its last run stored can't be compared.
fn uncompared_line() -> String {
    then(note("This database can't be compared here with what its last migration left"), &[("To compare it with a dry run", &["reylo migrate deploy --dry-run"])])
}

/// `reylo migrate deploy --dry-run`: everything `deploy` checks, the database against a replay of its
/// history, what is pending tried in a shadow, and the migrations against schema.sql; runs nothing,
/// and writes nothing to the database.
#[cfg(feature = "cli")]
pub async fn dry_run(opts: &Options, log: Log<'_>) -> Result<i32, Error> {
    if let Some(code) = refuse_lock_file(opts, log) {
        return Ok(code);
    }
    // A SQLite file that isn't there yet is a database a first deploy makes: checked as the
    // empty one opening it makes, which is removed after, with what its engine wrote beside it.
    let made: Vec<PathBuf> = match missing_file(opts)? {
        Some((path, _)) => ["", "-wal", "-shm", "-journal"].iter().map(|x| PathBuf::from(format!("{}{x}", path.display()))).filter(|p| !p.exists()).collect(),
        None => vec![],
    };
    let result = dry_run_on(opts, log).await;
    for path in &made {
        let _ = std::fs::remove_file(path);
    }
    result
}

#[cfg(feature = "cli")]
async fn dry_run_on(opts: &Options, log: Log<'_>) -> Result<i32, Error> {
    let (mut db, mut server, swept) = match open(opts, log).await? {
        Ok(opened) => opened,
        Err(code) => return Ok(code),
    };
    swept_note(&server, swept, log);
    let unmodeled = crate::catalog::unmodeled(&mut db, false).await;
    let result = check(&mut db, Some(&mut server), opts).await;
    let result = match result {
        Ok(mut plan) if plan.ok() && lock_free(opts) => {
            let (files, _) = opts.migrations()?;
            // Read again since the check: what changed meanwhile is held to a shadow as well.
            let unreplayable = replay_problems(opts, &files, None, db.dialect());
            if unreplayable.is_empty() {
                let texts: Vec<&str> = files.iter().map(|f| f.sql.as_str()).collect();
                let mut shadows = Shadows::new(&mut server, &opts.url, &opts.root, &opts.cache, &texts).await;
                let late = late_problem(&mut shadows, &opts.dir, &plan.ran, &plan.pending, &files).await;
                shadows.finish().await;
                late.map(|late| {
                    plan.problems.extend(late);
                    plan
                })
            } else {
                plan.problems.extend(unreplayable);
                Ok(plan)
            }
        }
        other => other,
    };
    // What each pending migration blocks while it reads every row of a table, and what it drops:
    // said, not refused, since `migrate new` asked when it was written, and a hand-written one is
    // its author's to judge. And the rows here that a constraint it adds refuses, which fail it.
    let findings = match &result {
        Ok(plan) if plan.ok() && !plan.pending.is_empty() => {
            let texts: Vec<&str> = plan.ran.iter().chain(&plan.pending).map(|f| f.sql.as_str()).collect();
            let mut shadows = Shadows::new(&mut server, &opts.url, &opts.root, &opts.cache, &texts).await;
            let found = pending_findings(&mut shadows, &mut db, &plan.ran, &plan.pending).await;
            shadows.finish().await;
            found?
        }
        _ => vec![],
    };
    db.close().await;
    server.close().await;
    let plan = result?;
    unmodeled_note("This database has what Reylo doesn't fully compare, so these checks can't see every change to it", &unmodeled?, log);
    if !plan.ok() {
        print_problems(&plan.problems, log);
        return Ok(1);
    }
    if !plan.ran.is_empty() {
        log(&format!("{} This database is as its migrations left it", paint("green", "✓")));
    }
    if plan.unchanged == Some(false) {
        log(&then(format!("{} Deploys refuse this database until its fingerprint is stored again", paint("yellow", "!")), &[("Store it, running nothing", &["reylo migrate history push"])]));
    }
    log(&schema_line(opts));
    for x in &plan.leftovers {
        log(&then(format!("{} A cut-off run of {} left {} invalid", paint("yellow", "!"), x.migration, x.index), &[("Drop it and run the migration again", &["reylo migrate deploy"])]));
    }
    for name in &plan.landed {
        log(&then(format!("{} A cut-off run of {name} left its change in place", paint("yellow", "!")), &[("Record it, running nothing", &["reylo migrate history push"])]));
    }
    match plan.pending.len() {
        0 => log(&format!("{} Nothing is pending", paint("green", "✓"))),
        n => {
            log(&format!("{} Tried {} on a shadow database:", paint("green", "✓"), if n == 1 { "the pending migration".to_string() } else { format!("each of {n} pending migrations") }));
            for m in &plan.pending {
                log(&format!("    {} {}", paint("dim", "·"), m.name));
                for (_, f) in findings.iter().filter(|(name, _)| *name == m.name) {
                    let mark = if f.fails { paint("red", "✗") } else { paint("yellow", "!") };
                    log(&format!("      {mark} It {}", f.what));
                    if let Some(query) = &f.query {
                        log(&steps(8, &[("List them", &[query.as_str()])]));
                    }
                }
            }
        }
    }
    for (_, f) in findings.iter().filter(|(name, _)| name.is_empty()) {
        log(&note(&f.what));
    }
    if findings.iter().any(|(_, f)| f.fails) {
        log(&then(
            format!("\n{} The deploy would fail on rows this database holds", paint("red", "✗")),
            &[("Fix the rows, or the migration where no database has run it yet, then try again", &["reylo migrate deploy --dry-run"])],
        ));
        return Ok(1);
    }
    Ok(0)
}

/// What `deploy --dry-run` says of a pending migration, under its name: a lock it holds that blocks
/// writes, a table or column it drops or a type change that loses values (as `migrate new` asks
/// about, said here of every migration, one written by hand too), and a constraint that rows the
/// database already holds break, which fails the deploy.
#[cfg(feature = "cli")]
struct Finding {
    what: String,
    /// Whether the deploy fails on it, rather than doing what the migration says.
    fails: bool,
    /// The query that lists the rows it is about.
    query: Option<String>,
}

#[cfg(feature = "cli")]
impl Finding {
    fn said(what: String) -> Finding {
        Finding { what, fails: false, query: None }
    }
}

/// How many rows a count reads before it stops: past it, a table is only said to have more.
#[cfg(feature = "cli")]
const COUNTED: u64 = 1_000_000;

/// How long a read of the database's rows may take, in milliseconds, before it is left unsaid.
#[cfg(feature = "cli")]
const READ_MS: u32 = 5000;

/// What each pending migration does, run on a replay of what the database ran, then each in turn:
/// the locks it holds that block writes to a table with rows while it reads them all (Postgres and
/// MySQL); what it drops, with what `target` holds of it; and on Postgres and MySQL, each constraint
/// it adds or validates, checked against `target`'s rows, read only.
#[cfg(feature = "cli")]
async fn pending_findings(shadows: &mut Shadows<'_>, target: &mut Db, ran: &[Migration], pending: &[Migration]) -> Result<Vec<(String, Finding)>, Error> {
    let lane = shadows.lane();
    shadows.push(lane, ran.iter().map(Step::of));
    if shadows.open(lane).await?.is_err() {
        return Ok(vec![]);
    }
    shadows.taint(lane);
    let d = shadows.db(lane).dialect();
    // On MySQL, where a change is a migration per statement, a table a pending one made has no rows.
    let made = if d == Dialect::Mysql { crate::locks::mysql_tables_made(pending.iter().map(|m| m.sql.as_str())) } else { Default::default() };
    let alterer = crate::diff::alterer(shadows.db(lane)).await.ok();
    let mut before = crate::diff::read(shadows.db(lane)).await.ok();
    // The schema the target has, the one its migrations left: what is not in it has no rows there.
    let here = before.clone();
    let mut checks = row_checks(shadows.db(lane)).await;
    // Each table the target has, as its columns are: a check is read on the target only where its
    // tables are still those, not one made again under the name, or a column dropped and added or
    // retyped, whose values the target doesn't hold.
    let tables_here = table_identities(shadows.db(lane)).await;
    // After a pending statement that changes data, the rows a constraint meets are the ones it
    // leaves, which may be through a function, a trigger or a cascade no reading of it can follow:
    // the deploy is left to meet them.
    let mut data_changed = false;
    // The tables and columns a pending statement drops or renames, so that one made again under the
    // name holds none of the target's values: MySQL's catalog can show a column dropped and added
    // again as it was.
    let (mut gone_tables, mut gone_columns): (Vec<String>, Vec<(String, String)>) = (Vec::new(), Vec::new());
    let mut timed = false;
    let mut out = Vec::new();
    for m in pending {
        let hazards = match crate::locks::run_watched(shadows.db(lane), m).await {
            Ok(found) => found,
            // The check already tried each pending migration: one that fails here says nothing more.
            Err(_) => break,
        };
        out.extend(hazards.into_iter().filter(|h| !made.contains(&h.table.to_lowercase())).map(|h| (m.name.clone(), Finding::said(h.what))));
        let after = crate::diff::read(shadows.db(lane)).await.ok();
        if let (Some(from), Some(to), Some(alterer), Some(here)) = (&before, &after, &alterer, &here) {
            let (lost, narrowed) = losses(from, to, alterer.as_ref(), &m.sql, d);
            for id in lost {
                let counted = if here.objects.contains_key(&id) { count_lost(target, here, &id, &mut timed).await } else { None };
                out.push((m.name.clone(), Finding::said(lost_line(&id, counted))));
            }
            for (id, why) in narrowed {
                out.push((m.name.clone(), Finding::said(format!("changes {} {}", shown_name(&id), why.strip_prefix("changes ").unwrap_or(&why)))));
            }
        }
        data_changed |= split_statements(&m.sql, d).iter().any(|s| changes_data(s, d));
        let (dropped_tables, dropped_columns) = drops_of(&m.sql, d);
        let (renamed_tables, renamed_columns) = renames_of(&m.sql, d);
        gone_tables.extend(dropped_tables.into_iter().map(|(_, t)| t).chain(renamed_tables).map(|t| folded(&t, d)));
        gone_columns.extend(dropped_columns.into_iter().map(|((_, t), c)| (t, c)).chain(renamed_columns).map(|(t, c)| (folded(&t, d), folded(&c, d))));
        let now = row_checks(shadows.db(lane)).await;
        let tables_now = if data_changed { BTreeMap::new() } else { table_identities(shadows.db(lane)).await };
        let bare = |t: &str| folded(&unquoted(&crate::sql::last_written(t), d), d);
        // Whether `columns` of `table` (all of them, for None) are those the target holds: the same
        // table, each column as it was, and none dropped or renamed by a pending statement.
        let unchanged = |table: &str, columns: Option<&[String]>| -> bool {
            let (Some(here), Some(now)) = (tables_here.get(table), tables_now.get(table)) else { return false };
            let name = bare(table);
            let all: Vec<String>;
            let columns = match columns {
                Some(c) => c,
                None => {
                    all = here.columns.keys().chain(now.columns.keys()).cloned().collect();
                    &all
                }
            };
            here.table == now.table && !gone_tables.contains(&name) && columns.iter().all(|c| here.columns.get(c).is_some() && here.columns.get(c) == now.columns.get(c) && !gone_columns.contains(&(name.clone(), c.clone())))
        };
        for (key, check) in &now {
            let parent_unchanged = check.parent.as_ref().is_none_or(|(p, refs)| unchanged(p, Some(refs)));
            if checks.contains_key(key) || data_changed || !unchanged(&check.ident, check.columns.as_deref()) || !parent_unchanged {
                continue;
            }
            if let Some(f) = broken_rows(target, shadows.db(lane), check, &mut timed).await {
                out.push((m.name.clone(), f));
            }
        }
        checks = now;
        before = after;
    }
    if timed {
        out.push((String::new(), Finding::said(format!("Some reads of this database's rows ran past {} s, so what they would say is left out", READ_MS / 1000))));
    }
    Ok(out)
}

/// What a migration loses, from the schemas before and after it ran on a shadow: the tables and
/// columns it drops, and the columns whose change of type loses what their values hold. A table or
/// column the migration renames keeps its rows, which only its statements say: the catalog shows a
/// rename as one going and another coming.
#[cfg(feature = "cli")]
fn losses(before: &crate::diff::Schema, after: &crate::diff::Schema, alterer: &dyn crate::diff::Alterer, sql: &str, d: Dialect) -> (Vec<String>, Vec<(String, String)>) {
    let (tables, columns) = renames_of(sql, d);
    let same = |a: &str, b: &str| if d == Dialect::Postgres { a == b } else { a.eq_ignore_ascii_case(b) };
    let table_name = |id: &str| unquoted(&crate::sql::last_written(id.strip_prefix("table ").unwrap_or(id)), d);
    let renamed_table = |id: &str| tables.iter().any(|t| same(t, &table_name(id)));
    let renamed_column = |id: &str| -> bool {
        let Some(owner) = before.objects.get(id).and_then(|o| o.owner.clone()) else { return false };
        let column = id.strip_prefix(&format!("column {}.", owner.strip_prefix("table ").unwrap_or(&owner))).map(|c| unquoted(c, d)).unwrap_or_default();
        renamed_table(&owner) || columns.iter().any(|(t, c)| same(t, &table_name(&owner)) && same(c, &column))
    };
    let dropped: BTreeSet<String> = before.objects.iter().filter(|(id, o)| o.kind == Some(crate::diff::Kind::Column) && !after.objects.contains_key(*id) && !renamed_column(id)).map(|(id, _)| id.clone()).collect();
    let plan = crate::diff::plan_with(before, after, alterer, &[], &dropped);
    let mut lost: Vec<String> = plan.losses.into_iter().filter(|id| if id.starts_with("table ") { !renamed_table(id) } else { !renamed_column(id) }).collect();
    // A table or column the migration drops and makes again, as it was, reads as unchanged in the
    // catalog, while its rows are gone: its statements say so.
    let (drop_tables, drop_columns) = drops_of(sql, d);
    // The table a statement's name means: by its schema where it gives one; else the one table of
    // that name, or Postgres's `public` one of several.
    let tables: Vec<(&String, Vec<String>)> = before.objects.iter().filter(|(_, o)| o.kind == Some(crate::diff::Kind::Table)).map(|(id, _)| (id, name_parts(id.strip_prefix("table ").unwrap_or(id), d))).collect();
    let means = |(schema, name): &Named, id: &str| -> bool {
        let named: Vec<&&String> = tables.iter().filter(|(_, parts)| parts.last().is_some_and(|n| same(n, name)) && schema.as_ref().is_none_or(|s| parts.len() > 1 && same(&parts[0], s))).map(|(id, _)| id).collect();
        let one = match named.as_slice() {
            [one] => Some(**one),
            many => many.iter().find(|id| name_parts(id.strip_prefix("table ").unwrap_or(id), d).first().is_some_and(|s| s == "public")).map(|id| **id),
        };
        one.is_some_and(|one| one == id)
    };
    for (id, o) in &before.objects {
        if !after.objects.contains_key(id) || lost.contains(id) {
            continue;
        }
        let hit = match o.kind {
            Some(crate::diff::Kind::Table) => drop_tables.iter().any(|t| means(t, id)),
            Some(crate::diff::Kind::Column) => o.owner.as_ref().is_some_and(|owner| {
                let column = id.strip_prefix(&format!("column {}.", owner.strip_prefix("table ").unwrap_or(owner))).map(|c| unquoted(c, d)).unwrap_or_default();
                !lost.contains(owner) && !drop_tables.iter().any(|t| means(t, owner)) && drop_columns.iter().any(|(t, c)| means(t, owner) && same(c, &column))
            }),
            _ => false,
        };
        if hit {
            lost.push(id.clone());
        }
    }
    (lost, plan.narrowings)
}

/// A table as a statement names it: its schema where it says one, and its name, each as the
/// database keeps it.
#[cfg(feature = "cli")]
type Named = (Option<String>, String);

/// The tables and columns a migration's statements drop, by their names as the database keeps them:
/// `drop table`, and `alter table … drop [column]`, each column with its table.
#[cfg(feature = "cli")]
fn drops_of(sql: &str, d: Dialect) -> (Vec<Named>, Vec<(Named, String)>) {
    use crate::sql::TokenKind;
    let (mut tables, mut columns) = (Vec::new(), Vec::new());
    for s in split_statements(sql, d) {
        let t = &s.tokens;
        let up: Vec<String> = t.iter().map(|x| x.upper()).collect();
        let symbol = |k: usize, c: &str| t.get(k).is_some_and(|x| x.kind == TokenKind::Symbol && x.text == c);
        let name_at = |k: usize| -> Option<(Named, usize)> {
            let mut end = k;
            while symbol(end + 1, ".") && t.get(end + 2).is_some() {
                end += 2;
            }
            let schema = (end > k).then(|| t.get(end - 2).map(|x| unquoted(&x.text, d))).flatten();
            Some(((schema, unquoted(&t.get(end)?.text, d)), end + 1))
        };
        let skip = |mut k: usize| {
            while matches!(up.get(k).map(String::as_str), Some("ONLY" | "IF" | "EXISTS" | "TEMPORARY")) {
                k += 1;
            }
            k
        };
        match (up.first().map(String::as_str), up.get(1).map(String::as_str)) {
            (Some("DROP"), Some("TABLE")) => {
                let mut k = skip(2);
                while let Some((name, next)) = name_at(k) {
                    tables.push(name);
                    if !symbol(next, ",") {
                        break;
                    }
                    k = next + 1;
                }
            }
            (Some("ALTER"), Some("TABLE")) => {
                let Some((table, k)) = name_at(skip(2)) else { continue };
                let mut starts = vec![k];
                let mut depth = 0i32;
                for j in k..t.len() {
                    if symbol(j, "(") {
                        depth += 1;
                    } else if symbol(j, ")") {
                        depth -= 1;
                    } else if symbol(j, ",") && depth == 0 {
                        starts.push(j + 1);
                    }
                }
                for a in starts {
                    if up.get(a).map(String::as_str) != Some("DROP") {
                        continue;
                    }
                    let mut c = a + 1;
                    match up.get(c).map(String::as_str) {
                        Some("COLUMN") => c += 1,
                        Some("CONSTRAINT" | "INDEX" | "KEY" | "PRIMARY" | "FOREIGN" | "CHECK" | "PARTITION" | "DEFAULT" | "IDENTITY" | "EXPRESSION" | "NOT") | None => continue,
                        _ => {}
                    }
                    while matches!(up.get(c).map(String::as_str), Some("IF" | "EXISTS")) {
                        c += 1;
                    }
                    columns.extend(t.get(c).map(|x| (table.clone(), unquoted(&x.text, d))));
                }
            }
            _ => {}
        }
    }
    (tables, columns)
}

/// A name as a statement or the catalog writes it, as the database keeps it: unquoted, a quoted
/// one's doubled quotes made one; an unquoted Postgres name lowercased, as Postgres folds it.
#[cfg(feature = "cli")]
fn unquoted(name: &str, d: Dialect) -> String {
    for q in ['"', '`'] {
        if name.len() >= 2 && name.starts_with(q) && name.ends_with(q) {
            return name[1..name.len() - 1].replace(&format!("{q}{q}"), &q.to_string());
        }
    }
    if name.len() >= 2 && name.starts_with('[') && name.ends_with(']') {
        return name[1..name.len() - 1].to_string();
    }
    if d == Dialect::Postgres {
        name.to_ascii_lowercase()
    } else {
        name.to_string()
    }
}

/// A name as a check compares it: as Postgres keeps it, where `"Plan"` and `plan` are two columns;
/// lowercased on MySQL and SQLite, whose column names are the same in any case.
#[cfg(feature = "cli")]
fn folded(name: &str, d: Dialect) -> String {
    if d == Dialect::Postgres {
        name.to_string()
    } else {
        name.to_lowercase()
    }
}

/// A dotted name's parts, each as the database keeps it: `billing."Logs"` is `billing` and `Logs`.
#[cfg(feature = "cli")]
fn name_parts(name: &str, d: Dialect) -> Vec<String> {
    let (mut parts, mut at, mut quote) = (Vec::new(), 0, None);
    for (k, ch) in name.char_indices() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '`' => quote = Some(ch),
            None if ch == '.' => {
                parts.push(unquoted(&name[at..k], d));
                at = k + 1;
            }
            None => {}
        }
    }
    parts.push(unquoted(&name[at..], d));
    parts
}

/// The tables and columns a migration's statements rename, by their old names as the database keeps
/// them: `alter table … rename to`, `alter table … rename [column] … to`, MySQL's `rename table` and
/// `change [column]`. Each column with its table's last name part.
#[cfg(feature = "cli")]
fn renames_of(sql: &str, d: Dialect) -> (Vec<String>, Vec<(String, String)>) {
    use crate::sql::TokenKind;
    let (mut tables, mut columns) = (Vec::new(), Vec::new());
    for s in split_statements(sql, d) {
        let t = &s.tokens;
        let up: Vec<String> = t.iter().map(|x| x.upper()).collect();
        let symbol = |k: usize, c: &str| t.get(k).is_some_and(|x| x.kind == TokenKind::Symbol && x.text == c);
        // A dotted name from token `k`: its last part as kept, and the token after it.
        let name_at = |k: usize| -> Option<(String, usize)> {
            let mut end = k;
            while symbol(end + 1, ".") && t.get(end + 2).is_some() {
                end += 2;
            }
            Some((unquoted(&t.get(end)?.text, d), end + 1))
        };
        match (up.first().map(String::as_str), up.get(1).map(String::as_str)) {
            (Some("RENAME"), Some("TABLE")) => {
                // `rename table a to b, c to d`.
                let mut k = 2;
                while let Some((old, next)) = name_at(k) {
                    tables.push(old);
                    let Some(comma) = (next..t.len()).find(|&j| symbol(j, ",")) else { break };
                    k = comma + 1;
                }
            }
            (Some("ALTER"), Some("TABLE")) => {
                let mut k = 2;
                while matches!(up.get(k).map(String::as_str), Some("ONLY" | "IF" | "EXISTS")) {
                    k += 1;
                }
                let Some((table, mut k)) = name_at(k) else { continue };
                if symbol(k, "*") {
                    k += 1;
                }
                // Each action, from its first token, split at commas outside parentheses.
                let mut starts = vec![k];
                let mut depth = 0i32;
                for j in k..t.len() {
                    if symbol(j, "(") {
                        depth += 1;
                    } else if symbol(j, ")") {
                        depth -= 1;
                    } else if symbol(j, ",") && depth == 0 {
                        starts.push(j + 1);
                    }
                }
                for a in starts {
                    match up.get(a).map(String::as_str) {
                        Some("RENAME") => match up.get(a + 1).map(String::as_str) {
                            Some("TO" | "AS") => tables.push(table.clone()),
                            Some("CONSTRAINT" | "INDEX" | "KEY") => {}
                            Some("COLUMN") => columns.extend(t.get(a + 2).map(|c| (table.clone(), unquoted(&c.text, d)))),
                            // MySQL's `rename new_name`, or Postgres's and SQLite's `rename a to b`.
                            Some(_) if up.get(a + 2).map(String::as_str) == Some("TO") => columns.extend(t.get(a + 1).map(|c| (table.clone(), unquoted(&c.text, d)))),
                            Some(_) => tables.push(table.clone()),
                            None => {}
                        },
                        Some("SET") if up.get(a + 1).map(String::as_str) == Some("SCHEMA") => tables.push(table.clone()),
                        Some("CHANGE") => {
                            let c = if up.get(a + 1).map(String::as_str) == Some("COLUMN") { a + 2 } else { a + 1 };
                            if let (Some(old), Some(new)) = (t.get(c), t.get(c + 1)) {
                                if !unquoted(&old.text, d).eq_ignore_ascii_case(&unquoted(&new.text, d)) {
                                    columns.push((table.clone(), unquoted(&old.text, d)));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    (tables, columns)
}

/// What a dropped table or column held on the target, counted up to [`COUNTED`] rows: the rows of a
/// table, and the rows where a column holds a value. None where it can't be read in time.
#[cfg(feature = "cli")]
async fn count_lost(target: &mut Db, here: &crate::diff::Schema, id: &str, timed: &mut bool) -> Option<u64> {
    let d = target.dialect();
    let quote = |name: &str| match d {
        Dialect::Postgres => name.to_string(),
        Dialect::Mysql => format!("`{}`", name.replace('`', "``")),
        Dialect::Sqlite => crate::db::pg_quoted(name),
    };
    let (table, column) = if id.starts_with("column ") {
        let owner = here.objects.get(id)?.owner.clone()?;
        let table = owner.strip_prefix("table ").unwrap_or(&owner).to_string();
        let column = id.strip_prefix(&format!("column {table}."))?.to_string();
        (table, Some(column))
    } else {
        (id.strip_prefix("table ")?.to_string(), None)
    };
    let filter = column.map(|c| format!(" where {} is not null", quote(&c))).unwrap_or_default();
    read_count(target, &format!("select 1 from {}{filter}", quote(&table)), COUNTED, timed).await
}

/// The rows `query` returns, counted up to `limit` and one more, read with a time limit. On Postgres
/// and MySQL in a read-only transaction, rolled back: a function a constraint calls may write, and
/// the server refuses that here, which leaves the count unsaid.
#[cfg(feature = "cli")]
async fn read_count(target: &mut Db, query: &str, limit: u64, timed: &mut bool) -> Option<u64> {
    let query = query.trim().trim_end_matches(';');
    let sql = match target.dialect() {
        Dialect::Postgres => format!("select count(*)::text as n from ({query} limit {}) as s", limit + 1),
        Dialect::Mysql => format!("select /*+ MAX_EXECUTION_TIME({READ_MS}) */ count(*) as n from ({query} limit {}) as s", limit + 1),
        Dialect::Sqlite => format!("select count(*) as n from ({query} limit {}) as s", limit + 1),
    };
    let opened = match target.dialect() {
        Dialect::Postgres => {
            target.exec("begin transaction read only").await.is_ok() && target.exec(&format!("set local statement_timeout = {READ_MS}; set local lock_timeout = {READ_MS}")).await.is_ok()
        }
        Dialect::Mysql => target.exec(&format!("set session lock_wait_timeout = {}", READ_MS.div_ceil(1000))).await.is_ok() && target.exec("start transaction read only").await.is_ok(),
        Dialect::Sqlite => true,
    };
    let read = if opened { target.rows(&sql).await } else { Err(Error("no read-only transaction".to_string())) };
    if target.dialect() != Dialect::Sqlite {
        let _ = target.exec("rollback").await;
    }
    match read {
        Ok(rows) => rows.first().and_then(|r| r.text("n").parse().ok()),
        Err(e) => {
            let e = e.0.to_ascii_lowercase();
            *timed |= e.contains("statement timeout") || e.contains("execution time");
            None
        }
    }
}

/// The line a table or column dropped is said with, with what the target held of it.
#[cfg(feature = "cli")]
fn lost_line(id: &str, counted: Option<u64>) -> String {
    let name = shown_name(id);
    let rows = |n: u64| if n > COUNTED { format!("over {COUNTED} rows") } else if n == 1 { "1 row".to_string() } else { format!("{n} rows") };
    match (id.starts_with("table "), counted) {
        (true, Some(0)) => format!("drops the table {name}, which is empty on this database"),
        (true, Some(n)) => format!("drops the table {name} and its {} on this database", rows(n)),
        (true, None) => format!("drops the table {name} and its rows"),
        (false, Some(0)) => format!("drops {name}, which holds no value on this database"),
        (false, Some(n)) => format!("drops {name}, which holds a value in {} on this database", rows(n)),
        (false, None) => format!("drops {name} and its data"),
    }
}

/// A constraint, a unique index or a NOT NULL that a database's rows must meet: its table and name as
/// the shadow's catalog gives them, and what it is.
#[cfg(feature = "cli")]
#[derive(Clone)]
struct RowCheck {
    table: String,
    name: String,
    /// What the line names it: a constraint or index by its name, a NOT NULL by its column.
    shown: String,
    /// Its table by the name [`table_identities`] gives it.
    ident: String,
    /// The columns of its table it reads, lowercased; None where the catalog can't say (an
    /// expression index, a partial one, a MySQL `check` with no column named), so it reads any.
    columns: Option<Vec<String>>,
    /// The other table its rows are checked against, a foreign key's parent, by that name too, and
    /// the columns of it the key references.
    parent: Option<(String, Vec<String>)>,
    /// A Postgres `check … no inherit`, which holds its own table's rows alone, not its children's.
    only: bool,
    kind: RowCheckKind,
}

#[cfg(feature = "cli")]
#[derive(Clone)]
enum RowCheckKind {
    /// A Postgres `check` or foreign key, read back from the catalog by its name.
    Constraint,
    /// A unique index, or the one a unique key or primary key holds.
    Unique,
    /// A NOT NULL on a column, named as SQL writes it.
    NotNull,
    /// The query that lists the rows that break it, built from the catalog (MySQL).
    Rows(String),
    /// A MySQL unique key on plain columns, as SQL writes them.
    Keys(Vec<String>),
}

/// Every constraint, unique index and NOT NULL of a shadow's tables that its rows must meet, each by
/// what it is and how it is defined, so that one changed in place reads as a new one. A constraint
/// in place unchecked (Postgres's `not valid`) holds no rows to it, and is left out until validated.
#[cfg(feature = "cli")]
async fn row_checks(db: &mut Db) -> BTreeMap<String, RowCheck> {
    let mut out = BTreeMap::new();
    match db.dialect() {
        Dialect::Postgres => {
            let user = "n.nspname not in ('pg_catalog', 'information_schema') and n.nspname not like 'pg\\_%'";
            let sql = format!(
                "select 'k' as kind, c.contype::text as sub, c.conrelid::regclass::text as t, c.conname::text as name, pg_get_constraintdef(c.oid) as def,
                     case when c.contype = 'f' then c.confrelid::regclass::text end as parent,
                     case when not 0 = any (c.conkey) then (select string_agg(a.attname, chr(31)) from unnest(c.conkey) k(n) join pg_attribute a on a.attrelid = c.conrelid and a.attnum = k.n) end as cols,
                     (select string_agg(a.attname, chr(31)) from unnest(c.confkey) k(n) join pg_attribute a on a.attrelid = c.confrelid and a.attnum = k.n) as refs,
                     c.contype = 'c' and c.connoinherit as only
                   from pg_constraint c join pg_class r on r.oid = c.conrelid join pg_namespace n on n.oid = r.relnamespace
                   where c.contype in ('c', 'f', 'u', 'p') and c.convalidated and c.conislocal and c.conparentid = 0 and r.relkind in ('r', 'p') and {user}
                 union all
                 select 'i', '', i.indrelid::regclass::text, ic.relname::text, pg_get_indexdef(i.indexrelid), null,
                     case when i.indpred is null and not 0 = any (i.indkey::int2[]) then (select string_agg(a.attname, chr(31)) from unnest(i.indkey::int2[]) k(n) join pg_attribute a on a.attrelid = i.indrelid and a.attnum = k.n) end,
                     null, false
                   from pg_index i join pg_class ic on ic.oid = i.indexrelid join pg_class r on r.oid = i.indrelid join pg_namespace n on n.oid = r.relnamespace
                   where i.indisunique and i.indisvalid and r.relkind in ('r', 'p') and not r.relispartition and {user}
                     and not exists (select 1 from pg_constraint x where x.conindid = i.indexrelid and x.conrelid = i.indrelid and x.contype in ('p', 'u', 'x'))
                 union all
                 select 'n', '', r.oid::regclass::text, quote_ident(a.attname), '', null, a.attname::text, null, false
                   from pg_attribute a join pg_class r on r.oid = a.attrelid join pg_namespace n on n.oid = r.relnamespace
                   where a.attnotnull and a.attnum > 0 and not a.attisdropped and r.relkind in ('r', 'p') and not r.relispartition and {user}
                     and not exists (select 1 from pg_constraint x where x.conrelid = r.oid and x.contype = 'n' and not x.convalidated and a.attnum = any (x.conkey))"
            );
            for r in db.rows(&sql).await.unwrap_or_default() {
                let kind = match (r.text("kind"), r.text("sub")) {
                    ("k", "u" | "p") | ("i", _) => RowCheckKind::Unique,
                    ("k", _) => RowCheckKind::Constraint,
                    _ => RowCheckKind::NotNull,
                };
                let key = format!("{} {} {} {} {}", r.text("kind"), r.text("sub"), r.text("t"), r.text("name"), r.text("def"));
                let shown = match kind {
                    RowCheckKind::NotNull => format!("{}.{}", shown_name(&format!("table {}", r.text("t"))), r.text("name")),
                    _ => r.text("name").to_string(),
                };
                let list = |v: Option<&str>| v.map(|v| v.split('\u{1f}').map(String::from).collect::<Vec<_>>());
                let parent = r.get("parent").map(|p| (p.to_string(), list(r.get("refs")).unwrap_or_default()));
                let check = RowCheck { table: r.text("t").to_string(), name: r.text("name").to_string(), shown, ident: r.text("t").to_string(), columns: list(r.get("cols")), parent, only: r.flag("only"), kind };
                out.insert(key, check);
            }
        }
        Dialect::Mysql => {
            let bt = |s: &str| format!("`{}`", s.replace('`', "``"));
            let reads = [
                "select c.table_name as t, c.column_name as name from information_schema.columns c join information_schema.tables x on x.table_schema = c.table_schema and x.table_name = c.table_name
                   where c.table_schema = database() and x.table_type = 'BASE TABLE' and c.is_nullable = 'NO'",
                "select tc.table_name as t, tc.constraint_name as name, cc.check_clause as def from information_schema.table_constraints tc join information_schema.check_constraints cc
                   on cc.constraint_schema = tc.constraint_schema and cc.constraint_name = tc.constraint_name
                   where tc.table_schema = database() and tc.constraint_type = 'CHECK' and tc.enforced = 'YES'",
                "select s.table_name as t, s.index_name as name, group_concat(s.column_name order by s.seq_in_index separator '\u{1f}') as def
                   from information_schema.statistics s where s.table_schema = database() and s.non_unique = 0
                   group by s.table_name, s.index_name having sum(s.column_name is null or s.sub_part is not null) = 0",
                "select k.table_name as t, k.constraint_name as name, k.referenced_table_name as parent,
                     group_concat(k.column_name order by k.ordinal_position separator '\u{1f}') as def,
                     group_concat(k.referenced_column_name order by k.ordinal_position separator '\u{1f}') as refs
                   from information_schema.key_column_usage k where k.table_schema = database() and k.referenced_table_name is not null
                   group by k.table_name, k.constraint_name, k.referenced_table_name",
            ];
            for (n, sql) in reads.iter().enumerate() {
                for r in db.rows(sql).await.unwrap_or_default() {
                    let (table, name, def) = (r.text("t"), r.text("name"), r.get("def").unwrap_or_default());
                    let split = |v: &str| -> Vec<String> { v.split('\u{1f}').map(bt).collect() };
                    let kind = match n {
                        0 => RowCheckKind::NotNull,
                        1 => RowCheckKind::Rows(format!("select * from {} where not ({});", bt(table), crate::diff::mysql::stored_text(def))),
                        2 => RowCheckKind::Keys(split(def)),
                        _ => RowCheckKind::Rows(orphans_of(&bt(table), &split(def), &bt(r.text("parent")), &split(r.get("refs").unwrap_or_default()), false)),
                    };
                    let key = format!("{n} {table} {name} {def} {}", r.get("parent").unwrap_or_default());
                    let shown = if n == 0 { format!("{table}.{name}") } else { name.to_string() };
                    let raw = |v: &str| -> Vec<String> { v.split('\u{1f}').map(|c| c.to_lowercase()).collect() };
                    let columns = match n {
                        0 => Some(vec![name.to_lowercase()]),
                        // The columns a check's clause names, each in backticks as MySQL stores it.
                        1 => {
                            // Its quoted identifiers, as MySQL's lexer reads them: never a backtick inside a string.
                            let clause = format!("select {}", crate::diff::mysql::stored_text(def));
                            let named: Vec<String> = split_statements(&clause, Dialect::Mysql).iter().flat_map(|s| s.tokens.iter()).filter(|t| t.kind == crate::sql::TokenKind::Quoted && t.text.starts_with('`')).map(|t| unquoted(&t.text, Dialect::Mysql).to_lowercase()).collect();
                            (!named.is_empty()).then_some(named)
                        }
                        _ => Some(raw(def)),
                    };
                    let parent = r.get("parent").map(|p| (p.to_string(), raw(r.get("refs").unwrap_or_default())));
                    let check = RowCheck { table: bt(table), name: if n == 0 { bt(name) } else { name.to_string() }, shown, ident: table.to_string(), columns, parent, only: false, kind };
                    out.insert(key, check);
                }
            }
        }
        Dialect::Sqlite => {}
    }
    out
}

/// What makes a table the same one, and each of its columns, by name, lowercased.
#[cfg(feature = "cli")]
#[derive(PartialEq)]
struct TableIdentity {
    table: String,
    columns: BTreeMap<String, String>,
}

/// Each table of a shadow by the name its checks give it, with what makes it the same table: on
/// Postgres its oid and its partitions or children, and each column's number and type; on MySQL each
/// column's type, since its catalog has no identity for a table, and a column dropped and added again
/// is told by the statement that drops it.
#[cfg(feature = "cli")]
async fn table_identities(db: &mut Db) -> BTreeMap<String, TableIdentity> {
    let (tables, columns) = match db.dialect() {
        Dialect::Postgres => {
            let user = "n.nspname not in ('pg_catalog', 'information_schema') and n.nspname not like 'pg\\_%' and c.relkind in ('r', 'p')";
            (
                format!(
                    "select c.oid::regclass::text as t, c.oid::text || ' ' || coalesce((select string_agg(i.inhrelid::text, ',' order by i.inhrelid) from pg_inherits i where i.inhparent = c.oid), '') as i
                       from pg_class c join pg_namespace n on n.oid = c.relnamespace where {user}"
                ),
                format!(
                    "select c.oid::regclass::text as t, a.attname::text as c, a.attnum || '/' || a.atttypid || '/' || a.atttypmod as i
                       from pg_attribute a join pg_class c on c.oid = a.attrelid join pg_namespace n on n.oid = c.relnamespace
                       where a.attnum > 0 and not a.attisdropped and {user}"
                ),
            )
        }
        Dialect::Mysql => (
            "select table_name as t, '' as i from information_schema.tables where table_schema = database() and table_type = 'BASE TABLE'".to_string(),
            "select table_name as t, lower(column_name) as c, column_type as i from information_schema.columns where table_schema = database()".to_string(),
        ),
        Dialect::Sqlite => return BTreeMap::new(),
    };
    let mut out: BTreeMap<String, TableIdentity> = db.rows(&tables).await.unwrap_or_default().iter().map(|r| (r.text("t").to_string(), TableIdentity { table: r.text("i").to_string(), columns: BTreeMap::new() })).collect();
    for r in db.rows(&columns).await.unwrap_or_default() {
        if let Some(t) = out.get_mut(r.text("t")) {
            t.columns.insert(r.text("c").to_string(), r.text("i").to_string());
        }
    }
    out
}

/// What `check` says of the target's rows, where some break it: a finding that fails the deploy,
/// with the query that lists them. Its query is read from the shadow `lane`, where it is in place. A
/// table or column the target doesn't have yet holds no rows to break it, and says nothing.
#[cfg(feature = "cli")]
async fn broken_rows(target: &mut Db, lane: &mut Db, check: &RowCheck, timed: &mut bool) -> Option<Finding> {
    let breach = |kind| Breach { kind, name: check.name.clone(), table: check.table.clone(), column: None, rows: None };
    let (query, repeats) = match &check.kind {
        RowCheckKind::Constraint => {
            let query = pg_constraint_rows(lane, &breach(Breaks::Check)).await?.query?;
            // A check its table's children don't take holds the table's own rows alone.
            let query = if check.only { query.replacen(&format!("from {} ", check.table), &format!("from only {} ", check.table), 1) } else { query };
            (query, false)
        }
        RowCheckKind::Unique => (duplicates_query(lane, &breach(Breaks::Unique)).await?, true),
        RowCheckKind::NotNull => (format!("select * from {} where {} is null;", check.table, check.name), false),
        RowCheckKind::Rows(q) => (q.clone(), false),
        RowCheckKind::Keys(cols) => {
            let set: Vec<String> = cols.iter().map(|c| format!("{c} is not null")).collect();
            (format!("select {}, count(*) from {} where {} group by {} having count(*) > 1;", cols.join(", "), check.table, set.join(" and "), cols.join(", ")), true)
        }
    };
    let n = read_count(target, &query, 1000, timed).await?;
    if n == 0 {
        return None;
    }
    let count = if n > 1000 { "over 1000".to_string() } else { n.to_string() };
    let shown = &check.shown;
    let what = match (&check.kind, repeats) {
        (RowCheckKind::NotNull, _) => format!("fails on this database: {count} {} nulls in {shown}", if n == 1 { "row has" } else { "rows have" }),
        (_, true) => format!("fails on this database: {count} {} in {shown}", if n == 1 { "value repeats" } else { "values repeat" }),
        _ => format!("fails on this database: {count} {} {shown}", if n == 1 { "row breaks" } else { "rows break" }),
    };
    Some(Finding { what, fails: true, query: Some(query) })
}

/// `reylo db wipe`: deletes every row of the project's tables, and nothing else: the schema and
/// the history stay, and identities and sequences start again. The tables are those the comparison
/// reads, so a platform's own (Supabase's auth.users) keep theirs.
#[cfg(feature = "cli")]
pub async fn db_wipe(opts: &Options, in_ci: bool, log: Log<'_>) -> Result<i32, Error> {
    if in_ci {
        return Ok(refused_in_ci("reylo db wipe", log));
    }
    if let Some(m) = missing(opts)? {
        return Ok(refuse(&[Problem::new("no-database", format!("There's {m}"))], log));
    }
    let mut db = Db::open(opts).await?;
    let had = table_rows(&mut db).await.map(|counts| counts.values().filter(|n| **n > 0).count());
    let result = wipe_rows_on(&mut db).await;
    db.close().await;
    let tables = result?;
    let had = match had? {
        n if n == tables => String::new(),
        0 => " (none had rows)".to_string(),
        n => format!(" ({n} had rows)"),
    };
    log(&format!("{} Deleted every row of {} table{} on {}{had}", paint("green", "✓"), tables, if tables == 1 { "" } else { "s" }, shown_url(&opts.url)));
    log(&note("The schema and the history stay"));
    Ok(0)
}

#[cfg(feature = "cli")]
async fn wipe_rows_on(db: &mut Db) -> Result<usize, Error> {
    let d = db.dialect();
    let names: Vec<String> = match d {
        Dialect::Postgres => {
            let schema = crate::diff::read(db).await?;
            schema.objects.values().filter(|o| o.kind == Some(crate::diff::Kind::Table) && !o.extra.iter().any(|l| l.starts_with("shape foreign"))).filter_map(|o| o.id.strip_prefix("table ").map(String::from)).collect()
        }
        Dialect::Mysql => db
            .rows(&format!("select table_name as n from information_schema.tables where table_schema = database() and table_type = 'BASE TABLE' and {}", crate::catalog::not_history("table_name")))
            .await?
            .iter()
            .map(|r| format!("`{}`", r.text("n").replace('`', "``")))
            .collect(),
        // A virtual table's own tables (an FTS5 index's, an R*Tree's) are its to empty, and a libSQL
        // vector index's are libSQL's: deleting from them directly breaks it.
        Dialect::Sqlite => db
            .rows(&format!(
                "select name as n from pragma_table_list where schema = 'main' and type in ('table', 'virtual') and name not like 'sqlite_%' and {} and name {} and {}",
                crate::catalog::not_history("name"),
                crate::catalog::NOT_TURSO_OWN,
                crate::catalog::NOT_LIBSQL_VECTOR_OWN
            ))
            .await?
            .iter()
            .map(|r| format!("\"{}\"", r.text("n").replace('"', "\"\"")))
            .collect(),
    };
    if names.is_empty() {
        return Ok(0);
    }
    match d {
        Dialect::Postgres => db.exec(&format!("truncate table {} restart identity", names.join(", "))).await?,
        Dialect::Mysql => {
            db.exec("set foreign_key_checks = 0").await?;
            let mut result = Ok(());
            for n in &names {
                result = db.exec(&format!("truncate table {n}")).await;
                if result.is_err() {
                    break;
                }
            }
            db.exec("set foreign_key_checks = 1").await?;
            result?
        }
        Dialect::Sqlite => {
            db.exec("pragma foreign_keys = off").await?;
            // A full-text table whose content is another table's, or none, is emptied by its own
            // command, after the tables (its content among them) are: deleting from it reads the
            // content it indexes, and a contentless one refuses a delete.
            let virtual_sql: std::collections::BTreeMap<String, String> = db
                .rows("select name, sql from sqlite_schema where type = 'table' and sql like 'create virtual table%'")
                .await?
                .iter()
                .map(|r| (format!("\"{}\"", r.text("name").replace('"', "\"\"")), r.text("sql").to_string()))
                .collect();
            // A table with a libSQL vector index is emptied row by row: emptied whole, as a delete with
            // no condition does, it keeps the index's rows, and the next insert fails on them.
            let vector_indexed: std::collections::BTreeSet<String> = db
                .rows("select distinct tbl_name as t from sqlite_schema where type = 'index' and sql like '%libsql_vector_idx%'")
                .await?
                .iter()
                .map(|r| format!("\"{}\"", r.text("t").replace('"', "\"\"")))
                .collect();
            let module = regex::Regex::new(r"(?i)\busing\s+(\w+)").unwrap();
            let content = regex::Regex::new(r#"(?i)\bcontent\s*=\s*(?:'([^']*)'|"([^"]*)"|(\w+))"#).unwrap();
            let mut sql = String::new();
            let mut after = String::new();
            for n in &names {
                let Some(text) = virtual_sql.get(n) else {
                    sql.push_str(&format!("delete from {n}{};", if vector_indexed.contains(n) { " where true" } else { "" }));
                    continue;
                };
                let fts = module.captures(text).map(|c| c[1].to_lowercase()).filter(|m| m.starts_with("fts"));
                let of = content.captures(text).map(|c| c.get(1).or(c.get(2)).or(c.get(3)).map_or("", |m| m.as_str()).to_string());
                match (fts.as_deref(), of.as_deref()) {
                    (Some("fts5"), Some(_)) => after.push_str(&format!("insert into {n}({n}) values ('delete-all');")),
                    (Some(_), Some("")) => after.push_str(&format!("drop table {n}; {text};")),
                    (Some(_), Some(_)) => after.push_str(&format!("insert into {n}({n}) values ('rebuild');")),
                    _ => sql.push_str(&format!("delete from {n};")),
                }
            }
            sql.push_str(&after);
            let has_sequence = db.rows("select count(*) > 0 as ok from sqlite_schema where name = 'sqlite_sequence'").await?.first().is_some_and(|r| r.flag("ok"));
            // The counters of the tables it empties: a history table keeps its own.
            let reset = if has_sequence { format!("delete from sqlite_sequence where {};", crate::catalog::not_history("name")) } else { String::new() };
            let turso = crate::catalog::holds_turso_tables(db).await?;
            let restart = crate::catalog::turso_sequences(db, turso, true).await?;
            let result = db.script(&format!("begin; {sql} {reset} {restart} commit;")).await;
            if result.is_err() {
                let _ = db.exec("rollback").await;
            }
            db.exec("pragma foreign_keys = on").await?;
            result?
        }
    }
    Ok(names.len())
}

/// `reylo migrate history push`: rewrites the database's history to the migrations its schema
/// already has, running nothing: the longest run of migrations, in their order, whose replay in a
/// shadow database equals the live database. Those after it stay pending, and so do the migrations
/// with data statements at its end, which a schema can't show ran. For a database changed by hand
/// (a hotfix, then written as a migration), and one whose history was lost or kept by another tool.
#[cfg(feature = "cli")]
pub async fn history_push(opts: &Options, log: Log<'_>) -> Result<i32, Error> {
    history_push_as(opts, false, log).await
}

/// `history push`, or `init`'s recording of init.sql (`init`), whose refusal leaves no files to
/// edit, so it suggests none.
#[cfg(feature = "cli")]
async fn history_push_as(opts: &Options, init: bool, log: Log<'_>) -> Result<i32, Error> {
    if let Some(code) = refuse_lock_file(opts, log) {
        return Ok(code);
    }
    if let Some(m) = missing(opts)? {
        return Ok(refuse(&[Problem::new("no-database", format!("There's {m}"))], log));
    }
    let (mut db, mut server, swept) = match open(opts, log).await? {
        Ok(opened) => opened,
        Err(code) => return Ok(code),
    };
    swept_note(&server, swept, log);
    let result = history_push_on(&mut db, &mut server, opts, init, log).await;
    db.close().await;
    server.close().await;
    result
}

#[cfg(feature = "cli")]
async fn history_push_on(db: &mut Db, server: &mut Server, opts: &Options, init: bool, log: Log<'_>) -> Result<i32, Error> {
    let _held = lock(db, opts).await?;
    let d = db.dialect();
    let (files, strays) = opts.migrations()?;
    let before_rows = read_history(db).await?;
    let schema_sql = opts.schema_text();
    let mut problems = file_problems(opts, &files, &strays, schema_sql.as_deref(), d);
    problems.extend(unreadable_grants(db, opts, &files, None).await?);
    problems.extend(missing_dir(opts, &before_rows));
    if !problems.is_empty() {
        return Ok(refuse(&problems, log));
    }
    // The history is the lock's; its drafts are development's alone.
    let problems: Vec<Problem> = lock_problems(opts, &files).into_iter().filter(|p| !p.is_drafts()).collect();
    if !problems.is_empty() {
        return Ok(refuse(&problems, log));
    }
    let l = read_lock(opts).unwrap_or_default();
    let order: Vec<Migration> = if lock_free(opts) { files.clone() } else { l.entries.iter().filter_map(|e| files.iter().find(|f| f.name == e.name).cloned()).collect() };
    let live = crate::catalog::without_comments(&catalog(db).await?);
    let texts: Vec<&str> = order.iter().map(|f| f.sql.as_str()).chain(schema_sql.as_deref()).collect();
    let mut shadows = Shadows::new(server, &opts.url, &opts.root, &opts.cache, &texts).await;
    // Everything agrees but the history it writes: the migrations reach schema.sql.
    let all = shadows.lane();
    shadows.push(all, order.iter().map(Step::of));
    let agreement = match shadows.lines(all).await {
        Ok(Ok(made)) => match schema_sql.as_deref() {
            Some(sql) => schema_problems(&mut shadows, all, &made, sql, &shown(&opts.schema)).await,
            None => Ok(vec![]),
        },
        Ok(Err((m, error))) => Ok(vec![Problem::new("replay-fails", format!("Replaying {m} in a shadow database fails")).note(error)]),
        Err(e) => Err(e),
    };
    let agreement = match agreement {
        Ok(problems) => problems,
        Err(e) => {
            shadows.finish().await;
            return Err(e);
        }
    };
    if !agreement.is_empty() {
        shadows.finish().await;
        return Ok(refuse(&agreement, log));
    }
    // The run the database's history says it ran, where that is how the order starts.
    let applied: Vec<&HistoryRow> = before_rows.iter().filter(|r| r.applied).collect();
    let history_run = (!applied.is_empty() && applied.len() <= order.len() && applied.iter().zip(&order).all(|(r, f)| r.name == f.name && same_statements(&r.sql, &f.sql, d))).then_some(applied.len());
    // The replay after each migration, from none: the longest run that equals the database, and
    // else the one closest to it: the history's run where it is as close as any, since a migration
    // that changes no schema leaves the run before it just as close.
    let replayed = async {
        let lane = shadows.lane();
        if let Err((_, e)) = shadows.open(lane).await? {
            return Ok(Err(e));
        }
        shadows.taint(lane);
        let mut matches: Vec<usize> = Vec::new();
        let mut closest: (usize, usize, Vec<String>) = (usize::MAX, 0, vec![]);
        let mut at_history: Option<(usize, Vec<String>)> = None;
        let mut stages: Vec<Vec<String>> = Vec::new();
        for k in 0..=order.len() {
            let now = crate::catalog::without_comments(&catalog(shadows.db(lane)).await?);
            stages.push(now.clone());
            if crate::catalog::without_extra_children(&live, &now) == now {
                matches.push(k);
            } else {
                let (a, b) = difference(&now, &live);
                if Some(k) == history_run {
                    at_history = Some((a.len() + b.len(), now.clone()));
                }
                if a.len() + b.len() < closest.0 {
                    closest = (a.len() + b.len(), k, now);
                }
            }
            if k == order.len() {
                break;
            }
            if let Err(e) = run(shadows.db(lane), &order[k]).await {
                return Ok(Err(format!("{} fails when replayed in a shadow database: {e}", order[k].name)));
            }
        }
        if let (Some(k), Some((distance, lines))) = (history_run, at_history) {
            if distance == closest.0 {
                closest = (distance, k, lines);
            }
        }
        Ok::<_, Error>(Ok((matches, closest, stages)))
    }
    .await;
    let (matches, closest, stages) = match replayed {
        Ok(Ok(found)) => found,
        Ok(Err(e)) => {
            shadows.finish().await;
            return Ok(refuse(&[Problem::new("replay-fails", "The migrations can't be replayed").note(e)], log));
        }
        Err(e) => {
            shadows.finish().await;
            return Err(e);
        }
    };
    // A migration at the end of the run that changes data and nothing a schema shows can't be told
    // to have run: it stays pending, to run.
    // One the history says ran, as written, stays recorded.
    let ran = |m: &Migration| before_rows.iter().any(|r| r.applied && r.name == m.name && same_statements(&r.sql, &m.sql, d));
    let matched = matches.last().copied().map(|mut n| {
        while n > 0 && !data_statements(&order[n - 1].sql, d).is_empty() && matches.contains(&(n - 1)) && !ran(&order[n - 1]) {
            n -= 1;
        }
        n
    });
    // A later migration that makes all this database has since the run its history ends at, which is
    // where deploy starts, ran here before those ahead of it: it is recorded, and the order made the
    // one this database ran, with it right after that run and the rest after it ([`unmovable`] says
    // where it can't be). It is looked for too where a run that changes data also matches, which
    // can't be recorded by its schema alone. Where it can't move, the change is undone instead, for
    // deploy to run the migrations in their order.
    let unproven = |n: usize| history_run.is_some_and(|h| order[h.min(n)..n].iter().any(|m| !data_statements(&m.sql, d).is_empty()));
    let mut moved: Option<(Vec<Migration>, usize, String, String)> = None;
    let mut unskipped: Option<String> = None;
    if let Some(h) = history_run.filter(|&h| !init && h < order.len() && matched.is_none_or(unproven)) {
        let found = match behind(&mut shadows, &order[..h], &order[h..], &live).await {
            Ok(Some(j)) => unmovable(&mut shadows, &order[..h], &order[h..], j, d).await.map(|why| Some((j, why))),
            other => other.map(|_| None),
        };
        match found {
            Ok(None) => {}
            Ok(Some((j, None))) => {
                let (it, ahead) = (&order[h + j], &order[h..h + j]);
                let reordered: Vec<Migration> = order[..h].iter().chain([it]).chain(ahead).chain(&order[h + j + 1..]).cloned().collect();
                moved = Some((reordered, h + 1, format!("{} now runs before {}, as it did here", it.name, names_of(ahead)), names_of(ahead)));
            }
            Ok(Some((j, Some(why)))) => {
                let problem = behind_problem(&mut shadows, db, &opts.url, &order[..h], &order[h + j], &order[h..h + j], &live, Some(why)).await;
                shadows.finish().await;
                return Ok(refuse(&[problem], log));
            }
            Err(e) => {
                shadows.finish().await;
                return Err(e);
            }
        }
    }
    // A database that ran every migration of a run but one, as one another tool kept can have: the
    // one it skipped is moved after that run, as this database will run it, and stays pending, where
    // it can move there ([`unskippable`] says where it can't); the run is recorded. Where it can't,
    // its change is made here instead, and the migrations recorded in their order.
    if moved.is_none() && matched.is_none() && !init && (applied.is_empty() || history_run.is_some()) {
        let found = match skipped(&mut shadows, &order, &stages, history_run.unwrap_or(0), &live, d).await {
            Ok(Some((k, m))) => unskippable(&mut shadows, &order, k, m, d).await.map(|why| Some((k, m, why))),
            other => other.map(|_| None),
        };
        match found {
            Ok(None) => {}
            Ok(Some((k, m, None))) => {
                let (it, past) = (&order[k], &order[k + 1..m]);
                moved = Some((moved_after(&order, k, m), m - 1, format!("{} now runs after {}, which this database ran without it", it.name, run_of(past)), it.name.clone()));
            }
            Ok(Some((k, m, Some((why, makeable))))) => {
                if makeable {
                    shadows.finish().await;
                    return Ok(refuse(&[skipped_problem(&order[k], &order[k + 1..m], &why, d)], log));
                }
                unskipped = Some(format!("It has what they make without {}. {why}", order[k].name));
            }
            Err(e) => {
                shadows.finish().await;
                return Err(e);
            }
        }
    }
    let (order, matched, moved) = match moved {
        Some((reordered, n, written, first)) => (reordered, Some(n), Some((written, first))),
        None => (order, matched, None),
    };
    let Some(n) = matched else {
        // The closest run, and the change from what it makes to this database, as SQL.
        let (_, k, lines) = closest;
        let lane = shadows.lane();
        shadows.push(lane, order[..k].iter().map(Step::of));
        let mut sql = None;
        if let Ok(Ok(())) = shadows.open(lane).await {
            shadows.taint(lane);
            sql = plan_text(shadows.db(lane), db, None).await;
        }
        shadows.finish().await;
        let closest = if k == 0 { "The closest is with no migration run, which differs by:".to_string() } else { format!("The closest is after {}, which differs by:", order[k - 1].name) };
        let problem = Problem::new("history-push-no-match", "No run of the migrations makes this database's schema").note(closest);
        let problem = match sql {
            Some(sql) => problem.sql(&sql),
            None => problem.lines(std::iter::once(String::new()).chain(changed_lines(&lines, &live, d)).chain([String::new()])),
        };
        let problem = match unskipped {
            Some(why) => problem.note(why),
            None => problem,
        };
        if init {
            return Ok(refuse(&[problem], log));
        }
        let mut problem = match schema_sql {
            Some(_) => problem.fix(format!("To keep it, write it into {}, then its migration", shown(&opts.schema)), &["reylo migrate new <name>"]),
            None => problem.fix("To keep it, write it as a migration", &["reylo migrate new <name>"]),
        };
        if k == 0 {
            problem = problem.fix("To start a new project from it instead, in a directory of its own", &["reylo migrate init"]);
        }
        return Ok(refuse(&[problem], log));
    };
    shadows.finish().await;
    // The lock takes the order first: a history written after it can be written again, where one
    // written before a lock that never came would be out of its order.
    let relocked = moved.is_some() && !lock_free(opts);
    if relocked {
        let texts: Vec<(String, String)> = order.iter().map(|f| (f.name.clone(), normalized(&f.sql))).collect();
        let entries = lock::chain(texts.iter().map(|(n, t)| (n.as_str(), t.as_str())));
        // The order moved ends as the one schema.sql was shown to match, above.
        let schema = match schema_sql.as_deref() {
            Some(sql) => Some(lock::schema_sum(&entries.last().map(|e| e.sum.clone()).unwrap_or_default(), sql)),
            None => l.schema.clone(),
        };
        lock::write(&opts.lock, &lock::Lock { schema, entries })?;
    }
    let recorded = &order[..n];
    let before = before_rows;
    ensure_history(db).await?;
    // The schema it records, which running nothing leaves as it is, on the last row.
    let lines = catalog(db).await?;
    let value = fingerprint_of(db, &lines).await?;
    // The rows that stay as they are, with the time each ran: the history's start, as far as it
    // agrees with what is recorded, in its order. The rest is written again, in order, after them.
    let kept = before.iter().zip(recorded).take_while(|(r, f)| r.applied && r.name == f.name && same_statements(&r.sql, &f.sql, d)).count();
    db.exec(if d == Dialect::Mysql { "start transaction" } else { "begin" }).await?;
    let written = async {
        let names: Vec<String> = recorded[..kept].iter().map(|f| crate::db::literal(&f.name)).collect();
        if names.is_empty() {
            db.exec(&format!("delete from {HISTORY_TABLE}")).await?;
        } else {
            db.exec(&format!("delete from {HISTORY_TABLE} where name not in ({})", names.join(", "))).await?;
        }
        for f in &recorded[kept..] {
            insert_row(db, f, true, None).await?;
        }
        if let Some(last) = recorded.last() {
            set_fingerprint(db, &last.name, &value).await?;
        }
        db.exec("commit").await
    }
    .await;
    if let Err(e) = written {
        let _ = db.exec("rollback").await;
        return Err(e);
    }
    if let Some(last) = recorded.last() {
        store_catalog(db, &last.name, &lines).await?;
    }
    let added: Vec<&Migration> = recorded.iter().filter(|f| !before.iter().any(|r| r.applied && r.name == f.name && same_statements(&r.sql, &f.sql, d))).collect();
    let taken: Vec<&HistoryRow> = before.iter().filter(|r| !recorded.iter().any(|f| f.name == r.name && same_statements(&r.sql, &f.sql, d))).collect();
    let mut rows = Rows::new(format!("Recording on {}", shown_url(&opts.url)), added.iter().map(|f| f.name.as_str()).chain(taken.iter().map(|r| r.name.as_str())));
    for f in &added {
        let data = data_statements(&f.sql, d);
        if data.is_empty() {
            rows.row(log, &paint("green", "+"), &f.name, "recorded: already in place");
        } else {
            rows.row(log, &paint("green", "+"), &f.name, &format!("recorded: {}", paint("yellow", &format!("its data statements ({}) may not have run here", data.join(", ")))));
        }
    }
    for r in &taken {
        rows.row(log, &paint("red", "-"), &r.name, "taken out of the history");
    }
    let count = |n: usize| format!("{n} migration{}", if n == 1 { "" } else { "s" });
    if added.is_empty() && taken.is_empty() && kept < recorded.len() {
        log(&format!("{} The history now lists {}, in the order they run", paint("green", "✓"), count(recorded.len())));
    } else if added.is_empty() && taken.is_empty() {
        log(&format!("{} The history already matches this database: {}", paint("green", "✓"), count(recorded.len())));
    } else if added.is_empty() {
        log(&format!("{} The history now lists {}", paint("green", "✓"), count(recorded.len())));
    } else {
        log(&format!("{} Recorded {}, running nothing", paint("green", "✓"), count(added.len())));
    }
    // A development database that ran them in the old order has what the new one makes: its history
    // takes the new order with no rewind, which would drop what they made, rows included.
    if let (Some((written, first)), true) = (&moved, relocked) {
        log(&format!("{} Wrote {}: {written}", paint("green", "✓"), shown(&opts.lock)));
        log(&then(note(format!("Development databases ran {first} first")), &[("To record the lock's new order on one, running nothing", &["reylo migrate history push"])]));
    }
    let pending = &order[n..];
    if !pending.is_empty() {
        let names = pending.iter().map(|f| f.name.as_str()).collect::<Vec<_>>().join(", ");
        log(&then(note(format!("This database hasn't run {names}")), &[(if pending.len() == 1 { "To run it" } else { "To run them" }, &["reylo migrate deploy"])]));
    }
    Ok(0)
}

/// `reylo migrate history pull`: writes each migration the database ran into its file, as it ran
/// it, from the history table, which keeps every applied migration's text: a file deleted is written
/// again, and one that reads differently to the lock is written over. A file that differs in comments
/// alone is left as it is, and so is a file the database hasn't run. The lock then lists what it ran,
/// in its order, before what the lock already listed that it hasn't run: a lock lost or merged wrong
/// is written again, and a deploy still to reach this database stays locked. Changes no database, and
/// refuses one whose tables aren't what its history makes, whose files would not make it either.
#[cfg(feature = "cli")]
pub async fn history_pull(opts: &Options, log: Log<'_>) -> Result<i32, Error> {
    if let Some(code) = refuse_lock_file(opts, log) {
        return Ok(code);
    }
    if let Some(m) = missing(opts)? {
        return Ok(refuse(&[Problem::new("no-database", format!("There's {m}"))], log));
    }
    let (mut db, mut server, swept) = match open(opts, log).await? {
        Ok(opened) => opened,
        Err(code) => return Ok(code),
    };
    swept_note(&server, swept, log);
    let result = history_as_it_stands(&mut db, &mut server, opts).await;
    db.close().await;
    server.close().await;
    let (history, problems) = result?;
    if !problems.is_empty() {
        return Ok(refuse(&problems, log));
    }
    let (files, _) = opts.migrations()?;
    let ran: Vec<&HistoryRow> = history.iter().filter(|r| r.applied).collect();
    let mut rows = Rows::new(format!("Pulling from {}", shown_url(&opts.url)), ran.iter().map(|r| r.name.as_str()));
    for r in &ran {
        let path = opts.dir.join(&r.name);
        let what = match files.iter().find(|f| f.name == r.name) {
            None => "written: it wasn't here",
            // By the lock's own reading, so that what the lock refuses as edited is written back.
            Some(f) if lock::canonical(&normalized(&f.sql)) != lock::canonical(&normalized(&r.sql)) => "written over: it differed from what ran",
            Some(_) => continue,
        };
        std::fs::create_dir_all(&opts.dir)?;
        std::fs::write(&path, &r.sql)?;
        rows.row(log, &paint("green", "↓"), &r.name, what);
    }
    if !rows.opened {
        log(&format!("{} Every migration this database ran is in {}, as it ran it", paint("green", "✓"), shown(&opts.dir)));
    }
    let left: Vec<&str> = files.iter().filter(|f| !ran.iter().any(|r| r.name == f.name)).map(|f| f.name.as_str()).collect();
    if !left.is_empty() {
        log(&paint("dim", &format!("Not run on this database, and left as they are: {}", left.join(", "))));
    }
    let before = match opts.lock_state() {
        lock::State::Lock(l) => Some(l),
        _ => None,
    };
    let mut texts: Vec<(String, String)> = ran.iter().map(|r| (r.name.clone(), normalized(&r.sql))).collect();
    for e in before.iter().flat_map(|l| &l.entries) {
        if let Some(f) = files.iter().find(|f| f.name == e.name && !ran.iter().any(|r| r.name == f.name)) {
            texts.push((f.name.clone(), normalized(&f.sql)));
        }
    }
    let entries = lock::chain(texts.iter().map(|(n, t)| (n.as_str(), t.as_str())));
    // A project with no lock keeps none: reylo migrate lock writes one.
    if !lock_free(opts) && before.as_ref().map(|l| &l.entries) != Some(&entries) {
        // What schema.sql was proven against went with the old chain's last line.
        let schema = before.filter(|l| l.entries.last() == entries.last()).and_then(|l| l.schema);
        lock::write(&opts.lock, &lock::Lock { schema, entries })?;
        log(&format!("{} Wrote {}", paint("green", "✓"), shown(&opts.lock)));
    }
    Ok(0)
}

/// A SQLite file that isn't there, which a command that only reads the database, or
/// changes what it already holds, refuses: that is no database to read, and opening it would make
/// one. What it says, where the file is missing.
fn missing(opts: &Options) -> Result<Option<String>, Error> {
    Ok(missing_file(opts)?.map(|(path, which)| format!("no {which} database at {}", path.display())))
}

/// The file a SQLite URL names, where it isn't there, with the database's name for a message.
fn missing_file(opts: &Options) -> Result<Option<(PathBuf, &'static str)>, Error> {
    missing_at(&opts.url, &opts.root)
}

fn missing_at(url: &str, root: &Path) -> Result<Option<(PathBuf, &'static str)>, Error> {
    let (path, which) = match Dialect::of_url(url)? {
        // A libSQL server's database is there for as long as the server is.
        Dialect::Sqlite if crate::libsql::is_url(url) => return Ok(None),
        Dialect::Sqlite => (crate::db::sqlite_path(url, root), "SQLite"),
        _ => return Ok(None),
    };
    Ok((path != ":memory:" && !Path::new(&path).exists()).then(|| (PathBuf::from(path), which)))
}

/// The same for a command that only reads: its error.
fn there(opts: &Options) -> Result<(), Error> {
    match missing(opts)? {
        Some(m) => Err(Error(m)),
        None => Ok(()),
    }
}

/// `reylo migrate status`: what this database ran, what's pending and what changed, and every check
/// the files can answer: their names, git's conflict markers, whether each runs atomically and holds
/// nothing a shadow can't replay (read from its text), the lock against them, and schema.sql against
/// the checksum the lock holds. It reads, writes nothing and needs no shadow. It exits 1 where the
/// database and the files aren't in step: a migration pending, cut off, edited or deleted, a change
/// made by hand, or a check that fails.
#[cfg(feature = "cli")]
pub async fn status(opts: &Options, log: Log<'_>) -> Result<i32, Error> {
    if let Some(code) = refuse_lock_file(opts, log) {
        return Ok(code);
    }
    there(opts)?;
    let mut db = Db::open(opts).await?;
    let result = status_on(&mut db, opts, log).await;
    db.close().await;
    result
}

async fn status_on(db: &mut Db, opts: &Options, log: Log<'_>) -> Result<i32, Error> {
    let d = db.dialect();
    let (files, strays) = opts.migrations()?;
    let schema_sql = opts.schema_text();
    let history = if has_history(db).await? { read_history(db).await? } else { vec![] };
    let times = applied_times(db).await?;
    let order = run_order(opts, &files);
    let pending: Vec<&Migration> = order.iter().filter(|f| !history.iter().any(|r| r.name == f.name)).collect();
    let names: Vec<&str> = history.iter().map(|r| r.name.as_str()).chain(pending.iter().map(|f| f.name.as_str())).collect();
    let mut rows = Rows::new(format!("Migrations on {}", shown_url(&opts.url)), names.iter().copied());
    // The same, for --json: each migration's state, and when it was applied.
    let mut listed: Vec<serde_json::Value> = Vec::new();
    // Whether a migration this database ran no longer agrees with its file, or was cut off.
    let mut out_of_step = false;
    for r in &history {
        // To the minute, in UTC, for a person.
        let at = times.get(&r.name).map(|t| format!("applied {} {} UTC", &t[..10], &t[11..16])).unwrap_or_else(|| "applied".into());
        let wrong = |rows: &mut Rows, log: Log<'_>, what: &str| rows.row(log, &paint("bold red", "✗"), &r.name, what);
        let state = match (r.applied, files.iter().find(|f| f.name == r.name)) {
            (false, _) => {
                wrong(&mut rows, log, "cut off before it finished");
                log(&steps(6, &[("See whether it landed", &["reylo migrate deploy --dry-run"])]));
                "cut-off"
            }
            (true, None) => {
                wrong(&mut rows, log, &format!("{at}, and deleted since"));
                log(&steps(6, &[("Write it back as it ran", &["reylo migrate history pull"])]));
                "missing"
            }
            (true, Some(f)) if !same_statements(&f.sql, &r.sql, d) => {
                wrong(&mut rows, log, &format!("{at}, and edited since"));
                log(&steps(6, &[("Take back the text it ran", &["reylo migrate history pull"])]));
                "edited"
            }
            (true, Some(_)) => {
                rows.row(log, &paint("green", "✓"), &r.name, &paint("dim", &at));
                "applied"
            }
        };
        out_of_step |= state != "applied";
        listed.push(serde_json::json!({ "name": r.name, "state": state, "applied_at": times.get(&r.name) }));
    }
    for f in &pending {
        rows.row(log, &paint("dim", "·"), &f.name, "pending");
        listed.push(serde_json::json!({ "name": f.name, "state": "pending" }));
    }
    // The files: their names, conflict markers, atomicity and what a shadow can't replay.
    let mut problems = file_problems(opts, &files, &strays, schema_sql.as_deref(), d);
    // Each check that holds, or can't be made, as a line; one that fails is a refusal, below the rest.
    let mut said: Vec<String> = Vec::new();
    // Whether anything changed the database since its last migration, as deploy would see it.
    let stored = history.iter().rfind(|r| r.applied).and_then(|r| r.fingerprint.clone());
    let mut database = if history.is_empty() { "no-history" } else { "unknown" };
    let mut changes: Vec<String> = Vec::new();
    if !history.is_empty() {
        let resumed = match stored.as_deref() {
            Some(stored) if is_pending(stored) => unchanged_since_cut_off(db, stored).await,
            _ => false,
        };
        match stored.as_deref() {
            Some(_) if resumed => {
                database = "unchanged";
                said.push(format!("{} Unchanged since a run was cut off after its last migration", paint("green", "✓")));
            }
            Some(stored) if is_pending(stored) => {
                database = "cut-off";
                problems.push(
                    Problem::new("fingerprint-cut-off", format!("A run was cut off after {} applied, before it stored this database's fingerprint", history.iter().rfind(|r| r.applied).map(|r| r.name.as_str()).unwrap_or_default()))
                        .note("So nothing shows that its schema is as that migration left it.")
                        .fix("See whether it is as its migrations make it", &["reylo migrate deploy --dry-run"])
                        .fix("Then store its fingerprint, running nothing", &["reylo migrate history push"]),
                );
            }
            Some(stored) => {
                let now_lines = catalog(db).await?;
                let now = fingerprint_of(db, &now_lines).await?;
                let how = |f: &str| f.rsplit_once(':').map(|(how, _)| how.to_string());
                if how(stored) != how(&now) {
                    said.push(uncompared_line());
                } else if stored == now {
                    database = "unchanged";
                    said.push(format!("{} Unchanged since its last migration", paint("green", "✓")));
                } else {
                    database = "changed";
                    let mut problem = Problem::new("database-changed", CHANGED_OUTSIDE);
                    if let Some(last) = history.iter().rfind(|r| r.applied) {
                        if let Some(stored) = stored_catalog(db, &last.name).await? {
                            let lines = changed_lines(&stored, &fingerprinted(&now_lines, db.dialect()), db.dialect());
                            changes = lines.iter().map(|l| l.trim().to_string()).collect();
                            problem = problem.lines(std::iter::once(String::new()).chain(lines).chain([String::new()]));
                        }
                    }
                    problems.push(
                        problem
                            .fix("On a development database, see how to keep it or undo it", &["reylo migrate dev"])
                            .fix("Elsewhere, see which migrations make it", &["reylo migrate deploy --dry-run"]),
                    );
                }
            }
            None => said.push(uncompared_line()),
        }
    }
    // The lock against the files, and schema.sql against the lock's checksum, once the lock agrees.
    let lock_problems = lock_problems(opts, &files);
    let lock_state: serde_json::Value = match opts.lock_state() {
        lock::State::Missing => {
            said.push(no_lock_line());
            "none".into()
        }
        _ if lock_problems.is_empty() => {
            said.push(lock_line(opts));
            "ok".into()
        }
        _ => lock_problems.iter().map(|p| p.json()).collect(),
    };
    // A draft leaves the checksum as it was; any other disagreement leaves it nothing to be held to.
    let stamped = if lock_problems.iter().all(Problem::is_drafts) { stamp(opts, schema_sql.as_deref()) } else { Stamp::Unreadable };
    let drafts = !lock_problems.is_empty();
    // Once the lock and the files agree, the order this database ran them in, as deploy holds it to.
    let misordered = if out_of_step || drafts {
        None
    } else {
        let applied: Vec<&HistoryRow> = history.iter().filter(|r| r.applied).collect();
        lock_order_problem(opts, &applied, &order, d)
    };
    // Deploy refuses such a database, so running what it hasn't run is offered only where it doesn't.
    let offer_run = misordered.is_none();
    problems.extend(lock_problems);
    problems.extend(misordered);
    match stamped {
        // A draft written from schema.sql makes the change the checksum doesn't hold yet, until development locks it.
        Stamp::Changed if drafts => said.push(then(note(format!("Until the drafts are locked, {} isn't held to the lock", shown(&opts.schema))), &[("To lock them", &["reylo migrate dev"])])),
        Stamp::Changed => problems.push(stamp_problem(opts)),
        Stamp::Unproven => problems.push(unproven_problem(opts)),
        _ => said.push(stamp_line(opts, stamped)),
    }
    let schema = match stamped {
        Stamp::NoSchema => "none",
        Stamp::Proven => "matched",
        Stamp::Changed => "changed",
        Stamp::NoLock | Stamp::Unproven | Stamp::Unreadable => "unproven",
    };
    // A change `migrate new` wrote as several migrations, which this database ran only part of.
    let ran: Vec<&HistoryRow> = history.iter().filter(|r| r.applied).collect();
    let (partway, partway_json) = partway_lines(db, &ran, &pending).await;
    set_result(serde_json::json!({ "migrations": listed, "database": database, "changes": changes, "lock": lock_state, "schema_sql": schema, "partway": partway_json }));
    // A project with nothing in it yet has nothing to say but how to start.
    if names.is_empty() && problems.is_empty() {
        let lead = if schema_sql.is_some() { format!("To write the first, once {} declares tables", shown(&opts.schema)) } else { "To write the first from this database".to_string() };
        log(&then(note("There are no migrations yet"), &[(lead.as_str(), &["reylo migrate new <name>"])]));
        return Ok(0);
    }
    for line in &said {
        log(line);
    }
    for line in &partway {
        log(line);
    }
    match pending.len() {
        _ if !offer_run => {}
        0 => {}
        1 => log(&then(note(format!("This database hasn't run {}", pending[0].name)), &[("To run it in development", &["reylo migrate dev"]), ("To run it in production", &["reylo migrate deploy"])])),
        n => log(&then(note(format!("This database hasn't run {n} migrations")), &[("To run them in development", &["reylo migrate dev"]), ("To run them in production", &["reylo migrate deploy"])])),
    }
    if !problems.is_empty() {
        log("");
        print_problems(&problems, log);
    }
    Ok(if problems.is_empty() && !out_of_step && pending.is_empty() { 0 } else { 1 })
}

/// The refusal of a lock that holds no checksum of schema.sql: written while the migrations didn't make it.
fn unproven_problem(opts: &Options) -> Problem {
    Problem::new("schema-sql-unproven", format!("The lock holds no checksum of {}", shown(&opts.schema)))
        .note(format!("The migrations didn't match {} when the lock was written.", shown(&opts.schema)))
        .fix("Write what they don't make as a migration, and run it in development, which writes the checksum", &["reylo migrate new <name>", "reylo migrate dev"])
}

/// When each applied migration ran, by name, in UTC: ISO 8601 to the second.
async fn applied_times(db: &mut Db) -> Result<std::collections::HashMap<String, String>, Error> {
    if !has_history(db).await? {
        return Ok(Default::default());
    }
    // As seconds since the epoch: MySQL's datetime is in the session's time zone, which this reads.
    let at = match db.dialect() {
        Dialect::Postgres => "cast(floor(extract(epoch from applied_at)) as bigint)",
        Dialect::Mysql => "floor(unix_timestamp(applied_at))",
        Dialect::Sqlite => "cast(strftime('%s', applied_at) as integer)",
    };
    let rows = db.rows(&format!("select name, {at} as at from {HISTORY_TABLE} where applied_at is not null")).await?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let stamp = time::stamp(r.text("at").parse().ok()?);
            Some((r.text("name").to_string(), format!("{}-{}-{}T{}:{}:{}Z", &stamp[0..4], &stamp[4..6], &stamp[6..8], &stamp[8..10], &stamp[10..12], &stamp[12..14])))
        })
        .collect())
}

/// What a command says in a project with no lock: the order is the files' names', and nothing
/// holds every database to one.
fn no_lock_line() -> String {
    then(note("With no lock, migrations run in name order"), &[("To write one", &["reylo migrate lock"])])
}

/// What `status` says of a lock that agrees with the files.
fn lock_line(opts: &Options) -> String {
    format!("{} The migration files match {}", paint("green", "✓"), shown(&opts.lock))
}

/// `reylo migrate lock`: writes the lock from this database's history, the order it ran its
/// migrations in, with schema.sql's checksum where the shadows show the migrations reach it. Every
/// migration it ran must be in the migrations as it ran it. This is how a project that deleted its
/// lock takes linear history back.
#[cfg(feature = "cli")]
pub async fn lock_from_history(opts: &Options, log: Log<'_>) -> Result<i32, Error> {
    if let Some(code) = refuse_lock_file(opts, log) {
        return Ok(code);
    }
    if let Some(m) = missing(opts)? {
        return Ok(refuse(&[Problem::new("no-database", format!("There's {m}"))], log));
    }
    let (mut db, mut server, swept) = match open(opts, log).await? {
        Ok(opened) => opened,
        Err(code) => return Ok(code),
    };
    swept_note(&server, swept, log);
    let result = lock_from_history_on(&mut db, &mut server, opts, log).await;
    db.close().await;
    server.close().await;
    result
}

#[cfg(feature = "cli")]
async fn lock_from_history_on(db: &mut Db, server: &mut Server, opts: &Options, log: Log<'_>) -> Result<i32, Error> {
    let d = db.dialect();
    // A lock lists what other databases ran too: written over from this one's history, they'd lose it.
    if !lock_free(opts) {
        return Ok(refuse(&[Problem::new("lock-exists", format!("A lock exists already: {}", shown(&opts.lock))).note("It lists what every database runs, which this one may not have run yet.").fix("To write it again from this database's history, delete it, then", &["reylo migrate lock"])], log));
    }
    let _held = lock(db, opts).await?;
    let (files, _) = opts.migrations()?;
    let history = if has_history(db).await? { read_history(db).await? } else { vec![] };
    let schema_sql = opts.schema_text();
    let mut problems = applied_unlike(opts, &history, &files, d);
    if history.iter().any(|r| !r.applied) {
        problems.push(Problem::new("run-cut-off", "A run on this database was cut off before it finished").fix("Finish it first", &["reylo migrate dev"]));
    }
    problems.extend(replay_problems(opts, &files, schema_sql.as_deref(), d));
    if !problems.is_empty() {
        return Ok(refuse(&problems, log));
    }
    let texts: Vec<&str> = files.iter().map(|f| f.sql.as_str()).chain(schema_sql.as_deref()).collect();
    let mut shadows = Shadows::new(server, &opts.url, &opts.root, &opts.cache, &texts).await;
    let next = locked(&mut shadows, db, opts, &files, schema_sql.as_deref()).await.map(|(next, _)| next);
    shadows.finish().await;
    let ran = history.iter().filter(|r| r.applied).count();
    match next? {
        None => log(&format!("{} The lock already lists what this database ran", paint("green", "✓"))),
        Some(next) => {
            lock::write(&opts.lock, &next)?;
            if ran == 0 {
                log(&format!("{} Wrote {}: this database has run no migrations, so it lists none yet", paint("green", "✓"), shown(&opts.lock)));
            } else {
                log(&format!("{} Wrote {}: {ran} migration{}, in the order this database ran them", paint("green", "✓"), shown(&opts.lock), if ran == 1 { "" } else { "s" }));
            }
        }
    }
    log(&stamp_line(opts, stamp(opts, schema_sql.as_deref())));
    Ok(0)
}

/// `reylo db reset`: rebuilds a development database from its migrations. Everything on it is dropped,
/// with its rows, but a platform's own objects and the extensions the project's files don't create;
/// the history is emptied; and every migration runs as `reylo migrate dev` runs it. What `dev` would
/// refuse of the files and the lock, it refuses first, with nothing dropped. Never in CI.
#[cfg(feature = "cli")]
pub async fn db_reset(opts: &Options, in_ci: bool, log: Log<'_>) -> Result<i32, Error> {
    if let Some(code) = refuse_lock_file(opts, log) {
        return Ok(code);
    }
    if in_ci {
        return Ok(refused_in_ci("reylo db reset", log));
    }
    let (mut db, mut server, swept) = match open(opts, log).await? {
        Ok(opened) => opened,
        Err(code) => return Ok(code),
    };
    swept_note(&server, swept, log);
    let result = reset_on(&mut db, &mut server, opts, log).await;
    db.close().await;
    server.close().await;
    result
}

#[cfg(feature = "cli")]
async fn reset_on(db: &mut Db, server: &mut Server, opts: &Options, log: Log<'_>) -> Result<i32, Error> {
    // What dev checks before it changes anything, over the history the reset leaves it, which is none;
    // and a migrations directory that is gone, which this database ran migrations from.
    let (files, strays) = opts.migrations()?;
    let mut problems = file_problems(opts, &files, &strays, opts.schema_text().as_deref(), db.dialect());
    problems.extend(unreadable_grants(db, opts, &files, opts.schema_text().as_deref()).await?);
    problems.extend(missing_dir(opts, &read_history(db).await?));
    if problems.is_empty() {
        problems.extend(dev_lock_notes(opts, &files, &[]).err());
    }
    if !problems.is_empty() {
        return Ok(refuse(&problems, log));
    }
    // Emptied, a file Turso's engine wrote holds none of its tables until the migrations make the
    // tables again, each with its sequence.
    let turso = crate::catalog::holds_turso_tables(db).await?;
    match emptied(db, server, opts, log).await? {
        0 => {}
        code => return Ok(code),
    }
    match dev_on(db, server, 0, opts, "", log).await? {
        0 => {}
        code => return Ok(code),
    }
    crate::catalog::keep_turso_sequences_as(db, turso).await?;
    let ran = read_history(db).await.map(|h| h.iter().filter(|r| r.applied).count()).unwrap_or(0);
    if ran > 0 {
        log(&format!("{} Rebuilt from {ran} migration{}", paint("green", "✓"), if ran == 1 { "" } else { "s" }));
    }
    Ok(0)
}

/// Takes the database to what an empty shadow holds, and forgets its history: in the same transaction
/// where the database has transactional DDL, and on MySQL, which commits each statement, the history
/// first, so that a reset cut off leaves migrations to run again, never ones recorded over nothing.
#[cfg(feature = "cli")]
async fn emptied(db: &mut Db, server: &mut Server, opts: &Options, log: Log<'_>) -> Result<i32, Error> {
    let _held = lock(db, opts).await?;
    let mut shadows = Shadows::new(server, &opts.url, &opts.root, &opts.cache, &[]).await;
    let lane = shadows.lane();
    let empty = async {
        if let Err((_, e)) = shadows.open(lane).await? {
            return Err(Error(format!("An empty shadow database can't be made: {e}")));
        }
        let lines = catalog(shadows.db(lane)).await?;
        let target = read_target(shadows.db(lane), None).await?;
        Ok((lines, target))
    }
    .await;
    shadows.finish().await;
    let (lines, target) = empty?;
    let counts = table_rows(db).await?;
    let d = db.dialect();
    // The extensions the project's files create go too: they create them again.
    let mut then: Vec<String> = if d == Dialect::Postgres { created_extensions(opts).iter().map(|x| format!("drop extension if exists {}", crate::db::ident(x))).collect() } else { vec![] };
    let had_history = has_history(db).await? && !read_history(db).await?.is_empty();
    if has_history(db).await? {
        let forget = format!("delete from {HISTORY_TABLE}");
        if d == Dialect::Mysql {
            db.exec(&forget).await?;
        } else {
            then.push(forget);
        }
    }
    match take_to(db, &target, &then, &lines, None).await? {
        Err(Unreached::Refused(problems)) => return Ok(refuse(&problems, log)),
        Err(Unreached::Differs(now, expect)) => {
            let problem = Problem::new("reset-incomplete", "Reylo can't drop everything on this database").lines(describe_diff("this database", &now, "an empty database", &expect));
            if d != Dialect::Mysql {
                return Ok(refuse(&[problem], log));
            }
            print_problems(&[problem.note("MySQL commits each statement, so it is part of the way there, and its history is empty.").fix("Run every migration again", &["reylo migrate dev"])], log);
            return Ok(1);
        }
        Ok((_, plan)) => {
            log(&format!("Resetting {}", shown_url(&opts.url)));
            let dropped = dropped_items(&plan, &counts);
            match (dropped, had_history) {
                (Some(dropped), true) => log(&format!("  {} {dropped} and emptied the history", paint("yellow", "!"))),
                (Some(dropped), false) => log(&format!("  {} {dropped}", paint("yellow", "!"))),
                (None, true) => log(&format!("  {} Emptied the history", paint("yellow", "!"))),
                (None, false) => {}
            }
        }
    }
    Ok(0)
}

/// The database's history, and where its tables aren't what the migrations it ran make, as it ran
/// them, why: what was changed by hand, with the ways to record it.
#[cfg(feature = "cli")]
async fn history_as_it_stands(db: &mut Db, server: &mut Server, opts: &Options) -> Result<(Vec<HistoryRow>, Vec<Problem>), Error> {
    let history = read_history(db).await?;
    // The history replays as it ran, which no file's check saw.
    let problems = history_replay_problems(&history, &[], db.dialect());
    if !problems.is_empty() {
        return Ok((history, problems));
    }
    let leftovers = leftovers(db, &history).await?;
    let live: Vec<String> = catalog(db).await?.into_iter().filter(|l| !leftovers.iter().any(|x| l.starts_with(&x.line))).collect();
    let ran: Vec<&HistoryRow> = history.iter().filter(|r| r.applied).collect();
    let texts: Vec<&str> = ran.iter().map(|r| r.sql.as_str()).collect();
    let mut shadows = Shadows::new(server, &opts.url, &opts.root, &opts.cache, &texts).await;
    let lane = shadows.lane();
    shadows.push(lane, ran.iter().map(|r| Step::migration(&r.name, &r.sql)));
    let replayed = shadows.lines(lane).await;
    let made = match replayed {
        Ok(Ok(made)) => made,
        Ok(Err((m, error))) => {
            shadows.finish().await;
            return Ok((history, vec![Problem::new("history-replay-fails", format!("Replaying {m} in a shadow database, as this database ran it, fails")).note(error)]));
        }
        Err(e) => {
            shadows.finish().await;
            return Err(e);
        }
    };
    if crate::catalog::same_schema(&live, &made) {
        shadows.finish().await;
        return Ok((history, vec![]));
    }
    // The change as SQL, from what the history makes to this database.
    let mut sql = None;
    if let Ok(Ok(())) = shadows.open(lane).await {
        shadows.taint(lane);
        sql = plan_text(shadows.db(lane), db, None).await;
    }
    shadows.finish().await;
    let made = crate::catalog::without_comments(&crate::catalog::without_missing_children(&made, &live));
    let live = crate::catalog::without_comments(&crate::catalog::without_extra_children(&live, &made));
    let (only_history, only_live) = difference(&made, &live);
    let problem = Problem::new("database-drift", CHANGED_OUTSIDE);
    let problem = match sql {
        Some(sql) => problem.sql(&sql),
        None => problem.lines(std::iter::once(String::new()).chain(changed_lines(&made, &live, db.dialect())).chain([String::new()])),
    };
    let problem = problem.note("Files written from its history wouldn't make it either.");
    let schema = opts.schema_exists().then(|| shown(&opts.schema));
    Ok((history, vec![keep_fixes(problem, schema.as_deref(), only_live.is_empty(), only_history.is_empty())]))
}

/// `reylo migrate new <name>`: a migration named with the current UTC time, refused while any
/// migration is unapplied, so that its time comes after theirs.
/// What the person writing a migration states about it: the tables and columns it may drop, with
/// their rows, and the columns it renames.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choices {
    /// Names as written: `users.nickname`, `audit_log`, `billing.invoices`, a schema's name.
    pub drop: Vec<String>,
    /// Each a column's old name, `users.full_name`, and its new one, `name`.
    pub rename: Vec<(String, String)>,
    /// Whether the migration may hold a lock that blocks writes to a table with rows while it reads
    /// them all, a table copied by a change of type among them (`--blocking`).
    pub blocking: bool,
}

#[cfg(feature = "cli")]
pub async fn create(opts: &Options, name: &str, choices: &Choices, log: Log<'_>) -> Result<i32, Error> {
    if let Some(code) = refuse_lock_file(opts, log) {
        return Ok(code);
    }
    static VALID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9_]+$").expect("a valid pattern"));
    if name.is_empty() {
        return Err(Error("name the migration: reylo migrate new <name>".into()));
    }
    if !VALID.is_match(name) {
        // The name it was likely meant to be: lowercase, with a _ for each run of anything else.
        let mut like = String::new();
        for ch in name.chars() {
            if ch.is_ascii_alphanumeric() {
                like.push(ch.to_ascii_lowercase());
            } else if !like.ends_with('_') {
                like.push('_');
            }
        }
        let like = like.trim_matches('_');
        return Err(Error(if like.is_empty() { "a migration's name is a-z, 0-9 and _".to_string() } else { format!("a migration's name is a-z, 0-9 and _, as {like}") }));
    }
    let (files, _) = opts.migrations()?;
    // The migrations replay in the lock's order, then its drafts; a lock that can't be read is
    // resolved first.
    if let Err(problem) = read_lock(opts) {
        return Ok(refuse(&[problem], log));
    }
    // Before any of them reaches a shadow: what reaches past one, a role, is made on its server. The
    // text checked is the one replayed.
    let schema_sql = opts.schema_text();
    let problems = replay_problems(opts, &files, schema_sql.as_deref(), Dialect::of_url(&opts.url)?);
    if !problems.is_empty() {
        return Ok(refuse(&problems, log));
    }
    let files = run_order(opts, &files);
    // The plan starts from every migration file, drafts not yet applied anywhere included, and is
    // named after the newest of them. Its target is schema.sql, or in a project with none, the
    // development database as it stands, written out as SQL (as `db introspect` prints it). Either
    // way, the development database must have run each migration it applied as it now reads: one
    // edited or deleted since is `migrate dev`'s to settle first.
    let (target_sql, target_name, added) = match schema_sql {
        Some(sql) => {
            let problems = ran_as_written(opts, &files).await?;
            if !problems.is_empty() {
                return Ok(refuse(&problems, log));
            }
            (sql, shown(&opts.schema), vec![])
        }
        None => match as_its_migrations_leave_it(opts, &files).await? {
            Ok((sql, added)) => (sql, "this database".to_string(), added),
            Err(problems) => return Ok(refuse(&problems, log)),
        },
    };
    let added = unneeded_by_history(opts, &files, added).await?;
    let drafted = match draft(opts, &files, &target_sql, &target_name, &added, name, choices, log).await? {
        Ok(drafted) => drafted,
        Err(problems) => return Ok(refuse(&problems, log)),
    };
    std::fs::create_dir_all(&opts.dir)?;
    // After the newest migration, should one share this second; on MySQL a plan of several DDL
    // statements is several files, a second apart, `<name>`, `<name>_2`, ….
    let newest = files.iter().filter_map(|f| time::seconds(&f.time)).max();
    let at = time::now_seconds().max(newest.map_or(0, |n| n + 1));
    let mut written = Vec::new();
    for (k, text) in drafted.texts.iter().enumerate() {
        let suffix = if k == 0 { String::new() } else { format!("_{}", k + 1) };
        let file = opts.dir.join(format!("{}_{name}{suffix}.sql", time::stamp(at + k as i64)));
        std::fs::write(&file, text)?;
        written.push(shown(&file));
    }
    match written.as_slice() {
        // With schema.sql, any schema change goes there, so an empty migration is for data; with
        // none, it is as much for a schema change written by hand.
        [one] if drafted.empty && drafted.target.ends_with(".sql") => {
            log(&format!("{} Wrote {one}", paint("green", "✓")));
            log(&note(format!("Empty: {} has nothing new, so it's for a change to data", drafted.target)));
        }
        [one] if drafted.empty => {
            log(&format!("{} Wrote {one}", paint("green", "✓")));
            log(&note(format!("Empty, to write by hand: {} has nothing new", drafted.target)));
        }
        [one] => log(&format!("{} Wrote {one}", paint("green", "✓"))),
        many => {
            // A file past one per commit is the lock-safe shape's.
            let unblocking = drafted.lock_safe && many.len() > drafted.commits + 1;
            let why = match (unblocking, drafted.commits > 0) {
                (true, true) => "so that none blocks writes to a table while it reads every row, and none uses an enum value it adds",
                (true, false) => "so that none blocks writes to a table while it reads every row",
                (false, true) => "since Postgres can't use a new enum value in the migration that adds it",
                (false, false) => "one per statement, since MySQL commits each DDL statement on its own",
            };
            log(&format!("{} Wrote {} migrations, {why}:", paint("green", "✓"), many.len()));
            for path in many {
                log(&format!("    {path}"));
            }
        }
    }
    for line in &drafted.changes {
        log(line);
    }
    // The risks stand apart from the diff above them.
    if !drafted.changes.is_empty() && drafted.notes.first().is_some_and(|n| !n.is_empty()) {
        log("");
    }
    for note in &drafted.notes {
        log(note);
    }
    Ok(drafted.code)
}

/// The development database as SQL that makes it, for `migrate new` in a project with no schema.sql:
/// the migration is what it has that the migrations don't make. Refused unless it ran every migration,
/// each as it now reads, since one it hasn't run would read as something to drop. With it, the
/// extensions of the project's it has that no file creates, for the migration to make where the
/// history doesn't need them ([`unneeded_by_history`]).
#[cfg(feature = "cli")]
async fn as_its_migrations_leave_it(opts: &Options, files: &[Migration]) -> Result<Result<(String, Vec<String>), Vec<Problem>>, Error> {
    let mut db = Db::open(opts).await?;
    let result = async {
        let d = db.dialect();
        let history = if has_history(&mut db).await? { read_history(&mut db).await? } else { vec![] };
        let mut problems = applied_unlike(opts, &history, files, d);
        let pending: Vec<&str> = files.iter().filter(|f| !history.iter().any(|r| r.applied && r.name == f.name)).map(|f| f.name.as_str()).collect();
        if !pending.is_empty() {
            let one = pending.len() == 1;
            problems.push(
                Problem::new("migrations-unapplied", format!("This database hasn't applied {}", pending.join(", ")))
                    .note(format!("With no {}, a migration is what this database has beyond the migrations, so it must have run them all.", shown(&opts.schema)))
                    .fix(format!("Run {} first", if one { "it" } else { "them" }), &["reylo migrate dev"]),
            );
        }
        if !problems.is_empty() {
            return Ok(Err(problems));
        }
        let created = created_extensions(opts);
        let added: Vec<String> = if d == Dialect::Postgres { project_extensions(&mut db).await?.into_iter().filter(|x| !created.contains(x)).collect() } else { vec![] };
        Ok(schema_as_sql_on(&mut db, opts, &[], true, &mut Vec::new()).await?.map(|sql| (passing_over_existing(&sql, d), added)).map_err(|problem| vec![problem]))
    }
    .await;
    db.close().await;
    result
}

/// Of `extensions`, those the migrations replay without, which a new migration makes: one a migration
/// needs is the environment's (a platform's, or one the database had before the project began),
/// which every shadow keeps, so that the history replays as it did on the database. The history
/// needs an extension where its replay without it fails, makes anything it doesn't make with it, or
/// lacks anything it makes with it but what the extension itself brings a shadow (its line, its
/// schema, their grants, read from an empty one), as a migration that does something only where the
/// extension is. Each is tried with
/// those already taken, so that one the history needs takes no other with it.
#[cfg(feature = "cli")]
async fn unneeded_by_history(opts: &Options, files: &[Migration], extensions: Vec<String>) -> Result<Vec<String>, Error> {
    use std::collections::BTreeSet;
    if extensions.is_empty() {
        return Ok(extensions);
    }
    let mut db = Db::open(opts).await?;
    let server = Server::open(&mut db, &opts.url, opts.shadow_url.as_deref()).await;
    db.close().await;
    let mut server = server?;
    server.copy_default_privileges(!sets_default_privileges(opts));
    let texts: Vec<&str> = files.iter().map(|f| f.sql.as_str()).collect();
    // The catalog's lines of a shadow that leaves out `left`, after the migrations or with none run;
    // nothing where they fail.
    let mut lines = async |left: &[String], history: bool| -> Result<Option<BTreeSet<String>>, Error> {
        server.leave_extensions(created_extensions(opts).into_iter().chain(left.iter().cloned()).collect());
        server.leave_schemas(created_schemas(opts));
        let mut shadows = Shadows::new(&mut server, &opts.url, &opts.root, &opts.cache, &texts).await;
        let lane = shadows.lane();
        if history {
            shadows.push(lane, files.iter().map(Step::of));
        }
        let lines = shadows.lines(lane).await;
        shadows.finish().await;
        Ok(lines?.ok().map(|l| l.into_iter().collect()))
    };
    let result = async {
        let Some(empty) = lines(&[], false).await? else { return Ok(vec![]) };
        // A history that replays only without them (one that renames a schema to the one an
        // extension is in) says nothing of which ran beside it: the draft's replay refuses it.
        let Some(replayed) = lines(&[], true).await? else { return Ok(vec![]) };
        let mut unneeded = async |left: Vec<String>| -> Result<Option<Vec<String>>, Error> {
            let (Some(without), Some(empty_without)) = (lines(&left, true).await?, lines(&left, false).await?) else { return Ok(None) };
            // Each line the replay lost is one they bring a shadow, and it gained none: the history
            // may make one of those itself, as their schema.
            let brought: BTreeSet<&String> = empty.difference(&empty_without).collect();
            Ok((without.is_subset(&replayed) && replayed.difference(&without).all(|l| brought.contains(l))).then_some(left))
        };
        if let Some(all) = unneeded(extensions.clone()).await? {
            return Ok(all);
        }
        let mut taken: Vec<String> = Vec::new();
        for extension in &extensions {
            if let Some(more) = unneeded(taken.iter().chain([extension]).cloned().collect()).await? {
                taken = more;
            }
        }
        Ok(taken)
    }
    .await;
    server.close().await;
    result
}

/// `sql` with each `create schema` and `create extension` passing over one that exists, as the dump
/// writes them: as a draft's target, the database's own SQL replays on shadows that already have each
/// extension the project's files don't create, made in its schema.
#[cfg(feature = "cli")]
fn passing_over_existing(sql: &str, d: Dialect) -> String {
    if d != Dialect::Postgres {
        return sql.to_string();
    }
    let statements: Vec<String> = crate::sql::split_statements(sql, d)
        .iter()
        .map(|st| {
            let text = st.text.trim();
            let lower = text.to_ascii_lowercase();
            for create in ["create schema ", "create extension "] {
                if lower.starts_with(create) && !lower[create.len()..].starts_with("if not exists ") {
                    return format!("{create}if not exists {};", &text[create.len()..]);
                }
            }
            format!("{text};")
        })
        .collect();
    format!("{}\n", statements.join("\n\n"))
}

/// For `migrate new` with a schema.sql: what the development database applied that no longer reads as
/// it ran, one query on a database the shadows are made beside. Nothing where it has no history.
#[cfg(feature = "cli")]
async fn ran_as_written(opts: &Options, files: &[Migration]) -> Result<Vec<Problem>, Error> {
    let mut db = Db::open(opts).await?;
    let result = async {
        let history = if has_history(&mut db).await? { read_history(&mut db).await? } else { vec![] };
        Ok(applied_unlike(opts, &history, files, db.dialect()))
    }
    .await;
    db.close().await;
    result
}

/// Each migration a database applied that isn't in the files, or whose statements changed since.
#[cfg(feature = "cli")]
fn applied_unlike(opts: &Options, history: &[HistoryRow], files: &[Migration], d: Dialect) -> Vec<Problem> {
    let mut problems = Vec::new();
    for r in history.iter().filter(|r| r.applied) {
        match files.iter().find(|f| f.name == r.name) {
            None => problems.push(missing_problem(&r.name, &r.sql, &shown(&opts.dir))),
            Some(f) if !same_statements(&f.sql, &r.sql, d) => problems.push(edited_problem(&r.name, &r.sql, &f.sql)),
            Some(_) => {}
        }
    }
    problems
}

/// The refusal of a target that doesn't replay: schema.sql, or the database's schema as SQL.
fn fails_problem(target_name: &str, error: String) -> Problem {
    let title = if target_name.ends_with(".sql") { format!("On an empty database, {target_name} fails") } else { "This database's schema doesn't replay on an empty database".to_string() };
    Problem::new("schema-sql-fails", title).note(error)
}

/// A migration `draft` writes: its files' texts, the exit code, what to say under the files once
/// they are written, whether it is empty (for a change of data), and what it was drafted from.
struct Drafted {
    texts: Vec<String>,
    code: i32,
    notes: Vec<String>,
    empty: bool,
    target: String,
    /// Whether its files are the lock-safe shape's (`locks`), rather than one per MySQL statement.
    lock_safe: bool,
    /// Where its plan commits partway, each enum's labels before what uses them, a file ending at
    /// each: how many times.
    commits: usize,
    /// What it changes, a line each, as `changed_lines` writes it.
    changes: Vec<String>,
}

/// What a plan loses, as a sentence names it: `the column users.bio`, `the table tags`.
fn described(loss: &str) -> String {
    match loss.split_once(' ') {
        Some((kind, name)) => format!("the {kind} {name}"),
        None => loss.to_string(),
    }
}

/// The plan as migration files: one, or on MySQL, where every DDL statement commits on its own,
/// one per statement, each with the comments written above it.
#[cfg(feature = "cli")]
fn plan_files(plan: &crate::diff::Plan, d: Dialect) -> Vec<String> {
    // A file per transaction: one, or more where the plan commits partway.
    if d != Dialect::Mysql {
        return plan.transactions().iter().map(|t| t.sql()).collect();
    }
    let mut files: Vec<String> = Vec::new();
    let mut pending = String::new();
    for loss in &plan.losses {
        pending.push_str(&format!("-- loses data: {}\n", shown_id(loss)));
    }
    for (column, why) in &plan.narrowings {
        pending.push_str(&format!("-- loses data: {} {why}\n", column.trim_start_matches("column ")));
    }
    for s in &plan.statements {
        if s.starts_with("--") {
            pending.push_str(s);
            pending.push('\n');
            continue;
        }
        files.push(format!("{pending}{};\n", s.trim_end()));
        pending.clear();
    }
    if files.is_empty() {
        files.push(pending);
    }
    files
}

/// A dotted name's parts, each unquoted: `public."Users".nick` is public, Users, nick.
fn split_name(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut part = String::new();
    let mut quote: Option<char> = None;
    for ch in name.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => part.push(ch),
            None if ch == '"' || ch == '`' => quote = Some(ch),
            None if ch == '.' => out.push(std::mem::take(&mut part)),
            None => part.push(ch),
        }
    }
    out.push(part);
    out
}

/// The tables and columns among `ids` (`table public.users`, `column users.nick`) that a name
/// written on the command line names: a table by its name (`users`, `billing.invoices`), every
/// table of a schema by the schema's name, a column by its table and its name (`users.nick`,
/// `public.users.nick`). A column is never named by its name alone. Where the name could be more
/// than one thing, `Err` holds each.
fn resolve(ids: &[String], written: &str) -> Result<Vec<String>, Vec<String>> {
    // As written first; then, where that names nothing, ignoring case, as a shell makes quoting a
    // name awkward and MySQL's and SQLite's names ignore it.
    match resolve_by(ids, written, true) {
        Ok(hits) if hits.is_empty() => resolve_by(ids, written, false),
        other => other,
    }
}

fn resolve_by(ids: &[String], written: &str, exact: bool) -> Result<Vec<String>, Vec<String>> {
    let same = |a: &[String], b: &[String]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| if exact { x == y } else { x.eq_ignore_ascii_case(y) });
    let w = split_name(written);
    let (mut exact_hits, mut by_suffix, mut by_schema) = (Vec::new(), Vec::new(), Vec::new());
    for id in ids {
        let (kind, rest) = id.split_once(' ').unwrap_or(("", id.as_str()));
        let p = split_name(rest);
        if same(&p, &w) {
            exact_hits.push(id.clone());
            continue;
        }
        match kind {
            "table" if w.len() == 1 && same(&p[p.len() - 1..], &w) => by_suffix.push(id.clone()),
            "table" if w.len() == 1 && p.len() == 2 && same(&p[..1], &w) => by_schema.push(id.clone()),
            "column" if w.len() >= 2 && p.len() > w.len() && same(&p[p.len() - w.len()..], &w) => by_suffix.push(id.clone()),
            _ => {}
        }
    }
    // A column and a MySQL partition of one table may share a name: one name for both is no answer.
    let kinds: std::collections::BTreeSet<&str> = exact_hits.iter().filter_map(|id: &String| id.split(' ').next()).collect();
    if kinds.len() > 1 {
        return Err(exact_hits);
    }
    match (exact_hits.is_empty(), by_suffix.len(), by_schema.is_empty()) {
        (false, 0, _) => Ok(exact_hits),
        (true, 0, _) => Ok(by_schema),
        (true, 1, true) => Ok(by_suffix),
        _ => Err(exact_hits.into_iter().chain(by_suffix).chain(by_schema).collect()),
    }
}

/// A schema to take a database to: as the diff engine reads it, and on Postgres every table's
/// indexes by name, those no object shows included.
#[cfg(feature = "cli")]
struct Target {
    schema: crate::diff::Schema,
    indexes: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
}

/// The target `db` holds, declared by `declared` where it was built from a file of its own.
#[cfg(feature = "cli")]
async fn read_target(db: &mut Db, declared: Option<&str>) -> Result<Target, Error> {
    let mut schema = crate::diff::read(db).await?;
    let mut indexes = Default::default();
    if db.dialect() == Dialect::Postgres {
        if let Some(sql) = declared {
            crate::diff::postgres::declare(db, &mut schema, sql).await?;
        }
        indexes = crate::diff::postgres::index_names(db).await?;
    }
    Ok(Target { schema, indexes })
}

/// Renames the tables `renamed` names on `db`, and on Postgres what is named after them, and gives
/// the statements that did, as the migration writes them.
#[cfg(feature = "cli")]
async fn rename_tables(db: &mut Db, current: &crate::diff::Schema, target: &Target, renamed: &[(String, String, bool)]) -> Result<Vec<String>, Error> {
    let d = db.dialect();
    let mut statements = table_rename_statements(d, current, &target.schema, renamed);
    let sql: Vec<String> = statements.iter().filter(|s| !s.starts_with("--")).map(|s| format!("{s};")).collect();
    if sql.is_empty() {
        return Ok(statements);
    }
    db.script(&sql.join("\n")).await?;
    if d == Dialect::Postgres {
        let bare = |id: &str| split_name(&shown_id(id)).pop().unwrap_or_default();
        let names: Vec<(String, String, String)> = renamed.iter().map(|(old, new, _)| (bare(old), bare(new), new.clone())).collect();
        let more = crate::diff::postgres::inherited_renames(db, &target.indexes, &names).await?;
        if !more.is_empty() {
            db.script(&more.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n")).await?;
            statements.extend(more);
        }
        crate::diff::postgres::style_statements(db, &mut statements, &target.schema.verbatim).await?;
    }
    Ok(statements)
}

/// The plan from the database to `target`: a table that goes while one of its columns comes renamed
/// first, on the database itself, then the diff engine's plan from there, which is not run.
#[cfg(feature = "cli")]
async fn plan_to(db: &mut Db, target: &Target, live: bool) -> Result<(Vec<String>, crate::diff::Plan), Error> {
    match plan_asked(db, target, live, None).await? {
        Ok(Settled { renames, plan, .. }) => Ok((renames, plan)),
        Err(_) => unreachable!("nothing is refused where nothing is asked"),
    }
}

/// What `reylo db push` is told about what its plan drops and renames: as `migrate new` is, and whether
/// `--force` accepts every destructive change at once.
#[derive(Clone, Debug, Default)]
pub struct Push {
    pub choices: Choices,
    pub force: bool,
}

/// `plan_to`, settled as `asking` (what was stated, the command, and `--force`) says.
#[cfg(feature = "cli")]
async fn plan_asked(db: &mut Db, target: &Target, live: bool, asking: Option<(&Push, &Asked)>) -> Result<Result<Settled, Vec<Problem>>, Error> {
    let mut current = crate::diff::read(db).await?;
    current.live = live;
    let none = Choices::default();
    let choices = asking.map_or(&none, |(push, _)| &push.choices);
    settle(db, current, target, live, choices, asking.map(|(_, asked)| asked), asking.is_some_and(|(push, _)| push.force)).await
}

/// The statements that rename the tables `renamed` names (each its old id in `current`, its new id
/// in `target`, and whether --rename stated it), and on Postgres what is named after them; a guessed
/// rename is written down above its statement with what to do instead.
#[cfg(feature = "cli")]
fn table_rename_statements(d: Dialect, current: &crate::diff::Schema, target: &crate::diff::Schema, renamed: &[(String, String, bool)]) -> Vec<String> {
    // A table's name as its statements write it, and whether it is a foreign table.
    let written = |schema: &crate::diff::Schema, id: &str| -> (String, bool) {
        let drop = schema.objects[id].drop.first().map(String::as_str).unwrap_or_default();
        match drop.strip_prefix("drop foreign table ") {
            Some(name) => (name.to_string(), true),
            None => (drop.strip_prefix("drop table ").map_or_else(|| shown_id(id), str::to_string), false),
        }
    };
    let bare = |id: &str| split_name(&shown_id(id)).pop().unwrap_or_default();
    let mut out = Vec::new();
    let mut names = Vec::new();
    for (old_id, new_id, was_stated) in renamed {
        let ((from_w, foreign), (to_w, _)) = (written(current, old_id), written(target, new_id));
        let new_name = if d == Dialect::Postgres { crate::diff::last_written(&to_w) } else { to_w };
        let (old, new) = (bare(old_id), bare(new_id));
        if !was_stated {
            out.push(format!("-- {old} is renamed {new}, keeping its rows. If they are two tables, drop the one and create the other instead."));
        }
        // A view the target replaces with the table takes the name first: it goes before the rename,
        // with what is made on it, which the plan makes again where the target has it.
        if let Some(view) = new_id.strip_prefix("table ").and_then(|n| current.objects.get(&format!("view {n}"))) {
            out.extend(view.drop.iter().map(|drop| if d == Dialect::Postgres { format!("{drop} cascade") } else { drop.clone() }));
        }
        out.push(format!("alter {}table {from_w} rename to {new_name}", if foreign { "foreign " } else { "" }));
        names.push((old, new, old_id.clone(), new_id.clone()));
    }
    if d == Dialect::Postgres {
        out.extend(crate::diff::postgres::derived_renames(current, target, &names));
    }
    out
}

/// An object's id as a command names it: no kind, and no `public` schema.
fn shown_name(id: &str) -> String {
    let name = shown_id(id);
    name.strip_prefix("public.").map(str::to_string).unwrap_or(name)
}

fn shown_id(id: &str) -> String {
    id.split_once(' ').map_or(id, |(_, rest)| rest).to_string()
}

/// A name as the shell is to be given it: quoted where it isn't one word, or holds a quote. The
/// placeholder for a name to write is left to be written over.
fn shell_word(name: &str) -> String {
    if name == "<new name>" || name.chars().all(|c| c.is_ascii_alphanumeric() || "_.$-/".contains(c)) {
        return name.to_string();
    }
    if cfg!(windows) {
        return format!("\"{}\"", name.replace('"', "\\\""));
    }
    format!("'{}'", name.replace('\'', "'\\''"))
}

/// The command a refusal of what a change drops speaks of: what it calls the change (`migration`,
/// `push`), the command line its fixes repeat with their flags, and whether `--force` can accept
/// every destructive change at once.
struct Asked {
    subject: &'static str,
    head: String,
    offers_force: bool,
    /// What suggested commands state, without `--blocking`, where the migration each writes holds a
    /// lock that `--blocking` accepts, with what holds it: each such command takes the flag, and
    /// its refusal says what the flag accepts.
    rewrites: Vec<(Choices, Vec<String>)>,
}

impl Asked {
    fn migration(name: &str, rewrites: &[(Choices, Vec<String>)]) -> Asked {
        Asked { subject: "migration", head: format!("reylo migrate new {name}"), offers_force: false, rewrites: rewrites.to_vec() }
    }

    fn push(watch: bool) -> Asked {
        Asked { subject: "push", head: if watch { "reylo db push --watch".to_string() } else { "reylo db push".to_string() }, offers_force: true, rewrites: vec![] }
    }

    /// `reading`, with `--blocking` where the migration it writes needs it.
    fn blocking(&self, reading: &Choices) -> Choices {
        Choices { blocking: reading.blocking || !self.hazards(reading).is_empty(), ..reading.clone() }
    }

    /// What the migration `reading` writes does that `--blocking` accepts.
    fn hazards(&self, reading: &Choices) -> &[String] {
        self.rewrites.iter().find(|(r, _)| r == reading).map_or(&[], |(_, what)| what.as_slice())
    }
}

/// What `choices` states, with `more_drops` and `renames` added: what a suggested command states.
fn reading(choices: &Choices, more_drops: &[String], renames: &[(String, String)]) -> Choices {
    let mut drop: Vec<String> = Vec::new();
    for name in choices.drop.iter().chain(more_drops) {
        if !drop.contains(name) {
            drop.push(name.clone());
        }
    }
    Choices { drop, rename: choices.rename.iter().chain(renames).cloned().collect(), blocking: choices.blocking }
}

/// A table a plan drops to create it again under its name, since the server can't change it in
/// place, with the partitions of it that go too.
struct RemadeTable {
    id: String,
    why: Remade,
    parts: Vec<String>,
    /// The statements that keep its rows, added to the migration that creates it again.
    keep: String,
}

/// What a table is made again for.
enum Remade {
    /// A plain table made partitioned.
    Partitioned,
    /// A partitioned table made plain.
    Plain,
    /// A partitioned table given another partition key.
    Rekeyed,
    Other,
}

/// The tables in `lost` that `to` has too, which the plan drops to create again, each with why.
#[cfg(feature = "cli")]
fn remade_tables(lost: &[String], from: &crate::diff::Schema, to: &crate::diff::Schema, renames: &[(String, String)]) -> Vec<RemadeTable> {
    let key = |o: &crate::diff::Object| o.extra.iter().find_map(|l| l.strip_prefix("shape ")).and_then(|s| s.split_once(" partition by ")).map(|(_, k)| k.to_string());
    let parent = |o: &crate::diff::Object| o.extra.iter().find_map(|l| l.strip_prefix("partition of ")).and_then(|p| p.split_once(' ')).map(|(p, _)| format!("table {p}"));
    let mut out = Vec::new();
    for id in lost.iter().filter(|id| id.starts_with("table ")) {
        let (Some(was), Some(is)) = (from.objects.get(id), to.objects.get(id)) else { continue };
        let why = match (key(was), key(is)) {
            (None, Some(_)) => Remade::Partitioned,
            (Some(_), None) => Remade::Plain,
            (Some(a), Some(b)) if a != b => Remade::Rekeyed,
            _ => Remade::Other,
        };
        // Its partitions, and theirs, that the plan drops along with it; one the schema keeps as a
        // table of its own, no longer its partition, keeps its rows apart.
        let (mut parts, mut apart): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
        let mut parents = vec![id.clone()];
        while let Some(p) = parents.pop() {
            for (part, _) in from.objects.iter().filter(|(part, o)| lost.contains(*part) && parent(o).as_deref() == Some(p.as_str())) {
                if to.objects.get(part).is_some_and(|o| parent(o).as_deref() != Some(p.as_str())) {
                    apart.push(part.clone());
                } else if !parts.contains(part) {
                    parts.push(part.clone());
                    parents.push(part.clone());
                }
            }
        }
        let keep = keep_rows(id, from, to, renames, &apart);
        out.push(RemadeTable { id: id.clone(), why, parts, keep });
    }
    // A partition made again with its table goes with it: the table's copy holds its rows.
    let parts: Vec<String> = out.iter().flat_map(|r| r.parts.clone()).collect();
    out.retain(|r| !parts.contains(&r.id));
    out
}

/// The statements that keep a table's rows through a migration that creates it again. At its top,
/// the table locked against writes, which its drop would lose, and its rows copied (without those of
/// `apart`, partitions that become tables of their own, and of their partitions). Right after the
/// statements that create it again, in the same file and before what a trigger made after them
/// would do to the rows, the rows put back: in each column the new table has from the one it was,
/// `renames` (column ids, old and new) included, cast where its type changes, past an identity that
/// only takes its own values; and each sequence of the new table moved past them where that lies
/// within its bounds and past its start.
#[cfg(feature = "cli")]
fn keep_rows(id: &str, from: &crate::diff::Schema, to: &crate::diff::Schema, renames: &[(String, String)], apart: &[String]) -> String {
    let full = id.trim_start_matches("table ");
    let shown = |full: &str| full.strip_prefix("public.").unwrap_or(full).to_string();
    let table = shown(full);
    // The copy's name, the table's with `_rows` after it, inside its quotes where it has them.
    let copy = match table.strip_suffix('"') {
        Some(quoted) => format!("{quoted}_rows\""),
        None => format!("{table}_rows"),
    };
    let prefix = format!("column {full}.");
    let columns = |s: &crate::diff::Schema| -> Vec<(String, String)> { s.objects.iter().filter(|(c, o)| o.kind == Some(crate::diff::Kind::Column) && c.starts_with(&prefix)).map(|(c, o)| (c[prefix.len()..].to_string(), o.def.clone())).collect() };
    let (was, is) = (columns(from), columns(to));
    // A column's type: its definition up to what follows the type.
    let ty = |def: &str| [" collate ", " default ", " generated ", " not null", " constraint "].iter().filter_map(|m| def.find(m)).min().map_or(def.to_string(), |at| def[..at].to_string());
    // Each column put back, with what it is read from; a generated one takes no value of its own.
    let mut kept: Vec<(&str, &str, String, &str)> = Vec::new();
    for (c, def) in is.iter().filter(|(_, def)| !def.contains(" generated always as (")) {
        let old = renames.iter().find(|(_, new)| new == &format!("{prefix}{c}")).and_then(|(old, _)| old.strip_prefix(&prefix)).unwrap_or(c);
        let Some((_, was_def)) = was.iter().find(|(w, _)| w == old) else { continue };
        let read = if ty(was_def) == ty(def) { old.to_string() } else { format!("{old}::{}", ty(def)) };
        kept.push((c, old, read, def));
    }
    let names = kept.iter().map(|(c, _, _, _)| *c).collect::<Vec<_>>().join(", ");
    let reads = kept.iter().map(|(_, _, r, _)| r.as_str()).collect::<Vec<_>>().join(", ");
    let overriding = if kept.iter().any(|(_, _, _, def)| def.contains(" generated always as identity")) { " overriding system value" } else { "" };
    let literal = |t: &str| format!("'{}'", t.replace('\'', "''"));
    let without = if apart.is_empty() { String::new() } else { format!(" where tableoid not in ({})", apart.iter().map(|a| format!("select relid from pg_partition_tree({}::regclass)", literal(&shown(a.trim_start_matches("table "))))).collect::<Vec<_>>().join(" union all ")) };
    let partitioned = to.objects.get(id).is_some_and(|o| o.extra.iter().any(|l| l.starts_with("shape ") && l.contains(" partition by ")));
    let after = if partitioned { format!("-- right after it creates {table} and its partitions") } else { format!("-- right after it creates {table}") };
    let mut out = vec![
        "-- at the top".to_string(),
        format!("lock table {table} in exclusive mode;"),
        format!("create table {copy} as select * from {table}{without};"),
        after,
        format!("insert into {table} ({names}){overriding} select {reads} from {copy};"),
    ];
    for (c, old, _, def) in &kept {
        // An identity's sequence, found through its column, and a sequence the column owned, which
        // goes with the table and is made again, named, since the migration makes it the column's
        // only after the rows are back. Either moves past the rows where the column holds its values
        // as they are; one the column only calls, which stays, keeps its place.
        let owned = |schema: &crate::diff::Schema, column: &str| schema.objects.iter().find(|(_, o)| o.kind == Some(crate::diff::Kind::Sequence) && o.goes_with.as_deref() == Some(format!("{prefix}{column}").as_str())).map(|(id, o)| (id.clone(), o.def.clone()));
        let (sequence, options) = if def.contains(" as identity") {
            let unquoted = c.strip_prefix('"').and_then(|c| c.strip_suffix('"')).map_or(c.to_string(), |c| c.replace("\"\"", "\""));
            (format!("pg_get_serial_sequence({}, {})", literal(&table), literal(&unquoted)), def.to_string())
        } else if let Some((id, options)) = owned(to, c).filter(|(id, _)| !from.objects.contains_key(id) || owned(from, old).is_some_and(|(was, _)| &was == id)) {
            // Made again with the table, or new: one that outlives it, which the column only comes
            // to own, keeps its place.
            let name = shown(id.trim_start_matches("sequence "));
            let integer = ["smallint", "integer", "bigint"].contains(&ty(def).as_str());
            let full = id.trim_start_matches("sequence ");
            let alone = [&name, full].iter().any(|n| def.contains(&format!("default nextval({}::regclass)", literal(n))));
            // One that counts wider than its column may find no value both hold that the rows leave
            // free, which only the rows can say.
            let width = |t: &str| ["smallint", "integer", "bigint"].iter().position(|w| *w == t);
            let counts = options.split_whitespace().skip_while(|w| *w != "as").nth(1).unwrap_or("bigint");
            if !integer || !alone || width(counts) > width(&ty(def)) {
                out.push(format!("-- and move the sequence {name} past the values put back in {c}"));
                continue;
            }
            (literal(&name), options)
        } else {
            continue;
        };
        let (edge, past, beyond) = if options.contains("increment by -") { ("min", "- 1", "<") } else { ("max", "+ 1", ">") };
        let next = format!("{edge}({c})::numeric {past}");
        out.push(format!("select setval({sequence}, coalesce((select {next} from {table} having {next} between seqmin and seqmax and {next} {beyond} seqstart), seqstart)::bigint, false) from pg_sequence where seqrelid = {sequence}::regclass;"));
    }
    out.push(format!("drop table {copy};"));
    out.join("\n")
}

/// The refusals `--force` accepts: what a change drops, a type change that loses values, and a rename
/// that may be a drop. A name `--drop` or `--rename` gives that names nothing still stands.
const DESTRUCTIVE: [&str; 3] = ["drops-data", "loses-data", "rename-guess"];

/// The command that makes the change with what the person stated and what is to be added.
fn create_command(asked: &Asked, choices: &Choices, more_drops: &[String], renames: &[(String, String)]) -> String {
    command_line(asked, choices, more_drops, renames, false)
}

fn command_line(asked: &Asked, choices: &Choices, more_drops: &[String], renames: &[(String, String)], force: bool) -> String {
    let mut drops: Vec<&String> = Vec::new();
    for name in choices.drop.iter().chain(more_drops) {
        if !drops.contains(&name) {
            drops.push(name);
        }
    }
    let mut flags: Vec<String> = Vec::new();
    if !drops.is_empty() {
        flags.push(format!("--drop {}", drops.iter().map(|n| shell_word(n)).collect::<Vec<_>>().join(",")));
    }
    flags.extend(choices.rename.iter().chain(renames).map(|(old, new)| format!("--rename {}={}", shell_word(old), shell_word(new))));
    if choices.blocking {
        flags.push("--blocking".to_string());
    }
    if force {
        flags.push("--force".to_string());
    }
    // Several flags read one to a line, where the shell continues a line with a backslash.
    let (head, gap) = (asked.head.clone(), if cfg!(windows) || flags.len() < 2 { " " } else { " \\\n      " });
    std::iter::once(head).chain(flags).collect::<Vec<_>>().join(gap)
}

/// A rename `create` won't assume, since it changes more than a name: only the person can say whether
/// it is one thing renamed or two things.
struct Guess {
    /// The old name, as a command writes it: `users.email`, `orders`.
    old: String,
    /// The new name, as `--rename` takes it: `contact`, `invoices`.
    new: String,
    /// The schema's difference, as the lines it prints: the column's old line and its new; a table's,
    /// then those of its columns that differ.
    diff: Vec<String>,
    /// The id of its table where that table is itself one of the guesses, so that dropping that
    /// table says it all.
    owner: Option<String>,
    /// Its id, a table's new one: what a column's `owner` names, however the names are shown.
    id: String,
    /// Whether it is a table, rather than a column.
    table: bool,
}

/// A column's lines in a guess's diff: the old name and definition `a`, and the new. `old` is the name
/// as a command writes it, `new` the new name as `--rename` takes it. Either may be a rename or a drop
/// and an add, so neither is shown as the other.
fn column_diff(old: &str, new: &str, a: &str, b: &str) -> Vec<String> {
    vec![gone_line(&format!("column {old} {a}")), came_line(&format!("column {} {b}", renamed_to(old, new)))]
}

/// A line of a refusal's diff that only the old schema has, and one that only the new has.
fn gone_line(line: &str) -> String {
    paint("red", &format!("    - {line}"))
}

fn came_line(line: &str) -> String {
    paint("green", &format!("    + {line}"))
}

/// A line of `migrate new`'s summary changed in place, as a rename changes it: `~`, then the line with
/// the words it changes highlighted.
fn changed_line(a: &str, b: &str) -> String {
    format!("  {} {}", paint("yellow", "~"), inline_diff(a, b))
}

/// The name `old` takes when renamed `new`, as `--rename` takes it: `posts.title` renamed `heading`
/// is `posts.heading`. A new name that names its schema or table is that already.
fn renamed_to(old: &str, new: &str) -> String {
    if crate::sql::last_written(new) != new {
        return new.to_string();
    }
    format!("{}{new}", &old[..old.len() - crate::sql::last_written(old).len()])
}

/// A line changed in place, as git's word diff shows one: the words only `a` has, then those only `b`
/// has, highlighted red and green among the words both share. Colour off, they are marked as git
/// marks them: `posts.[-title-]{+heading+}`. A line's kind and name are compared apart from its
/// definition, so that the old name lines up with the new and the old type with the new.
fn inline_diff(a: &str, b: &str) -> String {
    match (split_head(a), split_head(b)) {
        ((x, Some(p)), (y, Some(q))) => format!("{} {}", word_diff(x, y), word_diff(p, q)),
        ((x, None), (y, None)) => word_diff(x, y),
        _ => word_diff(a, b),
    }
}

/// A comparison line's kind and name (`column posts.title`), and the definition after them, if any:
/// split at the first space outside quotes after the name.
fn split_head(line: &str) -> (&str, Option<&str>) {
    let Some(start) = line.find(' ') else { return (line, None) };
    let mut quote: Option<char> = None;
    for (k, ch) in line[start + 1..].char_indices() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '`' => quote = Some(ch),
            None if ch == ' ' => {
                let at = start + 1 + k;
                return (&line[..at], Some(&line[at + 1..]));
            }
            None => {}
        }
    }
    (line, None)
}

/// The words of `a` and `b` as `inline_diff` shows them.
fn word_diff(a: &str, b: &str) -> String {
    fn tokens(s: &str) -> Vec<&str> {
        let (mut out, mut start, mut word) = (Vec::new(), 0, false);
        for (k, ch) in s.char_indices() {
            let w = ch.is_alphanumeric() || ch == '_';
            if k > start && !(w && word) {
                out.push(&s[start..k]);
                start = k;
            }
            word = w;
        }
        if start < s.len() {
            out.push(&s[start..]);
        }
        out
    }
    fn mark(removed: bool, text: &str) -> String {
        match (color(), removed) {
            (true, _) => paint(if removed { "removed" } else { "added" }, text),
            (false, true) => format!("[-{text}-]"),
            (false, false) => format!("{{+{text}+}}"),
        }
    }
    if a == b {
        return a.to_string();
    }
    let (a, b) = (tokens(a), tokens(b));
    // What both start and end with is shared; only what lies between is compared word by word, and
    // where that is long, as a long default can be, it is shown as replaced whole.
    let head = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let tail = a[head..].iter().rev().zip(b[head..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (start, end) = (a[..head].concat(), a[a.len() - tail..].concat());
    let (a, b) = (&a[head..a.len() - tail], &b[head..b.len() - tail]);
    const MOST: usize = 10_000;
    if a.len() * b.len() > MOST {
        let (gone, came) = (a.concat(), b.concat());
        let marked = |removed: bool, text: &str| if text.is_empty() { String::new() } else { mark(removed, text) };
        return format!("{start}{}{}{end}", marked(true, &gone), marked(false, &came));
    }
    let (n, m) = (a.len(), b.len());
    // The longest run of words both share, from each pair of positions to the end.
    let mut common = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            common[i][j] = if a[i] == b[j] { common[i + 1][j + 1] + 1 } else { common[i + 1][j].max(common[i][j + 1]) };
        }
    }
    let (mut out, mut gone, mut came) = (String::new(), String::new(), String::new());
    let flush = |out: &mut String, gone: &mut String, came: &mut String| {
        if !gone.is_empty() {
            out.push_str(&mark(true, gone));
        }
        if !came.is_empty() {
            out.push_str(&mark(false, came));
        }
        gone.clear();
        came.clear();
    };
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            flush(&mut out, &mut gone, &mut came);
            out.push_str(a[i]);
            (i, j) = (i + 1, j + 1);
        } else if j < m && (i == n || common[i][j + 1] >= common[i + 1][j]) {
            // On a tie, what `b` adds goes first, so that a shared word matches its earliest place.
            came.push_str(b[j]);
            j += 1;
        } else {
            gone.push_str(a[i]);
            i += 1;
        }
    }
    flush(&mut out, &mut gone, &mut came);
    format!("{start}{out}{end}")
}

/// The refusal for guesses, with what else the migration asks of `--drop`. One guess gets the
/// command for each reading. Several get a line each, its two flags side by side, and the command
/// to add the picked flags to, since any mix of the two readings may be the right one.
fn guess_problem(guesses: &[Guess], drops: &[(String, String, Option<String>)], narrowed: &[(String, String, Option<String>)], outside: &[String], stated: &Choices, asked: &Asked) -> Problem {
    let subject = asked.subject;
    let one = guesses.len() == 1;
    let kinds = match (guesses.iter().any(|g| g.table), guesses.iter().any(|g| !g.table)) {
        (true, true) => "tables and columns",
        (true, false) => "tables",
        _ => "columns",
    };
    let kind = if guesses[0].table { "table" } else { "column" };
    let title = if one { format!("This {subject} can't tell a rename from a new {kind}") } else { format!("This {subject} can't tell renames from new {kinds}") };
    // The schema's difference, each guess's lines together; a guess within a guessed table is among
    // its table's.
    let diff = guesses.iter().filter(|g| g.owner.is_none()).flat_map(|g| g.diff.iter().cloned());
    let mut problem = Problem::new("rename-guess", title).lines([String::new()]).lines(diff);
    // What a rename's change of type would lose, which --drop names by the old name.
    let guessed = |name: &str| guesses.iter().any(|g| g.old == name);
    // What it drops whichever way each guess goes: not what a renamed table drops, which its
    // change says.
    let others: Vec<(String, String)> = drops.iter().filter(|(_, n, _)| !guessed(n) && outside.contains(n)).map(|(k, n, _)| (k.clone(), n.clone())).collect();
    if !others.is_empty() {
        problem = problem.lines([String::new()]).note(if others.len() == 1 { format!("It also drops {} and its data.", drop_phrase(&others)) } else { format!("It also drops {}, with their data.", drop_phrase(&others)) });
    } else if !narrowed.is_empty() {
        problem = problem.lines([String::new()]);
    }
    for (name, why, _) in narrowed {
        problem = problem.note(if guessed(name) { format!("As a rename, {name} {why}.") } else { format!("{name} {why}.") });
    }
    // Each name --drop may need, with the id of the guessed table it belongs to, if any.
    let owned: Vec<(String, Option<String>)> = drops.iter().map(|(_, n, o)| (n.clone(), o.clone())).chain(narrowed.iter().map(|(n, _, o)| (n.clone(), o.clone()))).collect();
    let more: Vec<String> = owned.iter().map(|(n, _)| n.clone()).collect();
    // What each reading holds that its command's --blocking accepts, said before the command.
    let mut noted = !others.is_empty() || !narrowed.is_empty();
    let mut hazard = |problem: Problem, lead: &str, r: &Choices| -> Problem {
        let what = asked.hazards(r);
        let mut problem = if !what.is_empty() && !noted { problem.lines([String::new()]) } else { problem };
        noted |= !what.is_empty();
        for w in what {
            problem = problem.note(format!("{lead}, it {w}."));
        }
        problem
    };
    if one {
        let renames: Vec<(String, String)> = guesses.iter().map(|g| (g.old.clone(), g.new.clone())).collect();
        let dropped: Vec<String> = guesses.iter().map(|g| g.old.clone()).chain(outside.iter().cloned()).collect();
        let (keep, drop) = (reading(stated, &more, &renames), reading(stated, &dropped, &[]));
        let problem = hazard(problem, "As a rename", &keep);
        let problem = hazard(problem, "As a drop", &drop);
        return problem
            .offer(format!("To rename the {kind}, keeping its rows"), asked, keep)
            .offer(format!("To drop the {kind} and its data, and {} a new one", if guesses[0].table { "create" } else { "add" }), asked, drop);
    }
    // Each line's flags: a rename takes along what its own reading also asks of --drop, the columns
    // a renamed table drops, and what the rename's change of type loses; a guess within a guessed
    // table goes with its table's line, which a drop of the table settles. Each is matched to its
    // table by the table's id, since a column's name may be shown without the schema its table's
    // carries, as under a search_path.
    let mut taken: Vec<String> = Vec::new();
    let mut rows: Vec<(String, String)> = Vec::new();
    // What each line's rename states, its renames and its drops, and what its drop does.
    let mut keeps: Vec<(Vec<(String, String)>, Vec<String>)> = Vec::new();
    for g in guesses.iter().filter(|g| g.owner.is_none()) {
        let within: Vec<&Guess> = guesses.iter().filter(|c| g.table && c.owner.as_deref() == Some(g.id.as_str())).collect();
        let mine: Vec<String> = owned.iter().filter(|(n, o)| *n == g.old || (g.table && o.as_deref() == Some(g.id.as_str())) || within.iter().any(|c| *n == c.old)).map(|(n, _)| n.clone()).collect();
        let renames: Vec<(String, String)> = std::iter::once(g).chain(within.iter().copied()).map(|c| (c.old.clone(), c.new.clone())).collect();
        let mut keep: Vec<String> = renames.iter().map(|(old, new)| format!("--rename {}={}", shell_word(old), shell_word(new))).collect();
        let mut lost: Vec<String> = Vec::new();
        for n in &mine {
            if !lost.contains(n) {
                lost.push(n.clone());
            }
        }
        if !lost.is_empty() {
            keep.push(format!("--drop {}", lost.iter().map(|n| shell_word(n)).collect::<Vec<_>>().join(",")));
        }
        taken.extend(mine);
        rows.push((keep.join(" "), format!("--drop {}", shell_word(&g.old))));
        keeps.push((renames, lost));
    }
    let rest: Vec<String> = more.iter().filter(|n| !taken.contains(n)).cloned().collect();
    // A line's rename takes `--blocking` where its migration needs it, the other lines dropped.
    let olds: Vec<String> = guesses.iter().filter(|g| g.owner.is_none()).map(|g| g.old.clone()).collect();
    let mut readings = Vec::new();
    for (k, (renames, lost)) in keeps.iter().enumerate() {
        let drops: Vec<String> = rest.iter().chain(lost).cloned().chain(olds.iter().enumerate().filter(|(j, _)| *j != k).map(|(_, o)| o.clone())).collect();
        let r = reading(stated, &drops, renames);
        if asked.blocking(&r).blocking && !stated.blocking {
            rows[k].0.push_str(" --blocking");
        }
        let (old, new) = &renames[0];
        problem = hazard(problem, &format!("With --rename {}={}", shell_word(old), shell_word(new)), &r);
        readings.push(r);
    }
    let width = rows.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
    let lead = "For each, rename it to keep its rows, or drop it and add the new one";
    problem.lines.extend([String::new(), paint("dim", &format!("  {lead}:"))]);
    for (keep, drop) in &rows {
        problem.lines.push(format!("    {}{}   or   {}", paint("cyan", keep), " ".repeat(width - keep.chars().count()), paint("cyan", drop)));
        problem.detail.push(format!("{keep} or {drop}"));
    }
    problem.fixed = true;
    problem.readings.extend(readings);
    problem.fix("Add the flags you picked to", &[&create_command(asked, stated, &rest, &[])])
}

/// The refusal of a migration that holds a lock blocking writes to a table with rows while it
/// reads them all, which `--blocking` accepts.
#[cfg(feature = "cli")]
fn lock_problem(hazards: &[crate::locks::Hazard], asked: &Asked, choices: &Choices) -> Problem {
    let title = match hazards {
        [one] => format!("This migration {}", one.what),
        _ => "This migration blocks writes to tables while it reads every row".to_string(),
    };
    let mut problem = Problem::new("blocks-writes", title);
    problem.hazards = hazards.iter().map(|h| h.what.clone()).collect();
    if hazards.len() > 1 {
        problem = problem.lines([String::new()]).lines(hazards.iter().map(|h| format!("    It {}", h.what))).lines([String::new()]);
    }
    problem = problem.note("On a table with many rows, the app's writes wait for as long as that takes.");
    if hazards.iter().any(|h| h.what.starts_with("rewrites ")) {
        problem = problem.note("To change a column without copying its table, add a column, fill it in batches, then swap the two, each step a migration of its own.");
    }
    let blocking = Choices { blocking: true, ..choices.clone() };
    problem.fix("Where the table is small enough to wait for", &[&create_command(asked, &blocking, &[], &[])])
}

/// The problems with what `choices` states about the tables and columns a plan drops (`lost`, as
/// the plan's ids, and `shown`, as the file names them, one for one): each must be named in
/// `--drop`, and each name in `--drop` must name only what it drops. `guesses`, renames it won't
/// assume, are settled with them; `owner` gives the new id of the guessed table an id belongs to,
/// whose own columns a drop of the table takes along; `candidates` gives, for a table or column it
/// drops, the names a person could mean by its rename; `remade` are the tables it drops to create again.
#[allow(clippy::too_many_arguments)]
fn drop_problems(lost: &[String], shown: &[String], narrowed: &[(String, String, String)], guesses: &[Guess], owner: &dyn Fn(&str) -> Option<String>, candidates: &dyn Fn(&str) -> Vec<String>, remade: &[RemadeTable], choices: &Choices, asked: &Asked) -> Vec<Problem> {
    let mut problems = drop_problems_of(lost, shown, narrowed, guesses, owner, candidates, remade, choices, asked);
    // Where --force can accept them all, the last says so, once nothing but what it accepts is left.
    if asked.offers_force && !problems.is_empty() && problems.iter().all(|p| DESTRUCTIVE.contains(&p.code)) {
        let stated = Choices { drop: vec![], rename: choices.rename.clone(), ..Default::default() };
        let last = problems.pop().expect("a problem");
        problems.push(last.fix("To accept every destructive change", &[&command_line(asked, &stated, &[], &[], true)]));
    }
    problems
}

#[allow(clippy::too_many_arguments)]
fn drop_problems_of(lost: &[String], shown: &[String], narrowed: &[(String, String, String)], guesses: &[Guess], owner: &dyn Fn(&str) -> Option<String>, candidates: &dyn Fn(&str) -> Vec<String>, remade: &[RemadeTable], choices: &Choices, asked: &Asked) -> Vec<Problem> {
    let subject = asked.subject;
    let display = |id: &String| lost.iter().position(|l| l == id).map(|k| shown_id(&shown[k])).or_else(|| narrowed.iter().find(|(n, _, _)| n == id).map(|(_, s, _)| shown_id(s))).unwrap_or_else(|| shown_id(id));
    let mut problems = Vec::new();
    let mut named: Vec<String> = Vec::new();
    // What --drop states that holds, which a suggested command keeps. It names what is dropped, and
    // a column whose change of type loses what its values hold.
    let mut stated = Choices { drop: vec![], rename: choices.rename.clone(), ..Default::default() };
    let all: Vec<String> = lost.iter().cloned().chain(narrowed.iter().map(|(id, _, _)| id.clone())).collect();
    for w in &choices.drop {
        match resolve(&all, w) {
            Ok(hits) if hits.is_empty() => {
                let columns: Vec<String> = lost.iter().filter(|l| l.starts_with("column ") && split_name(&shown_id(l)).last() == split_name(w).last() && split_name(w).len() == 1).map(&display).collect();
                problems.push(Problem::new("drop-name-unknown", format!("The --drop flag names {w}, which this {subject} doesn't drop")).note(if !columns.is_empty() {
                    format!("A column is named with its table: {}.", columns.join(", "))
                } else if lost.is_empty() {
                    "It drops no table or column.".to_string()
                } else {
                    format!("It drops {}.", lost.iter().map(&display).collect::<Vec<_>>().join(", "))
                }))
            }
            Ok(hits) => {
                named.extend(hits);
                stated.drop.push(w.clone());
            }
            Err(could) if could.iter().filter_map(|c| c.split(' ').next()).collect::<std::collections::BTreeSet<_>>().len() > 1 => {
                problems.push(Problem::new("drop-name-ambiguous", format!("The --drop flag names {w}, which is both {}", and_list(&could.iter().map(|c| described(c)).collect::<Vec<_>>()))).note("Make one of the changes in schema.sql and write its migration, then the other."))
            }
            Err(could) => problems.push(Problem::new("drop-name-ambiguous", format!("The --drop flag names {w}, which could be any of {}", could.iter().map(&display).collect::<Vec<_>>().join(", "))).note("Name each with its table, and its schema where two share a name.")),
        }
    }
    let mut unstated: Vec<&String> = lost.iter().filter(|l| !named.contains(l)).collect();
    // A table the plan drops to create it again under its name, with the partitions that go with
    // it, is refused for what the server can't change in place, and no rename is offered for it.
    let mut remakes: Vec<Problem> = Vec::new();
    for r in remade {
        if !unstated.contains(&&r.id) {
            continue;
        }
        let gone: Vec<String> = r.parts.iter().filter(|p| unstated.contains(p)).chain(std::iter::once(&r.id)).cloned().collect();
        unstated.retain(|l| !gone.contains(l));
        let table = display(&r.id);
        let (title, why) = match &r.why {
            Remade::Partitioned => (format!("Postgres can't make {table} a partitioned table in place"), Some(format!("A table is partitioned only when it's created, so this {subject} drops {table} and its rows to create it again."))),
            Remade::Plain => (format!("Postgres can't make {table} a plain table in place"), Some(format!("A partitioned table is made plain only by creating it again, so this {subject} drops {table}, its partitions and their rows."))),
            Remade::Rekeyed => (format!("Postgres can't change the partition key of {table} in place"), Some(format!("A partition key is set only when its table is created, so this {subject} drops {table}, its partitions and their rows to create them again."))),
            Remade::Other => (format!("This {subject} drops the table {table} and its rows to create it again"), None),
        };
        let mut problem = Problem::new("drops-data", title);
        if let Some(why) = why {
            problem = problem.note(why);
        }
        if subject == "migration" {
            problem = problem.note("To keep the rows, add these lines to the migration it writes:").sql(&r.keep);
            // The blank line that closes the statements is the one before what follows.
            if problem.lines.last().is_some_and(|l| l.is_empty()) {
                problem.lines.pop();
            }
        }
        remakes.push(problem);
        // Each command from here on drops it too, so that any of them writes the migration.
        stated.drop.extend(gone.iter().map(&display));
    }
    let remade_from = problems.len();
    problems.extend(remakes);
    let unnamed: Vec<(&String, String, String)> = narrowed.iter().filter(|(id, _, _)| !named.contains(id)).map(|(id, shown, why)| (id, shown_id(shown), why.clone())).collect();
    if !guesses.is_empty() {
        let drops: Vec<(String, String, Option<String>)> = unstated.iter().map(|l| (l.split(' ').next().unwrap_or_default().to_string(), display(l), owner(l))).collect();
        let lossy: Vec<(String, String, Option<String>)> = unnamed.iter().map(|(id, name, why)| (name.clone(), why.clone(), owner(id))).collect();
        let outside: Vec<String> = unstated.iter().copied().chain(unnamed.iter().map(|(id, _, _)| *id)).filter(|id| owner(id).is_none()).map(&display).collect();
        problems.push(guess_problem(guesses, &drops, &lossy, &outside, &stated, asked));
        return problems;
    }
    if !unstated.is_empty() {
        let list: Vec<String> = unstated.iter().map(|l| display(l)).collect();
        let what: Vec<(String, String)> = unstated.iter().map(|l| (l.split(' ').next().unwrap_or_default().to_string(), display(l))).collect();
        let title = match unstated.as_slice() {
            [one] => format!("This {subject} drops {} and {}", drop_phrase(&what), if one.starts_with("column ") { "its data" } else { "its rows" }),
            _ => format!("This {subject} drops {}, with their data", drop_phrase(&what)),
        };
        // A column and a MySQL partition of one table may share a name, which --drop can't tell apart.
        let shared: Vec<&(String, String)> = what.iter().filter(|(_, n)| what.iter().filter(|(_, m)| m == n).count() > 1).collect();
        let r = reading(&stated, &list, &[]);
        let problem = asked.hazards(&r).iter().fold(Problem::new("drops-data", title), |p, w| p.note(format!("It also {w}.")));
        let mut problem = match shared.as_slice() {
            [] => problem.offer(if list.len() == 1 { "To drop it" } else { "To drop them" }, asked, r),
            [(a, name), (b, _), ..] => problem.note(format!("A {a} and a {b} share the name {name}: make one of the changes in schema.sql and write its migration, then the other.")),
            [_] => unreachable!("a name shared by one"),
        };
        // Where something comes as these go, any of them may be renamed: which became which is
        // for the person to say, so the command names no new name for them. Each is offered only
        // where something it could be renamed to comes.
        let mut like: Vec<String> = Vec::new();
        let mut renames: Vec<(String, String)> = Vec::new();
        for l in &unstated {
            let could = candidates(l);
            if !could.is_empty() {
                renames.push((display(l), "<new name>".to_string()));
            }
            for c in could {
                if !like.contains(&c) {
                    like.push(c);
                }
            }
        }
        if !like.is_empty() {
            let what = format!("If {} renamed instead, as {}", if renames.len() == 1 { "it was" } else { "any were" }, like.join(", "));
            problem = problem.fix(what, &[&create_command(asked, &stated, &[], &renames)]);
        }
        problems.push(problem);
    }
    for (_, name, why) in &unnamed {
        let r = reading(&stated, std::slice::from_ref(name), &[]);
        problems.push(asked.hazards(&r).iter().fold(Problem::new("loses-data", format!("Column {name} {why}")), |p, w| p.note(format!("It also {w}."))).offer("To accept that", asked, r));
    }
    // Where nothing after them names a command, the last table made again does.
    if problems.len() > remade_from && problems[remade_from..].iter().all(|p| p.fixes.is_empty()) {
        let last = problems.pop().expect("a problem");
        problems.push(last.offer(if subject == "migration" { "To write it, dropping the rows" } else { "To drop the rows" }, asked, reading(&stated, &[], &[])));
    }
    problems
}

/// The columns `choices` renames, as (old, new) ids of `from` and `to`; or why one cannot be.
#[cfg(feature = "cli")]
fn stated_renames(from: &crate::diff::Schema, to: &crate::diff::Schema, choices: &Choices, subject: &str) -> Result<Vec<(String, String)>, Vec<Problem>> {
    let gone: Vec<String> = from.objects.keys().filter(|id| id.starts_with("column ") && !to.objects.contains_key(*id)).cloned().collect();
    let mut out = Vec::new();
    let mut problems = Vec::new();
    for (old, new) in &choices.rename {
        let old_id = match resolve(&gone, old) {
            Ok(hits) if hits.len() == 1 => hits[0].clone(),
            Ok(_) => {
                problems.push(Problem::new("rename-name-unknown", format!("The --rename flag names {old}, which isn't a table or column this {subject} removes")).note("A column is named with its table, --rename users.full_name=name, and a table by its name, --rename users=accounts."));
                continue;
            }
            Err(could) => {
                problems.push(Problem::new("rename-name-ambiguous", format!("The --rename flag names {old}, which could be any of {}", could.iter().map(|c| shown_id(c)).collect::<Vec<_>>().join(", "))).note("Name it with its schema."));
                continue;
            }
        };
        let table = from.objects[&old_id].owner.clone().unwrap_or_default();
        let wanted = split_name(new).pop().unwrap_or_default();
        let added: Vec<(&String, String)> = to.objects.iter().filter(|(id, o)| o.kind == Some(crate::diff::Kind::Column) && o.owner.as_deref() == Some(table.as_str()) && !from.objects.contains_key(*id)).map(|(id, _)| (id, split_name(id.split_once(' ').map_or(id.as_str(), |(_, r)| r)).pop().unwrap_or_default())).collect();
        let exact: Vec<&String> = added.iter().filter(|(_, n)| *n == wanted).map(|(id, _)| *id).collect();
        let found = if exact.is_empty() { added.iter().filter(|(_, n)| n.eq_ignore_ascii_case(&wanted)).map(|(id, _)| *id).collect() } else { exact };
        match found.as_slice() {
            [new_id] => out.push((old_id, (*new_id).clone())),
            [] => problems.push(Problem::new("rename-target-unknown", format!("The --rename flag gives {old} the name {new}, which isn't a column this {subject} adds to its table"))),
            many => problems.push(Problem::new("rename-target-ambiguous", format!("The --rename flag gives {old} the name {new}, which could be any of {}", many.iter().map(|c| shown_id(c)).collect::<Vec<_>>().join(", "))).note("Quote the name as it is written: --rename 'users.n=\"Total\"'.")),
        }
    }
    if problems.is_empty() { Ok(out) } else { Err(problems) }
}

/// A plan `settle` settled: the statements that renamed tables on the database, the plan from there,
/// and the renames it took without being told, each the old name, the new one and whether it is a
/// table.
#[cfg(feature = "cli")]
struct Settled {
    renames: Vec<String>,
    plan: crate::diff::Plan,
    assumed: Vec<(String, String, bool)>,
    /// The tables renamed, each its old id and its new.
    tables: Vec<(String, String)>,
    /// The catalog as the table renames left it, read before any column was renamed, where one was.
    after_tables: Option<Vec<String>>,
    /// The catalog once the column renames that ran first ran, where any did.
    after_columns: Option<Vec<String>>,
}

/// The plan from `db`, which reads as `current`, to `target`, as `choices` settles it: tables renamed
/// as --rename states, or where one goes while one of the same columns comes, on `db` itself, keeping
/// their rows, with what Postgres named after them; then the diff engine's plan from there, which is
/// not run. With `asked`, a plan that drops a table or column `--drop` doesn't name, changes a
/// column's type in a way that loses values, or renames what may be new is refused, as is a name
/// `--drop` or `--rename` gives that names nothing; with `force` too, only the names are. Without it,
/// everything is taken as planned. `live` marks the schemas read from `db` as a live database's.
#[cfg(feature = "cli")]
async fn settle(db: &mut Db, mut current: crate::diff::Schema, target: &Target, live: bool, choices: &Choices, asked: Option<&Asked>, force: bool) -> Result<Result<Settled, Vec<Problem>>, Error> {
    let d = db.dialect();
    let subject = asked.map_or("change", |a| a.subject);
    let tables_only = |a: &crate::diff::Schema, b: &crate::diff::Schema| -> Vec<String> { a.objects.values().filter(|o| o.kind == Some(crate::diff::Kind::Table) && !b.objects.contains_key(&o.id)).map(|o| o.id.clone()).collect() };
    let (gone_tables, come_tables) = (tables_only(&current, &target.schema), tables_only(&target.schema, &current));
    let mut stated_tables = Vec::new();
    let mut column_renames = Vec::new();
    let mut problems = Vec::new();
    for (old, new) in &choices.rename {
        match resolve(&gone_tables, old) {
            Ok(hits) if hits.len() == 1 => match resolve(&come_tables, new) {
                Ok(to) if to.len() == 1 => stated_tables.push((hits[0].clone(), to[0].clone())),
                _ => problems.push(Problem::new("rename-target-unknown", format!("The --rename flag gives the table {old} the name {new}, which isn't a table this {subject} creates"))),
            },
            Err(could) => problems.push(Problem::new("rename-name-ambiguous", format!("The --rename flag names {old}, which could be any of {}", could.iter().map(|c| shown_id(c)).collect::<Vec<_>>().join(", "))).note("Name it with its schema.")),
            _ => column_renames.push((old.clone(), new.clone())),
        }
    }
    if !problems.is_empty() {
        return Ok(Err(problems));
    }
    let dropped_tables: std::collections::BTreeSet<String> = choices.drop.iter().filter_map(|w| resolve(&gone_tables, w).ok()).flatten().collect();
    let renamed_tables = crate::diff::table_renames(&current, &target.schema, &stated_tables, &dropped_tables);
    // A table renamed with its columns changing too is a guess only the person can settle.
    let mut guesses: Vec<Guess> = Vec::new();
    let mut guessed_tables: std::collections::BTreeSet<String> = Default::default();
    // Renames written without being asked: each old name, new name, and whether it is a table.
    let mut assumed: Vec<(String, String, bool)> = Vec::new();
    for (old, new, _) in renamed_tables.iter().filter(|(_, _, was_stated)| !was_stated) {
        match crate::diff::column_changes(&current, &target.schema, old, new) {
            Some(columns) => {
                let line = |(id, def): &(String, String)| format!("column {} {def}", shown_name(id));
                let mut diff = vec![gone_line(&format!("table {}", shown_name(old))), came_line(&format!("table {}", shown_name(new)))];
                for (a, b) in columns {
                    diff.extend(a.as_ref().map(|a| gone_line(&line(a))));
                    diff.extend(b.as_ref().map(|b| came_line(&line(b))));
                }
                guesses.push(Guess { old: shown_name(old), new: shown_name(new), diff, owner: None, id: new.clone(), table: true });
                guessed_tables.insert(new.clone());
            }
            None => assumed.push((shown_name(old), shown_name(new), true)),
        }
    }
    let before = current.clone();
    let mut renames = Vec::new();
    if !renamed_tables.is_empty() {
        renames = rename_tables(db, &current, target, &renamed_tables).await?;
        current = crate::diff::read(db).await?;
        current.live = live;
    }
    let settled = async {
        // A name --drop or --rename gives through a renamed table's old name means it under its new one.
        let follow = |name: &str| -> String {
            if !matches!(resolve(&gone_tables, name), Ok(hits) if hits.is_empty()) {
                return name.to_string();
            }
            let w = split_name(name);
            for (old_id, new_id, _) in &renamed_tables {
                let (old, new) = (split_name(&shown_id(old_id)), split_name(&shown_id(new_id)));
                let head = &w[..w.len().saturating_sub(1)];
                if !head.is_empty() && head.len() <= old.len() && old[old.len() - head.len()..] == *head {
                    let mut out: Vec<String> = new[new.len() - head.len()..].to_vec();
                    out.push(w.last().cloned().unwrap_or_default());
                    return out.join(".");
                }
            }
            name.to_string()
        };
        let columns = Choices { drop: choices.drop.iter().map(|w| follow(w)).collect(), rename: column_renames.iter().map(|(a, b)| (follow(a), b.clone())).collect(), ..Default::default() };
        let alterer = crate::diff::alterer(db).await?;
        let stated = match stated_renames(&current, &target.schema, &columns, subject) {
            Ok(stated) => stated,
            Err(problems) => return Ok(Err(problems)),
        };
        // The columns --drop names are never guessed renamed.
        let gone: Vec<String> = current.objects.keys().filter(|id| id.starts_with("column ") && !target.schema.objects.contains_key(*id)).cloned().collect();
        let dropped: std::collections::BTreeSet<String> = columns.drop.iter().filter_map(|w| resolve(&gone, w).ok()).flatten().collect();
        let mut plan = crate::diff::plan_with(&current, &target.schema, alterer.as_ref(), &stated, &dropped);
        let lost = plan.losses.clone();
        let narrowing: Vec<(String, String)> = plan.narrowings.clone();
        let guessed: Vec<String> = plan.renames.iter().map(|r| r.0.clone()).collect();
        let paired: Vec<String> = plan.pairs.iter().map(|p| p.0.clone()).collect();
        let claimed: std::collections::BTreeSet<String> = plan.renamed.iter().map(|(_, new)| new.clone()).collect();
        style(db, &mut plan, &target.schema).await?;
        let mut narrowed: Vec<(String, String, String)> = narrowing.iter().zip(&plan.narrowings).map(|((id, why), (shown, _))| (id.clone(), shown.clone(), why.clone())).collect();
        let guess_owner = |id: &str| current.objects.get(id).and_then(|o| o.owner.clone()).filter(|t| guessed_tables.contains(t));
        let mut guesses = guesses;
        let mut assumed = assumed;
        for (raw, (old, new, change)) in guessed.iter().zip(&plan.renames) {
            let old = shown_id(old);
            match change {
                Some((a, b)) => guesses.push(Guess { diff: column_diff(&old, new, a, b), old, new: new.clone(), owner: guess_owner(raw), id: raw.clone(), table: false }),
                None => assumed.push((old, new.clone(), false)),
            }
        }
        // A column dropped as one of another type comes, alone in a table that stays, is asked about
        // as a guess is: the plan drops it, and --rename keeps its rows. What the rename's change of
        // type would lose is named as a kept column's is.
        let (mut lost, mut shown) = (lost, plan.losses.clone());
        for (raw, (old, new, (a, b), loses)) in paired.iter().zip(&plan.pairs) {
            if current.objects.get(raw).and_then(|o| o.owner.as_ref()).is_some_and(|t| guessed_tables.contains(t)) {
                continue;
            }
            if let Some(k) = lost.iter().position(|l| l == raw) {
                lost.remove(k);
                shown.remove(k);
            }
            if let Some(why) = loses {
                narrowed.push((raw.clone(), old.clone(), why.clone()));
            }
            let old = shown_id(old);
            guesses.push(Guess { diff: column_diff(&old, new, a, b), old, new: new.clone(), owner: None, id: raw.clone(), table: false });
        }
        // Tables first, then columns, each by name.
        guesses.sort_by(|a, b| (!a.table, &a.old).cmp(&(!b.table, &b.old)));
        if let Some(asked) = asked {
            // The names a person might mean by a rename of what the plan drops: a table it creates,
            // and a column it adds to the table the dropped one was in, that nothing renames onto. A
            // partition of a table the schema has had is none: that table is made again, not renamed.
            let candidates = |id: &str| -> Vec<String> {
                let Some(gone) = current.objects.get(id).filter(|o| matches!(o.kind, Some(crate::diff::Kind::Column | crate::diff::Kind::Table))) else { return vec![] };
                let table = gone.kind == Some(crate::diff::Kind::Table);
                target.schema.objects.iter().filter(|(new, o)| o.kind == gone.kind && !current.objects.contains_key(*new) && !claimed.contains(*new) && (table || (o.owner.is_some() && o.owner == gone.owner)) && !o.extra.iter().any(|l| l.strip_prefix("partition of ").and_then(|p| p.split_once(' ')).is_some_and(|(p, _)| current.objects.contains_key(&format!("table {p}"))))).map(|(new, _)| shown_name(new)).collect()
            };
            let remade = remade_tables(&lost, &current, &target.schema, &stated);
            let mut problems = drop_problems(&lost, &shown, &narrowed, &guesses, &guess_owner, &candidates, &remade, &Choices { drop: columns.drop.clone(), rename: choices.rename.clone(), ..Default::default() }, asked);
            if force {
                problems.retain(|p| !DESTRUCTIVE.contains(&p.code));
            }
            if !problems.is_empty() {
                return Ok(Err(problems));
            }
        }
        Ok(Ok((plan, assumed, dropped)))
    }
    .await;
    // MySQL commits each rename on its own, so a refusal renames the tables back.
    if d == Dialect::Mysql && !renamed_tables.is_empty() && !matches!(settled, Ok(Ok(_))) {
        let back: Vec<(String, String, bool)> = renamed_tables.iter().rev().map(|(old, new, _)| (new.clone(), old.clone(), true)).collect();
        for statement in table_rename_statements(d, &target.schema, &before, &back) {
            db.script(&format!("{statement};")).await?;
        }
    }
    let tables = renamed_tables.iter().map(|(old, new, _)| (old.clone(), new.clone())).collect();
    // On Postgres the columns the plan renames are renamed first, on `db` itself, and the rest
    // planned from there: the server renames a column in every index, constraint, view and policy
    // that names it, which a plan from before the rename would drop and make again.
    let (mut after_tables, mut after_columns) = (None, None);
    let settled = match settled {
        Ok(Ok((plan, assumed, dropped))) if d == Dialect::Postgres && !plan.renamed.is_empty() => {
            if !renamed_tables.is_empty() {
                after_tables = Some(catalog(db).await?);
            }
            match columns_renamed_first(db, &plan, &current, target, &dropped, live).await? {
                Some((ran, rest)) => {
                    renames.extend(ran);
                    after_columns = Some(catalog(db).await?);
                    Ok(Ok((rest, assumed)))
                }
                None => Ok(Ok((plan, assumed))),
            }
        }
        other => other.map(|r| r.map(|(plan, assumed, _)| (plan, assumed))),
    };
    settled.map(|r| r.map(|(plan, assumed)| Settled { renames, plan, assumed, tables, after_tables, after_columns }))
}

/// The statements of a Postgres plan that rename the columns `renamed` names (each an old column's
/// id and the new one's), each rename's with the comments above it and the not null renamed after
/// it, by the rename's place in `renamed`, in the plan's order.
#[cfg(feature = "cli")]
fn column_rename_statements(plan: &crate::diff::Plan, renamed: &[&(String, String)]) -> Vec<(usize, Vec<String>)> {
    let bare = |id: &str| crate::sql::last_written(id);
    let table_of = |id: &str| id.strip_prefix("column ").and_then(|c| c.strip_suffix(bare(id).as_str())).and_then(|t| t.strip_suffix('.')).map(bare).unwrap_or_default();
    let wanted: Vec<(String, String)> = renamed.iter().map(|(old, new)| (bare(&table_of(old)), format!(" rename column {} to {}", bare(old), bare(new)))).collect();
    let is = |s: &str, what: &str| s.trim_start().to_ascii_lowercase().starts_with("alter table ") && s.contains(what);
    let (mut out, mut comments) = (Vec::new(), Vec::new());
    let mut statements = plan.statements.iter().peekable();
    while let Some(s) = statements.next() {
        if s.starts_with("--") {
            comments.push(s.clone());
            continue;
        }
        let table = s.trim_start().get("alter table ".len()..).and_then(|rest| rest.split(" rename column ").next()).map(bare);
        let which = wanted.iter().position(|(t, what)| s.trim_end().ends_with(what.as_str()) && table.as_deref() == Some(t.as_str()));
        if let Some(k) = which.filter(|_| is(s, " rename column ")) {
            let mut these = std::mem::take(&mut comments);
            these.push(s.clone());
            if let Some(next) = statements.next_if(|n| is(n, " rename constraint ")) {
                these.push(next.clone());
            }
            out.push((k, these));
        }
        comments.clear();
    }
    out
}

/// Runs `sql` on `db` whole or not at all: in a transaction of its own, or under a savepoint where
/// `db` is in one already (`live`: a push's). Whether it ran.
#[cfg(feature = "cli")]
async fn run_or_undo(db: &mut Db, sql: &str, live: bool) -> Result<bool, Error> {
    let (open, keep, undo) = if live { ("savepoint reylo_renames", "release savepoint reylo_renames", "rollback to savepoint reylo_renames") } else { ("begin", "commit", "rollback") };
    db.exec(open).await?;
    if db.script(sql).await.is_ok() {
        db.exec(keep).await?;
        return Ok(true);
    }
    db.exec(undo).await?;
    if live {
        db.exec(keep).await?;
    }
    Ok(false)
}

/// What follows the column renames `renamed` (each an old column's id and the new one's) once they
/// ran on `db`, which then reads as `now`, as statements, where the target names it after the new
/// column: a view's or a materialized view's column of the old column's name, which the server
/// leaves as it was, where the view's definition isn't yet the target's; and a serial column's
/// sequence, which keeps its name.
#[cfg(feature = "cli")]
async fn following_renames(db: &mut Db, now: &crate::diff::Schema, target: &Target, renamed: &[&(String, String)]) -> Result<Vec<String>, Error> {
    use crate::diff::postgres::quote_ident;
    let bare = |id: &str| crate::sql::last_written(id);
    let mut out = Vec::new();
    let views = db
        .rows(
            r"select format('%I.%I', n.nspname, c.relname) as rel, c.relkind::text as kind, a.attname as name
            from pg_attribute a join pg_class c on c.oid = a.attrelid join pg_namespace n on n.oid = c.relnamespace
            where c.relkind in ('v', 'm') and a.attnum > 0 and not a.attisdropped and n.nspname not in ('pg_catalog', 'information_schema') and n.nspname not like 'pg\_%'",
        )
        .await?;
    for (old, new) in renamed {
        let (old, new) = (bare(old), bare(new));
        for v in views.iter().filter(|v| quote_ident(v.text("name")) == old) {
            let (rel, word) = (v.text("rel"), if v.text("kind") == "m" { "materialized view" } else { "view" });
            let id = format!("{word} {rel}");
            let differs = now.objects.get(&id).zip(target.schema.objects.get(&id)).is_some_and(|(a, b)| a.def != b.def);
            if differs && !views.iter().any(|w| w.text("rel") == rel && quote_ident(w.text("name")) == new) {
                out.push(format!("alter {word} {rel} rename column {old} to {new}"));
            }
        }
    }
    for (_, new) in renamed {
        let owned_by = |s: &crate::diff::Schema| s.objects.values().find(|o| o.kind == Some(crate::diff::Kind::Sequence) && o.goes_with.as_deref() == Some(new.as_str())).map(|o| o.id.clone());
        let (Some(was), Some(is)) = (owned_by(now), owned_by(&target.schema)) else { continue };
        if was != is && !now.objects.contains_key(&is) && !target.schema.objects.contains_key(&was) {
            out.push(format!("alter sequence {} rename to {}", was.trim_start_matches("sequence "), bare(&is)));
        }
    }
    crate::diff::postgres::style_statements(db, &mut out, &target.schema.verbatim).await?;
    Ok(out)
}

/// The constraints of `now` that the target has under another name, on the same table and alike but
/// for it, each once (a key, a check or a foreign key named after a renamed column), as statements
/// that rename them.
#[cfg(feature = "cli")]
async fn constraint_renames(db: &mut Db, now: &crate::diff::Schema, target: &Target) -> Result<Vec<String>, Error> {
    use crate::diff::Kind;
    let constraints = |s: &crate::diff::Schema, other: &crate::diff::Schema| -> Vec<(String, String, String)> {
        s.objects.values().filter(|o| o.kind == Some(Kind::Constraint) && !other.objects.contains_key(&o.id)).filter_map(|o| Some((o.id.clone(), o.owner.clone()?, o.def.clone()))).collect()
    };
    let (gone, come) = (constraints(now, &target.schema), constraints(&target.schema, now));
    let mut out = Vec::new();
    for (id, owner, def) in &gone {
        let alike = |list: &[(String, String, String)]| list.iter().filter(|(_, o, d)| o == owner && d == def).count();
        let [(new, _, _)] = come.iter().filter(|(_, o, d)| o == owner && d == def).collect::<Vec<_>>()[..] else { continue };
        if alike(&gone) != 1 {
            continue;
        }
        let Some(table) = owner.strip_prefix("table ") else { continue };
        out.push(format!("alter table {table} rename constraint {} to {}", crate::sql::last_written(id), crate::sql::last_written(new)));
    }
    crate::diff::postgres::style_statements(db, &mut out, &target.schema.verbatim).await?;
    Ok(out)
}

/// `plan`'s column renames run on `db` with what follows them, and the plan from there to `target`,
/// which carries `plan`'s renames for what is shown of them; None, running nothing, where none can
/// run first. A rename runs first where its column keeps its values (a change of type, collation or
/// stored expression makes what holds it again around the change, as the plan writes it), its new
/// name is no column of `db` (one the plan drops first), and it runs on `db` (its not null's new name
/// may be one the plan frees first); the rest stay in the plan, as stated.
#[cfg(feature = "cli")]
async fn columns_renamed_first(db: &mut Db, plan: &crate::diff::Plan, current: &crate::diff::Schema, target: &Target, dropped: &std::collections::BTreeSet<String>, live: bool) -> Result<Option<(Vec<String>, crate::diff::Plan)>, Error> {
    let alterer = crate::diff::alterer(db).await?;
    let candidates: Vec<&(String, String)> = plan.renamed.iter().filter(|(old, new)| !current.objects.contains_key(new) && current.objects.get(old).zip(target.schema.objects.get(new)).is_some_and(|(a, b)| alterer.keeps_values(a, b))).collect();
    let sql = |statements: &[String]| statements.iter().filter(|s| !s.starts_with("--")).map(|s| format!("{};", s.trim_end())).collect::<Vec<_>>().join("\n");
    let (mut ran, mut first) = (Vec::new(), Vec::new());
    for (k, statements) in column_rename_statements(plan, &candidates) {
        if !first.contains(&candidates[k]) && run_or_undo(db, &sql(&statements), live).await? {
            ran.extend(statements);
            first.push(candidates[k]);
        }
    }
    if first.is_empty() {
        return Ok(None);
    }
    let mut now = crate::diff::read(db).await?;
    let following = following_renames(db, &now, target, &first).await?;
    if !following.is_empty() && run_or_undo(db, &sql(&following), live).await? {
        ran.extend(following);
        now = crate::diff::read(db).await?;
    }
    let renamed_constraints = constraint_renames(db, &now, target).await?;
    if !renamed_constraints.is_empty() && run_or_undo(db, &sql(&renamed_constraints), live).await? {
        ran.extend(renamed_constraints);
        now = crate::diff::read(db).await?;
    }
    now.live = live;
    let stated: Vec<(String, String)> = plan.renamed.iter().filter(|r| !first.contains(&r)).cloned().collect();
    let mut rest = crate::diff::plan_with(&now, &target.schema, alterer.as_ref(), &stated, dropped);
    style(db, &mut rest, &target.schema).await?;
    (rest.renames, rest.renamed, rest.pairs) = (plan.renames.clone(), plan.renamed.clone(), plan.pairs.clone());
    Ok(Some((ran, rest)))
}

/// The new migration's text: the diff engine's plan from the migrations replayed in order to
/// `schema.sql`, run on the replay and compared with `schema.sql` before it is written. A plan that
/// does not reach it is written all the same, with what remains, for the person (or agent) to finish.
/// `added` are extensions the shadows leave out beside those the project's files create.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
#[cfg(feature = "cli")]
async fn draft(opts: &Options, files: &[Migration], schema_sql: &str, target_name: &str, added: &[String], migration: &str, choices: &Choices, log: Log<'_>) -> Result<Result<Drafted, Vec<Problem>>, Error> {
    let first = draft_with(opts, files, schema_sql, target_name, added, migration, choices, &[], log).await?;
    // On Postgres and MySQL, each command a refusal suggests is tried, and takes `--blocking` where
    // the migration it writes would be refused for nothing else: one more run would only add it.
    let readings: Vec<Choices> = match &first {
        Err(problems) if !choices.blocking && matches!(Dialect::of_url(&opts.url).ok(), Some(Dialect::Postgres | Dialect::Mysql)) => {
            let mut out: Vec<Choices> = Vec::new();
            for r in problems.iter().flat_map(|p| &p.readings) {
                if !out.contains(r) {
                    out.push(r.clone());
                }
            }
            out
        }
        _ => return Ok(first),
    };
    let mut rewrites = Vec::new();
    for r in readings {
        if let Ok(Err(problems)) = draft_with(opts, files, schema_sql, target_name, added, migration, &r, &[], &mut |_: &str| {}).await {
            if !problems.is_empty() && problems.iter().all(|p| p.code == "blocks-writes") {
                rewrites.push((r, problems.iter().flat_map(|p| p.hazards.iter().cloned()).collect()));
            }
        }
    }
    if rewrites.is_empty() {
        return Ok(first);
    }
    draft_with(opts, files, schema_sql, target_name, added, migration, choices, &rewrites, &mut |_: &str| {}).await
}

/// `draft`, with the suggested commands `rewrites` names (each as it states its flags, without
/// `--blocking`, with what holds the lock) taking `--blocking`, and the refusal saying why.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
#[cfg(feature = "cli")]
async fn draft_with(opts: &Options, files: &[Migration], schema_sql: &str, target_name: &str, added: &[String], migration: &str, choices: &Choices, rewrites: &[(Choices, Vec<String>)], log: Log<'_>) -> Result<Result<Drafted, Vec<Problem>>, Error> {
    let drafted = |texts: Vec<String>, code: i32, notes: Vec<String>, empty: bool| Drafted { texts, code, notes, empty, target: target_name.to_string(), lock_safe: false, commits: 0, changes: vec![] };
    let mut db = Db::open(opts).await?;
    let mut problems = out_of_reach(&mut db, &opts.url).await?;
    problems.extend(unreadable_grants(&mut db, opts, files, Some(schema_sql)).await?);
    if !problems.is_empty() {
        db.close().await;
        return Ok(Err(problems));
    }
    let mut server = Server::open(&mut db, &opts.url, opts.shadow_url.as_deref()).await?;
    server.copy_default_privileges(!sets_default_privileges(opts));
    server.leave_extensions(created_extensions(opts).into_iter().chain(added.iter().cloned()).collect());
    server.leave_schemas(created_schemas(opts));
    left_out_note(&server, log);
    db.close().await;
    let texts: Vec<&str> = files.iter().map(|f| f.sql.as_str()).chain([schema_sql]).collect();
    let mut shadows = Shadows::new(&mut server, &opts.url, &opts.root, &opts.cache, &texts).await;
    let result = async {
        let a = shadows.lane();
        shadows.push(a, [Step::script("schema.sql", schema_sql)]);
        if let Err((_, e)) = shadows.open(a).await? {
            return Ok(Err(vec![fails_problem(target_name, e)]));
        }
        // The plan runs on the replay, which is then no longer one: the replay is kept
        // first, as the start of the check that follows.
        let c = shadows.lane();
        shadows.push(c, files.iter().map(Step::of));
        shadows.mark(c);
        if let Err((m, e)) = shadows.open(c).await? {
            return Ok(Err(vec![Problem::new("replay-fails", format!("Replaying {m} in a shadow database fails")).note(e)]));
        }
        shadows.taint(c);
        // What the migrations make, for the summary of what the new one changes.
        let before = catalog(shadows.db(c)).await?;
        let unread = crate::catalog::unmodeled(shadows.db(a), true).await?;
        unmodeled_note("Reylo doesn't fully compare these in schema.sql, so a migration can leave them, or a change to them, out: check them by hand", &unread, log);
        let mut target = match crate::diff::read(shadows.db(a)).await {
            Ok(schema) => schema,
            Err(e) => {
                let empty = format!("  {} The migration is empty, to write the change to {target_name} by hand: {e}", paint("yellow", "!"));
                return Ok(Ok(drafted(vec![String::new()], 0, vec![empty], false)));
            }
        };
        let mut current = crate::diff::read(shadows.db(c)).await?;
        // On Postgres the file creates each object from the text schema.sql declares it with, and a
        // table moved to another schema is moved first, keeping its rows: the moves run on the
        // replay before the rest is planned from it.
        let mut moves = Vec::new();
        let mut moved: Option<Vec<String>> = None;
        if shadows.db(c).dialect() == Dialect::Postgres {
            crate::diff::postgres::declare(shadows.db(a), &mut target, schema_sql).await?;
            moves = crate::diff::postgres::moves(&current, &target);
            crate::diff::postgres::style_statements(shadows.db(c), &mut moves, &target.verbatim).await?;
            if !moves.is_empty() {
                let file = Migration { name: "moves".into(), time: String::new(), path: PathBuf::new(), sql: moves.iter().map(|m| format!("{m};")).collect::<Vec<_>>().join("\n") };
                run(shadows.db(c), &file).await?;
                current = crate::diff::read(shadows.db(c)).await?;
                moved = Some(catalog(shadows.db(c)).await?);
            }
        }
        // Tables renamed, as --rename states, or where one goes while one of the same columns comes:
        // renamed first on the replay, keeping their rows, with what Postgres named after them, and
        // the rest planned from there.
        let d = shadows.db(c).dialect();
        let indexes = if d == Dialect::Postgres { crate::diff::postgres::index_names(shadows.db(a)).await? } else { Default::default() };
        let target = Target { schema: target, indexes };
        let asked = Asked::migration(migration, rewrites);
        let Settled { renames, mut plan, assumed, tables, after_tables, after_columns } = match settle(shadows.db(c), current, &target, false, choices, Some(&asked), false).await? {
            Ok(settled) => settled,
            Err(problems) => return Ok(Err(problems)),
        };
        // The schema the table renames left, for the summary, which shows each as a rename and leaves
        // out what only took its new name.
        let renamed = match (tables.is_empty(), after_tables) {
            (true, _) => None,
            (false, Some(lines)) => Some(lines),
            (false, None) => Some(catalog(shadows.db(c)).await?),
        };
        moves.extend(renames);
        if plan.statements.is_empty() && moves.is_empty() {
            return Ok(Ok(drafted(vec![String::new()], 0, vec![], true)));
        }
        // On Postgres, a plan that would hold a lock that blocks writes to a table with rows while it
        // reads them all is written in the shape that doesn't, as several files (`locks`).
        let existing = if d == Dialect::Postgres { crate::locks::existing(shadows.db(c)).await? } else { Default::default() };
        let mut split = crate::locks::lock_safe(&plan, &existing);
        let commits = plan.statements.iter().filter(|s| *s == crate::diff::COMMIT).count();
        // The plan runs on the replay as `deploy` will run its files: one file in one transaction (on
        // SQLite with foreign keys off until it is checked), on MySQL a file per statement, and the
        // lock-safe shape's files one after another. Each says the locks it holds.
        let pieces = |split: &Option<Vec<Vec<String>>>, plan: &crate::diff::Plan| -> Vec<String> {
            match split {
                Some(files) => crate::locks::file_texts(plan, files),
                None if d == Dialect::Mysql => plan.runnable().cloned().collect(),
                None => vec![plan.runnable().map(|s| format!("{};", s.trim_end())).collect::<Vec<_>>().join("\n")],
            }
        };
        let run_pieces = async |db: &mut Db, sqls: Vec<String>| -> Result<(Option<String>, Vec<crate::locks::Hazard>), Error> {
            let mut hazards = Vec::new();
            for sql in sqls {
                let file = Migration { name: "plan".into(), time: String::new(), path: PathBuf::new(), sql };
                match crate::locks::run_watched(db, &file).await {
                    Ok(found) => hazards.extend(found),
                    Err(e) => return Ok((Some(e.0), hazards)),
                }
            }
            Ok((None, hazards))
        };
        let (mut failed, mut hazards) = run_pieces(shadows.db(c), pieces(&split, &plan)).await?;
        // On MySQL each statement is a file of its own: a table an earlier one made has no rows.
        if d == Dialect::Mysql {
            let made = crate::locks::mysql_tables_made(plan.runnable().map(String::as_str));
            hazards.retain(|h| !made.contains(&h.table.to_lowercase()));
        }
        let wanted = match shadows.lines(a).await? {
            Ok(lines) => lines,
            Err((_, e)) => return Ok(Err(vec![fails_problem(target_name, e)])),
        };
        let mut reached = catalog(shadows.db(c)).await?;
        // Where the lock-safe shape doesn't reach the target, each on a new replay: the same shape
        // with each index the plan replaces dropped before its replacement is built (a statement
        // between that needs the old one gone), then the plan as one file, as the planner wrote it
        // (a statement that needs a constraint validated, or a column not null, before the last file).
        let mut notes = Vec::new();
        if split.is_some() && (failed.is_some() || reached != wanted) {
            let dropping_first = crate::locks::lock_safe_dropping_first(&plan, &existing).filter(|f| Some(f) != split.as_ref());
            let mut tries = vec![None];
            if dropping_first.is_some() {
                tries.insert(0, dropping_first);
            }
            for shape in tries {
                let c2 = shadows.lane();
                shadows.push(c2, files.iter().map(Step::of));
                if let Err((m, e)) = shadows.open(c2).await? {
                    return Ok(Err(vec![Problem::new("replay-fails", format!("Replaying {m} in a shadow database fails")).note(e)]));
                }
                shadows.taint(c2);
                if !moves.is_empty() {
                    let file = Migration { name: "moves".into(), time: String::new(), path: PathBuf::new(), sql: moves.iter().map(|m| format!("{m};")).collect::<Vec<_>>().join("\n") };
                    run(shadows.db(c2), &file).await?;
                }
                (failed, hazards) = run_pieces(shadows.db(c2), pieces(&shape, &plan)).await?;
                reached = catalog(shadows.db(c2)).await?;
                let note = match (&shape, commits) {
                    (Some(_), _) => format!("  {} Drops each index it replaces before it builds the new one: built first, the new ones didn't reach {target_name}", paint("yellow", "!")),
                    (None, 0) => format!("  {} Written as one file: split so that none blocks writes while it reads every row, it didn't reach {target_name}", paint("yellow", "!")),
                    (None, _) => format!("  {} Split only after each new enum value: split so that none blocks writes while it reads every row, it didn't reach {target_name}", paint("yellow", "!")),
                };
                let last = shape.is_none();
                split = shape;
                if last || (failed.is_none() && reached == wanted) {
                    notes.push(note);
                    break;
                }
            }
        }
        if !hazards.is_empty() && !choices.blocking && failed.is_none() && reached == wanted {
            return Ok(Err(vec![lock_problem(&hazards, &asked, choices)]));
        }
        for h in &hazards {
            notes.push(format!("  {} It {}", paint("yellow", "!"), h.what));
        }
        let split_files = split.as_ref().map(|files| files.len());
        let mut texts = match split {
            Some(mut files) => {
                // The moves first, in a file of their own where the first is an index built
                // concurrently; past the files that commit an enum's new labels alone.
                if !moves.is_empty() {
                    let at = commits.min(files.len().saturating_sub(1));
                    let first = files[at].iter().filter(|s| !s.starts_with("--")).map(|s| format!("{s};")).collect::<Vec<_>>().join("\n");
                    if runs_alone(&first, d) {
                        files.insert(at, std::mem::take(&mut moves));
                    } else {
                        files[at].splice(0..0, std::mem::take(&mut moves));
                    }
                }
                crate::locks::file_texts(&plan, &files)
            }
            None => {
                let at = plan.statements.iter().rposition(|s| s == crate::diff::COMMIT).map_or(0, |c| c + 1);
                plan.statements.splice(at..at, std::mem::take(&mut moves));
                plan_files(&plan, d)
            }
        };
        for loss in &plan.losses {
            notes.push(format!("  {} Drops {} and its {}", paint("yellow", "!"), described(loss), if loss.starts_with("column ") { "data" } else { "rows" }));
        }
        for (column, why) in &plan.narrowings {
            notes.push(format!("  {} Column {} {why}", paint("yellow", "!"), column.trim_start_matches("column ")));
        }
        if !assumed.is_empty() {
            let one = assumed.len() == 1;
            let names: Vec<String> = assumed.iter().map(|(old, _, _)| old.clone()).collect();
            // The summary above shows each rename.
            notes.push(format!("  {} {}", paint("yellow", "!"), if one { "Written as a rename, keeping its rows" } else { "Written as renames, keeping their rows" }));
            let unasked = match (assumed.iter().all(|a| a.2), assumed.iter().all(|a| !a.2)) {
                (true, _) if one => "If they're different tables",
                (_, true) if one => "If they're different columns",
                _ => "Where any aren't renames",
            };
            notes.push(paint("dim", &format!("    {unasked}, delete the file, then:")));
            notes.push(format!("      {}", paint("cyan", &create_command(&asked, choices, &names, &[]))));
        }
        if failed.is_none() && reached == wanted {
            let steps = Steps { dialect: d, before: &before, moved: moved.as_deref(), renamed: renamed.as_deref(), columns: after_columns.as_deref(), after: &reached };
            return Ok(Ok(Drafted { lock_safe: split_files.is_some(), commits, changes: summary_lines(&steps, &tables, &plan.renamed), ..drafted(texts, 0, notes, false) }));
        }
        let mut remaining = Vec::new();
        if let Some(f) = &failed {
            remaining.push(format!("fails at {f}"));
        }
        let (only_a, only_b) = difference(&reached, &wanted);
        remaining.extend(only_a.iter().map(|l| format!("- {l}")));
        remaining.extend(only_b.iter().map(|l| format!("+ {l}")));
        notes.push(paint("bold red", &format!("✗ The migration doesn't reach {target_name}: what remains is written at its top, to finish by hand")));
        notes.extend(remaining.iter().map(|r| format!("  {r}")));
        texts[0] = format!("-- This plan does not reach {target_name} yet. What remains:\n{}\n{}", remaining.iter().map(|r| format!("--   {}", r.replace('\n', " "))).collect::<Vec<_>>().join("\n"), texts[0]);
        Ok(Ok(Drafted { lock_safe: split_files.is_some(), commits, ..drafted(texts, 1, notes, false) }))
    }
    .await;
    shadows.finish().await;
    server.close().await;
    result
}

/// The database's schema as SQL that makes it, for init's schema.sql, `db introspect` and a `migrate
/// new` with no schema.sql, or the refusal that says why it can't be written: written as `migrate
/// new` writes a migration (lowercase, unqualified, from Reylo's own reading of the database) where
/// a shadow database shows that text makes exactly this schema; else the database's own dump, which
/// also holds what Reylo reads by name only.
#[cfg(feature = "cli")]
async fn schema_as_sql(opts: &Options, schemas: &[String]) -> Result<Result<String, Problem>, Error> {
    let mut db = Db::open(opts).await?;
    let result = schema_as_sql_on(&mut db, opts, schemas, true, &mut Vec::new()).await;
    db.close().await;
    result
}

/// `shadowed` is whether the command needs shadow databases anyway: where none can be made, it is
/// told so, rather than given the dump. Where none can be made on the server `shadow_url` names,
/// every command is refused: that server is the project's for shadows, so one that can't make them
/// is a setting to fix, not a reason to print SQL of another kind. Where the text is the dump,
/// `dumped` says why, as lines a person reads.
#[cfg(feature = "cli")]
async fn schema_as_sql_on(db: &mut Db, opts: &Options, schemas: &[String], shadowed: bool, dumped: &mut Vec<String>) -> Result<Result<String, Problem>, Error> {
    if db.dialect() != Dialect::Postgres {
        return checked_against_dump(db, opts, schemas, shadowed, dumped).await;
    }
    // Reylo's own SQL, checked against the database itself, needs no pg_dump: only the dump it
    // falls back on does. It makes every schema, so --schema naming only some of those that hold
    // anything leaves the dump.
    let partial = left_out(db, schemas).await?;
    let why = if !partial.is_empty() {
        vec![format!("  Reylo's own SQL makes every schema, and --schema leaves out {}", and_list(&partial))]
    } else {
        match own_sql(db, opts).await? {
            Ok(sql) => return Ok(Ok(sql)),
            Err(NotOwn::NoShadow(e)) if shadowed => return Err(e),
            Err(NotOwn::NoShadow(e)) if opts.shadow_url.is_some() => return Ok(Err(shadow_url_problem(&e))),
            Err(NotOwn::NoShadow(e)) => no_shadow(&e),
            Err(NotOwn::Short(why)) => why,
        }
    };
    let version: i64 = db.rows("select current_setting('server_version_num') as v").await?.first().and_then(|r| r.first()?.parse().ok()).unwrap_or(0);
    if let Err(missing) = crate::db::find_pg_dump(version / 10000) {
        return Ok(Err(pg_dump_problem(&missing, why)));
    }
    let sql = dump_from(db, opts, schemas).await?;
    *dumped = why;
    Ok(Ok(sql))
}

/// Why the schema is the dump where no shadow database can be made on the database's own server,
/// `shadow_url` unset.
#[cfg(feature = "cli")]
fn no_shadow(e: &Error) -> Vec<String> {
    vec![format!("  No shadow database can be made to check Reylo's own SQL for it: {e}")]
}

/// The refusal where no shadow database can be made on the server `shadow_url` names.
#[cfg(feature = "cli")]
fn shadow_url_problem(e: &Error) -> Problem {
    Problem::new("shadow-url-unusable", "No shadow database can be made on the server shadow_url names")
        .lines([format!("  {e}")])
        .note("Reylo's own SQL is checked on a shadow database before it's printed.")
        .note("Make sure that server is running and its role can create a database, or set shadow_url in .reylo.yaml to one that can.")
}

/// What --schema (`schemas`, where it names any) leaves out: each schema of the project's it doesn't
/// name that Reylo's own SQL would write ([`schemas_to_name`]). Postgres alone has schemas to name.
#[cfg(feature = "cli")]
async fn left_out(db: &mut Db, schemas: &[String]) -> Result<Vec<String>, Error> {
    if schemas.is_empty() || db.dialect() != Dialect::Postgres {
        return Ok(vec![]);
    }
    Ok(schemas_to_name(db).await?.into_iter().filter(|s| !schemas.contains(s)).collect())
}

/// `schemas`, as --schema names them, with each schema of the project's that what they hold refers
/// to, said in a note: SQL that made them alone wouldn't run.
#[cfg(feature = "cli")]
async fn with_referenced(db: &mut Db, schemas: &[String], log: Log<'_>) -> Result<Vec<String>, Error> {
    if schemas.is_empty() || db.dialect() != Dialect::Postgres {
        return Ok(schemas.to_vec());
    }
    let project = crate::catalog::user_schemas(db).await?;
    let mut named = schemas.to_vec();
    loop {
        let more: Vec<String> = referenced_schemas(db, &named).await?.into_iter().filter(|s| project.contains(s) && !named.contains(s)).collect();
        if more.is_empty() {
            break;
        }
        named.extend(more);
    }
    if named.len() > schemas.len() {
        log(&note(format!("Reading {} too, which {} refers to", and_list(&named[schemas.len()..]), and_list(schemas))));
    }
    Ok(named)
}

/// The schemas of the project's that --schema has to name for SQL of them to hold them: each that
/// holds an object of its own (not one an extension made, nor what comes with another object, as a
/// table's index or sequence does, nor a history table), and each that holds nothing at all, but
/// `public`, which every new database has. One that holds an extension alone is made with it.
#[cfg(feature = "cli")]
async fn schemas_to_name(db: &mut Db) -> Result<Vec<String>, Error> {
    let rows = db
        .rows(&format!(
            "select n.nspname as name from pg_namespace n where {} and {} and (exists (
               select 1 from pg_depend d where d.refclassid = 'pg_namespace'::regclass and d.refobjid = n.oid and d.deptype = 'n'
                 and not exists (select 1 from pg_depend o where o.classid = d.classid and o.objid = d.objid and o.deptype in ('e', 'a', 'i'))
                 and not (d.classid = 'pg_class'::regclass and d.objid in (select oid from pg_class where not {})))
               or (n.nspname <> 'public' and not exists (select 1 from pg_depend d where d.refclassid = 'pg_namespace'::regclass and d.refobjid = n.oid)))
             order by 1",
            crate::catalog::PG_USER_NS,
            crate::catalog::not_platform_schema("n"),
            crate::catalog::not_history("relname")
        ))
        .await?;
    Ok(rows.iter().map(|r| r.text("name").to_string()).collect())
}

/// The schemas outside `named` that what `named` holds refers to: a view's tables, a key's table, a
/// column's type, a default's sequence, a trigger's function.
#[cfg(feature = "cli")]
async fn referenced_schemas(db: &mut Db, named: &[String]) -> Result<Vec<String>, Error> {
    let list = named.iter().map(|s| crate::db::literal(s)).collect::<Vec<_>>().join(", ");
    // The schema of what refers: a rule's, a default's, a trigger's and a policy's is its table's.
    let on_table = |catalog: &str, table: &str| format!("when '{catalog}'::regclass then (select c.relnamespace from {catalog} x join pg_class c on c.oid = x.{table} where x.oid = d.objid)");
    let rows = db
        .rows(&format!(
            "select distinct r.schema as name from pg_depend d
               cross join lateral (select coalesce(case d.classid {} {} {} {} end,
                 (select n.oid from pg_namespace n where n.nspname = (pg_identify_object(d.classid, d.objid, 0)).schema)) as ns) o
               join pg_namespace n on n.oid = o.ns
               cross join lateral pg_identify_object(d.refclassid, d.refobjid, 0) r
             where d.deptype = 'n' and d.objid >= 16384 and n.nspname in ({list}) and r.schema is not null and r.schema not in ({list})
             order by 1",
            on_table("pg_rewrite", "ev_class"),
            on_table("pg_attrdef", "adrelid"),
            on_table("pg_trigger", "tgrelid"),
            on_table("pg_policy", "polrelid")
        ))
        .await?;
    Ok(rows.iter().map(|r| r.text("name").to_string()).collect())
}

/// `live` as a shadow that ran SQL written from it reads, where the SQL makes all of it: without the
/// lines the environment gives a shadow as it is made and `made` lacks, which the SQL leaves to it
/// (the privileges on what an extension made). Default privileges, partitions, comments and the
/// rest stay, for the SQL to make.
#[cfg(feature = "cli")]
fn as_made(live: &[String], made: &[String]) -> Vec<String> {
    use crate::catalog::EXTENSION_GRANT;
    live.iter().filter(|l| !l.starts_with(EXTENSION_GRANT) || made.contains(l)).cloned().collect()
}

/// The refusal of an init whose --schema (`schemas`) leaves out schemas of the project's (`left`):
/// init.sql must make the whole schema, to be recorded as run.
#[cfg(feature = "cli")]
fn schemas_left_out(schemas: &[String], left: &[String]) -> Problem {
    let flags: Vec<String> = schemas.iter().chain(left).map(|s| format!("--schema {}", shell_word(s))).collect();
    Problem::new("schemas-left-out", format!("This database's schema is in {} too, which --schema leaves out", and_list(left)))
        .note("The whole schema has to be in init.sql, to record it as run here.")
        .fix("Name every schema it's in", &[format!("reylo migrate init {}", flags.join(" ")).as_str()])
}

/// The extensions of the project's a Postgres database has, by name, lowercase.
#[cfg(feature = "cli")]
async fn project_extensions(db: &mut Db) -> Result<Vec<String>, Error> {
    let rows = db.rows(&format!("select lower(x.extname) as name from pg_extension x join pg_namespace n on n.oid = x.extnamespace where x.extname <> 'plpgsql' and {}", crate::catalog::project_extension("x", "n"))).await?;
    Ok(rows.iter().map(|r| r.text("name").to_string()).collect())
}

/// Why Reylo's own SQL for a schema isn't taken: no shadow database can be made to check it, or it
/// falls short, as lines a person reads.
#[cfg(feature = "cli")]
enum NotOwn {
    NoShadow(Error),
    Short(Vec<String>),
}

/// Reylo's own SQL for a Postgres database's schema, the plan `migrate new` would write from an
/// empty database to this one, where a shadow database that runs it then reads as this database
/// does; else why not.
#[cfg(feature = "cli")]
async fn own_sql(db: &mut Db, opts: &Options) -> Result<Result<String, NotOwn>, Error> {
    let mut server = match shadow_server(db, opts).await {
        Ok(server) => server,
        Err(e) => return Ok(Err(NotOwn::NoShadow(e))),
    };
    // The project's extensions are the text's to make, as a fresh database needs them: a shadow that
    // already had them would hide a plan that leaves them out. So are its default privileges and
    // its grants on `public`.
    server.leave_extensions(project_extensions(db).await?);
    server.copy_no_privileges();
    let result = async {
        let live = catalog(db).await?;
        let mut empty = match server.shadow().await {
            Ok(shadow) => shadow,
            Err(e) => return Ok(Err(NotOwn::NoShadow(e))),
        };
        let planned = plan_text(&mut empty.db, db, None).await;
        server.drop_shadow(empty).await;
        let Some(planned) = planned else { return Ok(Err(NotOwn::Short(vec!["  Reylo can't write it as SQL of its own".to_string()]))) };
        // A blank line between statements, for a file a person reads whole; one with comments of
        // the plan's own stays as written.
        let sql = if planned.lines().any(|l| l.trim_start().starts_with("--")) {
            planned
        } else {
            crate::sql::split_statements(&planned, db.dialect()).iter().map(|st| format!("{};", st.text.trim())).collect::<Vec<_>>().join("\n\n")
        };
        let mut shadows = Shadows::new(&mut server, &opts.url, &opts.root, &opts.cache, &[sql.as_str()]).await;
        let lane = shadows.lane();
        shadows.push(lane, [Step::script("schema.sql", &sql)]);
        let made = shadows.lines(lane).await;
        shadows.finish().await;
        let made = match made? {
            Ok(made) => made,
            Err((_, e)) => return Ok(Err(NotOwn::Short(vec![format!("  Reylo's own SQL for it fails on a shadow database: {e}")]))),
        };
        let live = as_made(&live, &made);
        if live == made {
            return Ok(Ok(format!("{}\n", sql.trim_end())));
        }
        let mut why = vec!["  Reylo's own SQL doesn't make all of it:".to_string()];
        why.extend(changed_lines(&made, &live, db.dialect()));
        Ok::<_, Error>(Err(NotOwn::Short(why)))
    }
    .await;
    server.close().await;
    result
}

/// The refusal of a schema Reylo's own SQL doesn't make, for the reason `why` gives, where no
/// pg_dump can write it instead.
#[cfg(feature = "cli")]
fn pg_dump_problem(missing: &crate::db::NoPgDump, why: Vec<String>) -> Problem {
    let major = missing.major;
    let mut problem = Problem::new("pg-dump-missing", format!("Writing this schema as SQL needs pg_dump {major} or newer")).lines(why);
    for (path, version) in &missing.older {
        problem = problem.note(format!("The pg_dump at {path} is version {version}, which can't read Postgres {major}"));
    }
    problem = problem.note("Reylo looks for it in PGBIN, then where Postgres installs it, then on PATH.");
    match install_pg_dump(major) {
        Some(command) => problem.fix(format!("Install pg_dump {major}, then run it again"), &[command.as_str()]),
        None => problem.note(format!("Run it again where pg_dump {major} or newer is installed.")),
    }
}

/// The command that installs pg_dump `major` on this system, where there is one to name: none in a
/// container with no package manager, as Reylo's own image is.
#[cfg(feature = "cli")]
fn install_pg_dump(major: i64) -> Option<String> {
    let exists = |p: &str| Path::new(p).exists();
    if cfg!(target_os = "macos") {
        // The client alone: pg_dump reads every server older than itself.
        Some("brew install libpq".into())
    } else if cfg!(windows) {
        Some("scoop install postgresql".into())
    } else if !exists("/bin/sh") {
        None
    } else if exists("/etc/alpine-release") {
        Some(format!("apk add postgresql{major}-client"))
    } else if exists("/etc/redhat-release") {
        Some(format!("sudo dnf install postgresql{major}"))
    } else {
        Some(format!("sudo apt-get install postgresql-client-{major}"))
    }
}

/// The schema of a MySQL or SQLite database as SQL that makes it: Reylo's own, where a shadow shows
/// it makes exactly what the database's own dump makes, else the dump, and `why` says why. Where no
/// shadow can be made, as [`schema_as_sql_on`].
#[cfg(feature = "cli")]
async fn checked_against_dump(db: &mut Db, opts: &Options, schemas: &[String], shadowed: bool, why: &mut Vec<String>) -> Result<Result<String, Problem>, Error> {
    let dumped = dump_from(db, opts, schemas).await?;
    // Where no shadow database can be made (a role without CREATEDB, and no shadow_url), the dump.
    let mut server = match shadow_server(db, opts).await {
        Ok(server) => server,
        Err(e) if shadowed => return Err(e),
        Err(e) if opts.shadow_url.is_some() => return Ok(Err(shadow_url_problem(&e))),
        Err(e) => {
            *why = no_shadow(&e);
            return Ok(Ok(dumped));
        }
    };
    let mut shadows = Shadows::new(&mut server, &opts.url, &opts.root, &opts.cache, &[dumped.as_str()]).await;
    let styled = async {
        // The plan from an empty database to this one, as `new` writes a migration.
        let empty = shadows.lane();
        match shadows.open(empty).await {
            Ok(Ok(())) => {}
            Err(e) => return Ok(Err(NotOwn::NoShadow(e))),
            Ok(Err(_)) => return Ok(Err(NotOwn::NoShadow(Error("an empty shadow database can't be made".into())))),
        }
        shadows.taint(empty);
        let Some(planned) = plan_text(shadows.db(empty), db, None).await else { return Ok(Err(NotOwn::Short(vec!["  Reylo can't write it as SQL of its own".to_string()]))) };
        // A blank line between statements, for a file a person reads whole; one with comments of
        // the plan's own stays as written.
        let sql = if planned.lines().any(|l| l.trim_start().starts_with("--")) {
            planned
        } else {
            crate::sql::split_statements(&planned, db.dialect()).iter().map(|st| format!("{};", st.text.trim())).collect::<Vec<_>>().join("\n\n")
        };
        let (from_dump, from_plan) = (shadows.lane(), shadows.lane());
        shadows.push(from_dump, [Step::script("the dump", &dumped)]);
        shadows.push(from_plan, [Step::script("schema.sql", &sql)]);
        Ok::<_, Error>(match (shadows.lines(from_dump).await?, shadows.lines(from_plan).await?) {
            (Ok(a), Ok(b)) if a == b => Ok(sql),
            (Err((_, e)), _) => Err(NotOwn::Short(vec![format!("  The dump fails on a shadow database: {e}")])),
            (_, Err((_, e))) => Err(NotOwn::Short(vec![format!("  Reylo's own SQL for it fails on a shadow database: {e}")])),
            _ => Err(NotOwn::Short(vec!["  Reylo's own SQL doesn't make what the dump does".to_string()])),
        })
    }
    .await;
    shadows.finish().await;
    server.close().await;
    // Whatever keeps the plan from being shown to make the dump's schema leaves the dump, but a
    // shadow that can't be made or fails on its server.
    *why = match styled {
        Ok(Ok(sql)) => return Ok(Ok(format!("{}\n", sql.trim_end()))),
        Ok(Err(NotOwn::NoShadow(e))) | Err(e) if shadowed => return Err(e),
        Ok(Err(NotOwn::NoShadow(e))) | Err(e) if opts.shadow_url.is_some() => return Ok(Err(shadow_url_problem(&e))),
        Ok(Err(NotOwn::NoShadow(e))) => no_shadow(&e),
        Err(e) => vec![format!("  Reylo's own SQL can't be checked on a shadow database: {e}")],
        Ok(Err(NotOwn::Short(lines))) => lines,
    };
    Ok(Ok(dumped))
}

/// A statement of pg_dump's without the column options a platform stamps on every column it makes,
/// which no other Postgres takes and the platform sets again itself: Neon's `pg_attver`. None where
/// the statement set nothing else.
fn without_platform_column_options(statement: &str) -> Option<String> {
    static OPTIONS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)^(ALTER TABLE ONLY .+ ALTER COLUMN .+ SET \()(.*)\)$").expect("a valid pattern"));
    let Some(c) = OPTIONS.captures(statement) else { return Some(statement.to_string()) };
    let kept: Vec<&str> = c[2].split(", ").filter(|o| !o.starts_with("pg_attver=")).collect();
    match kept.len() {
        0 => None,
        _ => Some(format!("{}{})", &c[1], kept.join(", "))),
    }
}

/// pg_dump's text without the entries `owners` own: each entry is what follows the comment pg_dump
/// heads it with, `-- Name: …; Type: …; Schema: …; Owner: <role>`, up to the next one, so an object
/// goes with its owner, privileges and comment, and a default privilege with the role it is for. A
/// relation's entries in one of the `project` schemas stay, whoever owns it: the relation is the
/// project's ([`crate::catalog::project_relation`]).
fn without_entries_of(text: &str, owners: &[String], project: &[String]) -> String {
    static HEADER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^--\n-- (?:Data for )?Name: ([^\n]*); Type: ([^\n]*); Schema: ([^\n]*); Owner: ([^;\n]*)(?:; Tablespace: [^\n]*)?\n--\n").expect("a valid pattern"));
    // pg_dump's types for a relation and for what it holds, and the names it gives the privileges,
    // comments and labels on them.
    const RELATION: [&str; 18] = ["TABLE", "VIEW", "MATERIALIZED VIEW", "SEQUENCE", "SEQUENCE OWNED BY", "FOREIGN TABLE", "INDEX", "INDEX ATTACH", "TABLE ATTACH", "CONSTRAINT", "FK CONSTRAINT", "CHECK CONSTRAINT", "DEFAULT", "TRIGGER", "POLICY", "ROW SECURITY", "RULE", "STATISTICS"];
    const ON_RELATION: [&str; 12] = ["TABLE ", "VIEW ", "MATERIALIZED VIEW ", "SEQUENCE ", "FOREIGN TABLE ", "COLUMN ", "INDEX ", "CONSTRAINT ", "TRIGGER ", "POLICY ", "RULE ", "STATISTICS "];
    let relation = |name: &str, kind: &str| RELATION.contains(&kind) || (matches!(kind, "ACL" | "COMMENT" | "SECURITY LABEL") && ON_RELATION.iter().any(|p| name.starts_with(p)));
    let heads: Vec<(usize, bool)> = HEADER
        .captures_iter(text)
        .map(|c| (c.get(0).map_or(0, |m| m.start()), owners.iter().any(|o| o == &c[4]) && !(project.iter().any(|s| s == &c[3]) && relation(&c[1], &c[2]))))
        .collect();
    let Some(&(first, _)) = heads.first() else { return text.to_string() };
    let mut out = text[..first].to_string();
    for (k, &(start, theirs)) in heads.iter().enumerate() {
        let end = heads.get(k + 1).map_or(text.len(), |h| h.0);
        if !theirs {
            out.push_str(&text[start..end]);
        }
    }
    out
}

async fn dump_from(db: &mut Db, opts: &Options, schemas: &[String]) -> Result<String, Error> {
    match db.dialect() {
        Dialect::Postgres => {
            let version: i64 = db.rows("select current_setting('server_version_num') as v").await?.first().and_then(|r| r.first()?.parse().ok()).unwrap_or(0);
            let mut args: Vec<String> = ["--schema-only", "--no-publications", "--no-subscriptions", "--no-security-labels", "--no-tablespaces"].map(String::from).to_vec();
            // Owners are kept where they aren't the role the file will run as, which owns what it makes.
            let runner = db.rows("select quote_ident(current_user) as u").await?.first().map(|r| r.text("u").to_string()).unwrap_or_default();
            // The history tables, and the sequences they own, each named as it is written: pg_dump
            // folds what isn't quoted.
            let history = db
                .rows(&format!(
                    "select format('\"%s\".\"%s\"', replace(n.nspname, '\"', '\"\"'), replace(c.relname, '\"', '\"\"')) as p
                    from pg_class c join pg_namespace n on n.oid = c.relnamespace where c.relkind in ('r', 'p', 'S') and {} and not {}",
                    crate::catalog::PG_USER_NS,
                    crate::catalog::pg_not_history("c")
                ))
                .await?;
            for r in &history {
                args.push(format!("--exclude-table={}", r.text("p")));
            }
            // Every schema of the project's, unless --schema names some.
            let mut named_schemas: Vec<String> = if schemas.is_empty() { crate::catalog::user_schemas(db).await? } else { schemas.to_vec() };
            if named_schemas.is_empty() {
                named_schemas.push("public".into());
            }
            for s in &named_schemas {
                // A name, matched as it is written: pg_dump reads a pattern, folding what isn't quoted.
                args.push("--schema".into());
                args.push(format!("\"{}\"", s.replace('"', "\"\"")));
            }
            // What the platform's roles own in a dumped schema is the platform's, which every
            // comparison leaves out (Tiger Cloud's functions in `public`, its superuser's default
            // privileges), so no file makes it; but for a relation in a schema of the project's,
            // which is the project's whoever owns it.
            let owners = crate::catalog::platform_owners(db).await?;
            let project = crate::catalog::user_schemas(db).await?;
            let text = without_entries_of(&crate::db::pg_dump(&opts.url, version / 10000, &args)?, &owners, &project);
            // psql's meta-commands, which pg_dump writes on lines of their own around the file.
            static META: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\\(un)?restrict\b.*$").expect("a valid pattern"));
            let text = META.replace_all(&text, "");
            // What pg_dump writes for a session of its own: its settings (an empty search_path would
            // outlive the file in a shadow's session), and comments, which fall between statements.
            // Read as statements, so a body or a string that holds such a line keeps it.
            let own = format!(" OWNER TO {runner}");
            // A platform's default privileges are its own, which only its roles may set, and which
            // every shadow is given as they are.
            let platform_default = |s: &str| owners.iter().any(|r| s.starts_with(&format!("ALTER DEFAULT PRIVILEGES FOR ROLE {r} ")) || s.starts_with(&format!("ALTER DEFAULT PRIVILEGES FOR ROLE {} ", crate::db::ident(r))));
            let mut kept: Vec<String> = Vec::new();
            // The extensions the dumped schemas use, which pg_dump writes only for the schema it
            // dumps them in: the platform's own aside, made where they are, if they aren't already.
            // `plpgsql` is every database's, and one the platform's owner has in its own schema or in
            // `pg_catalog` is the platform's (Azure's `pgaadauth`); one the project put in `pg_catalog`
            // is its own, made there, and no `create schema` names `pg_catalog`; one in a schema of the
            // project's is its own, whoever owns it.
            for r in db
                .rows(&format!(
                    "select quote_ident(x.extname) as name, quote_ident(n.nspname) as schema, n.nspname as raw from pg_extension x join pg_namespace n on n.oid = x.extnamespace where x.extname <> 'plpgsql' and {}",
                    crate::catalog::project_extension("x", "n")
                ))
                .await?
            {
                // Its schema first, dumped or not, once for the extensions that share it: pg_dump's own
                // `create schema`, later, then finds it there.
                let schema = format!("CREATE SCHEMA IF NOT EXISTS {}", r.text("schema"));
                if !matches!(r.text("raw"), "public" | "pg_catalog") && !kept.contains(&schema) {
                    kept.push(schema);
                    // With its owner, where it isn't the runner, and the privileges on it, which are
                    // compared: pg_dump writes them only for a schema it dumps.
                    if !named_schemas.iter().any(|s| s == r.text("raw")) {
                        let name = crate::db::literal(r.text("raw"));
                        for g in db
                            .rows(&format!(
                                "select 0 as k, format('ALTER SCHEMA %I OWNER TO %I', n.nspname, pg_get_userbyid(n.nspowner)) as s from pg_namespace n
                                  where n.nspname = {name} and n.nspowner <> (select oid from pg_roles where rolname = current_user) and {}
                                union all
                                select 1, format('GRANT %s ON SCHEMA %I TO %s%s', x.privilege_type, n.nspname, case when x.grantee = 0 then 'PUBLIC' else quote_ident(pg_get_userbyid(x.grantee)) end, case when x.is_grantable then ' WITH GRANT OPTION' else '' end)
                                  from pg_namespace n, aclexplode(n.nspacl) x where n.nspname = {name} and x.grantee <> n.nspowner and {}
                                order by k, s",
                                crate::catalog::not_platform_schema("n"),
                                crate::catalog::not_platform_schema("n")
                            ))
                            .await?
                        {
                            kept.push(g.text("s").to_string());
                        }
                    }
                }
                kept.push(format!("CREATE EXTENSION IF NOT EXISTS {} WITH SCHEMA {}", r.text("name"), r.text("schema")));
            }
            // Foreign data wrappers and servers, which are in no schema, so pg_dump writes them only for a
            // whole database: what the dumped schemas' foreign tables are made on, an extension's and the
            // platform's own aside, each with its grants, an owner that isn't the runner, and its comment.
            // User mappings stay out, since their options hold passwords. They are made after the
            // functions a wrapper names as its handler or validator, and before the foreign tables made
            // on a server, both of which pg_dump writes: where its first foreign table is.
            let me = "(select oid from pg_roles where rolname = current_user)";
            let options = |column: &str| format!("coalesce(' OPTIONS (' || (select string_agg(format('%I %L', split_part(x, '=', 1), substr(x, strpos(x, '=') + 1)), ', ') from unnest({column}) x) || ')', '')");
            // Granted before the owner is set, which then grants them as the owner.
            let grants = |acl: &str, owner: &str, on: &str, name: &str| {
                format!(
                    "coalesce((select array_agg(format('GRANT %s ON {on} %I TO %s%s', x.privilege_type, {name}, case when x.grantee = 0 then 'PUBLIC' else quote_ident(pg_get_userbyid(x.grantee)) end,
                        case when x.is_grantable then ' WITH GRANT OPTION' else '' end) order by x.grantee, x.privilege_type) from aclexplode({acl}) x where x.grantee <> {owner}), '{{}}')"
                )
            };
            let mut foreign: Vec<String> = Vec::new();
            for r in db
                .rows(&format!(
                    "select o.k, o.id, u.s from (
                      select 0 as k, w.oid as id, array[format('CREATE FOREIGN DATA WRAPPER %I', w.fdwname) || coalesce(' HANDLER ' || nullif(w.fdwhandler::oid, 0)::regproc::text, '')
                        || coalesce(' VALIDATOR ' || nullif(w.fdwvalidator::oid, 0)::regproc::text, '') || {}] || {}
                        || array[case when w.fdwowner <> {me} then format('ALTER FOREIGN DATA WRAPPER %I OWNER TO %I', w.fdwname, pg_get_userbyid(w.fdwowner)) end,
                          case when obj_description(w.oid, 'pg_foreign_data_wrapper') is not null then format('COMMENT ON FOREIGN DATA WRAPPER %I IS %L', w.fdwname, obj_description(w.oid, 'pg_foreign_data_wrapper')) end] as ss
                      from pg_foreign_data_wrapper w where {} and {}
                    union all
                      select 1, sv.oid, array[format('CREATE SERVER %I', sv.srvname) || coalesce(' TYPE ' || quote_literal(sv.srvtype), '') || coalesce(' VERSION ' || quote_literal(sv.srvversion), '')
                        || format(' FOREIGN DATA WRAPPER %I', w.fdwname) || {}] || {}
                        || array[case when sv.srvowner <> {me} then format('ALTER SERVER %I OWNER TO %I', sv.srvname, pg_get_userbyid(sv.srvowner)) end,
                          case when obj_description(sv.oid, 'pg_foreign_server') is not null then format('COMMENT ON SERVER %I IS %L', sv.srvname, obj_description(sv.oid, 'pg_foreign_server')) end]
                      from pg_foreign_server sv join pg_foreign_data_wrapper w on w.oid = sv.srvfdw where {} and {}
                    ) o, unnest(o.ss) with ordinality u(s, n) where u.s is not null
                    order by o.k, o.id, u.n",
                    options("w.fdwoptions"),
                    grants("w.fdwacl", "w.fdwowner", "FOREIGN DATA WRAPPER", "w.fdwname"),
                    crate::catalog::not_extension("pg_foreign_data_wrapper", "w"),
                    crate::catalog::not_platform("w.fdwowner"),
                    options("sv.srvoptions"),
                    grants("sv.srvacl", "sv.srvowner", "FOREIGN SERVER", "sv.srvname"),
                    crate::catalog::not_extension("pg_foreign_server", "sv"),
                    crate::catalog::not_platform("sv.srvowner")
                ))
                .await?
            {
                foreign.push(r.text("s").to_string());
            }
            // What every new database has already: its public schema, as initdb makes it.
            const AS_MADE: [&str; 4] = ["CREATE SCHEMA public", "CREATE SCHEMA IF NOT EXISTS public", "ALTER SCHEMA public OWNER TO pg_database_owner", "COMMENT ON SCHEMA public IS 'standard public schema'"];
            for statement in crate::sql::split_statements(&text, Dialect::Postgres) {
                let Some(s) = without_platform_column_options(&statement.text) else { continue };
                let s = s.as_str();
                if s.starts_with("SET ") || s.starts_with("SELECT pg_catalog.set_config") || (s.starts_with("ALTER ") && s.ends_with(&own)) || platform_default(s) || s.starts_with("CREATE EXTENSION ") || AS_MADE.contains(&s) {
                    continue;
                }
                if s.starts_with("CREATE FOREIGN TABLE ") {
                    kept.append(&mut foreign);
                }
                let s = match s.strip_prefix("CREATE SCHEMA ") {
                    Some(rest) if !rest.starts_with("IF NOT EXISTS ") => format!("CREATE SCHEMA IF NOT EXISTS {rest}"),
                    _ => s.to_string(),
                };
                // A schema made already, for the extensions in it.
                if !(s.starts_with("CREATE SCHEMA IF NOT EXISTS ") && kept.contains(&s)) {
                    kept.push(s);
                }
            }
            kept.append(&mut foreign);
            // Event triggers and publications, which are in no schema, so pg_dump writes neither for a
            // schema (and publications not at all here): the project's, which are compared, after the
            // functions and tables they name, an extension's and the platform's own aside.
            let (columns, schemas) = if version >= 150000 {
                (
                    "coalesce(' (' || (select string_agg(format('%I', a.attname), ', ' order by a.attnum) from pg_attribute a where a.attrelid = c.oid and a.attnum = any(pr.prattrs::int2[])) || ')', '') || coalesce(' WHERE (' || pg_get_expr(pr.prqual, c.oid) || ')', '')",
                    "(select string_agg(format('TABLES IN SCHEMA %I', n.nspname), ', ' order by n.nspname) from pg_publication_namespace pn join pg_namespace n on n.oid = pn.pnnspid where pn.pnpubid = p.oid)",
                )
            } else {
                ("''", "null")
            };
            // Whether a publication publishes its tables' stored generated columns is Postgres 18's.
            let generated = if version >= 180000 { "case when p.pubgencols = 's' then ', publish_generated_columns = stored' else '' end" } else { "''" };
            for r in db
                .rows(&format!(
                    "select 0 as k, et.evtname::text as name, format('CREATE EVENT TRIGGER %I ON %s', et.evtname, et.evtevent)
                        || coalesce(' WHEN TAG IN (' || (select string_agg(quote_literal(t), ', ' order by t) from unnest(et.evttags) t) || ')', '')
                        || format(' EXECUTE FUNCTION %I.%I()', fn.nspname, f.proname) as s,
                      case et.evtenabled when 'D' then format('ALTER EVENT TRIGGER %I DISABLE', et.evtname) when 'R' then format('ALTER EVENT TRIGGER %I ENABLE REPLICA', et.evtname)
                        when 'A' then format('ALTER EVENT TRIGGER %I ENABLE ALWAYS', et.evtname) end as enabled,
                      case when et.evtowner <> {me} then format('ALTER EVENT TRIGGER %I OWNER TO %I', et.evtname, pg_get_userbyid(et.evtowner)) end as owner,
                      case when obj_description(et.oid, 'pg_event_trigger') is not null then format('COMMENT ON EVENT TRIGGER %I IS %L', et.evtname, obj_description(et.oid, 'pg_event_trigger')) end as comment
                    from pg_event_trigger et join pg_proc f on f.oid = et.evtfoid join pg_namespace fn on fn.oid = f.pronamespace where {} and {}
                    union all
                    select 1, p.pubname::text, format('CREATE PUBLICATION %I', p.pubname)
                        || coalesce(' FOR ' || case when p.puballtables then 'ALL TABLES' else nullif(concat_ws(', ',
                          (select string_agg(format('TABLE %I.%I', n.nspname, c.relname) || {columns}, ', ' order by n.nspname, c.relname)
                            from pg_publication_rel pr join pg_class c on c.oid = pr.prrelid join pg_namespace n on n.oid = c.relnamespace where pr.prpubid = p.oid),
                          {schemas}), '') end, '')
                        || format(' WITH (publish = %L%s%s)', concat_ws(', ', case when p.pubinsert then 'insert' end, case when p.pubupdate then 'update' end,
                          case when p.pubdelete then 'delete' end, case when p.pubtruncate then 'truncate' end), case when p.pubviaroot then ', publish_via_partition_root = true' else '' end, {generated}),
                      null,
                      case when p.pubowner <> {me} then format('ALTER PUBLICATION %I OWNER TO %I', p.pubname, pg_get_userbyid(p.pubowner)) end,
                      case when obj_description(p.oid, 'pg_publication') is not null then format('COMMENT ON PUBLICATION %I IS %L', p.pubname, obj_description(p.oid, 'pg_publication')) end
                    from pg_publication p where {}
                    order by k, name",
                    crate::catalog::not_extension("pg_event_trigger", "et"),
                    crate::catalog::not_platform("et.evtowner"),
                    crate::catalog::not_platform("p.pubowner")
                ))
                .await?
            {
                kept.extend(["s", "enabled", "owner", "comment"].iter().filter_map(|k| r.get(k).map(String::from)));
            }
            let mut joined = kept.iter().map(|s| format!("{s};")).collect::<Vec<_>>().join("\n\n");
            // What the project made on a platform's tables (Supabase's `auth.users`, `storage.objects`),
            // which no schema dumped holds: its triggers, whose function is no platform's, and its
            // policies, which pg_dump writes only with their table.
            let list = named_schemas.iter().map(|s| crate::db::literal(s)).collect::<Vec<_>>().join(", ");
            let extra = db
                .rows(&format!(
                    "select pg_get_triggerdef(t.oid) || ';' as s from pg_trigger t join pg_class c on c.oid = t.tgrelid join pg_namespace n on n.oid = c.relnamespace join pg_proc f on f.oid = t.tgfoid
                    where not t.tgisinternal and t.tgparentid = 0 and n.nspname not in ({list}) and not {} and {}
                    union all
                    select format('CREATE POLICY %I ON %I.%I AS %s FOR %s TO %s%s%s;', p.polname, n.nspname, c.relname, case when p.polpermissive then 'PERMISSIVE' else 'RESTRICTIVE' end,
                      case p.polcmd when 'r' then 'SELECT' when 'a' then 'INSERT' when 'w' then 'UPDATE' when 'd' then 'DELETE' else 'ALL' end,
                      (select string_agg(case when r = 0 then 'public' else quote_ident(pg_get_userbyid(r)) end, ', ' order by 1) from unnest(p.polroles) r),
                      coalesce(' USING (' || pg_get_expr(p.polqual, p.polrelid) || ')', ''), coalesce(' WITH CHECK (' || pg_get_expr(p.polwithcheck, p.polrelid) || ')', ''))
                    from pg_policy p join pg_class c on c.oid = p.polrelid join pg_namespace n on n.oid = c.relnamespace
                    where n.nspname not in ({list}) and not {}",
                    crate::catalog::project_relation("c", "n"),
                    crate::catalog::not_platform("f.proowner"),
                    crate::catalog::project_relation("c", "n")
                ))
                .await?;
            if !extra.is_empty() {
                joined.push_str("\n\n");
                joined.push_str(&extra.iter().map(|r| r.text("s").to_string()).collect::<Vec<_>>().join("\n\n"));
            }
            let sql = joined.trim().to_string();
            // pg_dump makes functions before the tables a SQL function's body may read, which Postgres
            // checks as the function is made, so the check is off while the file runs, as in pg_dump's
            // own output.
            static SQL_BODY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*LANGUAGE sql\b").expect("a valid pattern"));
            Ok(if SQL_BODY.is_match(&sql) { format!("set check_function_bodies = off;\n\n{sql}\n\nreset check_function_bodies;\n") } else { format!("{sql}\n") })
        }
        Dialect::Sqlite => {
            let rows = db
                .rows(&format!(
                    "select sql from sqlite_schema where sql is not null and name not like 'sqlite_%' and {}
                      and name not in (select name from pragma_table_list where type = 'shadow') and name {turso} and tbl_name {turso} and {}
                    order by case type when 'table' then 0 when 'index' then 1 when 'view' then 2 else 3 end, rowid",
                    crate::catalog::not_history("tbl_name"),
                    crate::catalog::NOT_LIBSQL_VECTOR_OWN,
                    turso = crate::catalog::NOT_TURSO_OWN
                ))
                .await?;
            Ok(format!("{}\n", rows.iter().map(|r| format!("{};", r.text("sql"))).collect::<Vec<_>>().join("\n\n")))
        }
        Dialect::Mysql => {
            static AUTO: LazyLock<Regex> = LazyLock::new(|| Regex::new(r" AUTO_INCREMENT=\d+").expect("a valid pattern"));
            static DEFINER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r" DEFINER=\S+").expect("a valid pattern"));
            let mut out = vec!["set foreign_key_checks = 0;".to_string()];
            let tables = db.rows(&format!("select table_name as n, table_type as t from information_schema.tables where table_schema = database() and {} order by table_type, table_name", crate::catalog::not_history("table_name"))).await?;
            for t in tables.iter().filter(|t| t.text("t") == "BASE TABLE") {
                let row = db.rows(&format!("show create table `{}`", t.text("n"))).await?;
                out.push(format!("{};", AUTO.replace(row.first().map(|r| r.text("Create Table")).unwrap_or_default(), "")));
            }
            // Routines before the views, which may call a function: MySQL resolves what a routine's body
            // names only when it runs, so a routine may come before the views and tables it reads.
            for r in db.rows("select routine_name as n, routine_type as t from information_schema.routines where routine_schema = database()").await? {
                let kind = r.text("t");
                let row = db.rows(&format!("show create {} `{}`", kind.to_lowercase(), r.text("n"))).await?;
                let column = if kind == "PROCEDURE" { "Create Procedure" } else { "Create Function" };
                out.push(format!("{};", DEFINER.replace(row.first().map(|x| x.text(column)).unwrap_or_default(), "")));
            }
            // Each view after the views it reads.
            let mut views: Vec<String> = tables.iter().filter(|t| t.text("t") == "VIEW").map(|t| t.text("n").to_string()).collect();
            let reads: Vec<(String, String)> = db
                .rows("select view_name as v, table_name as t from information_schema.view_table_usage where view_schema = database() and table_schema = database()")
                .await
                .map(|rows| rows.iter().map(|r| (r.text("v").to_string(), r.text("t").to_string())).collect())
                .unwrap_or_default();
            let mut ordered: Vec<String> = Vec::new();
            while !views.is_empty() {
                let ready = views.iter().position(|v| !reads.iter().any(|(of, t)| of == v && t != v && views.contains(t))).unwrap_or(0);
                ordered.push(views.remove(ready));
            }
            for name in &ordered {
                let row = db.rows(&format!("show create view `{name}`")).await?;
                out.push(format!("{};", DEFINER.replace(row.first().map(|r| r.text("Create View")).unwrap_or_default(), "")));
            }
            for r in db.rows(&format!("select trigger_name as n from information_schema.triggers where trigger_schema = database() and {}", crate::catalog::not_history("event_object_table"))).await? {
                let row = db.rows(&format!("show create trigger `{}`", r.text("n"))).await?;
                out.push(format!("{};", DEFINER.replace(row.first().map(|x| x.text("SQL Original Statement")).unwrap_or_default(), "")));
            }
            // Recurring events, as the catalog reads them: a one-time event is gone once it fires.
            for r in db.rows("select event_name as n from information_schema.events where event_schema = database() and event_type <> 'ONE TIME'").await? {
                let row = db.rows(&format!("show create event `{}`", r.text("n"))).await?;
                out.push(format!("{};", DEFINER.replace(row.first().map(|x| x.text("Create Event")).unwrap_or_default(), "")));
            }
            // The privileges on what it holds, which a push compares as it does the rest.
            for g in crate::catalog::mysql_grants(db).await?.unwrap_or_default() {
                out.push(format!("{};", g.statement(&|name| format!("`{}`", name.replace('`', "``")), true)));
            }
            out.push("set foreign_key_checks = 1;".into());
            Ok(format!("{}\n", out.join("\n\n")))
        }
    }
}

/// `reylo migrate init`: writes `schema.sql` and `migrations/init.sql` from the database, then
/// records init.sql without running it, once a shadow shows the database already has its schema.
#[cfg(feature = "cli")]
pub async fn init(opts: &Options, schemas: &[String], log: Log<'_>) -> Result<i32, Error> {
    // The directories it may make, the deepest first: `reylo/migrations/`, and `reylo/` where it's
    // new, which the shadows' replay cache goes in too. A run that doesn't start the project takes
    // them away again, so that init runs again once it can, and a refusal's "Nothing was changed"
    // holds.
    // Nothing at the path, not even a link: `exists` follows one, and a link that leads nowhere is
    // still the user's.
    let made: Vec<PathBuf> = opts.dir.ancestors().take_while(|d| !d.as_os_str().is_empty() && std::fs::symlink_metadata(d).is_err()).map(Path::to_path_buf).collect();
    let result = init_in(opts, schemas, log).await;
    if let (false, Some(top)) = (matches!(result, Ok(0)), made.last()) {
        let _ = std::fs::remove_dir_all(top);
    }
    result
}

#[cfg(feature = "cli")]
async fn init_in(opts: &Options, schemas: &[String], log: Log<'_>) -> Result<i32, Error> {
    if let Some(code) = refuse_lock_file(opts, log) {
        return Ok(code);
    }
    let started = if opts.schema_exists() { Some(shown(&opts.schema)) } else if !opts.migrations()?.0.is_empty() { Some(shown(&opts.dir)) } else { None };
    if let Some(what) = started {
        let problem = Problem::new("project-started", format!("This project has started already ({what} {})", if what.ends_with(".sql") { "exists" } else { "holds migrations" }))
            .fix("To see this database's schema as SQL", &["reylo db introspect"]);
        return Ok(refuse(&[problem], log));
    }
    let home = opts.schema.parent().map(|p| format!("{}/", shown(p))).unwrap_or_default();
    let branch = |last: bool| if last { "└──" } else { "├──" };
    // A file of the tree, with what it is, dim.
    let entry = |prefix: &str, name: &str, what: &str| {
        let head = format!("{prefix} {name}");
        if what.is_empty() { format!("  {head}") } else { format!("  {head:<24}{}", paint("dim", what)) }
    };
    let file_name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let dir_name = format!("{}/", file_name(&opts.dir));
    // An empty database has no history to start: the first migration is `new`'s. The lock starts
    // empty, holding the checksum of the empty schema.sql the no migrations reach, so the project
    // keeps linear history from its first migration.
    let spinning = crate::terminal::spinner(format!("Introspecting {}", shown_url(&opts.url)));
    let mut db = Db::open(opts).await?;
    // Relations of the project's a superuser out of the role's reach owns are its to give first: no
    // file the role runs makes them as they are here.
    let problems = out_of_reach(&mut db, &opts.url).await;
    let lines = catalog(&mut db).await;
    let left = left_out(&mut db, schemas).await;
    let roles = named_roles(&mut db).await;
    let new = new_database(&mut db).await;
    db.close().await;
    let problems = problems?;
    let mut roles = roles?;
    let new = new?;
    if !problems.is_empty() {
        return Ok(refuse(&problems, log));
    }
    let left = left?;
    if !left.is_empty() {
        return Ok(refuse(&[schemas_left_out(schemas, &left)], log));
    }
    if !holds_sql(&lines?, &new) {
        drop(spinning);
        std::fs::create_dir_all(&opts.dir)?;
        std::fs::write(&opts.schema, "")?;
        lock::write(&opts.lock, &lock::Lock { schema: Some(lock::schema_sum("", "")), entries: vec![] })?;
        log(&format!("{} Started {home} for an empty database", paint("green", "✓")));
        log(&format!("  {home}"));
        log(&entry(branch(false), &file_name(&opts.schema), "empty: declare your tables here"));
        log(&entry(branch(false), &dir_name, ""));
        log(&entry(branch(true), &file_name(&opts.lock), ""));
        log(&then(note("There are no migrations yet"), &[("To write the first", &["reylo migrate new <name>"])]));
        return Ok(0);
    }
    let sql = schema_as_sql(opts, schemas).await?;
    // Recording init.sql replays it on a shadow database, which says so itself.
    drop(spinning);
    let sql = match sql {
        Ok(sql) => sql,
        Err(problem) => return Ok(refuse(&[problem], log)),
    };
    std::fs::create_dir_all(&opts.dir)?;
    std::fs::write(&opts.schema, &sql)?;
    let init = opts.dir.join(INIT);
    std::fs::write(&init, &sql)?;
    // The history is the lock's: init.sql is its first line, and reaches schema.sql.
    roles.retain(|role| names(&sql, role));
    // Locked as every check reads the file back: `canonical` keeps the rest after a `#` as written,
    // its last newline included.
    let entries = lock::chain([(INIT, normalized(&sql).as_str())]);
    let schema = Some(lock::schema_sum(&entries[0].sum, &sql));
    lock::write(&opts.lock, &lock::Lock { schema, entries })?;
    // init.sql is recorded as run, which the tree says. One that can't start (no shadow database can
    // be made) or is refused (init.sql doesn't replay, or doesn't make this schema) leaves no project
    // behind: its files here, and its directories in `init`.
    let mut said: Vec<String> = Vec::new();
    let recorded = history_push_as(opts, true, &mut |l: &str| said.push(l.to_string())).await;
    if !matches!(recorded, Ok(0)) {
        for file in [&opts.schema, &init, &opts.lock] {
            let _ = std::fs::remove_file(file);
        }
    }
    let code = match recorded {
        Ok(0) => {
            // What the shadows leave out of this database holds for every command after.
            for line in said.iter().filter(|l| l.contains(crate::db::LEFT_OUT)) {
                log(line);
            }
            0
        }
        Ok(code) => {
            for line in &said {
                log(line);
            }
            return Ok(code);
        }
        Err(e) => return Err(e),
    };
    log(&format!("{} Started {home} from {}", paint("green", "✓"), shown_url(&opts.url)));
    log(&format!("  {home}"));
    log(&entry(branch(false), &file_name(&opts.schema), "this database's schema"));
    log(&entry(branch(false), &dir_name, ""));
    log(&entry(&format!("│   {}", branch(true)), INIT, "the same, recorded as run here"));
    log(&entry(branch(true), &file_name(&opts.lock), "the order the migrations run in"));
    if !roles.is_empty() {
        let named = if roles.len() == 1 { format!("the role {}", roles[0]) } else { format!("the roles {}", and_list(&roles)) };
        log(&note(format!("{INIT} names {named}: make {} on each new server before it runs there, since a migration may not", if roles.len() == 1 { "it" } else { "them" })));
    }
    Ok(code)
}

/// The roles the database names, as an owner, a grantee or a policy's role, that a new server
/// lacks: none of the role that runs the migrations, a platform's, or Postgres's own. A migration
/// may make no role, so they are the server's to have first, where the SQL written names them.
#[cfg(feature = "cli")]
async fn named_roles(db: &mut Db) -> Result<Vec<String>, Error> {
    if db.dialect() != Dialect::Postgres {
        return Ok(vec![]);
    }
    let rows = db
        .rows(&format!(
            "select distinct quote_ident(r.rolname) as r from pg_shdepend d join pg_roles r on r.oid = d.refobjid
            where d.dbid = (select oid from pg_database where datname = current_database()) and d.refclassid = 'pg_authid'::regclass
              and d.deptype in ('o', 'a', 'r') and r.oid <> {} and r.rolname !~ '^pg_' and {}
            order by 1",
            crate::catalog::runner(),
            crate::catalog::not_platform("r.oid")
        ))
        .await?;
    Ok(rows.iter().map(|r| r.text("r").to_string()).collect())
}

/// `reylo db introspect`: the database's schema as SQL that makes it, as `init` writes schema.sql;
/// nothing for an empty database. Writes no file and changes no database: saving it over schema.sql,
/// whose comments and layout it doesn't keep, is a redirect. `Err` is the code to exit with, once
/// `log` has said why it can't be written.
#[cfg(feature = "cli")]
pub async fn db_introspect(opts: &Options, schemas: &[String], log: Log<'_>) -> Result<Result<String, i32>, Error> {
    there(opts)?;
    let spinning = crate::terminal::spinner(format!("Introspecting {}", shown_url(&opts.url)));
    let mut db = Db::open(opts).await?;
    let d = db.dialect();
    let mut dumped = Vec::new();
    let result = async {
        let new = new_database(&mut db).await?;
        if !holds_sql(&catalog(&mut db).await?, &new) {
            return Ok(Ok(String::new()));
        }
        let schemas = with_referenced(&mut db, schemas, &mut *log).await?;
        schema_as_sql_on(&mut db, opts, &schemas, false, &mut dumped).await
    }
    .await;
    db.close().await;
    drop(spinning);
    Ok(match result? {
        Ok(sql) => {
            // The dump is SQL of another kind than Reylo's own, so a run that saves it over
            // schema.sql is told so, on stderr, beside the SQL.
            if !dumped.is_empty() {
                log(&note(if d == Dialect::Postgres { "Printing pg_dump's output, not Reylo's own SQL" } else { "Printing the database's own dump, not Reylo's own SQL" }));
                for line in &dumped {
                    log(line);
                }
            }
            Ok(sql)
        }
        Err(problem) => Err(refuse(&[problem], log)),
    })
}

// ---------------------------------------------------------------------------------------------
// The development commands: `dev`, a database taken to its migrations; `db push`, one at its migrations
// taken to schema.sql.

/// Runs statements the diff engine wrote on the database, with `then` (statements of the history's own),
/// and keeps the result only if the database then reads as `expect`; `Ok(Err(lines))` says how it
/// differs. Where the database has transactional DDL, it is all one transaction, rolled back on a
/// difference, so nothing has changed; on MySQL, which commits each statement, it is checked after.
async fn run_to(db: &mut Db, plan: &[String], then: &[String], expect: &[String]) -> Result<Result<(), Vec<String>>, Error> {
    let d = db.dialect();
    let expect = crate::catalog::without_comments(expect);
    let expect = &expect[..];
    let statements: Vec<String> = plan.iter().filter(|s| !s.starts_with("--")).map(|s| format!("{};", s.trim_end().trim_end_matches(';'))).chain(then.iter().cloned()).collect();
    let differs = |now: &[String]| describe_diff("this database", now, "the target", expect);
    if d == Dialect::Mysql || statements.iter().any(|s| runs_alone(s, d)) {
        for s in &statements {
            db.script(s).await?;
        }
        let now = crate::catalog::without_comments(&crate::catalog::without_extra_children(&catalog(db).await?, expect));
        return Ok(if now == expect { Ok(()) } else { Err(differs(&now)) });
    }
    if d == Dialect::Sqlite {
        db.exec("pragma foreign_keys = off").await?;
    }
    db.exec("begin").await?;
    let result = async {
        db.script(&statements.join("\n")).await?;
        if d == Dialect::Sqlite && !db.rows("pragma foreign_key_check").await?.is_empty() {
            return Err(Error("rows would break a foreign key".into()));
        }
        crate::catalog::keep_turso_sequences(db).await?;
        let now = crate::catalog::without_comments(&crate::catalog::without_extra_children(&catalog(db).await?, expect));
        if now != expect {
            return Ok(Err(differs(&now)));
        }
        db.exec("commit").await?;
        Ok(Ok(()))
    }
    .await;
    if !matches!(result, Ok(Ok(()))) {
        let _ = db.exec("rollback").await;
    }
    if d == Dialect::Sqlite {
        db.exec("pragma foreign_keys = on").await?;
    }
    result
}

/// What a push that failed after committing part of its plan says: an enum's labels, which what
/// follows them uses, are committed first.
const COMMITTED: &str = "The plan commits partway (an enum's new labels, before what uses them), so its first part is in place.";

/// Why `take_to` left the database as it was.
enum Unreached {
    /// The plan ran, but the database didn't then read as the target: its schema then, and the
    /// target's, which `differs` describes.
    Differs(Vec<String>, Vec<String>),
    /// What was stated refused the plan before it ran.
    Refused(Vec<Problem>),
}

/// Takes the database to `target`: a table that goes while one of its columns comes renamed first,
/// keeping its rows, then the diff engine's plan from there, then `then` (statements of the
/// history's own). Kept only if the database then reads as `expect`, all in one transaction where
/// the database has transactional DDL, so nothing has changed otherwise; on MySQL, which commits
/// each statement, checked after. Gives the renames and the plan it ran. With `asking`, a plan that
/// drops what wasn't stated is refused before any of it runs (`settle`).
#[cfg(feature = "cli")]
async fn take_to(db: &mut Db, target: &Target, then: &[String], expect: &[String], asking: Option<(&Push, &Asked)>) -> Result<Result<(Vec<String>, crate::diff::Plan), Unreached>, Error> {
    let d = db.dialect();
    let expect = crate::catalog::without_comments(expect);
    let expect = &expect[..];
    let transactional = d != Dialect::Mysql;
    if d == Dialect::Sqlite {
        db.exec("pragma foreign_keys = off").await?;
    }
    if transactional {
        db.exec("begin").await?;
    }
    let result = async {
        let (renames, plan) = match plan_asked(db, target, true, asking).await? {
            Ok(Settled { renames, plan, .. }) => (renames, plan),
            Err(problems) => return Ok(Err(Unreached::Refused(problems))),
        };
        // Where the plan commits partway, the renames planning ran on the database run again after
        // the commit, as a migration writes them: an enum's new labels commit alone, and a push that
        // fails past them leaves every table under its name.
        let mut statements: Vec<String> = plan.runnable().cloned().collect();
        if let Some(last) = statements.iter().rposition(|s| s == crate::diff::COMMIT).filter(|_| transactional) {
            let ran: Vec<String> = renames.iter().filter(|s| !s.starts_with("--")).cloned().collect();
            if !ran.is_empty() {
                db.exec("rollback").await?;
                db.exec("begin").await?;
                statements.splice(last + 1..last + 1, ran);
            }
        }
        let mut committed = false;
        for s in statements.iter().chain(then) {
            if let Err(e) = db.script(&format!("{};", s.trim_end().trim_end_matches(';'))).await {
                return Err(if committed { Error(format!("{}\n{COMMITTED}", e.0)) } else { e });
            }
            committed |= s == crate::diff::COMMIT;
        }
        if d == Dialect::Sqlite && !db.rows("pragma foreign_key_check").await?.is_empty() {
            return Err(Error("rows would break a foreign key".into()));
        }
        crate::catalog::keep_turso_sequences(db).await?;
        let now = crate::catalog::without_comments(&crate::catalog::without_extra_children(&catalog(db).await?, expect));
        if now != expect {
            return Ok(Err(Unreached::Differs(now, expect.to_vec())));
        }
        if transactional {
            db.exec("commit").await?;
        }
        Ok(Ok((renames, plan)))
    }
    .await;
    if transactional && !matches!(result, Ok(Ok(_))) {
        let _ = db.exec("rollback").await;
    }
    if d == Dialect::Sqlite {
        db.exec("pragma foreign_keys = on").await?;
    }
    result
}

/// The refusal of MySQL grants in the project's files where the account may not read the
/// privilege tables, which a plan compares them by: information_schema shows such an account its
/// own privileges alone, so a replay's grants and the database's would go unseen.
async fn unreadable_grants(db: &mut Db, opts: &Options, files: &[Migration], schema_sql: Option<&str>) -> Result<Option<Problem>, Error> {
    if db.dialect() != Dialect::Mysql {
        return Ok(None);
    }
    let schema_name = opts.schema.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "schema.sql".into());
    let Some(name) = files.iter().map(|f| (f.name.as_str(), f.sql.as_str())).chain(schema_sql.map(|s| (schema_name.as_str(), s))).find(|(_, sql)| crate::sql::grants_on_objects(sql)).map(|(n, _)| n.to_string()) else { return Ok(None) };
    if crate::catalog::mysql_grants(db).await?.is_some() {
        return Ok(None);
    }
    let me = db.rows("select current_user() as u").await?.first().map(|r| r.text("u").to_string()).unwrap_or_default();
    let (user, host) = me.rsplit_once('@').unwrap_or((me.as_str(), "%"));
    let account = format!("'{}'@'{}'", user.replace('\'', "''"), host.replace('\'', "''"));
    let grants: Vec<String> = ["tables_priv", "columns_priv", "procs_priv"].iter().map(|t| format!("grant select on mysql.{t} to {account};")).collect();
    let grants: Vec<&str> = grants.iter().map(String::as_str).collect();
    Ok(Some(
        Problem::new("grants-unreadable", format!("Can't plan the grants in {name}"))
            .note(format!("MySQL shows {account} its own privileges alone, and Reylo Migrate reads every account's from mysql.tables_priv, mysql.columns_priv and mysql.procs_priv"))
            .fix("Let it read them, as an administrator", &grants),
    ))
}

/// The plan from the database `from` to the database `to`, as `create` would write it (from
/// `declared`, the text that built `to`), styled as schema.sql is.
#[cfg(feature = "cli")]
async fn plan_between(from: &mut Db, to: &mut Db, declared: Option<&str>) -> Result<crate::diff::Plan, Error> {
    let mut target = crate::diff::read(to).await?;
    if let (Some(sql), Dialect::Postgres) = (declared, to.dialect()) {
        crate::diff::postgres::declare(to, &mut target, sql).await?;
    }
    let current = crate::diff::read(from).await?;
    let alterer = crate::diff::alterer(from).await?;
    let mut plan = crate::diff::plan(&current, &target, alterer.as_ref());
    style(from, &mut plan, &target).await?;
    Ok(plan)
}


/// The refusal of a development command where CI is set: nothing it does is wanted there, and what
/// it would write (the lock) would go uncommitted.
pub fn refused_in_ci(command: &str, log: Log<'_>) -> i32 {
    let problem = Problem::new("refused-in-ci", format!("CI is set, and {command} is for development databases")).fix("In CI, try the migrations", &["reylo migrate deploy --dry-run"]).fix("In CI, run the migrations", &["reylo migrate deploy"]);
    refuse(&[problem], log)
}

/// `reylo migrate dev`: takes a development database to its migrations, whatever its
/// history. The history is kept as far as it agrees with the migration files: where a migration it
/// applied was edited or is gone (another branch's), the database is rewound to that point by the
/// diff engine's plan from what those migrations made, losing what it drops. The migrations after it
/// then run for real, their data statements included; one with no data statements whose change the
/// database already has (`db push` made it) is recorded instead. Never in CI (`in_ci`, from the `CI`
/// variable).
#[cfg(feature = "cli")]
pub async fn dev(opts: &Options, in_ci: bool, log: Log<'_>) -> Result<i32, Error> {
    if let Some(code) = refuse_lock_file(opts, log) {
        return Ok(code);
    }
    if in_ci {
        return Ok(refused_in_ci("reylo migrate dev", log));
    }
    let (mut db, mut server, swept) = match open(opts, log).await? {
        Ok(opened) => opened,
        Err(code) => return Ok(code),
    };
    let result = dev_on(&mut db, &mut server, swept, opts, &format!("Applying to {}", shown_url(&opts.url)), log).await;
    db.close().await;
    server.close().await;
    result
}

/// The files in the migrations directory that aren't migrations, as one refusal: a `.sql` file that
/// lacks only the time is told the name it needs.
fn stray_problem(opts: &Options, files: &[Migration], strays: &[String]) -> Option<Problem> {
    let dir = shown(&opts.dir);
    let title = match strays {
        [] => return None,
        [one] => format!("A file in {dir} isn't a migration: {one}"),
        many => format!("Files in {dir} that aren't migrations: {}", many.join(", ")),
    };
    let mut problem = Problem::new("not-a-migration", title).note(format!("A file in {dir} is init.sql or <YYYYMMDDHHMMSS>_<name>.sql, the name in a-z, 0-9 and _."));
    for note in stray_renames(files, strays, time::now_seconds()) {
        problem = problem.note(note);
    }
    Some(problem)
}

/// The rename each stray `.sql` file named in a-z, 0-9 and _ needs, as a note. One whose name starts
/// with a shorter stamp (`20260320_b.sql`, as Prisma accepts) is padded to the full time in place, so
/// that it runs where its name sorts among the migrations: from the start of its period, else its
/// end, whichever keeps it between the same neighbours. Any other takes the first second from `now`
/// that no file has, which puts it last.
fn stray_renames(files: &[Migration], strays: &[String], now: i64) -> Vec<String> {
    static BARE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9_]+\.sql$").expect("a valid pattern"));
    static SHORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(\d{8,13})_([a-z0-9_]+)\.sql$").expect("a valid pattern"));
    let real = |t: &str| time::seconds(t).map(time::stamp).as_deref() == Some(t);
    // Each migration, and each stray already given a time, as (its time, its name, the name it is
    // ordered by now): init.sql runs first whatever its name.
    let mut placed: Vec<(String, String, String)> = files.iter().map(|f| (f.time.clone(), f.name.clone(), if f.name == INIT { String::new() } else { f.name.clone() })).collect();
    // Every name a file in the directory has or is told to take, in lowercase, as a filesystem that
    // ignores case reads it, so that no rename meets another file.
    let mut taken: std::collections::HashSet<String> = files.iter().map(|f| &f.name).chain(strays).map(|n| n.to_lowercase()).collect();
    let mut notes = Vec::new();
    // A name with no stamp, or digits that aren't a date, can take any time from now, so it waits
    // for those that can only take a time of their own period.
    let mut unstamped = Vec::new();
    for s in strays.iter().filter(|s| BARE.is_match(s)) {
        let Some((short, rest)) = SHORT.captures(s).filter(|m| real(format!("{:0<14}", &m[1]).as_str())).map(|m| (m[1].to_string(), m[2].to_string())) else {
            unstamped.push(s);
            continue;
        };
        // The start of the stamp's period, and its end: each digit after the prefix the largest that
        // still makes a time.
        let start = format!("{short:0<14}");
        let end = (short.len()..14).fold(short.clone(), |t, _| (0..=9).rev().map(|d| format!("{t}{d}")).find(|t| real(format!("{t:0<14}").as_str())).expect("0 makes a time"));
        let candidates = [start, end];
        let before = placed.iter().filter(|p| p.2.as_str() < s.as_str()).count();
        let keeps = |t: &String| {
            let name = format!("{t}_{rest}.sql");
            !taken.contains(&name) && placed.iter().filter(|p| (p.0.as_str(), p.1.as_str()) < (t.as_str(), name.as_str())).count() == before
        };
        match candidates.iter().find(|t| keeps(t)) {
            Some(t) => {
                let name = format!("{t}_{rest}.sql");
                notes.push(format!("Rename {s} to {name}."));
                taken.insert(name.clone());
                placed.push((t.clone(), name, s.clone()));
            }
            None => {
                let mut by_order: Vec<&(String, String, String)> = placed.iter().collect();
                by_order.sort_by(|a, b| a.2.cmp(&b.2));
                let (prev, next) = (before.checked_sub(1).map(|i| &by_order[i].1), by_order.get(before).map(|p| &p.1));
                notes.push(match (prev, next) {
                    (Some(p), Some(n)) => format!("Rename {s} with a time that runs it where its name sorts, after {p} and before {n}."),
                    (Some(p), None) => format!("Rename {s} with a time that runs it where its name sorts, after {p}."),
                    (None, Some(n)) => format!("Rename {s} with a time that runs it where its name sorts, before {n}."),
                    (None, None) => format!("Rename {s} with a time no other file has."),
                });
            }
        }
    }
    for s in unstamped {
        let name = (now..).map(|t| format!("{}_{s}", time::stamp(t))).find(|n| !taken.contains(n)).expect("a free second");
        notes.push(format!("Rename {s} to {name}."));
        taken.insert(name);
    }
    notes
}

/// The problems no database is needed to see: files that aren't migrations, a migration that can't
/// run atomically, and a text (a migration's, or schema.sql's) a shadow can't replay.
fn file_problems(opts: &Options, files: &[Migration], strays: &[String], schema_sql: Option<&str>, d: Dialect) -> Vec<Problem> {
    let mut problems = Vec::new();
    let conflicted = |sql: &str| sql.lines().any(|l| l.starts_with("<<<<<<< ") || l.starts_with(">>>>>>> ") || l == "=======");
    for (path, sql) in files.iter().map(|f| (shown(&f.path), f.sql.as_str())).chain(schema_sql.map(|s| (shown(&opts.schema), s))) {
        if conflicted(sql) {
            problems.push(Problem::new("merge-conflict-markers", format!("Git's conflict markers are still in {path}")).note("Resolve git's conflict markers (<<<<<<<, =======, >>>>>>>) first."));
        }
    }
    problems.extend(stray_problem(opts, files, strays));
    for f in files.iter().filter(|f| f.name != INIT) {
        if let Some(why) = atomicity_problem(&f.sql, d) {
            problems.push(Problem::new("not-atomic", format!("Migration {} can't run atomically", f.name)).note(why));
        }
    }
    problems.extend(replay_problems(opts, files, schema_sql, d));
    problems
}

/// The refusals of the project's texts a shadow can't replay: each migration's, then schema.sql's.
/// Every command that loads them into a shadow checks them first, since the shadow's server keeps
/// what reaches past it: a role made there is made for the whole server.
fn replay_problems(opts: &Options, files: &[Migration], schema_sql: Option<&str>, d: Dialect) -> Vec<Problem> {
    let mut problems: Vec<Problem> = files.iter().filter_map(|f| replay_problem(&format!("Migration {}", f.name), &f.sql, d)).collect();
    problems.extend(schema_sql.and_then(|sql| replay_problem(&shown(&opts.schema), sql, d)));
    problems
}

/// The same of what a database ran, for a command that replays its history as it ran: a row whose
/// file is the same text is the file's to refuse, by [`replay_problems`]. Only the same text: two
/// that are the same statements to [`same_statements`] may still read otherwise on a server, as
/// MySQL reads a versioned comment.
fn history_replay_problems(history: &[HistoryRow], files: &[Migration], d: Dialect) -> Vec<Problem> {
    let as_file = |r: &HistoryRow| files.iter().any(|f| f.name == r.name && f.sql == r.sql);
    history.iter().filter(|r| r.applied && !as_file(r)).filter_map(|r| replay_problem(&format!("Migration {}, as this database ran it,", r.name), &r.sql, d)).collect()
}

/// The refusal of a file with a statement a shadow can't replay: the statement, why, and what to
/// do instead. `file` is how the title names it, as "Migration <name>".
fn replay_problem(file: &str, sql: &str, d: Dialect) -> Option<Problem> {
    let u = unreplayable(sql, d)?;
    let mut why = u.why;
    if let Some(first) = why.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    let problem = Problem::new("not-replayable", format!("{file} can't be replayed in a shadow database")).sql(&u.statement).note(format!("{why}."));
    Some(match u.instead {
        Some(instead) => problem.note(instead),
        None => problem,
    })
}

#[cfg(feature = "cli")]
async fn dev_on(db: &mut Db, server: &mut Server, swept: usize, opts: &Options, heading: &str, log: Log<'_>) -> Result<i32, Error> {
    swept_note(&server, swept, log);
    let _held = lock(db, opts).await?;
    let d = db.dialect();
    let (files, strays) = opts.migrations()?;
    let mut problems = file_problems(opts, &files, &strays, opts.schema_text().as_deref(), d);
    problems.extend(unreadable_grants(db, opts, &files, opts.schema_text().as_deref()).await?);
    let history = read_history(db).await?;
    problems.extend(missing_dir(opts, &history));
    // A migration edited or deleted since it ran is rewound in a shadow as it ran.
    problems.extend(history_replay_problems(&history, &files, d));
    // What the lock's rewrite says of it, printed once it is written.
    let mut notes = Vec::new();
    if problems.is_empty() {
        match dev_lock_notes(opts, &files, &history) {
            Ok(said) => notes = said,
            Err(problem) => problems.push(problem),
        }
    }
    if !problems.is_empty() {
        return Ok(refuse(&problems, log));
    }
    let files = run_order(opts, &files);
    let left = leftovers(db, &history).await?;
    drop_leftovers(db, &left, log).await?;
    if history.iter().any(|r| !r.applied) {
        // A row marked started is a pending migration's: written again when it runs, or is recorded.
        db.exec(&format!("delete from {HISTORY_TABLE} where applied_at is null")).await?;
    }
    let applied: Vec<&HistoryRow> = history.iter().filter(|r| r.applied).collect();
    // The history as far as it agrees with the files: each row's file there, where the lock puts it,
    // with the text it ran.
    let agree = if lock_free(opts) {
        // With no lock, its order is the database's own, as far as each migration still reads as it ran.
        applied.iter().take_while(|r| files.iter().any(|f| f.name == r.name && same_statements(&f.sql, &r.sql, d))).count()
    } else {
        applied.iter().zip(&files).take_while(|(r, f)| r.name == f.name && same_statements(&f.sql, &r.sql, d)).count()
    };
    let live = catalog(db).await?;
    let schema_sql = opts.schema_text();
    if left.is_empty() && !history.iter().any(|r| !r.applied) && agree == applied.len() {
        if let Some(code) = dev_without_shadows(db, opts, &files, &applied, schema_sql.as_deref(), &live, heading, log).await? {
            return Ok(code);
        }
    }
    let texts: Vec<&str> = files.iter().map(|f| f.sql.as_str()).chain(history.iter().map(|r| r.sql.as_str())).chain(schema_sql.as_deref()).collect();
    let mut shadows = Shadows::new(server, &opts.url, &opts.root, &opts.cache, &texts).await;
    let result = dev_in_shadows(&mut shadows, db, opts, &files, &applied, agree, &live, heading, log).await;
    let result = match result {
        Ok(code) => async {
            // A run that failed part of the way locks what ran before it failed, where that only adds
            // to the lock: every line it had ran here, and this branch's own migrations after them.
            // Where it failed before a line the lock already had, the lock stays as it was, since its
            // lines are what every database runs.
            let adds = code != 0
                && match opts.lock_state() {
                    lock::State::Lock(l) => {
                        let ran: Vec<String> = read_history(db).await?.into_iter().filter(|r| r.applied).map(|r| r.name).collect();
                        let had = l.names();
                        ran.len() > had.len() && ran.starts_with(&had)
                    }
                    _ => false,
                };
            if code == 0 || adds {
                relock(&mut shadows, db, opts, &files, schema_sql.as_deref(), &notes, code == 0, log).await?;
            }
            if code == 0 {
                replayed_fingerprint(&mut shadows, db, &files).await?;
            }
            Ok(code)
        }
        .await,
        other => other,
    };
    shadows.finish().await;
    result
}

/// `dev` where every pair already agrees, with no replay: schema.sql with the lock's checksum of it,
/// the history with the files (in the lock's order), and the database with the fingerprint its last
/// run stored. What is pending then runs as deploy runs it: already locked, or with no lock, none of
/// it sorting before what ran. None where a pair can't be shown to agree this way, for the shadows to
/// settle.
#[allow(clippy::too_many_arguments)]
async fn dev_without_shadows(db: &mut Db, opts: &Options, files: &[Migration], applied: &[&HistoryRow], schema_sql: Option<&str>, live: &[String], heading: &str, log: Log<'_>) -> Result<Option<i32>, Error> {
    let d = db.dialect();
    let Some(stored) = applied.last().and_then(|r| r.fingerprint.clone()) else { return Ok(None) };
    let pending: Vec<&Migration> = files.iter().filter(|f| !applied.iter().any(|r| r.name == f.name)).collect();
    if lock_free(opts) {
        let last = applied.iter().map(|r| name_order(&r.name)).max();
        let in_order = applied.windows(2).all(|w| name_order(&w[0].name) <= name_order(&w[1].name));
        if !in_order || pending.iter().any(|m| last.is_some_and(|last| name_order(&m.name) < last)) {
            return Ok(None);
        }
    } else {
        let Ok(l) = read_lock(opts) else { return Ok(None) };
        // Every line of the lock links to its file as it was locked: one edited or gone is relocked
        // by the shadows' path.
        if !links(&l, files).iter().all(|link| matches!(link, Link::Ok)) || !pending.iter().all(|m| l.entries.iter().any(|e| e.name == m.name)) || !matches!(stamp(opts, schema_sql), Stamp::Proven | Stamp::NoSchema) {
            return Ok(None);
        }
    }
    if stored != fingerprint_of(db, live).await? {
        return Ok(None);
    }
    let mut rows = Rows::new(heading.to_string(), pending.iter().map(|m| m.name.as_str()));
    if !pending.is_empty() {
        ensure_history(db).await?;
    }
    let mut runs = Runs::new();
    for m in &pending {
        let started = std::time::Instant::now();
        if let Err(Failure { error: e, .. }) = runs.run(db, m).await {
            failed(&mut rows, m, &e, d, log);
            return Ok(Some(1));
        }
        rows.row(log, &paint("green", "✓"), &m.name, &format!("applied {}", paint("dim", &format!("· {} ms", started.elapsed().as_millis()))));
    }
    runs.finish(db).await;
    if pending.is_empty() {
        log(&format!("{} Up to date with the migrations", paint("green", "✓")));
    }
    Ok(Some(0))
}

/// What `dev` leaves for `deploy` to compare with: the fingerprint of what the migrations make, not of
/// this database, which may hold more that no migration made.
#[cfg(feature = "cli")]
async fn replayed_fingerprint(shadows: &mut Shadows<'_>, db: &mut Db, files: &[Migration]) -> Result<(), Error> {
    let history = read_history(db).await?;
    let Some(last) = history.iter().rfind(|r| r.applied) else { return Ok(()) };
    let ran: Vec<&Migration> = history.iter().filter(|r| r.applied).filter_map(|r| files.iter().find(|f| f.name == r.name)).collect();
    let lane = shadows.lane();
    shadows.push(lane, ran.iter().map(|m| Step::of(m)));
    if let Ok(Ok(lines)) = shadows.lines(lane).await {
        let value = fingerprint_of(db, &lines).await?;
        ensure_history(db).await?;
        set_fingerprint(db, &last.name, &value).await?;
        store_catalog(db, &last.name, &lines).await?;
    }
    Ok(())
}

/// What `dev` says of the lock as it takes it, where the files no longer agree with it: it runs
/// them in the lock's order and writes the lock again, so a line edited or whose file is gone is
/// locked as the files now read, and so is one merged out of its order that this database ran (this
/// branch's own, after main's). One merged out of its order that this database never ran came from
/// another branch, placed after this branch's: refused, before it is locked so. A lock it can't
/// read (git's conflict markers in it) is refused too. Every database's deploy still refuses a
/// history that isn't a prefix of the lock's order, or an edit to a migration it ran.
fn dev_lock_notes(opts: &Options, files: &[Migration], history: &[HistoryRow]) -> Result<Vec<String>, Problem> {
    // A missing lock is written as the migrations apply.
    if opts.lock_state() == lock::State::Missing {
        return Ok(vec![]);
    }
    let l = read_lock(opts)?;
    let path = shown(&opts.lock);
    let ran_here = |name: &str| history.iter().any(|r| r.applied && r.name == name);
    let mut notes = Vec::new();
    for (e, link) in l.entries.iter().zip(links(&l, files)) {
        match link {
            Link::Ok => {}
            Link::Missing => notes.push(format!("Taken out of the lock, since it isn't in {}: {}", shown(&opts.dir), e.name)),
            Link::Edited => notes.push(format!("Locked again as it now reads: {}\nA database that ran the old text refuses it: if one did, put the change in a new migration instead.", e.name)),
            Link::Moved { .. } if ran_here(&e.name) => notes.push(format!("The lock was merged out of order at {}: it now lists them in the order they ran here", e.name)),
            Link::Moved { after } => {
                return Err(Problem::new("lock-out-of-order", format!("The lock was merged out of order at {}, which this database never ran", e.name))
                    .note(format!("It came from another branch, and was locked {}. In {path}, keep main's lines, in main's order, and delete this branch's.", after.map_or("first".to_string(), |a| format!("right after {a}"))))
                    .fix("Then run this branch's migrations after main's, which locks them again", &["reylo migrate dev"]))
            }
        }
    }
    Ok(notes)
}

/// Writes the lock after `dev`: what the database applied, in the order it
/// did, chained; and schema.sql's checksum where the migrations reach it.
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "cli")]
async fn relock(shadows: &mut Shadows<'_>, db: &mut Db, opts: &Options, files: &[Migration], schema_sql: Option<&str>, notes: &[String], whole: bool, log: Log<'_>) -> Result<(), Error> {
    let (next, reach) = locked(shadows, db, opts, files, schema_sql).await?;
    let schema = shown(&opts.schema);
    // What schema.sql is against the migrations, after a run that finished.
    match reach.filter(|_| whole) {
        Some(Reach::SchemaAhead) => log(&then(note("The schema is ahead of the migrations"), &[("To write the changes to a migration file", &["reylo migrate new <name>"])])),
        Some(Reach::MigrationsAhead) => log(&format!("{} The migrations make what {schema} doesn't declare: add it there, or take it out of the migration", paint("yellow", "!"))),
        // Either may be what's wanted, schema.sql edited or a migration written by hand, so the lead
        // says what the migration would hold. A dry run would stop at what db push made.
        Some(Reach::Differ) => log(&then(format!("{} The migrations and {schema} disagree", paint("yellow", "!")), &[(&format!("To write what {schema} has to a migration file"), &["reylo migrate new <name>"])])),
        Some(Reach::SchemaFails(e)) => log(&format!("{} On an empty database, {schema} fails: {}", paint("yellow", "!"), e.lines().next().unwrap_or_default())),
        Some(Reach::Same) | None => {}
    }
    // A project with no lock keeps none: reylo migrate lock writes one.
    if lock_free(opts) {
        return Ok(());
    }
    if let Some(next) = next {
        lock::write(&opts.lock, &next)?;
        let through = if whole { String::new() } else { next.entries.last().map(|e| format!(", through {}", e.name)).unwrap_or_default() };
        log(&format!("{} Wrote {}{through}", paint("green", "✓"), shown(&opts.lock)));
        for note in notes {
            for (k, line) in note.lines().enumerate() {
                log(&if k == 0 { format!("  {} {line}", paint("yellow", "!")) } else { format!("    {line}") });
            }
        }
    }
    Ok(())
}

/// What a replay of the migrations is against schema.sql's.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Reach {
    Same,
    /// schema.sql has what the migrations don't make.
    SchemaAhead,
    /// The migrations make what schema.sql doesn't declare.
    MigrationsAhead,
    /// Each has what the other doesn't.
    Differ,
    /// schema.sql doesn't run, with why.
    SchemaFails(String),
}

/// Whether `a` has what `b` doesn't, and `b` what `a` doesn't, as `difference` finds it, but for an
/// enum: one whose values in one are the other's with more among them is only the larger one's
/// change, since a value is added in place (`alter type … add value … after`) and none is taken out.
#[cfg(feature = "cli")]
fn sides(a: &[String], b: &[String]) -> (bool, bool) {
    let (mut only_a, mut only_b) = difference(a, b);
    let (mut a_more, mut b_more) = (false, false);
    only_a.retain(|line| {
        let Some((name, values)) = enum_values(line) else { return true };
        let Some((i, other)) = only_b.iter().enumerate().find_map(|(i, l)| enum_values(l).filter(|(n, _)| *n == name).map(|(_, v)| (i, v))) else { return true };
        if within(&other, &values) {
            a_more = true;
        } else if within(&values, &other) {
            b_more = true;
        } else {
            return true;
        }
        only_b.remove(i);
        false
    });
    (a_more || !only_a.is_empty(), b_more || !only_b.is_empty())
}

/// An enum's line, `enum <name> (<values as JSON strings>)`, as its name and values. The name ends at
/// the first space outside its quotes.
#[cfg(feature = "cli")]
fn enum_values(line: &str) -> Option<(&str, Vec<String>)> {
    let rest = line.strip_prefix("enum ")?;
    let mut quoted = false;
    let (k, _) = rest.char_indices().find(|&(_, c)| {
        quoted ^= c == '"';
        c == ' ' && !quoted
    })?;
    let values = rest[k..].strip_prefix(" (")?.strip_suffix(')')?;
    Some((&rest[..k], serde_json::from_str(&format!("[{values}]")).ok()?))
}

/// Whether `fewer` is `more` with some of its values left out, in the same order.
#[cfg(feature = "cli")]
fn within(fewer: &[String], more: &[String]) -> bool {
    let mut rest = more.iter();
    fewer.len() < more.len() && fewer.iter().all(|v| rest.any(|m| m == v))
}

/// The lock as the database's history makes it: the migrations it ran, in its order, with
/// schema.sql's checksum where their replay reaches schema.sql. None where the lock already is that.
#[cfg(feature = "cli")]
async fn locked(shadows: &mut Shadows<'_>, db: &mut Db, opts: &Options, files: &[Migration], schema_sql: Option<&str>) -> Result<(Option<lock::Lock>, Option<Reach>), Error> {
    let history = read_history(db).await?;
    let ran: Vec<&Migration> = history.iter().filter(|r| r.applied).filter_map(|r| files.iter().find(|f| f.name == r.name)).collect();
    let texts: Vec<(String, String)> = ran.iter().map(|f| (f.name.clone(), normalized(&f.sql))).collect();
    let entries = lock::chain(texts.iter().map(|(n, t)| (n.as_str(), t.as_str())));
    let before = match opts.lock_state() {
        lock::State::Lock(l) => Some(l),
        _ => None,
    };
    // The migrations' replay against schema.sql's, where there is one. Where they don't reach it, the
    // checksum from the last time they did stays: it no longer matches, so status refuses until a
    // migration makes what schema.sql has.
    let mut reached = None;
    let mut schema = before.as_ref().and_then(|l| l.schema.clone());
    if let Some(sql) = schema_sql {
        let ours = shadows.lane();
        shadows.push(ours, ran.iter().map(|m| Step::of(m)));
        let theirs = shadows.lane();
        shadows.push(theirs, [Step::script("schema.sql", sql)]);
        let reach = match (shadows.lines(ours).await?, shadows.lines(theirs).await?) {
            (Ok(a), Ok(b)) if a == b => Reach::Same,
            // A database that has schema.sql and not what its migrations make (db push made it) has
            // what the next migration writes, whatever changed. What it doesn't compare (a comment)
            // can match both.
            (Ok(a), Ok(b)) if {
                let live = catalog(db).await?;
                crate::catalog::same_schema(&live, &b) && !crate::catalog::same_schema(&live, &a)
            } => Reach::SchemaAhead,
            (Ok(a), Ok(b)) => match sides(&a, &b) {
                (false, true) => Reach::SchemaAhead,
                (true, false) => Reach::MigrationsAhead,
                _ => Reach::Differ,
            },
            (_, Err((_, e))) => Reach::SchemaFails(e),
            (Err(_), Ok(_)) => Reach::Differ,
        };
        if reach == Reach::Same {
            schema = Some(lock::schema_sum(&entries.last().map(|e| e.sum.clone()).unwrap_or_default(), sql));
        }
        reached = Some(reach);
    }
    let next = lock::Lock { schema, entries };
    Ok(((before.as_ref() != Some(&next)).then_some(next), reached))
}

/// The refusal of a development database that has changes nothing in the project makes, before
/// anything is changed: what it has beyond what its history ran (as it ran it) that schema.sql
/// doesn't declare (`db push` made what it does), and the next migrations, none of which changes
/// data, don't make either (made by hand, then written). Shown as the SQL from its history to it.
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "cli")]
async fn unexplained(shadows: &mut Shadows<'_>, db: &mut Db, opts: &Options, kept: &[Migration], rewound: &[&&HistoryRow], pending: &[Migration], at_kept: &[String], live: &[String]) -> Result<Option<Problem>, Error> {
    let d = db.dialect();
    let ran = shadows.lane();
    shadows.push(ran, kept.iter().map(Step::of).chain(rewound.iter().map(|r| Step::migration(&r.name, &r.sql))));
    let at_ran = match shadows.lines(ran).await? {
        Ok(lines) => lines,
        // Nothing can say what changed this database, so nothing is undone: refused, as a history that
        // doesn't replay is everywhere.
        Err((m, e)) => return Ok(Some(Problem::new("history-replay-fails", format!("Replaying {m} in a shadow database, as this database ran it, fails")).note(e).note("So nothing can tell what else changed this database, and nothing is rewound.").fix("Rebuild this database from the migrations", &["reylo db reset"]))),
    };
    let at_ran = crate::catalog::without_comments(&crate::catalog::without_missing_children(&at_ran, live));
    let here = crate::catalog::without_comments(&crate::catalog::without_extra_children(live, &at_ran));
    let (extra, missing) = difference(&here, &at_ran);
    if extra.is_empty() && missing.is_empty() {
        return Ok(None);
    }
    // What schema.sql declares, where the project has one.
    let mut explainers: Vec<usize> = Vec::new();
    let declared = match std::fs::read_to_string(&opts.schema) {
        Ok(sql) => {
            let lane = shadows.lane();
            shadows.push(lane, [Step::script("schema.sql", &sql)]);
            match shadows.lines(lane).await? {
                Ok(lines) => {
                    explainers.push(lane);
                    Some(crate::catalog::without_comments(&lines))
                }
                Err(_) => None,
            }
        }
        Err(_) => None,
    };
    // What the pending migrations make: all of them, or where one fails, those before it. One that
    // changes data too explains a change it makes: it can't be recorded as in place, so what was made
    // by hand is undone below for it to run.
    let (mut adds, mut removes) = (vec![], vec![]);
    for n in [pending.len(), pending.iter().take_while(|m| data_statements(&m.sql, d).is_empty()).count()] {
        if n == 0 {
            continue;
        }
        let lane = shadows.lane();
        shadows.push(lane, kept.iter().chain(&pending[..n]).map(Step::of));
        if let Ok(lines) = shadows.lines(lane).await? {
            (adds, removes) = difference(&crate::catalog::without_comments(&lines), at_kept);
            explainers.push(lane);
            break;
        }
    }
    let explained_extra = |l: &String| adds.contains(l) || declared.as_ref().is_some_and(|d| d.contains(l));
    let explained_missing = |l: &String| removes.contains(l) || declared.as_ref().is_some_and(|d| !d.contains(l));
    if extra.iter().all(explained_extra) && missing.iter().all(explained_missing) {
        return Ok(None);
    }
    // What the refused change is: something this database has, something it lost, or both.
    let (adds_some, removes_some) = (!extra.iter().all(explained_extra), !missing.iter().all(explained_missing));
    let schema = shown(&opts.schema);
    let title = if declared.is_some() { format!("This database was changed outside {schema} and its migrations") } else { "This database has a change no migration makes yet".to_string() };
    let mut problem = Problem::new("unexplained-change", title);
    // The change refused, as SQL, and the statements that undo it and nothing else: not what schema.sql
    // or a pending migration explains, comments, or partitions made or dropped at run time.
    let mut explaining = Vec::new();
    for lane in explainers {
        if let Ok(Ok(())) = shadows.open(lane).await {
            shadows.taint(lane);
            if let Ok(schema) = crate::diff::read(shadows.db(lane)).await {
                explaining.push(schema);
            }
        }
    }
    let mut undo = None;
    if let Ok(Ok(())) = shadows.open(ran).await {
        shadows.taint(ran);
        match scoped_change(db, shadows.db(ran), &explaining).await {
            Some((sql, statements, _)) => {
                problem = problem.sql(&sql);
                undo = Some(statements);
            }
            // Where the refused lines can't all be told apart, the whole difference is shown, and
            // no command undoes it.
            None => {
                if let Some(sql) = plan_text(shadows.db(ran), db, None).await {
                    problem = problem.sql(&sql);
                }
            }
        }
    }
    // Kept by writing it as a migration: from schema.sql, or without one, from this database, which
    // `migrate new` reads only once every migration has run here.
    if declared.is_some() {
        let keep = match (adds_some, removes_some) {
            (true, false) => format!("To keep it, add it to {schema}, then write its migration"),
            (false, true) => format!("To keep it, take it out of {schema}, then write its migration"),
            _ => format!("To keep it, make {schema} match, then write its migration"),
        };
        problem = problem.fix(keep, &["reylo migrate new <name>"]);
    } else if pending.is_empty() {
        problem = problem.fix("To keep it, write it as a migration", &["reylo migrate new <name>"]);
    }
    problem = match undo {
        Some(statements) => problem.fix("To undo it", &[&format!("reylo run {}", shell_word(&statements.join(" ")))]),
        None => problem.note("To undo it, change it back by hand."),
    };
    Ok(Some(problem.fix("To rebuild this database from the migrations, losing its rows", &["reylo db reset"])))
}

/// The refused change on `db`, against its history replayed in `history`, as SQL, with the
/// statements that undo it alone. An object that differs from the history is refused unless one of
/// `explaining` (schema.sql, the pending migrations) has it as the database does, it is a comment,
/// or it is a partition, which is data; what is refused comes back from the history, and everything
/// else stays as it is on the database. Last, what the undo loses: each table or column it drops or
/// narrows, by name. None where nothing would change.
#[cfg(feature = "cli")]
async fn scoped_change(db: &mut Db, history: &mut Db, explaining: &[crate::diff::Schema]) -> Option<(String, Vec<String>, Vec<String>)> {
    let mut current = crate::diff::read(db).await.ok()?;
    current.live = true;
    let mut target = crate::diff::read(history).await.ok()?;
    let ids: std::collections::BTreeSet<String> = current.objects.keys().chain(target.objects.keys()).cloned().collect();
    let def = |o: Option<&crate::diff::Object>| o.map(|o| o.def.clone());
    let mut kept: Vec<(String, Option<crate::diff::Object>)> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    for id in &ids {
        let (now, then) = (current.objects.get(id), target.objects.get(id));
        if def(now) == def(then) {
            continue;
        }
        let data = now.or(then).is_some_and(crate::diff::is_child);
        if data || id.starts_with("comment ") || explaining.iter().any(|e| def(e.objects.get(id)) == def(now)) {
            kept.push((id.clone(), now.cloned()));
        } else {
            refused.push(id.clone());
        }
    }
    // Where a refused object holds one that is kept (on MySQL and SQLite a table's definition holds
    // its columns), the two can't be undone apart.
    if kept.iter().any(|(id, _)| current.objects.get(id).or(target.objects.get(id)).and_then(|o| o.owner.as_ref()).is_some_and(|owner| refused.contains(owner))) {
        return None;
    }
    for (id, now) in kept {
        match now {
            Some(object) => {
                target.objects.insert(id, object);
            }
            None => {
                target.objects.remove(&id);
            }
        }
    }
    let alterer = crate::diff::alterer(db).await.ok()?;
    let mut undo = crate::diff::plan(&current, &target, alterer.as_ref());
    style(db, &mut undo, &target).await.ok()?;
    let mut change = crate::diff::plan(&target, &current, alterer.as_ref());
    style(db, &mut change, &current).await.ok()?;
    let statements: Vec<String> = undo.runnable().map(|s| format!("{};", s.trim_end().trim_end_matches(';'))).collect();
    let lost: Vec<String> = undo.losses.iter().chain(undo.narrowings.iter().map(|(column, _)| column)).map(|l| shown_id(l)).collect();
    (!statements.is_empty() && !change.statements.is_empty()).then(|| (change.sql(), statements, lost))
}

#[allow(clippy::too_many_arguments)]
#[cfg(feature = "cli")]
async fn dev_in_shadows(shadows: &mut Shadows<'_>, db: &mut Db, opts: &Options, files: &[Migration], applied: &[&HistoryRow], agree: usize, live: &[String], heading: &str, log: Log<'_>) -> Result<i32, Error> {
    let d = db.dialect();
    let fail = |m: &str, error: String, log: Log<'_>| {
        print_problems(&[Problem::new("dev-failed", m.to_string()).note(error)], log);
        Ok(1)
    };
    let kept_of = |n: usize| -> Vec<Migration> { applied[..n].iter().filter_map(|r| files.iter().find(|f| f.name == r.name).cloned()).collect() };
    let keep = agree;
    let kept = &kept_of(keep)[..];
    let k_lane = shadows.lane();
    shadows.push(k_lane, kept.iter().map(Step::of));
    let at_kept = match shadows.lines(k_lane).await? {
        Ok(lines) => lines,
        Err((m, e)) => return fail(&format!("Replaying {m} in a shadow database fails"), e, log),
    };
    // Children of a table made at run time are data: never compared, never rewound. Comments are
    // written by what runs, and aren't compared with this database either.
    let at_kept = crate::catalog::without_comments(&at_kept);
    // The extensions this database has, and the privileges on what they made, which the comparison
    // with what the migrations kept make leaves out, and which nothing undoes.
    let extensions: Vec<String> = live.iter().filter(|l| l.starts_with("extension ")).cloned().collect();
    let live = crate::catalog::without_comments(&crate::catalog::without_extra_children(live, &at_kept));
    let live = &live[..];
    let mut pending: Vec<Migration> = files.iter().filter(|f| !kept.iter().any(|k| k.name == f.name)).cloned().collect();
    let rewind_rows: Vec<&&HistoryRow> = applied[keep..].iter().collect();
    if lock_free(opts) {
        if let Some(problem) = late_problem(shadows, &opts.dir, kept, &pending, files).await? {
            return Ok(refuse(&[problem], log));
        }
    }
    let mut rows = Rows::new(heading.to_string(), rewind_rows.iter().map(|r| r.name.as_str()).chain(pending.iter().map(|m| m.name.as_str())));
    let mut records: Vec<Migration> = Vec::new();
    // The database's catalog as each step leaves it.
    let mut base: Vec<String> = live.to_vec();
    let refused = |what: &str, lines: Vec<String>, log: Log<'_>| {
        let problem = Problem::new("rewind-incomplete", what.to_string()).lines(lines);
        let problem = if d == Dialect::Mysql { problem.note("MySQL commits each statement, so it is part of the way there.") } else { problem.note("That step changed nothing.") };
        print_problems(&[problem.fix("To rebuild this database from the migrations, losing its rows", &["reylo db reset"])], log);
        Ok(1)
    };
    if let Some(problem) = unexplained(shadows, db, opts, kept, &rewind_rows, &pending, &at_kept, live).await? {
        return Ok(refuse(&[problem], log));
    }
    let counts = if rewind_rows.is_empty() && (base == at_kept || pending.is_empty()) { Default::default() } else { table_rows(db).await? };
    if !rewind_rows.is_empty() {
        ensure_history(db).await?;
        let quoted = |name: &str| format!("'{}'", name.replace('\'', "''"));
        let mut forget: Vec<String> = rewind_rows.iter().map(|r| format!("delete from {HISTORY_TABLE} where name = {};", quoted(&r.name))).collect();
        if d == Dialect::Mysql {
            // MySQL commits each statement: the rows go first, so a rewind cut off leaves the
            // migrations it undid unrecorded, to run again, never recorded over a schema without them.
            for statement in forget.drain(..) {
                db.exec(&statement).await?;
            }
        }
        if let Err((_, e)) = shadows.open(k_lane).await? {
            return fail("The history fails when replayed in a shadow database", e, log);
        }
        let k_target = read_target(shadows.db(k_lane), None).await?;
        // What the rewound migrations made, as this database recorded them, is undone, and nothing
        // else: the plan from their replay to the history kept, so that changes no migration made
        // stay, with their rows, and a table one renamed is renamed back.
        let h_lane = shadows.lane();
        shadows.push(h_lane, kept.iter().map(Step::of).chain(rewind_rows.iter().map(|r| Step::migration(&r.name, &r.sql))));
        let mut undone = None;
        if let Ok(Ok(at_applied)) = shadows.lines(h_lane).await {
            if let Ok(()) = shadows.open(h_lane).await? {
                shadows.taint(h_lane);
                let (renames, plan) = plan_to(shadows.db(h_lane), &k_target, false).await?;
                let (only_h, only_k) = difference(&crate::catalog::without_comments(&at_applied), &at_kept);
                let expect: Vec<String> = live.iter().filter(|l| !only_h.contains(l)).cloned().chain(only_k).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
                let statements: Vec<String> = renames.iter().cloned().chain(plan.runnable().cloned()).collect();
                if let Ok(Ok(())) = run_to(db, &statements, &forget, &expect).await {
                    base = expect;
                    undone = Some(plan);
                }
            }
        }
        // Where that doesn't reach it, the database is taken to the history kept itself, which
        // undoes the changes no migration made too.
        let plan = match undone {
            Some(plan) => plan,
            None => match take_to(db, &k_target, &forget, &at_kept, None).await {
                Err(e) => return fail("Rewinding this database failed", e.0, log),
                Ok(Err(Unreached::Refused(problems))) => return Ok(refuse(&problems, log)),
                Ok(Err(Unreached::Differs(now, expect))) => return refused("Rewinding this database wouldn't reach what its migrations make", describe_diff("this database", &now, "its migrations", &expect), log),
                Ok(Ok((_, plan))) => {
                    base = at_kept.clone();
                    plan
                }
            },
        };
        // The first rewound in its place gives way to what the lock now runs there; the rest after it
        // run again in the lock's order.
        let mut displaced = false;
        for r in &rewind_rows {
            let why = match files.iter().find(|f| f.name == r.name) {
                Some(f) if !same_statements(&f.sql, &r.sql, d) => "rewound: it changed since it ran".to_string(),
                Some(_) if !displaced => {
                    displaced = true;
                    match files.get(keep) {
                        Some(before) if before.name != r.name => format!("rewound: the lock now runs {} before it", before.name),
                        _ => "rewound, to run in the lock's order".to_string(),
                    }
                }
                Some(_) => "rewound, to run in the lock's order".to_string(),
                None => "rewound: it isn't in the migrations".to_string(),
            };
            rows.row(log, &paint("yellow", "↺"), &r.name, &why);
            let data = data_statements(&r.sql, d);
            if !data.is_empty() {
                log(&format!("    {} Its rows stay ({}): a rewind undoes the schema, not the data", paint("yellow", "!"), data.join(", ")));
                log(&steps(6, &[("To delete every row", &["reylo db wipe"])]));
            }
        }
        dropped_row(&plan, &counts, log);
    }
    // Changes no migration made, which schema.sql or the next migrations explain (`unexplained`
    // refused the rest), are kept, unless a migration still to run cannot run on top of them.
    if base != at_kept && !pending.is_empty() {
        // The next migrations, none of which changes data, whose change this database already has
        // (sync, then create, then sync) are recorded rather than run again: each line they add is
        // in the database, and none they remove.
        for j in (1..=pending.len()).rev() {
            if !pending[..j].iter().all(|m| data_statements(&m.sql, d).is_empty()) {
                continue;
            }
            let lane = shadows.lane();
            shadows.push(lane, kept.iter().chain(&pending[..j]).map(Step::of));
            if let Ok(Ok(lines)) = shadows.lines(lane).await {
                let (added, removed) = difference(&crate::catalog::without_comments(&lines), &at_kept);
                if (!added.is_empty() || !removed.is_empty()) && added.iter().all(|l| base.contains(l) || extensions.contains(l)) && !removed.iter().any(|l| base.contains(l)) {
                    records = pending.drain(..j).collect();
                    break;
                }
            }
        }
        // The rest is tried first on a shadow database made to read as this one.
        let mut runs = pending.is_empty();
        if !runs {
            let lane = shadows.lane();
            shadows.push(lane, kept.iter().chain(&records).map(Step::of));
            if let Ok(()) = shadows.open(lane).await? {
                shadows.taint(lane);
                let plan = plan_between(shadows.db(lane), db, None).await?;
                let statements: Vec<String> = plan.runnable().cloned().collect();
                if let Ok(Ok(())) = run_to(shadows.db(lane), &statements, &[], &base).await {
                    runs = true;
                    for m in &pending {
                        if run(shadows.db(lane), m).await.is_err() {
                            runs = false;
                            break;
                        }
                    }
                }
            }
        }
        if !runs {
            // A migration that fails on what the migrations make, too, fails for its own reasons:
            // undoing what no migration made would lose it for nothing.
            let clean = shadows.lane();
            shadows.push(clean, kept.iter().chain(&records).chain(&pending).map(Step::of));
            if let Err((m, e)) = shadows.lines(clean).await? {
                return fail(&format!("Replaying {m} in a shadow database fails"), e, log);
            }
            // Undone, to what the migrations kept and recorded make: the recorded ones' change stays.
            let lane = shadows.lane();
            shadows.push(lane, kept.iter().chain(&records).map(Step::of));
            let expect = match shadows.lines(lane).await? {
                Ok(lines) => lines,
                Err((m, e)) => return fail(&format!("Replaying {m} in a shadow database fails"), e, log),
            };
            if let Err((_, e)) = shadows.open(lane).await? {
                return fail("The history fails when replayed in a shadow database", e, log);
            }
            let target = read_target(shadows.db(lane), None).await?;
            match take_to(db, &target, &[], &expect, None).await {
                Err(e) => return fail("Undoing changes no migration made failed", e.0, log),
                Ok(Err(Unreached::Refused(problems))) => return Ok(refuse(&problems, log)),
                Ok(Err(Unreached::Differs(now, expect))) => return refused("Undoing changes no migration made wouldn't reach what the migrations make", describe_diff("this database", &now, "the migrations", &expect), log),
                Ok(Ok((_, plan))) => {
                    rows.open(log);
                    log(&format!("  {} Undid what no migration made, so that {} runs on what the migrations make", paint("yellow", "↺"), pending[0].name));
                    dropped_row(&plan, &counts, log);
                }
            }
        }
    }
    if !records.is_empty() || !pending.is_empty() {
        ensure_history(db).await?;
    }
    for m in &records {
        insert_row(db, m, true, None).await?;
        rows.row(log, &paint("green", "+"), &m.name, "recorded: already in place");
    }
    let mut runs = Runs::new();
    for m in &pending {
        let started = std::time::Instant::now();
        if let Err(Failure { error: e, .. }) = runs.run(db, m).await {
            failed(&mut rows, m, &e, d, log);
            return Ok(1);
        }
        rows.row(log, &paint("green", "✓"), &m.name, &format!("applied {}", paint("dim", &format!("· {} ms", started.elapsed().as_millis()))));
    }
    runs.finish(db).await;
    if rewind_rows.is_empty() && records.is_empty() && pending.is_empty() {
        log(&format!("{} Up to date with the migrations", paint("green", "✓")));
    }
    Ok(0)
}

/// `reylo db push`: takes a development database that is at its migrations (`dev` brings it
/// there) to schema.sql: what schema.sql has that no migration makes is made, unrecorded,
/// for `create` to write next. Refuses a database with a migration it hasn't applied, or one whose
/// history disagrees with the files, and as `create` does, a push that drops a table or column, or
/// changes a column's type in a way that loses values, unless `push` states it with `--drop`
/// (`--force` accepts every one), or renames what may be new. Never in CI.
#[cfg(feature = "cli")]
pub async fn db_push(opts: &Options, in_ci: bool, push: &Push, log: Log<'_>) -> Result<i32, Error> {
    db_push_as(opts, in_ci, push, &Asked::push(false), log).await
}

#[cfg(feature = "cli")]
async fn db_push_as(opts: &Options, in_ci: bool, push: &Push, asked: &Asked, log: Log<'_>) -> Result<i32, Error> {
    if let Some(code) = refuse_lock_file(opts, log) {
        return Ok(code);
    }
    if in_ci {
        return Ok(refused_in_ci("reylo db push", log));
    }
    let (mut db, mut server, swept) = match open(opts, log).await? {
        Ok(opened) => opened,
        Err(code) => return Ok(code),
    };
    let result = db_push_on(&mut db, &mut server, swept, opts, push, asked, log).await;
    db.close().await;
    server.close().await;
    result
}

/// `reylo db push --watch`: syncs, then again each time schema.sql or a migration changes,
/// until it is stopped. Each run prints its block under the time it ran; one that changes nothing
/// prints a line. A refusal (a migration to apply first, a drop `push` doesn't state) is printed and
/// waited out like any run, and what `push` states holds for every run. `runs` bounds how many runs it
/// makes, for a test; None runs until stopped.
#[cfg(feature = "cli")]
pub async fn db_push_watch(opts: &Options, in_ci: bool, push: &Push, runs: Option<usize>, log: Log<'_>) -> Result<i32, Error> {
    if let Some(code) = refuse_lock_file(opts, log) {
        return Ok(code);
    }
    if in_ci {
        return Ok(refused_in_ci("reylo db push --watch", log));
    }
    let snapshot = || -> Vec<(PathBuf, Option<std::time::SystemTime>, u64)> {
        let mut paths = vec![opts.schema.clone()];
        if let Ok(entries) = std::fs::read_dir(&opts.dir) {
            let mut files: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "sql")).collect();
            files.sort();
            paths.extend(files);
        }
        paths.into_iter().map(|p| {
            let meta = std::fs::metadata(&p).ok();
            let (at, len) = (meta.as_ref().and_then(|m| m.modified().ok()), meta.map_or(0, |m| m.len()));
            (p, at, len)
        }).collect()
    };
    log(&paint("dim", &format!("Watching {} and {}: each save pushes again (Ctrl-C stops)", shown(&opts.schema), shown(&opts.dir))));
    let mut made = 0;
    let mut last = Vec::new();
    loop {
        let before = snapshot();
        let mut lines: Vec<String> = Vec::new();
        // A database that went away (restarting) is waited out like a refusal: said, then watched.
        let code = match db_push_as(opts, false, push, &Asked::push(true), &mut |l: &str| lines.push(l.to_string())).await {
            Ok(code) => code,
            Err(e) => {
                lines.push(format!("{} {e}", paint("bold red", "✗")));
                1
            }
        };
        let at = time::clock();
        // A run that pushed nothing is one line, which says what it said before, if anything.
        let pushed = lines.iter().any(|l| l.starts_with("Pushing "));
        let quiet = code == 0 && !pushed && lines.len() <= 2;
        if quiet && !last.is_empty() {
            log(&paint("dim", &format!("{at} · nothing to push")));
        } else {
            log("");
            log(&paint("dim", &format!("{at}")));
            for l in &lines {
                log(l);
            }
        }
        last = lines;
        made += 1;
        if runs.is_some_and(|n| made >= n) {
            return Ok(code);
        }
        // Until a file changes, and a moment longer, so an editor's save of several files is one run.
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            if snapshot() != before {
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                break;
            }
        }
    }
}

#[cfg(feature = "cli")]
async fn db_push_on(db: &mut Db, server: &mut Server, swept: usize, opts: &Options, push: &Push, asked: &Asked, log: Log<'_>) -> Result<i32, Error> {
    swept_note(&server, swept, log);
    let _held = lock(db, opts).await?;
    let d = db.dialect();
    let (files, strays) = opts.migrations()?;
    let schema_sql = opts.schema_text();
    let mut problems = file_problems(opts, &files, &strays, schema_sql.as_deref(), d);
    problems.extend(unreadable_grants(db, opts, &files, schema_sql.as_deref()).await?);
    if schema_sql.as_deref().is_some_and(|sql| crate::sql::is_blank(sql, d)) {
        problems.push(
            Problem::new("schema-sql-empty", format!("Nothing is declared in {}", shown(&opts.schema)))
                .note("Synchronizing this database with it would drop every table, with its rows.")
                .fix("To rebuild this database from its migrations instead", &["reylo db reset"]),
        );
    }
    if schema_sql.is_none() {
        problems.push(Problem::new("schema-sql-missing", format!("There's no {} to push", shown(&opts.schema))).note("This project keeps its schema in its migrations.").fix("Run them", &["reylo migrate dev"]));
    }
    // The database is at its migrations, as `dev` leaves it: every migration applied, each
    // with the text on disk, and nothing else.
    let history = read_history(db).await?;
    let applied: Vec<&HistoryRow> = history.iter().filter(|r| r.applied).collect();
    let mut behind = Vec::new();
    for r in &applied {
        match files.iter().find(|f| f.name == r.name) {
            None => behind.push(format!("  {} isn't in the migrations", r.name)),
            Some(f) if !same_statements(&f.sql, &r.sql, d) => behind.push(format!("  {} changed since it ran", r.name)),
            Some(_) => {}
        }
    }
    let unrun: Vec<&Migration> = files.iter().filter(|f| !applied.iter().any(|r| r.name == f.name)).collect();
    for f in &unrun {
        behind.push(format!("  {} hasn't run here", f.name));
    }
    // The lock lists what the database applied, in its order.
    if behind.is_empty() {
        match opts.lock_state() {
            lock::State::Lock(l) if l.names() == applied.iter().map(|r| r.name.clone()).collect::<Vec<_>>() => {}
            lock::State::Missing => {}
            _ => behind.push(format!("  {} doesn't list what it applied, in its order", shown(&opts.lock))),
        }
    }
    if !behind.is_empty() {
        let (title, fix) = match unrun.as_slice() {
            [one] if behind.len() == 1 => (format!("This database hasn't run {}", one.name), "Run it first"),
            many if behind.len() == many.len() => (format!("This database hasn't run {} migrations", many.len()), "Run them first"),
            _ => ("This database doesn't match its migrations".to_string(), "Make it match its migrations first"),
        };
        let listed = if behind.len() == 1 && unrun.len() == 1 { vec![] } else { behind };
        problems.push(Problem::new("database-not-at-migrations", title).lines(listed).fix(fix, &["reylo migrate dev"]));
    }
    if !problems.is_empty() {
        return Ok(refuse(&problems, log));
    }
    let schema_sql = schema_sql.unwrap_or_default();
    let texts: Vec<&str> = files.iter().map(|f| f.sql.as_str()).chain([schema_sql.as_str()]).collect();
    let mut shadows = Shadows::new(server, &opts.url, &opts.root, &opts.cache, &texts).await;
    let in_order: Vec<Migration> = applied.iter().filter_map(|r| files.iter().find(|f| f.name == r.name).cloned()).collect();
    let result = db_push_in_shadows(&mut shadows, db, opts, &in_order, &schema_sql, push, asked, log).await;
    shadows.finish().await;
    result
}

/// What schema.sql has that no migration makes, made on the database, unrecorded; `applied` is the
/// migrations in the order the database ran them.
#[allow(clippy::too_many_arguments)]
#[cfg(feature = "cli")]
async fn db_push_in_shadows(shadows: &mut Shadows<'_>, db: &mut Db, opts: &Options, applied: &[Migration], schema_sql: &str, push: &Push, asked: &Asked, log: Log<'_>) -> Result<i32, Error> {
    let d = db.dialect();
    let fail = |m: &str, error: String, log: Log<'_>| {
        let partway = error.contains(COMMITTED);
        let problem = Problem::new("db-push-failed", m.to_string()).note(error.replace(&format!("\n{COMMITTED}"), ""));
        if partway {
            print_problems(&[problem.note(COMMITTED)], log);
            return Ok(1);
        }
        if d == Dialect::Mysql {
            print_problems(&[problem.note("MySQL commits each statement, so part of the push may be in place.")], log);
            return Ok(1);
        }
        Ok(refuse(&[problem], log))
    };
    let s_lane = shadows.lane();
    shadows.push(s_lane, [Step::script("schema.sql", schema_sql)]);
    match shadows.open(s_lane).await? {
        Err((_, e)) => return fail(&format!("On an empty database, {} fails", shown(&opts.schema)), e, log),
        Ok(()) => shadows.taint(s_lane),
    }
    let wanted = catalog(shadows.db(s_lane)).await?;
    let now = crate::catalog::without_extra_children(&catalog(db).await?, &wanted);
    unmodeled_note("Reylo doesn't fully compare these in schema.sql, so a push can leave them, or a change to them, out: check them by hand", &crate::catalog::unmodeled(shadows.db(s_lane), true).await?, log);
    if now != wanted {
        let target = read_target(shadows.db(s_lane), Some(schema_sql)).await?;
        let counts = table_rows(db).await?;
        let (renames, plan) = match take_to(db, &target, &[], &wanted, Some((push, asked))).await {
            Err(e) => return fail(if d == Dialect::Mysql { "reylo db push failed partway" } else { "reylo db push failed" }, e.0, log),
            Ok(Err(Unreached::Refused(problems))) => return Ok(refuse(&problems, log)),
            Ok(Err(Unreached::Differs(now, expect))) => {
                let problem = Problem::new("db-push-incomplete", format!("Reylo can't fully synchronize this database with {}", shown(&opts.schema))).lines(describe_diff("this database", &now, &shown(&opts.schema), &expect)).fix("Write the change as a migration, finishing by hand what it leaves out", &["reylo migrate new <name>"]);
                if d != Dialect::Mysql {
                    return Ok(refuse(&[problem], log));
                }
                print_problems(&[problem.note("MySQL commits each statement, so part of the push may be in place.")], log);
                return Ok(1);
            }
            Ok(Ok(done)) => done,
        };
        log(&format!("Pushing {} to {}", shown(&opts.schema), shown_url(&opts.url)));
        for statement in renames.iter().chain(&plan.statements) {
            statement_row(statement, log);
        }
        dropped_row(&plan, &counts, log);
    }
    // Whether the migrations themselves make schema.sql, or this database holds more than they do.
    let m_lane = shadows.lane();
    shadows.push(m_lane, applied.iter().map(Step::of));
    let made = match shadows.lines(m_lane).await? {
        Ok(lines) => lines == wanted,
        Err(_) => false,
    };
    // The lock certifies that its migrations reach this schema.sql.
    let mut wrote = false;
    if made {
        if let lock::State::Lock(mut l) = opts.lock_state() {
            let sum = lock::schema_sum(&l.entries.last().map(|e| e.sum.clone()).unwrap_or_default(), schema_sql);
            if l.schema.as_deref() != Some(sum.as_str()) {
                l.schema = Some(sum);
                lock::write(&opts.lock, &l)?;
                wrote = true;
            }
        }
    }
    let schema = shown(&opts.schema);
    if made {
        log(&format!("{} This database matches {schema} and its migrations", paint("green", "✓")));
    } else {
        log(&format!("{} This database matches {schema}", paint("green", "✓")));
    }
    if wrote {
        log(&format!("{} Wrote {}", paint("green", "✓"), shown(&opts.lock)));
    }
    Ok(0)
}

/// One statement `db push` ran: `+` for what it adds, `-` for what it takes away; a comment is dim.
fn statement_row(statement: &str, log: Log<'_>) {
    if let Some(said) = statement.strip_prefix("--") {
        log(&format!("    {} {}", paint("yellow", "!"), said.trim()));
        return;
    }
    let text = statement.trim_end();
    let lower = text.trim_start().to_lowercase();
    let mark = if lower.starts_with("drop ") || lower.contains(" drop ") { paint("yellow", "-") } else { paint("green", "+") };
    for (k, line) in format!("{text};").lines().enumerate() {
        if k == 0 {
            log(&format!("  {mark} {line}"));
        } else {
            log(&format!("    {line}"));
        }
    }
}

/// The rows of each of the database's tables, by table id.
#[cfg(feature = "cli")]
async fn table_rows(db: &mut Db) -> Result<std::collections::BTreeMap<String, u64>, Error> {
    let schema = crate::diff::read(db).await?;
    // By the id a plan's losses name it by: on Postgres, without the first schema of the search_path.
    let first = if db.dialect() == Dialect::Postgres { format!(" {}.", crate::diff::postgres::first_schema(db).await?) } else { String::new() };
    let mut out = std::collections::BTreeMap::new();
    for o in schema.objects.values().filter(|o| o.kind == Some(crate::diff::Kind::Table) && !o.extra.iter().any(|l| l.starts_with("shape foreign"))) {
        let Some(name) = o.drop.first().and_then(|d| d.strip_prefix("drop table ")) else { continue };
        if let Ok(rows) = db.rows(&format!("select count(*) as n from {name}")).await {
            if let Some(n) = rows.first().and_then(|r| r.get("n")).and_then(|n| n.parse().ok()) {
                out.insert(if first.is_empty() { o.id.clone() } else { o.id.replacen(&first, " ", 1) }, n);
            }
        }
    }
    Ok(out)
}

/// The line saying what a plan drops, under the row it belongs to, if it drops anything.
#[cfg(feature = "cli")]
fn dropped_row(plan: &crate::diff::Plan, counts: &std::collections::BTreeMap<String, u64>, log: Log<'_>) {
    if let Some(dropped) = dropped_items(plan, counts) {
        log(&format!("    {} {dropped}", paint("yellow", "!")));
    }
}

/// What a plan drops, as one phrase: each table with how many rows it held where `counts` holds any,
/// and each column ("Dropped tags (3 rows) and users.bio"); many empty tables, counted.
#[cfg(feature = "cli")]
fn dropped_items(plan: &crate::diff::Plan, counts: &std::collections::BTreeMap<String, u64>) -> Option<String> {
    if plan.losses.is_empty() {
        return None;
    }
    let tables = plan.losses.iter().filter(|l| l.starts_with("table ")).count();
    if tables > 2 && tables == plan.losses.len() && plan.losses.iter().all(|l| counts.get(l).copied().unwrap_or(0) == 0) {
        return Some(format!("Dropped {tables} tables, none with rows"));
    }
    let what: Vec<String> = plan
        .losses
        .iter()
        .map(|l| {
            let name = shown_id(l);
            match counts.get(l).copied() {
                Some(1) => format!("{name} (1 row)"),
                Some(n) if n > 1 => format!("{name} ({n} rows)"),
                _ => name.to_string(),
            }
        })
        .collect();
    Some(format!("Dropped {}", and_list(&what)))
}

/// What a migration drops, each kind together: `the column users.bio`, `the columns users.a and
/// users.b`, `the table tags, and the column users.bio`.
fn drop_phrase(items: &[(String, String)]) -> String {
    let mut kinds: Vec<&str> = Vec::new();
    for (kind, _) in items {
        if !kinds.contains(&kind.as_str()) {
            kinds.push(kind);
        }
    }
    kinds
        .iter()
        .map(|kind| {
            let names: Vec<String> = items.iter().filter(|(k, _)| k == kind).map(|(_, n)| n.clone()).collect();
            if names.len() == 1 { format!("the {kind} {}", names[0]) } else { format!("the {kind}s {}", and_list(&names)) }
        })
        .collect::<Vec<_>>()
        .join(", and ")
}

/// Whether `sql` names `role`, written as `quote_ident` writes it, as a word of its own.
#[cfg(feature = "cli")]
fn names(sql: &str, role: &str) -> bool {
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$');
    sql.match_indices(role).any(|(k, _)| !word(sql[..k].chars().next_back()) && !word(sql[k + role.len()..].chars().next()))
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// `reylo run "<sql>"` and `reylo run --file <path>`: runs SQL on the database as written, inline
/// text or files, in the order given; records nothing. For a change typed by hand during
/// development, and a seed after `reylo migrate dev`.
#[cfg(feature = "cli")]
pub async fn run_sql(opts: &Options, items: &[(String, String)], log: Log<'_>) -> Result<i32, Error> {
    let (url, root) = (opts.url.as_str(), opts.root.as_path());
    // A SQLite file that isn't there is no database to run on, and opening it would make
    // one: refused, as the migrations' commands refuse it.
    if let Some((path, which)) = missing_at(url, root)? {
        return Ok(refuse(&[Problem::new("no-database", format!("There's no {which} database at {}", path.display()))], log));
    }
    let mut db = Db::open(opts).await?;
    let mut code = 0;
    for (label, sql) in items {
        match db.script_tables(sql, RUN_ROWS_SHOWN).await {
            Ok(tables) => {
                log(&format!("{} {label}", paint("green", "✓")));
                for (k, table) in tables.iter().enumerate() {
                    if k > 0 {
                        log("");
                    }
                    for line in table_lines(table) {
                        log(&line);
                    }
                }
            }
            Err(e) => {
                log(&paint("bold red", &format!("✗ {label}")));
                log(&format!("  {e}"));
                code = 1;
                break;
            }
        }
    }
    // What ran before a statement that failed may have committed, so the sequences follow it either
    // way: after a transaction the SQL left open, by failing or never committing, is rolled back, as
    // closing would, so that it takes nothing with it. A failure of their own is reported where the
    // SQL itself ran.
    if db.dialect() == Dialect::Sqlite {
        let _ = db.exec("rollback").await;
    }
    let kept = crate::catalog::keep_turso_sequences(&mut db).await;
    db.close().await;
    if code == 0 {
        kept?;
    }
    Ok(code)
}

/// How many rows `reylo run` shows of what a statement returns.
#[cfg(feature = "cli")]
const RUN_ROWS_SHOWN: usize = 50;

/// What a statement returned, as `reylo run` shows it: a table of the rows it kept under its
/// columns' names, each value on one line, a number set right and null dim, then how many rows it
/// returned, and how many of those aren't shown.
#[cfg(feature = "cli")]
fn table_lines(table: &crate::db::Table) -> Vec<String> {
    const WIDEST: usize = 60;
    let cell = |value: &Option<String>| -> Option<String> {
        let text = value.as_ref()?.replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t");
        Some(if text.chars().count() > WIDEST { format!("{}…", text.chars().take(WIDEST - 1).collect::<String>()) } else { text })
    };
    let rows: Vec<Vec<Option<String>>> = table.rows.iter().map(|row| row.iter().map(cell).collect()).collect();
    let width = |i: usize| rows.iter().map(|r| r[i].as_ref().map_or(4, |v| v.chars().count())).chain([table.columns[i].chars().count()]).max().unwrap_or(0);
    let widths: Vec<usize> = (0..table.columns.len()).map(width).collect();
    let numeric: Vec<bool> = (0..table.columns.len()).map(|i| rows.iter().any(|r| r[i].is_some()) && rows.iter().all(|r| r[i].as_ref().is_none_or(|v| v.parse::<f64>().is_ok()))).collect();
    let pad = |text: &str, painted: String, i: usize| {
        let gap = " ".repeat(widths[i] - text.chars().count());
        if numeric[i] { format!("{gap}{painted}") } else { format!("{painted}{gap}") }
    };
    let line = |cells: Vec<String>| format!("  {}", cells.join("  ")).trim_end().to_string();
    let mut out = vec![line(table.columns.iter().enumerate().map(|(i, c)| pad(c, paint("bold", c), i)).collect())];
    out.push(line(widths.iter().map(|w| paint("dim", &"─".repeat(*w))).collect()));
    for row in &rows {
        out.push(line(row.iter().enumerate().map(|(i, v)| match v {
            Some(v) => pad(v, v.clone(), i),
            None => pad("null", paint("dim", "null"), i),
        }).collect()));
    }
    let total = table.total;
    let counted = format!("  Returned {total} row{}", if total == 1 { "" } else { "s" });
    out.push(paint("dim", &if total > rows.len() { format!("{counted}, {} not shown", total - rows.len()) } else { counted }));
    out
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "cli")]
    #[test]
    fn renames_are_read_from_each_form_of_the_statement() {
        use super::renames_of;
        let (p, m) = (Dialect::Postgres, Dialect::Mysql);
        let names = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let pairs = |v: &[(&str, &str)]| v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
        assert_eq!(renames_of("alter table public.Old rename to fresh;", p), (names(&["old"]), vec![]));
        assert_eq!(renames_of("alter table if exists only \"Users\" rename column \"Bio\" to about;", p), (vec![], pairs(&[("Users", "Bio")])));
        assert_eq!(renames_of("alter table users rename bio to about, add column x int;", p), (vec![], pairs(&[("users", "bio")])));
        assert_eq!(renames_of("alter table users rename constraint a to b;", p), (vec![], vec![]));
        assert_eq!(renames_of("rename table a to b, `C` to d;", m), (names(&["a", "C"]), vec![]));
        assert_eq!(renames_of("alter table users change column bio about text, change n n bigint;", m), (vec![], pairs(&[("users", "bio")])));
        assert_eq!(renames_of("alter table users drop column bio;", p), (vec![], vec![]));
    }

    #[cfg(feature = "cli")]
    #[test]
    fn a_grant_to_a_role_of_its_own_is_sql_to_write() {
        use super::{NewDatabase, holds_sql};
        let lines = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let stock = NewDatabase::default();
        let supabase = NewDatabase { platform: true, ..NewDatabase::default() };
        let platform = lines(&["schema public", "grant schema public PUBLIC:USAGE,anon:USAGE,service_role:USAGE*", "default privileges in schema public grant select on tables to anon"]);
        assert!(!holds_sql(&platform, &supabase));
        assert!(holds_sql(&platform, &stock), "anon is a role of the project's on a server of no platform");
        assert!(!holds_sql(&lines(&["schema public", "grant schema public PUBLIC:USAGE"]), &stock));
        assert!(holds_sql(&lines(&["schema public", "grant schema public PUBLIC:CREATE,PUBLIC:USAGE"]), &stock));
        assert!(!holds_sql(&lines(&["schema public", "grant schema public PUBLIC:CREATE,PUBLIC:USAGE"]), &NewDatabase { public_create: true, ..NewDatabase::default() }));
        assert!(holds_sql(&lines(&["schema public", "schema reports", "grant schema reports PUBLIC:USAGE"]), &stock));
        assert!(holds_sql(&lines(&["schema public", "grant schema public PUBLIC:USAGE,reporting:CREATE"]), &supabase));
        assert!(holds_sql(&lines(&["schema public", "default privileges in schema public grant select on tables to reporting with grant option"]), &supabase));
        assert!(holds_sql(&lines(&["schema public", "default privileges revoke execute on functions from public"]), &supabase));
        assert!(super::names("grant select on table t to ops_read;", "ops_read"));
        assert!(!super::names("grant select on table t to ops_readwrite;", "ops_read"));
        assert!(!super::names("create table ops_read_log (id int);", "ops_read"));
    }

    use super::*;

    #[test]
    fn a_virtual_tables_own_tables_are_left_out_of_its_summary() {
        let lines = |l: &[&str]| -> Vec<String> { l.iter().map(|s| s.to_string()).collect() };
        let before = lines(&["table notes", "column notes.id integer primary key"]);
        let mut after = before.clone();
        after.extend(lines(&[
            "table notes_fts using fts5",
            "column notes_fts.body",
            "column notes_fts.content = 'notes'",
            "table notes_fts_config without rowid",
            "column notes_fts_config.k primary key",
            "table notes_fts_data",
            "column notes_fts_data.id integer primary key",
            "table box using rtree",
            "table box_node",
            "table notes_fts_archive",
            "table notes_fts_content",
        ]));
        let shown = changed_lines(&before, &after, Dialect::Sqlite).join("\n");
        assert!(shown.contains("notes_fts using fts5") && shown.contains("notes_fts_archive") && shown.contains("notes_fts_content") && shown.contains("box using rtree"), "{shown}");
        assert!(!shown.contains("notes_fts_config") && !shown.contains("notes_fts_data") && !shown.contains("box_node"), "{shown}");
    }

    #[test]
    fn a_file_that_adds_or_retypes_a_column_alters_its_rows() {
        let pg = Dialect::Postgres;
        assert!(alters_rows("alter table people add column nick text not null;", pg));
        assert!(alters_rows("alter table people add nick text;", pg));
        assert!(alters_rows("alter table people alter column score type bigint using (-score);", pg));
        assert!(alters_rows("alter table people add column bonus int not null default 0, add constraint people_bonus check ((bonus > 0))", Dialect::Mysql));
        assert!(alters_rows("alter table people modify column mood enum('ok')", Dialect::Mysql));
        assert!(!alters_rows("alter table people add constraint people_mood check (mood in ('a', 'b')) not valid;", pg));
        assert!(!alters_rows("alter table people validate constraint people_mood;", pg));
        assert!(!alters_rows("alter table people alter column id set not null;", pg));
        assert!(!alters_rows("alter table users drop index users_org_email, add unique key users_email (email)", Dialect::Mysql));
        assert!(!alters_rows("create unique index concurrently users_email on users (email);", pg));
    }

    #[test]
    fn a_constraint_not_validated_is_said_so_once() {
        assert_eq!(readable("constraint public.users users_plan_check CHECK ((plan = 'free'::text)) NOT VALID not valid"), "constraint users_plan_check on users: check ((plan = 'free'::text)) not valid");
    }

    #[test]
    fn the_files_of_one_change_share_a_table_or_an_index() {
        let pg = Dialect::Postgres;
        assert!(related("alter table people add constraint people_mood check (mood <> 'x') not valid;", "alter table people validate constraint people_mood;", pg));
        assert!(related("drop index users_email;", "create unique index concurrently users_email on users (email);", pg));
        assert!(related("create unique index concurrently users_email_new on users (email);", "drop index users_email;\nalter index users_email_new rename to users_email;", pg));
        assert!(!related("alter table teams add column note text;", "alter table people validate constraint people_mood;", pg));
    }

    #[test]
    fn what_a_migration_drops_is_read_by_name() {
        let names = |sql: &str, d: Dialect| dropped_by(sql, d).into_iter().map(|x| (x.name, x.written, x.table)).collect::<Vec<_>>();
        assert_eq!(dropped_by("alter table people drop check people_x, drop index people_y", Dialect::Mysql).iter().map(|x| x.index).collect::<Vec<_>>(), [false, true]);
        assert!(names("alter domain positive drop constraint positive_check;", Dialect::Postgres).is_empty());
        assert_eq!(names("drop index people_x on people", Dialect::Mysql), [("people_x".into(), "people_x".into(), Some("people".to_string()))]);
        let some = |s: &str| Some(s.to_string());
        assert_eq!(names("drop index concurrently if exists app.users_email;\nalter table only public.users drop constraint \"Users_Key\";", Dialect::Postgres), [("users_email".into(), "app.users_email".into(), None), ("Users_Key".into(), "\"Users_Key\"".into(), some("public.users"))]);
        assert_eq!(names("alter table users drop index users_org_email, add unique key users_email (email)", Dialect::Mysql), [("users_org_email".into(), "users_org_email".into(), some("users"))]);
        assert!(dropped_by("alter table people drop constraint people_mood;\nalter table people add constraint people_mood check (mood in ('a')) not valid;", Dialect::Postgres).is_empty());
        assert!(dropped_by("drop index users_email;\ncreate unique index concurrently users_email on users (email);", Dialect::Postgres).is_empty());
    }

    #[test]
    fn a_mysql_duplicate_names_the_last_key_its_message_gives() {
        let b = crate::db::mysql_breach(1062, "Duplicate entry 'x' for key 'y' for key 'users.email'").unwrap();
        assert_eq!((b.table.as_str(), b.name.as_str()), ("users", "email"));
        let b = crate::db::mysql_breach(3819, "Check constraint 'people_mood' is violated.").unwrap();
        assert_eq!(b.name, "people_mood");
    }

    #[test]
    fn the_rows_query_reads_a_mysql_statement_after_blank_lines() {
        let b = Breach { kind: Breaks::Check, name: "people_mood".into(), ..Breach::default() };
        assert_eq!(mysql_rows_query("\n\nalter table people drop check people_mood, add constraint people_mood check ((mood <> 'bad'));\n", &b).as_deref(), Some("select * from people where not ((mood <> 'bad'));"));
    }

    #[test]
    fn a_change_written_as_several_files_is_told_by_name_and_time() {
        let names = ["20261027110000_ab.sql", "20261027110001_ab_2.sql", "20261027110002_ab_3.sql", "20261027110005_ab_2.sql", "20261027110001_other.sql"];
        assert_eq!(same_change("20261027110001_ab_2.sql", names), (vec!["20261027110000_ab.sql".to_string()], vec!["20261027110002_ab_3.sql".to_string()]));
        // A change whose own name ends in `_2`, as `tighten_2`, then `tighten_2_2`.
        assert_eq!(same_change("20261027110001_tighten_2_2.sql", ["20261027110000_tighten_2.sql"]).0, ["20261027110000_tighten_2.sql"]);
        assert_eq!(same_change("20261027110000_tighten_2.sql", ["20261027110001_tighten_2_2.sql"]).1, ["20261027110001_tighten_2_2.sql"]);
        // Over midnight, by the time each name gives.
        assert_eq!(same_change("20261028000000_ab_2.sql", ["20261027235959_ab.sql"]).0, ["20261027235959_ab.sql"]);
    }

    #[test]
    fn a_stray_with_a_shorter_stamp_is_padded_where_its_name_sorts() {
        let files = |names: &[&str]| -> Vec<Migration> { names.iter().map(|n| Migration { sql: String::new(), name: n.to_string(), time: if *n == INIT { "00000000000000".into() } else { n[..14].to_string() }, path: PathBuf::from(n) }).collect() };
        let renames_at = |now: &str, names: &[&str], strays: &[&str]| stray_renames(&files(names), &strays.iter().map(|s| s.to_string()).collect::<Vec<_>>(), time::seconds(now).unwrap());
        let renames = |names: &[&str], strays: &[&str]| renames_at("20261123191900", names, strays);
        // Between its neighbours, from the start of its day.
        assert_eq!(renames(&[INIT, "20260101000000_a.sql", "20260401000000_c.sql"], &["20260320_b.sql"]), ["Rename 20260320_b.sql to 20260320000000_b.sql."]);
        assert_eq!(renames(&[], &["20260320_a.sql", "20260320_b.sql"]), ["Rename 20260320_a.sql to 20260320000000_a.sql.", "Rename 20260320_b.sql to 20260320000000_b.sql."]);
        // Strays come in name order, and each keeps its place after those before it.
        assert_eq!(renames(&[], &["202603201230_c.sql", "20260320_a.sql"]), ["Rename 202603201230_c.sql to 20260320123000_c.sql.", "Rename 20260320_a.sql to 20260320235959_a.sql."]);
        // After a migration of the same day, which its name sorts after, from the end of the day.
        assert_eq!(renames(&["20260320100000_x.sql", "20260401000000_c.sql"], &["20260320_b.sql"]), ["Rename 20260320_b.sql to 20260320235959_b.sql."]);
        // A name with no date in front takes the time now, as does one whose digits are no date.
        assert_eq!(renames(&["20260101000000_a.sql"], &["notes.sql", "20261399_x.sql", "README.md"]), ["Rename notes.sql to 20261123191900_notes.sql.", "Rename 20261399_x.sql to 20261123191900_20261399_x.sql."]);
        // The end of a period read from the prefix's own digits: 1 of a 9-digit stamp is the hours 10 to 19.
        assert_eq!(renames(&["20260320180000_a.sql"], &["202603201_b.sql"]), ["Rename 202603201_b.sql to 20260320195959_b.sql."]);
        // No two renames, nor a rename and a migration, take one name.
        assert_eq!(renames(&["20261123191900_bar.sql"], &["2026112319190_foo.sql", "bar.sql", "foo.sql"]), ["Rename 2026112319190_foo.sql to 20261123191900_foo.sql.", "Rename bar.sql to 20261123191901_bar.sql.", "Rename foo.sql to 20261123191901_foo.sql."]);
        // A name that differs only in case is taken, and an unstamped file takes its time after the stamped ones.
        assert_eq!(renames(&["20260320180000_a.sql"], &["20260320195959_B.sql", "202603201_b.sql"]), ["Rename 202603201_b.sql with a time that runs it where its name sorts, after 20260320180000_a.sql."]);
        assert_eq!(renames_at("20261123191929", &["20261123191925_a.sql"], &["1_notes.sql", "2026112319192_1_notes.sql"]), ["Rename 2026112319192_1_notes.sql to 20261123191929_1_notes.sql.", "Rename 1_notes.sql to 20261123191930_1_notes.sql."]);
        // No time of the day keeps it between its neighbours.
        assert_eq!(renames(&["20260320000000_z.sql", "20260320235959_z.sql", "20260401000000_c.sql"], &["20260320_b.sql"]), ["Rename 20260320_b.sql with a time that runs it where its name sorts, after 20260320235959_z.sql and before 20260401000000_c.sql."]);
    }

    #[test]
    fn a_printed_run_is_offered_only_where_it_runs_as_a_migration() {
        for (sql, d) in [
            ("create index a_id on a (id);", Dialect::Sqlite),
            ("drop index a_id;", Dialect::Sqlite),
            ("create table b (id integer);", Dialect::Sqlite),
            ("drop index a_id on a;", Dialect::Mysql),
            ("drop index a_id; drop table b;", Dialect::Postgres),
            ("create index concurrently a_id on a (id);", Dialect::Postgres),
        ] {
            assert!(runs_as_a_migration(sql, d), "{sql}");
        }
        for (sql, d) in [
            // With foreign keys on, as reylo run has them, a parent's rows take its children's.
            ("drop table parent;", Dialect::Sqlite),
            ("alter table a drop column b;", Dialect::Sqlite),
            ("drop index a_id; drop index b_id;", Dialect::Sqlite),
            ("drop index a_id on a; drop table b;", Dialect::Mysql),
            ("alter type mood add value 'x'; commit; begin; update t set m = 'x';", Dialect::Postgres),
        ] {
            assert!(!runs_as_a_migration(sql, d), "{sql}");
        }
    }

    #[cfg(feature = "cli")]
    #[test]
    fn a_table_made_from_a_query_is_told_by_its_words() {
        let fills_all = |sql: &str, d: Dialect| split_statements(sql, d).iter().any(|s| fills(s, d));
        for (sql, d) in [
            ("create table snap as select id from a", Dialect::Postgres),
            ("CREATE TEMP TABLE IF NOT EXISTS snap AS (WITH x AS (SELECT 1) SELECT * FROM x)", Dialect::Postgres),
            ("create table snap as table a", Dialect::Postgres),
            ("create table snap (id bigint) engine=InnoDB select id from a", Dialect::Mysql),
            ("create table snap (select id from a)", Dialect::Mysql),
            ("create table snap (id bigint) ((select id from a))", Dialect::Mysql),
            ("create materialized view totals as select count(*) from a", Dialect::Postgres),
            ("refresh materialized view totals", Dialect::Postgres),
        ] {
            assert!(fills_all(sql, d), "{sql}");
        }
        for (sql, d) in [
            ("create table notes (id int, body text default 'select')", Dialect::Sqlite),
            ("create table notes (with integer)", Dialect::Sqlite),
            ("create table notes (values text, table text)", Dialect::Sqlite),
            ("create table notes (id int) -- as select\n", Dialect::Postgres),
            ("create table t (a int, b int generated always as (a + 1) stored)", Dialect::Postgres),
            ("create table p partition of parent for values in (1)", Dialect::Postgres),
            ("create table t (id int) with (fillfactor = 70)", Dialect::Postgres),
            (r#"create table t ("select" int)"#, Dialect::Postgres),
            ("create view v as select id from a", Dialect::Postgres),
        ] {
            assert!(!fills_all(sql, d), "{sql}");
        }
    }

    #[test]
    #[cfg(feature = "cli")]
    fn a_statements_rows_show_as_a_table() {
        set_color(false);
        let t = |columns: &[&str], rows: &[&[Option<&str>]], total: usize| crate::db::Table { columns: columns.iter().map(|c| c.to_string()).collect(), rows: rows.iter().map(|r| r.iter().map(|v| v.map(String::from)).collect()).collect(), total };
        let authors = t(&["name", "score", "bio"], &[&[Some("Ada"), Some("3"), None], &[Some("Grace"), Some("12"), Some("line one\nline two")]], 2);
        assert_eq!(table_lines(&authors).join("\n"), "  name   score  bio\n  ─────  ─────  ──────────────────\n  Ada        3  null\n  Grace     12  line one\\nline two\n  Returned 2 rows");
        // Of a statement's rows, those kept are shown, and the count says how many weren't.
        assert_eq!(table_lines(&t(&["name", "score", "bio"], &[&[Some("Ada"), Some("3"), None]], 51)).join("\n"), "  name  score  bio\n  ────  ─────  ────\n  Ada       3  null\n  Returned 51 rows, 50 not shown");
        assert_eq!(table_lines(&t(&["n"], &[], 0)).join("\n"), "  n\n  ─\n  Returned 0 rows");
    }

    #[test]
    #[cfg(feature = "cli")]
    fn a_libsql_script_splits_where_sqlite_ends_each_statement() {
        // A column named end in a trigger's body, a `;` in a string and in a comment, and a last statement with no `;`.
        let script = "create trigger t after insert on a begin insert into audit values (new.end); end;\n-- a note; and more\ninsert into a values ('x;y');\n\nselect 1";
        assert_eq!(crate::db::sqlite_statements(script), ["create trigger t after insert on a begin insert into audit values (new.end); end;", "\n-- a note; and more\ninsert into a values ('x;y');", "\n\nselect 1"]);
        assert!(crate::db::sqlite_statements("-- nothing;\n").is_empty());
        // An empty statement is none, a comment before its `;` or not.
        assert_eq!(crate::db::sqlite_statements("select 1;; -- note\n; select 2;"), ["select 1;", " select 2;"]);
    }

    #[test]
    #[cfg(feature = "cli")]
    fn an_enum_with_a_value_added_is_only_its_sides_change() {
        let lines = |values: &str| vec!["table public.t".to_string(), format!("enum public.visibility ({values})")];
        let (two, three) = (lines(r#""private", "public""#), lines(r#""private", "team", "public""#));
        assert_eq!(sides(&two, &three), (false, true));
        assert_eq!(sides(&three, &two), (true, false));
        // Reordered, or one taken out and another added, each side has what the other doesn't.
        assert_eq!(sides(&lines(r#""public", "private""#), &two), (true, true));
        assert_eq!(sides(&lines(r#""private", "team""#), &two), (true, true));
        // A quoted name with a space and a parenthesis in it is one name.
        let quoted = |values: &str| vec![format!("enum public.\"state (internal)\" ({values})")];
        assert_eq!(sides(&quoted(r#""a""#), &quoted(r#""a", "b""#)), (false, true));
        // A value with a comma in it is one value.
        assert_eq!(sides(&lines(r#""a, b""#), &lines(r#""a, b", "c""#)), (false, true));
        assert_eq!(sides(&lines(r#""a""#), &lines(r#""a, b""#)), (true, true));
        // Another line that differs still counts.
        let mut more = three.clone();
        more.push("column public.t.x integer".into());
        assert_eq!(sides(&more, &two), (true, false));
        assert_eq!(sides(&[two.clone(), vec!["column public.t.x integer".into()]].concat(), &three), (true, true));
    }

    #[test]
    fn a_new_table_lists_what_it_holds_under_it() {
        let lines = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let made = lines(&[
            "column public.documents.embedding public.vector(1536)",
            "column public.documents.id integer not null",
            "column public.users.bio text",
            "constraint public.documents documents_pkey PRIMARY KEY (id)",
            "extension vector schema=public",
            "index CREATE INDEX documents_embedding ON public.documents USING hnsw (embedding public.vector_cosine_ops)",
            "index CREATE UNIQUE INDEX documents_pkey ON public.documents USING btree (id)",
            "table public.documents",
        ]);
        let users = lines(&["table public.users", "column public.users.id integer"]);
        let before: Vec<String> = users.clone();
        let after: Vec<String> = users.iter().chain(&made).cloned().collect();
        assert_eq!(
            changed_lines(&before, &after, Dialect::Postgres),
            [
                "  + column users.bio text",
                "  + extension vector schema=public",
                "  + table documents",
                "      embedding vector(1536)",
                "      id integer not null",
                "      primary key (id)",
                "      index documents_embedding using hnsw (embedding vector_cosine_ops)",
            ]
        );
        // A table dropped reads the same way, and SQLite's constraints, which name none, as theirs.
        assert_eq!(changed_lines(&lines(&["table t strict", "column t.id integer", "constraint t primary key (id)"]), &[], Dialect::Sqlite), ["  - table t strict", "      id integer", "      primary key (id)"]);
        // A table both sides have is no new table, whatever its own line became.
        assert_eq!(
            changed_lines(&lines(&["table public.t", "column public.t.id integer"]), &lines(&["table public.t row level security", "column public.t.id integer", "column public.t.label text"]), Dialect::Postgres),
            ["  + table t row level security", "  + column t.label text", "  - table t"]
        );
        // A line two tables could own, `t` and `t.archive`, stays a line of its own.
        assert_eq!(
            changed_lines(&lines(&["table t.archive", "column t.archive.id integer"]), &lines(&["table t.archive", "column t.archive.id integer", "column t.archive.label text", "table t", "column t.id integer"]), Dialect::Sqlite),
            ["  + column t.archive.label text", "  + table t", "      id integer"]
        );
        // A quoted name is one name.
        assert_eq!(changed_lines(&[], &lines(&["table public.\"order items\"", "column public.\"order items\".id integer"]), Dialect::Postgres), ["  + table \"order items\"", "      id integer"]);
        assert_eq!(changed_lines(&[], &lines(&["table public.\"orders engine=archive\"", "column public.\"orders engine=archive\".id integer"]), Dialect::Postgres), ["  + table \"orders engine=archive\"", "      id integer"]);
        // On SQLite, `options` is a table like any other: made strict, it is no new table.
        assert_eq!(
            changed_lines(&lines(&["table options", "column options.id integer", "column options.note text"]), &lines(&["table options strict", "column options.id integer"]), Dialect::Sqlite),
            ["  + table options strict", "  - table options", "  - column options.note text"]
        );
        // MySQL writes its names unquoted, a table's as all before its engine. Its foreign keys,
        // checks and table options go under their table, but for one two tables could own, `orders`
        // and `orders archive`.
        assert_eq!(
            changed_lines(
                &[],
                &lines(&[
                    "table orders engine=InnoDB collate=utf8mb4_0900_ai_ci",
                    "column orders.id int not null",
                    "foreign key orders fk_user (user_id) references users (id) on update RESTRICT on delete CASCADE",
                    "table orders archive engine=InnoDB collate=utf8mb4_0900_ai_ci",
                    "column orders archive.id int not null",
                    "check orders archive positive (`id` > 0)",
                    "table options orders key_block_size=8",
                ]),
                Dialect::Mysql
            ),
            [
                "  + table orders engine=InnoDB collate=utf8mb4_0900_ai_ci",
                "      id int not null",
                "      foreign key fk_user (user_id) references users (id) on update RESTRICT on delete CASCADE",
                "      options key_block_size=8",
                "  + table orders archive engine=InnoDB collate=utf8mb4_0900_ai_ci",
                "      id int not null",
                "  + check orders archive positive (`id` > 0)",
            ]
        );
        // A table named `options orders` is a table beside `orders`, made or dropped with it.
        let both = lines(&["table orders engine=InnoDB collate=c", "column orders.id int not null", "table options orders engine=InnoDB collate=c", "column options orders.id int not null"]);
        let grouped = ["table orders engine=InnoDB collate=c", "      id int not null", "table options orders engine=InnoDB collate=c", "      id int not null"];
        assert_eq!(changed_lines(&[], &both, Dialect::Mysql), grouped.map(|l| if l.starts_with("table") { format!("  + {l}") } else { l.to_string() }));
        assert_eq!(changed_lines(&both, &[], Dialect::Mysql), grouped.map(|l| if l.starts_with("table") { format!("  - {l}") } else { l.to_string() }));
        // A MySQL table named `options` is a table, by its engine.
        assert_eq!(
            changed_lines(&[], &lines(&["table options engine=InnoDB collate=utf8mb4_0900_ai_ci", "column options.id int not null"]), Dialect::Mysql),
            ["  + table options engine=InnoDB collate=utf8mb4_0900_ai_ci", "      id int not null"]
        );
    }

    #[test]
    fn a_function_a_trigger_and_a_view_read_as_one_line_each() {
        let function = "function CREATE OR REPLACE FUNCTION public.touch()\n RETURNS trigger\n LANGUAGE plpgsql\nAS $function$\nbegin new.updated_at := now(); return new; end $function$";
        assert_eq!(readable(function), "function touch() returns trigger");
        assert_eq!(readable("function CREATE OR REPLACE PROCEDURE public.tidy(IN days integer DEFAULT 30)\n LANGUAGE sql\nAS $procedure$ delete from logs $procedure$"), "procedure tidy(in days integer default 30)");
        assert_eq!(readable("trigger CREATE TRIGGER users_touch BEFORE UPDATE ON public.users FOR EACH ROW EXECUTE FUNCTION public.touch()"), "trigger users_touch on users: before update for each row execute function touch()");
        let lines = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let made = lines(&[
            "table public.users",
            "column public.users.id integer",
            "trigger CREATE TRIGGER users_touch BEFORE UPDATE ON public.users FOR EACH ROW EXECUTE FUNCTION public.touch()",
            "view public.pro_users",
            "view public.pro_users as SELECT id FROM users;",
        ]);
        assert_eq!(
            changed_lines(&[], &made, Dialect::Postgres),
            ["  + table users", "      id integer", "      trigger users_touch: before update for each row execute function touch()", "  + view pro_users as SELECT id FROM users;"]
        );
        // A trigger's table is the name after the ON outside quotes, whole.
        assert_eq!(
            readable("trigger CREATE TRIGGER t_upd BEFORE UPDATE OF \"stage ON decoy name\" ON public.\"order items\" FOR EACH ROW EXECUTE FUNCTION public.touch()"),
            "trigger t_upd on \"order items\": before update of \"stage ON decoy name\" for each row execute function touch()"
        );
        // A materialized view's definition takes its kind back.
        assert_eq!(changed_lines(&[], &lines(&["materialized view public.mat", "view public.mat as SELECT 1;"]), Dialect::Postgres), ["  + materialized view mat as SELECT 1;"]);
        // A function whose body alone changed reads as changed in place, here and in a headed diff.
        let one = lines(&["function CREATE OR REPLACE FUNCTION public.rate()\n RETURNS integer\n LANGUAGE sql\nAS $function$ select 1 $function$"]);
        let two = lines(&["function CREATE OR REPLACE FUNCTION public.rate()\n RETURNS integer\n LANGUAGE sql\nAS $function$ select 2 $function$"]);
        assert_eq!(changed_lines(&one, &two, Dialect::Postgres), ["  ~ function rate() returns integer"]);
        // Two overloads are two functions, even where an argument's default holds a line break.
        let integer = lines(&["function CREATE OR REPLACE FUNCTION public.rate(a text DEFAULT 'hel\nlo'::text, b integer)\n RETURNS integer\n LANGUAGE sql\nAS $function$ select 1 $function$"]);
        let boolean = lines(&["function CREATE OR REPLACE FUNCTION public.rate(a text DEFAULT 'hel\nlo'::text, b boolean)\n RETURNS integer\n LANGUAGE sql\nAS $function$ select 1 $function$"]);
        assert_eq!(
            changed_lines(&integer, &boolean, Dialect::Postgres),
            ["  + function rate(a text default 'hel↵lo'::text, b boolean) returns integer", "  - function rate(a text default 'hel↵lo'::text, b integer) returns integer"]
        );
        // A name with a line break and one with a backslash and an n are two functions, and read so.
        let broken = lines(&["function CREATE OR REPLACE FUNCTION public.\"line\nbreak\"()\n RETURNS integer\n LANGUAGE sql\nAS $function$ select 1 $function$"]);
        let escaped = lines(&["function CREATE OR REPLACE FUNCTION public.\"line\\nbreak\"()\n RETURNS integer\n LANGUAGE sql\nAS $function$ select 1 $function$"]);
        assert_eq!(changed_lines(&escaped, &broken, Dialect::Postgres), ["  + function \"line↵break\"() returns integer", "  - function \"line\\nbreak\"() returns integer"]);
        assert_eq!(describe_diff("this database", &one, "a fresh database", &two), ["", "    --- this database", "    +++ a fresh database", "    ~ function rate() returns integer", ""]);
    }

    #[test]
    fn a_postgres_constraint_named_like_a_keyword_keeps_its_name() {
        assert_eq!(readable("constraint public.t check CHECK ((id > 0))"), "constraint check on t: check ((id > 0))");
        assert_eq!(readable("constraint t check (id > 0)"), "constraint on t: check (id > 0)");
    }

    #[test]
    #[cfg(feature = "cli")]
    fn a_rename_reads_as_one_line_changed_in_place() {
        assert_eq!(inline_diff("column posts.title character varying(100)", "column posts.heading character varying(200)"), "column posts.[-title-]{+heading+} character varying([-100-]{+200+})");
        assert_eq!(inline_diff("column users.bio character varying(200)", "column users.about text"), "column users.[-bio-]{+about+} [-character varying(200)-]{+text+}");
        assert_eq!(inline_diff("column users.score bigint", "column users.label varchar(200) collate utf8mb4_0900_ai_ci"), "column users.[-score-]{+label+} [-bigint-]{+varchar(200) collate utf8mb4_0900_ai_ci+}");
        assert_eq!(inline_diff("column \"My Users\".\"Full Name\" text", "column \"My Users\".\"Display Name\" text"), "column \"My Users\".\"[-Full-]{+Display+} Name\" text");
        assert_eq!(inline_diff("table orders", "table invoices"), "table [-orders-]{+invoices+}");
        // A long definition the same on both sides, and one replaced whole past the bound.
        let long = format!("column t.c jsonb default '{}'", "x ".repeat(5000));
        assert_eq!(inline_diff(&long, &long.replacen("t.c", "t.d", 1)), long.replacen("t.c", "t.[-c-]{+d+}", 1));
        let (a, b) = (format!("default {}", "a ".repeat(200)), format!("default {}", "b ".repeat(200)));
        assert_eq!(word_diff(&a, &b), format!("default [-{}-]{{+{}+}} ", "a ".repeat(200).trim_end(), "b ".repeat(200).trim_end()));
        let lines = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // A column renamed, beside one dropped and one added.
        let before = lines(&["table public.posts", "column public.posts.title character varying(100)", "column public.posts.body text"]);
        let after = lines(&["table public.posts", "column public.posts.heading character varying(200)", "column public.posts.summary text"]);
        let steps = Steps { dialect: Dialect::Postgres, before: &before, moved: None, renamed: None, columns: None, after: &after };
        let columns = vec![("column public.posts.title".to_string(), "column public.posts.heading".to_string())];
        assert_eq!(summary_lines(&steps, &[], &columns), ["  ~ column posts.[-title-]{+heading+} character varying([-100-]{+200+})", "  + column posts.summary text", "  - column posts.body text"]);
        // A column renamed first: what only took its new name is left out, and what changes besides
        // is listed as it was before and is after.
        let before = lines(&["table public.posts", "column public.posts.pid integer not null", "constraint public.posts pid_pos CHECK ((pid > 0))", "index CREATE INDEX posts_pid ON public.posts USING btree (pid)", "index CREATE INDEX posts_pid_title ON public.posts USING btree (pid, title)", "view public.v as SELECT pid FROM posts;"]);
        let renamed = lines(&["table public.posts", "column public.posts.post_id integer not null", "constraint public.posts pid_pos CHECK ((post_id > 0))", "index CREATE INDEX posts_pid ON public.posts USING btree (post_id)", "index CREATE INDEX posts_pid_title ON public.posts USING btree (post_id, title)", "view public.v as SELECT post_id AS pid FROM posts;"]);
        let mut after = renamed.clone();
        after[4] = "index CREATE INDEX posts_pid_title ON public.posts USING btree (post_id DESC, title)".to_string();
        let steps = Steps { dialect: Dialect::Postgres, before: &before, moved: None, renamed: None, columns: Some(&renamed), after: &after };
        let columns = vec![("column public.posts.pid".to_string(), "column public.posts.post_id".to_string())];
        let shown = summary_lines(&steps, &[], &columns);
        assert_eq!(shown[0], "  ~ column posts.[-pid-]{+post_id+} integer not null");
        assert_eq!(shown.len(), 3, "{shown:?}");
        assert!(shown[1..].iter().all(|l| l.contains("posts_pid_title")), "{shown:?}");
        assert_eq!(swap_word("(pid, pid_x, xpid) pid", "pid", "post_id"), "(post_id, pid_x, xpid) post_id");
        // A table renamed: what only took its new name is left out, a default that changes besides is
        // listed as it was before and is after, and a column of it renamed is a rename under the
        // table's new name.
        let before = lines(&["table public.orders", "column public.orders.id integer not null", "column public.orders.title text", "column public.orders.kind text default 'orders'::text", "constraint public.orders orders_pkey PRIMARY KEY (id)"]);
        let renamed = lines(&["table public.invoices", "column public.invoices.id integer not null", "column public.invoices.title text", "column public.invoices.kind text default 'orders'::text", "constraint public.invoices invoices_pkey PRIMARY KEY (id)"]);
        let after = lines(&["table public.invoices", "column public.invoices.id integer not null", "column public.invoices.heading text", "column public.invoices.kind text default 'invoices'::text", "constraint public.invoices invoices_pkey PRIMARY KEY (id)"]);
        let steps = Steps { dialect: Dialect::Postgres, before: &before, moved: None, renamed: Some(&renamed), columns: None, after: &after };
        let tables = vec![("table public.orders".to_string(), "table public.invoices".to_string())];
        let columns = vec![("column public.invoices.title".to_string(), "column public.invoices.heading".to_string())];
        assert_eq!(summary_lines(&steps, &tables, &columns), ["  ~ table [-orders-]{+invoices+}", "  ~ column invoices.[-title-]{+heading+} text", "  + column invoices.kind text default 'invoices'::text", "  - column orders.kind text default 'orders'::text"]);
        // A column named after another table keeps its identity, and a foreign key between two
        // renamed tables is left out.
        let before = lines(&["table public.alpha", "column public.alpha.alpha_code integer", "column public.alpha.beta_code integer"]);
        let renamed = lines(&["table public.beta", "column public.beta.alpha_code integer", "column public.beta.beta_code integer"]);
        let after = lines(&["table public.beta", "column public.beta.beta_code integer"]);
        let steps = Steps { dialect: Dialect::Postgres, before: &before, moved: None, renamed: Some(&renamed), columns: None, after: &after };
        let tables = vec![("table public.alpha".to_string(), "table public.beta".to_string())];
        assert_eq!(summary_lines(&steps, &tables, &[]), ["  ~ table [-alpha-]{+beta+}", "  - column alpha.alpha_code integer"]);
        let before = lines(&["table public.orders", "table public.clients", "constraint public.orders fk_client FOREIGN KEY (client_id) REFERENCES public.clients(id)"]);
        let after = lines(&["table public.invoices", "table public.customers", "constraint public.invoices fk_client FOREIGN KEY (client_id) REFERENCES public.customers(id)"]);
        let steps = Steps { dialect: Dialect::Postgres, before: &before, moved: None, renamed: Some(&after), columns: None, after: &after };
        let tables = vec![("table public.orders".to_string(), "table public.invoices".to_string()), ("table public.clients".to_string(), "table public.customers".to_string())];
        assert_eq!(summary_lines(&steps, &tables, &[]), ["  ~ table [-orders-]{+invoices+}", "  ~ table [-clients-]{+customers+}"]);
        // A column whose name starts with another renamed table's is no reference to it.
        let before = lines(&["table public.orders", "table public.clients", "constraint public.orders fk_account FOREIGN KEY (clients_id) REFERENCES public.accounts(id)"]);
        let after = lines(&["table public.invoices", "table public.customers", "constraint public.invoices fk_account FOREIGN KEY (clients_id) REFERENCES public.accounts(id)"]);
        let steps = Steps { dialect: Dialect::Postgres, before: &before, moved: None, renamed: Some(&after), columns: None, after: &after };
        let tables = vec![("table public.orders".to_string(), "table public.invoices".to_string()), ("table public.clients".to_string(), "table public.customers".to_string())];
        assert_eq!(summary_lines(&steps, &tables, &[]), ["  ~ table [-orders-]{+invoices+}", "  ~ table [-clients-]{+customers+}"]);
        // A view that names a renamed table, as Postgres rewrites it, is the same view.
        let before = lines(&["table public.orders", "view public.order_count as SELECT count(orders.id) AS count FROM (orders JOIN details ON ((orders.id = details.id)));"]);
        let after = lines(&["table public.invoices", "view public.order_count as SELECT count(invoices.id) AS count FROM (invoices JOIN details ON ((invoices.id = details.id)));"]);
        let steps = Steps { dialect: Dialect::Postgres, before: &before, moved: None, renamed: Some(&after), columns: None, after: &after };
        let tables = vec![("table public.orders".to_string(), "table public.invoices".to_string())];
        assert_eq!(summary_lines(&steps, &tables, &[]), ["  ~ table [-orders-]{+invoices+}"]);
        // A view whose quoted name holds a renamed table's name is another view still.
        let before = lines(&["table public.alpha", "view public.omega as SELECT 1 AS n;", "view public.\"public.alpha\" as SELECT omega.n FROM omega;", "view public.\"public.omega\" as SELECT alpha.id FROM alpha;"]);
        let after = lines(&["table public.omega", "view public.\"public.omega\" as SELECT omega.id FROM omega;"]);
        let steps = Steps { dialect: Dialect::Postgres, before: &before, moved: None, renamed: Some(&after), columns: None, after: &after };
        let tables = vec![("table public.alpha".to_string(), "table public.omega".to_string())];
        assert_eq!(summary_lines(&steps, &tables, &[]), ["  ~ table [-alpha-]{+omega+}", "  - view omega as SELECT 1 AS n;", "  - view \"public.alpha\" as SELECT omega.n FROM omega;"]);
        assert_eq!(renamed_in("view public.\"public.alpha\" as SELECT 1 FROM public.alpha", &[("public.alpha".to_string(), "public.omega".to_string())], true), "view public.\"public.alpha\" as SELECT 1 FROM public.omega");
        // A function's body is its text as written, while its signature follows the rename.
        let names = [("public.alpha".to_string(), "public.omega".to_string())];
        assert_eq!(renamed_in("function public.old_size(arg public.alpha) AS $function$ BEGIN RETURN pg_relation_size('public.alpha'::regclass); END; $function$", &names, true), "function public.old_size(arg public.omega) AS $function$ BEGIN RETURN pg_relation_size('public.alpha'::regclass); END; $function$");
        assert_eq!(renamed_in("policy public.notes p USING (($1 = 'public.alpha'::regclass))", &names, true), "policy public.notes p USING (($1 = 'public.omega'::regclass))");
        // Elsewhere a dollar sign is part of a name.
        assert_eq!(renamed_in("constraint notes $link$ FOREIGN KEY (alpha_id) REFERENCES alpha (id)", &[("alpha".to_string(), "`sales orders`".to_string())], false), "constraint notes $link$ FOREIGN KEY (alpha_id) REFERENCES `sales orders` (id)");
        // A relation constant follows the rename.
        assert_eq!(renamed_in("policy public.notes visible USING ((pg_relation_size('public.alpha'::regclass) > 0))", &[("public.alpha".to_string(), "public.omega".to_string())], true), "policy public.notes visible USING ((pg_relation_size('public.omega'::regclass) > 0))");
        // A string is left as it is, and a double quote inside one quotes no name.
        assert_eq!(renamed_in("policy public.alpha visible USING ((body = '\"public.alpha\"'::text))", &[("public.alpha".to_string(), "public.omega".to_string())], true), "policy public.omega visible USING ((body = '\"public.alpha\"'::text))");
        assert_eq!(renamed_in("policy public.notes visible USING ((body = '\"'::text) OR (EXISTS (SELECT 1 FROM public.alpha)))", &[("public.alpha".to_string(), "public.omega".to_string())], true), "policy public.notes visible USING ((body = '\"'::text) OR (EXISTS (SELECT 1 FROM public.omega)))");
        // Two tables of one name in two schemas, each renamed its own way, with a column of one.
        let before = lines(&["table east.orders", "table west.orders", "column west.orders.title text", "constraint west.orders orders_pkey PRIMARY KEY (id)"]);
        let renamed = lines(&["table east.invoices", "table west.shipments", "column west.shipments.title text", "constraint west.shipments shipments_pkey PRIMARY KEY (id)"]);
        let after = lines(&["table east.invoices", "table west.shipments", "column west.shipments.heading text", "constraint west.shipments shipments_pkey PRIMARY KEY (id)"]);
        let steps = Steps { dialect: Dialect::Postgres, before: &before, moved: None, renamed: Some(&renamed), columns: None, after: &after };
        let tables = vec![("table east.orders".to_string(), "table east.invoices".to_string()), ("table west.orders".to_string(), "table west.shipments".to_string())];
        let columns = vec![("column west.shipments.title".to_string(), "column west.shipments.heading".to_string())];
        assert_eq!(summary_lines(&steps, &tables, &columns), ["  ~ table east.[-orders-]{+invoices+}", "  ~ table west.[-orders-]{+shipments+}", "  ~ column west.shipments.[-title-]{+heading+} text"]);
        // A view the table rename drops, where the table takes its name, stays listed.
        let before = lines(&["table public.t_data", "column public.t_data.id integer", "view public.data select id from public.t_data", "view public.q select id from public.data"]);
        let renamed = lines(&["table public.data", "column public.data.id integer"]);
        let steps = Steps { dialect: Dialect::Postgres, before: &before, moved: None, renamed: Some(&renamed), columns: None, after: &renamed };
        let tables = vec![("table public.t_data".to_string(), "table public.data".to_string())];
        assert_eq!(summary_lines(&steps, &tables, &[]), ["  ~ table [-t_data-]{+data+}", "  - view data select id from t_data", "  - view q select id from data"]);
        // A table moved to another schema shows as the move's endpoints, never a line in between.
        let before = lines(&["table public.orders", "column public.orders.status text default 'orders'::text"]);
        let moved = lines(&["table archive.orders", "column archive.orders.status text default 'orders'::text"]);
        let after = lines(&["table archive.orders", "column archive.orders.status text default 'archived'::text"]);
        let steps = Steps { dialect: Dialect::Postgres, before: &before, moved: Some(&moved), renamed: None, columns: None, after: &after };
        assert_eq!(summary_lines(&steps, &[], &[]), ["  + table archive.orders", "      status text default 'archived'::text", "  - table orders", "      status text default 'orders'::text"]);
    }

    #[test]
    fn a_new_types_default_grant_is_left_out() {
        let lines = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let made = lines(&["column public.posts.status public.post_status not null default 'draft'::public.post_status", "enum public.post_status (\"draft\", \"published\")", "grant type public.post_status PUBLIC:USAGE"]);
        assert_eq!(without_default_grants(&made), made[..2]);
        // On a type both sides have, a grant is a change.
        let granted = lines(&["grant type public.post_status PUBLIC:USAGE"]);
        assert_eq!(without_default_grants(&granted), granted);
        assert_eq!(readable(&made[0]), "column posts.status post_status not null default 'draft'::post_status");
    }

    #[test]
    fn an_index_reads_the_same_on_every_database() {
        // As each database's catalog writes it.
        assert_eq!(readable("index CREATE INDEX bookmarks_title ON public.bookmarks USING btree (title)"), "index bookmarks_title on bookmarks (title)");
        assert_eq!(readable("index orders.orders_qty BTREE (qty)"), "index orders_qty on orders (qty)");
        assert_eq!(readable("index orders.orders_ref unique HASH invisible (ref, qty desc)"), "unique index orders_ref on orders using hash (ref, qty desc) invisible");
        assert_eq!(readable("index users_name3 create index users_name3 on users (name)"), "index users_name3 on users (name)");
        assert_eq!(readable("index users_email create unique index if not exists users_email on users (email)"), "unique index users_email on users (email)");
        // A view and a trigger name themselves once too.
        assert_eq!(readable("view v_users create view v_users as select id from users"), "view v_users as select id from users");
        assert_eq!(readable("trigger t create trigger t after insert on users begin select 1; end"), "trigger t after insert on users begin select 1; end");
        // What is not an index's own line is left as it is.
        assert_eq!(readable("index options orders.orders_qty key_block_size=8"), "index options orders.orders_qty key_block_size=8");
    }

    #[test]
    fn a_platforms_column_stamp_leaves_the_dump() {
        // As pg_dump writes a Neon database's columns: each with Neon's own option, which plain Postgres refuses.
        assert_eq!(without_platform_column_options("ALTER TABLE ONLY public.users ALTER COLUMN id SET (pg_attver=42164944)"), None);
        assert_eq!(without_platform_column_options("ALTER TABLE ONLY public.users ALTER COLUMN id SET (n_distinct=-1, pg_attver=42164944)").as_deref(), Some("ALTER TABLE ONLY public.users ALTER COLUMN id SET (n_distinct=-1)"));
        assert_eq!(without_platform_column_options("ALTER TABLE ONLY public.users ALTER COLUMN id SET (n_distinct=-1)").as_deref(), Some("ALTER TABLE ONLY public.users ALTER COLUMN id SET (n_distinct=-1)"));
        assert_eq!(without_platform_column_options("ALTER TABLE ONLY public.users ALTER COLUMN email SET STATISTICS 500").as_deref(), Some("ALTER TABLE ONLY public.users ALTER COLUMN email SET STATISTICS 500"));
    }

    #[test]
    fn a_dump_leaves_out_what_the_platform_owns() {
        // As pg_dump writes a Tiger Cloud database's public schema: a function of the platform's
        // superuser, with its privileges, and its default privileges, among the project's.
        let dump = "SET check_function_bodies = false;\n\n--\n-- Name: create_playground(regclass, boolean); Type: FUNCTION; Schema: public; Owner: postgres\n--\n\nCREATE FUNCTION public.create_playground(src regclass, b boolean) RETURNS integer\n    LANGUAGE sql\n    AS $$select 1$$;\n\n\nALTER FUNCTION public.create_playground(src regclass, b boolean) OWNER TO postgres;\n\n--\n-- Name: users; Type: TABLE; Schema: public; Owner: tsdbadmin\n--\n\nCREATE TABLE public.users (\n    id integer NOT NULL\n);\n\n\nALTER TABLE public.users OWNER TO tsdbadmin;\n\n--\n-- Name: FUNCTION create_playground(src regclass, b boolean); Type: ACL; Schema: public; Owner: postgres\n--\n\nGRANT ALL ON FUNCTION public.create_playground(src regclass, b boolean) TO tsdbadmin;\n\n--\n-- Name: DEFAULT PRIVILEGES FOR SCHEMAS; Type: DEFAULT ACL; Schema: -; Owner: postgres\n--\n\nALTER DEFAULT PRIVILEGES FOR ROLE postgres GRANT USAGE ON SCHEMAS TO tsdbadmin;\n\n\n--\n-- PostgreSQL database dump complete\n--\n";
        let public = ["public".to_string()];
        let kept = without_entries_of(dump, &["postgres".to_string()], &public);
        assert_eq!(kept, "SET check_function_bodies = false;\n\n--\n-- Name: users; Type: TABLE; Schema: public; Owner: tsdbadmin\n--\n\nCREATE TABLE public.users (\n    id integer NOT NULL\n);\n\n\nALTER TABLE public.users OWNER TO tsdbadmin;\n\n");
        // With no platform's role among the owners, the dump is as written.
        assert_eq!(without_entries_of(dump, &["supabase_admin".to_string()], &public), dump);
    }

    #[test]
    #[cfg(feature = "cli")]
    fn a_url_as_another_user_keeps_its_server_and_drops_the_password() {
        assert_eq!(as_user("postgres://app:secret@db.example.com:5432/application?sslmode=require&password=x", "postgres"), "postgres://postgres@db.example.com:5432/application?sslmode=require");
        assert_eq!(as_user("postgresql://localhost/application", "postgres"), "postgresql://postgres@localhost/application");
        assert_eq!(as_user("postgres://app@localhost", "the owner"), "postgres://the%20owner@localhost");
    }

    #[test]
    fn a_dump_keeps_a_projects_relations_whoever_owns_them() {
        // A table a superuser made in `public`, with its key, its privileges and a column's comment,
        // beside the superuser's function: the table's entries stay, and the function's go.
        let table = "--\n-- Name: customers; Type: TABLE; Schema: public; Owner: postgres\n--\n\nCREATE TABLE public.customers (\n    id integer NOT NULL\n);\n\n\nALTER TABLE public.customers OWNER TO postgres;\n\n--\n-- Name: COLUMN customers.id; Type: COMMENT; Schema: public; Owner: postgres\n--\n\nCOMMENT ON COLUMN public.customers.id IS 'the key';\n\n--\n-- Name: customers customers_pkey; Type: CONSTRAINT; Schema: public; Owner: postgres\n--\n\nALTER TABLE ONLY public.customers\n    ADD CONSTRAINT customers_pkey PRIMARY KEY (id);\n\n--\n-- Name: TABLE customers; Type: ACL; Schema: public; Owner: postgres\n--\n\nGRANT SELECT ON TABLE public.customers TO app;\n\n";
        let function = "--\n-- Name: helper(); Type: FUNCTION; Schema: public; Owner: postgres\n--\n\nCREATE FUNCTION public.helper() RETURNS integer\n    LANGUAGE sql\n    AS $$select 1$$;\n\n--\n-- Name: FUNCTION helper(); Type: ACL; Schema: public; Owner: postgres\n--\n\nGRANT ALL ON FUNCTION public.helper() TO app;\n\n";
        let dump = format!("SET check_function_bodies = false;\n\n{function}{table}");
        let owners = ["postgres".to_string()];
        assert_eq!(without_entries_of(&dump, &owners, &["public".to_string()]), format!("SET check_function_bodies = false;\n\n{table}"));
        // In a schema of the platform's, the table is the platform's too.
        assert_eq!(without_entries_of(&dump, &owners, &[]), "SET check_function_bodies = false;\n\n");
    }

    #[test]
    fn a_change_that_fills_existing_rows_is_told_by_its_words() {
        let fills = |sql: &str| split_statements(sql, Dialect::Postgres).iter().any(fills_existing);
        assert!(fills("alter table t add column v int default f()"));
        assert!(fills("alter table t add column v int default \"Backfill\"()"));
        assert!(fills("alter table t add column v int default public.f(1)"));
        assert!(fills("alter table t add column v int not null default (now())"));
        assert!(fills("alter table t add column v int generated always as (id * 2) stored"));
        assert!(fills("alter table t alter column n type bigint using n::bigint"));
        assert!(!fills("alter table t add column v int default 1"));
        assert!(!fills("alter table t add column v numeric(10, 2) default 0, add column w int default 2"));
        assert!(!fills("alter table t alter column n set default f()"));
        assert!(!fills("create table t (v int default f())"));
        assert!(fills("create index t_k on t (idx_key(id))"));
        assert!(fills("create index t_k on t using btree (lower(email))"));
        assert!(fills("create unique index t_k on t (id) where active(id)"));
        assert!(!fills("create index t_k on t using btree (id) include (email)"));
        assert!(!fills("create index concurrently if not exists t_k on public.t (id, email desc)"));
        assert!(!fills("create index t_k on t (id) where (id is not null)"));
        assert!(!fills("create index t_k on t (id) where id > 0 and (email is null or id < 9)"));
        assert!(!fills("alter table t add column v int default 1 check (v > 0)"));
        assert!(!fills("alter table t add column v int default 0 references r (id)"));
        assert!(fills("alter table t add column v int default public.include(1)"));
        assert!(fills("create index t_k on t (public.include(id))"));
        assert!(fills("create index t_k on t (id) where public.any(id)"));
        assert!(fills("create index t_k on t (include(id))"));
        assert!(!fills("create index t_k on t (id) where id is distinct from (1)"));
        assert!(fills("create index t_k on t (id) where id is distinct from (f(1))"));
        assert!(fills("create index t_k on t (similar(id))"));
        assert!(fills("alter table t add column v int default similar(1)"));
        assert!(!fills("create index t_k on t (id) where email similar to ('a%')"));
        assert!(!fills("create index t_k on t (id) where email like ('a%')"));
        assert!(!fills("create index t_k on t (id) where email not ilike ('a%')"));
        assert!(fills("create index t_k on t (id) where email like (f('a%'))"));
        assert!(fills("create index t_k on t (id) where like(id)"));
        assert!(fills("create index t_k on t (id) where id > 0 and ilike(email)"));
        assert!(!fills("create index t_k on t (id) where t.default like ('a%')"));
    }

    #[test]
    fn a_rebuilds_own_copy_of_rows_is_no_data_statement() {
        let sqlite = "create table \"new_posts\" (id integer primary key autoincrement, body text);\ninsert into \"new_posts\" (id, body) select id, body from posts;\ndelete from sqlite_sequence where name = 'new_posts';\ninsert into sqlite_sequence (name, seq) select 'new_posts', seq from sqlite_sequence where name = 'posts';\ndrop table posts;\nalter table \"new_posts\" rename to posts;";
        assert!(data_statements(sqlite, Dialect::Sqlite).is_empty());
        // Rows the file writes itself: a copy that keeps some rows, one into a table it doesn't make, or from one it keeps.
        assert_eq!(data_statements(&sqlite.replace("from posts;", "from posts where id > 1;"), Dialect::Sqlite).len(), 1);
        assert_eq!(data_statements(&sqlite.replace("select id, body from", "select id, upper(body) from"), Dialect::Sqlite).len(), 1);
        assert_eq!(data_statements(&sqlite.replace("select id, body from", "select id, current_timestamp from"), Dialect::Sqlite).len(), 1);
        assert_eq!(data_statements(&sqlite.replace("select id, body from", "select id, null from"), Dialect::Sqlite).len(), 1);
        // A quoted name is a column's on Postgres, which refuses one no table has (SQLite would read it as a string).
        assert!(data_statements("create table new_posts (id int, body text);\ninsert into new_posts (id, body) select id, \"null\" from posts;\ndrop table posts;\nalter table new_posts rename to posts;", Dialect::Postgres).is_empty());
        // A source named with its schema.
        assert!(data_statements(&sqlite.replace("from posts;", "from main.posts;").replace("drop table posts;", "drop table main.posts;"), Dialect::Sqlite).is_empty());
        // SQLite reads `true` as the name of a column of that name, which its rebuild writes bare.
        let sqlite_true = "create table \"new_t\" (id integer primary key, true integer);\ninsert into \"new_t\" (id, true) select id, true from t;\ndrop table t;\nalter table \"new_t\" rename to t;";
        assert!(data_statements(sqlite_true, Dialect::Sqlite).is_empty());
        assert_eq!(data_statements(&sqlite_true.replace("(id, true) select id, true", "(id, x) select id, true"), Dialect::Sqlite).len(), 1);
        assert_eq!(data_statements("create table archive (id integer); insert into archive (id) select id from posts;", Dialect::Sqlite).len(), 1);
        assert_eq!(data_statements("insert into posts (id) select id from old; drop table old;", Dialect::Sqlite).len(), 1);
        assert_eq!(data_statements("update posts set body = '';", Dialect::Sqlite).len(), 1);
        assert_eq!(data_statements("load data infile 'a.csv' into table t;", Dialect::Mysql).len(), 1);
    }

    #[test]
    fn a_refusal_gives_json_its_code_and_each_part() {
        set_color(false);
        let p = Problem::new("database-changed", "This database changed since Reylo last ran on it")
            .lines(["  + now: column users.hotfix bigint".to_string()])
            .sql("alter table users add column hotfix bigint;")
            .note("A note.")
            .fix("To keep it", &["reylo migrate new <name>", "reylo migrate history push"]);
        let j = p.json();
        assert_eq!(j["code"], "database-changed");
        assert_eq!(j["title"], "This database changed since Reylo last ran on it");
        assert_eq!(j["detail"], serde_json::json!(["+ now: column users.hotfix bigint"]));
        assert_eq!(j["sql"], "alter table users add column hotfix bigint;");
        assert_eq!(j["notes"], serde_json::json!(["A note."]));
        assert_eq!(j["fixes"], serde_json::json!([{ "what": "To keep it", "commands": ["reylo migrate new <name>", "reylo migrate history push"] }]));
    }

    #[test]
    fn init_sql_comes_first_in_name_order() {
        let mut names = vec!["20261109120000_b.sql", "init.sql", "20261108120000_a.sql"];
        names.sort_by(|a, b| name_order(a).cmp(&name_order(b)));
        assert_eq!(names, ["init.sql", "20261108120000_a.sql", "20261109120000_b.sql"]);
    }
}

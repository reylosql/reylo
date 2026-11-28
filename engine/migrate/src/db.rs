//! The databases `reylo migrate` works on: the target, through the connection code the type check
//! uses (`reylo-engine`), and the shadows it builds beside it. A shadow is a scratch database
//! on the target's own server (`create database`), so it has the target's engine, version and
//! extensions; SQLite's live in memory, a libSQL server's (Turso's) among them. The connection that creates a shadow holds a session lock
//! named after it for the whole run, and the lock dies with the session, so a run that is killed
//! leaves a shadow nobody holds, which the next run drops before it builds its own.

use crate::{Dialect, Error};
#[cfg(feature = "cli")]
use mysql_async::prelude::Queryable;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;

/// One row, every value as text (Postgres's booleans as `t` and `f`), as its columns name them.
#[derive(Clone, Debug, Default)]
pub struct Row {
    columns: Vec<String>,
    values: Vec<Option<String>>,
}

impl Row {
    /// A row of `values`, in the order `columns` names them; a host's [`Session`] builds its rows so.
    pub fn new(columns: Vec<String>, values: Vec<Option<String>>) -> Row {
        Row { columns, values }
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        let at = self.columns.iter().position(|c| c == name)?;
        self.values[at].as_deref()
    }
    /// The value, or "" where it is null.
    pub fn text(&self, name: &str) -> &str {
        self.get(name).unwrap_or_default()
    }
    pub fn flag(&self, name: &str) -> bool {
        matches!(self.get(name), Some("t" | "true" | "1"))
    }
    /// The first column's value.
    pub fn first(&self) -> Option<&str> {
        self.values.first().and_then(|v| v.as_deref())
    }
}

/// What one statement returned: its columns, its first rows, every value as text as a [`Row`]'s is,
/// and how many rows it returned in all.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    pub total: usize,
}

impl Table {
    fn new(columns: Vec<String>) -> Table {
        Table { columns, ..Default::default() }
    }

    /// Counts a row, keeping it while fewer than `keep` are kept.
    fn add(&mut self, keep: usize, row: impl FnOnce() -> Vec<Option<String>>) {
        if self.rows.len() < keep {
            self.rows.push(row());
        }
        self.total += 1;
    }
}

pub enum Db {
    #[cfg(feature = "cli")]
    Postgres(tokio_postgres::Client),
    #[cfg(feature = "cli")]
    Mysql(mysql_async::Conn),
    #[cfg(feature = "cli")]
    Sqlite(rusqlite::Connection),
    /// A libSQL server's database, Turso Cloud's among them, over HTTP.
    #[cfg(feature = "cli")]
    Libsql(crate::libsql::Remote),
    /// A database on libSQL's own engine in this process: the development file of a project whose
    /// SQL is libSQL's own, and every shadow of a libSQL database.
    #[cfg(feature = "cli")]
    LibsqlLocal(crate::libsql_local::Local),
    /// A database the host holds the connection to, as a Rust app's own pool through
    /// `reylo::migrate!()`.
    Host(Box<dyn Session>),
}

/// A future a [`Session`] answers with.
pub type Answer<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>;

/// One connection a host holds to the database deploy and status run on: each statement run as
/// written, every value of a row as text (a boolean as `t` or `true`), as [`Db::rows`] reads them.
/// The connection is the same one for the whole run, since the run's lock is the session's.
pub trait Session: Send {
    fn dialect(&self) -> Dialect;
    /// The rows of the statements in `sql`.
    fn rows<'a>(&'a mut self, sql: &'a str) -> Answer<'a, Vec<Row>>;
    /// Runs `sql`: one statement or several.
    fn exec<'a>(&'a mut self, sql: &'a str) -> Answer<'a, ()>;
    /// Runs one statement with text parameters, `$1` on Postgres and `?` elsewhere.
    fn exec_with<'a>(&'a mut self, sql: &'a str, params: &'a [&'a str]) -> Answer<'a, ()>;
}

/// How a script failed: waiting for a lock past its limit, having changed nothing; otherwise, with
/// nothing of it left once its transaction rolled back; or with its change maybe in place, as a
/// statement run outside a transaction may leave it.
#[derive(Debug)]
pub(crate) enum Ran {
    Waited(Error),
    Failed(Error),
    Partly(Error),
    /// Failed as `Failed` does, on a row the table already held that breaks a constraint or a
    /// unique index the statement checked the rows against.
    Broke(Error, Breach),
}

/// What a statement found a row breaking: a constraint or a unique index, by the name the server
/// gave, and the table it is on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Breach {
    pub kind: Breaks,
    /// The constraint's name, or the unique index's, or for a NOT NULL with no name the column's.
    pub name: String,
    /// The table, schema-qualified where the server said (Postgres), else as the error named it.
    pub table: String,
    /// The column a NOT NULL is on, where the server said, unquoted.
    pub column: Option<String>,
    /// A query that lists the rows that break it, where the database could say.
    pub rows: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Breaks {
    #[default]
    Check,
    ForeignKey,
    Unique,
    NotNull,
}

/// A Postgres name in double quotes, which reads as the name whatever it is: a reserved word too.
pub(crate) fn pg_quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// What a Postgres error says of a row that breaks a constraint, by its SQLSTATE: a `check`, a
/// foreign key, a unique index or a NOT NULL. None for any other error.
#[cfg(feature = "cli")]
pub(crate) fn pg_breach(e: &tokio_postgres::Error) -> Option<Breach> {
    use tokio_postgres::error::SqlState;
    let d = e.as_db_error()?;
    let kind = match d.code() {
        c if *c == SqlState::CHECK_VIOLATION => Breaks::Check,
        c if *c == SqlState::FOREIGN_KEY_VIOLATION => Breaks::ForeignKey,
        c if *c == SqlState::UNIQUE_VIOLATION => Breaks::Unique,
        c if *c == SqlState::NOT_NULL_VIOLATION => Breaks::NotNull,
        _ => return None,
    };
    // A check, a foreign key and a unique index are named by their constraint; a NOT NULL may be
    // named by its column alone. A domain's check is named by neither as it is in the table.
    let name = match kind {
        Breaks::NotNull => d.constraint().or(d.column())?,
        _ => d.constraint()?,
    }
    .to_string();
    let table = match (d.schema(), d.table()) {
        (Some(schema), Some(table)) => format!("{}.{}", pg_quoted(schema), pg_quoted(table)),
        (None, Some(table)) => pg_quoted(table),
        _ => return None,
    };
    Some(Breach { kind, name, table, column: d.column().map(String::from), rows: None })
}

/// What a MySQL error says of a row that breaks a constraint: a `check` (3819), a foreign key
/// (1452) or a unique key (1062), its name read off the server's message. None for any other
/// error, and where the message doesn't read as MySQL's English one does.
#[cfg(feature = "cli")]
pub(crate) fn mysql_breach(code: u16, message: &str) -> Option<Breach> {
    let between = |from: &str, to: &str| -> Option<String> {
        let at = message.find(from)? + from.len();
        let end = message[at..].find(to)? + at;
        Some(message[at..end].to_string())
    };
    match code {
        // Check constraint 'people_mood' is violated.
        3819 => Some(Breach { kind: Breaks::Check, name: between("Check constraint '", "' is violated")?, ..Breach::default() }),
        // Cannot add or update a child row: a foreign key constraint fails (`db`.`#sql-…`, CONSTRAINT `people_team` FOREIGN KEY (`team_id`) REFERENCES `teams` (`id`))
        1452 => Some(Breach { kind: Breaks::ForeignKey, name: between("CONSTRAINT `", "` FOREIGN KEY")?, ..Breach::default() }),
        // Duplicate entry 'a' for key 'users.email'
        1062 => {
            // The last ` for key '…'`, since the duplicated value comes first and may hold the words.
            let key = message.rsplit_once(" for key '").map(|(_, k)| k.trim_end_matches(['\'', '.']).to_string())?;
            let (table, name) = key.rsplit_once('.').map(|(t, n)| (t.to_string(), n.to_string())).unwrap_or((String::new(), key));
            Some(Breach { kind: Breaks::Unique, name, table, ..Breach::default() })
        }
        _ => None,
    }
}

impl From<Error> for Ran {
    fn from(e: Error) -> Ran {
        Ran::Failed(e)
    }
}

/// The sql_mode a MySQL session whose `mode` isn't strict runs a table's change under: its own,
/// with strict mode on, so that the change fails on a value its new type can't hold (an enum's
/// label it no longer lists, a string too long, a number out of range) rather than storing another
/// in its place, as a change fails on a value it can't keep on Postgres. Strict mode also refuses
/// dates a session that isn't strict takes (a zero date, `2026-02-31`) and a division by zero,
/// where the table may hold such values the change copies: so the modes that refuse them are left
/// out, and invalid dates allowed, which keeps them as they are. None where `mode` is strict already.
pub fn strict_mode(mode: &str) -> Option<String> {
    let modes: Vec<&str> = mode.split(',').map(str::trim).filter(|m| !m.is_empty()).collect();
    if modes.iter().any(|m| matches!(*m, "STRICT_TRANS_TABLES" | "STRICT_ALL_TABLES" | "TRADITIONAL")) {
        return None;
    }
    let mut strict: Vec<&str> = modes.into_iter().filter(|m| !matches!(*m, "NO_ZERO_DATE" | "NO_ZERO_IN_DATE" | "ERROR_FOR_DIVISION_BY_ZERO" | "ALLOW_INVALID_DATES")).collect();
    strict.extend(["ALLOW_INVALID_DATES", "STRICT_ALL_TABLES"]);
    Some(strict.join(","))
}

/// The sql_mode a MySQL session in `mode` runs a table's change under: strict, as [`strict_mode`]
/// makes it, and with NO_AUTO_VALUE_ON_ZERO, so that a column made AUTO_INCREMENT keeps a 0 it holds
/// rather than taking the next value of its sequence in its place, which a foreign key added back
/// after can take without a word (a column that is AUTO_INCREMENT already keeps its 0 through a
/// copy). None where `mode` is both already.
pub fn table_change_mode(mode: &str) -> Option<String> {
    let base = strict_mode(mode).unwrap_or_else(|| mode.to_string());
    let mut modes: Vec<&str> = base.split(',').map(str::trim).filter(|m| !m.is_empty()).collect();
    if !modes.contains(&"NO_AUTO_VALUE_ON_ZERO") {
        modes.push("NO_AUTO_VALUE_ON_ZERO");
    }
    let changed = modes.join(",");
    (changed != mode).then_some(changed)
}

fn set_sql_mode(mode: &str) -> String {
    format!("set session sql_mode = '{}'", mode.replace('\\', "\\\\").replace('\'', "''"))
}

#[cfg(feature = "cli")]
pub(crate) fn pg_error(e: tokio_postgres::Error) -> Error {
    match e.as_db_error() {
        Some(d) => {
            let mut message = d.message().to_string();
            if let Some(detail) = d.detail() {
                message = format!("{message} ({detail})");
            }
            // The server's hint is often the way out, which a platform's own refusals give too.
            if let Some(hint) = d.hint() {
                message = format!("{message}. {hint}");
            }
            Error(message)
        }
        None => Error(e.to_string()),
    }
}

#[cfg(feature = "cli")]
pub(crate) fn mysql_error(e: mysql_async::Error) -> Error {
    match e {
        mysql_async::Error::Server(s) => Error(s.message),
        other => Error(other.to_string()),
    }
}

/// `name`, a libc locale, as `known` (a server's locale names) spells it: itself, else one that differs
/// only in case and in the encoding's punctuation (`en_US.utf8` and `en_US.UTF-8`), else none.
pub(crate) fn locale_spelled(name: &str, known: &[String]) -> Option<String> {
    let plain = |n: &str| match n.to_lowercase().split_once('.') {
        Some((language, rest)) => {
            let (encoding, modifier) = rest.split_once('@').map(|(e, m)| (e.to_string(), format!("@{m}"))).unwrap_or((rest.to_string(), String::new()));
            format!("{language}.{}{modifier}", encoding.replace(['-', '_'], ""))
        }
        None => n.to_lowercase(),
    };
    if known.iter().any(|k| k == name) {
        return Some(name.to_string());
    }
    known.iter().find(|k| plain(k) == plain(name)).cloned()
}

/// A server's refusal to create a shadow database, with what to do about it: each shadow is a new
/// database, so a role that may not create one needs a server where it may (PlanetScale, Vitess,
/// and some managed roles). A role that may (`may_create`) was refused by the server itself, which
/// can limit the databases made on it by name, as Tiger Cloud's does: only another server helps.
pub(crate) fn cannot_create(e: Error, may_create: bool) -> Error {
    if may_create {
        return Error(format!("{e}. The role may create databases, but this server refuses the shadow database each check makes beside yours: set shadow_url in .reylo.yaml to a server that takes it"));
    }
    Error(format!("{e}. Each shadow database is a new database on this server: give the role the right to create databases, or set shadow_url in .reylo.yaml to a server where it has it"))
}

/// Whether a MySQL server's error for `create database` is a refusal to make one, and if so
/// whether the role may create databases elsewhere on it: ER_DBACCESS_DENIED_ERROR and
/// ER_SPECIFIC_ACCESS_DENIED_ERROR are the role's missing CREATE; Vitess refuses every
/// `create database` itself, as VT12001 under the generic error code 1105.
#[cfg(feature = "cli")]
fn mysql_create_refused(code: u16, message: &str) -> Option<bool> {
    match code {
        1044 | 1227 => Some(false),
        1105 if message.starts_with("VT12001") && message.contains("create database") => Some(true),
        _ => None,
    }
}

#[cfg(feature = "cli")]
pub(crate) fn sqlite_error(e: rusqlite::Error) -> Error {
    Error(e.to_string())
}

/// The statements of a SQLite script, each ended where SQLite's own reading ends it: at the first `;`
/// after which the text read so far is complete (`sqlite3_complete`), so that no `;` or `end` inside
/// a trigger's body, a string or a comment ends one early. Those of nothing but comments before their
/// `;` are left out.
#[cfg(feature = "cli")]
pub(crate) fn sqlite_statements(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    for (at, _) in sql.match_indices(';') {
        let Ok(text) = std::ffi::CString::new(&sql[start..=at]) else { continue };
        // SAFETY: a NUL-terminated text that outlives the call, which only reads it.
        if unsafe { rusqlite::ffi::sqlite3_complete(text.as_ptr()) } != 0 {
            out.push(sql[start..=at].to_string());
            start = at + 1;
        }
    }
    out.push(sql[start..].to_string());
    out.retain(|s| !crate::sql::is_blank(s.strip_suffix(';').unwrap_or(s), Dialect::Sqlite));
    out
}

/// A SQLite value as text: an integer and a real as Rust writes them, a blob's bytes read as UTF-8.
#[cfg(feature = "cli")]
fn sqlite_text(value: rusqlite::types::ValueRef<'_>) -> Option<String> {
    use rusqlite::types::ValueRef::*;
    match value {
        Null => None,
        Integer(v) => Some(v.to_string()),
        Real(v) => Some(v.to_string()),
        Text(t) | Blob(t) => Some(String::from_utf8_lossy(t).into_owned()),
    }
}

#[cfg(feature = "cli")]
fn mysql_text(value: &mysql_async::Value) -> Option<String> {
    use mysql_async::Value::*;
    match value {
        NULL => None,
        Bytes(b) => Some(String::from_utf8_lossy(b).into_owned()),
        Int(i) => Some(i.to_string()),
        UInt(u) => Some(u.to_string()),
        Float(f) => Some(f.to_string()),
        Double(f) => Some(f.to_string()),
        other => Some(other.as_sql(true).trim_matches('\'').to_string()),
    }
}

/// Whether the SQLite file at `path` is libSQL's already, whatever the project's files say now (a
/// branch without the migration that made it): it holds a vector index's table, or its history
/// holds a migration of libSQL's own SQL, which only libSQL's engine replays.
#[cfg(feature = "cli")]
fn libsql_file(path: &str) -> bool {
    if path == ":memory:" || !Path::new(path).exists() {
        return false;
    }
    let Ok(db) = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI) else { return false };
    let vector = db.query_row("select count(*) from sqlite_schema where name = 'libsql_vector_meta_shadow'", [], |r| r.get::<_, i64>(0)).unwrap_or(0) > 0;
    vector || {
        let history = format!("select sql from {}", crate::catalog::HISTORY_TABLE);
        db.prepare(&history).and_then(|mut s| s.query_map([], |r| r.get::<_, Option<String>>(0)).map(|rows| rows.flatten().flatten().any(|sql| crate::libsql::uses_own_sql(&sql)))).unwrap_or(false)
    }
}

/// The path a SQLite URL names: `sqlite:` or `file:` before it, `:memory:` alone; a relative path
/// is the project's.
pub fn sqlite_path(url: &str, base: &Path) -> String {
    if url == ":memory:" || url == "file::memory:" {
        return ":memory:".into();
    }
    let raw = if let Some(rest) = url.strip_prefix("file://") {
        rest.to_string()
    } else {
        let lower = url.to_ascii_lowercase();
        let rest = if lower.starts_with("sqlite:") {
            &url[7..]
        } else if lower.starts_with("file:") {
            &url[5..]
        } else {
            url
        };
        rest.strip_prefix("//").unwrap_or(rest).to_string()
    };
    let raw = crate::percent_decode(&raw);
    if Path::new(&raw).is_absolute() {
        raw
    } else {
        base.join(raw).to_string_lossy().into_owned()
    }
}

/// The rows of `sql` over Postgres's simple protocol, every value as text.
#[cfg(feature = "cli")]
pub(crate) async fn pg_rows(client: &tokio_postgres::Client, sql: &str) -> Result<Vec<Row>, Error> {
    let mut out = Vec::new();
    for message in client.simple_query(sql).await.map_err(pg_error)? {
        if let tokio_postgres::SimpleQueryMessage::Row(row) = message {
            let columns = row.columns().iter().map(|c| c.name().to_string()).collect();
            let values = (0..row.len()).map(|i| row.get(i).map(String::from)).collect();
            out.push(Row { columns, values });
        }
    }
    Ok(out)
}

/// A connection to the Postgres database a URL names, as the engine opens one (its TLS, timeout and
/// failure messages), without the server version it reports.
#[cfg(feature = "cli")]
pub(crate) async fn open_postgres(url: &str) -> Result<tokio_postgres::Client, Error> {
    reylo_engine::postgres::open(url).await.map(|(client, _)| client).map_err(|failure| Error(failure.to_string()))
}

/// A connection to the MySQL database a URL names, as the engine opens one.
#[cfg(feature = "cli")]
pub(crate) async fn open_mysql(url: &str) -> Result<mysql_async::Conn, Error> {
    reylo_engine::mysql::open(url).await.map(|(conn, _)| conn).map_err(|failure| Error(failure.to_string()))
}

impl Db {
    #[cfg(feature = "cli")]
    pub async fn connect(url: &str, base: &Path) -> Result<Db, Error> {
        match Dialect::of_url(url)? {
            Dialect::Postgres => Ok(Db::Postgres(crate::db::open_postgres(url).await?)),
            Dialect::Mysql => Ok(Db::Mysql(crate::db::open_mysql(url).await?)),
            Dialect::Sqlite if crate::libsql::is_url(url) => Ok(Db::Libsql(crate::libsql::Remote::open(url)?)),
            Dialect::Sqlite => Ok(Db::Sqlite(rusqlite::Connection::open(sqlite_path(url, base)).map_err(sqlite_error)?)),
        }
    }

    /// A connection for reading only: a SQLite file is opened so that nothing can create or checkpoint it.
    #[cfg(feature = "cli")]
    pub async fn connect_read_only(url: &str, base: &Path) -> Result<Db, Error> {
        if Dialect::of_url(url)? == Dialect::Sqlite && !crate::libsql::is_url(url) {
            let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX | rusqlite::OpenFlags::SQLITE_OPEN_URI;
            return Ok(Db::Sqlite(rusqlite::Connection::open_with_flags(sqlite_path(url, base), flags).map_err(sqlite_error)?));
        }
        Db::connect(url, base).await
    }

    /// The database `opts` names, a SQLite file on libSQL's engine where the project runs there
    /// ([`crate::Options::libsql_engine`]).
    #[cfg(feature = "cli")]
    pub async fn open(opts: &crate::Options) -> Result<Db, Error> {
        if Dialect::of_url(&opts.url)? == Dialect::Sqlite && !crate::libsql::is_url(&opts.url) {
            let path = sqlite_path(&opts.url, &opts.root);
            if opts.libsql_engine() || libsql_file(&path) {
                return Ok(Db::LibsqlLocal(crate::libsql_local::Local::open(&path)?));
            }
        }
        Db::connect(&opts.url, &opts.root).await
    }

    /// Whether the database runs libSQL: a libSQL server's, or one on libSQL's engine here.
    pub fn is_libsql(&self) -> bool {
        #[cfg(feature = "cli")]
        if matches!(self, Db::Libsql(_) | Db::LibsqlLocal(_)) {
            return true;
        }
        false
    }

    pub fn dialect(&self) -> Dialect {
        match self {
            #[cfg(feature = "cli")]
            Db::Postgres(_) => Dialect::Postgres,
            #[cfg(feature = "cli")]
            Db::Mysql(_) => Dialect::Mysql,
            #[cfg(feature = "cli")]
            Db::Sqlite(_) | Db::Libsql(_) | Db::LibsqlLocal(_) => Dialect::Sqlite,
            Db::Host(session) => session.dialect(),
        }
    }

    /// The rows of the statements in `sql`, run as written (Postgres's simple protocol, MySQL's text
    /// protocol), every value as text.
    pub async fn rows(&mut self, sql: &str) -> Result<Vec<Row>, Error> {
        match self {
            Db::Host(session) => session.rows(sql).await,
            #[cfg(feature = "cli")]
            Db::Postgres(client) => pg_rows(client, sql).await,
            #[cfg(feature = "cli")]
            Db::Mysql(conn) => {
                let rows: Vec<mysql_async::Row> = conn.query(sql).await.map_err(mysql_error)?;
                Ok(rows
                    .into_iter()
                    .map(|row| {
                        let columns = row.columns_ref().iter().map(|c| c.name_str().into_owned()).collect();
                        let values = (0..row.len()).map(|i| row.as_ref(i).and_then(mysql_text)).collect();
                        Row { columns, values }
                    })
                    .collect())
            }
            #[cfg(feature = "cli")]
            Db::Sqlite(conn) => {
                let mut statement = conn.prepare(sql).map_err(sqlite_error)?;
                let columns: Vec<String> = statement.column_names().into_iter().map(String::from).collect();
                let count = columns.len();
                let rows = statement
                    .query_map([], |row| {
                        (0..count)
                            .map(|i| row.get_ref(i).map(sqlite_text))
                            .collect::<rusqlite::Result<Vec<_>>>()
                    })
                    .map_err(sqlite_error)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(sqlite_error)?;
                Ok(rows.into_iter().map(|values| Row { columns: columns.clone(), values }).collect())
            }
            #[cfg(feature = "cli")]
            Db::Libsql(remote) => remote.rows(sql),
            #[cfg(feature = "cli")]
            Db::LibsqlLocal(local) => local.rows(sql),
        }
    }

    /// Runs `sql` as written: one statement or several.
    pub async fn exec(&mut self, sql: &str) -> Result<(), Error> {
        match self {
            Db::Host(session) => session.exec(sql).await,
            #[cfg(feature = "cli")]
            Db::Postgres(client) => client.batch_execute(sql).await.map_err(pg_error),
            #[cfg(feature = "cli")]
            Db::Mysql(conn) => conn.query_drop(sql).await.map_err(mysql_error),
            #[cfg(feature = "cli")]
            Db::Sqlite(conn) => conn.execute_batch(sql).map_err(sqlite_error),
            #[cfg(feature = "cli")]
            Db::Libsql(remote) => remote.exec(sql),
            #[cfg(feature = "cli")]
            Db::LibsqlLocal(local) => local.exec(sql),
        }
    }

    /// Runs a script of statements: on MySQL one at a time, since a server that takes several in
    /// one query reads them as one batch; elsewhere as written.
    pub async fn script(&mut self, sql: &str) -> Result<(), Error> {
        if self.dialect() != Dialect::Mysql {
            return self.exec(sql).await;
        }
        let mut locked = false;
        for statement in crate::sql::split_statements(sql, Dialect::Mysql) {
            let restore = if crate::sql::alters_table(&statement) { self.make_strict().await? } else { None };
            let ran = self.exec(&statement.text).await;
            // The session's own mode is set back whether the change ran or failed.
            let back = match restore {
                Some(mode) => self.exec(&set_sql_mode(&mode)).await,
                None => Ok(()),
            };
            if let Err(e) = ran.and(back) {
                self.unlock(locked).await;
                return Err(e);
            }
            locked = (locked || crate::sql::locks_tables(&statement)) && !crate::sql::unlocks_tables(&statement);
        }
        Ok(())
    }

    /// Releases the tables a MySQL script locked (`lock tables`) and failed before it unlocked them,
    /// so that the session can go on: it reaches no other table while it holds them.
    async fn unlock(&mut self, locked: bool) {
        if locked {
            let _ = self.exec("unlock tables").await;
        }
    }

    /// Sets a MySQL session to [`table_change_mode`] for a table's change, and gives its own mode, to
    /// set back after; None where its mode is that already.
    async fn make_strict(&mut self) -> Result<Option<String>, Error> {
        let mode = self.rows("select @@session.sql_mode as m").await?.first().map(|r| r.text("m").to_string()).unwrap_or_default();
        let Some(changed) = table_change_mode(&mode) else { return Ok(None) };
        self.exec(&set_sql_mode(&changed)).await?;
        Ok(Some(mode))
    }

    /// Runs a script as [`Db::script`] does, and gives what each statement that returns rows returned,
    /// in order: a `select`'s, or a `returning` clause's, its columns kept where it returned no row,
    /// and of its rows the first `keep` and their count. Postgres runs it as one simple query, as
    /// `script` does; elsewhere each statement runs in turn, a libSQL script's split where SQLite's
    /// own reading ends each statement.
    #[cfg(feature = "cli")]
    pub async fn script_tables(&mut self, sql: &str, keep: usize) -> Result<Vec<Table>, Error> {
        if let Db::Host(_) = self {
            self.script(sql).await?;
            return Ok(vec![]);
        }
        let mut tables: Vec<Table> = Vec::new();
        match self {
            Db::Postgres(client) => {
                use futures_util::StreamExt;
                let mut messages = std::pin::pin!(client.simple_query_raw(sql).await.map_err(pg_error)?);
                while let Some(message) = messages.next().await {
                    match message.map_err(pg_error)? {
                        tokio_postgres::SimpleQueryMessage::RowDescription(columns) => tables.push(Table::new(columns.iter().map(|c| c.name().to_string()).collect())),
                        tokio_postgres::SimpleQueryMessage::Row(row) => {
                            if tables.is_empty() {
                                tables.push(Table::new(row.columns().iter().map(|c| c.name().to_string()).collect()));
                            }
                            tables.last_mut().expect("a table").add(keep, || (0..row.len()).map(|i| row.get(i).map(String::from)).collect());
                        }
                        _ => {}
                    }
                }
            }
            Db::Mysql(conn) => {
                for statement in crate::sql::split_statements(sql, Dialect::Mysql) {
                    let mut result = conn.query_iter(statement.text.as_str()).await.map_err(mysql_error)?;
                    // A statement may return several result sets, as a procedure's `call` does.
                    loop {
                        let mut table = result.columns().filter(|c| !c.is_empty()).map(|columns| Table::new(columns.iter().map(|c| c.name_str().into_owned()).collect()));
                        result
                            .for_each(|row| {
                                if let Some(table) = table.as_mut() {
                                    table.add(keep, || (0..row.len()).map(|i| row.as_ref(i).and_then(mysql_text)).collect());
                                }
                            })
                            .await
                            .map_err(mysql_error)?;
                        tables.extend(table);
                        if result.is_empty() {
                            break;
                        }
                    }
                }
            }
            Db::Sqlite(conn) => {
                use rusqlite::fallible_iterator::FallibleIterator;
                let mut batch = rusqlite::Batch::new(conn, sql);
                while let Some(mut statement) = batch.next().map_err(sqlite_error)? {
                    let columns: Vec<String> = statement.column_names().into_iter().map(String::from).collect();
                    if columns.is_empty() {
                        statement.raw_execute().map_err(sqlite_error)?;
                        continue;
                    }
                    let count = columns.len();
                    let mut table = Table::new(columns);
                    let mut cursor = statement.raw_query();
                    while let Some(row) = cursor.next().map_err(sqlite_error)? {
                        if table.rows.len() < keep {
                            table.rows.push((0..count).map(|i| row.get_ref(i).map(sqlite_text)).collect::<rusqlite::Result<Vec<_>>>().map_err(sqlite_error)?);
                        }
                        table.total += 1;
                    }
                    tables.push(table);
                }
            }
            Db::Libsql(remote) => {
                for statement in sqlite_statements(sql) {
                    let mut table = remote.query(&statement)?;
                    table.rows.truncate(keep);
                    tables.push(table);
                }
            }
            Db::LibsqlLocal(local) => {
                for statement in sqlite_statements(sql) {
                    tables.push(local.query(&statement, keep)?);
                }
            }
            Db::Host(_) => {}
        }
        tables.retain(|t| !t.columns.is_empty());
        Ok(tables)
    }

    /// Runs a script as [`Db::script`] does, telling a statement that gave up waiting for a lock
    /// (Postgres's `lock_timeout`, MySQL's `lock_wait_timeout`) from any other failure: it changed
    /// nothing, so the file can run again. A host's session can't tell, and fails as `script` does.
    pub(crate) async fn script_or_wait(&mut self, sql: &str) -> Result<(), Ran> {
        match self {
            #[cfg(feature = "cli")]
            Db::Postgres(client) => client.batch_execute(sql).await.map_err(|e| {
                let waited = e.code() == Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE);
                let breach = pg_breach(&e);
                let e = pg_error(e);
                match breach {
                    _ if waited => Ran::Waited(e),
                    Some(b) => Ran::Broke(e, b),
                    None => Ran::Failed(e),
                }
            }),
            #[cfg(feature = "cli")]
            Db::Mysql(_) => {
                let mut locked = false;
                for statement in crate::sql::split_statements(sql, Dialect::Mysql) {
                    let restore = match crate::sql::alters_table(&statement) {
                        true => match self.make_strict().await {
                            Ok(restore) => restore,
                            Err(e) => {
                                self.unlock(locked).await;
                                return Err(Ran::Failed(e));
                            }
                        },
                        false => None,
                    };
                    let Db::Mysql(conn) = &mut *self else { unreachable!("a MySQL session") };
                    let ran = conn.query_drop(&statement.text).await;
                    // The session's own mode is set back whether the change ran or failed; failing to
                    // after it ran leaves the change in place.
                    let back = match restore {
                        Some(mode) => self.exec(&set_sql_mode(&mode)).await,
                        None => Ok(()),
                    };
                    if let Err(e) = ran {
                        self.unlock(locked).await;
                        // ER_LOCK_WAIT_TIMEOUT: a metadata lock, or a row's, waited past its limit.
                        let waited = matches!(&e, mysql_async::Error::Server(s) if s.code == 1205);
                        let breach = match &e {
                            mysql_async::Error::Server(s) => mysql_breach(s.code, &s.message),
                            _ => None,
                        };
                        let e = mysql_error(e);
                        return Err(match breach {
                            _ if waited => Ran::Waited(e),
                            Some(b) => Ran::Broke(e, b),
                            None => Ran::Failed(e),
                        });
                    }
                    if let Err(e) = back {
                        self.unlock(locked).await;
                        return Err(Ran::Partly(e));
                    }
                    locked = (locked || crate::sql::locks_tables(&statement)) && !crate::sql::unlocks_tables(&statement);
                }
                Ok(())
            }
            _ => self.script(sql).await.map_err(Ran::Failed),
        }
    }

    /// Runs one statement with text parameters, `$1` on Postgres and `?` elsewhere.
    pub async fn exec_with(&mut self, sql: &str, params: &[&str]) -> Result<(), Error> {
        match self {
            Db::Host(session) => session.exec_with(sql, params).await,
            #[cfg(feature = "cli")]
            Db::Postgres(client) => {
                let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params.iter().map(|p| p as &(dyn tokio_postgres::types::ToSql + Sync)).collect();
                client.execute(sql, &params).await.map(|_| ()).map_err(pg_error)
            }
            #[cfg(feature = "cli")]
            Db::Mysql(conn) => {
                let values: Vec<mysql_async::Value> = params.iter().map(|p| mysql_async::Value::from(*p)).collect();
                conn.exec_drop(sql, mysql_async::Params::Positional(values)).await.map_err(mysql_error)
            }
            #[cfg(feature = "cli")]
            Db::Sqlite(conn) => conn.execute(sql, rusqlite::params_from_iter(params.iter())).map(|_| ()).map_err(sqlite_error),
            #[cfg(feature = "cli")]
            Db::Libsql(remote) => remote.exec_with(sql, params),
            #[cfg(feature = "cli")]
            Db::LibsqlLocal(local) => local.exec_with(sql, params),
        }
    }

    pub async fn close(self) {
        #[cfg(feature = "cli")]
        match self {
            Db::Mysql(conn) => {
                let _ = conn.disconnect().await;
            }
            Db::Libsql(remote) => remote.close(),
            _ => {}
        }
    }

    /// The parameter marker for the `n`th parameter.
    pub fn param(&self, n: usize) -> String {
        match self.dialect() {
            Dialect::Postgres => format!("${n}"),
            _ => "?".into(),
        }
    }
}

pub const SHADOW_PREFIX: &str = "reylo_shadow_";

/// Where the host starts in what follows a URL's `://`: past the credentials, whose password may
/// hold a `#`. A `/` or `?` ends them, so an `@` in the path or the settings is no part of them.
fn host_start(rest: &str) -> usize {
    let head = &rest[..rest.find(['/', '?']).unwrap_or(rest.len())];
    head.rfind('@').map_or(0, |at| at + 1)
}

/// Why a run can't go through the transaction pooler a Postgres URL names, by the host its platform
/// gives one (Neon's endpoint with `-pooler`, Supabase's pooler on port 6543), with the connection
/// to use instead in `key`, the project file's setting that named it (`direct_url`, `shadow_url`). Such a pooler gives each transaction whichever server session is free, so a
/// run's session lock outlives the run on the pooler's session and stops the next run that connects
/// directly, and a setting a run makes for its session reaches the app's queries on it. An empty
/// `key` is a URL no project file names, as an app's own pool has.
pub fn transaction_pooler(url: &str, key: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if !matches!(scheme, "postgres" | "postgresql") {
        return None;
    }
    let rest = &rest[host_start(rest)..];
    let authority = rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())].to_ascii_lowercase();
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.contains(']') => (host, port.parse::<u16>().ok()),
        _ => (authority.as_str(), None),
    };
    // The query's host and port replace the URL's own, as libpq reads them: a project's env files
    // write their PGHOST and PGPORT there.
    let query = url.split_once('?').map_or("", |(_, q)| q.split('#').next().unwrap_or_default());
    let param = |name: &str| query.split('&').filter_map(|pair| pair.split_once('=')).filter(|(n, _)| crate::percent_decode(n) == name).map(|(_, v)| crate::percent_decode(v).to_ascii_lowercase()).last();
    let (given_host, given_port) = (param("host"), param("port"));
    let host = given_host.as_deref().map_or(host, |h| h.split(',').next().unwrap_or_default());
    let port = given_port.map_or(port, |p| p.split(',').next().and_then(|p| p.parse().ok()));
    transaction_pooler_at(host, port, &shown_url(url), key)
}

/// [`transaction_pooler`] of a host and port already parsed, as a driver's connect options hold them, with `shown` naming the connection in the message.
pub fn transaction_pooler_at(host: &str, port: Option<u16>, shown: &str, key: &str) -> Option<String> {
    // A name may end in the root's dot, as the same host's absolute form.
    let host = host.to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    let (first, domain) = host.split_once('.')?;
    let place = match key {
        "" => String::new(),
        "direct_url" => ", in direct_url in .reylo.yaml or with --url".to_string(),
        key => format!(", in {key} in .reylo.yaml"),
    };
    if let Some(endpoint) = first.strip_suffix("-pooler").filter(|_| domain.ends_with("neon.tech")) {
        return Some(format!(
            "{shown} is Neon's pooled connection, whose PgBouncer shares server sessions between clients, and a run of reylo migrate holds one: use the direct connection, host {endpoint}.{domain}{place}"
        ));
    }
    if domain.ends_with("pooler.supabase.com") && port == Some(6543) {
        return Some(format!(
            "{shown} is Supabase's transaction pooler, which shares server sessions between clients, and a run of reylo migrate holds one: use its session pooler, port 5432 on the same host, or the direct connection{place}"
        ));
    }
    if first == "db" && port == Some(6543) && domain.strip_suffix(".supabase.co").is_some_and(|project| !project.contains('.')) {
        return Some(format!(
            "{shown} is Supabase's dedicated transaction pooler, which shares server sessions between clients, and a run of reylo migrate holds one: use the direct connection, port 5432 on the same host{place}"
        ));
    }
    None
}

/// The URL with its database replaced by `name`.
pub fn with_database(url: &str, name: &str) -> String {
    let Some(scheme_end) = url.find("://") else { return url.to_string() };
    let rest = &url[scheme_end + 3..];
    let start = host_start(rest);
    let authority_end = rest[start..].find(['/', '?', '#']).map_or(rest.len(), |at| start + at);
    let (authority, tail) = rest.split_at(authority_end);
    let query = match tail.find(['?', '#']) {
        Some(at) => &tail[at..],
        None => "",
    };
    // A `dbname` setting names the database over the path, as libpq and tokio-postgres read it: it
    // would send a shadow's session to the target's own database.
    let (query, fragment) = query.split_at(query.find('#').unwrap_or(query.len()));
    let kept: Vec<&str> = query.strip_prefix('?').unwrap_or(query).split('&').filter(|pair| !pair.is_empty() && crate::percent_decode(pair.split('=').next().unwrap_or_default()) != "dbname").collect();
    let query = if kept.is_empty() { String::new() } else { format!("?{}", kept.join("&")) };
    format!("{}://{authority}/{name}{query}{fragment}", &url[..scheme_end])
}

/// A URL as a heading says it: no credentials and no settings. Whatever is before the last `@` is
/// taken for credentials, so that a password never shows, even in a form no driver reads; but a
/// SQLite file's URL is its path, whole, as [`sqlite_path`] opens it (an `@` or a `?` is part of
/// its name), and holds none.
pub fn shown_url(url: &str) -> String {
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("sqlite:") || lower.starts_with("file:") {
        return url.to_string();
    }
    let bare = &url[..url.find('?').unwrap_or(url.len())];
    match bare.split_once("://") {
        Some((scheme, rest)) => {
            let host = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
            format!("{scheme}://{}", &host[..host.find('#').unwrap_or(host.len())])
        }
        None => bare.to_string(),
    }
}

/// The database a URL names.
pub fn database_of(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else { return String::new() };
    let rest = &url[scheme_end + 3..];
    let start = host_start(rest);
    let Some(slash) = rest[start..].find('/') else { return String::new() };
    let path = &rest[start + slash + 1..];
    let end = path.find(['?', '#']).unwrap_or(path.len());
    crate::percent_decode(&path[..end])
}

pub(crate) fn random_name() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    hasher.write_u32(std::process::id());
    format!("{SHADOW_PREFIX}{:010x}", hasher.finish() & 0xff_ffff_ffff)
}

/// A Postgres identifier, quoted.
pub(crate) fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

thread_local! {
    static PG_DUMP_DIRS: std::cell::RefCell<Option<Vec<std::path::PathBuf>>> = const { std::cell::RefCell::new(None) };
}

/// Looks for pg_dump on this thread in `dirs` alone (none: nowhere), in place of [`pg_dump_dirs`];
/// `None` goes back to them. For tests, which run on threads of their own.
pub fn set_pg_dump_dirs(dirs: Option<Vec<std::path::PathBuf>>) {
    PG_DUMP_DIRS.with(|d| *d.borrow_mut() = dirs);
}

/// Where pg_dump is looked for, for a server of `major`'s, in order: `$PGBIN`; each install of that
/// version (Homebrew's keg, Debian's and Ubuntu's directory, whose PATH wrapper picks a local
/// cluster's version, PGDG's on RHEL, Alpine's, Postgres.app, Windows' installer); the client
/// alone (Homebrew's `libpq`); each install of a newer version, the nearest first, since pg_dump
/// reads every older server; then the PATH's.
fn pg_dump_dirs(major: i64) -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    if let Some(dirs) = PG_DUMP_DIRS.with(|d| d.borrow().clone()) {
        return dirs;
    }
    // Each place an install of a version goes, as a directory of versions and the bin directory
    // under the version's: each system's own, since another's path reads as one in the project.
    let installs: &[(&str, &str, &str)] = if cfg!(windows) {
        &[(r"C:\Program Files\PostgreSQL", "", "bin")]
    } else {
        &[
            ("/opt/homebrew/opt", "postgresql@", "bin"),
            ("/usr/local/opt", "postgresql@", "bin"),
            ("/usr/lib/postgresql", "", "bin"),
            ("/usr", "pgsql-", "bin"),
            ("/usr/libexec", "postgresql", ""),
            ("/Applications/Postgres.app/Contents/Versions", "", "bin"),
        ]
    };
    let bin = |parent: &str, prefix: &str, sub: &str, version: i64| {
        let dir = PathBuf::from(parent).join(format!("{prefix}{version}"));
        if sub.is_empty() { dir } else { dir.join(sub) }
    };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PGBIN").filter(|v| !v.is_empty()).map(PathBuf::from).into_iter().collect();
    dirs.extend(installs.iter().map(|&(parent, prefix, sub)| bin(parent, prefix, sub, major)));
    if !cfg!(windows) {
        dirs.extend(["/opt/homebrew/opt/libpq/bin", "/usr/local/opt/libpq/bin"].map(PathBuf::from));
    }
    let mut newer: Vec<(i64, PathBuf)> = Vec::new();
    for &(parent, prefix, sub) in installs {
        let Ok(entries) = std::fs::read_dir(parent) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(version) = name.strip_prefix(prefix).and_then(|v| v.parse::<i64>().ok()).filter(|&v| v > major) {
                newer.push((version, bin(parent, prefix, sub, version)));
            }
        }
    }
    newer.sort();
    dirs.extend(newer.into_iter().map(|(_, dir)| dir));
    dirs.extend(std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default());
    dirs
}

/// Why no pg_dump can dump a server: the version it needs, and the older ones found, each with its
/// path.
#[derive(Debug, Clone)]
pub struct NoPgDump {
    pub major: i64,
    pub older: Vec<(String, i64)>,
}

/// The pg_dump to dump a server of `major`'s with: the first in [`pg_dump_dirs`] of that version or
/// newer, since pg_dump refuses a server newer than itself.
pub(crate) fn find_pg_dump(major: i64) -> Result<std::path::PathBuf, NoPgDump> {
    let version_of = |p: &Path| -> i64 {
        let out = std::process::Command::new(p).arg("--version").output().ok();
        let text = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        text.split_whitespace().find_map(|w| w.split('.').next()?.parse().ok()).unwrap_or(0)
    };
    let name = if cfg!(windows) { "pg_dump.exe" } else { "pg_dump" };
    let mut older: Vec<(String, i64)> = Vec::new();
    for dir in pg_dump_dirs(major) {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        match version_of(&path) {
            v if v >= major => return Ok(path),
            0 => {}
            v => {
                let shown = path.display().to_string();
                if !older.iter().any(|(p, _)| p == &shown) {
                    older.push((shown, v));
                }
            }
        }
    }
    Err(NoPgDump { major, older })
}

/// pg_dump against `url`, with `args`, its password in its environment rather than on its command
/// line, with the pg_dump [`find_pg_dump`] finds for the server's `major` version.
pub(crate) fn pg_dump(url: &str, major: i64, args: &[String]) -> Result<String, Error> {
    let binary = find_pg_dump(major).map_err(|_| Error(format!("reading this schema needs pg_dump {major} or newer (the server is Postgres {major})")))?;
    let (bare, password) = without_password(url);
    let mut command = std::process::Command::new(binary);
    command.args(args).arg(bare);
    if let Some(password) = password {
        command.env("PGPASSWORD", password);
    }
    let out = command.output()?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        // Its message, without the prefix it puts on every line.
        let said = said.trim().trim_start_matches("pg_dump: error: ").trim_start_matches("pg_dump: ");
        if said.starts_with("no matching schemas were found") {
            return Err(Error("this database has no schema of the name --schema gives".into()));
        }
        return Err(Error(format!("pg_dump couldn't read this schema: {said}")));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The URL without its password, and the password, decoded.
/// A `password` the query names is taken too, and wins over the login's, as libpq reads the URL.
fn without_password(url: &str) -> (String, Option<String>) {
    let (url, in_query) = crate::without_query_password(url);
    let Some(scheme_end) = url.find("://") else { return (url, in_query) };
    let rest = &url[scheme_end + 3..];
    let at = host_start(rest);
    if at == 0 {
        return (url.clone(), in_query);
    }
    let userinfo = &rest[..at - 1];
    match userinfo.split_once(':') {
        Some((user, password)) => (format!("{}://{user}@{}", &url[..scheme_end], &rest[at..]), in_query.or_else(|| Some(crate::percent_decode(password)))),
        None => (url.clone(), in_query),
    }
}

/// The platform's own schemas on the target, as the statements that make them, read and planned
/// as `migrate new` plans (no pg_dump, which a build machine often lacks): without owners,
/// privileges or comments, and without the extensions, which a shadow makes itself. What the
/// project made on the platform's tables is left to the project's own SQL, which makes it again: a
/// policy (Reylo takes every policy for the project's), and a trigger whose function is outside the
/// platform's schemas (`on_auth_user_created`, calling `public.handle_new_user()`). Where they hold
/// what Reylo can't make (a text search configuration), pg_dump makes them, where there is one;
/// where there is none, the rest, and the note that says what every shadow leaves out.
#[cfg(feature = "cli")]
async fn platform_ddl(target: &mut Db, url: &str, version: i64, schemas: &[String]) -> Result<(Vec<String>, Option<String>), Error> {
    let read = async {
        let unread = crate::diff::postgres::platform_unread(target, schemas).await?;
        let platform = crate::diff::postgres::read_platform(target, schemas).await?;
        let alterer = crate::diff::alterer(target).await?;
        Ok::<_, Error>((unread, crate::diff::plan(&crate::diff::Schema::default(), &platform, alterer.as_ref())))
    };
    let (unread, plan) = read.await.map_err(|e| Error(format!("reading the platform's schemas {} for shadow databases: {e}", schemas.join(", "))))?;
    let mut note = None;
    if !unread.is_empty() {
        let major = version / 10000;
        let fix = match find_pg_dump(major) {
            Err(_) => format!("pg_dump {major} or newer, where it's installed, makes it"),
            Ok(_) => match platform_dump(url, major, schemas) {
                Ok(dumped) => return Ok((dumped, None)),
                Err(e) => e.0,
            },
        };
        note = Some(format!("· {LEFT_OUT} {}, the platform's, which Reylo can't make, so a migration that uses it fails in them: {fix}", unread.join(", ")));
    }
    Ok((plan.statements.into_iter().filter(|s| !s.trim_start().starts_with("--")).collect(), note))
}

/// The platform's schemas `schemas` as pg_dump makes them, with what [`platform_ddl`] says it leaves
/// out left out.
fn platform_dump(url: &str, major: i64, schemas: &[String]) -> Result<Vec<String>, Error> {
    let mut args: Vec<String> = ["--schema-only", "--no-owner", "--no-privileges", "--no-comments", "--no-publications", "--no-subscriptions", "--no-security-labels", "--no-tablespaces"].map(String::from).to_vec();
    for s in schemas {
        args.push("--schema".into());
        args.push(s.clone());
    }
    let text = pg_dump(url, major, &args)?;
    let kept: Vec<&str> = text.lines().filter(|l| !l.starts_with('\\') && !l.starts_with("SET ") && !l.starts_with("SELECT pg_catalog.set_config") && !l.starts_with("--")).collect();
    Ok(crate::sql::split_statements(&kept.join("\n"), Dialect::Postgres)
        .into_iter()
        .map(|s| s.text)
        .filter(|t| {
            let head = t.trim_start().to_ascii_uppercase();
            if head.starts_with("CREATE EXTENSION") || head.starts_with("COMMENT ON EXTENSION") || head.starts_with("CREATE POLICY") {
                return false;
            }
            if head.starts_with("CREATE TRIGGER") || head.starts_with("CREATE CONSTRAINT TRIGGER") {
                static FUNCTION: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r#"(?i)\bexecute\s+(?:function|procedure)\s+"?([^".(\s]+)"?\."#).expect("a valid pattern"));
                return FUNCTION.captures(t).is_some_and(|c| schemas.iter().any(|s| s.eq_ignore_ascii_case(&c[1])));
            }
            true
        })
        .collect())
}

/// How the note opens that says what every shadow leaves out of the target.
pub const LEFT_OUT: &str = "Shadow databases leave out";

/// Runs `statements` in the new shadow `name` as `bare`, a role the connection may become, which may
/// create schemas in it and has `privileges` on every schema for the while it makes them, given back
/// after. A statement it can't run is left out.
#[cfg(feature = "cli")]
async fn make_as(admin: &tokio_postgres::Client, db: &mut Db, name: &str, bare: &str, statements: &[String], privileges: &[&str]) {
    let role = ident(bare);
    let _ = admin.batch_execute(&format!("grant create on database \"{name}\" to {role}")).await;
    let lacking = |privilege: &str| {
        format!(
            "select format('grant {} on schema %I to %s', nspname, {}) as s from pg_namespace where nspname not like 'pg\\_%' and nspname <> 'information_schema' and not has_schema_privilege({}, oid, '{privilege}')",
            privilege.to_lowercase(),
            literal(&role),
            literal(bare)
        )
    };
    let mut grants: Vec<String> = Vec::new();
    for privilege in privileges {
        grants.extend(db.rows(&lacking(privilege)).await.unwrap_or_default().iter().map(|r| r.text("s").to_string()));
    }
    for g in &grants {
        let _ = db.exec(g).await;
    }
    let _ = db.exec(&format!("set role {role}")).await;
    let _ = db.exec("set check_function_bodies = off").await;
    for statement in statements {
        let _ = db.exec(statement).await;
    }
    let _ = db.exec("reset check_function_bodies").await;
    let _ = db.exec("reset role").await;
    for g in &grants {
        let _ = db.exec(&format!("revoke{}", &g["grant".len()..]).replacen(" to ", " from ", 1)).await;
    }
    let _ = admin.batch_execute(&format!("revoke create on database \"{name}\" from {role}")).await;
}

/// What a superuser the target's role may not act as owns there that the shadows need beyond the
/// name list's schemas, which is the platform's: a shadow makes it as a role every comparison
/// leaves out, or leaves it out.
#[cfg(feature = "cli")]
#[derive(Default)]
pub struct Owned {
    /// Its schemas, whole, as Reylo plans them, with the privileges the target gives on them, and
    /// its routines in the project's schemas.
    pub(crate) statements: Vec<String>,
    /// Its extensions (Tiger Cloud's `timescaledb`, Azure's `pgaadauth`), each `(name, schema)`.
    pub(crate) extensions: Vec<(String, String)>,
    /// The role of the shadows' server that makes them, [`crate::catalog::SHADOW_PLATFORM_ROLE`],
    /// where the connection may become it.
    pub(crate) role: Option<String>,
    pub(crate) note: Option<String>,
}

#[cfg(feature = "cli")]
impl Owned {
    /// What a replay cache's key takes of it beside its statements.
    pub(crate) fn key(&self) -> String {
        let extensions: Vec<String> = self.extensions.iter().map(|(e, s)| format!("{e} {s}")).collect();
        format!("{} {}", self.role.as_deref().unwrap_or_default(), extensions.join(","))
    }
}

/// What [`Owned`] the target has, with the role the shadows' server (`admin`'s) makes it as, where
/// that server has it and the connection may become it; else the note that says it is left out.
/// Reylo makes no role itself: a role is the whole server's, and outlives every run, so the shadows'
/// server, often the target's own, is given one only by whoever runs it.
#[cfg(feature = "cli")]
async fn owned_by_platform(target: &mut Db, admin: &tokio_postgres::Client, url: &str, version: i64) -> Result<Owned, Error> {
    use crate::catalog::{beyond_reach, in_project_schema, not_extension, not_platform_schema, PG_USER_NS, PLATFORM_ROLES, SHADOW_PLATFORM_ROLE};
    let named = PLATFORM_ROLES.iter().map(|r| literal(r)).collect::<Vec<_>>().join(", ");
    let theirs = |owner: &str| format!("{} and pg_get_userbyid({owner}) <> all (array[{named}])", beyond_reach(owner));
    let rows = target
        .rows(&format!(
            "select 's' as k, n.nspname::text as a, null::text as b, pg_get_userbyid(n.nspowner)::text as owner, 0::int8 as o from pg_namespace n
              where {} and n.nspname <> 'public' and n.nspname not like 'pg\\_%' and n.nspname <> 'information_schema' and {}
            union all
            select 'x', x.extname, n.nspname, pg_get_userbyid(x.extowner), 0 from pg_extension x join pg_namespace n on n.oid = x.extnamespace where x.extname <> 'plpgsql' and {}
            union all
            select 'f', pg_get_functiondef(p.oid), null, pg_get_userbyid(p.proowner), p.oid::int8 from pg_proc p join pg_namespace n on n.oid = p.pronamespace
              where p.prokind in ('f', 'p') and {PG_USER_NS} and {} and {} and {}
            order by 1, 5, 2",
            theirs("n.nspowner"),
            not_extension("pg_namespace", "n"),
            // One in a schema of the project's is the project's, whoever owns it.
            format!("{} and not {}", theirs("x.extowner"), in_project_schema("n")),
            theirs("p.proowner"),
            not_platform_schema("n"),
            not_extension("pg_proc", "p")
        ))
        .await?;
    if rows.is_empty() {
        return Ok(Owned::default());
    }
    let of = |k: &'static str| rows.iter().filter(move |r| r.text("k") == k);
    let schemas: Vec<String> = of("s").map(|r| r.text("a").to_string()).collect();
    let mut owners: Vec<String> = rows.iter().map(|r| r.text("owner").to_string()).collect();
    owners.sort();
    owners.dedup();
    let extensions = of("x").map(|r| (r.text("a").to_string(), r.text("b").to_string())).collect();
    let role = SHADOW_PLATFORM_ROLE;
    let may = format!("select pg_has_role(current_user, oid, case when current_setting('server_version_num')::int >= 160000 then 'SET' else 'MEMBER' end)::text as may from pg_roles where rolname = {}", literal(role));
    if pg_rows(admin, &may).await?.first().is_some_and(|r| r.text("may") == "true") {
        let (mut statements, note) = if schemas.is_empty() { (vec![], None) } else { platform_ddl(target, url, version, &schemas).await? };
        statements.extend(schema_grants(target, &schemas).await?);
        statements.extend(of("f").map(|r| r.text("a").to_string()));
        return Ok(Owned { statements, extensions, role: Some(role.to_string()), note });
    }
    // The role the shadows are made as, which takes on the role to make it: `shadow_url`'s, else
    // the target's.
    let user = pg_rows(admin, "select current_user as u").await?.first().map(|r| r.text("u").to_string()).unwrap_or_default();
    let note = format!(
        "· {LEFT_OUT} what {} {} here, the platform's, so a migration that uses it fails in them: a role {role} on their server that {user}, the role they are made as, may SET ROLE to lets them make it",
        owners.join(", "),
        if owners.len() == 1 { "owns" } else { "own" }
    );
    Ok(Owned { statements: vec![], extensions, role: None, note: Some(note) })
}

/// A privilege one of Supabase's Data API roles may have by default on what `postgres` makes in
/// `public`: `(objects, role, privilege)`, as `("tables", "anon", "SELECT")`.
pub(crate) type ApiPrivilege = (String, String, String);

/// The privileges a Supabase project's settings grant its Data API's roles by default, or not, each
/// set together: whether new tables are exposed to the API (reading and writing a table's rows, and
/// using a sequence), and whether new functions are. Supabase turned the first off for new projects
/// in 2026 and for every hosted project later, while its local stack keeps it on unless told
/// otherwise. A new Supabase database grants every other privilege on what `postgres` makes in
/// `public` either way.
pub(crate) const API_SETTINGS: [&[(&str, &[&str])]; 2] = [&[("tables", &["DELETE", "INSERT", "SELECT", "UPDATE"]), ("sequences", &["SELECT", "USAGE"])], &[("functions", &["EXECUTE"])]];

/// For each of [`API_SETTINGS`], whether a database started with it on: read from the privileges it
/// grants now (`current`), all but those a project's SQL changes (`changed`), as most of the rest
/// grant or don't; on where it changes them all, as every new Supabase database once started.
pub(crate) fn api_settings(current: &[ApiPrivilege], changed: &[ApiPrivilege]) -> [bool; 2] {
    api_settings_shown(current, changed).map(|shown| shown.unwrap_or(true))
}

/// As [`api_settings`], but none for a setting all of whose privileges `changed` holds, which the
/// privileges left say nothing of.
pub(crate) fn api_settings_shown(current: &[ApiPrivilege], changed: &[ApiPrivilege]) -> [Option<bool>; 2] {
    API_SETTINGS.map(|setting| {
        let (mut on, mut off) = (0, 0);
        for (objects, privileges) in setting {
            for role in crate::catalog::PLATFORM_API_ROLES {
                for privilege in privileges.iter() {
                    let one = (objects.to_string(), role.to_string(), privilege.to_string());
                    if changed.contains(&one) {
                        continue;
                    }
                    if current.contains(&one) {
                        on += 1;
                    } else {
                        off += 1;
                    }
                }
            }
        }
        (on + off > 0).then_some(on >= off)
    })
}

/// The note on the history table that records how a Supabase database began: under which settings
/// ([`API_SETTINGS`]), and with which of Supabase's own extensions
/// ([`crate::catalog::PLATFORM_EXTENSIONS`]), which nothing else shows once its migrations have
/// changed them.
pub(crate) fn api_settings_note(on: [bool; 2], extensions: &[String]) -> String {
    let said = |on: bool| if on { "exposed" } else { "not exposed" };
    let with = if extensions.is_empty() { "none of Supabase's extensions".to_string() } else { format!("Supabase's extensions {}", extensions.join(", ")) };
    format!("Reylo Migrate's history. This database began with Supabase's Data API settings: new tables {}, new functions {}. It began with {with}.", said(on[0]), said(on[1]))
}

/// The extensions of Supabase's a history table's `note` records the database began with
/// ([`api_settings_note`]).
pub(crate) fn extensions_in_note(note: &str) -> Option<Vec<String>> {
    if note.contains("It began with none of Supabase's extensions.") {
        return Some(vec![]);
    }
    let rest = note.split_once("It began with Supabase's extensions ")?.1;
    let list = rest.split_once('.')?.0;
    Some(list.split(", ").map(String::from).collect())
}

/// The settings a history table's `note` records ([`api_settings_note`]).
pub(crate) fn api_settings_in_note(note: &str) -> Option<[bool; 2]> {
    let read = |what: &str| {
        if note.contains(&format!("new {what} not exposed")) {
            Some(false)
        } else if note.contains(&format!("new {what} exposed")) {
            Some(true)
        } else {
            None
        }
    };
    Some([read("tables")?, read("functions")?])
}

/// What a new Supabase database starts with for its Data API, under the settings `on`
/// ([`api_settings`]), as the statements that make it: `postgres`'s default privileges in `public`
/// for the API's roles, and the grants of `USAGE` on `public` to them and to `postgres`. One
/// statement each, so that a server without one of the roles leaves out only it.
pub(crate) fn platform_defaults(on: [bool; 2]) -> (Vec<String>, Vec<String>) {
    use crate::catalog::{PLATFORM_API_ROLES, PLATFORM_CREATOR};
    let head = format!("alter default privileges for role {PLATFORM_CREATOR} in schema public");
    let mut privileges: Vec<String> = Vec::new();
    for role in PLATFORM_API_ROLES {
        for objects in ["tables", "sequences", "functions"] {
            privileges.push(format!("{head} grant all on {objects} to {role}"));
        }
        for (setting, on) in API_SETTINGS.iter().zip(on) {
            for (objects, taken) in setting.iter().filter(|_| !on) {
                privileges.push(format!("{head} revoke {} on {objects} from {role}", taken.join(", ").to_lowercase()));
            }
        }
    }
    let grants = std::iter::once(&PLATFORM_CREATOR).chain(PLATFORM_API_ROLES).map(|role| format!("grant usage on schema public to {role}")).collect();
    (privileges, grants)
}

/// What `statements`, each one of [`default_privileges`], grant the Data API's roles on what
/// `postgres` makes in `public`: the role that runs the migrations is `postgres` on Supabase, whose
/// own name none of its statements, or another role named for `postgres`.
pub(crate) fn api_privileges(statements: &[String]) -> Vec<ApiPrivilege> {
    use crate::catalog::{PLATFORM_API_ROLES, PLATFORM_CREATOR};
    statements
        .iter()
        .filter_map(|statement| {
            let rest = statement.strip_prefix("alter default privileges in schema public grant ").or_else(|| statement.strip_prefix(&format!("alter default privileges for role {PLATFORM_CREATOR} in schema public grant ")))?;
            let (privilege, rest) = rest.split_once(" on ")?;
            let (objects, role) = rest.trim_end_matches(" with grant option").split_once(" to ")?;
            (PLATFORM_API_ROLES.contains(&role) && ["tables", "sequences", "functions"].contains(&objects)).then(|| (objects.to_string(), role.to_string(), privilege.to_uppercase()))
        })
        .collect()
}

/// Gives a shadow's session the target's word on whether it is a platform's (`settings`, the target
/// database's own, carry [`crate::catalog::PLATFORM_SCHEMAS_SETTING`]).
#[cfg(feature = "cli")]
pub(crate) async fn mark_platform_names(db: &mut Db, settings: &[String]) {
    let key = format!("{}=", crate::catalog::PLATFORM_SCHEMAS_SETTING);
    if let Some(value) = settings.iter().find_map(|s| s.strip_prefix(&key)) {
        let _ = db.exec(&format!("select set_config({}, {}, false)", literal(crate::catalog::PLATFORM_SCHEMAS_SETTING), literal(value))).await;
    }
}

/// The target's privileges on `schemas`, as the grants that make them, its owner's own left out.
async fn schema_grants(target: &mut Db, schemas: &[String]) -> Result<Vec<String>, Error> {
    let mut out = Vec::new();
    for schema in schemas {
        let rows = target
            .rows(&format!(
                "select format('grant %s on schema %I to %s%s', x.privilege_type, n.nspname, case when x.grantee = 0 then 'public' else quote_ident(pg_get_userbyid(x.grantee)) end, case when x.is_grantable then ' with grant option' else '' end) as g
                from pg_namespace n, aclexplode(n.nspacl) x where n.nspname = {} and x.grantee <> n.nspowner",
                literal(schema)
            ))
            .await?;
        out.extend(rows.iter().map(|r| r.text("g").to_string()));
    }
    Ok(out)
}

/// Revokes what was granted on the MySQL database `name`'s tables, columns and routines, which
/// MySQL keeps after the database is dropped: a shadow's grants would pile up on the server, and a
/// database made later under its name would have them. A column's privilege is kept with a row for
/// its table, so the tables' and the routines' rows name everything. What the connection may not
/// read or revoke is left.
#[cfg(feature = "cli")]
async fn mysql_revoke_on(admin: &mut mysql_async::Conn, name: &str) {
    let tick = |n: &str| format!("`{}`", n.replace('`', "``"));
    let rows: Vec<(String, String, String, String)> = admin
        .query(format!("select user, host, 'table', table_name from mysql.tables_priv where db = {} union all select user, host, lower(routine_type), routine_name from mysql.procs_priv where db = {}", literal(name), literal(name)))
        .await
        .unwrap_or_default();
    for (user, host, kind, object) in rows {
        let on = if kind == "table" { String::new() } else { format!("{kind} ") };
        for what in ["all privileges", "grant option"] {
            let _ = admin.query_drop(format!("revoke if exists {what} on {on}{}.{} from {}@{} ignore unknown user", tick(name), tick(&object), tick(&user), tick(&host))).await;
        }
    }
}

pub(crate) fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

pub struct Shadow {
    pub db: Db,
    pub name: String,
}

/// Where shadows are made: the target's server, or the one `shadow_url` names. Without the `cli`
/// feature there is none: deploy and status make no shadow.
#[cfg(not(feature = "cli"))]
pub enum Server {}

/// Where shadows are made: the target's server, or the one `shadow_url` names.
#[cfg(feature = "cli")]
pub enum Server {
    Postgres {
        admin: tokio_postgres::Client,
        url: String,
        /// What follows `create database "<name>"`: the target's encoding and locale, where they are not template1's.
        create: String,
        /// Why a shadow's `create database` may refuse the target's locale: a name the shadow server
        /// knows by no spelling, as the refusal says.
        locale: Option<String>,
        /// The target database's own settings (`alter database … set`), such as a search_path, which
        /// decide where an unqualified name goes.
        settings: Vec<String>,
        /// The target's default privileges (`alter default privileges`), which grant on what a
        /// migration creates, as a platform's do (Supabase's grant every new table to its API's
        /// roles); made again in each shadow unless the project sets its own.
        privileges: Vec<String>,
        copy_privileges: bool,
        /// The target's extensions, each `(name, schema)`, made again in each shadow but those the
        /// project's own files create (`leave`): a platform installs some for every project
        /// (Supabase's `pgcrypto` and `uuid-ossp`, in its `extensions` schema), which migrations call.
        extensions: Vec<(String, String)>,
        leave: Vec<String>,
        /// The schemas a platform's roles own on the target (Supabase's `auth` and `storage`), as the
        /// statements that make them, which a migration's `references auth.users` needs, and the
        /// platform's role they are made as in each shadow (one the connection may become), so that
        /// every comparison leaves them out there as it does on the target.
        environment: Vec<String>,
        environment_role: Option<String>,
        /// What a superuser the target's role may not act as owns there beyond those schemas (Tiger
        /// Cloud's `timescale_functions`, its functions in `public`, its extensions), the platform's
        /// too: made in each shadow as a role every comparison leaves out there, as the superuser's
        /// is on the target, and left out where the shadows' role may not become it.
        owned: Owned,
        /// The target's privileges on the schemas a shadow has without the project making them
        /// (`public`, and those its extensions are in), as `grant` statements; made again in each
        /// shadow but where SQL is written to make the database whole.
        schema_grants: Vec<String>,
        copy_schema_grants: bool,
        /// On a Supabase target, what a shadow is given where it takes none of the target's default
        /// privileges or grants on its schemas: the default privileges `postgres` gives the Data
        /// API's roles in `public` as the target began with them, under its project's settings
        /// ([`API_SETTINGS`], [`Server::began_with`]), which the project's SQL changes from there;
        /// and, for SQL written to make the database whole, their grants on `public` as a new
        /// database has them.
        platform_privileges: Vec<String>,
        platform_schema_grants: Vec<String>,
        /// Whether the target is Supabase's.
        platform: bool,
        /// Which of Supabase's own extensions the target began with, where that is known: what a
        /// shadow has of them, in `extensions`, whatever the target has now.
        began_extensions: Option<Vec<String>>,
        /// Whether each shadow is given the platform's publication, empty, as the target was:
        /// Supabase's `supabase_realtime`, which a project's SQL adds its tables to.
        publication: bool,
        /// Whether the target is the database its server's pg_cron makes itself in, where a shadow
        /// makes pg_cron under [`PG_CRON_NAME`] ([`Server::name_for_pg_cron`]); and whether a shadow of
        /// this run couldn't be named back from it, which keeps it from every other.
        pg_cron_target: bool,
        pg_cron_stuck: bool,
    },
    Mysql {
        admin: mysql_async::Conn,
        url: String,
        /// The target database's default character set and collation, which every table created without its own inherits.
        defaults: Option<(String, String)>,
    },
    /// Shadows in memory: on libSQL's engine for a libSQL database (Turso's), else Reylo's SQLite.
    Sqlite { libsql: bool },
}

#[cfg(feature = "cli")]
const PG_LISTS: &[&str] = &["search_path", "temp_tablespaces", "local_preload_libraries", "session_preload_libraries"];

#[cfg(feature = "cli")]
impl Server {
    /// The server shadows are made on: `shadow_url`'s, else the target's. A shadow is made like the
    /// target, which `target` is connected to.
    pub async fn open(target: &mut Db, url: &str, shadow_url: Option<&str>) -> Result<Server, Error> {
        let server_url = shadow_url.unwrap_or(url);
        match target.dialect() {
            Dialect::Postgres => {
                // The maintenance connection: `postgres`, else `template1`, since `create database`
                // cannot run in the database it creates beside.
                let mut admin = None;
                let mut last = String::new();
                for db in ["postgres", "template1"] {
                    match crate::db::open_postgres(&with_database(server_url, db)).await {
                        Ok(client) => {
                            admin = Some(client);
                            break;
                        }
                        Err(e) => last = e.0,
                    }
                }
                let Some(admin) = admin else {
                    return Err(Error(format!("cannot connect to the server's postgres or template1 database to create shadow databases: {last}")));
                };
                let version: i64 = target.rows("select current_setting('server_version_num') as v").await?.first().and_then(|r| r.first()?.parse().ok()).unwrap_or(0);
                // The locale provider is Postgres 15's, and the locale `datlocale` from 17 and
                // `daticulocale` in 15 and 16: read through `to_jsonb`, a column the server lacks is
                // null, and a database before 15 is libc's.
                let props = |filter: &str| {
                    format!(
                        "select pg_encoding_to_char(encoding) as enc, datcollate as collate, datctype as ctype, coalesce(to_jsonb(d) ->> 'datlocprovider', 'c') as provider,
                          coalesce(to_jsonb(d) ->> 'datlocale', to_jsonb(d) ->> 'daticulocale') as locale from pg_database d where {filter}"
                    )
                };
                let mine = target.rows(&props("datname = current_database()")).await?.into_iter().next();
                let template = pg_rows(&admin, &props("datname = 'template1'")).await?.into_iter().next();
                let key = |r: &Row| ["enc", "collate", "ctype", "provider", "locale"].map(|k| r.get(k).map(String::from));
                // A libc locale is named as the shadow server's system names it, which another
                // system may spell otherwise (Linux's `en_US.utf8` is macOS's `en_US.UTF-8`).
                let known: Vec<String> = pg_rows(
                    &admin,
                    "select collcollate as n from pg_collation where collprovider = 'c' and collcollate is not null
                      union select collctype from pg_collation where collprovider = 'c' and collctype is not null
                      union select datcollate from pg_database union select datctype from pg_database",
                )
                .await?
                .iter()
                .filter_map(|r| r.get("n").map(String::from))
                .collect();
                let mut unknown_locale = None;
                let create = match (&mine, &template) {
                    (Some(m), Some(t)) if key(m) != key(t) => {
                        let provider = match m.text("provider") {
                            "i" => "icu",
                            "b" => "builtin",
                            _ => "libc",
                        };
                        let locale = match m.get("locale") {
                            Some(l) if !l.is_empty() => format!(" {} {}", if provider == "icu" { "icu_locale" } else { "builtin_locale" }, literal(l)),
                            _ => String::new(),
                        };
                        // `locale_provider` is Postgres 15's: before it, every database is libc's. The
                        // statement runs on the shadows' server, which `shadow_url` may make another.
                        let shadow_version: i64 = pg_rows(&admin, "select current_setting('server_version_num') as v").await?.first().and_then(|r| r.first()?.parse().ok()).unwrap_or(0);
                        let provider = if shadow_version >= 150000 { format!(" locale_provider {provider}{locale}") } else { String::new() };
                        let mut spelled = |name: &str| match locale_spelled(name, &known) {
                            Some(on_shadow) => on_shadow,
                            None => {
                                unknown_locale = Some(format!(
                                    "This database's locale, {name}, is one the shadow server has under no spelling (its own is {}): set shadow_url in .reylo.yaml to a server that has it",
                                    t.text("collate")
                                ));
                                name.to_string()
                            }
                        };
                        let (collate, ctype) = (spelled(m.text("collate")), spelled(m.text("ctype")));
                        format!(" template template0 encoding {} lc_collate {} lc_ctype {}{provider}", literal(m.text("enc")), literal(&collate), literal(&ctype))
                    }
                    _ => String::new(),
                };
                let mut settings: Vec<String> = target
                    .rows("select unnest(setconfig) as s from pg_db_role_setting where setrole = 0 and setdatabase = (select oid from pg_database where datname = current_database())")
                    .await?
                    .iter()
                    .filter_map(|r| r.get("s").map(String::from))
                    .collect();
                // Whether the target is a platform's, which every shadow takes from it.
                let setting = crate::catalog::PLATFORM_SCHEMAS_SETTING;
                let names = target.rows(&format!("select ({})::text as on", crate::catalog::platform_names())).await?.first().is_some_and(|r| r.text("on") == "true");
                settings.retain(|s| !s.starts_with(&format!("{setting}=")));
                settings.push(format!("{setting}={}", if names { "on" } else { "off" }));
                let privileges = default_privileges(target).await?;
                let extensions: Vec<(String, String)> = target
                    .rows("select e.extname as name, n.nspname as schema from pg_extension e join pg_namespace n on n.oid = e.extnamespace where e.extname <> 'plpgsql' order by 1")
                    .await?
                    .iter()
                    .map(|r| (r.text("name").to_string(), r.text("schema").to_string()))
                    .collect();
                let roles = crate::catalog::PLATFORM_ROLES.iter().map(|r| literal(r)).collect::<Vec<_>>().join(", ");
                // Not `public`, which Postgres 14 and older made the bootstrap superuser's, a platform's
                // role on Supabase.
                let platform: Vec<String> = target
                    .rows(&format!("select nspname as n from pg_namespace where pg_get_userbyid(nspowner) in ({roles}) and nspname not like 'pg\\_%' and nspname <> 'information_schema' and nspname <> 'public' order by 1"))
                    .await?
                    .iter()
                    .map(|r| r.text("n").to_string())
                    .collect();
                let environment_role = if platform.is_empty() {
                    None
                } else {
                    // Postgres 16 grants `SET ROLE` apart from membership; before it, membership is it.
                    let can = if version >= 160000 { "SET" } else { "MEMBER" };
                    target.rows(&format!("select rolname as r from pg_roles where rolname in ({roles}) and pg_has_role(current_user, oid, '{can}') order by rolname limit 1")).await?.first().map(|r| r.text("r").to_string())
                };
                // Made where the shadows may become one of the platform's roles.
                let (environment, left_out) = if environment_role.is_some() { platform_ddl(target, url, version, &platform).await? } else { (vec![], None) };
                let schemas: Vec<String> = std::iter::once("public".to_string()).chain(extensions.iter().map(|(_, s)| s.clone())).filter(|s| !platform.contains(s)).collect();
                let schema_grants = schema_grants(target, &schemas).await?;
                let mut owned = owned_by_platform(target, &admin, url, version).await?;
                // What every shadow leaves out, said once.
                owned.note = [left_out, owned.note.take()].into_iter().flatten().reduce(|a, b| format!("{a}\n{b}"));
                // On Supabase, the default privileges its Data API's roles have, the target's own,
                // which Supabase sets for every project as it chooses; and its publication.
                let platform_privileges = if names { platform_defaults(api_settings(&api_privileges(&privileges), &[])).0 } else { vec![] };
                // pg_cron is only ever made in its database, so one that has it is that; otherwise the setting says,
                // which pg_cron lets only a superuser and pg_read_all_settings read: unread, it names no database.
                let pg_cron_target = !target.rows("select 1 from pg_extension where extname = 'pg_cron'").await?.is_empty()
                    || target.rows("select (current_setting('cron.database_name', true) = current_database())::text as on").await.ok().is_some_and(|r| r.first().is_some_and(|r| r.text("on") == "true"));
                let publication = names && !target.rows(&format!("select 1 from pg_publication where pubname = {}", literal(crate::catalog::PLATFORM_PUBLICATION))).await?.is_empty();
                Ok(Server::Postgres {
                    admin,
                    url: server_url.to_string(),
                    create,
                    locale: unknown_locale,
                    settings,
                    privileges,
                    copy_privileges: true,
                    extensions,
                    leave: vec![],
                    environment,
                    environment_role,
                    owned,
                    schema_grants,
                    copy_schema_grants: true,
                    platform_privileges,
                    platform_schema_grants: vec![],
                    platform: names,
                    began_extensions: None,
                    publication,
                    pg_cron_target,
                    pg_cron_stuck: false,
                })
            }
            Dialect::Mysql => {
                let admin = crate::db::open_mysql(&with_database(server_url, "")).await?;
                let defaults = target
                    .rows("select @@character_set_database as cs, @@collation_database as co")
                    .await?
                    .into_iter()
                    .next()
                    .and_then(|r| Some((r.get("cs")?.to_string(), r.get("co")?.to_string())));
                Ok(Server::Mysql { admin, url: server_url.to_string(), defaults })
            }
            Dialect::Sqlite => Ok(Server::Sqlite { libsql: target.is_libsql() }),
        }
    }

    pub fn dialect(&self) -> Dialect {
        match self {
            Server::Postgres { .. } => Dialect::Postgres,
            Server::Mysql { .. } => Dialect::Mysql,
            Server::Sqlite { .. } => Dialect::Sqlite,
        }
    }

    /// A new, empty scratch database beside the target.
    pub async fn shadow(&mut self) -> Result<Shadow, Error> {
        match self {
            Server::Postgres { .. } => {
                let mut shadow = self.pg_shadow(None).await?;
                // pg_cron, where the target has it, refuses the shadow's own name.
                if self.copies_pg_cron() && shadow.db.rows("select 1 from pg_extension where extname = 'pg_cron'").await.is_ok_and(|r| r.is_empty()) {
                    let (mut named, renamed) = self.name_for_pg_cron(shadow).await?;
                    if renamed == Ok(true) {
                        let _ = named.db.exec("create extension if not exists pg_cron").await;
                        named = self.unname_for_pg_cron(named).await?;
                    }
                    shadow = named;
                }
                Ok(shadow)
            }
            Server::Mysql { admin, url, defaults } => {
                let name = random_name();
                admin.query_drop(format!("select get_lock('{name}', 0)")).await.map_err(mysql_error)?;
                let charset = match defaults {
                    Some((cs, co)) => format!(" character set {cs} collate {co}"),
                    None => String::new(),
                };
                admin.query_drop(format!("create database `{name}`{charset}")).await.map_err(|e| match &e {
                    mysql_async::Error::Server(s) => match mysql_create_refused(s.code, &s.message) {
                        Some(may_create) => cannot_create(mysql_error(e), may_create),
                        None => mysql_error(e),
                    },
                    _ => mysql_error(e),
                })?;
                let db = Db::Mysql(crate::db::open_mysql(&with_database(url, &name)).await?);
                Ok(Shadow { db, name })
            }
            // A libSQL database's shadows are on libSQL's engine, as the database is: its SQL, and the
            // version of SQLite Turso Cloud runs.
            Server::Sqlite { libsql: true } => Ok(Shadow { db: Db::LibsqlLocal(crate::libsql_local::Local::memory()?), name: ":memory:".into() }),
            Server::Sqlite { libsql: false } => Ok(Shadow { db: Db::Sqlite(rusqlite::Connection::open_in_memory().map_err(sqlite_error)?), name: ":memory:".into() }),
        }
    }

    /// A Postgres shadow: a new database made as the target's, or a copy of `template`, a replay
    /// cache, which already holds what a new shadow is given (its encoding, locale and default
    /// privileges) but not its database settings, which a copy does not take. The lock named after
    /// it is held until it is dropped; a copy that fails releases it.
    pub(crate) async fn pg_shadow(&mut self, template: Option<&str>) -> Result<Shadow, Error> {
        let Server::Postgres { admin, url, create, locale, settings, privileges, copy_privileges, extensions, leave, environment, environment_role, owned, schema_grants, copy_schema_grants, platform_privileges, platform_schema_grants, publication, began_extensions, .. } = self else {
            return Err(Error("not a Postgres server".into()));
        };
        let name = random_name();
        admin.batch_execute(&format!("select pg_advisory_lock(hashtextextended('{name}', 0))")).await.map_err(pg_error)?;
        let made = async {
            let made = match template {
                Some(t) => admin.batch_execute(&format!("create database \"{name}\" template \"{t}\"")).await,
                None => admin.batch_execute(&format!("create database \"{name}\"{create}")).await,
            };
            if let Err(e) = made {
                let refused = e.as_db_error().map(|d| d.message().to_string()).unwrap_or_default();
                if let (Some(why), true) = (locale.as_ref(), refused.contains("locale")) {
                    return Err(Error(format!("{refused}. {why}")));
                }
                if e.code() != Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE) {
                    return Err(pg_error(e));
                }
                let may = pg_rows(admin, "select (rolcreatedb or rolsuper)::text as may from pg_roles where rolname = current_user").await?;
                return Err(cannot_create(pg_error(e), may.first().is_some_and(|r| r.text("may") == "true")));
            }
            for s in settings.iter() {
                let Some((key, value)) = s.split_once('=') else { continue };
                let value = if PG_LISTS.contains(&key) {
                    value.split(',').map(|v| literal(v.trim().trim_matches('"'))).collect::<Vec<_>>().join(", ")
                } else {
                    literal(value)
                };
                // One the connected role may not set is the platform's (Supabase's `app.settings.*`,
                // which only its administrator sets), and no statement of the project's reads it.
                if let Err(e) = admin.batch_execute(&format!("alter database \"{name}\" set {key} = {value}")).await {
                    if e.code() != Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE) {
                        return Err(pg_error(e));
                    }
                }
            }
            crate::db::open_postgres(&with_database(url, &name)).await
        }
        .await;
        let client = match made {
            Ok(client) => client,
            Err(e) if template.is_some() => {
                let _ = admin.batch_execute(&format!("drop database if exists \"{name}\" with (force)")).await;
                let _ = admin.batch_execute(&format!("select pg_advisory_unlock(hashtextextended('{name}', 0))")).await;
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        let mut db = Db::Postgres(client);
        // For the session too, where the database may not take the setting from the role connected.
        mark_platform_names(&mut db, settings).await;
        if template.is_none() {
            // A role or schema the shadow's server lacks leaves that one out; the comparison says what follows.
            // Where the project's SQL sets default privileges, it sets them from those a new database
            // of the platform's has, which are none but on Supabase.
            for statement in if *copy_privileges { privileges.iter() } else { platform_privileges.iter() } {
                let _ = db.exec(statement).await;
            }
        }
        if template.is_none() {
            // The target's environment, as the platform made it: its extensions but those the
            // project creates, the privileges on the schemas they and `public` are in, and the
            // platform's own schemas, made as one of its roles. A statement the shadow can't run
            // (one only a superuser may) is left out; what the project then needs of it fails in
            // the replay, which says so.
            // The extensions in a schema of the project's first, which the platform's own may use;
            // those in a platform's schema after it is made, as the platform made it.
            // Supabase's own, where it is known which the target began with, as it began.
            // One it began without that it has elsewhere, where no migration made it, is the
            // environment's there, and copied; one it began with that is elsewhere now was moved there
            // since, which the migrations replay.
            let began = |e: &str| began_extensions.iter().flatten().any(|b| b.eq_ignore_ascii_case(e));
            let begun = |e: &str, s: &str| began_extensions.is_some() && crate::catalog::PLATFORM_EXTENSIONS.iter().any(|p| p.eq_ignore_ascii_case(e)) && (s == "extensions" || began(e));
            let copied: Vec<&(String, String)> = extensions.iter().filter(|(e, s)| !leave.contains(&e.to_lowercase()) && !owned.extensions.iter().any(|(o, _)| o == e) && !begun(e, s)).collect();
            for e in began_extensions.iter().flatten().filter(|e| !leave.contains(&e.to_lowercase())) {
                let _ = db.exec("create schema if not exists extensions").await;
                let _ = db.exec(&format!("create extension if not exists {} with schema extensions", ident(e))).await;
            }
            let platform_schema = |schema: &str| environment.iter().chain(owned.statements.iter()).any(|st| st.trim_start().to_ascii_lowercase().starts_with(&format!("create schema {}", schema.to_ascii_lowercase())));
            for (extension, schema) in copied.iter().filter(|(_, s)| !platform_schema(s)) {
                let _ = db.exec(&format!("create schema if not exists {}", ident(schema))).await;
                let _ = db.exec(&format!("create extension if not exists {} with schema {}", ident(extension), ident(schema))).await;
            }
            for (_, schema) in owned.extensions.iter().filter(|(e, s)| owned.role.is_some() && !leave.contains(&e.to_lowercase()) && !platform_schema(s) && s != "pg_catalog") {
                let _ = db.exec(&format!("create schema if not exists {}", ident(schema))).await;
            }
            for statement in if *copy_schema_grants { schema_grants.iter() } else { platform_schema_grants.iter() } {
                let _ = db.exec(statement).await;
            }
            if let (Some(role), false) = (environment_role.as_deref(), environment.is_empty()) {
                make_as(admin, &mut db, &name, role, environment, &["USAGE"]).await;
            }
            let owned_extensions: Vec<&(String, String)> = owned.extensions.iter().filter(|(e, _)| !leave.contains(&e.to_lowercase())).collect();
            if let Some(role) = owned.role.as_deref() {
                // Its extensions after its schemas, which they may be in, and before the rest of it,
                // which may use them; one no role but a superuser may make, made by a superuser
                // connected and given to the role.
                let after = owned.statements.iter().position(|st| !st.to_ascii_lowercase().starts_with("create schema ")).unwrap_or(owned.statements.len());
                let mut statements = owned.statements[..after].to_vec();
                statements.extend(owned_extensions.iter().map(|(e, s)| format!("create extension if not exists {} with schema {}", ident(e), ident(s))));
                statements.extend(owned.statements[after..].iter().cloned());
                make_as(admin, &mut db, &name, role, &statements, &["USAGE", "CREATE"]).await;
                let superuser = db.rows("select rolsuper::text as s from pg_roles where rolname = current_user").await.ok().is_some_and(|r| r.first().is_some_and(|r| r.text("s") == "true"));
                for (extension, schema) in owned_extensions.iter().filter(|_| superuser) {
                    let missing = db.rows(&format!("select 1 from pg_extension where extname = {}", literal(extension))).await.is_ok_and(|r| r.is_empty());
                    if missing && db.exec(&format!("create extension if not exists {} with schema {}", ident(extension), ident(schema))).await.is_ok() {
                        let given = format!("update pg_catalog.pg_extension set extowner = {}::regrole where extname = {}", literal(&ident(role)), literal(extension));
                        if db.exec(&given).await.is_err() {
                            let _ = db.exec(&format!("drop extension if exists {}", ident(extension))).await;
                        }
                    }
                }
            }
            for (extension, schema) in copied.iter().filter(|(_, s)| platform_schema(s)) {
                let _ = db.exec(&format!("create extension if not exists {} with schema {}", ident(extension), ident(schema))).await;
            }
            if *publication {
                let _ = db.exec(&format!("create publication {}", ident(crate::catalog::PLATFORM_PUBLICATION))).await;
            }
        }
        Ok(Shadow { db, name })
    }

    /// What every shadow leaves out of the target, and why, for the command to say once.
    pub fn note(&self) -> Option<&str> {
        match self {
            Server::Postgres { owned, .. } => owned.note.as_deref(),
            _ => None,
        }
    }

    /// A URL another process opens the shadow at: on Postgres and MySQL its database on the shadows'
    /// server, and on SQLite, whose shadows are in memory, a copy written to `file`.
    pub(crate) async fn url_of(&self, shadow: &mut Shadow, file: &Path) -> Result<String, Error> {
        match self {
            Server::Postgres { url, .. } | Server::Mysql { url, .. } => Ok(with_database(url, &shadow.name)),
            // Copied as the replay cache copies a SQLite shadow, page by page.
            Server::Sqlite { .. } => {
                match &mut shadow.db {
                    Db::Sqlite(conn) => crate::cache::sqlite_backup(conn, &rusqlite::Connection::open(file).map_err(sqlite_error)?)?,
                    Db::LibsqlLocal(local) => local.backup_to(&mut crate::libsql_local::Local::open(&file.to_string_lossy())?)?,
                    _ => return Err(Error("a SQLite shadow that is not in this process".into())),
                }
                Ok(format!("sqlite:{}", file.display()))
            }
        }
    }

    /// A statement on the session that makes shadows, so that a connection kept idle through a long
    /// command (whose lock keeps other runs' sweeps off its shadow) isn't closed as idle.
    pub(crate) async fn ping(&mut self) {
        match self {
            Server::Postgres { admin, .. } => {
                let _ = admin.simple_query("select 1").await;
            }
            Server::Mysql { admin, .. } => {
                let _ = admin.query_drop("select 1").await;
            }
            Server::Sqlite { .. } => {}
        }
    }

    pub async fn drop_shadow(&mut self, shadow: Shadow) {
        let Shadow { db, name } = shadow;
        match self {
            Server::Postgres { admin, .. } => {
                pg_close(admin, db, &name).await;
                let _ = admin.batch_execute(&format!("drop database if exists \"{name}\" with (force)")).await;
                let _ = admin.batch_execute(&format!("select pg_advisory_unlock(hashtextextended('{name}', 0))")).await;
            }
            Server::Mysql { admin, .. } => {
                db.close().await;
                let _ = admin.query_drop(format!("drop database if exists `{name}`")).await;
                mysql_revoke_on(admin, &name).await;
                let _ = admin.query_drop(format!("select release_lock('{name}')")).await;
            }
            Server::Sqlite { .. } => {}
        }
    }

    /// Gives `shadow` the name pg_cron makes itself under ([`PG_CRON_NAME`]) where its own would be
    /// refused, which a shadow's always is, for a target that is the database pg_cron makes itself in,
    /// so that a shadow takes what a deploy will.
    /// The answer says whether it was renamed, for [`Server::unname_for_pg_cron`] to name it back, or
    /// why it couldn't be, which pg_cron's refusal then goes on to show.
    pub(crate) async fn name_for_pg_cron(&mut self, mut shadow: Shadow) -> Result<(Shadow, Result<bool, String>), Error> {
        let Server::Postgres { admin, url, settings, pg_cron_target, pg_cron_stuck, .. } = self else { return Ok((shadow, Ok(false))) };
        if !*pg_cron_target || shadow.name == PG_CRON_NAME {
            return Ok((shadow, Ok(false)));
        }
        // On a server whose pg_cron reads its jobs from a database of that name, a shadow under it would run them.
        let named = pg_rows(admin, "select current_setting('cron.database_name', true) as n").await.unwrap_or_default();
        if named.first().and_then(|r| r.get("n")) == Some(PG_CRON_NAME) {
            return Ok((shadow, Err(format!("the shadows' server's pg_cron runs the jobs of a database named {PG_CRON_NAME}, so no shadow takes that name"))));
        }
        if *pg_cron_stuck {
            return Ok((shadow, Err(format!("another shadow of this run kept the name {PG_CRON_NAME}"))));
        }
        // One shadow at a time goes by the name, on the server: another run's holds it for a step, which
        // this waits out, and a run that ends lets it go.
        admin.batch_execute(&format!("select pg_advisory_lock(hashtextextended('{PG_CRON_NAME}', 0))")).await.map_err(pg_error)?;
        let unlock = format!("select pg_advisory_unlock(hashtextextended('{PG_CRON_NAME}', 0))");
        let ready: Result<Result<(), String>, Error> = async {
            match drop_left_pg_cron(admin).await? {
                Left::None => {}
                Left::Other => return Ok(Err(format!("a database named {PG_CRON_NAME} that is no shadow is already on the shadows' server: drop it to let a shadow take the name"))),
                Left::Kept => return Ok(Err(format!("another run's shadow has the name {PG_CRON_NAME}"))),
            }
            // Marked first, so that a run cut off while the shadow goes by the name leaves it to the next run's sweep.
            admin.batch_execute(&format!("comment on database \"{}\" is {}", shadow.name, literal(&format!("{PG_CRON_NOTE}{}", shadow.name)))).await.map_err(pg_error)?;
            Ok(Ok(()))
        }
        .await;
        match ready {
            Ok(Ok(())) => {}
            Ok(Err(why)) => {
                let _ = admin.batch_execute(&unlock).await;
                return Ok((shadow, Err(why)));
            }
            Err(e) => {
                let _ = admin.batch_execute(&unlock).await;
                return Err(e);
            }
        }
        let Shadow { db, name } = shadow;
        let (db, moved) = match pg_move(admin, url, settings, db, &name, PG_CRON_NAME).await {
            Ok(m) => m,
            Err(e) => {
                let _ = admin.batch_execute(&unlock).await;
                return Err(e);
            }
        };
        shadow = Shadow { db, name };
        if let Err(why) = moved {
            let _ = admin.batch_execute(&format!("comment on database \"{}\" is null", shadow.name)).await;
            let _ = admin.batch_execute(&unlock).await;
            return Ok((shadow, Err(why)));
        }
        Ok((shadow, Ok(true)))
    }

    /// Names back a shadow [`Server::name_for_pg_cron`] renamed. One that can't be keeps the name pg_cron
    /// allows, under which it is dropped, and no other shadow of the run takes it.
    pub(crate) async fn unname_for_pg_cron(&mut self, shadow: Shadow) -> Result<Shadow, Error> {
        let Server::Postgres { admin, url, settings, pg_cron_stuck, .. } = self else { return Ok(shadow) };
        let Shadow { db, name } = shadow;
        let (db, moved) = match pg_move(admin, url, settings, db, PG_CRON_NAME, &name).await {
            Ok(m) => m,
            Err(e) => {
                *pg_cron_stuck = true;
                return Err(e);
            }
        };
        if moved.is_err() {
            *pg_cron_stuck = true;
            return Ok(Shadow { db, name: PG_CRON_NAME.into() });
        }
        let _ = admin.batch_execute(&format!("comment on database \"{name}\" is null")).await;
        let _ = admin.batch_execute(&format!("select pg_advisory_unlock(hashtextextended('{PG_CRON_NAME}', 0))")).await;
        Ok(Shadow { db, name })
    }

    /// Whether a new shadow is to be given pg_cron as the target has it: an extension of the
    /// environment, which the project's files don't create.
    fn copies_pg_cron(&self) -> bool {
        let Server::Postgres { extensions, leave, owned, .. } = self else { return false };
        extensions.iter().any(|(e, _)| e == "pg_cron") && !leave.iter().any(|e| e == "pg_cron") && !owned.extensions.iter().any(|(e, _)| e == "pg_cron")
    }

    /// The extensions the project's own files create, which a shadow leaves to them.
    pub fn leave_extensions(&mut self, names: Vec<String>) {
        if let Server::Postgres { leave, .. } = self {
            *leave = names;
        }
    }

    /// Whether the extension `name` is one every new database of the target's platform has where the
    /// target has it ([`crate::catalog::PLATFORM_EXTENSIONS`]): a shadow has it as the target does,
    /// where the project's files create it only if it isn't there, and a reset leaves it.
    pub fn platform_extension(&self, name: &str) -> bool {
        let Server::Postgres { extensions, platform: true, began_extensions, .. } = self else { return false };
        if let Some(began) = began_extensions {
            return began.iter().any(|b| b.eq_ignore_ascii_case(name));
        }
        crate::catalog::PLATFORM_EXTENSIONS.iter().any(|p| p.eq_ignore_ascii_case(name)) && extensions.iter().any(|(e, s)| e.eq_ignore_ascii_case(name) && s == "extensions")
    }

    /// The schema the target has an extension in, by name.
    pub fn extension_schema(&self, name: &str) -> Option<&str> {
        let Server::Postgres { extensions, .. } = self else { return None };
        extensions.iter().find(|(e, _)| e.eq_ignore_ascii_case(name)).map(|(_, s)| s.as_str())
    }

    /// Which of Supabase's own extensions the target began with, where that is known.
    pub fn began_extensions(&self) -> Vec<String> {
        match self {
            Server::Postgres { began_extensions: Some(began), platform: true, .. } => began.clone(),
            _ => vec![],
        }
    }

    /// Which of Supabase's own extensions the target began with, which its shadows begin with.
    pub fn began_with_extensions(&mut self, names: Vec<String>) {
        if let Server::Postgres { began_extensions, platform: true, .. } = self {
            *began_extensions = Some(names);
        }
    }

    /// The schemas the project's own files create: an extension the target keeps in one of them (one
    /// the project once created there, and no longer does) is left out of a shadow too, whose
    /// `create schema` it would otherwise stop.
    pub fn leave_schemas(&mut self, schemas: Vec<String>) {
        if let Server::Postgres { leave, extensions, .. } = self {
            leave.extend(extensions.iter().filter(|(_, s)| schemas.iter().any(|x| x.eq_ignore_ascii_case(s))).map(|(e, _)| e.to_lowercase()));
        }
    }

    /// Whether shadows take the target's default privileges: not where the project's own SQL sets
    /// default privileges, which would then grant on what its earlier migrations made.
    pub fn copy_default_privileges(&mut self, on: bool) {
        if let Server::Postgres { copy_privileges, .. } = self {
            *copy_privileges = on;
        }
    }

    /// Shadows leave what a new database of the platform's has to the project's own files, which
    /// make it, as a project begun before Reylo took it for the platform's did: its publication, and
    /// its default privileges and grants for the Data API.
    pub fn leave_platform(&mut self) {
        if let Server::Postgres { publication, platform_privileges, platform_schema_grants, platform, .. } = self {
            *publication = false;
            *platform = false;
            platform_privileges.clear();
            platform_schema_grants.clear();
        }
    }

    /// Whether the target is a Supabase database whose shadows begin as Supabase makes one.
    pub fn is_platform(&self) -> bool {
        matches!(self, Server::Postgres { platform: true, .. })
    }

    /// Shadows begin with no default privileges at all, so that a grant to the Data API's roles is
    /// one the SQL states.
    pub fn grant_by_default_nothing(&mut self) {
        if let Server::Postgres { copy_privileges, platform_privileges, .. } = self {
            *copy_privileges = false;
            platform_privileges.clear();
        }
    }

    /// The settings the target began under ([`API_SETTINGS`]), which its shadows begin under.
    pub fn began_with(&mut self, on: [bool; 2]) {
        if let Server::Postgres { platform_privileges, platform: true, .. } = self {
            *platform_privileges = platform_defaults(on).0;
        }
    }

    /// Shadows take none of the target's default privileges or grants on its schemas: for SQL written
    /// to make the database whole (init's, introspect's), which makes them itself. Which of them a new
    /// database on the server has already, no name tells, so they are all the SQL's, but on Supabase,
    /// whose new databases have their own, which shadows take.
    pub fn copy_no_privileges(&mut self) {
        if let Server::Postgres { copy_privileges, copy_schema_grants, platform_schema_grants, platform, .. } = self {
            *copy_privileges = false;
            *copy_schema_grants = false;
            if *platform {
                *platform_schema_grants = platform_defaults([true, true]).1;
            }
        }
    }

    /// Drops the shadows a run that died left behind, and the replay caches nobody will use; answers
    /// how many shadows.
    pub async fn sweep(&mut self) -> Result<usize, Error> {
        let mut dropped = 0;
        match self {
            Server::Postgres { admin, .. } => {
                for row in pg_rows(admin, &format!("select datname from pg_database where datname like '{SHADOW_PREFIX}%'")).await? {
                    let name = row.text("datname").to_string();
                    let free = pg_rows(admin, &format!("select pg_try_advisory_lock(hashtextextended('{name}', 0)) as free")).await?.first().is_some_and(|r| r.flag("free"));
                    if !free {
                        continue;
                    }
                    // Another role's (a teammate's, on a shared server) is that role's to drop.
                    let drop = admin.batch_execute(&format!("drop database if exists \"{name}\" with (force)")).await;
                    admin.batch_execute(&format!("select pg_advisory_unlock(hashtextextended('{name}', 0))")).await.map_err(pg_error)?;
                    match drop {
                        Ok(()) => dropped += 1,
                        Err(e) if e.code() == Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE) => {}
                        Err(e) => return Err(pg_error(e)),
                    }
                }
            }
            Server::Mysql { admin, .. } => {
                let names: Vec<String> = admin.query(format!("select schema_name from information_schema.schemata where schema_name like '{SHADOW_PREFIX}%'")).await.map_err(mysql_error)?;
                for name in names {
                    let free: Option<i64> = admin.query_first(format!("select is_free_lock('{name}')")).await.map_err(mysql_error)?;
                    if free != Some(1) {
                        continue;
                    }
                    match admin.query_drop(format!("drop database if exists `{name}`")).await {
                        Ok(()) => {
                            mysql_revoke_on(admin, &name).await;
                            dropped += 1
                        }
                        Err(mysql_async::Error::Server(s)) if s.code == 1044 => {}
                        Err(e) => return Err(mysql_error(e)),
                    }
                }
            }
            Server::Sqlite { .. } => {}
        }
        // A shadow a run cut off while it went by pg_cron's name.
        if let Server::Postgres { admin, .. } = self {
            let name = format!("hashtextextended('{PG_CRON_NAME}', 0)");
            if pg_rows(admin, &format!("select 1 from pg_database where datname = '{PG_CRON_NAME}'")).await?.first().is_some()
                && pg_rows(admin, &format!("select pg_try_advisory_lock({name}) as ok")).await?.first().is_some_and(|r| r.flag("ok"))
            {
                if matches!(drop_left_pg_cron(admin).await, Ok(Left::None)) && pg_rows(admin, &format!("select 1 from pg_database where datname = '{PG_CRON_NAME}'")).await?.is_empty() {
                    dropped += 1;
                }
                let _ = admin.batch_execute(&format!("select pg_advisory_unlock({name})")).await;
            }
        }
        // The replay caches no run will use, which are not counted: they are not a run's leftovers.
        crate::cache::sweep(self).await?;
        Ok(dropped)
    }

    pub async fn close(self) {
        if let Server::Mysql { admin, .. } = self {
            let _ = admin.disconnect().await;
        }
    }
}

/// The one name pg_cron's install takes besides the database `cron.database_name` names: its own
/// tests'. A shadow goes by it while it makes pg_cron. pg_cron's worker reads jobs only from the named
/// database, so one a migration schedules in a shadow never runs.
pub(crate) const PG_CRON_NAME: &str = "contrib_regression";
/// How the comment on a shadow going by [`PG_CRON_NAME`] begins, before its own name.
#[cfg(feature = "cli")]
const PG_CRON_NOTE: &str = "reylo shadow ";

/// What is left on the shadows' server under [`PG_CRON_NAME`].
#[cfg(feature = "cli")]
enum Left {
    /// Nothing, or a shadow a run cut off left there (marked as its shadow, whose lock is free), now dropped.
    None,
    /// A database that is no shadow, which stays.
    Other,
    /// A shadow a live run holds, or one the role may not drop.
    Kept,
}

#[cfg(feature = "cli")]
async fn drop_left_pg_cron(admin: &tokio_postgres::Client) -> Result<Left, Error> {
    let left = pg_rows(admin, &format!("select shobj_description(oid, 'pg_database') as note from pg_database where datname = '{PG_CRON_NAME}'")).await?;
    let Some(row) = left.first() else { return Ok(Left::None) };
    let Some(own) = row.get("note").and_then(|n| n.strip_prefix(PG_CRON_NOTE)).map(String::from) else { return Ok(Left::Other) };
    // A session on it is a live run's, whose lock may be in another database than this one's: advisory
    // locks are the database's they're taken in.
    if pg_rows(admin, &format!("select 1 from pg_stat_activity where datname = '{PG_CRON_NAME}'")).await?.first().is_some() {
        return Ok(Left::Kept);
    }
    let lock = format!("hashtextextended({}, 0)", literal(&own));
    if !pg_rows(admin, &format!("select pg_try_advisory_lock({lock}) as free")).await?.first().is_some_and(|r| r.flag("free")) {
        return Ok(Left::Kept);
    }
    let dropped = admin.batch_execute(&format!("drop database if exists \"{PG_CRON_NAME}\" with (force)")).await.is_ok();
    let _ = admin.batch_execute(&format!("select pg_advisory_unlock({lock})")).await;
    Ok(if dropped { Left::None } else { Left::Kept })
}

/// Renames the database `from`, whose session is `db`, to `to`, and answers a session on it under
/// whichever name it has after, with why it couldn't be renamed where it wasn't. A server may keep a
/// session of its own on a database a moment after the last client leaves (Neon's), so one in use is
/// tried again, each try waiting up to Postgres's own five seconds; a pooler that keeps one open for
/// minutes (Supabase's session pooler) leaves it as it was.
#[cfg(feature = "cli")]
async fn pg_move(admin: &tokio_postgres::Client, url: &str, settings: &[String], db: Db, from: &str, to: &str) -> Result<(Db, Result<(), String>), Error> {
    pg_close(admin, db, from).await;
    let mut moved = Err(String::new());
    for _ in 0..3 {
        match admin.batch_execute(&format!("alter database \"{from}\" rename to \"{to}\"")).await {
            Ok(()) => {
                moved = Ok(());
                break;
            }
            Err(e) => {
                let busy = e.code() == Some(&tokio_postgres::error::SqlState::OBJECT_IN_USE);
                moved = Err(match busy {
                    true => "another session, such as a connection pooler's, kept the shadow database open: connect without the pooler".to_string(),
                    false => e.as_db_error().map(|d| d.message().to_string()).unwrap_or_else(|| e.to_string()),
                });
                if !busy {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }
    }
    let at = if moved.is_ok() { to } else { from };
    let mut db = Db::Postgres(open_postgres(&with_database(url, at)).await?);
    mark_platform_names(&mut db, settings).await;
    Ok((db, moved))
}

/// Ends a Postgres shadow's session, and waits up to a second for its server process to go: a
/// database with a session left cannot be dropped, renamed or copied without the server's own wait
/// for it, a tenth of a second at a time.
#[cfg(feature = "cli")]
pub(crate) async fn pg_close(admin: &tokio_postgres::Client, db: Db, name: &str) {
    drop(db);
    for _ in 0..200 {
        match pg_rows(admin, &format!("select count(*) as n from pg_stat_activity where datname = {}", literal(name))).await {
            Ok(rows) if rows.first().is_some_and(|r| r.text("n") != "0") => tokio::time::sleep(std::time::Duration::from_millis(5)).await,
            _ => return,
        }
    }
}

/// The default privileges of the database `target` is connected to, as the statements that make
/// them: each grant, and on functions and types the revoke from PUBLIC of what Postgres grants by
/// default, for the roles and in the schemas they name (the whole database where none). Those of the
/// role that runs the migrations name none, so that they are the defaults of whoever runs them: a
/// migration made by a developer's role runs in production as the deploy's. A platform's role's are
/// left out (Neon's `cloud_admin`'s, on every database its API makes): they grant only on what that
/// role makes, which is the platform's, and only that role may set them, so no shadow and no
/// migration can.
pub(crate) async fn default_privileges(target: &mut Db) -> Result<Vec<String>, Error> {
    let rows = target
        .rows(&format!(
            "select format('%s', d.defaclrole::regrole) as role, d.defaclrole = {} as mine,
              case when d.defaclnamespace <> 0 then format('%I', n.nspname) end as schema, d.defaclobjtype as kind,
              format('%s', (select json_agg(json_build_object('grantee', case when x.grantee = 0 then 'public' else quote_ident(pg_get_userbyid(x.grantee)) end, 'privilege', x.privilege_type, 'grantable', x.is_grantable))
                 from aclexplode(d.defaclacl) x where x.grantee <> d.defaclrole)) as acl
            from pg_default_acl d left join pg_namespace n on n.oid = d.defaclnamespace
            where {} and (d.defaclnamespace = 0 or {})",
            crate::catalog::runner(),
            crate::catalog::not_platform("d.defaclrole"),
            // In a platform's schema, they are the platform's (Supabase's own in `storage`).
            crate::catalog::not_platform_schema("n")
        ))
        .await?;
    let mut out = Vec::new();
    for r in rows {
        let objects = match r.text("kind") {
            "r" => "tables",
            "S" => "sequences",
            "f" => "functions",
            "T" => "types",
            "n" => "schemas",
            "L" => "large objects",
            _ => continue,
        };
        let role = if r.flag("mine") { String::new() } else { format!(" for role {}", r.text("role")) };
        let head = format!("alter default privileges{role}{}", r.get("schema").map(|s| format!(" in schema {s}")).unwrap_or_default());
        let grants: Vec<serde_json::Value> = serde_json::from_str(r.text("acl")).unwrap_or_default();
        let public = |privilege: &str| grants.iter().any(|g| g["grantee"] == "public" && g["privilege"] == privilege);
        if r.get("schema").is_none() && objects == "functions" && !public("EXECUTE") {
            out.push(format!("{head} revoke execute on functions from public"));
        }
        if r.get("schema").is_none() && objects == "types" && !public("USAGE") {
            out.push(format!("{head} revoke usage on types from public"));
        }
        for g in &grants {
            let option = if g["grantable"].as_bool().unwrap_or(false) { " with grant option" } else { "" };
            out.push(format!("{head} grant {} on {objects} to {}{option}", g["privilege"].as_str().unwrap_or_default().to_lowercase(), g["grantee"].as_str().unwrap_or_default()));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod url_tests {
    use super::*;

    #[test]
    fn a_supabase_databases_settings_are_read_from_what_its_sql_leaves_alone() {
        let all = |objects: &str, privileges: &[&str], roles: &[&str]| -> Vec<ApiPrivilege> { roles.iter().flat_map(|r| privileges.iter().map(move |p| (objects.to_string(), r.to_string(), p.to_string()))).collect() };
        let roles = ["anon", "authenticated", "service_role"];
        let tables = ["DELETE", "INSERT", "SELECT", "UPDATE"];
        let exposed: Vec<ApiPrivilege> = [all("tables", &tables, &roles), all("sequences", &["SELECT", "USAGE"], &roles), all("functions", &["EXECUTE"], &roles)].concat();
        assert_eq!(api_settings(&exposed, &[]), [true, true]);
        assert_eq!(api_settings(&all("functions", &["EXECUTE"], &roles), &[]), [false, true]);
        // Taken from anon alone, the rest say how it started; and a change to anon alone says nothing.
        let hidden_from_anon: Vec<ApiPrivilege> = exposed.iter().filter(|(_, r, _)| r != "anon").cloned().collect();
        assert_eq!(api_settings(&hidden_from_anon, &[]), [true, true]);
        assert_eq!(api_settings(&all("tables", &["SELECT"], &["authenticated"]), &all("tables", &["SELECT"], &["authenticated"])), [false, false]);
        // Every one changed says nothing either: it started as Supabase's projects once all did.
        assert_eq!(api_settings(&[], &exposed), [true, true]);
        assert_eq!(api_settings_in_note(&api_settings_note([false, true], &[])), Some([false, true]));
        assert_eq!(api_settings_in_note(&api_settings_note([true, false], &[])), Some([true, false]));
        assert_eq!(extensions_in_note(&api_settings_note([true, true], &["pgcrypto".into(), "uuid-ossp".into()])), Some(vec!["pgcrypto".to_string(), "uuid-ossp".to_string()]));
        assert_eq!(extensions_in_note(&api_settings_note([true, true], &[])), Some(vec![]));
        assert_eq!(extensions_in_note("Reylo Migrate's history. This database began with Supabase's Data API settings: new tables exposed, new functions exposed."), None);
        assert_eq!(api_settings_in_note("the history"), None);
        let (privileges, _) = platform_defaults([false, true]);
        assert!(privileges.contains(&"alter default privileges for role postgres in schema public revoke delete, insert, select, update on tables from anon".to_string()), "{privileges:?}");
    }

    #[test]
    fn a_table_change_runs_strict_where_the_session_isnt() {
        assert_eq!(strict_mode("ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES,NO_ZERO_DATE"), None);
        assert_eq!(strict_mode("TRADITIONAL"), None);
        assert_eq!(strict_mode("").as_deref(), Some("ALLOW_INVALID_DATES,STRICT_ALL_TABLES"));
        assert_eq!(strict_mode("NO_ENGINE_SUBSTITUTION").as_deref(), Some("NO_ENGINE_SUBSTITUTION,ALLOW_INVALID_DATES,STRICT_ALL_TABLES"));
        assert_eq!(strict_mode("ANSI_QUOTES,NO_ZERO_IN_DATE,NO_ZERO_DATE,ERROR_FOR_DIVISION_BY_ZERO,ALLOW_INVALID_DATES").as_deref(), Some("ANSI_QUOTES,ALLOW_INVALID_DATES,STRICT_ALL_TABLES"));
    }

    #[test]
    fn a_table_change_keeps_a_zero_a_column_made_auto_increment_holds() {
        assert_eq!(table_change_mode("STRICT_TRANS_TABLES").as_deref(), Some("STRICT_TRANS_TABLES,NO_AUTO_VALUE_ON_ZERO"));
        assert_eq!(table_change_mode("").as_deref(), Some("ALLOW_INVALID_DATES,STRICT_ALL_TABLES,NO_AUTO_VALUE_ON_ZERO"));
        assert_eq!(table_change_mode("STRICT_TRANS_TABLES,NO_AUTO_VALUE_ON_ZERO"), None);
        assert_eq!(table_change_mode("NO_AUTO_VALUE_ON_ZERO").as_deref(), Some("NO_AUTO_VALUE_ON_ZERO,ALLOW_INVALID_DATES,STRICT_ALL_TABLES"));
    }

    #[test]
    fn a_locale_is_spelled_as_the_shadow_server_spells_it() {
        let mac: Vec<String> = ["C", "POSIX", "en_US.UTF-8", "de_DE.ISO8859-1", "sr_YU.UTF-8@latin"].map(String::from).to_vec();
        let linux: Vec<String> = ["C", "POSIX", "C.utf8", "en_US.utf8", "de_DE.iso88591"].map(String::from).to_vec();
        assert_eq!(locale_spelled("en_US.utf8", &mac).as_deref(), Some("en_US.UTF-8"));
        assert_eq!(locale_spelled("en_US.UTF-8", &linux).as_deref(), Some("en_US.utf8"));
        assert_eq!(locale_spelled("de_DE.iso88591", &mac).as_deref(), Some("de_DE.ISO8859-1"));
        assert_eq!(locale_spelled("sr_YU.utf8@latin", &mac).as_deref(), Some("sr_YU.UTF-8@latin"));
        assert_eq!(locale_spelled("C.UTF-8", &linux).as_deref(), Some("C.utf8"));
        assert_eq!(locale_spelled("C", &linux).as_deref(), Some("C"));
        // Another language, or another encoding, is no spelling of it.
        assert_eq!(locale_spelled("en_GB.utf8", &mac), None);
        assert_eq!(locale_spelled("en_US.iso88591", &mac), None);
        assert_eq!(locale_spelled("en_US", &mac), None);
    }

    #[test]
    fn a_refused_shadow_is_told_what_helps() {
        // A role that may create databases, refused by the server by name, keeps the server's words and is sent to shadow_url alone.
        let e = cannot_create(Error("tsdb_admin: database reylo_shadow_1 is not an allowed database name. Contact your administrator".into()), true);
        assert!(e.0.starts_with("tsdb_admin: database reylo_shadow_1 is not an allowed database name. Contact your administrator. "), "{}", e.0);
        assert!(e.0.contains("shadow_url") && !e.0.contains("give the role the right"), "{}", e.0);
        let e = cannot_create(Error("permission denied to create database".into()), false);
        assert!(e.0.contains("give the role the right to create databases") && e.0.contains("shadow_url"), "{}", e.0);
    }

    #[test]
    #[cfg(feature = "cli")]
    fn a_mysql_refusal_to_create_a_database_is_told_what_helps() {
        assert_eq!(mysql_create_refused(1044, "Access denied for user 'app'@'%' to database 'reylo_shadow_1'"), Some(false));
        assert_eq!(mysql_create_refused(1227, "Access denied; you need (at least one of) the CREATE privilege(s) for this operation"), Some(false));
        assert_eq!(mysql_create_refused(1105, "VT12001: unsupported: create database by failDBDDL"), Some(true));
        assert_eq!(mysql_create_refused(1105, "VT12001: unsupported: something else"), None);
        assert_eq!(mysql_create_refused(1007, "Can't create database 'reylo_shadow_1'; database exists"), None);
    }

    #[test]
    fn a_password_holding_a_hash_stays_a_password() {
        let url = "postgres://postgres:pa#ss@localhost:5432/app?sslmode=require";
        assert_eq!(with_database(url, "shadow"), "postgres://postgres:pa#ss@localhost:5432/shadow?sslmode=require");
        assert_eq!(database_of(url), "app");
        assert_eq!(shown_url(url), "postgres://localhost:5432/app");
    }

    #[test]
    fn a_heading_never_shows_credentials_or_settings() {
        assert_eq!(shown_url("postgres://u:p@db.example.com/app"), "postgres://db.example.com/app");
        assert_eq!(shown_url("mysql://root@127.0.0.1:3306/app?ssl=true"), "mysql://127.0.0.1:3306/app");
        assert_eq!(shown_url("postgres://u:pa/ss@host/app"), "postgres://host/app");
        assert_eq!(shown_url("postgres://host/app?password=secret"), "postgres://host/app");
        assert_eq!(shown_url("libsql://app-acme.turso.io?authToken=secret"), "libsql://app-acme.turso.io");
        // A SQLite file's URL is its name, an @ and a ? in it included, each naming another file
        // without what comes before or after it.
        assert_eq!(shown_url("sqlite://team@prod.db"), "sqlite://team@prod.db");
        assert_eq!(shown_url("file:team@prod.db?mode=ro"), "file:team@prod.db?mode=ro");
        assert_eq!(sqlite_path("file:team@prod.db?mode=ro", Path::new("/p")), Path::new("/p").join("team@prod.db?mode=ro").to_string_lossy());
        // An @ past the host is no part of the credentials, where a connection is made.
        assert_eq!(with_database("postgres://u@host/d@b", "x"), "postgres://u@host/x");
        assert_eq!(with_database("postgres://host/app?application_name=a@b", "x"), "postgres://host/x?application_name=a@b");
    }

    #[test]
    fn a_transaction_pooler_is_refused_with_the_direct_connection() {
        let neon = transaction_pooler("postgresql://neondb_owner:secret@ep-late-darkness-bb241gty-pooler.c-15.us-east-1.aws.neon.tech/neondb?sslmode=require&channel_binding=require", "direct_url").expect("Neon's pooled host");
        assert!(neon.ends_with("host ep-late-darkness-bb241gty.c-15.us-east-1.aws.neon.tech, in direct_url in .reylo.yaml or with --url"), "{neon}");
        assert!(neon.starts_with("postgresql://ep-late-darkness-bb241gty-pooler.c-15.us-east-1.aws.neon.tech/neondb is Neon's"), "{neon}");
        assert!(!neon.contains("secret"), "{neon}");
        let supabase = transaction_pooler("postgres://postgres.abc:secret@aws-1-us-east-1.pooler.supabase.com:6543/postgres", "shadow_url").expect("Supabase's transaction pooler");
        assert!(supabase.contains("port 5432") && supabase.ends_with("in shadow_url in .reylo.yaml") && !supabase.contains("secret"), "{supabase}");
        // The query's port and host replace the URL's own, as a project's env files write PGPORT and PGHOST.
        assert!(transaction_pooler("postgres://postgres.abc@aws-1-us-east-1.pooler.supabase.com/postgres?port=6543", "").is_some());
        assert!(transaction_pooler("postgres://u@/neondb?host=ep-late-darkness-bb241gty-pooler.c-15.us-east-1.aws.neon.tech", "").is_some());
        assert_eq!(transaction_pooler("postgres://postgres.abc@aws-1-us-east-1.pooler.supabase.com:6543/postgres?port=5432", ""), None);
        // An app's own pool has no project file's key to name.
        let pool = transaction_pooler("postgres://neondb_owner@ep-late-darkness-bb241gty-pooler.c-15.us-east-1.aws.neon.tech:5432/neondb", "").expect("Neon's pooled host");
        assert!(pool.ends_with("host ep-late-darkness-bb241gty.c-15.us-east-1.aws.neon.tech"), "{pool}");
        let dedicated = transaction_pooler("postgres://postgres:secret@db.abc.supabase.co:6543/postgres", "direct_url").expect("Supabase's dedicated pooler");
        assert!(dedicated.contains("dedicated") && dedicated.contains("direct connection, port 5432") && !dedicated.contains("secret"), "{dedicated}");
        assert!(transaction_pooler("postgres://postgres@DB.abc.supabase.co.:6543/postgres", "").is_some(), "the host's absolute form");
        // The direct connections, Supabase's session pooler and any other host are no transaction pooler.
        for url in [
            "postgresql://neondb_owner:secret@ep-late-darkness-bb241gty.c-15.us-east-1.aws.neon.tech/neondb?sslmode=require",
            "postgres://postgres.abc:secret@aws-1-us-east-1.pooler.supabase.com:5432/postgres",
            "postgres://postgres:secret@db.abc.supabase.co:5432/postgres",
            "postgres://u@db.a.b.supabase.co:6543/app",
            "postgres://u@my-pooler.example.com:6543/app",
            "postgres://u@localhost:5432/app",
            "postgres://u@[::1]:6543/app",
            "mysql://u@x-pooler.neon.tech/app",
        ] {
            assert_eq!(transaction_pooler(url, "direct_url"), None, "{url}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::with_database;

    #[test]
    fn a_database_swapped_into_a_url_is_the_one_its_session_opens() {
        assert_eq!(with_database("postgres://u:p@h:5432/app?sslmode=require#x", "reylo_shadow_1"), "postgres://u:p@h:5432/reylo_shadow_1?sslmode=require#x");
        // A dbname setting would name the target over the path.
        assert_eq!(with_database("postgres://u:p@h/app?dbname=app&sslmode=require", "reylo_shadow_1"), "postgres://u:p@h/reylo_shadow_1?sslmode=require");
        assert_eq!(with_database("postgres://u:p@h/app?dbname=app", "postgres"), "postgres://u:p@h/postgres");
        assert_eq!(with_database("postgres://u:p@h/app?%64bname=app", "postgres"), "postgres://u:p@h/postgres");
        assert_eq!(with_database("mysql://root@h:3306", "reylo_shadow_1"), "mysql://root@h:3306/reylo_shadow_1");
    }

    #[test]
    fn pg_dump_takes_a_password_from_the_query_or_the_login_in_its_environment() {
        use super::without_password;
        assert_eq!(without_password("postgres://u:p%40ss@h/db"), ("postgres://u@h/db".to_string(), Some("p@ss".to_string())));
        // A project's env files write PGPASSWORD into the query, which wins over the login's, as libpq reads it.
        assert_eq!(without_password("postgres://u:old@h/db?sslmode=require&password=s3cret"), ("postgres://u@h/db?sslmode=require".to_string(), Some("s3cret".to_string())));
        assert_eq!(without_password("postgres://u@h/db"), ("postgres://u@h/db".to_string(), None));
    }
}

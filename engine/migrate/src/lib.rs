//! `reylo migrate` and its neighbours: migrations written by hand (or by an agent), checked against
//! the declared schema. `reylo migrate deploy --dry-run` builds shadows beside the target: one runs
//! `schema.sql`; the other replays what the target applied, is compared with the live target (the
//! drift check), and then runs what is pending; the two must agree. `reylo migrate deploy` builds none,
//! and trusts the fingerprint its last run stored. Every migration file runs atomically with its
//! history row, so a file that cannot (a statement that cannot run in a transaction beside others;
//! MySQL's DDL beside anything) is refused.
//!
//! Postgres, MySQL and SQLite, a libSQL server's (Turso's) among SQLite's (`libsql`). The commands are in `commands`; the comparator is `catalog`; the
//! lexer that splits and classifies statements is `sql`; the connections and shadows are `db`; the
//! replays the shadows run are `replay`, which the cache of what they reached (`cache`) shortens;
//! `lock` is the project's lock file; `terminal` is the spinner and the SQL's colours an interactive
//! terminal shows.
//!
//! The `cli` feature (on by default) builds every command, with the connections and shadows they
//! open. Without it, only `reylo migrate deploy` and `reylo migrate status` are built, each run on a
//! database the host connects ([`db::Session`]) and reading the project's files from the copies the
//! host holds ([`Embedded`]): what a Rust app's `reylo::migrate!()` runs, with nothing it would not.

// Without the `cli` feature, much of what the commands share is used by none of the two that remain.
#![cfg_attr(not(feature = "cli"), allow(dead_code, unused_imports))]

pub mod cache;
pub mod catalog;
pub mod commands;
pub mod db;
#[cfg(feature = "cli")]
pub mod diff;
pub mod libsql;
#[cfg(feature = "cli")]
pub mod libsql_local;
pub mod lock;
#[cfg(feature = "cli")]
pub mod locks;
#[cfg(feature = "cli")]
mod replay;
pub mod sql;
pub mod terminal;
mod time;

#[cfg(feature = "cli")]
pub use commands::{create, db_introspect, db_push, db_push_watch, db_reset, db_wipe, deploy, dev, dry_run, dry_run_then, history_pull, history_push, init, lock_from_history, run_sql, status, Choices, Push};
pub use commands::{deploy_on_host, status_on_host, Embedded, Options};

/// What went wrong, as a sentence for the person running the command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(e.to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Postgres,
    Mysql,
    Sqlite,
}

impl Dialect {
    /// The database a URL's scheme names.
    pub fn of_url(url: &str) -> Result<Dialect, Error> {
        let lower = url.to_ascii_lowercase();
        let scheme = lower.split_once(':').map(|(s, _)| s).unwrap_or_default();
        match scheme {
            "postgres" | "postgresql" => Ok(Dialect::Postgres),
            "mysql" | "mariadb" => Ok(Dialect::Mysql),
            "sqlite" | "file" => Ok(Dialect::Sqlite),
            // A libSQL server's database, Turso Cloud's among them, named as `@libsql/client` names it.
            "libsql" | "https" | "http" | "wss" | "ws" => Ok(Dialect::Sqlite),
            // ClickHouse, whose statements Reylo types and whose schema it does not migrate.
            // Its URL carries secrets in its query too, a token or a header's value, so the message names the server alone.
            "clickhouse+http" | "clickhouse+https" => Err(Error(format!("Reylo's migrations and reylo run don't support ClickHouse yet, which this URL names: {}", redact(url).split(['?', '#']).next().unwrap_or_default()))),
            // A bare path, or one on a Windows drive (`C:\app.db`): any other scheme names a database Reylo doesn't read.
            _ if scheme.len() <= 1 && (lower == ":memory:" || [".db", ".sqlite", ".sqlite3"].iter().any(|e| lower.ends_with(e))) => Ok(Dialect::Sqlite),
            _ => Err(Error(format!("no database for the URL's scheme: {}", redact(url)))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Dialect::Postgres => "postgres",
            Dialect::Mysql => "mysql",
            Dialect::Sqlite => "sqlite",
        }
    }

    /// How the database reads the text around its tokens: its whitespace, comments, quotes and names.
    pub fn rules(self) -> reylo_engine_core::lexical::Rules {
        use reylo_engine_core::dialect::Dialect as Core;
        reylo_engine_core::lexical::Rules::of(match self {
            Dialect::Postgres => Core::Postgres,
            Dialect::Mysql => Core::Mysql,
            Dialect::Sqlite => Core::Sqlite,
        })
    }

    /// Whether the database is a file the process opens, rather than a server: SQLite.
    pub fn is_file(self) -> bool {
        self == Dialect::Sqlite
    }
}

/// The URL with its user and password left out, for a message: what stands before the last `@`
/// ahead of its query, since a password may hold an `@` or a `/` unescaped. Where no `@` stands
/// there, the first one after it is taken for the login's, as a password that holds a `?` or a `#`
/// would leave it: hiding too much shows no secret.
pub fn redact(url: &str) -> String {
    let url = &without_query_password(url).0;
    let Some(s) = url.find("://") else { return url.to_string() };
    let host = s + 3;
    let end = url[host..].find(['?', '#']).map_or(url.len(), |i| host + i);
    match url[host..end].rfind('@').or_else(|| url[host..].find('@')) {
        Some(at) => format!("{}//…{}", &url[..s + 1], &url[host + at..]),
        None => url.to_string(),
    }
}

/// The URL without a `password` its query names (a name read decoded, as libpq reads one), and the
/// last of them, decoded, as libpq takes it: a project's env files write their `PGPASSWORD` there.
/// A credential in the query is as secret as one in the login, so whatever leaves out the login's
/// for a command line, a message or a cache's key leaves out this one too.
pub fn without_query_password(url: &str) -> (String, Option<String>) {
    let Some((base, query)) = url.split_once('?') else { return (url.to_string(), None) };
    let (query, fragment) = query.split_once('#').map_or((query, None), |(q, f)| (q, Some(f)));
    let mut password = None;
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            let is_password = percent_decode(name) == "password";
            if is_password {
                password = Some(percent_decode(value));
            }
            !is_password
        })
        .collect();
    if password.is_none() {
        return (url.to_string(), None);
    }
    let query = kept.join("&");
    let fragment = fragment.map(|f| format!("#{f}")).unwrap_or_default();
    (if query.is_empty() { format!("{base}{fragment}") } else { format!("{base}?{query}{fragment}") }, password)
}

/// `%XX` escapes decoded, as in a URL's path.
pub(crate) fn percent_decode(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_database_from_the_url() {
        assert_eq!(Dialect::of_url("postgresql://u@h/db").unwrap(), Dialect::Postgres);
        assert_eq!(Dialect::of_url("mysql://root@127.0.0.1:3306/app").unwrap(), Dialect::Mysql);
        assert_eq!(Dialect::of_url("file:./dev.db").unwrap(), Dialect::Sqlite);
        assert_eq!(Dialect::of_url("./dev.db").unwrap(), Dialect::Sqlite);
        assert_eq!(Dialect::of_url("C:\\app.db").unwrap(), Dialect::Sqlite);
        assert_eq!(Dialect::of_url(":memory:").unwrap(), Dialect::Sqlite);
        assert_eq!(Dialect::of_url("libsql://app-acme.turso.io").unwrap(), Dialect::Sqlite);
        assert!(Dialect::of_url("duckdb:./app.db").is_err());
        assert_eq!(percent_decode("a%20b%2"), "a b%2");
        assert_eq!(redact("postgres://u:p@h/db"), "postgres://…@h/db");
        // A password with an `@` or a `/` of its own, unescaped, and one with a `?`, which reads as a query's.
        assert_eq!(redact("mysql://u:p@ss@h:3306/db"), "mysql://…@h:3306/db");
        assert_eq!(redact("mysql://u:pa/ss@h:3306/db?charset=utf8mb4"), "mysql://…@h:3306/db?charset=utf8mb4");
        assert_eq!(redact("mysql://u:pa?ss@h:3306/db"), "mysql://…@h:3306/db");
        assert_eq!(redact("sqlite://app.db"), "sqlite://app.db");
        // A password the query names, as a project's env files write PGPASSWORD, is left out too.
        assert_eq!(redact("postgres://u@h/db?sslmode=require&password=s3cret"), "postgres://…@h/db?sslmode=require");
        assert_eq!(redact("postgres://h/db?%70assword=s3cret#x"), "postgres://h/db#x");
        assert_eq!(without_query_password("postgres://h/db?password=a&password=b%40c&dbname=x"), ("postgres://h/db?dbname=x".to_string(), Some("b@c".to_string())));
        // ClickHouse's URL keeps secrets in its query, which the refusal leaves out.
        let refusal = Dialect::of_url("clickhouse+https://app:p@ss@db.example:8443/shop?access_token=SECRET&http_header_X-Key=SECRET").unwrap_err().0;
        assert!(refusal.ends_with("clickhouse+https://…@db.example:8443/shop") && !refusal.contains("SECRET") && !refusal.contains("ss@"), "{refusal}");
        // A password that holds a `?` is hidden before the query is left out.
        let cut = Dialect::of_url("clickhouse+https://app:pa?ss@db.example:8443/shop?access_token=SECRET").unwrap_err().0;
        assert!(cut.ends_with("clickhouse+https://…@db.example:8443/shop") && !cut.contains("app:pa"), "{cut}");
    }
}

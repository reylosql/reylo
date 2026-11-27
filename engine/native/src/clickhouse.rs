//! The ClickHouse program: a server's HTTP interface, reached from the URL an app gives
//! `@clickhouse/client`, which a project writes after `clickhouse+`, with each statement analyzed by the replay loop of `reylo-engine`, which
//! keeps the connection's answers between statements. HTTP keeps no session, so a connection here
//! is the server's address and its login, checked once when it is opened.

use crate::clients::clickhouse::{self as answers, Describe, Outcome, Server};
use crate::{AnswerCache, Failure, Program, ReplayError};
use reylo_engine_core::api::Analysis;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};

/// The errors a server answers a login it refuses with: no such user, a wrong password, a password
/// the login needs, and no right to connect.
const REFUSED: &[&str] = &["192", "193", "194", "516"];

#[derive(Default)]
pub struct Clickhouse {
    connections: HashMap<u64, Connection>,
    next: u64,
}

struct Connection {
    server: Server,
    /// The server's `version()`, which the schema's fingerprint is written for.
    version: String,
    /// The engine's answers, kept between statements as the loader's session keeps them.
    answers: AnswerCache<Describe>,
    /// The fingerprint each server answered the schema's fingerprint with last, by the name it
    /// answers with ([`Server::answered`]).
    shown: HashMap<Option<String>, String>,
    /// The fingerprint read last, which the loader labels the next analysis with.
    label: Option<String>,
}

impl Connection {
    fn new(server: Server, version: String) -> Connection {
        Connection { server, version, answers: AnswerCache::until_cleared(), shown: HashMap::new(), label: None }
    }
}

impl Connection {
    /// The schema's fingerprint; a failure is the answer the program gives for it. An executable
    /// function's result is the server's configuration's, which only a server of 26.2 or later
    /// lists: where the configuration defines one and the server can't list it, there is no
    /// fingerprint to keep an analysis by.
    fn schema(&mut self) -> Result<crate::Schema, Value> {
        self.server.answered();
        let schema = self.fingerprint();
        let by = self.server.answered();
        self.label = schema.as_ref().ok().map(|schema| schema.fingerprint.clone());
        if let (Ok(schema), [server]) = (&schema, &by.into_iter().collect::<Vec<_>>()[..]) {
            self.shown.insert(server.clone(), schema.fingerprint.clone());
        }
        schema
    }

    fn fingerprint(&self) -> Result<crate::Schema, Value> {
        use crate::engines::clickhouse::schema_fingerprint as fingerprint;
        let rows = |sql: &str| match self.server.ask(sql) {
            Ok(Outcome::Rows(rows)) => Ok(rows),
            Ok(outcome) => Err(failure(&self.server, outcome)),
            Err(e) => Err(json!({ "status": "unreachable", "message": e })),
        };
        let base = rows(&fingerprint::query(false))?;
        let executable = base.first().and_then(|row| row.get(1)).and_then(|n| n.as_u64().or_else(|| n.as_str()?.parse().ok())).unwrap_or(0) > 0;
        if !executable {
            return Ok(crate::Schema::of(&base, None));
        }
        if !fingerprint::lists_signatures(&self.version) {
            return Err(json!({ "status": "error", "message": format!("ClickHouse {} lists no executable function's arguments and result, which 26.2 and later do", self.version) }));
        }
        Ok(crate::Schema::of(&rows(&fingerprint::query(true))?, None))
    }

    /// The analysis of one statement, and whether it may be kept under the fingerprint read last
    /// ([`kept`]); a failure is the answer the program gives for it.
    fn analyze(&mut self, sql: &str) -> Result<(Analysis, bool), Value> {
        let shown = self.shown.clone();
        self.server.answered();
        let analyzed = answers::analyze(&self.server, sql, &mut self.answers);
        let keep = kept(self.label.as_deref(), &shown, &self.server.answered());
        // Answers a replica gave before it showed the schema are no answers to keep for the next statement.
        if !keep {
            self.answers.clear();
        }
        match analyzed {
            Ok(analysis) => Ok((analysis, keep)),
            Err(ReplayError::Host(e)) if e.0.starts_with("the query did not reach the server") => Err(json!({ "status": "unreachable", "message": e.0.lines().next().unwrap_or_default() })),
            Err(e) => Err(json!({ "status": "error", "message": e.to_string() })),
        }
    }
}

/// Whether an analysis answered by the servers `by` may be kept under `label`, the fingerprint
/// read last: where each of them had answered the schema's fingerprint with `label` before the
/// analysis began (`shown`). A replica applies a migration after the one that ran it, and its schema
/// only moves forward, so one that had shown the label's schema answers for it; one that had not may
/// answer for the schema before, as when it refuses a table the migration just made. Every analysis
/// on one server is kept, its fingerprint read on the same server.
fn kept(label: Option<&str>, shown: &HashMap<Option<String>, String>, by: &BTreeSet<Option<String>>) -> bool {
    label.is_some_and(|label| by.iter().all(|server| shown.get(server).is_some_and(|f| f == label)))
}

/// The statements of one command line's run, analyzed on one server, which keeps the engine's
/// answers between them.
pub struct Session {
    connection: Connection,
    /// The server's `version()`, as `26.9.23.15`.
    pub version: String,
}

impl Session {
    pub fn open(url: &str) -> Result<Session, Failure> {
        let (server, version) = connect(url).map_err(|answer| Failure::of(&answer))?;
        Ok(Session { connection: Connection::new(server, version.clone()), version })
    }

    /// Drops the answers kept from earlier statements, for a caller that outlives a schema change.
    pub fn forget(&mut self) {
        self.connection.answers.clear();
    }

    pub fn analyze(&mut self, sql: &str) -> Result<Analysis, Failure> {
        self.connection.analyze(sql).map(|(analysis, _)| analysis).map_err(|answer| Failure::of(&answer))
    }
}

impl Program for Clickhouse {
    fn call(&self, request: &str) -> String {
        reylo_engine_core::api::call::<crate::engines::clickhouse::Clickhouse>(request)
    }

    fn fingerprint(&self) -> String {
        crate::cache_fingerprint(crate::engines::clickhouse::FINGERPRINT, &[])
    }

    async fn serve(&mut self, request: &Value) -> Option<Value> {
        let op = request["op"].as_str()?;
        let id = request["conn"].as_u64().unwrap_or(0);
        let sql = request["sql"].as_str().unwrap_or_default();
        Some(match op {
            "open" => match connect(request["url"].as_str().unwrap_or_default()) {
                Ok((server, version)) => {
                    self.next += 1;
                    self.connections.insert(self.next, Connection::new(server, version.clone()));
                    json!({ "status": "opened", "conn": self.next, "version": version })
                }
                Err(failure) => failure,
            },
            "run" => match self.connections.get_mut(&id) {
                None => closed(id),
                Some(c) => match c.analyze(sql) {
                    Ok((analysis, kept)) => json!({ "status": "analysis", "analysis": analysis, "kept": kept }),
                    Err(failure) => failure,
                },
            },
            // A query the host runs itself (a host's own catalog query), its rows as the server's JSON
            // writes them.
            "query" => match self.connections.get(&id) {
                None => closed(id),
                Some(c) => match c.server.ask(sql) {
                    Ok(Outcome::Rows(rows)) => json!({ "status": "rows", "rows": rows }),
                    Ok(outcome) => failure(&c.server, outcome),
                    Err(e) => json!({ "status": "unreachable", "message": e }),
                },
            },
            // The schema's fingerprint ([`crate::engines::clickhouse::schema_fingerprint`]).
            "schema" => match self.connections.get_mut(&id) {
                None => closed(id),
                Some(c) => match c.schema() {
                    Ok(schema) => schema.answer(),
                    Err(failure) => failure,
                },
            },
            // The schema changed: what the engine was told about the old one is stale.
            "forget" => match self.connections.get_mut(&id) {
                None => closed(id),
                Some(c) => {
                    c.answers.clear();
                    json!({ "status": "ok" })
                }
            },
            "close" => {
                self.connections.remove(&id);
                json!({ "status": "ok" })
            }
            _ => return None,
        })
    }
}

fn closed(id: u64) -> Value {
    json!({ "status": "unreachable", "message": format!("connection {id} is closed") })
}

/// The server a URL names, once it has answered as that login, and its `version()`.
fn connect(url: &str) -> Result<(Server, String), Value> {
    let server = Server::of(url).map_err(|message| json!({ "status": "error", "message": message }))?;
    match server.ask("SELECT version()") {
        Ok(Outcome::Rows(rows)) => {
            let version = rows.first().and_then(|row| row.first()).and_then(Value::as_str).unwrap_or_default().to_string();
            match too_old(&version) {
                Some(message) => Err(json!({ "status": "error", "message": message })),
                None => Ok((server, version)),
            }
        }
        Ok(outcome) => Err(failure(&server, outcome)),
        Err(e) => Err(json!({ "status": "unreachable", "message": e })),
    }
}

/// The first release Reylo types statements on: since 25.11 the HTTP interface answers an error
/// with the error alone, whatever format was asked for, and marks one that ends a result after
/// its first rows, which the run time reads a result by (src/decode-clickhouse.ts).
const OLDEST: (u32, u32) = (25, 11);

/// Why a server of this `version()` is not typed, if it is older than [`OLDEST`]: a version that
/// reads as none, as a fork's may, is taken as it is.
fn too_old(version: &str) -> Option<String> {
    let mut parts = version.split('.').map(|part| part.parse::<u32>().ok());
    let (major, minor) = (parts.next()??, parts.next()??);
    ((major, minor) < OLDEST).then(|| format!("this server is ClickHouse {version}, and Reylo types statements on ClickHouse {}.{} or later, the first whose HTTP interface marks an error that ends a result", OLDEST.0, OLDEST.1))
}

/// What an answer that is no rows means to the host: a refused login, or the server's error.
fn failure(server: &Server, outcome: Outcome) -> Value {
    match outcome {
        Outcome::Refused { message, code, status } if status == 401 || status == 403 || code.as_deref().is_some_and(|c| REFUSED.contains(&c)) => json!({ "status": "refused", "message": message, "code": code }),
        Outcome::Refused { message, code, .. } => json!({ "status": "error", "message": message, "code": code }),
        Outcome::Foreign(what) => json!({ "status": "error", "message": format!("the server at {} answered with {what}, which ClickHouse's HTTP interface does not: the URL is the one an app gives @clickhouse/client, after clickhouse+, as clickhouse+http://localhost:8123", server.place()) }),
        Outcome::Rows(_) | Outcome::Typed(..) => json!({ "status": "error", "message": "the server answered with rows where none were asked for" }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_before_the_oldest_typed_release_is_refused_by_its_version() {
        assert!(too_old("25.10.3.100").is_some_and(|m| m.contains("ClickHouse 25.10.3.100") && m.contains("25.11 or later")));
        assert!(too_old("24.8.1.1").is_some());
        for typed in ["25.11.1.558", "25.12.9.1", "26.8.40.7", "27.1.1.1", "v-next", ""] {
            assert_eq!(too_old(typed), None, "{typed}");
        }
    }

    #[test]
    fn an_analysis_is_kept_where_each_server_that_answered_it_had_shown_the_schema() {
        let server = |name: &str| Some(name.to_string());
        let shown: HashMap<Option<String>, String> = [(server("replica1"), "new".to_string()), (server("replica2"), "old".to_string())].into();
        let by = |names: &[&str]| names.iter().map(|n| server(n)).collect::<BTreeSet<_>>();
        assert!(kept(Some("new"), &shown, &by(&["replica1"])));
        // A replica that had shown the schema before the migration, or never answered the fingerprint.
        assert!(!kept(Some("new"), &shown, &by(&["replica1", "replica2"])));
        assert!(!kept(Some("new"), &shown, &by(&["replica3"])));
        // No fingerprint to label it with.
        assert!(!kept(None, &shown, &by(&["replica1"])));
        // A server whose answers carry no name is one server, as it was before names were read.
        let unnamed: HashMap<Option<String>, String> = [(None, "new".to_string())].into();
        assert!(kept(Some("new"), &unnamed, &[None].into()));
        assert!(!kept(Some("new"), &unnamed, &[None, server("replica1")].into()));
    }
}

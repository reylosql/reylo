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
    /// Every server the connection has heard from.
    seen: BTreeSet<Option<String>>,
    /// The servers whose answers `answers` may hold: each that answered since it was last cleared.
    held: BTreeSet<Option<String>>,
}

/// The most reads of the schema's fingerprint after an analysis, to hear again from each server the
/// engine holds answers of: a load balancer that sends each request to the next replica reaches each
/// in turn.
const CONFIRM_READS: usize = 8;

impl Connection {
    fn new(server: Server, version: String) -> Connection {
        Connection { server, version, answers: AnswerCache::until_cleared(), shown: HashMap::new(), seen: BTreeSet::new(), held: BTreeSet::new() }
    }

    /// Drops the engine's answers, and with them the servers they came from.
    fn clear(&mut self) {
        self.answers.clear();
        self.held.clear();
    }

    /// The schema's fingerprint read once, and the server that answered it, noted as the one it showed.
    fn read(&mut self) -> (Result<crate::Schema, Value>, Option<Option<String>>) {
        self.server.answered();
        let schema = self.fingerprint();
        let by = self.server.answered();
        self.seen.extend(by.iter().cloned());
        let server = (by.len() == 1).then(|| by.into_iter().next()).flatten();
        if let (Ok(schema), Some(server)) = (&schema, &server) {
            self.shown.insert(server.clone(), schema.fingerprint.clone());
        }
        (schema, server)
    }

    /// The schema's fingerprint, with a stamp that moves each time a server the connection had not
    /// heard from answers: never on one server, and once per replica where a load balancer sends the
    /// connection's requests to several, so that the loader makes an analysis again when a replica
    /// first answers the read after it ([`kept`]); a failure is the answer the program gives for it.
    fn schema(&mut self) -> Result<crate::Schema, Value> {
        let (schema, _) = self.read();
        schema.map(|schema| crate::Schema { stamp: Some(self.seen.len() as i64), ..schema })
    }

    /// The schema's fingerprint, in one request, so that one server answers it. An executable
    /// function's result is the server's configuration's, which only a server of 26.2 or later
    /// lists: where the configuration defines one and the server can't list it, there is no
    /// fingerprint to keep an analysis by.
    fn fingerprint(&self) -> Result<crate::Schema, Value> {
        use crate::engines::clickhouse::schema_fingerprint as fingerprint;
        let signatures = fingerprint::lists_signatures(&self.version);
        let rows = match self.server.ask(&fingerprint::query(signatures)) {
            Ok(Outcome::Rows(rows)) => rows,
            Ok(outcome) => return Err(failure(&self.server, outcome)),
            Err(e) => return Err(json!({ "status": "unreachable", "message": e })),
        };
        let executable = rows.first().and_then(|row| row.get(1)).and_then(|n| n.as_u64().or_else(|| n.as_str()?.parse().ok())).unwrap_or(0) > 0;
        if executable && !signatures {
            return Err(json!({ "status": "error", "message": format!("ClickHouse {} lists no executable function's arguments and result, which 26.2 and later do", self.version) }));
        }
        Ok(crate::Schema::of(&rows, None))
    }

    /// The analysis of one statement, and, where the loader names the fingerprint it would keep it
    /// under (`label`), whether it may be kept under it ([`kept`]); a failure is the answer the
    /// program gives for it.
    fn analyze(&mut self, sql: &str, label: Option<&str>) -> Result<(Analysis, bool), Value> {
        // A connection opened since the loader read the fingerprint has seen no server show it.
        if label.is_some() && self.shown.is_empty() {
            self.read();
        }
        let before = self.shown.clone();
        self.server.answered();
        let analyzed = answers::analyze(&self.server, sql, &mut self.answers);
        let by = self.server.answered();
        self.seen.extend(by.iter().cloned());
        self.held.extend(by);
        let keep = match label {
            None => true,
            Some(label) => {
                let (held, one) = (self.held.clone(), self.seen.len() == 1);
                kept(label, &before, &held, one, || match self.read() {
                    (Ok(schema), Some(server)) => Some((server, schema.fingerprint)),
                    _ => None,
                })
            }
        };
        // Answers a replica gave for another schema are no answers for the next statement either.
        if !keep {
            self.clear();
        }
        match analyzed {
            Ok(analysis) => Ok((analysis, keep)),
            Err(ReplayError::Host(e)) if e.0.starts_with("the query did not reach the server") => Err(json!({ "status": "unreachable", "message": e.0.lines().next().unwrap_or_default() })),
            Err(e) => Err(json!({ "status": "error", "message": e.to_string() })),
        }
    }
}

/// Whether an analysis may be kept under `label`, the fingerprint the loader would keep it under:
/// where each server whose answers the engine holds (`held`) had answered the schema's fingerprint
/// with `label` before the analysis began (`before`), and, where the connection has heard from more
/// than one server, answers it with `label` again after it, read until each has (`read`, at most
/// [`CONFIRM_READS`] times). A load balancer may send each of the connection's requests to another
/// replica, and replicas apply a migration one after another; a replica's schema only moves forward,
/// so one that showed the label's schema before and after the analysis answered for it throughout,
/// where one that had not may answer for the schema before, as by refusing a table the migration
/// just made, or for the one after. On one server, the loader's own read after the analysis reaches
/// the same server, and a server heard from for the first time moves the schema's stamp. Replicas
/// that answer with one name, as a shared `display_name` or a proxy that sets its own makes them, are
/// one server here.
fn kept(label: &str, before: &HashMap<Option<String>, String>, held: &BTreeSet<Option<String>>, one: bool, mut read: impl FnMut() -> Option<(Option<String>, String)>) -> bool {
    if !held.iter().all(|server| before.get(server).is_some_and(|f| f == label)) {
        return false;
    }
    if one {
        return true;
    }
    let mut again = BTreeSet::new();
    for _ in 0..CONFIRM_READS {
        if held.iter().all(|server| again.contains(server)) {
            return true;
        }
        match read() {
            Some((server, fingerprint)) if held.contains(&server) => {
                if fingerprint != label {
                    return false;
                }
                again.insert(server);
            }
            Some(_) => {}
            None => return false,
        }
    }
    held.iter().all(|server| again.contains(server))
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
        self.connection.clear();
    }

    pub fn analyze(&mut self, sql: &str) -> Result<Analysis, Failure> {
        self.connection.analyze(sql, None).map(|(analysis, _)| analysis).map_err(|answer| Failure::of(&answer))
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
                Some(c) => match c.analyze(sql, request["label"].as_str()) {
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
                    c.clear();
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
    fn an_analysis_is_kept_where_each_server_it_heard_from_showed_the_schema_before_and_after() {
        let server = |name: &str| Some(name.to_string());
        let set = |names: &[&str]| names.iter().map(|n| server(n)).collect::<BTreeSet<_>>();
        let shown = |pairs: &[(&str, &str)]| pairs.iter().map(|&(n, f)| (server(n), f.to_string())).collect::<HashMap<_, _>>();
        // The reads after the analysis, in turn; `None` once they run out.
        let reads = |answers: &[(&str, &str)]| {
            let mut answers: Vec<Option<(Option<String>, String)>> = answers.iter().map(|&(n, f)| Some((server(n), f.to_string()))).collect();
            answers.reverse();
            move || answers.pop().flatten()
        };
        // One server: shown before, and the loader's read after reaches it.
        assert!(kept("new", &shown(&[("a", "new")]), &set(&["a"]), true, reads(&[])));
        assert!(!kept("new", &shown(&[("a", "old")]), &set(&["a"]), true, reads(&[])));
        // Two replicas that showed the schema before and show it again after.
        assert!(kept("new", &shown(&[("a", "new"), ("b", "new")]), &set(&["a", "b"]), false, reads(&[("a", "new"), ("b", "new")])));
        // One that had not shown it, as a replica the migration has not reached.
        assert!(!kept("new", &shown(&[("a", "new"), ("b", "old")]), &set(&["a", "b"]), false, reads(&[("a", "new"), ("b", "new")])));
        // One that moved past it while the analysis ran.
        assert!(!kept("old", &shown(&[("a", "old"), ("b", "old")]), &set(&["a", "b"]), false, reads(&[("a", "new"), ("b", "old")])));
        // A replica whose answers the engine holds none of says nothing of the analysis.
        assert!(kept("new", &shown(&[("a", "new")]), &set(&["a"]), false, reads(&[("c", "old"), ("a", "new")])));
        // One the reads never reach again, or a read that fails.
        assert!(!kept("new", &shown(&[("a", "new"), ("b", "new")]), &set(&["a", "b"]), false, reads(&[("a", "new"); CONFIRM_READS])));
        assert!(!kept("new", &shown(&[("a", "new"), ("b", "new")]), &set(&["a", "b"]), false, reads(&[("a", "new")])));
        // A server whose answers carry no name is one server, as before names were read.
        assert!(kept("new", &[(None, "new".to_string())].into(), &[None].into(), true, reads(&[])));
    }
}

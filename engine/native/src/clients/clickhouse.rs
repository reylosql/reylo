//! The ClickHouse engine's queries answered over the server's HTTP interface. Each is one request:
//! the query is the body, sent as it is written with no parameters, and its rows come back as
//! JSON (`default_format=JSONCompact`). Nothing else is set: a user whose profile is read-only may
//! change no setting, and may still name a format.
//!
//! A server behind a load balancer may be one of several replicas, which each request can reach a
//! different one of, and which apply a migration one after another. Each request carries one
//! `X-ClickHouse-Replica-Tag` per connection, which ClickHouse Cloud sends to one replica, and each
//! answer is noted by the server that gave it (`X-ClickHouse-Server-Display-Name`), so that a caller
//! can tell an analysis whose answers came from more than one ([`Server::answered`]).

use crate::{server_errors, AnswerCache, Analysis, Answerer, DbError, HostError, ReplayError};
use reylo_engine_clickhouse::Clickhouse;
use reylo_engine_core::database::Database;
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::time::Duration;

/// What a ClickHouse describe is answered with: never anything, its describe being a query.
pub type Describe = <Clickhouse as Database>::Describe;

/// How long a connection attempt may take, as the other programs'.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long one of the engine's queries may take. None reads a table's rows but through a scalar
/// subquery of the statement, which the server computes as it reads one.
const QUERY_TIMEOUT: Duration = Duration::from_secs(60);

/// The header ClickHouse Cloud sends every request that carries one value to one replica.
const REPLICA_TAG: &str = "X-ClickHouse-Replica-Tag";

/// The most of an answer that is read: a describe's or a query tree's is a few kilobytes.
const ANSWER_LIMIT: u64 = 256 << 20;

/// The analysis of `sql`, its queries answered by `server`.
pub fn analyze(server: &Server, sql: &str, cache: &mut AnswerCache<Describe>) -> Result<Analysis, ReplayError<HostError>> {
    crate::replay::run::<Clickhouse, _>(sql, &mut Answers(server), cache)
}

/// A ClickHouse server's HTTP interface, as a URL names it.
pub struct Server {
    agent: ureq::Agent,
    /// Where each request goes: the scheme, the host and port, and the path before the query.
    endpoint: String,
    /// What each request's URL carries: the database, a role, and the settings the URL names.
    parameters: Vec<(String, String)>,
    /// What each request's headers carry: the user and password, and the headers the URL names.
    headers: Vec<(String, String)>,
    /// The connection's own `X-ClickHouse-Replica-Tag`, where the URL names none.
    tag: Option<String>,
    /// The servers that answered since [`Server::answered`] last read them.
    answered: Mutex<BTreeSet<Option<String>>>,
}

/// What a request came to: the server's answer, or its failure to give one.
pub enum Outcome {
    /// The rows of the answer.
    Rows(Vec<Vec<Value>>),
    /// The rows of the answer, with each column's name and type before them ([`Server::ask_typed`]).
    Typed(Vec<(String, String)>, Vec<Vec<Value>>),
    /// The server's error, with the code it carries in `X-ClickHouse-Exception-Code`, or where the
    /// error was raised once the answer had begun, with no such header, the code its message opens
    /// with (`Code: 241. DB::Exception: …`).
    Refused { message: String, code: Option<String>, status: u16 },
    /// An answer that is not one of ClickHouse's HTTP interface.
    Foreign(String),
}

/// A URL's text, percent-decoded.
fn percent(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok())) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Server {
    /// The server a URL names: `clickhouse+` and the URL of its HTTP interface as
    /// `@clickhouse/client` reads one,
    /// `http[s]://[user[:password]@]host[:port][/database][?parameters]`, or that URL alone, as a
    /// caller that already knows the server is ClickHouse gives it. A parameter named
    /// `clickhouse_setting_<name>` or `ch_<name>` is a setting every query runs under, as the
    /// client sends it, so the statements are described as the app runs them; `role` is a role,
    /// `http_header_<name>` a header, `access_token` a bearer token in place of a password, and
    /// `pathname` a path in front of the interface, behind a proxy. The client's other parameters
    /// are its own.
    pub fn of(url: &str) -> Result<Server, String> {
        let (scheme, rest) = url.split_once("://").ok_or("cannot read the URL: it has no scheme")?;
        let scheme = scheme.to_ascii_lowercase();
        let scheme = scheme.strip_prefix("clickhouse+").unwrap_or(&scheme);
        if scheme != "http" && scheme != "https" {
            return Err(format!("cannot read the URL: a ClickHouse database is named by the URL of its HTTP interface after clickhouse+, as clickhouse+http://localhost:8123, not {scheme}://"));
        }
        let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let (login, host) = authority.rsplit_once('@').map_or((None, authority), |(login, host)| (Some(login), host));
        if host.is_empty() {
            return Err("cannot read the URL: it names no host".to_string());
        }
        let mut parameters = Vec::new();
        let mut headers = Vec::new();
        let database = percent(path.split('/').next().unwrap_or_default());
        if !database.is_empty() {
            parameters.push(("database".to_string(), database));
        }
        let mut pathname = String::new();
        let mut token = None;
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            // A name is read decoded, as `@clickhouse/client` reads the URL's parameters.
            let key = percent(&key.replace('+', " "));
            let key = key.as_str();
            let value = percent(&value.replace('+', " "));
            let setting = key.strip_prefix("clickhouse_setting_").or_else(|| key.strip_prefix("ch_"));
            match (key, setting, key.strip_prefix("http_header_")) {
                (_, Some(setting), _) => parameters.push((setting.to_string(), value)),
                (_, _, Some(header)) => headers.push((header.to_string(), value)),
                ("role", ..) => parameters.push(("role".to_string(), value)),
                ("pathname", ..) => pathname = format!("/{}", value.trim_matches('/')),
                ("access_token", ..) => token = Some(value),
                _ => {}
            }
        }
        // A bearer token stands in for the login, and for nothing else: a header the URL names, as
        // the database's, is sent either way.
        match (token, login) {
            (Some(token), _) => headers.insert(0, ("Authorization".to_string(), format!("Bearer {token}"))),
            (None, Some(login)) => {
                let (user, password) = login.split_once(':').unwrap_or((login, ""));
                headers.splice(0..0, [("X-ClickHouse-User".to_string(), percent(user)), ("X-ClickHouse-Key".to_string(), percent(password))]);
            }
            (None, None) => {}
        }
        // No redirect is followed, as `@clickhouse/client` follows none: one would carry the login's
        // headers to whatever host it names. Its answer is then one the interface does not give.
        let config = ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).timeout_connect(Some(CONNECT_TIMEOUT)).timeout_global(Some(QUERY_TIMEOUT)).build();
        // A value of this connection's alone, so that ClickHouse Cloud sends its requests to one replica.
        let tag = (!headers.iter().any(|(name, _)| name.eq_ignore_ascii_case(REPLICA_TAG))).then(|| {
            use std::hash::BuildHasher;
            format!("reylo-{:016x}", std::collections::hash_map::RandomState::new().hash_one((std::process::id(), std::time::SystemTime::now())))
        });
        Ok(Server { agent: ureq::Agent::new_with_config(config), endpoint: format!("{scheme}://{host}{pathname}/"), parameters, headers, tag, answered: Mutex::default() })
    }

    /// The servers that answered since this was last called, by the name each answer carries in
    /// `X-ClickHouse-Server-Display-Name`, `None` for an answer that carries none: one server unless
    /// the connection reaches several replicas.
    pub fn answered(&self) -> BTreeSet<Option<String>> {
        std::mem::take(&mut *self.answered.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// The host and port, and the database where the URL names one, for a message.
    pub fn place(&self) -> String {
        let host = self.endpoint.split_once("://").map_or(self.endpoint.as_str(), |(_, rest)| rest).trim_end_matches('/');
        match self.parameters.iter().find(|(name, _)| name == "database") {
            Some((_, database)) => format!("{host}/{database}"),
            None => host.to_string(),
        }
    }

    /// One query's outcome, or why no answer came: the server cannot be reached, or went away.
    pub fn ask(&self, sql: &str) -> Result<Outcome, String> {
        Ok(match self.ask_typed(sql, &[])? {
            Outcome::Typed(_, rows) => Outcome::Rows(rows),
            other => other,
        })
    }

    /// One query's outcome with its columns' names and types, the request's URL carrying `fields`
    /// beside the connection's own: a statement's parameters (`param_<name>`) and settings, as the
    /// soundness oracle runs a statement.
    pub fn ask_typed(&self, sql: &str, fields: &[(String, String)]) -> Result<Outcome, String> {
        use std::io::Read;
        let mut request = self.agent.post(self.endpoint.as_str()).query("default_format", "JSONCompact");
        for (name, value) in self.parameters.iter().chain(fields) {
            request = request.query(name.as_str(), value.as_str());
        }
        for (name, value) in &self.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        if let Some(tag) = &self.tag {
            request = request.header(REPLICA_TAG, tag.as_str());
        }
        let mut response = request.send(sql.as_bytes()).map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(String::from);
        self.answered.lock().unwrap_or_else(|e| e.into_inner()).insert(header("x-clickhouse-server-display-name"));
        let code = header("x-clickhouse-exception-code");
        let ours = code.is_some() || header("x-clickhouse-query-id").is_some() || header("x-clickhouse-summary").is_some();
        let tag = header("x-clickhouse-exception-tag");
        let mut bytes = Vec::new();
        if let Err(e) = response.body_mut().with_config().limit(ANSWER_LIMIT).reader().read_to_end(&mut bytes) {
            // An error raised once the answer had begun: the server writes it after the rows it
            // sent, in a block its tag marks, and breaks the answer's framing, so no client reads
            // those rows as whole.
            let text = String::from_utf8_lossy(&bytes);
            return tag.as_deref().and_then(|tag| raised(&text, tag)).map(|message| refused(&message, code, status)).ok_or_else(|| e.to_string());
        }
        let body = String::from_utf8(bytes).map_err(|e| e.to_string())?;
        // The same block, where something between the server and the client closed the answer's
        // framing after it.
        if let Some(message) = tag.as_deref().and_then(|tag| raised(&body, tag)) {
            return Ok(refused(&message, code, status));
        }
        if status != 200 {
            return Ok(if ours { refused(body.trim(), code, status) } else { Outcome::Foreign(format!("HTTP {status}")) });
        }
        if body.trim().is_empty() {
            return Ok(Outcome::Typed(Vec::new(), Vec::new()));
        }
        let answer: Value = match serde_json::from_str(&body) {
            Ok(answer) => answer,
            Err(_) if ours => return Ok(refused(body.trim(), code, status)),
            Err(_) => return Ok(Outcome::Foreign("an answer that is no JSON".to_string())),
        };
        // An error raised once the answer had begun, written into it where the session sets
        // `output_format_json_valid_output_on_exception`.
        if let Some(exception) = answer.get("exception").and_then(Value::as_str) {
            return Ok(refused(exception, code, status));
        }
        let text = |column: &Value, key: &str| column.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        let columns = answer.get("meta").and_then(Value::as_array).map(|meta| meta.iter().map(|column| (text(column, "name"), text(column, "type"))).collect()).unwrap_or_default();
        let rows = answer.get("data").and_then(Value::as_array).map(|rows| rows.iter().map(|row| row.as_array().cloned().unwrap_or_default()).collect());
        Ok(rows.map_or_else(|| Outcome::Foreign("an answer with no rows in it".to_string()), |rows| Outcome::Typed(columns, rows)))
    }
}

/// The error an answer whose framing broke ends with, in the block ClickHouse writes it in:
/// `__exception__`, the tag, the message and a line break, their length and the tag again, and
/// `__exception__`. A block whose length is not its message's was cut, and is not read.
fn raised(body: &str, tag: &str) -> Option<String> {
    let opening = format!("__exception__\r\n{tag}\r\n");
    let rest = &body[body.rfind(&opening)? + opening.len()..];
    let block = &rest[..rest.rfind(&format!(" {tag}\r\n__exception__"))?];
    let (message, length) = block.rsplit_once('\n')?;
    (length.parse::<usize>().ok()? == message.len() + 1).then(|| message.trim().to_string())
}

/// A refusal, its code the header's or else the one its message opens with.
fn refused(message: &str, code: Option<String>, status: u16) -> Outcome {
    let opened = || message.strip_prefix("Code: ")?.split_once('.').map(|(n, _)| n).filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).map(String::from);
    Outcome::Refused { message: message.to_string(), code: code.or_else(opened), status }
}

/// A server's error as a person reads it: without the code it opens with and the version it ends
/// with, its name after its text, as the engine writes one into a diagnostic.
fn plain(message: &str) -> &str {
    let text = message.trim();
    let text = text.strip_prefix("Code: ").and_then(|rest| rest.split_once(". ")).map_or(text, |(_, after)| after);
    let text = text.strip_prefix("DB::Exception: ").unwrap_or(text);
    text.rfind(" (version ").map_or(text, |at| &text[..at])
}

/// The engine's queries, answered by a server.
pub struct Answers<'a>(pub &'a Server);

impl Answerer<Describe> for Answers<'_> {
    type Error = HostError;

    fn describe(&mut self, sql: &str) -> Result<Result<Describe, DbError>, HostError> {
        Err(HostError(format!("the ClickHouse engine asked to describe {sql}, which it asks as a query")))
    }

    fn query(&mut self, sql: &str, _params: &[Value]) -> Result<Result<Vec<Vec<Value>>, DbError>, HostError> {
        match self.0.ask(sql) {
            Ok(Outcome::Rows(rows) | Outcome::Typed(_, rows)) => Ok(Ok(rows)),
            // The statement's error, or one a catalog query is refused with, which the engine reads.
            Ok(Outcome::Refused { message, code: Some(code), .. }) if code.parse().is_ok_and(server_errors::clickhouse) => Ok(Err(DbError { message, code: Some(code), position: None })),
            // Any other, as a query cancelled, a timeout, the server's memory or its limit on
            // queries, or a remote source `s3()` reads unreachable: not the statement's, so nothing
            // is kept as its error, and the next analysis asks again.
            Ok(Outcome::Refused { message, .. }) => Err(HostError(format!("The server failed the analysis of this statement for a reason of its own, so run it again: {}", plain(&message)))),
            Ok(Outcome::Foreign(what)) => Err(HostError(format!("the server at {} answered with {what}, which ClickHouse's HTTP interface does not", self.0.place()))),
            Err(e) => Err(HostError(format!("the query did not reach the server: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_is_read_as_the_client_reads_it() {
        let server = Server::of("https://app:s%40cret@db.example:8443/shop?clickhouse_setting_join_use_nulls=1&ch_max_threads=2&role=reader&request_timeout=5000&http_header_X-Trace=a+b").unwrap();
        assert_eq!(server.endpoint, "https://db.example:8443/");
        assert_eq!(server.parameters, [("database", "shop"), ("join_use_nulls", "1"), ("max_threads", "2"), ("role", "reader")].map(|(k, v)| (k.to_string(), v.to_string())));
        assert_eq!(server.headers, [("X-ClickHouse-User", "app"), ("X-ClickHouse-Key", "s@cret"), ("X-Trace", "a b")].map(|(k, v)| (k.to_string(), v.to_string())));
        assert_eq!(server.place(), "db.example:8443/shop");
        let plain = Server::of("http://localhost:8123").unwrap();
        assert_eq!((plain.endpoint.as_str(), plain.parameters.len(), plain.headers.len(), plain.place()), ("http://localhost:8123/", 0, 0, "localhost:8123".to_string()));
        // The scheme a project's URL has, and the interface's own address, read alike.
        assert_eq!(Server::of("clickhouse+https://app@db.example:8443/shop").unwrap().endpoint, "https://db.example:8443/");
        assert_eq!(Server::of("ClickHouse+HTTP://localhost:8123").unwrap().endpoint, "http://localhost:8123/");
        let proxied = Server::of("HTTP://localhost/analytics?pathname=/clickhouse/&access_token=t0ken").unwrap();
        assert_eq!(proxied.endpoint, "http://localhost/clickhouse/");
        assert_eq!(proxied.headers, [("Authorization".to_string(), "Bearer t0ken".to_string())]);
        // A parameter's name is read decoded.
        assert_eq!(Server::of("http://localhost/?%63h_database=a").unwrap().parameters, [("database".to_string(), "a".to_string())]);
        // A header the URL names is sent beside a token, wherever the URL writes the two.
        for url in ["http://localhost/?http_header_X-ClickHouse-Database=a&access_token=t0ken", "http://localhost/?access_token=t0ken&http_header_X-ClickHouse-Database=a"] {
            assert_eq!(Server::of(url).unwrap().headers, [("Authorization", "Bearer t0ken"), ("X-ClickHouse-Database", "a")].map(|(k, v)| (k.to_string(), v.to_string())), "{url}");
        }
    }

    #[test]
    fn a_redirect_is_not_followed_and_the_login_goes_nowhere_else() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let (first, other) = (TcpListener::bind("127.0.0.1:0").unwrap(), TcpListener::bind("127.0.0.1:0").unwrap());
        other.set_nonblocking(true).unwrap();
        let location = format!("http://{}/", other.local_addr().unwrap());
        let url = format!("http://app:secret@{}/shop", first.local_addr().unwrap());
        let redirecting = std::thread::spawn(move || {
            let (mut stream, _) = first.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            // The request's head and its one line of SQL.
            while !request.windows(8).any(|w| w == b"SELECT 1") {
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            stream.write_all(format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).unwrap();
        });
        let outcome = Server::of(&url).unwrap().ask("SELECT 1").unwrap();
        redirecting.join().unwrap();
        assert!(matches!(&outcome, Outcome::Foreign(what) if what == "HTTP 302"));
        assert!(other.accept().is_err(), "the redirect's host was asked");
    }

    /// A stand-in for the HTTP interface that answers one request with `response`, and the URL it listens at.
    fn answering(response: String) -> (String, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let serving = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            // The request's head and its one line of SQL.
            while !request.windows(8).any(|w| w == b"SELECT 1") {
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            stream.write_all(response.as_bytes()).unwrap();
        });
        (url, serving)
    }

    #[test]
    fn each_request_carries_the_connections_replica_tag_and_each_answer_its_server() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let serving = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            while !request.windows(8).any(|w| w == b"SELECT 1") {
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            let body = r#"{"meta": [], "data": [[1]]}"#;
            stream.write_all(format!("HTTP/1.1 200 OK\r\nX-ClickHouse-Query-Id: q\r\nX-ClickHouse-Server-Display-Name: replica7\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).unwrap();
            String::from_utf8_lossy(&request).to_ascii_lowercase()
        });
        let server = Server::of(&url).unwrap();
        assert!(matches!(server.ask("SELECT 1").unwrap(), Outcome::Rows(_)));
        let request = serving.join().unwrap();
        assert!(request.contains("\r\nx-clickhouse-replica-tag: reylo-"), "{request}");
        assert_eq!(server.answered(), [Some("replica7".to_string())].into());
        assert!(server.answered().is_empty());
        // A tag the URL names is the one sent; none is added beside it.
        let named = Server::of("http://localhost:8123/?http_header_X-ClickHouse-Replica-Tag=app").unwrap();
        assert_eq!((named.tag, named.headers), (None, vec![("X-ClickHouse-Replica-Tag".to_string(), "app".to_string())]));
        // Two connections, two tags.
        assert_ne!(Server::of(&url).unwrap().tag, Server::of(&url).unwrap().tag);
    }

    /// `body` as the HTTP interface sends an error, with its code in a header where it has one.
    fn error(status: u16, code: Option<u32>, body: &str) -> String {
        let header = code.map_or(String::new(), |c| format!("X-ClickHouse-Exception-Code: {c}\r\n"));
        format!("HTTP/1.1 {status} Error\r\n{header}X-ClickHouse-Query-Id: q\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    /// The tag a server marks an error block with, as `X-ClickHouse-Exception-Tag` names it.
    const TAG: &str = "kxsaqfwvbmzkpwla";

    /// `data` as one chunk of a chunked answer.
    fn chunk(data: &str) -> String {
        format!("{:x}\r\n{data}\r\n", data.len())
    }

    /// `message` in the block a server writes an error raised once its answer had begun in.
    fn block(message: &str) -> String {
        format!("\r\n__exception__\r\n{TAG}\r\n{message}\n{} {TAG}\r\n__exception__\r\n", message.len() + 1)
    }

    #[test]
    fn an_error_block_is_read_only_whole() {
        let whole = format!("[[1],{}", block("Code: 241. DB::Exception: Memory limit (total) exceeded. (MEMORY_LIMIT_EXCEEDED)"));
        assert_eq!(raised(&whole, TAG).as_deref(), Some("Code: 241. DB::Exception: Memory limit (total) exceeded. (MEMORY_LIMIT_EXCEEDED)"));
        // A message cut short, a block with no end, another tag's.
        assert_eq!(raised(&whole.replace("(total) ", ""), TAG), None);
        assert_eq!(raised(&whole[..whole.len() - 20], TAG), None);
        assert_eq!(raised(&whole, "anotheranothertg"), None);
    }

    #[test]
    fn only_the_statements_own_error_is_the_answer_and_any_other_fails_the_analysis() {
        let cases = [
            // A query cancelled, as by `KILL QUERY`, and a timeout: the server's, before its answer began.
            (error(500, Some(394), "Code: 394. DB::Exception: Query was cancelled. (QUERY_WAS_CANCELLED)"), None),
            (error(500, Some(159), "Code: 159. DB::Exception: Timeout exceeded: elapsed 1 seconds. (TIMEOUT_EXCEEDED)"), None),
            // The server's memory, once its answer had begun: no header, the code in the answer, as
            // `output_format_json_valid_output_on_exception` writes it.
            (error(200, None, r#"{"exception": "Code: 241. DB::Exception: Memory limit (total) exceeded. (MEMORY_LIMIT_EXCEEDED)"}"#), None),
            // And as the server writes it by default: after the rows it sent, in a block its tag
            // marks, with no last chunk, so the answer's framing breaks.
            (format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nX-ClickHouse-Query-Id: q\r\nX-ClickHouse-Exception-Tag: {TAG}\r\nConnection: close\r\n\r\n{}{}", chunk("{\"meta\": [{\"name\": \"n\", \"type\": \"UInt64\"}], \"data\": [[1],"), chunk(&block("Code: 241. DB::Exception: Memory limit (total) exceeded. (MEMORY_LIMIT_EXCEEDED) (version 26.8.48.11 (official build))"))), None),
            // A name the statement gives that the schema lacks: the statement's.
            (error(404, Some(47), "Code: 47. DB::Exception: Unknown expression identifier `nope`. (UNKNOWN_IDENTIFIER)"), Some("47")),
        ];
        for (response, statements) in cases {
            let (url, serving) = answering(response);
            let answered = Answers(&Server::of(&url).unwrap()).query("SELECT 1", &[]);
            serving.join().unwrap();
            match (answered, statements) {
                (Ok(Err(e)), Some(code)) => assert_eq!(e.code.as_deref(), Some(code)),
                (Err(HostError(message)), None) => assert!(message.starts_with("The server failed the analysis of this statement for a reason of its own, so run it again: ") && !message.contains("Code: ") && message.ends_with(')'), "{message}"),
                (other, _) => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn a_url_of_another_kind_is_refused_by_what_to_write() {
        assert!(Server::of("clickhouse://localhost:9000/app").err().unwrap().contains("as clickhouse+http://localhost:8123, not clickhouse://"));
        assert!(Server::of("clickhouse+native://localhost:9000/app").is_err());
        assert!(Server::of("localhost:8123").is_err());
        assert!(Server::of("http:///app").err().unwrap().contains("names no host"));
    }
}

//! `reylo lsp`: a language server for `.sql` files, over standard input and output.
//!
//! Syntax errors come from the dialect's tree on every edit. Once the text has been still for a moment, a file of one statement is analyzed on the project's database, as `reylo analyze` analyzes it, for its diagnostics and the types hover shows. Completion and go-to-definition read the statement's tokens, the database's columns and the `CREATE TABLE`s under the project's `reylo/` directory.
//!
//! Everything that touches a database runs on a worker thread with a timeout, so a locked table or a silent server never holds up the editor: requests are answered from what the worker last read.

mod reading;

use crate::analyze::{untyped_params, Session};
use reading::{catalog_key, created_tables, for_grammar, items, key, output_column, scopes, select_list, tokens, Class, Created, Lines, Scopes, Source, SourceKind, Tok, RESERVED};
use reylo_engine::project::{self, Project};
use reylo_engine::Failure;
use reylo_engine_core::api::Analysis;
use reylo_migrate::db::Db;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use syntax::Dialect;

pub const USAGE: &str = "Usage: reylo lsp [--dialect postgres | mysql | sqlite]

Serves the language server for .sql files over standard input and output: syntax errors, the
database's own diagnostics, hover with inferred types, completion from the schema and
go-to-definition. Each file's project and database are found as reylo analyze finds them, from
the file's directory. --dialect reads files as that dialect where no project names a database;
those files get syntax errors and completion from their own text.";

/// How long the text stays still before it is analyzed on the database.
const QUIET: Duration = Duration::from_millis(250);
/// How long a read of the schema is used before the next request reads it again.
const FRESH: Duration = Duration::from_secs(5);
/// How long after a failed connection the next one is tried.
const RETRY: Duration = Duration::from_secs(10);
/// How long one connection, analysis or schema read may take.
const TIMEOUT: Duration = Duration::from_secs(10);
/// The most diagnostics a document shows of its syntax errors.
const MAX_ERRORS: usize = 200;
/// The largest message the server reads.
const MAX_MESSAGE: usize = 1 << 30;

pub fn run(args: &[String]) -> i32 {
    let mut forced = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let value = match arg.as_str() {
            "--help" | "-h" => {
                println!("{USAGE}");
                return 0;
            }
            // Editors pass this to every server that speaks over standard input.
            "--stdio" => continue,
            "--dialect" => rest.next().map(String::as_str),
            _ => match arg.strip_prefix("--dialect=") {
                Some(value) => Some(value),
                None => {
                    eprintln!("reylo lsp: unknown option {arg}\n\n{USAGE}");
                    return 2;
                }
            },
        };
        forced = match value.and_then(dialect_named) {
            Some(dialect) => Some(dialect),
            None => {
                eprintln!("reylo lsp: --dialect takes postgres, mysql or sqlite\n\n{USAGE}");
                return 2;
            }
        };
    }
    let (events, inbox) = mpsc::channel();
    let from_client = events.clone();
    std::thread::spawn(move || {
        let mut input = BufReader::new(std::io::stdin().lock());
        while let Some(message) = read_message(&mut input) {
            if from_client.send(Event::Message(message)).is_err() {
                return;
            }
        }
        let _ = from_client.send(Event::Closed);
    });
    let (jobs, queue) = mpsc::channel();
    std::thread::spawn(move || Worker { events, forced, places: HashMap::new() }.run(queue));
    Server { jobs, docs: HashMap::new(), places: HashMap::new(), pending: HashMap::new(), retrying: HashMap::new(), initialized: false, shutting_down: false }.serve(inbox)
}

fn dialect_named(name: &str) -> Option<Dialect> {
    Some(match name {
        "postgres" => Dialect::Postgres,
        "mysql" => Dialect::Mysql,
        "sqlite" => Dialect::Sqlite,
        "clickhouse" => Dialect::ClickHouse,
        "sqlserver" => Dialect::SqlServer,
        _ => return None,
    })
}

/// One message of the protocol: `Content-Length` and any other headers, a blank line, then JSON. Text that isn't JSON comes back as `Err`; a stream that can't be framed ends the server.
fn read_message(input: &mut impl BufRead) -> Option<Result<Value, ()>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse::<usize>().ok();
            }
        }
    }
    let length = length.filter(|n| *n <= MAX_MESSAGE)?;
    let mut body = vec![0; length];
    input.read_exact(&mut body).ok()?;
    Some(serde_json::from_slice(&body).map_err(|_| ()))
}

fn send(message: &Value) {
    let body = message.to_string();
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "Content-Length: {}\r\n\r\n{body}", body.len());
    let _ = out.flush();
}

fn log(message: &str) {
    send(&json!({ "jsonrpc": "2.0", "method": "window/logMessage", "params": { "type": 3, "message": format!("reylo: {message}") } }));
}

/// The path of a `file:` URI, its percent escapes read.
fn path_of(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let mut bytes = Vec::with_capacity(rest.len());
    let raw = rest.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' {
            let hex = rest.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(byte) = hex {
                bytes.push(byte);
                i += 3;
                continue;
            }
        }
        bytes.push(raw[i]);
        i += 1;
    }
    let path = String::from_utf8(bytes).ok()?;
    // `file:///C:/x` is `C:/x` on Windows.
    let windows = path.as_bytes().get(2) == Some(&b':') && path.starts_with('/');
    Some(PathBuf::from(if cfg!(windows) && windows { &path[1..] } else { path.as_str() }))
}

fn uri_of(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from(if text.starts_with('/') { "file://" } else { "file:///" });
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.~:".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// A table of the database.
#[derive(Clone)]
struct Table {
    schema: String,
    /// Where its schema stands on the search path; none where a statement must name the schema.
    rank: Option<u32>,
    name: String,
    key: String,
    columns: Vec<Column>,
}

#[derive(Clone)]
struct Column {
    name: String,
    key: String,
    ty: String,
    nullable: bool,
}

/// What the main thread asks of the worker.
enum Job {
    /// Find a document's project. `again` reads its database's URL anew.
    Locate { uri: String, dir: PathBuf, again: bool },
    Analyze { uri: String, place: PathBuf, text: String },
    Read { place: PathBuf },
}

/// What reaches the main thread.
enum Event {
    Message(Result<Value, ()>),
    Closed,
    Located { uri: String, place: Option<(PathBuf, Dialect)> },
    Analyzed { uri: String, text: String, analysis: Option<Box<Analysis>>, found: Vec<Value> },
    Read { place: PathBuf, tables: Vec<Table>, created: Vec<(PathBuf, Created)> },
    /// The database didn't answer; what needs it is tried again later.
    Unavailable { place: PathBuf },
}

/// The worker's side of a project: what it takes to reach its database.
struct WorkerPlace {
    dialect: Dialect,
    database: Option<(String, &'static str)>,
    base: PathBuf,
    home: PathBuf,
    project: Option<Project>,
    session: Option<Session>,
    /// The last failure logged, so that a server that stays down is said once.
    said: Option<String>,
}

struct Worker {
    events: Sender<Event>,
    forced: Option<Dialect>,
    places: HashMap<PathBuf, WorkerPlace>,
}

impl Worker {
    fn run(mut self, queue: Receiver<Job>) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime");
        while let Ok(first) = queue.recv() {
            // What piled up while the last job ran: only the newest text of each document, and one read of each schema.
            let mut batch = vec![first];
            batch.extend(queue.try_iter());
            let (mut analyzed, mut read) = (HashSet::new(), HashSet::new());
            let mut wanted = Vec::with_capacity(batch.len());
            for job in batch.into_iter().rev() {
                let keep = match &job {
                    Job::Locate { .. } => true,
                    Job::Analyze { uri, .. } => analyzed.insert(uri.clone()),
                    Job::Read { place } => read.insert(place.clone()),
                };
                if keep {
                    wanted.push(job);
                }
            }
            for job in wanted.into_iter().rev() {
                match job {
                    Job::Locate { uri, dir, again } => {
                        let place = self.locate(&dir, again);
                        let _ = self.events.send(Event::Located { uri, place });
                    }
                    Job::Analyze { uri, place, text } => self.analyze(&runtime, uri, place, text),
                    Job::Read { place } => self.read(&runtime, place),
                }
            }
        }
    }

    /// The project of a directory, read once unless `again` says its URL may have changed.
    fn locate(&mut self, dir: &Path, again: bool) -> Option<(PathBuf, Dialect)> {
        let env: HashMap<String, String> = std::env::vars_os().map(|(k, v)| (k.to_string_lossy().into_owned(), v.to_string_lossy().into_owned())).collect();
        let project = match Project::resolve(dir, env) {
            Ok(project) => Some(project),
            Err(why) => {
                log(&why);
                None
            }
        };
        let key = project.as_ref().map_or_else(|| dir.to_path_buf(), |p| p.root.clone());
        if let Some(place) = self.places.get(&key).filter(|_| !again) {
            return Some((key.clone(), place.dialect));
        }
        // A `direct_url` may run a command, which this thread can wait for.
        let url = project.as_ref().and_then(|p| p.type_check_url().ok().flatten()).map(|(url, _)| url).filter(|u| !u.is_empty());
        let named = url.as_deref().and_then(project::dialect_of);
        // The project's database decides; `--dialect` is for the file that has none.
        let Some(dialect) = named.and_then(dialect_named).or(self.forced) else {
            self.places.remove(&key);
            log(&format!("{} names no database, so its files get no diagnostics: set DATABASE_URL, or pass --dialect", key.display()));
            return None;
        };
        let database = url.zip(named);
        let home = project.as_ref().map_or_else(|| dir.join("reylo"), |p| p.root.join(p.config.dir.as_deref().unwrap_or("reylo")));
        let base = project.as_ref().map_or_else(|| dir.to_path_buf(), |p| p.root.clone());
        let kept = self.places.remove(&key).filter(|p| p.database == database);
        let (session, said) = kept.map_or((None, None), |p| (p.session, p.said));
        self.places.insert(key.clone(), WorkerPlace { dialect, database, base, home, project, session, said });
        Some((key, dialect))
    }

    fn unavailable(&mut self, key: &Path, why: String) {
        if let Some(place) = self.places.get_mut(key) {
            place.session = None;
            if place.said.as_deref() != Some(why.as_str()) {
                log(&why);
                place.said = Some(why);
            }
        }
        let _ = self.events.send(Event::Unavailable { place: key.to_path_buf() });
    }

    /// Analyzes a file of one statement on its database.
    fn analyze(&mut self, runtime: &tokio::runtime::Runtime, uri: String, key: PathBuf, text: String) {
        let Some(place) = self.places.get_mut(&key) else { return };
        let Some((url, name)) = place.database.clone() else { return };
        if place.session.is_none() {
            let opened = runtime.block_on(async { tokio::time::timeout(TIMEOUT, Session::open(name, &url, &place.base)).await });
            match opened {
                Ok(Ok(session)) => place.session = Some(session),
                Ok(Err(failure)) => return self.unavailable(&key, failure.to_string()),
                Err(_) => return self.unavailable(&key, format!("the database took more than {} seconds to connect", TIMEOUT.as_secs())),
            }
        }
        let session = place.session.as_mut().expect("opened above");
        let version = reylo_engine_core::types::DialectVersion { name: name.to_string(), version: session.version().to_string() };
        // The schema may have changed since the last analysis, which an editor can't tell the session.
        session.forget();
        let result = runtime.block_on(async { tokio::time::timeout(TIMEOUT, session.analyze(&text)).await });
        let lines = Lines::new(&text);
        let (analysis, found) = match result {
            Ok(Ok(analysis)) => {
                let mut diagnostics = analysis.descriptor(&text, version).diagnostics;
                if let Some(project) = place.project.as_ref().filter(|p| p.strict_params() && analysis.diagnostics.is_empty()) {
                    let dialect = match name {
                        "postgres" => reylo_engine_core::dialect::Dialect::Postgres,
                        "mysql" => reylo_engine_core::dialect::Dialect::Mysql,
                        _ => reylo_engine_core::dialect::Dialect::Sqlite,
                    };
                    diagnostics.extend(untyped_params(&text, &analysis, dialect, project));
                }
                let found = diagnostics.iter().map(|d| diagnostic(&text, &lines, byte_of(&text, d.start), byte_of(&text, d.end), &d.message, d.code.as_deref())).collect();
                (Some(Box::new(analysis)), found)
            }
            Ok(Err(Failure::Error(message))) => (None, vec![diagnostic(&text, &lines, 0, text.len(), &message, None)]),
            Ok(Err(failure)) => return self.unavailable(&key, failure.to_string()),
            Err(_) => return self.unavailable(&key, format!("the database took more than {} seconds to analyze a statement", TIMEOUT.as_secs())),
        };
        place.said = None;
        let _ = self.events.send(Event::Analyzed { uri, text, analysis, found });
    }

    /// Reads the database's tables and the tables the project's files create.
    fn read(&mut self, runtime: &tokio::runtime::Runtime, key: PathBuf) {
        let Some(place) = self.places.get(&key) else { return };
        let dialect = place.dialect;
        let created = created_in(&place.home, dialect);
        let Some((url, name)) = place.database.clone() else {
            let _ = self.events.send(Event::Read { place: key, tables: Vec::new(), created });
            return;
        };
        let base = place.base.clone();
        let rows = runtime.block_on(async {
            let read = async {
                let mut db = Db::connect_read_only(&url, &base).await?;
                db.rows(columns_query(name)).await
            };
            tokio::time::timeout(TIMEOUT, read).await
        });
        let rows = match rows {
            Ok(Ok(rows)) => rows,
            Ok(Err(e)) => return self.unavailable(&key, e.0),
            Err(_) => return self.unavailable(&key, format!("the database took more than {} seconds to list its tables", TIMEOUT.as_secs())),
        };
        let mut tables: Vec<Table> = Vec::new();
        for r in rows {
            let column = Column { name: r.text("c").to_string(), key: catalog_key(dialect, r.text("c")), ty: r.text("ty").to_string(), nullable: r.text("nn") != "1" };
            match tables.last_mut().filter(|t| t.schema == r.text("s") && t.name == r.text("t")) {
                Some(table) => table.columns.push(column),
                None => tables.push(Table { schema: r.text("s").to_string(), rank: r.text("sp").parse().ok(), name: r.text("t").to_string(), key: catalog_key(dialect, r.text("t")), columns: vec![column] }),
            }
        }
        let _ = self.events.send(Event::Read { place: key, tables, created });
    }
}

/// The tables the SQL files under a project's directory create: its migrations in order, then `schema.sql`, its statement of what it holds, which is found first.
fn created_in(home: &Path, dialect: Dialect) -> Vec<(PathBuf, Created)> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(home.join("migrations")).into_iter().flatten().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "sql")).collect();
    files.sort();
    files.push(home.join("schema.sql"));
    let mut created = Vec::new();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let tree = syntax::parse(dialect, &text);
        created.extend(created_tables(dialect, &tokens(dialect, &tree, &text)).into_iter().map(|c| (file.clone(), c)));
    }
    created
}

/// A span of a document's tokens, kept apart from its text.
#[derive(Clone, Copy)]
struct Span {
    start: u32,
    end: u32,
    class: Class,
}

/// What a document's text reads as, kept until the text changes.
struct Reading {
    spans: Vec<Span>,
    statements: Vec<(u32, u32)>,
    errors: Vec<(u32, u32, String)>,
    lines: Lines,
}

impl Reading {
    fn of(dialect: Dialect, text: &str) -> Reading {
        let written = for_grammar(dialect, text);
        let tree = syntax::parse(dialect, &written);
        let toks = tokens(dialect, &tree, text);
        let spans: Vec<Span> = toks.iter().map(|t| Span { start: t.start, end: t.end, class: t.class }).collect();
        let last = spans.last().map_or(0, |s| s.end as usize);
        let errors = tree
            .errors
            .iter()
            .take(MAX_ERRORS)
            .map(|e| {
                let (start, end) = (e.start as usize, e.end as usize);
                // A message quotes the token as the grammar read it, which a placeholder isn't.
                let message = if end > start { e.message.replace(&written[start..end], &text[start..end]) } else { e.message.clone() };
                // The end of the input is where its last token ends, not a blank line or a comment after it.
                if start >= last {
                    (last as u32, last as u32, message)
                } else {
                    (e.start, e.end, message)
                }
            })
            .collect();
        let statements = syntax::statements(dialect, &tree);
        Reading { spans, statements, errors, lines: Lines::new(text) }
    }

    /// The token at `offset`, or the name that ends there, where a cursor sits right after a word.
    fn token_at(&self, offset: usize) -> Option<usize> {
        let offset = offset as u32;
        let i = self.spans.partition_point(|t| t.end <= offset);
        if self.spans.get(i).is_some_and(|t| t.start <= offset) {
            return Some(i);
        }
        i.checked_sub(1).filter(|p| self.spans[*p].end == offset && matches!(self.spans[*p].class, Class::Word | Class::Quoted | Class::Variable))
    }

    /// The range of tokens in the statement around `offset`: the one holding it, else the last before it.
    fn statement_of(&self, offset: usize) -> (usize, usize) {
        let offset = offset as u32;
        let spans = &self.statements;
        let holding = spans.partition_point(|(a, _)| *a <= offset);
        let Some(&(start, end)) = holding.checked_sub(1).map(|i| &spans[i]).or(spans.first()) else { return (0, self.spans.len()) };
        // Text typed after the last statement's end still reads against it.
        let end = if spans.last() == Some(&(start, end)) { u32::MAX } else { end };
        (self.spans.partition_point(|t| t.start < start), self.spans.partition_point(|t| t.start < end))
    }

    fn toks<'s>(&self, text: &'s str, from: usize, to: usize) -> Vec<Tok<'s>> {
        self.spans[from..to].iter().map(|s| Tok { text: &text[s.start as usize..s.end as usize], start: s.start, end: s.end, class: s.class }).collect()
    }
}

struct Doc {
    text: String,
    version: i64,
    dir: Option<PathBuf>,
    /// The key of its [`Place`].
    place: Option<PathBuf>,
    reading: Option<Reading>,
    /// What the last analysis of this text found.
    analysis: Option<Box<Analysis>>,
    found: Vec<Value>,
}

/// What the main thread knows of a project.
struct Place {
    dialect: Dialect,
    tables: Vec<Table>,
    created: Vec<(PathBuf, Created)>,
    read_at: Option<Instant>,
    reading: bool,
}

struct Server {
    jobs: Sender<Job>,
    docs: HashMap<String, Doc>,
    places: HashMap<PathBuf, Place>,
    /// The documents to analyze, each with when it's due.
    pending: HashMap<String, Instant>,
    /// The documents whose database failed, whose project is read anew when they next come due.
    retrying: HashMap<String, PathBuf>,
    initialized: bool,
    shutting_down: bool,
}

impl Server {
    fn serve(&mut self, inbox: Receiver<Event>) -> i32 {
        loop {
            let wait = self.pending.values().map(|at| at.saturating_duration_since(Instant::now())).min().unwrap_or(Duration::from_secs(3600));
            match inbox.recv_timeout(wait) {
                Ok(Event::Message(Ok(message))) => {
                    if let Some(code) = self.handle(&message) {
                        return code;
                    }
                }
                Ok(Event::Message(Err(()))) => send(&json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "the message is not JSON" } })),
                Ok(Event::Closed) | Err(RecvTimeoutError::Disconnected) => return i32::from(!self.shutting_down),
                Ok(event) => self.from_worker(event),
                Err(RecvTimeoutError::Timeout) => {}
            }
            let now = Instant::now();
            let due: Vec<String> = self.pending.iter().filter(|(_, at)| **at <= now).map(|(uri, _)| uri.clone()).collect();
            for uri in due {
                self.pending.remove(&uri);
                self.analyze(&uri);
            }
        }
    }

    fn from_worker(&mut self, event: Event) {
        match event {
            Event::Located { uri, place } => {
                let Some(doc) = self.docs.get_mut(&uri) else { return };
                doc.reading = None;
                match place {
                    Some((key, dialect)) => {
                        doc.place = Some(key.clone());
                        let known = self.places.entry(key.clone()).or_insert_with(|| Place { dialect, tables: Vec::new(), created: Vec::new(), read_at: None, reading: false });
                        if known.dialect != dialect {
                            known.dialect = dialect;
                            known.tables.clear();
                            known.read_at = None;
                        }
                        self.publish(&uri);
                        self.read_schema(&key);
                        self.send_analysis(&uri);
                    }
                    None => {
                        doc.place = None;
                        self.publish(&uri);
                    }
                }
            }
            Event::Analyzed { uri, text, analysis, found } => {
                if let Some(doc) = self.docs.get_mut(&uri).filter(|d| d.text == text) {
                    doc.analysis = analysis;
                    doc.found = found;
                    self.publish(&uri);
                }
            }
            Event::Read { place, tables, created } => {
                if let Some(known) = self.places.get_mut(&place) {
                    known.tables = tables;
                    known.created = created;
                    known.read_at = Some(Instant::now());
                    known.reading = false;
                }
            }
            Event::Unavailable { place } => {
                if let Some(known) = self.places.get_mut(&place) {
                    known.reading = false;
                    known.read_at = Some(Instant::now());
                }
                // Try again, with the URL read anew, for a server that comes back or a `.env` that was fixed.
                let retry = Instant::now() + RETRY;
                let mine: Vec<String> = self.docs.iter().filter(|(_, d)| d.place.as_ref() == Some(&place)).map(|(uri, _)| uri.clone()).collect();
                for uri in mine {
                    self.pending.entry(uri.clone()).or_insert(retry);
                    if let Some(dir) = self.docs.get(&uri).and_then(|d| d.dir.clone()) {
                        self.retrying.insert(uri, dir);
                    }
                }
            }
            _ => {}
        }
    }

    /// Asks the worker for the schema, unless a read is recent or under way.
    fn read_schema(&mut self, key: &Path) {
        let Some(place) = self.places.get_mut(key) else { return };
        if place.reading || place.read_at.is_some_and(|at| at.elapsed() < FRESH) {
            return;
        }
        place.reading = true;
        let _ = self.jobs.send(Job::Read { place: key.to_path_buf() });
    }

    /// A document that's due: look its project up again where its database failed, else analyze it.
    fn analyze(&mut self, uri: &str) {
        if let Some(dir) = self.retrying.remove(uri) {
            let _ = self.jobs.send(Job::Locate { uri: uri.to_string(), dir, again: true });
            return;
        }
        let Some(doc) = self.docs.get(uri) else { return };
        if doc.place.is_none() {
            // No project yet: look again, as a `.env` may have appeared.
            if let Some(dir) = doc.dir.clone() {
                let _ = self.jobs.send(Job::Locate { uri: uri.to_string(), dir, again: true });
            }
            return;
        }
        self.send_analysis(uri);
    }

    fn send_analysis(&mut self, uri: &str) {
        let Some(doc) = self.docs.get_mut(uri) else { return };
        let Some(key) = doc.place.clone() else { return };
        let Some(place) = self.places.get(&key) else { return };
        let dialect = place.dialect;
        let exact = matches!(dialect, Dialect::Postgres | Dialect::Sqlite);
        let text = &doc.text;
        let reading = doc.reading.get_or_insert_with(|| Reading::of(dialect, text));
        // A file of several statements is a schema or a script, which no single analysis describes.
        if (exact && !reading.errors.is_empty()) || reading.statements.len() != 1 {
            return;
        }
        let _ = self.jobs.send(Job::Analyze { uri: uri.to_string(), place: key, text: doc.text.clone() });
    }

    /// Answers one message; the exit code, once the client says to exit.
    fn handle(&mut self, message: &Value) -> Option<i32> {
        let method = message["method"].as_str().unwrap_or("");
        let params = &message["params"];
        let Some(id) = message.get("id").filter(|_| !method.is_empty()) else {
            if method == "exit" {
                return Some(i32::from(!self.shutting_down));
            }
            // Before `initialize` and after `shutdown`, a notification is dropped.
            if self.initialized && !self.shutting_down {
                self.notified(method, params);
            }
            return None;
        };
        let answer: Result<Value, (i32, &str)> = match method {
            _ if self.shutting_down => Err((-32600, "the server is shutting down")),
            "initialize" if self.initialized => Err((-32600, "the server is already initialized")),
            "initialize" => {
                self.initialized = true;
                Ok(json!({
                    "capabilities": {
                        "positionEncoding": "utf-16",
                        "textDocumentSync": { "openClose": true, "change": 1, "save": { "includeText": false } },
                        "hoverProvider": true,
                        "definitionProvider": true,
                        "completionProvider": { "triggerCharacters": ["."] }
                    },
                    "serverInfo": { "name": "reylo", "version": env!("CARGO_PKG_VERSION") }
                }))
            }
            _ if !self.initialized => Err((-32002, "the server has not been initialized")),
            "shutdown" => {
                self.shutting_down = true;
                Ok(Value::Null)
            }
            "textDocument/hover" => Ok(self.hover(params)),
            "textDocument/completion" => Ok(self.completion(params)),
            "textDocument/definition" => Ok(self.definition(params)),
            _ => Err((-32601, "no such method")),
        };
        send(&match answer {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, text)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": text } }),
        });
        None
    }

    fn notified(&mut self, method: &str, params: &Value) {
        match method {
            "textDocument/didOpen" => {
                let d = &params["textDocument"];
                let uri = d["uri"].as_str().unwrap_or("").to_string();
                let dir = path_of(&uri).and_then(|p| p.parent().map(Path::to_path_buf));
                let doc = Doc { text: d["text"].as_str().unwrap_or("").to_string(), version: d["version"].as_i64().unwrap_or(0), dir: dir.clone(), place: None, reading: None, analysis: None, found: Vec::new() };
                self.docs.insert(uri.clone(), doc);
                if let Some(dir) = dir {
                    let _ = self.jobs.send(Job::Locate { uri, dir, again: false });
                }
            }
            "textDocument/didChange" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or("").to_string();
                if let Some(doc) = self.docs.get_mut(&uri) {
                    // The server asks for whole documents: the last change holds the text.
                    if let Some(text) = params["contentChanges"].as_array().and_then(|c| c.last()).and_then(|c| c["text"].as_str()) {
                        doc.text = text.to_string();
                    }
                    doc.version = params["textDocument"]["version"].as_i64().unwrap_or(doc.version);
                    doc.reading = None;
                    doc.analysis = None;
                    doc.found.clear();
                    self.publish(&uri);
                    self.pending.insert(uri, Instant::now() + QUIET);
                }
            }
            "textDocument/didSave" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or("");
                let Some(key) = self.docs.get(uri).and_then(|d| d.place.clone()) else { return };
                // A save may have changed the schema: every open file of the project is read again.
                if let Some(place) = self.places.get_mut(&key) {
                    place.read_at = None;
                }
                self.read_schema(&key);
                let now = Instant::now();
                let mine: Vec<String> = self.docs.iter().filter(|(_, d)| d.place.as_ref() == Some(&key)).map(|(u, _)| u.clone()).collect();
                for u in mine {
                    self.pending.insert(u, now);
                }
            }
            "textDocument/didClose" => {
                let uri = params["textDocument"]["uri"].as_str().unwrap_or("");
                self.docs.remove(uri);
                self.pending.remove(uri);
                self.retrying.remove(uri);
                send(&json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": { "uri": uri, "diagnostics": [] } }));
            }
            _ => {}
        }
    }

    /// Sends a document's syntax errors, and what the last analysis of this text found.
    fn publish(&mut self, uri: &str) {
        let Some(doc) = self.docs.get_mut(uri) else { return };
        let mut diagnostics: Vec<Value> = Vec::new();
        if let Some(place) = doc.place.as_ref().and_then(|p| self.places.get(p)) {
            let dialect = place.dialect;
            let text = &doc.text;
            let reading = doc.reading.get_or_insert_with(|| Reading::of(dialect, text));
            // A dialect read for its structure alone reports less exactly than its server, whose word stands.
            if matches!(dialect, Dialect::Postgres | Dialect::Sqlite) || doc.found.is_empty() {
                for (start, end, message) in &reading.errors {
                    diagnostics.push(diagnostic(text, &reading.lines, *start as usize, *end as usize, message, None));
                }
            }
        }
        diagnostics.extend(doc.found.iter().cloned());
        send(&json!({ "jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": { "uri": uri, "version": doc.version, "diagnostics": diagnostics } }));
    }

    /// The document a request names, read, with the byte offset of its position.
    fn at(&mut self, params: &Value) -> Option<(String, PathBuf, usize)> {
        let uri = params["textDocument"]["uri"].as_str()?.to_string();
        let doc = self.docs.get_mut(&uri)?;
        let key = doc.place.clone()?;
        let dialect = self.places.get(&key)?.dialect;
        let text = &doc.text;
        let reading = doc.reading.get_or_insert_with(|| Reading::of(dialect, text));
        let p = &params["position"];
        let offset = reading.lines.offset(text, p["line"].as_u64()? as u32, p["character"].as_u64()? as u32);
        self.read_schema(&key);
        Some((uri, key, offset))
    }

    fn hover(&mut self, params: &Value) -> Value {
        let Some((uri, key, offset)) = self.at(params) else { return Value::Null };
        let (doc, place) = (&self.docs[&uri], &self.places[&key]);
        let reading = doc.reading.as_ref().expect("read above");
        let Some(index) = reading.token_at(offset) else { return Value::Null };
        let (from, to) = reading.statement_of(offset);
        // A `;` between statements belongs to none.
        if index < from || index >= to {
            return Value::Null;
        }
        let stmt = reading.toks(&doc.text, from, to);
        let names = scopes(place.dialect, &stmt);
        let Some(markdown) = describe(place, doc.analysis.as_deref(), &stmt, &names, index - from) else { return Value::Null };
        json!({ "contents": { "kind": "markdown", "value": markdown }, "range": range(&doc.text, &reading.lines, stmt[index - from].start as usize, stmt[index - from].end as usize) })
    }

    fn completion(&mut self, params: &Value) -> Value {
        let empty = json!({ "isIncomplete": false, "items": [] });
        let Some((uri, home, offset)) = self.at(params) else { return empty };
        let (doc, place) = (&self.docs[&uri], &self.places[&home]);
        let reading = doc.reading.as_ref().expect("read above");
        if in_comment_or_string(&doc.text, reading, offset) {
            return empty;
        }
        let bytes = doc.text.as_bytes();
        let mut start = offset;
        while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_' || bytes[start - 1] >= 0x80) {
            start -= 1;
        }
        let prefix = &doc.text[start..offset];
        let (from, to) = reading.statement_of(start);
        // What the name being typed adds to the statement isn't part of what it already names.
        let stmt: Vec<Tok<'_>> = reading.toks(&doc.text, from, to).into_iter().filter(|t| !(t.start as usize == start && t.end as usize == offset)).collect();
        let names = scopes(place.dialect, &stmt);
        let before_count = stmt.partition_point(|t| (t.end as usize) <= start);
        // The query the cursor is in: that of the token before it, or the statement's own.
        let here = before_count.saturating_sub(1);
        let mut items = Vec::new();
        let dialect = place.dialect;
        match before_count.checked_sub(1).map(|i| stmt[i]) {
            Some(last) if last.is_punct(".") => {
                if let Some(q) = before_count.checked_sub(2).map(|i| stmt[i]).filter(Tok::is_name) {
                    let word = key(dialect, &q);
                    match names.source(here, &word) {
                        Some(source) => complete_source(place, &names, here, source, &mut items),
                        None => {
                            if let Some(cte) = names.cte(here, &word) {
                                items.extend(cte.columns.iter().map(|c| column_item(dialect, c, format!("{}.{c}", cte.name))));
                            }
                            for table in place.tables.iter().filter(|t| catalog_key(dialect, &t.schema) == word) {
                                items.push(table_item(dialect, table));
                            }
                        }
                    }
                }
            }
            Some(last) if last.is_any(&["FROM", "JOIN", "INTO", "UPDATE", "TABLE"]) => {
                items.extend(place.tables.iter().map(|t| table_item(dialect, t)));
                for q in names.chain(here) {
                    items.extend(names.queries[q].ctes.iter().map(|c| json!({ "label": c.name, "kind": 7, "detail": "WITH query", "insertText": quoted(dialect, &c.name) })));
                }
            }
            _ => {
                for q in names.chain(here) {
                    for s in &names.queries[q].sources {
                        complete_source(place, &names, here, s, &mut items);
                        if let Some(alias) = &s.alias {
                            items.push(json!({ "label": alias, "kind": 6, "detail": format!("alias of {}", s.name), "insertText": quoted(dialect, alias) }));
                        }
                    }
                }
                let upper = prefix.chars().next().is_some_and(char::is_uppercase);
                for word in syntax::keywords(dialect) {
                    let label = if upper { word.to_ascii_uppercase() } else { word.to_ascii_lowercase() };
                    items.push(json!({ "label": label, "kind": 14 }));
                }
            }
        }
        let wanted = prefix.to_lowercase();
        items.retain(|i| i["label"].as_str().is_some_and(|l| l.to_lowercase().starts_with(&wanted)));
        let mut seen = HashSet::new();
        items.retain(|i| seen.insert((i["label"].as_str().unwrap_or("").to_string(), i["kind"].as_u64())));
        json!({ "isIncomplete": false, "items": items })
    }

    fn definition(&mut self, params: &Value) -> Value {
        let Some((uri, key_of_place, offset)) = self.at(params) else { return Value::Null };
        let (doc, place) = (&self.docs[&uri], &self.places[&key_of_place]);
        let reading = doc.reading.as_ref().expect("read above");
        let Some(index) = reading.token_at(offset) else { return Value::Null };
        let (from, to) = reading.statement_of(offset);
        if index < from || index >= to {
            return Value::Null;
        }
        let stmt = reading.toks(&doc.text, from, to);
        let names = scopes(place.dialect, &stmt);
        let i = index - from;
        let t = stmt[i];
        let here = |at: (u32, u32)| json!({ "uri": uri, "range": range(&doc.text, &reading.lines, at.0 as usize, at.1 as usize) });
        // A parameter's first use.
        if t.class == Class::Variable {
            let first = stmt.iter().find(|o| o.class == Class::Variable && o.text == t.text).unwrap_or(&t);
            return here(first.at());
        }
        if !t.is_name() || (t.class == Class::Word && t.is_any(RESERVED)) {
            return Value::Null;
        }
        let dialect = place.dialect;
        let word = key(dialect, &t);
        let in_files = |source: &Source, column: Option<&str>| -> Option<Value> { self.created_at(place, source, column) };
        // A table reference, at its name or its alias.
        if let Some(s) = names.source_at(t.at()) {
            if s.kind == SourceKind::Table && s.schema.is_none() {
                if let Some(c) = names.cte(i, &s.key) {
                    return here(c.at);
                }
            }
            return in_files(s, None).unwrap_or(Value::Null);
        }
        let qualifier = i.checked_sub(2).filter(|q| stmt[q + 1].is_punct(".") && stmt[*q].is_name()).map(|q| key(dialect, &stmt[q]));
        if let Some(q) = qualifier {
            let Some(s) = names.source(i, &q) else {
                return names.cte(i, &q).map_or(Value::Null, |c| here(c.at));
            };
            if s.kind == SourceKind::Table && s.schema.is_none() {
                if let Some(c) = names.cte(i, &s.key) {
                    return here(c.at);
                }
            }
            return in_files(s, Some(&word)).unwrap_or(Value::Null);
        }
        // The first part of `alias.column`.
        if stmt.get(i + 1).is_some_and(|n| n.is_punct(".")) {
            if let Some(s) = names.source(i, &word) {
                return match s.alias_at {
                    Some(at) => here(at),
                    None => in_files(s, None).unwrap_or(Value::Null),
                };
            }
        }
        // A column of the nearest query that has one of that name.
        for q in names.chain(i) {
            let found = names.queries[q].sources.iter().filter(|s| has_column(place, &names, i, s, &word)).find_map(|s| in_files(s, Some(&word)));
            if let Some(found) = found {
                return found;
            }
        }
        Value::Null
    }

    /// Where the project's files create `source`'s table, or a column of it. An open file is read as it stands in the editor.
    fn created_at(&self, place: &Place, source: &Source, column: Option<&str>) -> Option<Value> {
        if source.kind != SourceKind::Table {
            return None;
        }
        let table = find_table(place, source.schema.as_deref(), &source.key);
        let first_schema = place.tables.iter().filter_map(|t| t.rank.map(|r| (r, &t.schema))).min_by_key(|(r, _)| *r).map(|(_, s)| s.clone());
        let matches = |c: &Created| {
            c.key == source.key
                && match table {
                    Some(t) => match c.schema.as_deref() {
                        Some(s) => s == catalog_key(place.dialect, &t.schema),
                        None => first_schema.as_deref() == Some(t.schema.as_str()),
                    },
                    None => source.schema.is_none() || c.schema == source.schema,
                }
        };
        let files: HashSet<&Path> = place.created.iter().map(|(f, _)| f.as_path()).collect();
        let open: Vec<(&Path, &str)> = files.into_iter().filter_map(|f| self.docs.get(&uri_of(f)).map(|d| (f, d.text.as_str()))).collect();
        let edited: Vec<(PathBuf, Created)> = open
            .iter()
            .flat_map(|(f, text)| {
                let tree = syntax::parse(place.dialect, text);
                created_tables(place.dialect, &tokens(place.dialect, &tree, text)).into_iter().map(|c| (f.to_path_buf(), c)).collect::<Vec<_>>()
            })
            .collect();
        let is_open = |f: &Path| open.iter().any(|(o, _)| *o == f);
        // Migrations in order, then `schema.sql`, and the last that creates it is the one that holds.
        let mut all: Vec<&(PathBuf, Created)> = place.created.iter().filter(|(f, _)| !is_open(f)).chain(edited.iter()).collect();
        all.sort_by_key(|(f, _)| (f.file_name().is_some_and(|n| n == "schema.sql"), f.clone()));
        let (file, c) = all.into_iter().rev().find(|(_, c)| matches(c))?;
        let at = match column {
            Some(column) => c.columns.iter().find(|x| x.0 == column)?.1,
            None => c.at,
        };
        let target = uri_of(file);
        let text = self.docs.get(&target).map(|d| d.text.clone()).or_else(|| std::fs::read_to_string(file).ok())?;
        let lines = Lines::new(&text);
        Some(json!({ "uri": target, "range": range(&text, &lines, at.0 as usize, at.1 as usize) }))
    }
}

/// Whether the cursor sits inside a comment or a string, where no completion applies.
fn in_comment_or_string(text: &str, reading: &Reading, offset: usize) -> bool {
    let i = reading.spans.partition_point(|t| (t.end as usize) <= offset);
    if let Some(t) = reading.spans.get(i).filter(|t| (t.start as usize) < offset) {
        return t.class == Class::String;
    }
    // The trivia between the token before the cursor and the cursor.
    let gap_start = i.checked_sub(1).map_or(0, |p| reading.spans[p].end as usize);
    let gap = &text[gap_start..offset];
    if let Some(open) = gap.rfind("/*") {
        if !gap[open..].contains("*/") {
            return true;
        }
    }
    gap.rsplit(['\n', '\r']).next().unwrap_or("").contains("--")
}

fn complete_source(place: &Place, names: &Scopes, at: usize, source: &Source, items: &mut Vec<Value>) {
    let dialect = place.dialect;
    match &source.kind {
        SourceKind::Derived(columns) => items.extend(columns.iter().map(|c| column_item(dialect, c, format!("{}.{c}", source.name)))),
        SourceKind::Function => {}
        SourceKind::Table => {
            if source.schema.is_none() {
                if let Some(cte) = names.cte(at, &source.key) {
                    items.extend(cte.columns.iter().map(|c| column_item(dialect, c, format!("{}.{c}", cte.name))));
                    return;
                }
            }
            if let Some(table) = find_table(place, source.schema.as_deref(), &source.key) {
                items.extend(table.columns.iter().map(|c| column_item(dialect, &c.name, format!("{}.{} {}", table.name, c.name, type_label(c)))));
            }
        }
    }
}

fn column_item(dialect: Dialect, name: &str, detail: String) -> Value {
    json!({ "label": name, "kind": 5, "detail": detail, "insertText": quoted(dialect, name) })
}

fn table_item(dialect: Dialect, table: &Table) -> Value {
    let name = quoted(dialect, &table.name);
    let insert = if table.rank.is_some() { name } else { format!("{}.{name}", quoted(dialect, &table.schema)) };
    json!({ "label": table.name, "kind": 7, "detail": format!("{} ({} columns)", qualified(table), table.columns.len()), "insertText": insert })
}

/// A name as `dialect` reads it back: bare where it can be, and quoted where it can't.
fn quoted(dialect: Dialect, name: &str) -> String {
    let letter = |c: char| if dialect == Dialect::Postgres { c.is_ascii_lowercase() } else { c.is_ascii_alphabetic() };
    let first = name.chars().next().is_some_and(|c| c == '_' || letter(c));
    let rest = name.chars().all(|c| c == '_' || c.is_ascii_digit() || letter(c));
    if first && rest && !RESERVED.iter().any(|w| w.eq_ignore_ascii_case(name)) {
        return name.to_string();
    }
    match dialect {
        Dialect::Mysql | Dialect::ClickHouse => format!("`{}`", name.replace('`', "``")),
        Dialect::SqlServer => format!("[{}]", name.replace(']', "]]")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

fn columns_query(dialect: &str) -> &'static str {
    match dialect {
        "postgres" => "select n.nspname as s, c.relname as t, a.attname as c, pg_catalog.format_type(a.atttypid, a.atttypmod) as ty, case when a.attnotnull then '1' else '0' end as nn, coalesce(pg_catalog.array_position(pg_catalog.current_schemas(false), n.nspname)::pg_catalog.text, '') as sp from pg_catalog.pg_attribute a join pg_catalog.pg_class c on c.oid = a.attrelid join pg_catalog.pg_namespace n on n.oid = c.relnamespace where c.relkind in ('r', 'v', 'm', 'f', 'p') and a.attnum > 0 and not a.attisdropped and n.nspname not in ('pg_catalog', 'information_schema') and n.nspname not like 'pg\\_toast%' order by n.nspname, c.relname, a.attnum",
        "mysql" => "select cast(table_schema as char) as s, cast(table_name as char) as t, cast(column_name as char) as c, cast(column_type as char) as ty, if(is_nullable = 'NO', '1', '0') as nn, '0' as sp from information_schema.columns where table_schema = database() order by table_name, ordinal_position",
        // A primary key holds NULL unless it is the rowid or the table is WITHOUT ROWID.
        _ => "select 'main' as s, m.name as t, p.name as c, p.type as ty, case when p.\"notnull\" or (p.pk > 0 and (select l.wr from pragma_table_list l where l.schema = 'main' and l.name = m.name)) or (p.pk = 1 and lower(p.type) = 'integer' and (select count(*) from pragma_table_xinfo(m.name) q where q.pk > 0) = 1) then '1' else '0' end as nn, '0' as sp from sqlite_schema m join pragma_table_xinfo(m.name) p where m.type in ('table', 'view') and m.name not like 'sqlite\\_%' escape '\\' order by m.name, p.cid",
    }
}

/// The byte offset of a UTF-16 offset into `text`, clamped to its end.
fn byte_of(text: &str, units: usize) -> usize {
    let mut seen = 0;
    for (byte, c) in text.char_indices() {
        if seen >= units {
            return byte;
        }
        seen += c.len_utf16();
    }
    text.len()
}

fn range(text: &str, lines: &Lines, start: usize, end: usize) -> Value {
    let (a, b) = (lines.position(text, start), lines.position(text, end.max(start)));
    json!({ "start": { "line": a.0, "character": a.1 }, "end": { "line": b.0, "character": b.1 } })
}

fn diagnostic(text: &str, lines: &Lines, start: usize, end: usize, message: &str, code: Option<&str>) -> Value {
    let mut out = json!({ "range": range(text, lines, start, end), "severity": 1, "source": "reylo", "message": message });
    if let Some(code) = code {
        out["code"] = json!(code);
    }
    out
}

/// The table a name reaches: in its schema, or the first on the search path.
fn find_table<'p>(place: &'p Place, schema: Option<&str>, key: &str) -> Option<&'p Table> {
    let mut named = place.tables.iter().filter(|t| t.key == key);
    match schema {
        Some(schema) => named.find(|t| catalog_key(place.dialect, &t.schema) == schema),
        None => named.filter(|t| t.rank.is_some()).min_by_key(|t| t.rank),
    }
}

fn qualified(table: &Table) -> String {
    if table.rank.is_some() {
        table.name.clone()
    } else {
        format!("{}.{}", table.schema, table.name)
    }
}

fn type_label(c: &Column) -> String {
    format!("{}{}", c.ty, if c.nullable { "?" } else { "" })
}

fn table_markdown(table: &Table) -> String {
    let lines: Vec<String> = table.columns.iter().map(|c| format!("  {} {}", c.name, type_label(c))).collect();
    format!("```sql\n{} (\n{}\n)\n```", qualified(table), lines.join(",\n"))
}

/// Whether a name read off a subquery's text is this column.
fn same_column(column: &str, word: &str) -> bool {
    column.to_lowercase() == word.to_lowercase()
}

/// Whether `source` has a column of this key, as far as what it reads says.
fn has_column(place: &Place, names: &Scopes, at: usize, source: &Source, word: &str) -> bool {
    match &source.kind {
        SourceKind::Derived(columns) => columns.iter().any(|c| same_column(c, word)),
        SourceKind::Function => false,
        SourceKind::Table => {
            if source.schema.is_none() {
                if let Some(cte) = names.cte(at, &source.key) {
                    return cte.columns.iter().any(|c| same_column(c, word));
                }
            }
            find_table(place, source.schema.as_deref(), &source.key).is_some_and(|t| t.columns.iter().any(|c| c.key == word))
        }
    }
}

/// What a source's column reads as on hover.
fn column_markdown(place: &Place, names: &Scopes, at: usize, source: &Source, word: &str, result: &str) -> Option<String> {
    let bare = |owner: &str, column: &str| format!("```sql\n{owner}.{column}\n```{result}");
    match &source.kind {
        SourceKind::Derived(columns) => columns.iter().find(|c| same_column(c, word)).map(|c| bare(&source.name, c)),
        SourceKind::Function => None,
        SourceKind::Table => {
            if source.schema.is_none() {
                if let Some(cte) = names.cte(at, &source.key) {
                    return cte.columns.iter().find(|c| same_column(c, word)).map(|c| bare(&cte.name, c));
                }
            }
            let table = find_table(place, source.schema.as_deref(), &source.key)?;
            table.columns.iter().find(|c| c.key == word).map(|c| format!("```sql\n{}.{} {}\n```{result}", qualified(table), c.name, type_label(c)))
        }
    }
}

/// What a source itself reads as on hover.
fn source_markdown(place: &Place, names: &Scopes, at: usize, source: &Source) -> Option<String> {
    let list = |name: &str, columns: &[String]| format!("```sql\n{name} (\n{}\n)\n```", columns.iter().map(|c| format!("  {c}")).collect::<Vec<_>>().join(",\n"));
    match &source.kind {
        SourceKind::Derived(columns) => Some(list(&source.name, columns)),
        SourceKind::Function => None,
        SourceKind::Table => {
            if source.schema.is_none() {
                if let Some(cte) = names.cte(at, &source.key) {
                    return Some(list(&cte.name, &cte.columns));
                }
            }
            find_table(place, source.schema.as_deref(), &source.key).map(table_markdown)
        }
    }
}

/// What hover says of the token at `at` of the statement `stmt`.
fn describe(place: &Place, analysis: Option<&Analysis>, stmt: &[Tok<'_>], names: &Scopes, at: usize) -> Option<String> {
    let t = stmt[at];
    let field = |name: &str, ty: String, nullable: bool| format!("{name}: {ty}{}", if nullable { "?" } else { "" });
    if t.class == Class::Variable {
        let bare = t.text.trim_start_matches([':', '$', '@', '?']);
        let p = analysis?.params.iter().find(|p| p.name == bare)?;
        return Some(format!("```sql\n{}\n```", field(t.text, p.ty.short(), p.nullable)));
    }
    if at == 0 && t.class == Class::Word {
        let a = analysis?;
        let mut lines = vec![format!("-- {} ({})", t.text.to_ascii_uppercase(), a.cardinality.as_str().replace('_', " "))];
        lines.extend(a.params.iter().map(|p| format!("-- param {}", field(&p.name, p.ty.short(), p.nullable))));
        lines.extend(a.columns.iter().map(|c| field(&c.name, c.ty.short(), c.nullable)));
        return Some(format!("```sql\n{}\n```", lines.join("\n")));
    }
    if !t.is_name() || (t.class == Class::Word && t.is_any(RESERVED)) {
        return None;
    }
    let dialect = place.dialect;
    let word = key(dialect, &t);
    // Only the outermost query's select list writes the result, and only where the columns line up.
    let output = output_column(stmt, at).filter(|_| names.of_token[at] == 0).and_then(|k| {
        let a = analysis?;
        let (from, to) = select_list(stmt)?;
        (items(stmt, from, to).len() == a.columns.len()).then(|| a.columns.get(k)).flatten()
    });
    let result = output.map(|c| format!("\n\nIn this result: `{}`", field(&c.name, c.ty.short(), c.nullable))).unwrap_or_default();
    let qualifier = at.checked_sub(2).filter(|q| stmt[q + 1].is_punct(".") && stmt[*q].is_name()).map(|q| key(dialect, &stmt[q]));
    // A table reference, at its name or its alias.
    if let Some(s) = names.source_at(t.at()) {
        return source_markdown(place, names, at, s);
    }
    if let Some(q) = &qualifier {
        return match names.source(at, q) {
            Some(s) => column_markdown(place, names, at, s, &word, &result),
            None => names.cte(at, q).and_then(|c| c.columns.iter().find(|x| same_column(x, &word)).map(|x| format!("```sql\n{}.{x}\n```{result}", c.name))),
        };
    }
    if stmt.get(at + 1).is_some_and(|n| n.is_punct(".")) {
        if let Some(s) = names.source(at, &word) {
            return source_markdown(place, names, at, s);
        }
    }
    // A column of the nearest query that has one of that name.
    for q in names.chain(at) {
        let found: Vec<String> = names.queries[q].sources.iter().filter_map(|s| column_markdown(place, names, at, s, &word, &result)).collect();
        if !found.is_empty() {
            return Some(found.join("\n\n"));
        }
    }
    output.map(|c| format!("```sql\n{}\n```", field(&c.name, c.ty.short(), c.nullable)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_framed_message_past_other_headers() {
        let body = r#"{"id":1}"#;
        let framed = format!("Content-Type: application/vscode-jsonrpc\r\nContent-Length: {}\r\n\r\n{body}", body.len());
        let mut input = std::io::Cursor::new(framed.into_bytes());
        assert_eq!(read_message(&mut input), Some(Ok(json!({ "id": 1 }))));
        assert_eq!(read_message(&mut input), None);
    }

    #[test]
    fn refuses_a_length_it_wont_allocate_and_reports_what_isnt_json() {
        let mut huge = std::io::Cursor::new(b"Content-Length: 999999999999999\r\n\r\n".to_vec());
        assert_eq!(read_message(&mut huge), None);
        let mut broken = std::io::Cursor::new(b"Content-Length: 2\r\n\r\n{]".to_vec());
        assert_eq!(read_message(&mut broken), Some(Err(())));
    }

    #[test]
    fn reads_file_uris_both_ways() {
        let path = path_of("file:///tmp/a%20b/caf%C3%A9.sql").unwrap();
        assert_eq!(path, PathBuf::from("/tmp/a b/café.sql"));
        assert_eq!(uri_of(&path), "file:///tmp/a%20b/caf%C3%A9.sql");
        assert_eq!(path_of("untitled:Untitled-1"), None);
    }

    #[test]
    fn quotes_a_name_only_where_the_dialect_needs_it() {
        for (dialect, name, written) in [(Dialect::Postgres, "users", "users"), (Dialect::Postgres, "camelCase", "\"camelCase\""), (Dialect::Postgres, "two words", "\"two words\""), (Dialect::Postgres, "order", "\"order\""), (Dialect::Mysql, "camelCase", "camelCase"), (Dialect::Mysql, "two words", "`two words`"), (Dialect::SqlServer, "a]b", "[a]]b]")] {
            assert_eq!(quoted(dialect, name), written);
        }
    }
}

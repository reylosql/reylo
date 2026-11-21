//! `reylo lsp` over its real protocol, on a SQLite project: what an editor sends, and what it gets back.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const SCHEMA: &str = "create table users (id integer primary key, name text not null, nick text);\ncreate table posts (id integer primary key, user_id integer not null references users(id), body text);\n";
const QUERY: &str = "select u.id, u.name, p.body\nfrom users u\nleft join posts p on p.user_id = u.id\nwhere u.id = :id\n";

struct Project(PathBuf);

impl Project {
    fn new(name: &str) -> Project {
        let dir = std::env::temp_dir().join(format!("reylo-lsp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("reylo")).unwrap();
        std::fs::write(dir.join("package.json"), "{}").unwrap();
        std::fs::write(dir.join(".env"), "DATABASE_URL=file:./app.db\n").unwrap();
        std::fs::write(dir.join("reylo/schema.sql"), SCHEMA).unwrap();
        // An empty file is an empty SQLite database, which `reylo run` writes to.
        std::fs::write(dir.join("app.db"), "").unwrap();
        let made = Command::new(env!("CARGO_BIN_EXE_reylo")).args(["run", SCHEMA]).current_dir(&dir).env_remove("DATABASE_URL").output().unwrap();
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
        Project(dir)
    }

    fn uri(&self, file: &str) -> String {
        format!("file://{}", self.0.join(file).display())
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Client {
    child: Child,
    input: ChildStdin,
    messages: Receiver<Value>,
    next: i64,
    diagnostics: Vec<(String, Value)>,
}

impl Client {
    fn start(dir: &Path) -> Client {
        Client::start_with(dir, &[])
    }

    fn start_with(dir: &Path, flags: &[&str]) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_reylo")).arg("lsp").args(flags).current_dir(dir).env_remove("DATABASE_URL").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let input = child.stdin.take().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let (sender, messages) = mpsc::channel();
        std::thread::spawn(move || loop {
            let mut length = 0;
            loop {
                let mut line = String::new();
                if output.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let line = line.trim_end();
                if line.is_empty() {
                    break;
                }
                if let Some(value) = line.strip_prefix("Content-Length: ") {
                    length = value.parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            if output.read_exact(&mut body).is_err() || sender.send(serde_json::from_slice(&body).unwrap()).is_err() {
                return;
            }
        });
        let mut client = Client { child, input, messages, next: 0, diagnostics: Vec::new() };
        let capabilities = client.request("initialize", json!({ "processId": null, "rootUri": null, "capabilities": {} }));
        assert_eq!(capabilities["capabilities"]["hoverProvider"], true);
        client.notify("initialized", json!({}));
        client
    }

    fn write(&mut self, message: Value) {
        let body = message.to_string();
        write!(self.input, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.input.flush().unwrap();
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.write(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn receive(&mut self, until: Instant) -> Option<Value> {
        let message = self.messages.recv_timeout(until.saturating_duration_since(Instant::now())).ok()?;
        if message["method"] == "textDocument/publishDiagnostics" {
            self.diagnostics.push((message["params"]["uri"].as_str().unwrap().to_string(), message["params"]["diagnostics"].clone()));
        }
        Some(message)
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            let message = self.receive(until).unwrap_or_else(|| panic!("no answer to {method}"));
            if message["id"] == id {
                assert!(message.get("error").is_none(), "{method}: {message}");
                return message["result"].clone();
            }
        }
    }

    /// Reads until the document's newest diagnostics say `message`.
    fn wait_for(&mut self, uri: &str, message: &str) -> Value {
        let until = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some((_, last)) = self.diagnostics.iter().rev().find(|(u, _)| u == uri) {
                if last.as_array().unwrap().iter().any(|d| d["message"].as_str().unwrap().contains(message)) {
                    return last.clone();
                }
            }
            self.receive(until).unwrap_or_else(|| panic!("no diagnostic saying {message}: {:?}", self.diagnostics.last()));
        }
    }

    fn open(&mut self, uri: &str, text: &str) {
        self.notify("textDocument/didOpen", json!({ "textDocument": { "uri": uri, "languageId": "sql", "version": 1, "text": text } }));
    }

    fn change(&mut self, uri: &str, version: i64, text: &str) {
        self.notify("textDocument/didChange", json!({ "textDocument": { "uri": uri, "version": version }, "contentChanges": [{ "text": text }] }));
    }

    /// Asks `method` at the start of `needle`, or just past it.
    fn at(&mut self, method: &str, uri: &str, text: &str, needle: &str, past: bool) -> Value {
        let at = text.find(needle).unwrap() + if past { needle.len() } else { 0 };
        let line = text[..at].matches('\n').count();
        let character = at - text[..at].rfind('\n').map_or(0, |i| i + 1);
        self.request(method, json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } }))
    }

    /// Completion just past `needle`, once the schema has been read.
    fn complete(&mut self, uri: &str, text: &str, needle: &str) -> Value {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let answer = self.at("textDocument/completion", uri, text, needle, true);
            if !answer["items"].as_array().unwrap().is_empty() || Instant::now() > until {
                return answer;
            }
            let _ = self.receive(Instant::now() + Duration::from_millis(50));
        }
    }

    fn hover(&mut self, uri: &str, text: &str, needle: &str) -> String {
        // The analysis runs once the text is still; a hover before it answers from the schema alone.
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let answer = self.at("textDocument/hover", uri, text, needle, false);
            let value = answer["contents"]["value"].as_str().unwrap_or("").to_string();
            if value.contains("In this result") || !needle.eq("body") || Instant::now() > until {
                return value;
            }
            let _ = self.receive(Instant::now() + Duration::from_millis(100));
        }
    }

    fn finish(mut self) {
        self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        drop(self.input);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{status}");
                return;
            }
            assert!(Instant::now() < deadline, "the server did not exit");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn labels(answer: &Value) -> Vec<String> {
    answer["items"].as_array().unwrap().iter().map(|i| i["label"].as_str().unwrap().to_string()).collect()
}

#[test]
fn an_editor_gets_types_completion_jumps_and_errors() {
    let project = Project::new("editor");
    let mut client = Client::start(&project.0);
    let uri = project.uri("q.sql");
    client.open(&uri, QUERY);

    let body = client.hover(&uri, QUERY, "body");
    assert!(body.contains("posts.body TEXT?") && body.contains("In this result"), "{body}");
    let summary = client.hover(&uri, QUERY, "select");
    assert!(summary.contains("SELECT (many)") && summary.contains("param id: int64"), "{summary}");
    assert!(client.hover(&uri, QUERY, ":id").contains(":id: int64"));

    let definition = client.at("textDocument/definition", &uri, QUERY, "users", false);
    assert!(definition["uri"].as_str().unwrap().ends_with("reylo/schema.sql"), "{definition}");
    assert_eq!(definition["range"]["start"], json!({ "line": 0, "character": 13 }));
    let column = client.at("textDocument/definition", &uri, QUERY, "body", false);
    assert_eq!(column["range"]["start"]["line"], 1);
    let alias = client.at("textDocument/definition", &uri, QUERY, "u.name", false);
    assert_eq!(alias["uri"], uri.as_str());
    assert_eq!(alias["range"]["start"], json!({ "line": 1, "character": 11 }));

    client.change(&uri, 2, "selec 1");
    let typo = client.wait_for(&uri, "near \"selec\": syntax error");
    assert_eq!(typo[0]["range"]["start"], json!({ "line": 0, "character": 0 }));
    client.change(&uri, 3, "select nope from users");
    client.wait_for(&uri, "no such column: nope");

    let typing = "select u. from users u";
    client.change(&uri, 4, typing);
    assert_eq!(labels(&client.at("textDocument/completion", &uri, typing, "u.", true)), ["id", "name", "nick"]);
    let from = "select id from ";
    client.change(&uri, 5, from);
    assert_eq!(labels(&client.at("textDocument/completion", &uri, from, "from ", true)), ["posts", "users"]);
    client.change(&uri, 6, "sel");
    assert_eq!(labels(&client.at("textDocument/completion", &uri, "sel", "sel", true)), ["select"]);
    client.finish();
}

#[test]
fn a_completion_in_an_unclosed_quote_keeps_the_rest_of_the_file() {
    let project = Project::new("quote");
    let mut client = Client::start(&project.0);
    let uri = project.uri("q.sql");
    let unclosed = "select *\nfrom \"us\nwhere id = 1\norder by id;\n";
    client.open(&uri, unclosed);
    let answer = client.complete(&uri, unclosed, "\"us");
    let edit = &answer["items"][0]["textEdit"];
    assert_eq!(edit["newText"], "\"users\"");
    assert_eq!(edit["range"], json!({ "start": { "line": 1, "character": 5 }, "end": { "line": 1, "character": 8 } }));
    assert_eq!(labels(&client.at("textDocument/completion", &uri, unclosed, "where", true)), Vec::<String>::new());

    // The quote after the cursor may be another name's: nothing past the cursor goes.
    let beside = "select * from \"us,\"posts\"";
    client.change(&uri, 2, beside);
    let edit = client.complete(&uri, beside, "\"us")["items"][0]["textEdit"].clone();
    assert_eq!(edit["newText"], "\"users");
    assert_eq!(edit["range"]["end"], json!({ "line": 0, "character": 17 }));

    let closed = "select * from \"us\"";
    client.change(&uri, 3, closed);
    let answer = client.complete(&uri, closed, "\"us");
    assert_eq!(answer["items"][0]["textEdit"]["range"]["end"], json!({ "line": 0, "character": 18 }));
    client.finish();
}

#[test]
fn a_with_query_offers_the_columns_it_names() {
    let project = Project::new("with");
    let mut client = Client::start(&project.0);
    let uri = project.uri("q.sql");
    let returning = "with x as (insert into users (name) values ('a') returning id, name) select x. from x";
    client.open(&uri, returning);
    assert_eq!(labels(&client.complete(&uri, returning, "select x.")), ["id", "name"]);
    client.finish();
}

#[test]
fn a_new_database_url_takes_effect_on_the_next_edit() {
    let project = Project::new("moved");
    let mut client = Client::start(&project.0);
    let uri = project.uri("q.sql");
    client.open(&uri, "select nope from users");
    client.wait_for(&uri, "no such column: nope");

    std::fs::write(project.0.join("other.db"), "").unwrap();
    std::fs::write(project.0.join(".env"), "DATABASE_URL=file:./other.db\n").unwrap();
    let made = Command::new(env!("CARGO_BIN_EXE_reylo")).args(["run", "create table other (a integer)"]).current_dir(&project.0).env_remove("DATABASE_URL").output().unwrap();
    assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    client.change(&uri, 2, "select id from users");
    client.wait_for(&uri, "no such table: users");
    client.finish();
}

#[test]
fn an_inserts_alias_reaches_returning_past_a_set_operation() {
    let project = Project::new("compound");
    let mut client = Client::start(&project.0);
    let uri = project.uri("q.sql");
    let text = "insert into users as u (id) select 1 union all select 2 returning u.id";
    client.open(&uri, text);
    client.complete(&uri, text, "returning u.");
    let definition = client.at("textDocument/definition", &uri, text, "u.id", false);
    assert_eq!(definition["range"]["start"], json!({ "line": 0, "character": 21 }));
    client.finish();
}

#[test]
fn a_project_runs_no_command_unless_the_editor_allows_it() {
    let project = Project::new("command");
    let command = if cfg!(windows) { "(type nul > ran) & echo ./app.db" } else { "touch ran; echo ./app.db" };
    std::fs::write(project.0.join(".reylo.yaml"), format!("direct_url: \"file:$({command})\"\n")).unwrap();
    let uri = project.uri("q.sql");
    let mut client = Client::start(&project.0);
    client.open(&uri, "selec 1");
    client.wait_for(&uri, "syntax error");
    assert!(!project.0.join("ran").exists());
    client.finish();

    let mut client = Client::start_with(&project.0, &["--allow-url-command"]);
    client.open(&uri, "select nope from users");
    client.wait_for(&uri, "no such column: nope");
    assert!(project.0.join("ran").exists());
    client.finish();
}

#[test]
fn a_syntax_error_says_what_would_have_fit() {
    let project = Project::new("expected");
    let mut client = Client::start(&project.0);
    let uri = project.uri("q.sql");
    client.open(&uri, "select id from users order");
    let found = client.wait_for(&uri, "incomplete input");
    assert!(found[0]["message"].as_str().unwrap().ends_with("\nExpected BY"), "{found}");
    client.finish();
}

#[test]
fn a_file_of_many_statements_gets_syntax_errors_alone() {
    let project = Project::new("script");
    let mut client = Client::start(&project.0);
    let uri = project.uri("script.sql");
    let text = "select nope from users;\nselect 1 from;\n";
    client.open(&uri, text);
    let found = client.wait_for(&uri, "near \";\": syntax error");
    // Only the grammar's error: no single analysis describes a script, so `nope` goes unremarked.
    assert_eq!(found.as_array().unwrap().len(), 1, "{found}");
    assert_eq!(found[0]["range"]["start"]["line"], 1);
    client.finish();
}

#[test]
fn a_request_for_nothing_it_knows_is_refused_and_a_notification_ignored() {
    let project = Project::new("protocol");
    let mut client = Client::start(&project.0);
    client.notify("workspace/didChangeConfiguration", json!({ "settings": {} }));
    client.next += 1;
    let id = client.next;
    client.write(json!({ "jsonrpc": "2.0", "id": id, "method": "textDocument/rename", "params": {} }));
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let message = client.receive(until).expect("an answer");
        if message["id"] == id {
            assert_eq!(message["error"]["code"], -32601);
            break;
        }
    }
    client.finish();
}

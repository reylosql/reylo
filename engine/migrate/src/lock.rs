//! `reylo.lock`, the project's lock: the migrations in the order they run, each its name and a checksum chained to the entry before, and
//! the checksum of `schema.sql` from the last time the migrations were proven to reach it. Two
//! branches that each add a migration both append to it, so git conflicts on it when the second one
//! merges, which is the point: before anything is deployed, one branch's migrations must move after
//! the other's (keep main's lines, then `reylo migrate dev`). The chain makes an edit, a
//! deletion or a reordering visible without a database: a wrong "accept both" breaks it at the
//! first entry out of place. Only Reylo writes it, after applying what it lists to a development
//! database (`dev`) or reading what a database applied (`history pull`), so its checksums certify
//! what ran.

use crate::cache::{hex, Sha256};
use regex::Regex;
use serde::Deserialize;
use std::path::Path;
use std::sync::LazyLock;

pub const LOCK: &str = "reylo.lock";

/// The lock's name before 0.8.0. Every command that reads or writes the lock refuses a project that
/// still holds it, rather than read it: read as no lock, it would run the migrations in name order.
pub const OLD_LOCK: &str = "reylo-lock.yaml";

const HEADER: &str = "# On a conflict, keep main's and run reylo migrate dev\n";

/// The version of the lock's format: how its checksums are taken (`checksum`, `schema_sum` and the
/// text `canonical` keeps) and how it is written. A lock with none is version 1, as written up to
/// 0.8.2. A change to any of them moves it, so that a lock written by the old rules is still
/// checked by them, and a Reylo that reads an older version refuses a newer lock rather than read
/// its checksums by the wrong rules, which would show every migration as edited.
pub const VERSION: u64 = 1;

/// The lock: its migrations in order, and schema.sql's checksum where one was proven.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lock {
    pub schema: Option<String>,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// The checksum of the migration's statements and the entry before it.
    pub sum: String,
}

impl Lock {
    pub fn names(&self) -> Vec<String> {
        self.entries.iter().map(|e| e.name.clone()).collect()
    }
}

/// The lock as it reads on disk.
#[derive(Debug, PartialEq, Eq)]
pub enum State {
    Missing,
    /// It is there and can't be read (its permissions, a directory, text that isn't UTF-8): why.
    Unreadable(String),
    /// It holds git's conflict markers.
    Conflicted,
    Invalid(String),
    /// A newer Reylo wrote it: its version, above `VERSION`.
    Newer(u64),
    Lock(Lock),
}

/// The lock's version alone, read before the rest, which a newer version may write differently: as
/// its text, which is a version only as digits with no leading zero, so that every reader of the
/// lock (`readLock` in src/embedded-module.ts too) takes the same text for the same version.
#[derive(Deserialize)]
struct Versioned {
    #[serde(default)]
    version: Option<String>,
}

static VERSION_TEXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[1-9][0-9]{0,8}$").expect("a valid pattern"));

#[derive(Deserialize)]
struct LockFile {
    #[serde(default)]
    schema: Option<String>,
    #[serde(default)]
    migrations: Vec<EntryFile>,
}

/// An entry of `migrations`: an object with its `name` and its `checksum`, or, as locks were
/// written up to 0.8.2, one string with the two split by a space. Each value is read as the text
/// it is, so a checksum of digits alone keeps its leading zeros. A key it doesn't know is refused,
/// since a later version wrote it and this one would drop it when it writes the lock again, and so
/// is a key given twice, which would leave the bundlers' reader (`readLock` in
/// src/embedded-module.ts) and this one to pick different values.
struct EntryFile(Entry);

impl<'de> Deserialize<'de> for EntryFile {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = EntryFile;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a migration's name and its checksum")
            }
            fn visit_str<E: serde::de::Error>(self, line: &str) -> Result<EntryFile, E> {
                let (name, sum) = line.trim().split_once(' ').ok_or_else(|| E::custom(format!("\"{line}\" is a migration's name and its checksum")))?;
                Ok(EntryFile(Entry { name: name.trim().to_string(), sum: sum.trim().to_string() }))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<EntryFile, A::Error> {
                use serde::de::Error;
                let (mut name, mut sum) = (None, None);
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "name" if name.is_some() => return Err(A::Error::duplicate_field("name")),
                        "checksum" if sum.is_some() => return Err(A::Error::duplicate_field("checksum")),
                        "name" => name = Some(map.next_value::<String>()?),
                        "checksum" => sum = Some(map.next_value::<String>()?),
                        _ => return Err(A::Error::unknown_field(&key, &["name", "checksum"])),
                    }
                }
                Ok(EntryFile(Entry { name: name.ok_or_else(|| A::Error::missing_field("name"))?, sum: sum.ok_or_else(|| A::Error::missing_field("checksum"))? }))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

static MARKER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^(<<<<<<<|=======|>>>>>>>)( |$)").expect("a valid pattern"));

fn digest(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.part(p.as_bytes());
    }
    hex(&h.finish()[..4])
}

/// The checksum of a migration after the entry `prev` (empty for the first): of its name and its
/// SQL as `canonical` writes it, so that comments and spacing change nothing.
pub fn checksum(prev: &str, name: &str, sql: &str) -> String {
    digest(&[prev, "\n", name, "\n", &canonical(sql)])
}

/// The checksum of schema.sql's SQL, as `canonical` writes it with its `\r\n` line endings as
/// `\n`, so that a checkout that writes them matches, with the last entry's checksum the
/// migrations it was proven against end with: a stamp that no longer holds once they change.
pub fn schema_sum(last: &str, sql: &str) -> String {
    digest(&["schema\n", last, "\n", &canonical(&sql.replace("\r\n", "\n"))])
}

/// SQL without its comments, its whitespace runs as one space, as any of the three databases reads
/// it: what a lock's checksum covers, so an edit to a comment or to spacing is no edit. Spacing is
/// ASCII's alone, as `sql.rs` reads it: the databases read a U+3000 or a U+00A0 outside quotes and
/// comments as part of a name, so it is kept as written. Quoted text
/// (`'…'`, `"…"`, `` `…` ``, and Postgres's `$tag$…$tag$`) and MySQL's `/*! … */` are kept as
/// written, and so is everything after a string, a comment, a `--` or a `#` that the databases
/// read differently, or a comment that never ends, so that text one of them runs is never dropped
/// as a comment.
pub fn canonical(sql: &str) -> String {
    let b: Vec<char> = sql.chars().collect();
    let mut out = String::new();
    let mut space = false;
    let mut k = 0;
    let push = |out: &mut String, space: &mut bool, text: &str| {
        if *space && !out.is_empty() {
            out.push(' ');
        }
        *space = false;
        out.push_str(text);
    };
    while k < b.len() {
        let c = b[k];
        if c == '-' && b.get(k + 1) == Some(&'-') {
            // MySQL reads `--` as a comment only where a space or a control character follows it
            // (`n--1` is `n - -1` there, and so is `--` before a U+3000), Postgres and SQLite
            // everywhere: where the two differ, the rest is kept as written. The statements
            // `sql.rs` splits for MySQL end a comment's `--` at the same characters, up to the space.
            if b.get(k + 2).is_some_and(|n| *n > ' ') {
                push(&mut out, &mut space, &b[k..].iter().collect::<String>());
                k = b.len();
                continue;
            }
            // Postgres ends one at a lone carriage return too, MySQL and SQLite only at a line feed.
            let end = (k..b.len()).find(|&j| b[j] == '\n').unwrap_or(b.len());
            if (k..end).any(|j| b[j] == '\r' && b.get(j + 1) != Some(&'\n')) {
                push(&mut out, &mut space, &b[k..].iter().collect::<String>());
                k = b.len();
                continue;
            }
            k = end;
            space = true;
        } else if c == '#' {
            // MySQL reads `#` as a line comment, Postgres as an operator (`#`, `#>`, `#>>`): the rest
            // is kept as written, so that a `/*` or a quote inside a MySQL comment opens nothing.
            push(&mut out, &mut space, &b[k..].iter().collect::<String>());
            k = b.len();
        } else if c == '/' && b.get(k + 1) == Some(&'*') {
            // Postgres nests block comments, while MySQL and SQLite end one at its first `*/`: where
            // the two end it at different places, the rest is kept as written. MySQL runs the text
            // of `/*! … */` (as `SHOW CREATE TABLE` writes `/*!50100 PARTITION BY … */`), so that
            // comment is kept as written too, and so is the rest after a comment that never ends.
            let flat = (k + 2..b.len().saturating_sub(1)).find(|&j| b[j] == '*' && b[j + 1] == '/').map(|j| j + 2);
            let mut depth = 0;
            let mut nested = k;
            while nested < b.len() {
                if b[nested] == '/' && b.get(nested + 1) == Some(&'*') {
                    depth += 1;
                    nested += 2;
                } else if b[nested] == '*' && b.get(nested + 1) == Some(&'/') {
                    depth -= 1;
                    nested += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    nested += 1;
                }
            }
            match flat.filter(|&end| end == nested) {
                None => {
                    push(&mut out, &mut space, &b[k..].iter().collect::<String>());
                    k = b.len();
                }
                Some(end) if b.get(k + 2) == Some(&'!') => {
                    push(&mut out, &mut space, &b[k..end].iter().collect::<String>());
                    k = end;
                }
                Some(end) => {
                    k = end;
                    space = true;
                }
            }
        } else if c.is_ascii_whitespace() {
            space = true;
            k += 1;
        } else if c == '\'' || c == '"' || c == '`' {
            // MySQL reads a backslash in a string as escaping the next character, and so does
            // Postgres in an `E'…'` string, while a standard string or an identifier ends at its
            // first lone quote. Where the two readings end a string at different places (a `\'`
            // inside it), what follows could be a comment in one and string text in the other, so
            // the rest is kept as written: an edit anywhere after it is an edit.
            let standard = quoted_end(&b, k, c, false);
            let end = if c == '`' || quoted_end(&b, k, c, true) == standard { standard } else { b.len() };
            push(&mut out, &mut space, &b[k..end].iter().collect::<String>());
            k = end;
        } else if c == '$' && (k == 0 || !(tag_char(b[k - 1]) || b[k - 1] == '$')) {
            // A dollar-quoted string: $tag$ … $tag$. Postgres takes any character past ASCII in a
            // tag or a name, and a `$` inside a name, as `x$$a$`, opens none.
            let tag_end = (k + 1..b.len()).find(|&j| !tag_char(b[j]));
            match tag_end.filter(|&j| b[j] == '$') {
                Some(j) => {
                    let tag: String = b[k..=j].iter().collect();
                    let body_from = j + 1;
                    let rest: String = b[body_from..].iter().collect();
                    let end = rest.find(&tag).map_or(b.len(), |at| body_from + rest[..at].chars().count() + tag.chars().count());
                    push(&mut out, &mut space, &b[k..end].iter().collect::<String>());
                    k = end;
                }
                None => {
                    push(&mut out, &mut space, "$");
                    k += 1;
                }
            }
        } else {
            push(&mut out, &mut space, &c.to_string());
            k += 1;
        }
    }
    out
}

/// A character of a dollar quote's tag, as Postgres reads one.
fn tag_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || !c.is_ascii()
}

/// Where the string or identifier quoted by `q` at `start` ends: after its closing quote (a doubled
/// quote is one quote of its text), and, with `backslash`, a backslash escaping the next character.
fn quoted_end(b: &[char], start: usize, q: char, backslash: bool) -> usize {
    let mut k = start + 1;
    while k < b.len() {
        if backslash && b[k] == '\\' {
            k += 2;
        } else if b[k] == q {
            if b.get(k + 1) == Some(&q) {
                k += 2;
                continue;
            }
            return k + 1;
        } else {
            k += 1;
        }
    }
    b.len()
}

/// The entries of migrations in the order given, each its name and SQL.
pub fn chain<'a>(migrations: impl IntoIterator<Item = (&'a str, &'a str)>) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    for (name, canonical) in migrations {
        let prev = out.last().map(|e| e.sum.clone()).unwrap_or_default();
        out.push(Entry { name: name.to_string(), sum: checksum(&prev, name, canonical) });
    }
    out
}

fn parse(text: &str) -> Result<Lock, String> {
    let file: LockFile = serde_yaml::from_str(text).map_err(|e| e.to_string())?;
    Ok(Lock { schema: file.schema, entries: file.migrations.into_iter().map(|e| e.0).collect() })
}

/// The lock at `path`. Only a lock that isn't there is missing: one that is there and can't be read,
/// read as none, would turn its checksums off and deploy its drafts.
pub fn read(path: &Path) -> State {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::Missing,
        Err(e) => State::Unreadable(e.to_string()),
        Ok(text) => of_text(&text),
    }
}

/// A lock's text, as read from a file or from git.
pub fn of_text(text: &str) -> State {
    if MARKER.is_match(text) {
        return State::Conflicted;
    }
    match serde_yaml::from_str::<Versioned>(text).map(|v| v.version) {
        Err(e) => return State::Invalid(e.to_string()),
        Ok(Some(v)) if !VERSION_TEXT.is_match(&v) => return State::Invalid(format!("`version: {v}` isn't a lock's version")),
        Ok(Some(v)) if v.parse::<u64>().is_ok_and(|v| v > VERSION) => return State::Newer(v.parse().unwrap_or_default()),
        Ok(_) => {}
    }
    match parse(text) {
        Ok(lock) => State::Lock(lock),
        Err(e) => State::Invalid(e),
    }
}

pub fn render(lock: &Lock) -> String {
    let mut out = format!("{HEADER}version: {VERSION}\n");
    if let Some(schema) = &lock.schema {
        out.push_str(&format!("schema: {}\n", scalar(schema)));
    }
    out.push_str("migrations:\n");
    for e in &lock.entries {
        out.push_str(&format!("  - name: {}\n    checksum: {}\n", scalar(&e.name), scalar(&e.sum)));
    }
    out
}

/// A checksum some YAML reader would take for a number: digits alone (an integer, or in YAML 1.1 an
/// octal one), digits around an `e` (a float in YAML 1.2, however large), or `0b` and binary digits
/// (an integer in YAML 1.1).
static NUMERIC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:[0-9]+(?:[eE][0-9]+)?|0b[01]+)$").expect("a valid pattern"));

/// A value as YAML writes it, quoted where a YAML reader would take it for something other than its
/// text.
fn scalar(value: &str) -> String {
    if NUMERIC.is_match(value) {
        return format!("'{value}'");
    }
    serde_yaml::to_string(value).map_or_else(|_| value.to_string(), |text| text.trim_end().to_string())
}

pub fn write(path: &Path, lock: &Lock) -> std::io::Result<()> {
    std::fs::write(path, render(lock))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_reads_back_as_written() {
        let entries = chain([("20261101000000_a.sql", "create table a (id int)"), ("20261102000000_b.sql", "create table b (id int)")]);
        let lock = Lock { schema: Some(schema_sum(&entries[1].sum, "create table a (id int)")), entries };
        let text = render(&lock);
        let dir = std::env::temp_dir().join(format!("reylo-lock-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write(&dir.join(LOCK), &lock).unwrap();
        assert_eq!(read(&dir.join(LOCK)), State::Lock(lock.clone()));
        assert!(text.contains("\n  - name: 20261101000000_a.sql\n    checksum: "), "{text}");
        assert_eq!(lock.entries[1].sum, checksum(&lock.entries[0].sum, "20261102000000_b.sql", "create table b (id int)"));
        std::fs::write(dir.join(LOCK), format!("{text}<<<<<<< HEAD\n")).unwrap();
        assert_eq!(read(&dir.join(LOCK)), State::Conflicted);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_lock_written_up_to_0_8_2_reads_as_its_entries() {
        let objects = "schema: c7bf681b\nmigrations:\n  - name: 20261104120000_users.sql\n    checksum: c86e9de4\n  - name: 20261104140347_add_bio.sql\n    checksum: 78ad08c0\n";
        let strings = "schema: c7bf681b\nmigrations:\n  - 20261104120000_users.sql c86e9de4\n  - 20261104140347_add_bio.sql 78ad08c0\n";
        let State::Lock(lock) = of_text(objects) else { panic!("{objects}") };
        assert_eq!(lock.entries[1], Entry { name: "20261104140347_add_bio.sql".into(), sum: "78ad08c0".into() });
        assert_eq!(of_text(strings), State::Lock(lock.clone()));
        assert_eq!(render(&lock), format!("{HEADER}version: 1\n{objects}"));
        assert_eq!(of_text(&format!("version: 1\n{objects}")), State::Lock(lock.clone()));
        // Its keys in either order, and as a flow mapping.
        let swapped = objects.replace("  - name: 20261104140347_add_bio.sql\n    checksum: 78ad08c0\n", "  - checksum: 78ad08c0\n    name: 20261104140347_add_bio.sql\n");
        assert_eq!(of_text(&swapped), State::Lock(lock.clone()));
        assert_eq!(of_text(&objects.replace("  - name: 20261104120000_users.sql\n    checksum: c86e9de4\n", "  - { name: 20261104120000_users.sql, checksum: c86e9de4 }\n")), State::Lock(lock.clone()));
    }

    #[test]
    fn a_checksum_yaml_would_read_as_a_number_is_quoted_and_read_as_written() {
        let lock = Lock {
            schema: Some("01234567".into()),
            entries: vec![
                Entry { name: "init.sql".into(), sum: "00123456".into() },
                Entry { name: "20261101000000_a.sql".into(), sum: "1e234567".into() },
                Entry { name: "20261102000000_b.sql".into(), sum: "0b101101".into() },
                Entry { name: "20261103000000_c.sql".into(), sum: "1e2345ab".into() },
            ],
        };
        let text = render(&lock);
        for line in ["schema: '01234567'\n", "checksum: '00123456'\n", "checksum: '1e234567'\n", "checksum: '0b101101'\n", "checksum: 1e2345ab\n"] {
            assert!(text.contains(line), "{line}{text}");
        }
        assert_eq!(of_text(&text), State::Lock(lock.clone()));
        // Unquoted, as by hand, it still reads as its text.
        assert_eq!(of_text(&text.replace('\'', "")), State::Lock(lock));
    }

    #[test]
    fn an_entry_needs_its_name_and_checksum_and_no_other_key() {
        for (text, why) in [
            ("migrations:\n  - name: a.sql\n", "missing field `checksum`"),
            ("migrations:\n  - checksum: c86e9de4\n", "missing field `name`"),
            ("migrations:\n  - name: a.sql\n    checksum: c86e9de4\n    transaction: false\n", "unknown field `transaction`"),
            ("migrations:\n  - a.sql\n", "\"a.sql\" is a migration's name and its checksum"),
            ("migrations:\n  - name: a.sql\n    checksum: 00000000\n    checksum: c86e9de4\n", "duplicate field `checksum`"),
        ] {
            assert!(matches!(of_text(text), State::Invalid(e) if e.contains(why)), "{text}: {:?}", of_text(text));
        }
    }

    #[test]
    fn a_lock_a_newer_reylo_wrote_is_refused_by_its_version() {
        assert_eq!(of_text("version: 2\nmigrations:\n  - name: a.sql\n    checksum: c86e9de4\n    transaction: false\n"), State::Newer(2));
        for version in ["0", "01", "+1", "0x2", "1.0", "one"] {
            let text = format!("version: {version}\nmigrations:\n");
            assert!(matches!(of_text(&text), State::Invalid(ref e) if e.contains(&format!("`version: {version}` isn't a lock's version"))), "{text}: {:?}", of_text(&text));
        }
        for text in ["version: '1'\nmigrations:\n", "version:\nmigrations:\n", "version: NULL\nmigrations:\n", "version: ~ # none\nmigrations:\n", "migrations:\n"] {
            assert_eq!(of_text(text), State::Lock(Lock::default()), "{text}");
        }
        assert!(matches!(of_text("version: 'null'\nmigrations:\n"), State::Invalid(e) if e.contains("`version: null` isn't a lock's version")));
        assert_eq!(of_text("version: \"2\" # upgraded\nmigrations:\n"), State::Newer(2));
    }

    #[test]
    fn only_a_lock_that_isnt_there_is_missing() {
        let dir = std::env::temp_dir().join(format!("reylo-lock-unreadable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read(&dir.join(LOCK)), State::Missing);
        std::fs::write(dir.join(LOCK), b"migrations:\n  - name: a.sql\n    checksum: \xff\n").unwrap();
        assert!(matches!(read(&dir.join(LOCK)), State::Unreadable(why) if why.contains("UTF-8")));
        std::fs::remove_file(dir.join(LOCK)).unwrap();
        std::fs::create_dir(dir.join(LOCK)).unwrap();
        assert!(matches!(read(&dir.join(LOCK)), State::Unreadable(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn comments_and_spacing_are_no_edit() {
        let a = "create table a (\n  id int -- the key\n);\n/* made by hand */";
        assert_eq!(canonical(a), canonical("create table a ( id int );"));
        assert_ne!(canonical("insert into a values ('-- not a comment')"), canonical("insert into a values ('')"));
        assert_eq!(canonical("select $$ two  spaces $$"), "select $$ two  spaces $$");
        assert_eq!(canonical("do $f$ begin -- x\n end $f$;"), "do $f$ begin -- x\n end $f$;");
        // MySQL's `\'` is a quote inside the string, so `-- b` is string text there, not a comment.
        assert_ne!(canonical("insert into t values ('a\\' -- b')"), canonical("insert into t values ('a\\' -- c')"));
        assert_ne!(canonical("select E'a\\', '-- b'"), canonical("select E'a\\', '-- c'"));
        assert_ne!(canonical("insert into t values (\"a\\\" -- b\")"), canonical("insert into t values (\"a\\\" -- c\")"));
        // MySQL runs `/*! … */`, and reads `n--1` as `n - -1`.
        assert_ne!(canonical("create table t (a int) /*!50100 PARTITION BY HASH (a) */"), canonical("create table t (a int) /*!50100 PARTITION BY KEY (a) */"));
        assert_ne!(canonical("select 1--1"), canonical("select 1--2"));
        assert_eq!(canonical("select 1 --\tx\n, 2 --\n"), "select 1 , 2");
        // A comment Postgres nests and MySQL ends at its first `*/`.
        assert_ne!(canonical("/* a /* b */ create table t (a int); /* */"), canonical("/* a /* b */ create table t (a bigint); /* */"));
        assert_eq!(canonical("select /* a /* b */ c */  1"), "select /* a /* b */ c */  1");
        assert_eq!(canonical("select /* a */  1"), "select 1");
        // MySQL's `#` comments, and a comment that never ends.
        assert_ne!(canonical("# see reylo/*.sql\ncreate table t (a int);"), canonical("# see reylo/*.sql\ncreate table t (a bigint);"));
        assert_ne!(canonical("select 1; /* create table t (a int);"), canonical("select 1; /* create table t (a bigint);"));
        assert_eq!(canonical("select 1 /* a */"), "select 1");
        // A backslash both readings agree on changes nothing.
        assert_eq!(canonical("select 'a\\n' -- x\n,  'b\\\\'"), "select 'a\\n' , 'b\\\\'");
        assert_ne!(checksum("", "a.sql", "create table a (id int)"), checksum("", "a.sql", "create table a (id bigint)"));
    }

    #[test]
    fn a_space_beyond_ascii_is_part_of_a_name() {
        // Postgres, MySQL and SQLite read U+3000 and U+00A0 outside quotes and comments as part of a
        // name, so turning one into a space renames a column: an edit.
        assert_ne!(canonical("create view v as select 1 as x\u{3000}, 2 as y;"), canonical("create view v as select 1 as x , 2 as y;"));
        assert_ne!(canonical("create table t (a\u{a0}int)"), canonical("create table t (a int)"));
        assert_eq!(canonical("select  'a\u{3000}b' -- x\n"), "select 'a\u{3000}b'");
        // MySQL reads `--` before one, or before DEL as the statements are split for it, as `- -`
        // and runs the rest of the line, a `;` and what follows it included, so the rest is kept.
        assert_ne!(canonical("select 1 --\u{3000}x"), canonical("select 1 --\u{3000}; drop table users;"));
        assert_ne!(canonical("select 1 --\u{a0}x"), canonical("select 1 --\u{a0}; drop table users;"));
        assert_ne!(canonical("select 1 --\u{7f}x"), canonical("select 1 --\u{7f}; drop table users;"));
        // A lone carriage return ends a `--` comment on Postgres alone, and is spacing elsewhere.
        assert_ne!(canonical("-- note\rdrop table users;"), canonical("-- note\ndrop table users;"));
        assert_eq!(canonical("select 1 -- a\r\n,\r2"), "select 1 , 2");
        // Postgres takes any character past ASCII in a dollar quote's tag, and none after a name's `$`.
        assert_ne!(canonical("select $\u{3000}$--\nx$\u{3000}$"), canonical("select $\u{3000}$ x$\u{3000}$"));
        assert_ne!(canonical("select x$$a$, '$a$ -- b'"), canonical("select x$$a$, '$a$ -- c'"));
    }

    #[test]
    fn a_schema_checked_out_with_crlf_keeps_its_stamp() {
        let lf = "create function f() returns int as $$\n  select 1\n$$ language sql;\n# made by hand\n";
        assert_eq!(schema_sum("a", &lf.replace('\n', "\r\n")), schema_sum("a", lf));
    }

    #[test]
    fn the_chain_depends_on_order_and_text() {
        let ab = chain([("a.sql", "x"), ("b.sql", "y")]);
        let ba = chain([("b.sql", "y"), ("a.sql", "x")]);
        assert_ne!(ab[1].sum, ba[0].sum);
        assert_ne!(chain([("a.sql", "x2")])[0].sum, ab[0].sum);
    }
}

//! The SQLite diff engine's cases, `tests/diff/sqlite/<case>/`, as `tests/diff.rs` runs Postgres's:
//! `v1.sql` with `seed.sql`'s rows in one database, `v2.sql` in another, the plan from the first to
//! the second run on the first as `reylo migrate` runs a file (foreign keys off, one transaction,
//! `pragma foreign_key_check` before the commit), the first's catalog then required to equal the
//! second's and every table to keep its rows and their values, `check.sql`, where a case has one, to
//! run after it without an error (a full-text index's integrity check), a second plan to be empty, and the
//! plan compared with `plan.sql`, which `REYLO_BLESS=1` writes. Then the same database goes back to v1,
//! with its rows, as a rewind takes it, and up to v2 again, unless `one-way.txt` says why it can't: each
//! leg must reach the other side's catalog and keep its rows, counters and values as the first plan must;
//! where the way back can't hold those rows, `back-fails.txt` names the error it must fail with, and the
//! round trip runs without them. Every SQLite schema the oracle's cases set up is planned from an empty
//! database and back to one. SQLite needs no server: every database is in memory.

use reylo_migrate::catalog::{catalog, difference};
use reylo_migrate::db::Db;
use reylo_migrate::diff::{self, Plan};
use std::path::{Path, PathBuf};

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn only(mut cases: Vec<PathBuf>) -> Vec<PathBuf> {
    cases.sort();
    if let Ok(only) = std::env::var("REYLO_CASE") {
        cases.retain(|p| p.file_name().is_some_and(|n| n.to_string_lossy().contains(&only)));
    }
    cases
}

async fn memory() -> Db {
    Db::connect(":memory:", &std::env::temp_dir()).await.unwrap()
}

/// An empty database in memory on libSQL's engine, for a schema of libSQL's own SQL.
fn libsql_memory() -> Db {
    Db::LibsqlLocal(reylo_migrate::libsql_local::Local::memory().unwrap())
}

async fn plan(from: &mut Db, to: &mut Db) -> Plan {
    diff::plan(&diff::sqlite::read(from).await.unwrap(), &diff::sqlite::read(to).await.unwrap(), &diff::sqlite::SqliteAlterer::default())
}

/// Runs the plan as `reylo migrate` runs a file: the statement that failed, if one did.
async fn run(db: &mut Db, plan: &Plan) -> Option<String> {
    db.exec("pragma foreign_keys = off").await.unwrap();
    db.exec("begin").await.unwrap();
    let mut failed = None;
    for s in plan.runnable() {
        if let Err(e) = db.exec(s).await {
            failed = Some(format!("{s}: {e}"));
            break;
        }
    }
    if failed.is_none() {
        let broken = db.rows("pragma foreign_key_check").await.unwrap();
        if !broken.is_empty() {
            failed = Some(format!("{} rows break a foreign key after the plan", broken.len()));
        }
    }
    db.exec(if failed.is_some() { "rollback" } else { "commit" }).await.unwrap();
    db.exec("pragma foreign_keys = on").await.unwrap();
    failed
}

async fn counts(db: &mut Db) -> Vec<(String, String)> {
    let tables = db.rows("select name from pragma_table_list where schema = 'main' and type in ('table', 'virtual') and name not like 'sqlite_%' order by name").await.unwrap();
    let mut out = Vec::new();
    for t in tables {
        let name = t.text("name").to_string();
        // A full-text index that names a column its content table lacks fails every query.
        let n = match db.rows(&format!("select count(*) as n from \"{}\"", name.replace('"', "\"\""))).await {
            Ok(n) => n[0].text("n").to_string(),
            Err(e) => format!("unreadable: {e}"),
        };
        out.push((name.to_lowercase(), n));
    }
    out
}

/// Each table's rows, as text, column by column, sorted: `(table, column names, rows)`.
async fn values(db: &mut Db) -> Vec<(String, Vec<String>, Vec<Vec<String>>)> {
    let tables = db.rows("select name from pragma_table_list where schema = 'main' and type = 'table' and name not like 'sqlite_%' order by name").await.unwrap();
    let mut out = Vec::new();
    for t in tables {
        let name = t.text("name").to_string();
        let q = format!("\"{}\"", name.replace('"', "\"\""));
        let columns: Vec<String> = db.rows(&format!("select name from pragma_table_xinfo('{}') where hidden = 0 order by cid", name.replace('\'', "''"))).await.unwrap().iter().map(|r| r.text("name").to_string()).collect();
        if columns.is_empty() {
            continue;
        }
        // A number reads as itself whatever its storage class (1, 1.0, '1'), as a column's change of
        // affinity converts it; text that isn't one, and a blob, read as written.
        let cell = |c: &str| {
            let q = format!("\"{}\"", c.replace('"', "\"\""));
            format!("case typeof({q}) when 'null' then 'NULL' when 'blob' then 'x' || hex({q}) when 'text' then {q} else cast(case when {q} = cast({q} as integer) then cast({q} as integer) else {q} end as text) end")
        };
        let list = columns.iter().enumerate().map(|(k, c)| format!("{} as v{k}", cell(c))).collect::<Vec<_>>().join(", ");
        let mut rows: Vec<Vec<String>> = db.rows(&format!("select {list} from {q}")).await.unwrap().iter().map(|r| (0..columns.len()).map(|k| r.text(&format!("v{k}")).to_string()).collect()).collect();
        rows.sort();
        out.push((name.to_lowercase(), columns.iter().map(|c| c.to_lowercase()).collect(), rows));
    }
    out
}

/// The values of the columns a table keeps that a plan changed: before and after, each read in the
/// columns both have.
fn changed(before: &[(String, Vec<String>, Vec<Vec<String>>)], after: &[(String, Vec<String>, Vec<Vec<String>>)]) -> Vec<String> {
    let mut out = Vec::new();
    for (table, columns, rows) in before {
        let Some((_, now_columns, now_rows)) = after.iter().find(|(t, ..)| t == table) else { continue };
        let both: Vec<(usize, usize)> = columns.iter().enumerate().filter_map(|(k, c)| now_columns.iter().position(|n| n == c).map(|j| (k, j))).collect();
        let pick = |rows: &[Vec<String>], side: usize| -> Vec<Vec<String>> {
            let mut out: Vec<Vec<String>> = rows.iter().map(|r| both.iter().map(|(k, j)| r[if side == 0 { *k } else { *j }].clone()).collect()).collect();
            out.sort();
            out
        };
        if pick(rows, 0) != pick(now_rows, 1) {
            out.push(table.clone());
        }
    }
    out
}

/// Each AUTOINCREMENT table's counter, which can be past every key its table holds.
async fn counters(db: &mut Db) -> Vec<(String, i64)> {
    let exists = db.rows("select count(*) as n from sqlite_schema where name = 'sqlite_sequence'").await.unwrap();
    if exists[0].text("n") == "0" {
        return vec![];
    }
    db.rows("select lower(name) as name, seq from sqlite_sequence order by 1").await.unwrap().iter().map(|r| (r.text("name").to_string(), r.text("seq").parse().unwrap())).collect()
}

/// Deletes every table's rows.
async fn empty(db: &mut Db) {
    let tables = db.rows("select name from pragma_table_list where schema = 'main' and type = 'table' and name not like 'sqlite_%'").await.unwrap();
    db.exec("pragma foreign_keys = off").await.unwrap();
    for t in tables {
        db.exec(&format!("delete from \"{}\"", t.text("name").replace('"', "\"\""))).await.unwrap();
    }
    db.exec("pragma foreign_keys = on").await.unwrap();
}

fn differs(only_a: &[String], only_b: &[String]) -> Option<String> {
    (!only_a.is_empty() || !only_b.is_empty()).then(|| only_a.iter().map(|l| format!("  - {l}")).chain(only_b.iter().map(|l| format!("  + {l}"))).collect::<Vec<_>>().join("\n"))
}

/// What `plan` did to the rows `db` held before it, which `before`, `rows_before` and `sequences` read:
/// each table whose row count changed, but one the plan says it loses (a contentless full-text index
/// made again), each AUTOINCREMENT counter that went back or away from a table that keeps
/// AUTOINCREMENT, and where the plan ran (`ran`), each table whose values changed in the columns it
/// kept, but one the case (`changes`) says the change rewrites.
async fn row_problems(db: &mut Db, plan: &Plan, before: &[(String, String)], rows_before: &[(String, Vec<String>, Vec<Vec<String>>)], sequences: &[(String, i64)], ran: bool, changes: &[String]) -> Vec<String> {
    let after = counts(db).await;
    let lossy: Vec<String> = plan.losses.iter().filter_map(|l| l.strip_prefix("table ")).map(|t| t.to_lowercase()).collect();
    let mut lost: Vec<String> = before.iter().filter(|(t, n)| !lossy.contains(t) && after.iter().any(|(u, m)| u == t && m != n)).map(|(t, n)| format!("{t}: {n} rows before")).collect();
    // A table that keeps AUTOINCREMENT keeps its counter: no key it handed out is handed out again.
    let kept = counters(db).await;
    let autoincrement = db.rows("select lower(name) as name from sqlite_schema where type = 'table' and sql like '%autoincrement%'").await.unwrap();
    for (t, seq) in sequences {
        let now = kept.iter().find(|(u, _)| u == t).map(|(_, s)| *s);
        if now.is_some_and(|n| n < *seq) || (now.is_none() && autoincrement.iter().any(|r| r.text("name") == t)) {
            lost.push(format!("{t}'s counter: {seq} before, {now:?} after"));
        }
    }
    let mut out = Vec::new();
    if !lost.is_empty() {
        out.push(format!("rows lost: {}", lost.join(", ")));
    }
    // Every column a table keeps keeps its values, where the case doesn't say the change rewrites them.
    let changed: Vec<String> = if ran { changed(rows_before, &values(db).await).into_iter().filter(|t| !changes.contains(t)).collect() } else { vec![] };
    if !changed.is_empty() {
        out.push(format!("values changed in: {}", changed.join(", ")));
    }
    out
}

/// Takes `db`, with its rows, to the schema `like` has, running the plan as `reylo migrate` runs a
/// file, and says how it fell short: the statement that failed, each line the two differ in, the rows,
/// counters and values it changed, and what a second plan would still run. Empty where it reached it.
async fn leg(db: &mut Db, like: &mut Db, changes: &[String]) -> Vec<String> {
    let before = counts(db).await;
    let rows_before = values(db).await;
    let sequences = counters(db).await;
    let plan = plan(db, like).await;
    let failed = run(db, &plan).await;
    let mut lines: Vec<String> = failed.iter().map(|f| format!("the plan failed at {f}")).collect();
    let (only_a, only_b) = difference(&catalog(db).await.unwrap(), &catalog(like).await.unwrap());
    lines.extend(only_a.iter().map(|l| format!("- {l}")).chain(only_b.iter().map(|l| format!("+ {l}"))));
    lines.extend(row_problems(db, &plan, &before, &rows_before, &sequences, failed.is_none(), changes).await);
    if failed.is_none() && only_a.is_empty() && only_b.is_empty() {
        let more = crate::plan(db, like).await;
        if more.runnable().next().is_some() {
            lines.push(format!("again:\n{}", more.sql()));
        }
    }
    if !lines.is_empty() {
        lines.push(format!("plan:\n{}", plan.sql()));
    }
    lines
}

#[tokio::test(flavor = "current_thread")]
async fn sqlite() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/diff/sqlite");
    let cases = only(std::fs::read_dir(root).unwrap().map(|e| e.unwrap().path()).filter(|p| p.is_dir()).collect());
    let bless = std::env::var("REYLO_BLESS").is_ok();
    let mut failures = Vec::new();
    for case in &cases {
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        let (mut from, mut to) = (memory().await, memory().await);
        from.exec(&read(&case.join("v1.sql"))).await.unwrap_or_else(|e| panic!("{name}: v1.sql: {e}"));
        from.exec(&read(&case.join("seed.sql"))).await.unwrap_or_else(|e| panic!("{name}: seed.sql: {e}"));
        to.exec(&read(&case.join("v2.sql"))).await.unwrap_or_else(|e| panic!("{name}: v2.sql: {e}"));
        let before = counts(&mut from).await;
        let rows_before = values(&mut from).await;
        let sequences = counters(&mut from).await;
        let plan = plan(&mut from, &mut to).await;
        let text = plan.sql();
        let failed = run(&mut from, &plan).await;
        let check = read(&case.join("check.sql"));
        let checked = if failed.is_none() && !check.is_empty() { from.exec(&check).await.err() } else { None };
        let (only_a, only_b) = difference(&catalog(&mut from).await.unwrap(), &catalog(&mut to).await.unwrap());
        let changes: Vec<String> = read(&case.join("changes.txt")).lines().filter_map(|l| l.split_whitespace().next()).map(|t| t.to_lowercase()).collect();
        let rows = row_problems(&mut from, &plan, &before, &rows_before, &sequences, failed.is_none(), &changes).await;
        let expected = case.join("plan.sql");
        if bless {
            std::fs::write(&expected, &text).unwrap();
        }
        let mut problems = Vec::new();
        if let Some(f) = &failed {
            problems.push(format!("the plan failed at {f}"));
        }
        if let Some(d) = differs(&only_a, &only_b) {
            problems.push(format!("after the plan the database differs from v2:\n{d}"));
        }
        problems.extend(rows);
        if let Some(e) = checked {
            problems.push(format!("check.sql failed after the plan: {e}"));
        }
        if !bless && read(&expected) != text {
            problems.push(format!("the plan is not plan.sql's (REYLO_BLESS=1 writes it):\n{text}"));
        }
        // Planned again, a database the plan took to the target needs nothing more.
        let reached = failed.is_none() && differs(&only_a, &only_b).is_none();
        if reached {
            let more = crate::plan(&mut from, &mut to).await;
            if more.runnable().next().is_some() {
                problems.push(format!("after the plan, the plan again is not empty:\n{}", more.sql()));
            }
        }
        // Then back to v1 with the rows the plan kept, as a rewind takes a database back, and up to
        // v2 again, unless the case says why it goes one way only.
        if reached && read(&case.join("one-way.txt")).trim().is_empty() {
            let mut first = memory().await;
            first.exec(&read(&case.join("v1.sql"))).await.unwrap_or_else(|e| panic!("{name}: v1.sql: {e}"));
            // A case whose way back can't hold the rows the plan left (a not-null column made again
            // with no value for them) names the error in `back-fails.txt`: the way back must fail
            // with it, which rolls it back, and the round trip then runs without the rows.
            let back_fails = read(&case.join("back-fails.txt")).trim().to_string();
            let mut ready = true;
            if !back_fails.is_empty() {
                let reverse = crate::plan(&mut from, &mut first).await;
                let refused = match run(&mut from, &reverse).await {
                    Some(f) if f.contains(&back_fails) => None,
                    Some(f) => Some(format!("the way back failed otherwise than back-fails.txt says: {f}")),
                    None => Some(format!("the way back ran, where back-fails.txt says it fails with {back_fails}")),
                };
                ready = refused.is_none();
                problems.extend(refused);
                empty(&mut from).await;
            }
            let indent = |lines: Vec<String>| lines.iter().map(|l| format!("  {}", l.replace('\n', "\n  "))).collect::<Vec<_>>().join("\n");
            let back = if ready { leg(&mut from, &mut first, &changes).await } else { vec![] };
            if !back.is_empty() {
                problems.push(format!("back from v2 to v1:\n{}", indent(back)));
            } else if ready {
                let up = leg(&mut from, &mut to, &changes).await;
                if !up.is_empty() {
                    problems.push(format!("up from v1 to v2 again:\n{}", indent(up)));
                }
            }
        }
        if !problems.is_empty() {
            failures.push(format!("{name}:\n{}\nplan:\n{text}", problems.join("\n")));
        }
    }
    assert!(failures.is_empty(), "{} of {} cases failed:\n\n{}", failures.len(), cases.len(), failures.join("\n\n"));
}

/// Every SQLite schema the oracle's cases set up (`oracle/cases/<case>/setup.sql` where `dialect`
/// says `sqlite`), built from an empty database by its plan, and dropped back to an empty one by the
/// plan the other way: each direction must end with the other side's catalog. A case with a `libsql`
/// file is libSQL's own (a vector index), which only libSQL's engine makes, so it is built there.
#[tokio::test(flavor = "current_thread")]
async fn sqlite_oracle_schemas() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../oracle/cases");
    let cases = only(std::fs::read_dir(&root).unwrap().map(|e| e.unwrap().path()).filter(|p| p.join("setup.sql").exists() && read(&p.join("dialect")).trim() == "sqlite").collect());
    let (mut failures, mut skipped, mut passed) = (Vec::new(), Vec::new(), 0);
    for case in &cases {
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        let (mut empty, mut full, mut blank) = if case.join("libsql").exists() { (libsql_memory(), libsql_memory(), libsql_memory()) } else { (memory().await, memory().await, memory().await) };
        if let Err(err) = full.exec(&read(&case.join("setup.sql"))).await {
            skipped.push(format!("{name}: {err}"));
            continue;
        }
        let mut problems = Vec::new();
        for direction in ["from empty", "to empty"] {
            let (from, to) = if direction == "from empty" { (&mut empty, &mut full) } else { (&mut full, &mut blank) };
            let plan = plan(from, to).await;
            let failed = run(from, &plan).await;
            let (only_a, only_b) = difference(&catalog(from).await.unwrap(), &catalog(to).await.unwrap());
            if failed.is_some() || !only_a.is_empty() || !only_b.is_empty() {
                let mut lines: Vec<String> = failed.into_iter().map(|f| format!("  failed at {f}")).collect();
                lines.extend(only_a.iter().take(8).map(|l| format!("  - {l}")));
                lines.extend(only_b.iter().take(8).map(|l| format!("  + {l}")));
                problems.push(format!("{direction}:\n{}\nplan:\n{}", lines.join("\n"), plan.sql()));
            }
        }
        if problems.is_empty() {
            passed += 1;
        } else {
            failures.push(format!("{name}: {}", problems.join("\n")));
        }
    }
    eprintln!("{passed} schemas planned both ways; {} could not be set up here:\n{}", skipped.len(), skipped.join("\n"));
    assert!(skipped.is_empty(), "{} schemas could not be set up:\n{}", skipped.len(), skipped.join("\n"));
    assert!(failures.is_empty(), "{} of {} schemas failed:\n\n{}", failures.len(), cases.len(), failures.join("\n\n"));
}

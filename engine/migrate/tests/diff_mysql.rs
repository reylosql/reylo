//! The MySQL diff engine's cases, `tests/diff/mysql/<case>/`, as `tests/diff.rs` runs Postgres's:
//! `v1.sql` (with `seed.sql`'s rows) is the database as it is, `v2.sql` the schema it should have;
//! each case builds both, plans from one to the other, runs the plan on the first, and requires its
//! catalog to equal the second's, every table both have to keep its rows, and every column kept to
//! keep its values, unless the plan says it loses or narrows them or the case's `changes.txt` names
//! the table. A case's `sql_mode` file is the mode its sessions run in. A plan runs as Migrate runs a
//! migration's statements (`Db::script`). A case's `fails.txt` is the error its plan must fail with,
//! on a value a change can't hold: then every row must keep its values, and that is all it checks.
//! Its v1 and v2 must differ to
//! the comparator, unless `alike.txt` says why they read alike. `server.sql` makes
//! what a case needs of the server itself (an account), and `check.sql`, where a case has one,
//! runs after the plan, so a case can require what no catalog shows (an AUTO_INCREMENT counter
//! past the rows). The plan is kept beside the case as `plan.sql`: `REYLO_BLESS=1` writes it.
//! Then every MySQL schema the oracle's cases set up is built from an empty database by its plan
//! and dropped back to an empty one. The comparator leaves events out, so these tests compare
//! them themselves. After a plan has run, planning again must find nothing to do. Then the same
//! database goes back to v1, with its rows, as a rewind takes it, and up to v2 again, unless the
//! case's `one-way.txt` says why it can't: each leg must reach the other side's catalog and keep its
//! rows and values as the first plan must (those plans are not kept). Where the way back can't hold
//! those rows, `back-fails.txt` names the error it must fail with, and the round trip runs without them. Run against the server `REYLO_TEST_MYSQL` names, and skipped where it is not set.

use reylo_migrate::catalog::{catalog, difference};
use reylo_migrate::db::{with_database, Db};
use reylo_migrate::diff;
use std::path::{Path, PathBuf};

fn cases() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/diff/mysql");
    let mut out: Vec<PathBuf> = std::fs::read_dir(root).unwrap().map(|e| e.unwrap().path()).filter(|p| p.is_dir()).collect();
    out.sort();
    if let Ok(only) = std::env::var("REYLO_CASE") {
        out.retain(|p| p.file_name().is_some_and(|n| n.to_string_lossy().contains(&only)));
    }
    out
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Every table's row count.
async fn counts(db: &mut Db) -> Vec<(String, String)> {
    let tables = db.rows("select table_name as t from information_schema.tables where table_schema = database() and table_type = 'BASE TABLE' order by 1").await.unwrap();
    let mut out = Vec::new();
    for t in tables {
        let n = db.rows(&format!("select count(*) as n from `{}`", t.text("t"))).await.unwrap();
        out.push((t.text("t").to_string(), n[0].text("n").to_string()));
    }
    out
}

/// Each table's rows, as text, column by column (but generated ones, whose values are their
/// expression's): `(table, column names, rows)`.
async fn values(db: &mut Db) -> Vec<(String, Vec<String>, Vec<Vec<String>>)> {
    let tables = db.rows("select table_name as t from information_schema.tables where table_schema = database() and table_type = 'BASE TABLE' order by 1").await.unwrap();
    let mut out = Vec::new();
    for t in tables {
        let table = t.text("t").to_string();
        let columns: Vec<String> = db
            .rows(&format!("select column_name as c from information_schema.columns where table_schema = database() and table_name = '{}' and extra not like '%GENERATED%' order by ordinal_position", table.replace('\'', "''")))
            .await
            .unwrap()
            .iter()
            .map(|r| r.text("c").to_string())
            .collect();
        if columns.is_empty() {
            continue;
        }
        let list = columns.iter().enumerate().map(|(k, c)| format!("coalesce(hex(`{}`), 'NULL') as v{k}", c.replace('`', "``"))).collect::<Vec<_>>().join(", ");
        let mut rows: Vec<Vec<String>> = db.rows(&format!("select {list} from `{table}`")).await.unwrap().iter().map(|r| (0..columns.len()).map(|k| r.text(&format!("v{k}")).to_string()).collect()).collect();
        rows.sort();
        out.push((table.to_lowercase(), columns.iter().map(|c| c.to_lowercase()).collect(), rows));
    }
    out
}

/// The tables a plan changed the values of, in the columns they kept.
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

type Values = Vec<(String, Vec<String>, Vec<Vec<String>>)>;

/// What a plan did to the rows it was given: each table whose row count changed, but one whose
/// partition the plan drops, saying so; and where the plan ran (`rows_after` read), each whose values
/// changed in the columns it kept, but where the plan (as a loss or a narrowing) or the case
/// (`changes`, its tables) says it rewrites them.
fn row_problems(plan: &diff::Plan, before: &[(String, String)], after: &[(String, String)], rows_before: &Values, rows_after: Option<&Values>, changes: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let partitioned: Vec<String> = plan.losses.iter().filter_map(|l| l.strip_prefix("partition ")?.rsplit_once('.').map(|(t, _)| t.to_lowercase())).collect();
    let lost: Vec<String> = before.iter().filter(|(t, n)| !partitioned.contains(&t.to_lowercase()) && after.iter().any(|(u, m)| u == t && m != n)).map(|(t, n)| format!("{t}: {n} rows before")).collect();
    if !lost.is_empty() {
        out.push(format!("rows lost: {}", lost.join(", ")));
    }
    let declared = plan.losses.iter().chain(plan.narrowings.iter().map(|(c, _)| c)).filter_map(|id| id.strip_prefix("column ")?.rsplit_once('.').map(|(t, _)| t.trim_matches('`').to_lowercase()));
    let skip: Vec<String> = changes.iter().cloned().chain(declared).chain(partitioned.iter().cloned()).collect();
    let rewritten: Vec<String> = rows_after.map(|after| changed(rows_before, after).into_iter().filter(|t| !skip.contains(t)).collect()).unwrap_or_default();
    if !rewritten.is_empty() {
        out.push(format!("values changed in: {}", rewritten.join(", ")));
    }
    out
}

/// Takes `db`, with its rows, to the schema `like` has, running the plan as Migrate runs a
/// migration's statements, and says how it fell short: the statement that failed, each line the two
/// differ in, the rows and values it changed, and what a second plan would still run. Empty where it
/// reached it.
async fn leg(db: &mut Db, like: &mut Db, alterer: &dyn diff::Alterer, changes: &[String]) -> Vec<String> {
    let before = counts(db).await;
    let rows_before = values(db).await;
    let plan = diff::plan(&diff::mysql::read(db).await.unwrap(), &diff::mysql::read(like).await.unwrap(), alterer);
    let mut failed = None;
    for s in plan.runnable() {
        if let Err(e) = db.script(s).await {
            failed = Some(format!("{s}: {e}"));
            break;
        }
    }
    let mut lines: Vec<String> = failed.iter().map(|f| format!("the plan failed at {f}")).collect();
    let (only_a, only_b) = differences(db, like).await;
    lines.extend(only_a.iter().map(|l| format!("- {l}")).chain(only_b.iter().map(|l| format!("+ {l}"))));
    let rows_after = if failed.is_none() { Some(values(db).await) } else { None };
    lines.extend(row_problems(&plan, &before, &counts(db).await, &rows_before, rows_after.as_ref(), changes));
    if failed.is_none() && only_a.is_empty() && only_b.is_empty() {
        let again = diff::plan(&diff::mysql::read(db).await.unwrap(), &diff::mysql::read(like).await.unwrap(), alterer).sql();
        lines.extend(again.lines().map(|l| format!("again: {l}")));
    }
    if !lines.is_empty() {
        lines.push(format!("plan:\n{}", plan.sql().lines().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n")));
    }
    lines
}

/// The database's events, which the comparator does not read, with the database's own name taken
/// out and when each starts left out (the server fixes it as the event is made).
async fn events(db: &mut Db) -> Vec<String> {
    let name = db.rows("select database() as n").await.unwrap()[0].text("n").to_string();
    db.rows(
        "select concat_ws(' ', event_name, event_type, interval_value, interval_field, status, on_completion, definer, event_comment, event_definition) as line
        from information_schema.events where event_schema = database() order by event_name",
    )
    .await
    .unwrap()
    .iter()
    .map(|r| r.text("line").replace(&format!("`{name}`."), "").replace(&format!("{name}."), ""))
    .collect()
}

/// The lines the two databases differ in, the comparator's and the events'.
async fn differences(a: &mut Db, b: &mut Db) -> (Vec<String>, Vec<String>) {
    let (mut only_a, mut only_b) = difference(&catalog(a).await.unwrap(), &catalog(b).await.unwrap());
    let (ea, eb) = difference(&events(a).await, &events(b).await);
    only_a.extend(ea.into_iter().map(|l| format!("event {l}")));
    only_b.extend(eb.into_iter().map(|l| format!("event {l}")));
    (only_a, only_b)
}

/// Deletes every table's rows, keeping each AUTO_INCREMENT counter where it is.
async fn empty(db: &mut Db) {
    let tables = db.rows("select table_name as t from information_schema.tables where table_schema = database() and table_type = 'BASE TABLE'").await.unwrap();
    db.exec("set foreign_key_checks = 0").await.unwrap();
    for t in tables {
        db.exec(&format!("delete from `{}`", t.text("t").replace('`', "``"))).await.unwrap();
    }
    db.exec("set foreign_key_checks = 1").await.unwrap();
}

async fn recreate(admin: &mut Db, name: &str) {
    admin.exec(&format!("drop database if exists `{name}`")).await.unwrap();
    admin.exec(&format!("create database `{name}`")).await.unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn mysql() {
    let Ok(base) = std::env::var("REYLO_TEST_MYSQL") else {
        eprintln!("skipped: REYLO_TEST_MYSQL is not set");
        return;
    };
    let here = std::env::temp_dir();
    let mut admin = Db::connect(&base, &here).await.unwrap();
    let bless = std::env::var("REYLO_BLESS").is_ok();
    let mut failures = Vec::new();
    for (n, case) in cases().into_iter().enumerate() {
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        let (a, b) = (format!("diff_{}_{n}_a", std::process::id()), format!("diff_{}_{n}_b", std::process::id()));
        admin.script(&read(&case.join("server.sql"))).await.unwrap_or_else(|e| panic!("{name}: server.sql: {e}"));
        for db in [&a, &b] {
            recreate(&mut admin, db).await;
        }
        let mut from = Db::connect(&with_database(&base, &a), &here).await.unwrap();
        let mut to = Db::connect(&with_database(&base, &b), &here).await.unwrap();
        // A case's `sql_mode` file is the mode its sessions run in, which the alterer reads.
        let mode = read(&case.join("sql_mode")).trim().to_string();
        if !mode.is_empty() {
            for db in [&mut from, &mut to] {
                db.exec(&format!("set session sql_mode = '{mode}'")).await.unwrap();
            }
        }
        let alterer = diff::alterer(&mut from).await.unwrap();
        let alterer = alterer.as_ref();
        from.script(&read(&case.join("v1.sql"))).await.unwrap_or_else(|e| panic!("{name}: v1.sql: {e}"));
        from.script(&read(&case.join("seed.sql"))).await.unwrap_or_else(|e| panic!("{name}: seed.sql: {e}"));
        to.script(&read(&case.join("v2.sql"))).await.unwrap_or_else(|e| panic!("{name}: v2.sql: {e}"));
        // The two schemas differ to the comparator, or the case checks nothing it could miss; a
        // case alike on purpose says why in `alike.txt`.
        let (a_only, b_only) = differences(&mut from, &mut to).await;
        let vacuous = a_only.is_empty() && b_only.is_empty() && read(&case.join("alike.txt")).is_empty();
        let before = counts(&mut from).await;
        let rows_before = values(&mut from).await;
        let plan = diff::plan(&diff::mysql::read(&mut from).await.unwrap(), &diff::mysql::read(&mut to).await.unwrap(), alterer);
        let text = plan.sql();
        let mut failed = None;
        for s in plan.runnable() {
            if let Err(e) = from.script(s).await {
                failed = Some(format!("{s}: {e}"));
                break;
            }
        }
        let fails = read(&case.join("fails.txt")).trim().to_string();
        if !fails.is_empty() {
            let expected = case.join("plan.sql");
            if bless {
                std::fs::write(&expected, &text).unwrap();
            }
            let mut problems = Vec::new();
            match &failed {
                Some(f) if f.contains(&fails) => {}
                Some(f) => problems.push(format!("the plan failed otherwise than fails.txt says: {f}")),
                None => problems.push(format!("the plan ran, where fails.txt says it fails with {fails}")),
            }
            if values(&mut from).await != rows_before {
                problems.push("a failed plan changed the rows".to_string());
            }
            if !bless && read(&expected) != text {
                problems.push(format!("the plan is not plan.sql's (REYLO_BLESS=1 writes it):\n{text}"));
            }
            if !problems.is_empty() {
                failures.push(format!("{name}:\n{}\nplan:\n{text}", problems.join("\n")));
            }
            from.close().await;
            to.close().await;
            for db in [&a, &b] {
                admin.exec(&format!("drop database if exists `{db}`")).await.unwrap();
            }
            continue;
        }
        let (only_a, only_b) = differences(&mut from, &mut to).await;
        let again = diff::plan(&diff::mysql::read(&mut from).await.unwrap(), &diff::mysql::read(&mut to).await.unwrap(), alterer).sql();
        // Every table keeps its rows, and every column a table keeps its values, where neither the
        // plan (a partition or a column it says it loses, a narrowing) nor the case (`changes.txt`)
        // says the change rewrites them.
        let changes: Vec<String> = read(&case.join("changes.txt")).lines().filter_map(|l| l.split_whitespace().next()).map(|t| t.to_lowercase()).collect();
        let rows_after = if failed.is_none() { Some(values(&mut from).await) } else { None };
        let rows = row_problems(&plan, &before, &counts(&mut from).await, &rows_before, rows_after.as_ref(), &changes);
        let reached = failed.is_none() && only_a.is_empty() && only_b.is_empty();
        let check = read(&case.join("check.sql"));
        let checked = if failed.is_none() && !check.is_empty() { from.script(&check).await.err() } else { None };
        let expected = case.join("plan.sql");
        if bless {
            std::fs::write(&expected, &text).unwrap();
        }
        let mut problems = Vec::new();
        if vacuous {
            problems.push("v1 and v2 are alike to the comparator (a case alike on purpose says why in alike.txt)".to_string());
        }
        if let Some(f) = failed {
            problems.push(format!("the plan failed at {f}"));
        }
        if !only_a.is_empty() || !only_b.is_empty() {
            problems.push(format!("after the plan the database differs from v2:\n{}", only_a.iter().map(|l| format!("  - {l}")).chain(only_b.iter().map(|l| format!("  + {l}"))).collect::<Vec<_>>().join("\n")));
        }
        problems.extend(rows);
        if !again.is_empty() {
            problems.push(format!("after the plan, the plan again is not empty:\n{again}"));
        }
        if let Some(e) = checked {
            problems.push(format!("check.sql failed after the plan: {e}"));
        }
        if !bless && read(&expected) != text {
            problems.push(format!("the plan is not plan.sql's (REYLO_BLESS=1 writes it):\n{text}"));
        }
        // Then back to v1 with the rows the plan kept, as a rewind takes a database back, which
        // takes the opposite path through each change, and up to v2 again, unless the case says
        // why it goes one way only.
        if reached && read(&case.join("one-way.txt")).trim().is_empty() {
            let c = format!("diff_{}_{n}_c", std::process::id());
            recreate(&mut admin, &c).await;
            let mut first = Db::connect(&with_database(&base, &c), &here).await.unwrap();
            if !mode.is_empty() {
                first.exec(&format!("set session sql_mode = '{mode}'")).await.unwrap();
            }
            first.script(&read(&case.join("v1.sql"))).await.unwrap();
            // A case whose way back can't hold the rows the plan left (values it padded past a
            // narrower type) names the error in `back-fails.txt`: the way back must fail with it and
            // change nothing, and the round trip then runs without the rows.
            let back_fails = read(&case.join("back-fails.txt")).trim().to_string();
            let mut ready = true;
            if !back_fails.is_empty() {
                let reverse = diff::plan(&diff::mysql::read(&mut from).await.unwrap(), &diff::mysql::read(&mut first).await.unwrap(), alterer);
                let mut failed = None;
                for s in reverse.runnable() {
                    if let Err(e) = from.script(s).await {
                        failed = Some(format!("{s}: {e}"));
                        break;
                    }
                }
                let (only_a, only_b) = differences(&mut from, &mut to).await;
                let refused = match failed {
                    Some(f) if f.contains(&back_fails) => None,
                    Some(f) => Some(format!("the way back failed otherwise than back-fails.txt says: {f}")),
                    None => Some(format!("the way back ran, where back-fails.txt says it fails with {back_fails}")),
                };
                let changed = (!only_a.is_empty() || !only_b.is_empty()).then(|| "the way back that failed changed the database".to_string());
                ready = refused.is_none() && changed.is_none();
                problems.extend(refused.into_iter().chain(changed));
                empty(&mut from).await;
            }
            let indent = |lines: Vec<String>| lines.iter().map(|l| format!("  {}", l.replace('\n', "\n  "))).collect::<Vec<_>>().join("\n");
            let back = if ready { leg(&mut from, &mut first, alterer, &changes).await } else { vec![] };
            if !back.is_empty() {
                problems.push(format!("back from v2 to v1:\n{}", indent(back)));
            } else if ready {
                let up = leg(&mut from, &mut to, alterer, &changes).await;
                if !up.is_empty() {
                    problems.push(format!("up from v1 to v2 again:\n{}", indent(up)));
                }
            }
            first.close().await;
            admin.exec(&format!("drop database if exists `{c}`")).await.unwrap();
        }
        if !problems.is_empty() {
            failures.push(format!("{name}:\n{}\nplan:\n{text}", problems.join("\n")));
        }
        from.close().await;
        to.close().await;
        for db in [&a, &b] {
            admin.exec(&format!("drop database if exists `{db}`")).await.unwrap();
        }
    }
    admin.close().await;
    assert!(failures.is_empty(), "{} cases failed:\n\n{}", failures.len(), failures.join("\n\n"));
}

/// Every MySQL schema the oracle's cases set up (`oracle/cases/<case>/setup.sql`, where the case's
/// `dialect` file says `mysql`), built from an empty database by its plan, and dropped back to an
/// empty one by the plan the other way: each direction must end with the other side's catalog. A
/// case's `sql_mode` file is the mode of every session it runs in.
#[tokio::test(flavor = "current_thread")]
async fn mysql_oracle_schemas() {
    let Ok(base) = std::env::var("REYLO_TEST_MYSQL") else {
        eprintln!("skipped: REYLO_TEST_MYSQL is not set");
        return;
    };
    let here = std::env::temp_dir();
    let mut admin = Db::connect(&base, &here).await.unwrap();
    let alterer = diff::mysql::MysqlAlterer::default();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../oracle/cases");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&root).unwrap().map(|e| e.unwrap().path()).filter(|p| p.join("setup.sql").exists() && read(&p.join("dialect")).trim() == "mysql").collect();
    cases.sort();
    if let Ok(only) = std::env::var("REYLO_CASE") {
        cases.retain(|p| p.file_name().is_some_and(|n| n.to_string_lossy().contains(&only)));
    }
    let (mut failures, mut skipped, mut passed) = (Vec::new(), Vec::new(), 0);
    for (n, case) in cases.iter().enumerate() {
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        let (a, b, e) = (format!("odiff_{}_{n}_a", std::process::id()), format!("odiff_{}_{n}_b", std::process::id()), format!("odiff_{}_{n}_e", std::process::id()));
        for db in [&a, &b, &e] {
            recreate(&mut admin, db).await;
        }
        let mut empty = Db::connect(&with_database(&base, &a), &here).await.unwrap();
        let mut full = Db::connect(&with_database(&base, &b), &here).await.unwrap();
        let mut blank = Db::connect(&with_database(&base, &e), &here).await.unwrap();
        let mode = read(&case.join("sql_mode"));
        if !mode.trim().is_empty() {
            for db in [&mut empty, &mut full, &mut blank] {
                db.exec(&format!("set session sql_mode = '{}'", mode.trim())).await.unwrap();
            }
        }
        if let Err(err) = full.script(&read(&case.join("setup.sql"))).await {
            skipped.push(format!("{name}: {err}"));
        } else {
            let mut problems = Vec::new();
            for direction in ["from empty", "to empty"] {
                let (from, to) = if direction == "from empty" { (&mut empty, &mut full) } else { (&mut full, &mut blank) };
                let plan = diff::plan(&diff::mysql::read(from).await.unwrap(), &diff::mysql::read(to).await.unwrap(), &alterer);
                let mut failed = None;
                for s in plan.runnable() {
                    if let Err(err) = from.script(s).await {
                        failed = Some(format!("{s}: {err}"));
                        break;
                    }
                }
                let (only_a, only_b) = differences(from, to).await;
                let again = diff::plan(&diff::mysql::read(from).await.unwrap(), &diff::mysql::read(to).await.unwrap(), &alterer).sql();
                if failed.is_some() || !only_a.is_empty() || !only_b.is_empty() || !again.is_empty() {
                    let mut lines: Vec<String> = failed.into_iter().map(|f| format!("  failed at {f}")).collect();
                    lines.extend(again.lines().map(|l| format!("  again: {l}")));
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
        for db in [empty, full, blank] {
            db.close().await;
        }
        for db in [&a, &b, &e] {
            admin.exec(&format!("drop database if exists `{db}`")).await.unwrap();
        }
    }
    admin.close().await;
    eprintln!("{passed} schemas planned both ways; {} could not be set up here:\n{}", skipped.len(), skipped.join("\n"));
    assert!(skipped.is_empty() && failures.is_empty(), "{} of {} schemas failed, {} could not be set up:\n\n{}\n\n{}", failures.len(), cases.len(), skipped.len(), failures.join("\n\n"), skipped.join("\n"));
}

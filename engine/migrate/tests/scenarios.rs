//! Scenarios for `reylo migrate`: each builds a project and a fresh database, then does what teams
//! do (apply, edit, drift, a branch switch, history push, migrations that cannot be atomic, a run cut off
//! halfway) and checks what the command refuses and what the database ends up as. The adversarial
//! ones are chosen to break it: ways a shadow can differ from the target that are not the
//! migrations' doing, objects the comparison does not read, and commands that record what they did
//! not run. Postgres's and MySQL's run against the servers `REYLO_TEST_PG` and `REYLO_TEST_MYSQL`
//! name, and are skipped where the variable is not set; SQLite's always run. Turso's are SQLite's on a
//! libSQL server, over HTTP: each scenario starts a server of its own, the `sqld` that
//! `REYLO_TEST_SQLD` names, and is skipped where the variable is not set.

use reylo_migrate::catalog::catalog;
use reylo_migrate::db::{with_database, Db, Row, Server};
use reylo_migrate::{Dialect, Options};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// The dialect's spelling of the few column types the scenarios use.
struct Types {
    id: &'static str,
    text: &'static str,
    int: &'static str,
}

struct P {
    d: Dialect,
    dir: PathBuf,
    url: String,
    name: String,
    lines: Vec<String>,
    t: Types,
    strict: bool,
    /// The libSQL server a Turso scenario's database is on, stopped with the scenario.
    server: Option<Sqld>,
}

/// A libSQL server of one scenario's own, with an empty database, on a free port.
struct Sqld(std::process::Child);

impl Sqld {
    /// The server, and the URL that names its database; None where `REYLO_TEST_SQLD` is not set.
    fn start(dir: &std::path::Path) -> Option<(Sqld, String)> {
        let Ok(binary) = std::env::var("REYLO_TEST_SQLD") else {
            eprintln!("skipped: no sqld for turso");
            return None;
        };
        // The scenarios run side by side, and a port probed free can be probed again by another
        // before its server binds it, which would put two scenarios on one server: a port is handed
        // out once in the run, and one taken meanwhile by another process fails the server at once,
        // which tries the next.
        static TAKEN: std::sync::Mutex<Vec<u16>> = std::sync::Mutex::new(Vec::new());
        let mut taken = TAKEN.lock().unwrap_or_else(|e| e.into_inner());
        for _ in 0..5 {
            let port = loop {
                let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
                if !taken.contains(&port) {
                    taken.push(port);
                    break port;
                }
            };
            let log = std::fs::File::create(dir.join("sqld.log")).unwrap();
            let mut child = std::process::Command::new(&binary)
                .arg("--db-path")
                .arg(dir.join(format!("data-{port}.sqld")))
                .args(["--http-listen-addr", &format!("127.0.0.1:{port}")])
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap_or_else(|e| panic!("cannot start {binary}: {e}"));
            let started = std::time::Instant::now();
            while started.elapsed() < std::time::Duration::from_secs(30) {
                if child.try_wait().unwrap().is_some() {
                    break;
                }
                // Listening, and still the server this started: one that lost the port exits.
                if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    if child.try_wait().unwrap().is_none() {
                        return Some((Sqld(child), format!("libsql://127.0.0.1:{port}?tls=0")));
                    }
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        panic!("sqld did not start: {}", std::fs::read_to_string(dir.join("sqld.log")).unwrap_or_default());
    }
}

impl Drop for Sqld {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn base(d: Dialect) -> Option<String> {
    match d {
        Dialect::Postgres => std::env::var("REYLO_TEST_PG").ok(),
        Dialect::Mysql => std::env::var("REYLO_TEST_MYSQL").ok(),
        Dialect::Sqlite => Some(String::new()),
    }
}

impl P {
    async fn new(d: Dialect) -> Option<P> {
        P::with_create(d, "").await
    }

    /// A project and a fresh database, created with `create` after its name; None where the
    /// dialect's test server is not set.
    async fn with_create(d: Dialect, create: &str) -> Option<P> {
        let Some(base) = base(d) else {
            eprintln!("skipped: no test server for {}", d.name());
            return None;
        };
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let name = format!("mtest_{}_{n}", std::process::id());
        let dir = std::env::temp_dir().join(format!("reylo-migrate-{}-{name}", d.name()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("migrations")).unwrap();
        let url = match d {
            Dialect::Sqlite => format!("sqlite:{}", dir.join("dev.db").display()),
            _ => {
                let admin_url = if d == Dialect::Postgres { base.clone() } else { with_database(&base, "") };
                let mut admin = Db::connect(&admin_url, &dir).await.unwrap();
                admin.exec(&format!("drop database if exists {name}{}", if d == Dialect::Postgres { " with (force)" } else { "" })).await.unwrap();
                admin.exec(&format!("create database {name} {create}")).await.unwrap();
                admin.close().await;
                with_database(&base, &name)
            }
        };
        let t = match d {
            Dialect::Postgres => Types { id: "bigint generated always as identity primary key", text: "text", int: "bigint" },
            Dialect::Mysql => Types { id: "bigint auto_increment primary key", text: "varchar(200)", int: "bigint" },
            Dialect::Sqlite => Types { id: "integer primary key", text: "text", int: "integer" },
        };
        Some(P { d, dir, url, name, lines: vec![], t, strict: false, server: None })
    }

    /// A project and a fresh database on a libSQL server of its own (Turso's dialect, SQLite).
    async fn turso() -> Option<P> {
        let mut p = P::new(Dialect::Sqlite).await?;
        let (server, url) = Sqld::start(&p.dir)?;
        p.url = url;
        p.server = Some(server);
        Some(p)
    }

    /// A second project directory of the same dialect, with no database of its own yet.
    fn sibling(p: &P) -> P {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("reylo-migrate-{}-sib_{}_{n}", p.d.name(), std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("migrations")).unwrap();
        let t = match p.d {
            Dialect::Postgres => Types { id: p.t.id, text: p.t.text, int: p.t.int },
            _ => Types { id: p.t.id, text: p.t.text, int: p.t.int },
        };
        let (url, server) = match p.d {
            // A libSQL server's sibling gets an empty server, as a file's gets an empty file.
            Dialect::Sqlite if p.server.is_some() => {
                let (server, url) = Sqld::start(&dir).unwrap();
                (url, Some(server))
            }
            Dialect::Sqlite => (format!("sqlite:{}", dir.join("dev.db").display()), None),
            _ => (p.url.clone(), None),
        };
        P { d: p.d, dir, url, name: p.name.clone(), lines: vec![], t, strict: p.strict, server }
    }

    fn opts(&self) -> Options {
        Options {
            url: self.url.clone(),
            shadow_url: None,
            dir: self.dir.join("migrations"),
            schema: self.dir.join("schema.sql"),
            lock: self.dir.join(reylo_migrate::lock::LOCK),
            cache: self.dir.join(".cache"),
            root: self.dir.clone(),
            embedded: None,
        }
    }

    async fn run<'a, F>(&mut self, f: F) -> i32
    where
        F: AsyncFnOnce(&Options, reylo_migrate::commands::Log<'_>) -> Result<i32, reylo_migrate::Error>,
    {
        if !self.strict {
            self.relock();
        }
        let opts = self.opts();
        let mut lines = std::mem::take(&mut self.lines);
        let code = f(&opts, &mut |l: &str| lines.push(l.to_string())).await.unwrap_or_else(|e| panic!("refused to start: {e}"));
        self.lines = lines;
        code
    }

    async fn migrate(&mut self) -> i32 {
        self.run(async |o, log| reylo_migrate::deploy(o, log).await).await
    }

    async fn history_push(&mut self) -> i32 {
        self.run(async |o, log| reylo_migrate::history_push(o, log).await).await
    }

    /// `db push` alone.
    async fn db_push(&mut self) -> i32 {
        self.run(async |o, log| reylo_migrate::db_push(o, false, &Default::default(), log).await).await
    }

    /// `db push` with its flags: what it is told to drop and rename, and `--force`.
    async fn db_push_with(&mut self, push: &reylo_migrate::Push) -> i32 {
        self.run(async |o, log| reylo_migrate::db_push(o, false, push, log).await).await
    }

    /// `dev`, then `db push` where it succeeded: a development database taken to schema.sql.
    async fn dev_then_push(&mut self) -> i32 {
        let code = self.run(async |o, log| reylo_migrate::dev(o, false, log).await).await;
        if code != 0 {
            return code;
        }
        self.run(async |o, log| reylo_migrate::db_push(o, false, &Default::default(), log).await).await
    }

    /// `deploy --dry-run`: what deploy checks, and the migrations against schema.sql.
    async fn dry_run(&mut self) -> i32 {
        self.run(async |o, log| reylo_migrate::dry_run(o, log).await).await
    }

    /// `deploy --dry-run` with the replay cache on or off, and what it printed, but for what differs
    /// between any two runs (`same_run`).
    async fn check_with(&mut self, cache: bool) -> (i32, String) {
        reylo_migrate::cache::set_enabled(Some(cache));
        let code = self.dry_run().await;
        reylo_migrate::cache::set_enabled(None);
        (code, same_run(&self.output()))
    }

    /// The check four ways, each of which must print what the first does: with no cache; with the
    /// cache twice, the second answered by what the first kept; and with the cache's lines gone, so
    /// that its databases answer.
    async fn check_every_way(&mut self) -> (i32, String) {
        let off = self.check_with(false).await;
        let cold = self.check_with(true).await;
        let warm = self.check_with(true).await;
        let _ = std::fs::remove_dir_all(self.dir.join(".cache").join("migrate").join("lines"));
        let copied = self.check_with(true).await;
        assert_eq!(cold, off, "a cold cache changed the check");
        assert_eq!(warm, off, "a warm cache changed the check");
        assert_eq!(copied, off, "the cache's databases changed the check");
        off
    }

    /// The lock as development leaves it, listing every file by name, for a test that isn't about the
    /// lock (a test that is sets `strict`, and keeps it itself). What it holds of schema.sql stays where
    /// its lines do.
    fn relock(&self) {
        let path = self.dir.join(reylo_migrate::lock::LOCK);
        let Ok((files, _)) = reylo_migrate::commands::read_migrations(&self.dir.join("migrations")) else { return };
        let before = match reylo_migrate::lock::read(&path) {
            reylo_migrate::lock::State::Lock(l) => Some(l),
            _ => None,
        };
        if files.is_empty() && before.is_none() {
            return;
        }
        let texts: Vec<(String, String)> = files.iter().map(|f| (f.name.clone(), f.sql.replace("\r\n", "\n"))).collect();
        let entries = reylo_migrate::lock::chain(texts.iter().map(|(n, t)| (n.as_str(), t.as_str())));
        if before.as_ref().map(|l| &l.entries) != Some(&entries) {
            reylo_migrate::lock::write(&path, &reylo_migrate::lock::Lock { schema: None, entries }).unwrap();
        }
    }

    /// A project with linear history, as `reylo migrate init` or `reylo migrate lock` starts one: an
    /// empty lock, which the commands themselves keep from here.
    fn linear(&mut self) {
        self.strict = true;
        reylo_migrate::lock::write(&self.dir.join(reylo_migrate::lock::LOCK), &reylo_migrate::lock::Lock::default()).unwrap();
    }

    /// A lock listing `names`, chained as Reylo chains it, from the files' texts.
    fn lock(&self, names: &[&str]) {
        let texts: Vec<(String, String)> = names.iter().map(|n| (n.to_string(), std::fs::read_to_string(self.dir.join("migrations").join(n)).unwrap().replace("\r\n", "\n").trim_end().to_string())).collect();
        let l = reylo_migrate::lock::Lock { schema: None, entries: reylo_migrate::lock::chain(texts.iter().map(|(n, t)| (n.as_str(), t.as_str()))) };
        reylo_migrate::lock::write(&self.dir.join(reylo_migrate::lock::LOCK), &l).unwrap();
    }

    fn locked(&self) -> Vec<String> {
        match reylo_migrate::lock::read(&self.dir.join(reylo_migrate::lock::LOCK)) {
            reylo_migrate::lock::State::Lock(l) => l.names(),
            other => panic!("the lock reads {other:?}"),
        }
    }

    async fn dev(&mut self) -> i32 {
        self.run(async |o, log| reylo_migrate::dev(o, false, log).await).await
    }

    async fn status(&mut self) -> i32 {
        self.run(async |o, log| reylo_migrate::status(o, log).await).await
    }

    fn file(&self, name: &str, sql: &str) {
        std::fs::write(self.dir.join("migrations").join(name), sql).unwrap();
    }

    fn schema(&self, sql: &str) {
        std::fs::write(self.dir.join("schema.sql"), sql).unwrap();
    }

    fn files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.dir.join("migrations")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    /// What the commands logged since the last call.
    fn output(&mut self) -> String {
        std::mem::take(&mut self.lines).join("\n")
    }

    fn peek(&self) -> String {
        self.lines.join("\n")
    }

    async fn exec(&self, sql: &str) {
        let mut db = Db::open(&self.opts()).await.unwrap();
        db.script(sql).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
        db.close().await;
    }

    async fn try_rows(&self, sql: &str) -> Result<Vec<Row>, reylo_migrate::Error> {
        let mut db = Db::open(&self.opts()).await.unwrap();
        let rows = db.rows(sql).await;
        db.close().await;
        rows
    }

    async fn rows(&self, sql: &str) -> Vec<Row> {
        self.try_rows(sql).await.unwrap_or_else(|e| panic!("{sql}: {e}"))
    }

    /// The applied migrations, in the order Reylo reads them: by time, and on SQLite, whose clock has
    /// milliseconds, a tie by the order the rows were written.
    async fn history(&self) -> Vec<String> {
        let tie = if self.d == Dialect::Sqlite { "rowid, " } else { "" };
        self.try_rows(&format!("select name from __reylo_migrations where applied_at is not null order by applied_at, {tie}case when name = 'init.sql' then '' else name end"))
            .await
            .unwrap_or_default()
            .iter()
            .map(|r| r.text("name").to_string())
            .collect()
    }
}

/// A run's output without what any two runs differ in: the shadows' names, which MySQL's messages
/// carry, and the shadows a run sweeps, which another killed run left on the test server.
fn same_run(out: &str) -> String {
    let named = regex::Regex::new(r"reylo_shadow_[0-9a-f]+").unwrap().replace_all(out, "reylo_shadow_…").into_owned();
    named.lines().filter(|l| !has(l, r"^» Dropped \d+ shadow databases? a run left behind$")).collect::<Vec<_>>().join("\n")
}

/// Drops the replay caches only this test's target used, which would otherwise stay on the test
/// server for a week.
/// `db introspect`'s SQL, which the test expects it to write.
async fn introspect(opts: &Options) -> String {
    let mut said = Vec::new();
    match reylo_migrate::db_introspect(opts, &[], &mut |l: &str| said.push(l.to_string())).await {
        Ok(Ok(sql)) => sql,
        Ok(Err(_)) => panic!("introspect refused:\n{}", said.join("\n")),
        Err(e) => panic!("introspect: {e}"),
    }
}

async fn forget(url: &str) {
    let _ = reylo_migrate::cache::forget(url).await;
}

/// The SQL of the `reylo run` a refusal gives to undo a change, as the shell would pass it.
fn undo_in(out: &str) -> String {
    let quoted = regex::Regex::new(r"reylo run '((?:[^']|'\\'')*)'").unwrap().captures(out).unwrap_or_else(|| panic!("no undo in:\n{out}"))[1].to_string();
    quoted.replace("'\\''", "'")
}

/// Makes `p` connect to its Postgres database as a role that owns it and can't create a database, with
/// no shadow_url: no shadow database can be made, so its schema as SQL is the dump. The role is the
/// one `a_role_that_cannot_create_databases_is_told_of_shadow_url` makes, so no other role changes
/// every replay cache's key on the server under the scenarios beside it.
async fn with_no_shadows(p: &mut P) {
    p.exec("do $$ begin create role madv_nocreate login nocreatedb; exception when duplicate_object or unique_violation then null; end $$").await;
    p.exec(&format!("alter database {} owner to madv_nocreate", p.name)).await;
    p.url = as_role(&p.url, "madv_nocreate");
}

/// `url` connecting as `role`, with no password: the test servers trust their local connections.
fn as_role(url: &str, role: &str) -> String {
    regex::Regex::new(r"^([a-z]+://)(?:[^@/]*@)?").unwrap().replace(url, format!("${{1}}{role}@").as_str()).into_owned()
}

fn has(text: &str, pattern: &str) -> bool {
    regex::Regex::new(pattern).unwrap().is_match(text)
}

macro_rules! assert_has {
    ($text:expr, $pattern:expr) => {{
        let text = $text;
        assert!(has(&text, $pattern), "expected /{}/ in:\n{}", $pattern, text);
    }};
}

/// One test per dialect for each scenario, `<scenario>::postgres` and so on.
macro_rules! for_each_dialect {
    ($($scenario:ident),* $(,)?) => {$(
        mod $scenario {
            #[tokio::test(flavor = "current_thread")]
            async fn postgres() {
                if let Some(p) = super::P::new(super::Dialect::Postgres).await { let url = p.url.clone(); super::$scenario(p).await; super::forget(&url).await }
            }
            #[tokio::test(flavor = "current_thread")]
            async fn mysql() {
                if let Some(p) = super::P::new(super::Dialect::Mysql).await { super::$scenario(p).await }
            }
            #[tokio::test(flavor = "current_thread")]
            async fn sqlite() {
                if let Some(p) = super::P::new(super::Dialect::Sqlite).await { super::$scenario(p).await }
            }
            #[tokio::test(flavor = "current_thread")]
            async fn turso() {
                if let Some(p) = super::P::turso().await { super::$scenario(p).await }
            }
        }
    )*};
}

for_each_dialect!(
    dev_that_fails_on_a_locked_migration_keeps_the_lock,
    dev_that_fails_on_a_draft_locks_the_drafts_that_ran,
    the_undo_of_a_change_by_hand_leaves_what_db_push_made,
    dev_records_what_db_push_made_keeping_what_was_written_since,
    migrate_lock_refuses_a_lock_that_exists,
    status_refuses_a_lock_with_no_checksum_of_schema_sql,
    dev_relocks_an_edited_locked_migration_where_every_pair_agreed,
    without_a_lock_a_late_migration_runs_where_both_orders_agree,
    without_a_lock_a_late_migration_moves_past_migrations_ahead_of_the_clock,
    without_a_lock_a_late_migration_is_never_moved_onto_a_migration_a_run_started,
    without_a_lock_a_late_migration_is_not_moved_past_what_needs_it_or_what_it_needs,
    without_a_lock_init_sql_still_runs_first,
    status_shows_what_ran_what_is_pending_and_what_changed,
    lock_writes_the_lock_from_what_a_database_ran,
    db_reset_rebuilds_a_database_from_its_migrations,
    db_reset_refuses_what_dev_refuses_before_it_drops_anything,
    db_push_refuses_a_schema_that_declares_nothing,
    a_merge_conflict_left_in_a_file_is_refused,
    new_without_schema_sql_writes_what_the_database_has,
    new_refuses_a_migration_the_database_ran_differently,
    a_change_by_hand_before_dev_without_schema_sql_is_refused,
    applies_pending_migrations_once,
    refuses_migrations_that_do_not_add_up,
    column_order_is_not_compared,
    refuses_drift,
    an_edited_migration_is_refused,
    history_pull_takes_back_what_the_lock_refuses,
    ddl_run_by_hand_then_written_is_recorded,
    a_change_by_hand_with_schema_sql_is_told_to_write_the_migration,
    init_writes_and_records_the_baseline,
    init_leaves_out_another_tools_history,
    a_migration_the_lock_lacks_is_a_draft,
    status_reports_a_history_out_of_the_locks_order,
    a_rebased_migration_is_rewound_by_dev,
    history_pull_writes_the_lock_and_keeps_what_is_still_to_deploy,
    db_introspect_prints_the_schema,
    a_hotfix_by_hand_is_recorded_once_it_is_a_migration,
    a_hotfix_behind_a_pending_migration_moves_ahead_of_it,
    a_hotfix_that_cannot_move_ahead_is_undone_then_deployed,
    a_server_that_ran_what_a_hotfix_moved_ahead_of_takes_its_change,
    history_push_takes_a_database_to_a_reordered_lock_keeping_its_rows,
    a_database_that_skipped_a_migration_runs_it_after_the_rest,
    a_skipped_migration_moves_past_no_data_change,
    a_hotfix_whose_migration_changes_data_is_undone_then_deployed,
    a_hotfix_moves_past_no_table_made_from_a_query_and_is_none,
    history_push_follows_a_renamed_migration,
    a_failed_migration_leaves_nothing,
    a_check_the_rows_break_stops_its_change_partway,
    a_foreign_key_the_rows_break_stops_its_change_partway,
    a_unique_index_the_rows_break_lists_the_duplicates,
    a_unique_index_built_under_a_new_name_is_named_as_schema_sql_names_it,
    a_failure_the_file_caused_is_not_blamed_on_the_rows,
    a_change_split_across_tables_is_said_partway,
    rows_failures_on_postgres_say_only_what_they_know,
    a_not_null_the_rows_break_is_named_by_its_column,
    files_that_cannot_run_atomically_are_refused,
    a_lone_statement_cut_off_that_landed,
    a_lone_statement_cut_off_that_did_not_land,
    a_lone_statement_cut_off_with_no_fingerprint_to_compare,
    a_shadow_left_by_a_killed_run_is_swept,
    a_rebuilt_table_does_not_cascade,
    one_schema_built_two_ways_has_one_catalog,
    cached_and_uncached_checks_print_the_same_lines,
    a_cached_prefix_replays_only_what_follows_it,
    an_edited_migration_misses_the_cache,
    concurrent_runs_building_one_cache_both_succeed,
    create_drafts_one_migration_cached_or_not,
    two_applies_at_once_apply_each_migration_once,
    create_states_what_it_drops_and_renames,
    wipe_deletes_rows_and_keeps_the_schema,
    deploy_builds_no_shadow_database_and_trusts_the_fingerprint,
    deploy_goes_on_where_it_has_no_fingerprint_to_compare,
    deploy_refuses_tables_with_no_history,
    a_lock_whose_order_is_not_the_names_survives_one_run,
    a_fixed_migration_no_database_ran_is_locked_again,
    init_locks_the_baseline_in_a_locked_project,
    history_push_keeps_a_data_migration_that_ran,
    create_guesses_no_rename_of_unrelated_tables,
    the_drift_advice_in_a_locked_project_locks_the_migration_first,
    create_names_a_type_change_that_loses_values_in_drop,
    create_assumes_a_rename_where_one_goes_and_one_comes,
    db_push_watch_syncs_again_when_schema_sql_changes,
    db_push_takes_a_development_database_to_schema_sql,
    db_push_rewinds_what_no_longer_agrees,
    db_push_keeps_what_no_migration_makes_yet,
    apply_dev_locks_a_draft_after_what_is_locked,
    a_locked_project_runs_its_lock,
    a_lock_conflict_is_resolved_by_apply_dev,
    create_resolves_the_names_drop_gives,
    create_renames_a_table_keeping_its_rows,
    create_renames_a_not_null_column_with_its_table,
    create_offers_renames_only_to_what_the_dropped_ones_table_adds,
    create_offers_no_rename_to_a_column_another_table_renames_onto,
    create_lists_every_rename_it_assumes,
    commands_create_prints_for_names_with_spaces_run_as_printed,
    deploy_compares_with_what_dev_replayed,
    dev_says_schema_sql_is_ahead_once_a_changed_object_is_pushed,
    deploy_refuses_any_schema_with_no_history,
    init_on_an_empty_database_writes_an_empty_schema_and_lock,
    a_broken_draft_keeps_what_sync_made,
    a_migrations_directory_that_isnt_there_is_refused,
    a_run_cut_off_goes_on_where_its_schema_is_unchanged,
    a_rewind_that_fails_leaves_the_row_before_it_unverified,
    create_settles_every_guess_with_one_command,
    create_asks_about_a_column_replaced_by_one_of_another_type,
    db_push_renames_a_table_keeping_its_rows,
    db_push_renames_a_table_back_when_it_rewinds,
    db_push_keeps_what_it_records_when_it_undoes_the_rest,
    db_push_runs_or_records_a_migration_cut_off,
    db_push_rewinds_only_what_the_rewound_migrations_made,
    db_push_refuses_a_drop_until_drop_names_it,
    db_push_force_accepts_every_destructive_change,
    db_push_takes_a_rename_it_is_told_of,
    create_guesses_no_rename_a_table_that_stays_could_explain,
    each_run_leaves_its_fingerprint_on_its_newest_row,
    a_failed_migration_leaves_the_fingerprint_of_those_before_it,
    a_run_cut_off_before_its_fingerprint_is_refused,
);

async fn the_undo_of_a_change_by_hand_leaves_what_db_push_made(mut p: P) {
    let (id, text) = (p.t.id, p.t.text);
    p.schema(&format!("create table users (id {id}, name {text});"));
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, name {text});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.schema(&format!("create table users (id {id}, name {text}, bio {text});"));
    assert_eq!(p.db_push().await, 0, "{}", p.peek());
    p.exec("insert into users (name, bio) values ('ada', 'pushed')").await;
    p.exec("create index hand_idx on users (name)").await;
    p.output();
    // Only the index is refused: schema.sql explains the column db push made.
    assert_eq!(p.dev().await, 1);
    let out = p.output();
    // On MySQL a table's definition holds its columns, so its index and the pushed column can't be
    // undone apart: no command is offered, rather than one that undoes too much.
    let undo = if p.d == Dialect::Mysql {
        assert_has!(&out, "To undo it, change it back by hand");
        "drop index hand_idx on users".to_string()
    } else {
        undo_in(&out)
    };
    assert!(undo.contains("hand_idx") && !undo.contains("bio"), "{out}");
    p.exec(&undo).await;
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_eq!(p.rows("select bio from users").await[0].text("bio"), "pushed");
}

// db push, then the migration written for what it made: dev records the migration rather than
// running it again, and what was written into the database since stays. A check is made in place on
// Postgres and MySQL and by rebuilding the table on SQLite, whose own copy of the table's rows is no
// data statement of the migration.
async fn dev_records_what_db_push_made_keeping_what_was_written_since(mut p: P) {
    let (id, text) = (p.t.id, p.t.text);
    let v1 = format!("create table posts (id {id}, body {text});");
    p.schema(&v1);
    p.file("20261027100000_posts.sql", &v1);
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.exec("insert into posts (body) values ('a'), ('b')").await;
    p.schema(&format!("create table posts (id {id}, body {text} check (body <> ''), slug {text});"));
    assert_eq!(p.db_push().await, 0, "{}", p.peek());
    p.exec("update posts set slug = 'kept'").await;
    p.output();
    // MySQL adds a check by copying the table, which --blocking accepts.
    let choices = reylo_migrate::Choices { blocking: p.d == Dialect::Mysql, ..Default::default() };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "slug", &choices, log).await).await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"_slug\.sql\s+recorded: already in place");
    assert_eq!(p.rows("select count(*) as n from posts where slug = 'kept'").await[0].text("n"), "2");
}

async fn migrate_lock_refuses_a_lock_that_exists(mut p: P) {
    let int = p.t.int;
    p.linear();
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.run(async |o, log| reylo_migrate::lock_from_history(o, log).await).await, 1);
    assert_has!(p.output(), r"✗ A lock exists already: \S*reylo\.lock");
    assert_eq!(p.locked(), ["20261027100000_a.sql"]);
}

async fn status_refuses_a_lock_with_no_checksum_of_schema_sql(mut p: P) {
    let int = p.t.int;
    p.linear();
    p.schema(&format!("create table a (id {int});"));
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    // The lock written again where the migrations don't make schema.sql holds no checksum of it.
    p.schema(&format!("create table a (id {int}); create table b (id {int});"));
    std::fs::remove_file(p.dir.join(reylo_migrate::lock::LOCK)).unwrap();
    assert_eq!(p.run(async |o, log| reylo_migrate::lock_from_history(o, log).await).await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.status().await, 1);
    assert_has!(p.output(), r"✗ The lock holds no checksum of \S*schema\.sql");
}

async fn dev_that_fails_on_a_locked_migration_keeps_the_lock(mut p: P) {
    let int = p.t.int;
    p.linear();
    p.file("20261027100000_a.sql", &format!("create table a (id {int} primary key);"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.exec("insert into a (id) values (1)").await;
    // Main's lines, merged in: a2 runs, b then fails on this database's rows, and c comes after it.
    p.file("20261027105000_a2.sql", &format!("create table a2 (id {int});"));
    p.file("20261027110000_b.sql", "insert into a (id) values (1);");
    p.file("20261027120000_c.sql", &format!("create table c (id {int});"));
    p.lock(&["20261027100000_a.sql", "20261027105000_a2.sql", "20261027110000_b.sql", "20261027120000_c.sql"]);
    // And a draft of this branch's own, so that the run goes through the shadows.
    p.file("20261027130000_d.sql", &format!("create table d (id {int});"));
    let lock = std::fs::read_to_string(p.dir.join(reylo_migrate::lock::LOCK)).unwrap();
    p.output();
    assert_eq!(p.dev().await, 1);
    assert_has!(p.output(), r"✓ 20261027105000_a2\.sql +applied[\s\S]*✗ 20261027110000_b\.sql +failed");
    assert_eq!(std::fs::read_to_string(p.dir.join(reylo_migrate::lock::LOCK)).unwrap(), lock, "the lock keeps the lines every database runs");
}

async fn dev_that_fails_on_a_draft_locks_the_drafts_that_ran(mut p: P) {
    let int = p.t.int;
    p.linear();
    p.file("20261027100000_a.sql", &format!("create table a (id {int} primary key);"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.exec("insert into a (id) values (1)").await;
    // This branch's two: x runs, then y fails on this database's rows.
    p.file("20261027110000_x.sql", &format!("create table x (id {int});"));
    p.file("20261027120000_y.sql", "insert into a (id) values (1);");
    p.output();
    assert_eq!(p.dev().await, 1);
    let out = p.output();
    assert_has!(&out, r"✓ 20261027110000_x\.sql +applied[\s\S]*✗ 20261027120000_y\.sql +failed[\s\S]*✓ Wrote \S*reylo\.lock, through 20261027110000_x\.sql");
    assert_eq!(p.locked(), ["20261027100000_a.sql", "20261027110000_x.sql"]);
}

async fn dev_relocks_an_edited_locked_migration_where_every_pair_agreed(mut p: P) {
    let int = p.t.int;
    p.linear();
    p.file("20261027100000_one.sql", &format!("create table a (id {int});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    // Another database runs and locks the next migration; this one hasn't run it, and it is fixed.
    p.file("20261027110000_two.sql", &format!("create table b (id {int});"));
    let other = P::new(p.d).await.unwrap();
    let dev_url = std::mem::replace(&mut p.url, other.url.clone());
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.url = dev_url;
    p.file("20261027110000_two.sql", &format!("create table c (id {int});"));
    p.output();
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"Wrote \S*reylo\.lock");
    assert_eq!(p.status().await, 0, "{}", p.peek());
}

async fn without_a_lock_a_late_migration_runs_where_both_orders_agree(mut p: P) {
    let int = p.t.int;
    p.strict = true;
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    p.file("20261027120000_c.sql", &format!("create table c (id {int});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert!(!p.dir.join(reylo_migrate::lock::LOCK).exists(), "dev writes no lock where there is none");
    assert_eq!(p.status().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"» With no lock, migrations run in name order\n  To write one:\n    reylo migrate lock");
    let other = P::new(p.d).await.unwrap();
    // Another database deploys them; then one that sorts between them arrives, as from a merge.
    let deploy_on = async |p: &mut P, url: &str| {
        let dev_url = std::mem::replace(&mut p.url, url.to_string());
        let code = p.migrate().await;
        p.url = dev_url;
        code
    };
    assert_eq!(deploy_on(&mut p, &other.url).await, 0, "{}", p.peek());
    p.file("20261027110000_b.sql", &format!("create table b (id {int});"));
    p.output();
    // Its order and name order end the same, so it runs where it is, in development and at deploy.
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_eq!(p.history().await, ["20261027100000_a.sql", "20261027120000_c.sql", "20261027110000_b.sql"]);
    assert_eq!(deploy_on(&mut p, &other.url).await, 0, "{}", p.peek());
    assert_eq!(other.history().await, ["20261027100000_a.sql", "20261027120000_c.sql", "20261027110000_b.sql"]);
    // One whose two orders end differently is refused in both, with the way out.
    p.file("20261027140000_d.sql", &format!("alter table a add column x {int};"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_eq!(deploy_on(&mut p, &other.url).await, 0, "{}", p.peek());
    p.file("20261027130000_e.sql", "create table e as select * from a;");
    p.output();
    assert_eq!(p.dev().await, 1);
    let out = p.output();
    assert_has!(&out, r"✗ Migration 20261027130000_e\.sql sorts before 20261027140000_d\.sql, which this database already ran\n  Run in name order, as on a fresh database, the migrations end differently:\n\n    --- this database\n    \+\+\+ a fresh database\n(?:    [-+] [^\n]+\n)*    - column e\.x [^\n]+\n(?:    [-+] [^\n]+\n)*\n  Give it a later name");
    assert_has!(&out, r"\n\n  Give it a later name, so that every database runs it after 20261027140000_d\.sql:\n    mv \S+/20261027130000_e\.sql \S+/\d{14}_e\.sql\n");
    assert_eq!(deploy_on(&mut p, &other.url).await, 1);
    assert_has!(p.output(), r"sorts before 20261027140000_d\.sql");
    // Renamed as the refusal says, to sort last, it runs.
    let line = out.lines().find(|l| l.trim_start().starts_with("mv ")).unwrap().trim().to_string();
    let words: Vec<&str> = line.split(' ').collect();
    std::fs::rename(words[1], words[2]).unwrap();
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_eq!(deploy_on(&mut p, &other.url).await, 0, "{}", p.peek());
}

async fn without_a_lock_a_late_migration_moves_past_migrations_ahead_of_the_clock(mut p: P) {
    let int = p.t.int;
    p.strict = true;
    // Migrations dated ahead of the clock, as a machine with a wrong clock writes them.
    p.file("20990101000000_a.sql", &format!("create table a (id {int});"));
    p.file("20990101020000_d.sql", &format!("alter table a add column x {int};"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    // A late migration, and a pending one after it that needs it.
    p.file("20990101010000_e.sql", "create table e as select * from a;");
    p.file("20990101030000_f.sql", &format!("alter table e add column y {int};"));
    p.output();
    assert_eq!(p.dev().await, 1);
    let out = p.output();
    assert_has!(&out, r"Give it a later name, so that every database runs it after 20990101020000_d\.sql:\n    mv \S+/20990101010000_e\.sql \S+/20990101020001_e\.sql\n");
    // Renamed as the refusal says, it sorts after what it follows and before what needs it, and runs.
    let line = out.lines().find(|l| l.trim_start().starts_with("mv ")).unwrap().trim().to_string();
    let words: Vec<&str> = line.split(' ').collect();
    std::fs::rename(words[1], words[2]).unwrap();
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_eq!(p.history().await, ["20990101000000_a.sql", "20990101020000_d.sql", "20990101020001_e.sql", "20990101030000_f.sql"]);
}

async fn without_a_lock_a_late_migration_is_never_moved_onto_a_migration_a_run_started(mut p: P) {
    let int = p.t.int;
    p.strict = true;
    p.file("20990101000000_a.sql", &format!("create table a (id {int});"));
    p.file("20990101020000_d.sql", &format!("alter table a add column x {int};"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    // A run cut off with its row marked started, its change in place, in the one slot after d.
    let g = format!("create table g (id {int});");
    p.file("20990101020001_e.sql", &g);
    p.exec(&g).await;
    let column = if p.d == Dialect::Mysql { "`sql`" } else { "sql" };
    p.exec(&format!("insert into __reylo_migrations (name, {column}, applied_at) values ('20990101020001_e.sql', '{g}', null)")).await;
    // A late migration of the same name goes after it, which may be what it needs, not onto it.
    p.file("20990101010000_e.sql", "create table e as select * from a;");
    p.output();
    assert_eq!(p.dry_run().await, 1);
    assert_has!(p.output(), r"\n    mv \S+/20990101010000_e\.sql \S+/20990101020002_e\.sql(\n|$)");
}

async fn without_a_lock_a_late_migration_is_not_moved_past_what_needs_it_or_what_it_needs(mut p: P) {
    let int = p.t.int;
    p.strict = true;
    p.file("20990101000000_a.sql", &format!("create table a (id {int});"));
    p.file("20990101020000_d.sql", &format!("alter table a add column x {int};"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    // A run cut off after its change landed, and a pending migration before it that needs the late one.
    let g = format!("create table g (id {int});");
    p.file("20990101040000_g.sql", &g);
    p.exec(&g).await;
    let column = if p.d == Dialect::Mysql { "`sql`" } else { "sql" };
    p.exec(&format!("insert into __reylo_migrations (name, {column}, applied_at) values ('20990101040000_g.sql', '{g}', null)")).await;
    p.file("20990101010000_e.sql", "create table e as select * from a;");
    p.file("20990101030000_f.sql", &format!("alter table e add column y {int};"));
    p.output();
    // No name is both after what it may need and before what needs it: no move is printed.
    assert_eq!(p.dry_run().await, 1);
    let out = p.output();
    assert_has!(&out, r"No name sorts both after 20990101040000_g\.sql, which a run cut off already applied, and before 20990101030000_f\.sql, which is pending: put these migrations in the order they need by hand\.");
    assert!(!out.contains("mv "), "{out}");
}

async fn without_a_lock_init_sql_still_runs_first(mut p: P) {
    let (id, text) = (p.t.id, p.t.text);
    p.exec(&format!("create table users (id {id}, email {text})")).await;
    assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
    p.strict = true;
    std::fs::remove_file(p.dir.join(reylo_migrate::lock::LOCK)).unwrap();
    std::fs::remove_file(p.dir.join("schema.sql")).unwrap();
    p.file("20261028100000_nick.sql", &format!("alter table users add column nick {text};"));
    p.output();
    // init.sql sorts before every migration, as it runs: nothing here is late.
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    let q = P::new(p.d).await.unwrap();
    p.url = q.url.clone();
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(q.history().await, ["init.sql", "20261028100000_nick.sql"]);
}

async fn status_shows_what_ran_what_is_pending_and_what_changed(mut p: P) {
    let (int, text) = (p.t.int, p.t.text);
    p.linear();
    p.schema(&format!("create table a (id {int});"));
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.file("20261027110000_b.sql", &format!("create table b (id {int});"));
    p.output();
    // A migration this database hasn't run is out of step: status exits 1.
    assert_eq!(p.status().await, 1);
    let out = p.output();
    assert_has!(&out, r"✓ 20261027100000_a\.sql +applied \d{4}-\d\d-\d\d \d\d:\d\d");
    assert_has!(&out, r"· 20261027110000_b\.sql +pending");
    assert_has!(&out, r"✓ Unchanged since its last migration");
    assert_has!(&out, r"✓ The migrations match \S*schema\.sql");
    assert_has!(&out, r"» This database hasn't run 20261027110000_b\.sql\n  To run it in development:\n    reylo migrate dev\n  To run it in production:\n    reylo migrate deploy");
    assert_has!(&out, r"✗ The lock doesn't list 20261027110000_b\.sql yet");
    // Run, it is in step: status exits 0.
    p.schema(&format!("create table a (id {int}); create table b (id {int});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.status().await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"✓ 20261027110000_b\.sql +applied");
    assert_has!(&out, r"✓ The migration files match \S*reylo\.lock\n✓ The migrations match \S*schema\.sql$");
    // A change by hand shows; status changes nothing, and exits 1.
    p.exec(&format!("alter table a add column note {text}")).await;
    assert_eq!(p.status().await, 1);
    assert_has!(p.output(), r"✗ This database was changed outside its migrations\n\n  \+ column a\.note");
    assert_eq!(p.history().await, ["20261027100000_a.sql", "20261027110000_b.sql"]);
}

async fn lock_writes_the_lock_from_what_a_database_ran(mut p: P) {
    let int = p.t.int;
    p.linear();
    p.schema(&format!("create table a (id {int}); create table b (id {int});"));
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    p.file("20261027110000_b.sql", &format!("create table b (id {int});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    let before = std::fs::read_to_string(p.dir.join(reylo_migrate::lock::LOCK)).unwrap();
    std::fs::remove_file(p.dir.join(reylo_migrate::lock::LOCK)).unwrap();
    p.output();
    assert_eq!(p.run(async |o, log| reylo_migrate::lock_from_history(o, log).await).await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"✓ Wrote \S*reylo\.lock: 2 migrations, in the order this database ran them");
    assert_has!(&out, r"✓ The migrations match \S*schema\.sql");
    assert_eq!(std::fs::read_to_string(p.dir.join(reylo_migrate::lock::LOCK)).unwrap(), before);
    // A migration the database ran that isn't in the migrations as it ran it is refused.
    std::fs::remove_file(p.dir.join(reylo_migrate::lock::LOCK)).unwrap();
    p.file("20261027110000_b.sql", &format!("create table b (id {int}, x {int});"));
    assert_eq!(p.run(async |o, log| reylo_migrate::lock_from_history(o, log).await).await, 1);
    assert_has!(p.output(), r"Nothing was changed");
    assert!(!p.dir.join(reylo_migrate::lock::LOCK).exists());
}

async fn db_reset_rebuilds_a_database_from_its_migrations(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    p.schema(&format!("create table users (id {id}, name {text});"));
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, name {text});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.exec("insert into users (name) values ('ada')").await;
    p.exec(&format!("create table stray (id {int})")).await;
    p.output();
    assert_eq!(p.run(async |o, log| reylo_migrate::db_reset(o, false, log).await).await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"Dropped [^\n]*users \(1 row\)");
    assert_has!(&out, r"✓ 20261027100000_users\.sql +applied");
    assert!(p.try_rows("select 1 from stray").await.is_err(), "{out}");
    assert!(p.rows("select name from users").await.is_empty());
    assert_eq!(p.history().await, ["20261027100000_users.sql"]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    // Never in CI.
    assert_eq!(p.run(async |o, log| reylo_migrate::db_reset(o, true, log).await).await, 1);
    assert_has!(p.output(), r"CI is set, and reylo db reset is for development databases");
}

async fn db_reset_refuses_what_dev_refuses_before_it_drops_anything(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    p.linear();
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, name {text});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.exec("insert into users (name) values ('ada')").await;
    let kept = |p: &mut P| {
        let out = p.output();
        assert_has!(&out, r"Nothing was changed\.");
        assert!(!has(&out, r"Resetting|Emptied the history"), "{out}");
    };
    // A migration left with git's conflict markers in it.
    p.file("20261027110000_tags.sql", &format!("<<<<<<< HEAD\ncreate table tags (id {int});\n=======\ncreate table labels (id {int});\n>>>>>>> main\n"));
    p.output();
    assert_eq!(p.run(async |o, log| reylo_migrate::db_reset(o, false, log).await).await, 1);
    assert_has!(p.peek(), r"Git's conflict markers are still in \S*20261027110000_tags\.sql");
    kept(&mut p);
    assert_eq!(p.rows("select name from users").await.len(), 1);
    assert_eq!(p.history().await, ["20261027100000_users.sql"]);
    // A lock left with git's conflict markers in it.
    std::fs::remove_file(p.dir.join("migrations").join("20261027110000_tags.sql")).unwrap();
    let lock = p.dir.join(reylo_migrate::lock::LOCK);
    let text = std::fs::read_to_string(&lock).unwrap();
    std::fs::write(&lock, format!("{text}<<<<<<< HEAD\n")).unwrap();
    assert_eq!(p.run(async |o, log| reylo_migrate::db_reset(o, false, log).await).await, 1);
    assert_has!(p.peek(), r"The lock has a merge conflict");
    kept(&mut p);
    assert_eq!(p.rows("select name from users").await.len(), 1);
    assert_eq!(p.history().await, ["20261027100000_users.sql"]);
    std::fs::write(&lock, text).unwrap();
    // A migrations directory that is gone, which this database ran a migration from.
    let (dir, away) = (p.dir.join("migrations"), p.dir.join("migrations-away"));
    std::fs::rename(&dir, &away).unwrap();
    assert_eq!(p.run(async |o, log| reylo_migrate::db_reset(o, false, log).await).await, 1);
    assert_has!(p.peek(), r"There's no migrations directory at \S*migrations\n  This database ran 1 migration from it");
    kept(&mut p);
    assert_eq!(p.rows("select name from users").await.len(), 1);
    std::fs::rename(&away, &dir).unwrap();
    // With the files as dev takes them, it resets.
    assert_eq!(p.run(async |o, log| reylo_migrate::db_reset(o, false, log).await).await, 0, "{}", p.peek());
    assert!(p.rows("select name from users").await.is_empty());
    assert_eq!(p.history().await, ["20261027100000_users.sql"]);
}

async fn db_push_refuses_a_schema_that_declares_nothing(mut p: P) {
    let (id, text) = (p.t.id, p.t.text);
    p.schema(&format!("create table users (id {id}, name {text});"));
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, name {text});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.exec("insert into users (name) values ('ada')").await;
    p.schema("-- emptied\n\n");
    p.output();
    assert_eq!(p.db_push().await, 1);
    assert_has!(p.output(), r"✗ Nothing is declared in \S*schema\.sql\n  Synchronizing this database with it would drop every table, with its rows\.");
    assert_eq!(p.rows("select name from users").await.len(), 1);
}

async fn a_merge_conflict_left_in_a_file_is_refused(mut p: P) {
    let int = p.t.int;
    p.schema(&format!("create table a (id {int});\n<<<<<<< HEAD\ncreate table b (id {int});\n=======\ncreate table c (id {int});\n>>>>>>> main\n"));
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    assert_eq!(p.dev().await, 1);
    assert_has!(p.output(), r"Git's conflict markers are still in \S*schema\.sql");
    assert_eq!(p.status().await, 1);
    assert_has!(p.output(), r"✗ Git's conflict markers are still in \S*schema\.sql");
}

async fn new_without_schema_sql_writes_what_the_database_has(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, name {text});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    // Changed by hand on the development database, then written as a migration.
    p.exec(&format!("create table tags (id {int})")).await;
    p.output();
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tags", &Default::default(), log).await).await, 0, "{}", p.peek());
    // It says what the migration changes.
    assert_has!(p.output(), r"✓ Wrote \S*_tags\.sql\n(?:  \+ [^\n]+\n)*  \+ table tags[^\n]*\n      id ");
    let written = p.files().into_iter().find(|f| f.ends_with("_tags.sql")).unwrap();
    let sql = std::fs::read_to_string(p.dir.join("migrations").join(&written)).unwrap();
    assert_has!(&sql, r"(?i)create table [^\n]*tags");
    // dev records it as in place, and the database is its migrations again.
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_eq!(p.history().await, ["20261027100000_users.sql".to_string(), written.clone()]);
    p.output();
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"✓ Up to date");
    // With nothing changed, the migration is empty, to write by hand: a change of data or of schema.
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "backfill", &Default::default(), log).await).await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"✓ Wrote \S*_backfill\.sql\n» Empty, to write by hand: this database has nothing new");
    assert!(!out.contains("change to data"), "{out}");
}

async fn new_refuses_a_migration_the_database_ran_differently(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    let users = format!("create table users (id {id}, name {text});");
    let orders = format!("create table orders (id {int});");
    p.schema(&format!("{users}\n{orders}\ncreate table tags (id {int});"));
    p.file("20261027100000_users.sql", &users);
    p.file("20261027110000_orders.sql", &orders);
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    // A comment changes no statement the database ran.
    p.file("20261027100000_users.sql", &format!("-- the people\n{users}"));
    // A migration this database ran, since edited: refused, with nothing written.
    p.file("20261027110000_orders.sql", &format!("create table orders (id {int}, total {int});"));
    p.output();
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tags", &Default::default(), log).await).await, 1);
    let out = p.output();
    assert_has!(&out, r"✗ Migration 20261027110000_orders\.sql changed after this database applied it[\s\S]*reylo migrate dev[\s\S]*Nothing was changed\.");
    assert!(!out.contains("20261027100000_users.sql"), "{out}");
    // One it ran, since deleted, as after a switch to another branch: refused too.
    std::fs::remove_file(p.dir.join("migrations").join("20261027110000_orders.sql")).unwrap();
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tags", &Default::default(), log).await).await, 1);
    assert_has!(p.output(), r"✗ This database applied 20261027110000_orders\.sql, which isn't in [\s\S]*reylo migrate dev");
    assert_eq!(p.files(), ["20261027100000_users.sql"]);
    // dev undoes it, and new then writes the migration.
    p.schema(&format!("{users}\ncreate table tags (id {int});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tags", &Default::default(), log).await).await, 0, "{}", p.peek());
    // A migration written and not yet run is no disagreement: new runs again.
    p.schema(&format!("{users}\ncreate table tags (id {int});\ncreate table notes (id {int});"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "notes", &Default::default(), log).await).await, 0, "{}", p.peek());
    assert_eq!(p.files().len(), 3, "{:?}", p.files());
}

async fn a_change_by_hand_before_dev_without_schema_sql_is_refused(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, name {text});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    // Another branch's migration, merged and not yet run, then a change by hand.
    p.file("20261027110000_orders.sql", &format!("create table orders (id {int});"));
    p.exec(&format!("alter table users add column nickname {text}")).await;
    p.output();
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "nickname", &Default::default(), log).await).await, 1);
    assert_has!(p.output(), r"This database hasn't applied 20261027110000_orders\.sql");
    assert_eq!(p.dev().await, 1);
    let out = p.output();
    assert_has!(&out, r"✗ This database has a change no migration makes yet");
    assert!(!out.contains("To keep it"), "{out}");
    p.exec(&undo_in(&out)).await;
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_eq!(p.history().await, ["20261027100000_users.sql", "20261027110000_orders.sql"]);
}

async fn applies_pending_migrations_once(mut p: P) {
    p.schema(&format!("create table users (id {}, email {} not null, name {});", p.t.id, p.t.text, p.t.text));
    p.file("20261027100000_users.sql", &format!("create table users (id {}, email {} not null);", p.t.id, p.t.text));
    p.file("20261027110000_name.sql", &format!("alter table users add column name {};", p.t.text));
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    assert!(p.history().await.is_empty());
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.history().await, ["20261027100000_users.sql", "20261027110000_name.sql"]);
    p.output();
    assert_eq!(p.migrate().await, 0);
    assert_has!(p.output(), "Up to date");
}

async fn refuses_migrations_that_do_not_add_up(mut p: P) {
    p.schema(&format!("create table users (id {}, email {} not null, name {});", p.t.id, p.t.text, p.t.text));
    p.file("20261027100000_users.sql", &format!("create table users (id {}, email {} not null);", p.t.id, p.t.text));
    // check holds the migrations against schema.sql, and runs nothing.
    assert_eq!(p.dry_run().await, 1);
    assert_has!(p.output(), r"The schema is ahead of the migrations\n\n    alter table users add column name [^\n]+;\n\n  To write the changes to a migration file:\n    reylo migrate new <name>");
    assert!(p.try_rows("select 1 from users").await.is_err(), "nothing was applied");
    // apply moves the migrations into the database and never reads schema.sql.
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert!(p.try_rows("select 1 from users").await.is_ok());
    // The other way round: a migration makes what schema.sql doesn't declare.
    p.schema(&format!("create table users (id {}, email {} not null);", p.t.id, p.t.text));
    p.file("20261027110000_tags.sql", &format!("create table tags (id {});", p.t.int));
    p.output();
    assert_eq!(p.dry_run().await, 1);
    assert_has!(p.output(), r"The migrations make what \S*schema\.sql doesn't declare[\s\S]*tags");
}

async fn column_order_is_not_compared(mut p: P) {
    p.schema(&format!("create table users (id {}, name {}, email {} not null);", p.t.id, p.t.text, p.t.text));
    p.file("20261027100000_users.sql", &format!("create table users (id {}, email {} not null);", p.t.id, p.t.text));
    p.file("20261027110000_name.sql", &format!("alter table users add column name {};", p.t.text));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
}

async fn refuses_drift(mut p: P) {
    p.schema(&format!("create table users (id {}, email {} not null);", p.t.id, p.t.text));
    p.file("20261027100000_users.sql", &format!("create table users (id {}, email {} not null);", p.t.id, p.t.text));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec(&format!("alter table users add column hotfix {}", p.t.int)).await;
    p.schema(&format!("create table users (id {}, email {} not null, nick {});", p.t.id, p.t.text, p.t.text));
    p.file("20261027110000_nick.sql", &format!("alter table users add column nick {};", p.t.text));
    // deploy sees that it changed; check says how.
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), r"✗ This database was changed outside its migrations\n\n  \+ column users\.hotfix [^\n]+\n\n[\s\S]*reylo migrate deploy --dry-run\n\nNothing was changed\.$");
    assert_eq!(p.dry_run().await, 1);
    let out = p.output();
    assert_has!(&out, r"✗ This database was changed outside its migrations\n\n    alter table users add column hotfix [^\n]+;\n");
    assert_has!(&out, r"To keep it, add it to \S*schema\.sql and write its migration in development:\n    reylo migrate new <name>\n    reylo migrate dev\n  Then record it here, running nothing:\n    reylo migrate history push");
    assert_eq!(p.history().await, ["20261027100000_users.sql"]);
}

async fn an_edited_migration_is_refused(mut p: P) {
    let (int, text) = (p.t.int, p.t.text);
    let original = format!("create table users (id {int});");
    p.schema(&original);
    p.file("20261027100000_users.sql", &original);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    // An edit to comments alone changes nothing that ran.
    p.file("20261027100000_users.sql", &format!("-- the users\n{original}"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.output();
    // A real edit is refused, both texts shown, whatever the database has.
    p.file("20261027100000_users.sql", &format!("create table users (id {int}, email {text});"));
    p.schema(&format!("create table users (id {int}, email {text});"));
    assert_eq!(p.migrate().await, 1);
    let out = p.output();
    assert_has!(&out, r"✗ Migration 20261027100000_users\.sql changed after this database applied it\n\n    --- as applied\n    \+\+\+ on disk\n    - create table users \(id [^\n]+\);\n    \+ create table users \(id [^\n]+, email [^\n]+\);\n\n  Write the change as a new migration\.\n\n  Take back the text this database ran:\n    reylo migrate history pull\n  On a development database, run it again as it now reads:\n    reylo migrate dev");
    p.exec(&format!("alter table users add column email {text}")).await;
    assert_eq!(p.migrate().await, 1, "a database that matches the edit records nothing without being told: {}", p.peek());
    assert_has!(p.output(), "changed after this database applied it");
    p.exec("alter table users drop column email").await;
    // pull takes back the text it ran.
    assert_eq!(p.run(async |o, log| reylo_migrate::history_pull(o, log).await).await, 0, "{}", p.peek());
    assert_has!(p.output(), r"Pulling from [^\n]+\n  ↓ 20261027100000_users\.sql  written over: it differed from what ran");
    assert_eq!(std::fs::read_to_string(p.dir.join("migrations").join("20261027100000_users.sql")).unwrap(), original);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), "Up to date");
    // A deleted one is written again; a draft is left as it is.
    std::fs::remove_file(p.dir.join("migrations").join("20261027100000_users.sql")).unwrap();
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), r"This database applied 20261027100000_users\.sql, which isn't in [^\n]*migrations\n\n    create table users \(id [^\n]+\);\n\n  Write it back as it ran:\n    reylo migrate history pull\n  On a development database, undo it instead:\n    reylo migrate dev");
    p.file("20261027110000_draft.sql", "select 1;");
    assert_eq!(p.run(async |o, log| reylo_migrate::history_pull(o, log).await).await, 0, "{}", p.peek());
    assert_has!(p.output(), r"↓ 20261027100000_users\.sql  written: it wasn't here\nNot run on this database, and left as they are: 20261027110000_draft\.sql");
    assert_eq!(std::fs::read_to_string(p.dir.join("migrations").join("20261027100000_users.sql")).unwrap(), original);
    assert_eq!(p.run(async |o, log| reylo_migrate::history_pull(o, log).await).await, 0, "{}", p.peek());
    assert_has!(p.output(), r"^✓ Every migration this database ran is in \S*migrations, as it ran it\n");
}

async fn history_pull_takes_back_what_the_lock_refuses(mut p: P) {
    let int = p.t.int;
    p.linear();
    let original = format!("create table a (id {int});");
    p.schema(&original);
    p.file("20261027100000_a.sql", &original);
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.output();
    // A formatter's reflow reads as an edit to the lock, so pull writes it back.
    p.file("20261027100000_a.sql", &format!("create table a (\n  id {int}\n);"));
    assert_eq!(p.dry_run().await, 1);
    assert_has!(p.output(), r"changed after it was locked[\s\S]*reylo migrate history pull");
    assert_eq!(p.run(async |o, log| reylo_migrate::history_pull(o, log).await).await, 0, "{}", p.peek());
    assert_has!(p.output(), "written over");
    assert_eq!(std::fs::read_to_string(p.dir.join("migrations").join("20261027100000_a.sql")).unwrap(), original);
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    p.output();
    // A comment isn't, and stays.
    let commented = format!("-- the table\n{original}");
    p.file("20261027100000_a.sql", &commented);
    assert_eq!(p.run(async |o, log| reylo_migrate::history_pull(o, log).await).await, 0, "{}", p.peek());
    assert_has!(p.output(), "Every migration this database ran is in");
    assert_eq!(std::fs::read_to_string(p.dir.join("migrations").join("20261027100000_a.sql")).unwrap(), commented);
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    p.output();
    // A database changed by hand isn't pulled from: the files its history gives wouldn't make it.
    p.exec(&format!("create table stray (id {int})")).await;
    std::fs::remove_file(p.dir.join(reylo_migrate::lock::LOCK)).unwrap();
    assert_eq!(p.run(async |o, log| reylo_migrate::history_pull(o, log).await).await, 1);
    assert_has!(p.output(), r"✗ This database was changed outside its migrations\n[\s\S]*stray[\s\S]*reylo migrate history push[\s\S]*Nothing was changed\.");
    assert!(!p.dir.join(reylo_migrate::lock::LOCK).exists(), "it wrote nothing");
}

async fn ddl_run_by_hand_then_written_is_recorded(mut p: P) {
    let (int, text) = (p.t.int, p.t.text);
    p.schema(&format!("create table users (id {int});"));
    p.file("20261027100000_users.sql", &format!("create table users (id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec(&format!("alter table users add column nick {text}")).await;
    p.file("20261027110000_nick.sql", &format!("alter table users add column nick {text};"));
    p.schema(&format!("create table users (id {int}, nick {text});"));
    // Never recorded by deploy, which refuses it; check says the migration makes the change, and how to
    // record it.
    assert_eq!(p.migrate().await, 1);
    p.output();
    assert_eq!(p.dry_run().await, 1);
    assert_has!(p.output(), r"20261027110000_nick\.sql makes exactly this change: record it here, running nothing:\n    reylo migrate history push");
    assert_eq!(p.history_push().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"\+ 20261027110000_nick\.sql +recorded: already in place[\s\S]*✓ Recorded 1 migration, running nothing");
    assert_eq!(p.history().await, ["20261027100000_users.sql", "20261027110000_nick.sql"]);
    // One that also changes data is recorded where the schema needs it, and says its data never ran
    // (MySQL keeps a DDL statement in a file of its own).
    p.exec(&format!("alter table users add column score {int}")).await;
    let score = if p.d == Dialect::Mysql { format!("alter table users add column score {int};") } else { format!("alter table users add column score {int}; update users set score = 0;") };
    p.file("20261027120000_score.sql", &score);
    p.schema(&format!("create table users (id {int}, nick {text}, score {int});"));
    assert_eq!(p.history_push().await, 0, "{}", p.peek());
    if p.d != Dialect::Mysql {
        assert_has!(p.output(), r"\+ 20261027120000_score\.sql +recorded: its data statements \([^)]*\) may not have run here");
    }
    // One that only changes data, at the end, can't be told to have run: it stays pending, to run.
    p.file("20261027130000_seed.sql", "insert into users (id) values (1);");
    // And one the database doesn't have stays pending too.
    p.file("20261027140000_bio.sql", &format!("alter table users add column bio {text};"));
    p.schema(&format!("create table users (id {int}, nick {text}, score {int}, bio {text});"));
    assert_eq!(p.history_push().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"» This database hasn't run 20261027130000_seed\.sql, 20261027140000_bio\.sql\n  To run them:\n    reylo migrate deploy");
    assert_eq!(p.history().await.len(), 3);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.rows("select id from users").await.len(), 1);
}

async fn a_change_by_hand_with_schema_sql_is_told_to_write_the_migration(mut p: P) {
    let (int, text) = (p.t.int, p.t.text);
    p.schema(&format!("create table users (id {int});"));
    p.file("20261027100000_users.sql", &format!("create table users (id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec(&format!("alter table users add column nick {text}")).await;
    p.schema(&format!("create table users (id {int}, nick {text});"));
    assert_eq!(p.dry_run().await, 1);
    assert_has!(p.output(), r"This database was changed outside its migrations[\s\S]*reylo migrate new <name>");
}

async fn init_writes_and_records_the_baseline(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    p.exec(&format!("create table users (id {id}, email {text} not null); create table posts (id {int}, user_id {int} references users (id)); create view emails as select email from users")).await;
    assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.files(), ["init.sql"]);
    assert_eq!(p.history().await, ["init.sql"]);
    // A fresh database runs init.sql; the next migration applies on both.
    p.file("20261028100000_nick.sql", &format!("alter table users add column nick {text};"));
    let schema = std::fs::read_to_string(p.dir.join("schema.sql")).unwrap();
    p.schema(&format!("{schema}\nalter table users add column nick {text};\n"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    let q = P::new(p.d).await.unwrap();
    p.url = q.url.clone();
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(q.history().await, ["init.sql", "20261028100000_nick.sql"]);
}

/// Another migration tool's history: a table of Prisma Migrate's with a check, one of Knex's whose
/// key is a counter (a sequence the table owns on Postgres), Liquibase's lock in its capitals, and
/// on Postgres Drizzle Kit's, in a schema of its own, and on MySQL a trigger on Prisma's.
fn other_tools_history(p: &P) -> String {
    let text = p.t.text;
    let knex_id = match p.d {
        Dialect::Postgres => "serial primary key",
        Dialect::Mysql => "bigint auto_increment primary key",
        Dialect::Sqlite => "integer primary key autoincrement",
    };
    let mut sql = format!(
        "create table _prisma_migrations (id {text} primary key, migration_name {text} not null check (migration_name <> '')); insert into _prisma_migrations values ('a', '0_init');
         create table knex_migrations (id {knex_id}, name {text}); insert into knex_migrations (id, name) values (100, 'one.js');
         create table DATABASECHANGELOGLOCK (ID integer primary key, LOCKED boolean not null); insert into DATABASECHANGELOGLOCK values (1, false);"
    );
    match p.d {
        Dialect::Postgres => sql.push_str("create schema drizzle; create table drizzle.__drizzle_migrations (id serial primary key, hash text not null); insert into drizzle.__drizzle_migrations (hash) values ('h'); select setval('knex_migrations_id_seq', 100);"),
        Dialect::Mysql => sql.push_str("create trigger prisma_named before insert on _prisma_migrations for each row set new.migration_name = trim(new.migration_name);"),
        Dialect::Sqlite => {}
    }
    sql
}

async fn init_leaves_out_another_tools_history(p: P) {
    let (id, text) = (p.t.id, p.t.text);
    leaves_out_another_tools_history(p, &format!("create table users (id {id}, email {text} not null);")).await;
}

/// pg_dump's schema, where Reylo's own SQL can't be checked, leaves the other tool's tables out
/// too, and the sequences they own.
#[tokio::test(flavor = "current_thread")]
async fn the_dump_leaves_out_another_tools_history() {
    let Some(mut p) = P::new(Dialect::Postgres).await else { return };
    with_no_shadows(&mut p).await;
    p.exec(&format!("create table users (id bigint primary key, email text not null);{}", other_tools_history(&p))).await;
    let text = introspect(&p.opts()).await;
    assert!(text.contains("CREATE TABLE public.users"), "{text}");
    assert!(!has(&text, "(?i)prisma|knex|drizzle|databasechangelog"), "{text}");
}

async fn leaves_out_another_tools_history(mut p: P, app: &str) {
    p.exec(&format!("{app}{}", other_tools_history(&p))).await;
    assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
    p.output();
    let schema = std::fs::read_to_string(p.dir.join("schema.sql")).unwrap();
    assert!(has(&schema, "(?i)create table .*users"), "{schema}");
    assert!(!has(&schema, "(?i)prisma|knex|drizzle|databasechangelog"), "{schema}");
    assert_eq!(std::fs::read_to_string(p.dir.join("migrations/init.sql")).unwrap(), schema);
    assert!(!has(&introspect(&p.opts()).await, "(?i)prisma|knex|drizzle|databasechangelog"));
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    // Another database that still holds the old tool's history matches init.sql, and keeps it.
    let q = P::new(p.d).await.unwrap();
    q.exec(&format!("{app}{}", other_tools_history(&q))).await;
    p.url = q.url.clone();
    assert_eq!(p.history_push().await, 0, "{}", p.peek());
    assert_eq!(q.history().await, ["init.sql"]);
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    // A wipe empties the application's tables alone, and leaves the other tool's counter where it was.
    assert_eq!(p.run(async |o, log| reylo_migrate::db_wipe(o, false, log).await).await, 0, "{}", p.peek());
    assert_eq!(q.rows("select * from _prisma_migrations").await.len(), 1);
    assert_eq!(q.rows("select * from DATABASECHANGELOGLOCK").await.len(), 1);
    q.exec("insert into knex_migrations (name) values ('two.js')").await;
    assert_eq!(q.rows("select id from knex_migrations where name = 'two.js'").await[0].text("id"), "101");
    // A migration that drops it later is no change to the schema, and replays on a database that
    // never had it.
    p.file("20261123100000_drop_prisma.sql", "drop table if exists _prisma_migrations;");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    assert_eq!(q.history().await, ["init.sql", "20261123100000_drop_prisma.sql"]);
    // Nor is dropping a schema that held nothing but another tool's history.
    if p.d == Dialect::Postgres {
        q.exec("drop table if exists drizzle.__drizzle_migrations; drop schema if exists drizzle;").await;
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }
}

async fn a_migration_the_lock_lacks_is_a_draft(mut p: P) {
    let int = p.t.int;
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    p.file("20261027120000_c.sql", &format!("create table c (id {int});"));
    p.schema(&format!("create table a (id {int}); create table c (id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.schema(&format!("create table a (id {int}); create table b (id {int}); create table c (id {int});"));
    p.file("20261027110000_b.sql", &format!("create table b (id {int});"));
    // Locked, the lock's order is the order, whatever the names: b is a draft until development
    // locks it after c.
    p.linear();
    p.lock(&["20261027100000_a.sql", "20261027120000_c.sql"]);
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), r"The lock doesn't list 20261027110000_b\.sql yet\n\n  Run it in development, which locks it:\n    reylo migrate dev");
}

async fn status_reports_a_history_out_of_the_locks_order(mut p: P) {
    let int = p.t.int;
    let (a, tags, status) = ("20261027100000_a.sql", "20261027110000_tags.sql", "20261027120000_status.sql");
    p.linear();
    p.file(a, &format!("create table a (id {int});"));
    p.file(status, &format!("alter table a add column status {int};"));
    p.lock(&[a, status]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    // Main gained a migration named earlier, and the merge kept main's lines: the lock now runs it
    // before the one this database already ran.
    p.file(tags, &format!("alter table a add column tags {int};"));
    p.lock(&[a, tags, status]);
    p.output();
    let refusal = r"✗ This database applied 20261027120000_status\.sql where \S*reylo\.lock lists 20261027110000_tags\.sql\n  Every database runs the migrations in the lock's order, and this one ran 20261027120000_status\.sql without 20261027110000_tags\.sql, which the lock runs first\.";
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), refusal);
    // Status says the same, and doesn't offer the deploy that refuses.
    assert_eq!(p.status().await, 1);
    let out = p.output();
    assert_has!(&out, refusal);
    assert!(!out.contains("To run it in production"), "{out}");
}

async fn a_rebased_migration_is_rewound_by_dev(mut p: P) {
    let int = p.t.int;
    p.schema(&format!("create table a (id {int}); create table mine (id {int});"));
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    p.file("20261027110000_mine.sql", &format!("create table mine (id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    // Main merged a migration from another branch, newer than mine.
    p.file("20261027120000_theirs.sql", &format!("create table theirs (id {int});"));
    p.schema(&format!("create table a (id {int}); create table mine (id {int}); create table theirs (id {int});"));
    std::fs::rename(p.dir.join("migrations/20261027110000_mine.sql"), p.dir.join("migrations/20261027130000_mine.sql")).unwrap();
    let files = p.files();
    assert_eq!(files[1], "20261027120000_theirs.sql");
    assert!(has(&files[2], r"^\d{14}_mine\.sql$") && files[2].as_str() > "20261027120000", "{files:?}");
    p.output();
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), "which isn't in");
    p.exec("insert into a values (1)").await;
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_eq!(p.history().await, ["20261027100000_a.sql".to_string(), "20261027120000_theirs.sql".to_string(), files[2].clone()]);
    assert_eq!(p.rows("select * from a").await.len(), 1, "a rewind leaves what it doesn't undo");
}

async fn history_pull_writes_the_lock_and_keeps_what_is_still_to_deploy(mut p: P) {
    let int = p.t.int;
    let (a, b) = ("20261027100000_a.sql", "20261027110000_b.sql");
    p.linear();
    p.file(a, &format!("create table a (id {int});"));
    p.schema(&format!("create table a (id {int});"));
    p.lock(&[a]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.output();
    // b is locked, on its way to this database, which hasn't run it.
    p.file(b, &format!("create table b (id {int});"));
    p.lock(&[a, b]);
    assert_eq!(p.run(async |o, log| reylo_migrate::history_pull(o, log).await).await, 0, "{}", p.peek());
    assert!(!p.peek().contains("Wrote"), "{}", p.peek());
    assert_eq!(p.locked(), [a, b]);
    p.output();
    // A lock lost turns linear history off: history pull keeps it off, and reylo migrate lock writes
    // what this database ran; b is left as it is, a draft again.
    std::fs::remove_file(p.dir.join(reylo_migrate::lock::LOCK)).unwrap();
    assert_eq!(p.run(async |o, log| reylo_migrate::history_pull(o, log).await).await, 0, "{}", p.peek());
    assert_has!(p.output(), r"Not run on this database, and left as they are: 20261027110000_b\.sql");
    assert!(!p.dir.join(reylo_migrate::lock::LOCK).exists());
    assert_eq!(p.run(async |o, log| reylo_migrate::lock_from_history(o, log).await).await, 0, "{}", p.peek());
    assert_has!(p.output(), r"✓ Wrote \S*reylo\.lock: 1 migration");
    assert_eq!(p.locked(), [a]);
}

async fn db_introspect_prints_the_schema(p: P) {
    let int = p.t.int;
    // A SQLite file is made by opening it: introspect refuses one that isn't there.
    p.exec("select 1").await;
    assert_eq!(introspect(&p.opts()).await, "", "an empty database has no schema");
    p.exec(&format!("create table hand_made (id {int});")).await;
    let text = introspect(&p.opts()).await;
    assert!(has(&text, "(?i)create table .*hand_made"), "{text}");
    assert!(!p.dir.join("schema.sql").exists(), "it writes no file");
}

async fn a_hotfix_by_hand_is_recorded_once_it_is_a_migration(mut p: P) {
    let (int, text) = (p.t.int, p.t.text);
    p.schema(&format!("create table users (id {int}, email {text});"));
    p.file("20261027100000_users.sql", &format!("create table users (id {int}, email {text});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into users values (1, 'a@b.co')").await;
    p.exec("create index users_email on users (email)").await;
    p.output();
    assert_eq!(p.dry_run().await, 1, "drift until the hotfix is a migration");
    assert_has!(p.output(), "This database was changed outside its migrations");
    p.file("20261027110000_hotfix.sql", "create index users_email on users (email);");
    // Recorded once schema.sql has it too: everything agrees but the history it writes.
    assert_eq!(p.history_push().await, 1);
    assert_has!(p.output(), r"The migrations make what \S*schema\.sql doesn't declare[\s\S]*users_email[\s\S]*Nothing was changed\.");
    assert_eq!(p.history().await, ["20261027100000_users.sql"]);
    p.schema(&format!("create table users (id {int}, email {text}); create index users_email on users (email);"));
    assert_eq!(p.dry_run().await, 1, "recorded only when told");
    assert_has!(p.output(), r"20261027110000_hotfix\.sql makes exactly this change[^\n]*\n    reylo migrate history push");
    assert_eq!(p.history_push().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"\+ 20261027110000_hotfix\.sql +recorded");
    assert_eq!(p.history().await, ["20261027100000_users.sql", "20261027110000_hotfix.sql"]);
    assert_eq!(p.rows("select * from users").await.len(), 1);
}

async fn a_hotfix_behind_a_pending_migration_moves_ahead_of_it(mut p: P) {
    let (int, text) = (p.t.int, p.t.text);
    p.linear();
    let table = format!("create table users (id {int}, email {text}, nick {text});");
    let (users, backfill, nick, email) = ("20261027100000_users.sql", "20261027110000_backfill.sql", "20261027120000_nick_index.sql", "20261027130000_email_index.sql");
    p.schema(&table);
    p.file(users, &table);
    p.file(backfill, "update users set nick = email where nick is null;");
    p.lock(&[users, backfill]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into users values (1, 'a@b.co', null)").await;
    p.exec("create index users_email on users (email)").await;
    p.output();
    // The run named is the one this database ran, which the data migration ends: as close as the
    // run before it, which leaves the same schema.
    assert_eq!(p.history_push().await, 1);
    assert_has!(p.output(), r"✗ No run of the migrations makes this database's schema\n  The closest is after 20261027110000_backfill\.sql, which differs by:\n\n    [^\n]*users_email");
    // Development moves on, and the change made by hand is written into schema.sql and its migration,
    // which the lock runs after the one this database hasn't run.
    p.file(nick, "create index users_nick on users (nick);");
    p.schema(&format!("{table} create index users_nick on users (nick); create index users_email on users (email);"));
    p.file(email, "create index users_email on users (email);");
    p.lock(&[users, backfill, nick, email]);
    assert_eq!(p.dry_run().await, 1);
    assert_has!(p.output(), r"✗ This database was changed outside its migrations\n\n(?:    [^\n]*\n)*    [^\n]*users_email[^\n]*\n(?:    [^\n]*\n)*\n  Migration 20261027130000_email_index\.sql makes exactly this change: record it here, running nothing, and deploy runs 20261027120000_nick_index\.sql after it:\n    reylo migrate history push");
    // Recorded where this database ran it, ahead of the one it hasn't run, which the lock now runs
    // after it. Nothing is undone, and deploy runs the rest.
    assert_eq!(p.history_push().await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"\+ 20261027130000_email_index\.sql +recorded: already in place\n✓ Recorded 1 migration, running nothing\n✓ Wrote \S*reylo\.lock: 20261027130000_email_index\.sql now runs before 20261027120000_nick_index\.sql, as it did here\n» Development databases ran 20261027120000_nick_index\.sql first\n  To record the lock's new order on one, running nothing:\n    reylo migrate history push\n» This database hasn't run 20261027120000_nick_index\.sql\n  To run it:\n    reylo migrate deploy");
    assert_eq!(p.locked(), [users, backfill, email, nick]);
    assert_eq!(p.history().await, [users, backfill, email]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.history().await, [users, backfill, email, nick]);
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    // A migration that changes data, ahead of the one that makes a change by hand, is never recorded
    // by the schema the two leave, which the later one makes alone, nor moved past, since its
    // statements can run differently on the rows after the change: the change is undone, and it runs.
    let (refill, both) = ("20261027140000_refill.sql", "20261027150000_both_index.sql");
    p.file(refill, "update users set nick = 'x' where nick is null;");
    p.exec("create index users_both on users (email, nick)").await;
    p.schema(&format!("{table} create index users_nick on users (nick); create index users_email on users (email); create index users_both on users (email, nick);"));
    p.file(both, "create index users_both on users (email, nick);");
    p.lock(&[users, backfill, email, nick, refill, both]);
    p.output();
    let refused = r"✗ Migration 20261027150000_both_index\.sql makes this change, but runs after 20261027140000_refill\.sql, which this database hasn't run\n  Migration 20261027140000_refill\.sql changes data, which can run differently after this change, so it can't move ahead of it\.\n  It ran the migrations through 20261027120000_nick_index\.sql,";
    assert_eq!(p.history_push().await, 1);
    let out = p.output();
    assert_has!(&out, refused);
    assert_eq!(p.dry_run().await, 1);
    assert_has!(p.output(), refused);
    assert_eq!(p.locked(), [users, backfill, email, nick, refill, both]);
    assert_eq!(p.history().await, [users, backfill, email, nick]);
    p.exec(&undo_on(&out)).await;
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.history().await, [users, backfill, email, nick, refill, both]);
    let rows = p.rows("select nick from users").await;
    assert_eq!(rows.iter().map(|r| r.text("nick").to_string()).collect::<Vec<_>>(), ["x"]);
}

async fn history_push_takes_a_database_to_a_reordered_lock_keeping_its_rows(mut p: P) {
    let int = p.t.int;
    p.linear();
    let (a, notes, index) = ("20261027100000_a.sql", "20261027110000_notes.sql", "20261027120000_a_index.sql");
    p.schema(&format!("create table a (id {int}); create table notes (id {int}); create index a_id on a (id);"));
    p.file(a, &format!("create table a (id {int});"));
    p.file(notes, &format!("create table notes (id {int});"));
    p.file(index, "create index a_id on a (id);");
    p.lock(&[a, notes, index]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into notes values (1)").await;
    // Another database moved the index ahead, as it ran it: this one, which ran the old order, has
    // what the new one makes, and takes its order with nothing run or rewound.
    p.lock(&[a, index, notes]);
    p.output();
    assert_eq!(p.history_push().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"✓ The history now lists 3 migrations, in the order they run");
    assert_eq!(p.history().await, [a, index, notes]);
    assert_eq!(p.rows("select * from notes").await.len(), 1);
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
}

async fn a_database_that_skipped_a_migration_runs_it_after_the_rest(mut p: P) {
    let int = p.t.int;
    p.linear();
    let (a, index, c, row) = ("20261027100000_a.sql", "20261027110000_a_index.sql", "20261027120000_c.sql", "20261027130000_c_row.sql");
    p.schema(&format!("create table a (id {int}); create index a_id on a (id); create table c (id {int});"));
    p.file(a, &format!("create table a (id {int});"));
    p.file(index, "create index a_id on a (id);");
    p.file(c, &format!("create table c (id {int});"));
    p.file(row, "insert into c values (1);");
    p.lock(&[a, index, c, row]);
    // Another tool ran the migrations but the index, and maybe the row, and kept no history Reylo
    // reads: the tables are recorded, and the index moves after them in the lock, for deploy to run
    // with the row, which no schema shows ran.
    p.exec(&format!("create table a (id {int}); create table c (id {int});")).await;
    p.output();
    assert_eq!(p.history_push().await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"\+ 20261027100000_a\.sql +recorded: already in place\n *\+ 20261027120000_c\.sql +recorded: already in place\n✓ Recorded 2 migrations, running nothing\n✓ Wrote \S*reylo\.lock: 20261027110000_a_index\.sql now runs after 20261027120000_c\.sql, which this database ran without it\n» Development databases ran 20261027110000_a_index\.sql first\n  To record the lock's new order on one, running nothing:\n    reylo migrate history push\n» This database hasn't run 20261027110000_a_index\.sql, 20261027130000_c_row\.sql\n  To run them:\n    reylo migrate deploy");
    assert_eq!(p.locked(), [a, c, index, row]);
    assert_eq!(p.history().await, [a, c]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.history().await, [a, c, index, row]);
    assert_eq!(p.rows("select id from c").await.len(), 1);
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    // A database that ran them in the other order, as development did before the lock moved, is told
    // to record the lock's, which it takes running nothing.
    p.lock(&[a, index, c, row]);
    p.output();
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), r"✗ This database applied 20261027120000_c\.sql where \S*reylo\.lock lists 20261027110000_a_index\.sql\n  Every database runs the migrations in the lock's order, and this one ran the same migrations in another\.\n\n  On a development database, record the lock's order, running nothing:\n    reylo migrate history push\n  On any other, do the same:\n    reylo migrate history push --url <url>");
    assert_eq!(p.history_push().await, 0, "{}", p.peek());
    assert_eq!(p.history().await, [a, index, c, row]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.rows("select id from c").await.len(), 1);
}

async fn a_skipped_migration_moves_past_no_data_change(mut p: P) {
    let int = p.t.int;
    p.linear();
    let (a, index, fill, c) = ("20261027100000_a.sql", "20261027110000_a_index.sql", "20261027120000_fill.sql", "20261027130000_c.sql");
    p.file(a, &format!("create table a (id {int}, n {int});"));
    p.file(index, "create index a_id on a (id);");
    p.file(fill, "update a set n = 1 where n is null;");
    p.file(c, &format!("create table c (id {int});"));
    p.lock(&[a, index, fill, c]);
    // The data migration that ran here without the index can run differently once it is there, so
    // the index isn't moved after it: it is made here, and all four recorded in their order.
    p.exec(&format!("create table a (id {int}, n {int}); create table c (id {int});")).await;
    p.output();
    assert_eq!(p.history_push().await, 1);
    assert_has!(p.output(), r"✗ This database has what the migrations make without 20261027110000_a_index\.sql\n  It ran 20261027120000_fill\.sql and 20261027130000_c\.sql without it\. Migration 20261027120000_fill\.sql changes data, which can run differently without it, so it can't move after them\.\n\n  Make the change 20261027110000_a_index\.sql makes here, then redo what 20261027120000_fill\.sql wrote or built as it would after it, and record them all, running nothing:\n    reylo migrate history push --url <url>");
    assert_eq!(p.locked(), [a, index, fill, c]);
    assert!(p.history().await.is_empty());
    p.exec("create index a_id on a (id)").await;
    assert_eq!(p.history_push().await, 0, "{}", p.peek());
    assert_eq!(p.history().await, [a, index, fill, c]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
}

async fn a_hotfix_whose_migration_changes_data_is_undone_then_deployed(mut p: P) {
    // On MySQL each migration holds one change, so no migration both indexes and updates.
    if p.d == Dialect::Mysql {
        return;
    }
    let (int, text) = (p.t.int, p.t.text);
    p.linear();
    let table = format!("create table users (id {int}, email {text}, nick {text});");
    let (users, nick, email) = ("20261027100000_users.sql", "20261027110000_nick_index.sql", "20261027120000_email_index.sql");
    p.schema(&format!("{table} create index users_nick on users (nick); create index users_email on users (email);"));
    p.file(users, &table);
    p.lock(&[users]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into users values (1, 'a@b.co', null)").await;
    p.exec("create index users_email on users (email)").await;
    // The migration that makes the index made by hand also fills a column, which no schema shows ran.
    p.file(nick, "create index users_nick on users (nick);");
    p.file(email, "create index users_email on users (email); update users set nick = 'y' where nick is null;");
    p.lock(&[users, nick, email]);
    p.output();
    let refused = r"✗ Migration 20261027120000_email_index\.sql makes this change, but runs after 20261027110000_nick_index\.sql, which this database hasn't run\n  Its data statements can't be told to have run here, so it isn't recorded without running\.";
    assert_eq!(p.history_push().await, 1);
    let out = p.output();
    assert_has!(&out, refused);
    // The undo takes back the index alone: what the update wrote, where it ran here too, is the
    // operator's to take back, or deploy writes it again.
    assert_has!(&out, r"  The undo reverses the schema alone: where update users set… also ran here, reverse what it wrote first, or deploy writes it again\.\n  Give both commands");
    assert_eq!(p.dry_run().await, 1);
    assert_has!(p.output(), refused);
    p.exec(&undo_on(&out)).await;
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.history().await, [users, nick, email]);
    let rows = p.rows("select nick from users").await;
    assert_eq!(rows.iter().map(|r| r.text("nick").to_string()).collect::<Vec<_>>(), ["y"]);
}

async fn a_hotfix_moves_past_no_table_made_from_a_query_and_is_none(mut p: P) {
    let int = p.t.int;
    p.linear();
    let (a, snap, index) = ("20261027100000_a.sql", "20261027110000_snap.sql", "20261027120000_a_index.sql");
    p.file(a, &format!("create table a (id {int});"));
    p.schema(&format!("create table a (id {int}); create table snap as select id from a; create index a_id on a (id);"));
    p.lock(&[a]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into a values (1), (2)").await;
    // A table made from a query holds what it read when it ran: it is never moved behind a change,
    // nor recorded by its schema alone.
    p.exec("create index a_id on a (id)").await;
    p.file(snap, "create table snap as select id from a;");
    p.file(index, "create index a_id on a (id);");
    p.lock(&[a, snap, index]);
    p.output();
    assert_eq!(p.history_push().await, 1);
    let out = p.output();
    assert_has!(&out, r"✗ Migration 20261027120000_a_index\.sql makes this change, but runs after 20261027110000_snap\.sql, which this database hasn't run\n  Migration 20261027110000_snap\.sql changes data, which can run differently after this change, so it can't move ahead of it\.");
    p.exec(&undo_on(&out)).await;
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.history().await, [a, snap, index]);
    assert_eq!(p.rows("select * from snap").await.len(), 2);
    // And one made by hand, empty, is not the table the migration fills.
    let (b, copy) = ("20261027130000_b.sql", "20261027140000_copy.sql");
    // As each database types a column a query makes: SQLite by its affinity alone.
    let made = if p.d == Dialect::Sqlite { "INT" } else { int };
    p.exec(&format!("create table copy (id {made})")).await;
    p.file(b, &format!("create table b (id {int});"));
    // MySQL's way to write one with no `as`, a query in the parentheses.
    let fill = if p.d == Dialect::Mysql { "create table copy (select id from a);" } else { "create table copy as select id from a;" };
    p.file(copy, fill);
    p.schema(&format!("create table a (id {int}); create table snap as select id from a; create index a_id on a (id); create table b (id {int}); {fill}"));
    p.lock(&[a, snap, index, b, copy]);
    p.output();
    assert_eq!(p.history_push().await, 1);
    let out = p.output();
    assert_has!(&out, r"✗ Migration 20261027140000_copy\.sql makes this change, but runs after 20261027130000_b\.sql, which this database hasn't run\n  Its data statements can't be told to have run here, so it isn't recorded without running\.");
    assert_has!(&out, r"The undo reverses the schema alone: where create table copy… also ran here");
    // SQLite's undo drops a table, which reylo run, with foreign keys on, would do otherwise than a
    // migration: it is left to do by hand.
    if p.d == Dialect::Sqlite {
        assert_has!(&out, r"  Undo it on \S+ by hand, then give deploy the same URL as this command\.\n\n  Then deploy, which runs 20261027130000_b\.sql first, and then 20261027140000_copy\.sql:\n    reylo migrate deploy --url <url>");
        p.exec("drop table copy").await;
    } else {
        p.exec(&undo_on(&out)).await;
    }
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.rows("select * from copy").await.len(), 2);
    // A query's word in a string, or a column's name (SQLite's `with`), makes no table from a query:
    // the table made by hand is recorded ahead of the one this database hasn't run, its row kept.
    let (c, notes) = ("20261027150000_c.sql", "20261027160000_notes.sql");
    let text = p.t.text;
    let id = if p.d == Dialect::Sqlite { "with" } else { "id" };
    let table = format!("create table notes ({id} {int}, body {text} default 'select')");
    p.exec(&table).await;
    p.exec(&format!("insert into notes ({id}) values (1)")).await;
    p.file(c, &format!("create table c (id {int});"));
    p.file(notes, &format!("{table};"));
    p.schema(&format!("create table a (id {int}); create table snap as select id from a; create index a_id on a (id); create table b (id {int}); {fill} create table c (id {int}); {table};"));
    p.lock(&[a, snap, index, b, copy, c, notes]);
    p.output();
    assert_eq!(p.history_push().await, 0, "{}", p.peek());
    assert_eq!(p.locked(), [a, snap, index, b, copy, notes, c]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.history().await, [a, snap, index, b, copy, notes, c]);
    assert_eq!(p.rows("select * from notes").await.len(), 1);
}

async fn a_server_that_ran_what_a_hotfix_moved_ahead_of_takes_its_change(mut p: P) {
    let (int, text) = (p.t.int, p.t.text);
    p.linear();
    let table = format!("create table users (id {int}, email {text}, nick {text});");
    let (users, nick, email) = ("20261027100000_users.sql", "20261027120000_nick_index.sql", "20261027130000_email_index.sql");
    p.schema(&format!("{table} create index users_nick on users (nick); create index users_email on users (email);"));
    p.file(users, &table);
    p.file(nick, "create index users_nick on users (nick);");
    p.lock(&[users, nick]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into users values (1, 'a@b.co', null)").await;
    // Production's history push moved the hotfix's migration ahead of the one this server already ran.
    p.file(email, "create index users_email on users (email);");
    p.lock(&[users, email, nick]);
    p.output();
    let refused = r"✗ This database applied 20261027120000_nick_index\.sql where \S*reylo\.lock lists 20261027130000_email_index\.sql\n  Every database runs the migrations in the lock's order, and this one ran 20261027120000_nick_index\.sql without 20261027130000_email_index\.sql, which the lock runs first\.\n\n  On a development database, apply them in the lock's order:\n    reylo migrate dev\n  On any other, make its change there too, then record both, running nothing:\n    reylo run --url <url> --file \S+20261027130000_email_index\.sql\n    reylo migrate history push --url <url>";
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), refused);
    assert_eq!(p.status().await, 1);
    assert_has!(p.output(), refused);
    // Its change made here too, as the command runs the file, both are recorded in the lock's order.
    p.exec("create index users_email on users (email);").await;
    assert_eq!(p.history_push().await, 0, "{}", p.peek());
    assert_eq!(p.history().await, [users, email, nick]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), "Up to date");
    assert_eq!(p.rows("select * from users").await.len(), 1);
}

async fn a_hotfix_that_cannot_move_ahead_is_undone_then_deployed(mut p: P) {
    let int = p.t.int;
    p.linear();
    let (a, t, wider) = ("20261027100000_a.sql", "20261027110000_t.sql", "20261027120000_wider.sql");
    p.schema(&format!("create table a (id {int}); create table t (id {int});"));
    p.file(a, &format!("create table a (id {int});"));
    p.lock(&[a]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec(&format!("create table t (id {int}, x {int})")).await;
    // The migration that makes the change by hand does nothing after the one ahead of it, which fails
    // after it: the order can't move, and the change is undone, for deploy to run both.
    p.file(t, &format!("create table t (id {int});"));
    p.file(wider, &format!("create table if not exists t (id {int}, x {int});"));
    p.lock(&[a, t, wider]);
    p.output();
    let title = r"✗ Migration 20261027120000_wider\.sql makes this change, but runs after 20261027110000_t\.sql, which this database hasn't run\n  Run ahead of 20261027110000_t\.sql, the migrations end differently, so it can't move there\.\n  It ran the migrations through 20261027100000_a\.sql, and then had this change made outside them:";
    assert_eq!(p.history_push().await, 1);
    let out = p.output();
    assert_has!(&out, title);
    if p.d == Dialect::Sqlite {
        assert_has!(&out, r"  Undoing it loses what \S*t holds\.\n  Undo it on \S+ by hand, then give deploy the same URL as this command\.\n\n  Then deploy, which runs 20261027110000_t\.sql first, and then 20261027120000_wider\.sql:\n    reylo migrate deploy --url <url>\n\nNothing was changed\.");
    } else {
        assert_has!(&out, r"  Undoing it loses what \S*t holds\.\n  Give both commands the same URL as this one\.\n\n  Undo it on \S+, so that deploy runs 20261027110000_t\.sql first, and then 20261027120000_wider\.sql:\n    reylo run --url <url> '[^\n]*'\n    reylo migrate deploy --url <url>\n\nNothing was changed\.");
    }
    // The lead names this database, which a command without --url may not reach, as every command
    // shows one: with no credential or option, which a log keeps.
    assert!(out.contains(&format!("Undo it on {} ", reylo_migrate::db::shown_url(&p.url))) || out.contains(&format!("Undo it on {},", reylo_migrate::db::shown_url(&p.url))), "{out}");
    assert_eq!(p.locked(), [a, t, wider]);
    assert_eq!(p.history().await, [a]);
    assert_eq!(p.dry_run().await, 1);
    assert_has!(p.output(), title);
    if p.d == Dialect::Sqlite {
        p.exec("drop table t").await;
    } else {
        p.exec(&undo_on(&out)).await;
    }
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.history().await, [a, t, wider]);
}

/// The SQL of the `reylo run --url <url>` a refusal gives to undo a change on the database it names.
fn undo_on(out: &str) -> String {
    let quoted = regex::Regex::new(r"reylo run --url <url> '((?:[^']|'\\'')*)'").unwrap().captures(out).unwrap_or_else(|| panic!("no undo in:\n{out}"))[1].to_string();
    quoted.replace("'\\''", "'")
}

async fn history_push_follows_a_renamed_migration(mut p: P) {
    let int = p.t.int;
    p.schema(&format!("create table a (id {int}); create table mine (id {int});"));
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    p.file("20261027110000_mine.sql", &format!("create table mine (id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into a values (1)").await;
    std::fs::rename(p.dir.join("migrations/20261027110000_mine.sql"), p.dir.join("migrations/20261027130000_mine.sql")).unwrap();
    p.output();
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), "which isn't in");
    assert_eq!(p.history_push().await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"\+ 20261027130000_mine\.sql +recorded: already in place");
    assert_has!(&out, r"- 20261027110000_mine\.sql +taken out of the history");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), "Up to date");
    assert_eq!(p.rows("select * from a").await.len(), 1);
    // A database whose schema no run of the migrations makes is refused, and nothing changes.
    p.exec(&format!("create table stray (id {int})")).await;
    assert_eq!(p.history_push().await, 1);
    assert_has!(p.output(), r"✗ No run of the migrations makes this database's schema\n  The closest is after 20261027130000_mine\.sql, which differs by:[\s\S]*stray[\s\S]*Nothing was changed\.");
    assert_eq!(p.history().await, ["20261027100000_a.sql", "20261027130000_mine.sql"]);
}

async fn each_run_leaves_its_fingerprint_on_its_newest_row(mut p: P) {
    let (int, text) = (p.t.int, p.t.text);
    p.schema(&format!("create table a (id {int}, note {text});"));
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    p.file("20261027110000_note.sql", &format!("alter table a add column note {text};"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    // Read once, after the run's last migration: only the newest row's is compared, and the one before
    // keeps its pending mark, which says nothing of the schema now. MySQL commits each statement, so a
    // migration there may leave part of itself, and the schema is read before each.
    let fingerprints = async |p: &P| p.rows("select name, fingerprint from __reylo_migrations order by name").await.iter().map(|r| r.get("fingerprint").unwrap_or_default().to_string()).collect::<Vec<_>>();
    let first = fingerprints(&p).await;
    assert!(first[0].starts_with("pending") == (p.d != Dialect::Mysql) && !first[1].starts_with("pending"), "{first:?}");
    p.schema(&format!("create table a (id {int}, note {text}); create table b (id {int});"));
    p.file("20261027120000_b.sql", &format!("create table b (id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    let second = fingerprints(&p).await;
    assert!(!second[2].starts_with("pending") && second[2] != second[1], "{second:?}");
}

async fn a_failed_migration_leaves_the_fingerprint_of_those_before_it(mut p: P) {
    let int = p.t.int;
    p.schema(&format!("create table a (id {int});"));
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    if p.d == Dialect::Mysql {
        return;
    }
    // Fine in the shadows, where `a` is empty; fails on the live database, which has a row.
    p.exec("insert into a values (null)").await;
    let (change, column) = if p.d == Dialect::Postgres { ("alter column id set not null", " not null") } else { ("add column x integer not null", ", x integer not null") };
    p.file("20261027110000_b.sql", &format!("create table b (id {int});"));
    p.file("20261027120000_c.sql", &format!("alter table a {change};"));
    p.schema(&format!("create table a (id {int}{column}); create table b (id {int});"));
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    assert_eq!(p.history().await, ["20261027100000_a.sql", "20261027110000_b.sql"]);
    // c rolled back, so the schema is the one b left, and b's row holds its fingerprint: the next run
    // compares with it, and goes on.
    let rows = p.rows("select name from __reylo_migrations where fingerprint not like 'pending%'").await;
    assert_eq!(rows.iter().map(|r| r.text("name")).collect::<Vec<_>>(), ["20261027100000_a.sql", "20261027110000_b.sql"]);
    p.exec("delete from a").await;
    p.output();
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), "Applied 1 migration");
}

async fn a_run_cut_off_before_its_fingerprint_is_refused(mut p: P) {
    let int = p.t.int;
    p.schema(&format!("create table a (id {int});"));
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    // As the row reads where a run was cut off after the migration committed, before the
    // fingerprint was read.
    p.exec("update __reylo_migrations set fingerprint = 'pending'").await;
    p.output();
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), r"✗ A run was cut off after 20261027100000_a\.sql applied, before it stored this database's fingerprint[\s\S]*reylo migrate deploy --dry-run[\s\S]*reylo migrate history push[\s\S]*Nothing was changed\.");
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"! Deploys refuse this database until its fingerprint is stored again\n  Store it, running nothing:\n    reylo migrate history push");
    assert_eq!(p.history_push().await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), "Up to date");
}

async fn a_failed_migration_leaves_nothing(mut p: P) {
    let int = p.t.int;
    p.schema(&format!("create table a (id {int});"));
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    if p.d == Dialect::Mysql {
        return;
    }
    // Fine in the shadows, where `a` is empty; fails on the live database, which has a row.
    p.exec("insert into a values (null)").await;
    let (change, column) = if p.d == Dialect::Postgres { ("alter column id set not null", " not null") } else { ("add column x integer not null", ", x integer not null") };
    p.file("20261027110000_b.sql", &format!("create table b (id {int}); alter table a {change};"));
    p.schema(&format!("create table a (id {int}{column}); create table b (id {int});"));
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    // Said as every refusal is: what failed, that nothing of it stays, and what to run next. On
    // Postgres, the row that has no value is what failed it, so the way on is that row, which a
    // query lists.
    if p.d == Dialect::Postgres {
        assert_has!(p.output(), r"  ✗ 20261027110000_b\.sql  failed\n\n✗ The migration 20261027110000_b\.sql failed on rows with nulls in a\.id\n  [^\n]+\n  Nothing was recorded, so the next deploy runs it again\.\n\n  List the rows that break it:\n    select \* from a where id is null;\n  Fix them here, then deploy again:\n    reylo migrate deploy\n  If the rows are right and only development ran 20261027110000_b\.sql, change a\.id in [^\n]*schema\.sql and delete it, then take it off development and write the replacement:\n    reylo migrate dev\n    reylo migrate new <name>\n    reylo migrate dev$");
    } else {
        assert_has!(p.output(), r"  ✗ 20261027110000_b\.sql  failed\n\n✗ The migration 20261027110000_b\.sql failed and was rolled back\n  [^\n]+\n  Nothing was recorded, so the next deploy runs it again\.\n\n  Fix the file, lock the fix and deploy again:\n    reylo migrate dev\n    reylo migrate deploy\n  If another database already ran it, write a new migration instead:\n    reylo migrate new <name>$");
    }
    assert!(p.try_rows("select 1 from b").await.is_err(), "b rolled back with the file");
    assert_eq!(p.history().await, ["20261027100000_a.sql"]);
}

/// The query a failure on rows gives, as the refusal shows it under "List the rows that break it".
fn listed_query(out: &str) -> String {
    let at = out.find("  List the rows that break it:\n    ").unwrap_or_else(|| panic!("no query in:\n{out}")) + "  List the rows that break it:\n    ".len();
    out[at..].lines().next().unwrap().to_string()
}

/// A `check` changed on a table whose rows break the new one. On Postgres the change is two
/// migrations, the constraint added `not valid` and then validated, so a deploy stops between them:
/// the first in place, holding new writes, and the second failed. The refusal says so, lists the
/// rows, and says the two ways on; `status` says the database is partway through the change. On
/// MySQL the change is one statement, which the server undoes whole.
async fn a_check_the_rows_break_stops_its_change_partway(mut p: P) {
    if p.d == Dialect::Sqlite {
        return;
    }
    let (int, text) = (p.t.int, p.t.text);
    let base = format!("create table people (id {int} primary key, mood {text} not null, constraint people_mood check (mood in ('ok', 'happy', 'sad')));");
    p.schema(&base);
    p.file("20261027100000_people.sql", &base);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into people values (1, 'ok'), (2, 'happy')").await;
    p.schema(&base.replace("'ok', ", ""));
    if p.d == Dialect::Postgres {
        p.file("20261027110000_remove_ok.sql", "alter table people drop constraint people_mood;\nalter table people add constraint people_mood check (mood in ('happy', 'sad')) not valid;\n");
        // Blank lines before the statement, as a file edited by hand may have.
        p.file("20261027110001_remove_ok_2.sql", "\n\nalter table people validate constraint people_mood;\n");
    } else {
        p.file("20261027110000_remove_ok.sql", "\n\nalter table people drop check people_mood, add constraint people_mood check (mood in ('happy', 'sad'));\n");
    }
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    let out = p.output();
    if p.d == Dialect::Postgres {
        assert_has!(&out, r"  ✓ 20261027110000_remove_ok\.sql +applied[^\n]*\n  ✗ 20261027110001_remove_ok_2\.sql +failed\n\n✗ The migration 20261027110001_remove_ok_2\.sql failed on rows that break people_mood\n  check constraint .people_mood. of relation .people. is violated by some row\n  The migration before it, 20261027110000_remove_ok\.sql, added people_mood without checking the rows already in people: rows written from now on are held to it, and those aren't\.\n  Nothing was recorded, so the next deploy runs it again\.\n\n  List the rows that break it:\n    select \* from people where not \([^\n]+\);\n  Fix them here, then deploy again:\n    reylo migrate deploy\n  If the rows are right and only development ran 20261027110001_remove_ok_2\.sql, change people_mood in [^\n]*schema\.sql and delete it, then take it off development and write the replacement:\n    reylo migrate dev\n    reylo migrate new <name>\n    reylo migrate dev$");
        // The first half holds new writes already.
        assert!(p.try_rows("insert into people values (3, 'ok') returning id").await.is_err());
        assert_eq!(p.status().await, 1);
        assert_has!(p.output(), r"» This database ran part of one change: 20261027110000_remove_ok\.sql, and not 20261027110001_remove_ok_2\.sql\n» Until it runs, rows written to people are held to people_mood, and the rows already there aren't\n» This database hasn't run 20261027110001_remove_ok_2\.sql\n");
    } else {
        assert_has!(&out, r"  ✗ 20261027110000_remove_ok\.sql +failed\n\n✗ The migration 20261027110000_remove_ok\.sql failed on rows that break people_mood\n  Check constraint 'people_mood' is violated\.\n  Nothing was recorded, so the next deploy runs it again\.\n\n  List the rows that break it:\n    select \* from people where not \(mood in \('happy', 'sad'\)\);\n  Fix them here, then deploy again:\n    reylo migrate deploy\n  If the rows are right and only development ran 20261027110000_remove_ok\.sql, change people_mood in [^\n]*schema\.sql and delete it, then take it off development and write the replacement:\n    reylo migrate dev\n    reylo migrate new <name>\n");
        // Undone whole: the old check still takes 'ok'.
        p.exec("insert into people values (3, 'ok')").await;
        p.exec("delete from people where id = 3").await;
    }
    // The query lists the row that breaks it, and no other.
    let ids: Vec<String> = p.rows(&listed_query(&out)).await.iter().map(|r| r.text("id").to_string()).collect();
    assert_eq!(ids, ["1"]);
    // Fixed here, the same migration runs.
    p.exec("update people set mood = 'happy' where id = 1").await;
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
}

/// A failure a migration caused itself is not blamed on the rows: a column it adds that its own
/// NOT NULL or check refuses, and a type change whose `using` rewrites the values a check then
/// refuses. Such a file is to be fixed, and the refusal says so as before.
async fn a_failure_the_file_caused_is_not_blamed_on_the_rows(mut p: P) {
    if p.d == Dialect::Sqlite {
        return;
    }
    let int = p.t.int;
    p.schema(&format!("create table people (id {int} primary key, score {int});"));
    p.file("20261027100000_people.sql", &format!("create table people (id {int} primary key, score {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into people values (1, 5), (2, 7)").await;
    let files: Vec<&str> = if p.d == Dialect::Postgres {
        vec!["alter table people add column nick text not null;\n", "alter table people alter column score type bigint using (-score);\nalter table people add constraint people_score check (score > 0);\n"]
    } else {
        vec!["alter table people add column bonus int not null default 0, add constraint people_bonus check (bonus > 0);\n"]
    };
    for sql in files {
        p.file("20261027110000_change.sql", sql);
        p.output();
        assert_eq!(p.migrate().await, 1, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("on rows") && !out.contains("List the rows"), "{sql}\n{out}");
        assert_has!(&out, r"✗ The migration 20261027110000_change\.sql failed");
    }
}

/// On Postgres: a change that adds two checks validates both in its second file, and status names
/// each while it waits; a constraint an earlier migration of another change added `not valid` is not
/// said to be the work of the migration before; a domain's check, which no table's constraint names,
/// is said as any failure is; a unique constraint added in one transaction, whose index rolled back,
/// leaves no query, and the refusal says to fix the rows that break it; and a unique index that
/// compares by a nondeterministic collation lists the values it takes as equal.
async fn rows_failures_on_postgres_say_only_what_they_know(mut p: P) {
    if p.d != Dialect::Postgres {
        return;
    }
    let base = "create table people (id bigint primary key, a int, b int, extra int, email text);";
    p.schema(base);
    p.file("20261027100000_people.sql", base);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into people values (1, -1, -1, -1, 'a'), (2, 1, 1, 1, 'A')").await;
    p.file("20261027100001_people_check.sql", "alter table people add constraint people_extra check (extra > 0) not valid;\n");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());

    // A constraint the migration before didn't add.
    p.file("20261027110000_extra.sql", "alter table people add column note text;\n");
    p.file("20261027110001_extra_2.sql", "alter table people validate constraint people_extra;\n");
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    assert_has!(p.output(), r"  The constraint people_extra is in place without checking the rows already in people: rows written from now on are held to it, and those aren't\.\n");
    p.exec("update people set extra = 1 where id = 1").await;
    assert_eq!(p.migrate().await, 0, "{}", p.peek());

    // Two checks, validated in one file that opens on blank lines.
    p.file("20261027120000_ab.sql", "alter table people add constraint people_a check (a > 0) not valid;\nalter table people add constraint people_b check (b > 0) not valid;\n");
    p.file("20261027120001_ab_2.sql", "\n\nalter table people validate constraint people_a;\nalter table people validate constraint people_b;\n");
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    assert_eq!(p.status().await, 1);
    assert_has!(p.output(), r"» Until it runs, rows written to people are held to people_a, and the rows already there aren't\n» Until it runs, rows written to people are held to people_b, and the rows already there aren't\n");
    p.exec("update people set a = 1, b = 1 where id = 1").await;
    assert_eq!(p.migrate().await, 0, "{}", p.peek());

    // A domain's check.
    p.file("20261027130000_domain.sql", "create domain positive as int;\nalter table people add column score positive;\n");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("update people set score = -1 where id = 1").await;
    p.file("20261027130001_domain_check.sql", "alter domain positive add constraint positive_check check (value > 0) not valid;\n");
    p.file("20261027130002_domain_check_2.sql", "alter domain positive validate constraint positive_check;\n");
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    let out = p.output();
    assert!(!out.contains("on rows"), "{out}");
    p.exec("update people set score = 1 where id = 1").await;
    assert_eq!(p.migrate().await, 0, "{}", p.peek());

    // A unique constraint in one transaction.
    p.exec("update people set email = 'b' where id = 2").await;
    p.exec("insert into people (id, email) values (3, 'b')").await;
    p.file("20261027140000_unique.sql", "alter table people add constraint people_email_key unique (email);\n");
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    assert_has!(p.output(), r"✗ The migration 20261027140000_unique\.sql failed on rows that break people_email_key\n[^\n]+\n  Nothing was recorded, so the next deploy runs it again\.\n\n  Fix the rows that break it here, then deploy again:\n    reylo migrate deploy\n");
    std::fs::remove_file(p.dir.join("migrations").join("20261027140000_unique.sql")).unwrap();

    // A unique index that compares by a nondeterministic collation, whose duplicates differ in case.
    p.exec("update people set email = 'B' where id = 3").await;
    p.file("20261027150000_collation.sql", "create collation folded (provider = icu, locale = 'und-u-ks-level2', deterministic = false);\n");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.file("20261027150001_folded.sql", "create unique index concurrently people_email_folded on people (email collate folded);\n");
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"    select \(email collate public\.folded\), count\(\*\) from people where \(email collate public\.folded\) is not null group by \(email collate public\.folded\) having count\(\*\) > 1;\n");
    assert_eq!(p.rows(&listed_query(&out)).await.len(), 1);
    std::fs::remove_file(p.dir.join("migrations").join("20261027150001_folded.sql")).unwrap();

    // A key whose operator class compares otherwise than its type (`text_ops` on `citext`): no
    // query, since `group by` would take 'b' and 'B' as one where the index doesn't.
    p.file("20261027160000_citext.sql", "create extension if not exists citext;\nalter table people add column handle citext;\n");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("update people set handle = case id when 1 then 'c' when 2 then 'b' else 'B' end").await;
    p.exec("insert into people (id, handle) values (4, 'c')").await;
    p.file("20261027160001_handle.sql", "create unique index concurrently people_handle on people (handle text_ops);\n");
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"failed on rows that break people_handle\n[\s\S]*  Fix the rows that break it here, then deploy again:\n");
    assert!(!out.contains("List the rows"), "{out}");
    std::fs::remove_file(p.dir.join("migrations").join("20261027160001_handle.sql")).unwrap();
    // The same in an expression, whose type the index's own column has.
    p.file("20261027160001_handle.sql", "create unique index concurrently people_handle on people ((coalesce(handle, ''::citext)) text_ops);\n");
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    let out = p.output();
    assert!(out.contains("failed on rows that break people_handle") && !out.contains("List the rows"), "{out}");
    std::fs::remove_file(p.dir.join("migrations").join("20261027160001_handle.sql")).unwrap();
    // The same through a domain over `citext`, whose equality is its base type's.
    p.file("20261027160001_handle.sql", "create domain handle_text as citext;\nalter table people add column alias handle_text;\n");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("update people set alias = handle").await;
    p.file("20261027160004_alias.sql", "create unique index concurrently people_alias on people (alias text_ops);\n");
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    let out = p.output();
    assert!(out.contains("failed on rows that break people_alias") && !out.contains("List the rows"), "{out}");
    std::fs::remove_file(p.dir.join("migrations").join("20261027160004_alias.sql")).unwrap();
    // A `varchar` key, which takes `text`'s class and its equality, is listed.
    p.file("20261027160002_code.sql", "alter table people add column code varchar(10);\n");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("update people set code = 'x'").await;
    p.file("20261027160003_code_2.sql", "create unique index concurrently people_code on people (code);\n");
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    let out = p.output();
    assert_eq!(p.rows(&listed_query(&out)).await.len(), 1);
    std::fs::remove_file(p.dir.join("migrations").join("20261027160003_code_2.sql")).unwrap();

    // A check dropped and added under its own name in one transaction: after the rollback the
    // catalog holds the old one, whose definition isn't the one that failed.
    p.file("20261027170000_tighten.sql", "alter table people add constraint people_handle_short check (length(handle) < 10);\n");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    // Named unquoted in capitals, which Postgres folds, and with a letter outside ASCII, which it
    // keeps as written.
    p.file("20261027170001_etat.sql", "alter table people add constraint État check (length(handle) < 10);\n");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    for (file, name, sql) in [
        ("20261027170002_tighten_more.sql", "people_handle_short", "alter table people drop constraint PEOPLE_HANDLE_SHORT;\nalter table people add constraint PEOPLE_HANDLE_SHORT check (handle in ('b', 'B'));\n"),
        ("20261027170002_tighten_etat.sql", "État", "alter table people drop constraint État;\nalter table people add constraint État check (handle in ('b', 'B'));\n"),
    ] {
        p.file(file, sql);
        p.output();
        assert_eq!(p.migrate().await, 1, "{}", p.peek());
        let out = p.output();
        assert_has!(&out, &format!(r"failed on rows that break {name}\n[\s\S]*  Fix the rows that break it here, then deploy again:\n"));
        assert!(!out.contains("List the rows"), "{out}");
        std::fs::remove_file(p.dir.join("migrations").join(file)).unwrap();
    }

    // A column named by a reserved word is quoted in the query.
    p.file("20261027180000_order.sql", "alter table people add column \"order\" int;\n");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.file("20261027180001_order_required.sql", "alter table people alter column \"order\" set not null;\n");
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r#"    select \* from people where "order" is null;\n"#);
    assert_eq!(p.rows(&listed_query(&out)).await.len(), 4);
}

/// On Postgres 18, a NOT NULL added `not valid` and then validated is named by its column, as
/// schema.sql writes it, not by the constraint's name.
async fn a_not_null_the_rows_break_is_named_by_its_column(mut p: P) {
    if p.d != Dialect::Postgres || p.rows("select current_setting('server_version_num')::int >= 180000 as ok").await[0].text("ok") != "t" {
        return;
    }
    let base = "create table people (id bigint primary key, nick text);";
    p.schema(base);
    p.file("20261027100000_people.sql", base);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into people values (1, null), (2, 'b')").await;
    p.schema("create table people (id bigint primary key, nick text not null);");
    p.file("20261027110000_nick.sql", "alter table people add constraint people_nick_not_null not null nick not valid;\n");
    p.file("20261027110001_nick_2.sql", "alter table people validate constraint people_nick_not_null;\n");
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"✗ The migration 20261027110001_nick_2\.sql failed on rows with nulls in people\.nick\n[\s\S]*    select \* from people where nick is null;\n[\s\S]*change people\.nick in ");
    assert_eq!(p.rows(&listed_query(&out)).await.iter().map(|r| r.text("id").to_string()).collect::<Vec<_>>(), ["1"]);
}

/// A change `migrate new` split across two tables, an index on each in a file of its own: the files
/// share no table, and the applied one is still said to be of the change, by deploy and by status.
/// The refusal offers only the failed file for deletion.
async fn a_change_split_across_tables_is_said_partway(mut p: P) {
    if p.d != Dialect::Postgres {
        return;
    }
    let base = "create table a (id bigint primary key, x int);\ncreate table b (id bigint primary key, x int);";
    p.schema(base);
    p.file("20261027100000_ab.sql", base);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into a values (1, 1), (2, 2); insert into b values (1, 1), (2, 1)").await;
    p.schema(&format!("{base}\ncreate unique index a_x on a (x);\ncreate unique index b_x on b (x);"));
    p.file("20261027110000_unique_x.sql", "create unique index concurrently a_x on a (x);\n");
    p.file("20261027110001_unique_x_2.sql", "create unique index concurrently b_x on b (x);\n");
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    assert_has!(p.output(), r"  The migration before it, 20261027110000_unique_x\.sql, of the same change is applied\.\n[\s\S]*only development ran 20261027110001_unique_x_2\.sql, change b_x in ");
    assert_eq!(p.status().await, 1);
    assert_has!(p.output(), r"» This database ran part of one change: 20261027110000_unique_x\.sql, and not 20261027110001_unique_x_2\.sql\n");

    // An index a migration of the same name dropped, which a later one made again, isn't said to
    // be dropped.
    std::fs::remove_file(p.dir.join("migrations").join("20261027110001_unique_x_2.sql")).unwrap();
    p.file("20261027120000_cleanup.sql", "drop index a_x;\n");
    p.file("20261027120001_restore.sql", "create unique index a_x on a (x);\n");
    p.file("20261027120002_cleanup_3.sql", "create unique index concurrently b_x on b (x);\n");
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    let out = p.output();
    assert!(out.contains("failed on rows that break b_x") && !out.contains("dropped"), "{out}");
    assert_eq!(p.status().await, 1);
    let out = p.output();
    assert!(!out.contains("dropped"), "{out}");

    // An index dropped is said so though another schema has one by its name.
    std::fs::remove_file(p.dir.join("migrations").join("20261027120002_cleanup_3.sql")).unwrap();
    p.file("20261027130000_elsewhere.sql", "create schema elsewhere;\ncreate table elsewhere.t (x int);\ncreate index a_x on elsewhere.t (x);\n");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.file("20261027140000_swap.sql", "drop index a_x;\n");
    p.file("20261027140001_swap_2.sql", "create unique index concurrently b_x on b (x);\n");
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    assert_has!(p.output(), r"  The migration before it, 20261027140000_swap\.sql, of the same change is applied, so a_x is dropped and not yet replaced\.\n");
    assert_eq!(p.status().await, 1);
    assert_has!(p.output(), r"» Until it runs, a_x is dropped and not yet replaced\n");
}

/// A changed unique index built under a name of its own before the old one is dropped, and renamed
/// to it after: a build the rows fail leaves the old index in force, and the refusal names the index
/// as schema.sql does, with both migrations to replace.
async fn a_unique_index_built_under_a_new_name_is_named_as_schema_sql_names_it(mut p: P) {
    if p.d != Dialect::Postgres {
        return;
    }
    let base = "create table users (id bigint primary key, org bigint, email text);\ncreate unique index users_email on users (org, email);";
    p.schema(base);
    p.file("20261027100000_users.sql", base);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into users values (1, 1, 'a'), (2, 2, 'a')").await;
    p.schema("create table users (id bigint primary key, org bigint, email text);\ncreate unique index users_email on users (email);");
    p.file("20261027110000_email.sql", "create unique index concurrently users_email_new on users (email);\n");
    p.file("20261027110001_email_2.sql", "drop index users_email;\nalter index users_email_new rename to users_email;\n");
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    assert_has!(p.output(), r"✗ The migration 20261027110000_email\.sql failed on rows that break users_email\n[\s\S]*  If the rows are right and only development ran 20261027110000_email\.sql and 20261027110001_email_2\.sql, change users_email in [^\n]*schema\.sql and delete them, then take them off development and write the replacement:\n");
    assert_eq!(p.rows("select indexrelid::regclass::text as i from pg_index where indrelid = 'users'::regclass order by 1").await.iter().map(|r| r.text("i").to_string()).collect::<Vec<_>>(), ["users_email", "users_pkey"]);
}

/// A foreign key added to a table with rows that reference nothing. On Postgres it is added `not
/// valid`, then validated; on MySQL, a change with a column too is one statement per migration, so
/// the column is in place when the key fails. Either way the refusal names what of the change is in
/// place, and lists the rows, leaving out a row whose key is NULL, which a foreign key passes.
async fn a_foreign_key_the_rows_break_stops_its_change_partway(mut p: P) {
    if p.d == Dialect::Sqlite {
        return;
    }
    let int = p.t.int;
    let base = format!("create table teams (id {int} primary key);\ncreate table people (id {int} primary key, team_id {int});");
    p.schema(&base);
    // One statement a file, as MySQL takes them.
    p.file("20261027100000_teams.sql", &format!("create table teams (id {int} primary key);"));
    p.file("20261027100001_people.sql", &format!("create table people (id {int} primary key, team_id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into teams values (1)").await;
    p.exec("insert into people values (1, 1), (2, 7), (3, null)").await;
    if p.d == Dialect::Postgres {
        p.schema(&format!("create table teams (id {int} primary key);\ncreate table people (id {int} primary key, team_id {int}, constraint people_team foreign key (team_id) references teams (id));"));
        p.file("20261027110000_team.sql", "alter table people add constraint people_team foreign key (team_id) references teams (id) not valid;\n");
        p.file("20261027110001_team_2.sql", "alter table people validate constraint people_team;\n");
    } else {
        p.schema(&format!("create table teams (id {int} primary key);\ncreate table people (id {int} primary key, team_id {int}, nick varchar(10), constraint people_team foreign key (team_id) references teams (id));"));
        p.file("20261027110000_team.sql", "alter table people add column nick varchar(10);\n");
        p.file("20261027110001_team_2.sql", "alter table people add constraint people_team foreign key (team_id) references teams (id);\n");
    }
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    let out = p.output();
    let before = if p.d == Dialect::Postgres {
        r"The migration before it, 20261027110000_team\.sql, added people_team without checking the rows already in people: rows written from now on are held to it, and those aren't\."
    } else {
        r"The migration before it, 20261027110000_team\.sql, of the same change is applied\."
    };
    assert_has!(&out, &format!(r"✗ The migration 20261027110001_team_2\.sql failed on rows that break people_team\n  [^\n]+\n  {before}\n  Nothing was recorded, so the next deploy runs it again\.\n\n  List the rows that break it:\n    select c\.\* from people as c where c\.team_id is not null and not exists \(select 1 from teams as p where p\.id = c\.team_id\);\n"));
    let ids: Vec<String> = p.rows(&listed_query(&out)).await.iter().map(|r| r.text("id").to_string()).collect();
    assert_eq!(ids, ["2"]);
    if p.d == Dialect::Postgres {
        assert_eq!(p.status().await, 1);
        assert_has!(p.output(), r"» This database ran part of one change: 20261027110000_team\.sql, and not 20261027110001_team_2\.sql\n» Until it runs, rows written to people are held to people_team, and the rows already there aren't\n");
    }
    p.exec("update people set team_id = 1 where id = 2").await;
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
}

/// A unique index built on a table that holds duplicates, new and in place of another. On
/// Postgres it is built concurrently, alone in its file, and a build that fails leaves an invalid
/// index, which deploy drops; a changed one is dropped by the migration before it, which stays
/// applied. The refusal lists the duplicates, read off the index before it is dropped, leaving out
/// NULLs, which a unique index takes any number of.
async fn a_unique_index_the_rows_break_lists_the_duplicates(mut p: P) {
    if p.d == Dialect::Sqlite {
        return;
    }
    let (int, text) = (p.t.int, p.t.text);
    let base = format!("create table users (id {int} primary key, org {int}, email {text});\ncreate unique index users_email on users (org, email);");
    p.schema(&base);
    p.file("20261027100000_users.sql", &format!("create table users (id {int} primary key, org {int}, email {text});"));
    p.file("20261027100001_users_index.sql", "create unique index users_email on users (org, email);");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into users values (1, 1, 'a'), (2, 2, 'a'), (3, 1, 'b'), (4, 1, null), (5, 2, null)").await;
    p.schema(&format!("create table users (id {int} primary key, org {int}, email {text});\ncreate unique index users_email on users (email);"));
    if p.d == Dialect::Postgres {
        p.file("20261027110000_email.sql", "drop index users_email;\n");
        p.file("20261027110001_email_2.sql", "create unique index concurrently users_email on users (email);\n");
    } else {
        p.file("20261027110000_email.sql", "alter table users drop index users_email, add unique key users_email (email);\n");
    }
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    let out = p.output();
    if p.d == Dialect::Postgres {
        assert_has!(&out, r"✗ The migration 20261027110001_email_2\.sql failed on rows that break users_email\n  could not create unique index .users_email.[^\n]*\n  The migration before it, 20261027110000_email\.sql, of the same change is applied, so users_email is dropped and not yet replaced\.\n  Nothing was recorded, so the next deploy runs it again\.\n\n  List the rows that break it:\n    select email, count\(\*\) from users where email is not null group by email having count\(\*\) > 1;\n  Fix them here, then deploy again:\n    reylo migrate deploy\n  If the rows are right and only development ran 20261027110001_email_2\.sql, change users_email in [^\n]*schema\.sql and delete it, then take it off development and write the replacement:\n    reylo migrate dev\n    reylo migrate new <name>\n");
        // The invalid index the build left is gone, so the next deploy builds it again.
        assert!(p.rows("select 1 from pg_class where relname = 'users_email'").await.is_empty());
    } else {
        assert_has!(&out, r"✗ The migration 20261027110000_email\.sql failed on rows that break users_email\n  Duplicate entry [^\n]+\n  Nothing was recorded, so the next deploy runs it again\.\n\n  List the rows that break it:\n    select email, count\(\*\) from users where email is not null group by email having count\(\*\) > 1;\n");
        // An index dropped by one file and built again by the next, beside a `check` of its name:
        // the check doesn't stand in for the index.
        std::fs::remove_file(p.dir.join("migrations").join("20261027110000_email.sql")).unwrap();
        p.file("20261027110000_named.sql", "alter table users add constraint users_email check (id > 0);\n");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.file("20261027120000_rebuild.sql", "drop index users_email on users;\n");
        p.file("20261027120001_rebuild_2.sql", "alter table users add unique key users_email (email);\n");
        p.output();
        assert_eq!(p.migrate().await, 1, "{}", p.peek());
        let out = p.output();
        assert_has!(&out, r"of the same change is applied, so users_email is dropped and not yet replaced\.\n");
        assert_eq!(p.status().await, 1);
        assert_has!(p.output(), r"» Until it runs, users_email is dropped and not yet replaced\n");
    }
    let dupes: Vec<String> = p.rows(&listed_query(&out)).await.iter().map(|r| r.text("email").to_string()).collect();
    assert_eq!(dupes, ["a"]);
    p.exec("update users set email = 'c' where id = 2").await;
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
}

async fn files_that_cannot_run_atomically_are_refused(mut p: P) {
    let int = p.t.int;
    p.schema(&format!("create table a (id {int}); create table b (id {int});"));
    p.file("20261027100000_ab.sql", &format!("begin; create table a (id {int}); create table b (id {int}); commit;"));
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), "It has its own begin");
    p.file("20261027100000_ab.sql", &format!("create table a (id {int}); create table b (id {int});"));
    let code = p.migrate().await;
    if p.d == Dialect::Mysql {
        assert_eq!(code, 1);
        assert_has!(p.output(), "MySQL commits each on its own");
    } else {
        assert_eq!(code, 0, "{}", p.peek());
    }
}

async fn a_lone_statement_cut_off(mut p: P, landed: bool) {
    if p.d == Dialect::Sqlite {
        return;
    }
    let int = p.t.int;
    let lone = if p.d == Dialect::Postgres { "create index concurrently a_id on a (id)" } else { "create index a_id on a (id)" };
    p.schema(&format!("create table a (id {int});"));
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.file("20261027110000_index.sql", &format!("{lone};"));
    p.schema(&format!("create table a (id {int}); create index a_id on a (id);"));
    // The crash: the row marked started, and the statement run or not.
    let mut db = Db::connect(&p.url, &p.dir).await.unwrap();
    let column = if p.d == Dialect::Mysql { "`sql`" } else { "sql" };
    let insert = format!("insert into __reylo_migrations (name, {column}, applied_at) values ({}, {}, null)", db.param(1), db.param(2));
    db.exec_with(&insert, &["20261027110000_index.sql", &format!("{lone};")]).await.unwrap();
    if landed {
        db.exec(lone).await.unwrap();
    }
    db.close().await;
    p.output();
    if landed {
        // Its change is in place, so this database isn't as Reylo's last finished run left it: deploy
        // refuses, a dry run says what happened, and history push records it.
        assert_eq!(p.migrate().await, 1);
        assert_has!(p.output(), "was changed outside its migrations");
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"! A cut-off run of 20261027110000_index\.sql left its change in place\n  Record it, running nothing:\n    reylo migrate history push");
        assert_eq!(p.history_push().await, 0, "{}", p.peek());
        let out = p.output();
        assert_has!(&out, r"\+ 20261027110000_index\.sql +recorded");
        assert!(!out.contains("taken out"), "{out}");
    } else {
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"✓ 20261027110000_index\.sql +applied");
    }
    assert_eq!(p.history().await, ["20261027100000_a.sql", "20261027110000_index.sql"]);
    assert_eq!(p.migrate().await, 0);
    assert_has!(p.output(), "Up to date");
}

async fn a_lone_statement_cut_off_that_landed(p: P) {
    a_lone_statement_cut_off(p, true).await
}

async fn a_lone_statement_cut_off_that_did_not_land(p: P) {
    a_lone_statement_cut_off(p, false).await
}

async fn a_lone_statement_cut_off_with_no_fingerprint_to_compare(mut p: P) {
    if p.d == Dialect::Sqlite {
        return;
    }
    let int = p.t.int;
    let lone = if p.d == Dialect::Postgres { "create index concurrently a_id on a (id)" } else { "create index a_id on a (id)" };
    p.schema(&format!("create table a (id {int}); create index a_id on a (id);"));
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.file("20261027110000_index.sql", &format!("{lone};"));
    let mut db = Db::connect(&p.url, &p.dir).await.unwrap();
    let column = if p.d == Dialect::Mysql { "`sql`" } else { "sql" };
    let insert = format!("insert into __reylo_migrations (name, {column}, applied_at) values ({}, {}, null)", db.param(1), db.param(2));
    db.exec_with(&insert, &["20261027110000_index.sql", &format!("{lone};")]).await.unwrap();
    db.exec(lone).await.unwrap();
    db.exec("update __reylo_migrations set fingerprint = null").await.unwrap();
    db.close().await;
    p.output();
    // Run again, it fails where it landed, and says how to find out.
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), r"✗ 20261027110000_index\.sql  failed\n[\s\S]*A run before this one was cut off here, so its change may be in place\.\n\n  See whether it is:\n    reylo migrate deploy --dry-run\n  Record it if it is, running nothing:\n    reylo migrate history push");
    assert_eq!(p.dry_run().await, 1);
    assert_has!(p.output(), r"20261027110000_index\.sql makes exactly this change[^\n]*\n    reylo migrate history push");
    assert_eq!(p.history_push().await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), "Up to date");
}

async fn a_shadow_left_by_a_killed_run_is_swept(mut p: P) {
    if p.d == Dialect::Sqlite {
        return;
    }
    let int = p.t.int;
    p.schema(&format!("create table a (id {int});"));
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    let mut target = Db::connect(&p.url, &p.dir).await.unwrap();
    let mut server = Server::open(&mut target, &p.url, None).await.unwrap();
    let held = server.shadow().await.unwrap();
    let dead = format!("reylo_shadow_dead{}", COUNTER.fetch_add(1, Ordering::Relaxed));
    target.exec(&format!("create database {dead}")).await.unwrap();
    target.close().await;
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    // A run that makes shadows sweeps first, as a dry run does.
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    let list = if p.d == Dialect::Postgres { "select datname as n from pg_database where datname like 'reylo_shadow_%'" } else { r"select schema_name as n from information_schema.schemata where schema_name like 'reylo\_shadow\_%'" };
    // Another test's run may be sweeping at the same moment, which this run's sweep leaves to it:
    // the dead one is gone once that drop ends, or, where that sweep listed the shadows before it
    // was made, once a later run sweeps, as each run does.
    let mut names: Vec<String> = Vec::new();
    for k in 0..100 {
        names = p.rows(list).await.iter().map(|r| r.text("n").to_string()).collect();
        if !names.contains(&dead) {
            break;
        }
        if k % 10 == 9 {
            assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(names.contains(&held.name), "the held shadow stays: {names:?}");
    assert!(!names.contains(&dead), "the dead one is dropped: {names:?}");
    server.drop_shadow(held).await;
    server.close().await;
}

async fn a_rebuilt_table_does_not_cascade(mut p: P) {
    if p.d != Dialect::Sqlite {
        return;
    }
    p.file("20261027100000_tables.sql", "create table parent (id integer primary key, name text); create table child (id integer primary key, parent_id integer references parent (id) on delete cascade);");
    p.schema("create table parent (id integer primary key, name text); create table child (id integer primary key, parent_id integer references parent (id) on delete cascade);");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("pragma foreign_keys = on; insert into parent values (1, 'p'); insert into child values (1, 1);").await;
    p.file("20261027110000_not_null.sql", "create table parent_new (id integer primary key, name text not null); insert into parent_new select * from parent; drop table parent; alter table parent_new rename to parent;");
    p.schema("create table parent (id integer primary key, name text not null); create table child (id integer primary key, parent_id integer references parent (id) on delete cascade);");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.rows("select * from child").await.len(), 1, "the child row survives the rebuild");
}

async fn one_schema_built_two_ways_has_one_catalog(p: P) {
    let (int, text) = (p.t.int, p.t.text);
    let ways = [format!("create table users (id {int}, email {text});"), format!("create table users (id {int}); alter table users add column email {text};")];
    let mut target = Db::connect(&p.url, &p.dir).await.unwrap();
    let mut server = Server::open(&mut target, &p.url, None).await.unwrap();
    let mut out = Vec::new();
    for sql in &ways {
        let mut s = server.shadow().await.unwrap();
        s.db.script(sql).await.unwrap();
        out.push(catalog(&mut s.db).await.unwrap());
        server.drop_shadow(s).await;
    }
    server.close().await;
    target.close().await;
    assert_eq!(out[0], out[1]);
}

// ---------------------------------------------------------------------------------------------
// The replay cache: a check answered from it prints what one replayed in full prints.

async fn cached_and_uncached_checks_print_the_same_lines(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    let users = format!("create table users (id {id}, email {text} not null, name {text});");
    p.schema(&users);
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, email {text} not null);"));
    p.file("20261027110000_name.sql", &format!("alter table users add column name {text};"));
    assert_eq!(p.check_every_way().await.0, 0);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.output();
    let (code, out) = p.check_every_way().await;
    assert_eq!(code, 0);
    assert_has!(out, "Nothing is pending");
    // One more migration, pending on a history the cache holds.
    p.file("20261027120000_index.sql", "create index users_email on users (email);");
    p.schema(&format!("{users} create index users_email on users (email);"));
    assert_eq!(p.check_every_way().await.0, 0);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.output();
    // Drift, then undone.
    p.exec(&format!("alter table users add column hotfix {int}")).await;
    let (code, out) = p.check_every_way().await;
    assert_eq!(code, 1);
    assert_has!(out, "This database was changed outside its migrations");
    p.exec("alter table users drop column hotfix").await;
    // A migration that does not add up, then one that fails.
    p.file("20261027130000_nick.sql", &format!("alter table users add column nick {text};"));
    let (code, out) = p.check_every_way().await;
    assert_eq!(code, 1);
    assert_has!(out, r"The migrations make what \S*schema\.sql doesn't declare[\s\S]*nick");
    p.file("20261027130000_nick.sql", &format!("alter table nobody add column nick {text};"));
    let (code, out) = p.check_every_way().await;
    assert_eq!(code, 1);
    assert_has!(out, r"fails at 20261027130000_nick\.sql");
    std::fs::remove_file(p.dir.join("migrations").join("20261027130000_nick.sql")).unwrap();
    // A new migration, which the replay runs in its place.
    p.file("20261027140000_posts.sql", &format!("create table posts (id {int});"));
    p.schema(&format!("{users} create index users_email on users (email); create table posts (id {int});"));
    assert_eq!(p.check_every_way().await.0, 0);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.output();
    // An applied migration edited: in its comments, then in what it does.
    p.file("20261027110000_name.sql", &format!("-- the name\nalter table users add column name {text};"));
    let (code, out) = p.check_every_way().await;
    assert_eq!(code, 0, "an edit to comments alone changes nothing that ran: {out}");
    p.file("20261027110000_name.sql", &format!("alter table users add column name {text} not null default 'x';"));
    let (code, out) = p.check_every_way().await;
    assert_eq!(code, 1);
    assert_has!(out, "changed after this database applied it");
}

async fn a_cached_prefix_replays_only_what_follows_it(mut p: P) {
    let int = p.t.int;
    let tables = |n: usize| (1..=n).map(|k| format!("create table t{k} (id {int});")).collect::<Vec<_>>().join(" ");
    for k in 1..=3 {
        p.file(&format!("{}_t{k}.sql", 20261027100000u64 + k as u64), &format!("create table t{k} (id {int});"));
    }
    p.schema(&tables(3));
    reylo_migrate::cache::set_enabled(Some(true));
    // A check builds shadows, which the cache keeps; deploy builds none.
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.output();
    // With its lines gone, the check starts each shadow from the database the last run kept.
    let _ = std::fs::remove_dir_all(p.dir.join(".cache").join("migrate").join("lines"));
    reylo_migrate::cache::take_stats();
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    let stats = reylo_migrate::cache::take_stats();
    // MySQL has no copy of a database: the cache keeps its lines.
    let keeps_databases = p.d != Dialect::Mysql;
    if keeps_databases {
        assert_eq!((stats.replayed, stats.copies), (0, 2), "the databases the last check kept answer: {stats:?}");
    } else {
        assert_eq!(stats.replayed, 4, "MySQL replays it all: {stats:?}");
    }
    // Then their lines do, with no shadow at all.
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    let stats = reylo_migrate::cache::take_stats();
    assert_eq!((stats.replayed, stats.copies, stats.lines), (0, 0, 2), "{stats:?}");
    // A new migration: schema.sql is built anew, and the history replays only it.
    p.file("20261027100004_t4.sql", &format!("create table t4 (id {int});"));
    p.schema(&tables(4));
    p.output();
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    let stats = reylo_migrate::cache::take_stats();
    if keeps_databases {
        // A copy for the check, and one the pending migration runs on to say what it locks and drops.
        assert_eq!((stats.replayed, stats.copies), (2, 2), "{stats:?}");
    } else {
        // The check's replay, and the history again for what the pending migration locks and drops.
        assert_eq!(stats.replayed, 8, "{stats:?}");
    }
    reylo_migrate::cache::set_enabled(None);
}

async fn an_edited_migration_misses_the_cache(mut p: P) {
    let (int, text) = (p.t.int, p.t.text);
    p.schema(&format!("create table a (id {int}); create table b (id {int}, note {text});"));
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    p.file("20261027110000_b.sql", &format!("create table b (id {int}, note {text});"));
    reylo_migrate::cache::set_enabled(Some(true));
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    reylo_migrate::cache::take_stats();
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    assert_eq!(reylo_migrate::cache::take_stats().replayed, 0, "a warm check replays nothing");
    p.output();
    // The pending migration's text changes: what it makes no longer adds up, and the check says so,
    // where the lines kept for the text it had would answer that it does. (A database another run
    // kept for the edited history may answer it, which is the same.)
    p.file("20261027110000_b.sql", &format!("create table b (id {int});"));
    assert_eq!(p.dry_run().await, 1, "{}", p.peek());
    assert_has!(p.output(), r"The schema is ahead of the migrations[\s\S]*note");
    // Back as it was, and the kept lines answer again.
    p.file("20261027110000_b.sql", &format!("create table b (id {int}, note {text});"));
    reylo_migrate::cache::take_stats();
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    assert_eq!(reylo_migrate::cache::take_stats().replayed, 0);
    // Renamed, the same text is another history.
    std::fs::rename(p.dir.join("migrations/20261027110000_b.sql"), p.dir.join("migrations/20261027110000_b2.sql")).unwrap();
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    let stats = reylo_migrate::cache::take_stats();
    assert!(stats.replayed + stats.copies >= 1, "the renamed history is not answered by the old one's lines: {stats:?}");
    reylo_migrate::cache::set_enabled(None);
    p.output();
    let (code, _) = p.check_every_way().await;
    assert_eq!(code, 0);
}

async fn db_push_takes_a_development_database_to_schema_sql(mut p: P) {
    let (id, text) = (p.t.id, p.t.text);
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, name {text});"));
    p.schema(&format!("create table users (id {id}, name {text});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"✓ 20261027100000_users\.sql +applied");
    // schema.sql moves first: sync makes the change, unrecorded, and says what to write.
    p.schema(&format!("create table users (id {id}, name {text}, bio {text});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"Pushing \S*schema\.sql to \S+\n  \+ alter table users add column bio [^\n]+;\n✓ This database matches \S*schema\.sql");
    assert_eq!(p.history().await, ["20261027100000_users.sql"]);
    p.exec("insert into users (name, bio) values ('ada', 'hi')").await;
    // The migration written for it is +recorded, and the rows stay. sync alone refuses it
    // first: a database is brought to its migrations by apply --dev.
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "bio", &Default::default(), log).await).await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.db_push().await, 1);
    assert_has!(p.output(), r"✗ This database hasn't run \d{14}_bio\.sql\n\n  Run it first:\n    reylo migrate dev");
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"\d{14}_bio\.sql +recorded");
    assert_eq!(p.rows("select bio from users").await[0].text("bio"), "hi");
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"This database matches \S*schema\.sql and its migrations");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), "Up to date");
    // Never in CI.
    assert_eq!(p.run(async |o, log| reylo_migrate::db_push(o, true, &Default::default(), log).await).await, 1);
    assert_has!(p.output(), "CI is set, and reylo db push is for development databases");
}

async fn db_push_rewinds_what_no_longer_agrees(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, name {text});"));
    p.file("20261027110000_tags.sql", &format!("create table tags (id {int});"));
    p.schema(&format!("create table users (id {id}, name {text}); create table tags (id {int});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    p.exec("insert into users (name) values ('ada')").await;
    p.output();
    // An applied migration edited: rewound, then run as it reads now.
    p.file("20261027110000_tags.sql", &format!("create table tags (id {int}, label {text});"));
    p.schema(&format!("create table users (id {id}, name {text}); create table tags (id {int}, label {text});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"↺ 20261027110000_tags\.sql +rewound: it changed since it ran");
    assert_has!(&out, r"✓ 20261027110000_tags\.sql +applied");
    assert_eq!(p.rows("select name from users").await.len(), 1, "what agrees keeps its rows");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    // Switched to a branch without it: rewound away.
    std::fs::remove_file(p.dir.join("migrations").join("20261027110000_tags.sql")).unwrap();
    p.schema(&format!("create table users (id {id}, name {text});"));
    p.output();
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"↺ 20261027110000_tags\.sql +rewound: it isn't in the migrations");
    assert!(p.try_rows("select 1 from tags").await.is_err());
    assert_eq!(p.history().await, ["20261027100000_users.sql"]);
    // A change made by hand, which schema.sql doesn't declare: refused, with the command that undoes it.
    p.exec(&format!("alter table users add column hotfix {int}")).await;
    p.output();
    assert_eq!(p.dev().await, 1, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"✗ This database was changed outside \S*schema\.sql and its migrations\n\n    alter table users add column hotfix");
    p.exec(&undo_in(&out)).await;
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    assert!(p.try_rows("select hotfix from users").await.is_err());
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
}

async fn db_push_keeps_what_no_migration_makes_yet(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    let users = format!("create table users (id {id}, name {text});");
    let notes = format!("create table notes (id {int}, body {text});");
    p.file("20261027100000_users.sql", &users);
    p.schema(&format!("{users} {notes}"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"Pushing \S*schema\.sql to \S+");
    p.exec("insert into notes values (1, 'kept')").await;
    // Nothing changed: what the last sync made stays, with its rows.
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    let out = p.output();
    assert!(!out.contains("Undid"), "{out}");
    assert_has!(&out, r"✓ This database matches \S*schema\.sql");
    assert!(!out.contains("migration yet"), "{out}");
    assert_eq!(p.rows("select * from notes").await.len(), 1);
    // schema.sql moves again: only the new change is made.
    p.schema(&format!("{users} {notes} create table tags (id {int});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"Pushing \S*schema\.sql to \S+\n  \+ create table tags");
    assert_eq!(p.rows("select * from notes").await.len(), 1);
    // A migration written for part of it is recorded; the rest stays unrecorded.
    p.file("20261027110000_notes.sql", &notes);
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"20261027110000_notes\.sql +recorded");
    assert_has!(&out, r"The schema is ahead of the migrations");
    assert_eq!(p.rows("select * from notes").await.len(), 1);
    // A migration that can't run on what sync made: that is undone first.
    p.file("20261027120000_tags.sql", &format!("create table tags (id {int}, label {text});"));
    p.schema(&format!("{users} {notes} create table tags (id {int}, label {text});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"↺ Undid what no migration made, so that 20261027120000_tags\.sql runs on what the migrations make\n    ! Dropped tags\n");
    assert_has!(&out, r"✓ 20261027120000_tags\.sql +applied[\s\S]*This database matches \S*schema\.sql and its migrations");
    assert_eq!(p.history().await, ["20261027100000_users.sql", "20261027110000_notes.sql", "20261027120000_tags.sql"]);
    assert_eq!(p.rows("select * from notes").await.len(), 1);
}

async fn apply_dev_locks_a_draft_after_what_is_locked(mut p: P) {
    let int = p.t.int;
    p.linear();
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    p.file("20261027120000_c.sql", &format!("create table c (id {int});"));
    p.schema(&format!("create table a (id {int}); create table c (id {int});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"(?m)✓ 20261027120000_c\.sql +applied[^\n]*\n✓ Wrote \S*reylo\.lock$");
    p.exec("insert into c values (1)").await;
    // A draft named older than c runs after it, where the lock puts it: nothing is rewound.
    p.file("20261027110000_b.sql", &format!("create table b (id {int});"));
    p.schema(&format!("create table a (id {int}); create table b (id {int}); create table c (id {int});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    let out = p.output();
    assert!(!out.contains("rewound"), "{out}");
    assert_eq!(p.history().await, ["20261027100000_a.sql", "20261027120000_c.sql", "20261027110000_b.sql"]);
    assert_eq!(p.locked(), ["20261027100000_a.sql", "20261027120000_c.sql", "20261027110000_b.sql"]);
    assert_eq!(p.rows("select * from c").await.len(), 1);
}

async fn a_locked_project_runs_its_lock(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    p.linear();
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, email {text});"));
    p.schema(&format!("create table users (id {id}, email {text});"));
    // A project with linear history starts with a lock, empty: apply runs nothing it doesn't list, and
    // development applies the migrations, and that locks them.
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), r"The lock doesn't list 20261027100000_users\.sql yet");
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"(?m)^✓ Wrote \S*reylo\.lock$");
    // The lock certifies schema.sql too, since the migrations reach it.
    assert_eq!(p.status().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"The migration files match \S*reylo\.lock\n✓ The migrations match \S*schema\.sql");
    // Another database runs what the lock lists.
    let q = P::new(p.d).await.unwrap();
    let dev_url = std::mem::replace(&mut p.url, q.url.clone());
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(q.history().await, ["20261027100000_users.sql"]);
    p.url = dev_url;
    p.output();
    // An edit to a locked migration is seen by status, and refused by deploy.
    let locked_text = std::fs::read_to_string(p.dir.join("migrations/20261027100000_users.sql")).unwrap();
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, email {text}, nick {text});"));
    assert_eq!(p.status().await, 1);
    assert_has!(p.output(), r"Migration 20261027100000_users\.sql changed after it was locked");
    // Deploy refuses it once, as the migration this database ran, not again as the lock's.
    assert_eq!(p.migrate().await, 1);
    let out = p.output();
    assert_has!(&out, "changed after this database applied it");
    assert!(!out.contains("changed after it was locked"), "{out}");
    p.file("20261027100000_users.sql", &locked_text);
    // A draft beside the lock is refused by apply, and run by development.
    p.file("20261027110000_nick.sql", &format!("alter table users add column nick {text};"));
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), r"The lock doesn't list 20261027110000_nick\.sql yet");
    std::fs::remove_file(p.dir.join("migrations/20261027110000_nick.sql")).unwrap();
    // schema.sql that moves with no migration is seen by status too.
    p.schema(&format!("create table users (id {id}, email {text}, bio {int});"));
    assert_eq!(p.status().await, 1);
    assert_has!(p.output(), r"The schema changed since the lock was written\n  The lock holds a checksum");
    p.schema(&format!("create table users (id {id}, email {text});"));
    // Deleting the lock turns linear history off, and reylo migrate lock turns it back on.
    std::fs::remove_file(p.dir.join(reylo_migrate::lock::LOCK)).unwrap();
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.run(async |o, log| reylo_migrate::lock_from_history(o, log).await).await, 0, "{}", p.peek());
    assert_has!(p.output(), r"✓ Wrote \S*reylo\.lock: 1 migration, in the order this database ran them");
    assert_eq!(p.locked(), ["20261027100000_users.sql"]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
}

async fn a_lock_conflict_is_resolved_by_apply_dev(mut p: P) {
    let int = p.t.int;
    p.linear();
    p.file("20261027100000_base.sql", &format!("create table base (id {int});"));
    p.file("20261027120000_mine.sql", &format!("create table mine (id {int});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_eq!(p.locked(), ["20261027100000_base.sql", "20261027120000_mine.sql"]);
    p.exec("insert into base values (1)").await;
    // Main locked another migration after base; its lines are kept, and this branch's left out.
    p.file("20261027110000_theirs.sql", &format!("create table theirs (id {int});"));
    p.lock(&["20261027100000_base.sql", "20261027110000_theirs.sql"]);
    p.output();
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"↺ 20261027120000_mine\.sql +rewound[\s\S]*✓ 20261027110000_theirs\.sql +applied[\s\S]*✓ 20261027120000_mine\.sql +applied[^\n]*\n✓ Wrote \S*reylo\.lock");
    assert_eq!(p.locked(), ["20261027100000_base.sql", "20261027110000_theirs.sql", "20261027120000_mine.sql"]);
    assert_eq!(p.rows("select * from base").await.len(), 1);
    // Accepting both sides with this branch's line first puts main's line, which this database never
    // ran, out of its chain: refused, by dev and, with no database at all, by every deploy.
    let q = P::new(p.d).await.unwrap();
    let mut r = P { url: q.url.clone(), ..P::sibling(&p) };
    r.strict = true;
    r.file("20261027100000_base.sql", &format!("create table base (id {int});"));
    assert_eq!(r.dev().await, 0, "{}", r.peek());
    r.file("20261027110000_theirs.sql", &format!("create table theirs (id {int});"));
    r.file("20261027120000_mine.sql", &format!("create table mine (id {int});"));
    // Each line was locked right after base, on its own branch.
    let base = reylo_migrate::lock::chain([("20261027100000_base.sql", format!("create table base (id {int});").as_str())]);
    let after = |name: &str, sql: &str| reylo_migrate::lock::Entry { name: name.into(), sum: reylo_migrate::lock::checksum(&base[0].sum, name, sql) };
    let wrong = reylo_migrate::lock::Lock { schema: None, entries: vec![base[0].clone(), after("20261027120000_mine.sql", &format!("create table mine (id {int});")), after("20261027110000_theirs.sql", &format!("create table theirs (id {int});"))] };
    reylo_migrate::lock::write(&r.dir.join(reylo_migrate::lock::LOCK), &wrong).unwrap();
    r.output();
    assert_eq!(r.dev().await, 1);
    assert_has!(r.output(), r"The lock was merged out of order at 20261027110000_theirs\.sql, which this database never ran\n  It came from another branch, and was locked right after 20261027100000_base\.sql");
    let main = P::new(p.d).await.unwrap();
    let mut m = P { url: main.url.clone(), ..P::sibling(&p) };
    m.strict = true;
    m.file("20261027100000_base.sql", &format!("create table base (id {int});"));
    m.file("20261027110000_theirs.sql", &format!("create table theirs (id {int});"));
    m.lock(&["20261027100000_base.sql", "20261027110000_theirs.sql"]);
    assert_eq!(m.migrate().await, 0, "{}", m.peek());
    m.output();
    m.file("20261027120000_mine.sql", &format!("create table mine (id {int});"));
    std::fs::copy(r.dir.join(reylo_migrate::lock::LOCK), m.dir.join(reylo_migrate::lock::LOCK)).unwrap();
    assert_eq!(m.migrate().await, 1);
    assert_has!(m.output(), r"The lock was merged out of order at 20261027110000_theirs\.sql\n  It was locked right after 20261027100000_base\.sql");
}

async fn create_resolves_the_names_drop_gives(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, nick {text}, email {text}, legacy {int});"));
    p.file("20261027110000_posts.sql", &format!("create table posts (id {int}, nick {text});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into posts values (1, 'p')").await;
    p.schema(&format!("create table users (id {id}); create table posts (id {int}, handle {text});"));
    let made = |p: &P| p.files().into_iter().filter(|f| f.contains("_tidy")).collect::<Vec<_>>();
    // A column's bare name names no column, so it can't reach posts.nick, renamed to handle.
    let choices = reylo_migrate::Choices { drop: vec!["nick".into()], rename: vec![], ..Default::default() };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tidy", &choices, log).await).await, 1);
    let out = p.output();
    assert_has!(&out, r"The --drop flag names nick, which this migration doesn't drop\n  A column is named with its table: users\.nick\.");
    // What the suggestion runs keeps what was stated and holds.
    let choices = reylo_migrate::Choices { drop: vec!["users.nick".into()], rename: vec![], ..Default::default() };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tidy", &choices, log).await).await, 1);
    assert_has!(p.output(), r"drops the columns users\.email and users\.legacy, with their data\n\n  To drop them:\n    reylo migrate new tidy --drop users\.nick,users\.email,users\.legacy\n");
    assert!(made(&p).is_empty());
    let choices = reylo_migrate::Choices { drop: vec!["users.nick".into(), "users.email".into(), "users.legacy".into()], rename: vec![], ..Default::default() };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tidy", &choices, log).await).await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.rows("select handle from posts").await[0].text("handle"), "p", "posts.nick was renamed, not dropped");
}

async fn create_lists_every_rename_it_assumes(mut p: P) {
    let (id, text) = (p.t.id, p.t.text);
    p.file("20261027100000_t.sql", &format!("create table users (id {id}, bio {text});"));
    p.file("20261027110000_t.sql", &format!("create table posts (id {id}, title {text});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.output();
    p.schema(&format!("create table users (id {id}, about {text}); create table posts (id {id}, heading {text});"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "names", &Default::default(), log).await).await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"\n  ~ column posts\.\[-title-\]\{\+heading\+\} ");
    assert_has!(&out, r"\n  ~ column users\.\[-bio-\]\{\+about\+\} ");
    assert_has!(&out, r"  ! Written as renames, keeping their rows\n    Where any aren't renames, delete the file, then:\n      reylo migrate new [a-z_]+ --drop posts\.title,users\.bio");
}

/// What a suggested `reylo migrate new` line states.
fn choices_of(line: &str) -> reylo_migrate::Choices {
    let mut choices = reylo_migrate::Choices::default();
    let words: Vec<&str> = line.split_whitespace().filter(|w| *w != "\\").collect();
    choices.blocking = words.contains(&"--blocking");
    for pair in words.windows(2) {
        match pair[0] {
            "--drop" => choices.drop.extend(pair[1].split(',').map(String::from)),
            "--rename" => {
                let (old, new) = pair[1].split_once('=').unwrap();
                choices.rename.push((old.to_string(), new.to_string()));
            }
            _ => {}
        }
    }
    choices
}

/// The command a refusal of several guesses prints for adding flags to, with the flags of each line
/// it picks: the rename on the left, where `keep` says, else the drop on the right.
fn picked(out: &str, keep: impl Fn(usize) -> bool) -> String {
    let rows: Vec<&str> = out.lines().skip_while(|l| !l.contains("For each, rename it to keep its rows")).skip(1).take_while(|l| l.starts_with("    --")).collect();
    let base: Vec<&str> = out.lines().skip_while(|l| !l.contains("Add the flags you picked to")).skip(1).take_while(|l| l.starts_with("    ")).collect();
    assert!(!rows.is_empty() && !base.is_empty(), "{out}");
    let flags: Vec<&str> = rows
        .iter()
        .enumerate()
        .map(|(k, r)| {
            let (left, right) = r.trim().split_once("   or   ").unwrap();
            if keep(k) { left.trim() } else { right.trim() }
        })
        .collect();
    format!("{} {}", base.join("\n"), flags.join(" "))
}

async fn create_settles_every_guess_with_one_command(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    // SQLite keeps no length of a varchar: the change is the column's nullability.
    let not_null = format!("{text} not null default ''");
    let (short, long) = if p.d == Dialect::Sqlite { (text, not_null.as_str()) } else { ("varchar(20)", "varchar(40)") };
    p.file("20261027100000_t.sql", &format!("create table users (id {id}, nick {short});"));
    p.file("20261027110000_t.sql", &format!("create table orders (id {id}, a {text}, b {text}, c {text}, legacy {text});"));
    p.file("20261027120000_t.sql", &format!("create table posts (id {id}, old_a {text}, old_b {text});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into users (nick) values ('ada'); insert into orders (a, legacy) values ('x', 'y')").await;
    p.output();
    p.schema(&format!("create table users (id {id}, handle {long}); create table invoices (id {id}, a {text}, b {text}, c {text}, total {int}); create table posts (id {id});"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tidy", &Default::default(), log).await).await, 1);
    let out = p.output();
    assert_has!(&out, r"✗ This migration can't tell renames from new tables and columns\n\n    - table orders\n    \+ table invoices\n    - column orders\.legacy [^\n]+\n    \+ column invoices\.total [^\n]+\n    - column users\.nick [^\n]+\n    \+ column users\.handle [^\n]+\n\n  It also drops the columns posts\.old_a and posts\.old_b, with their data\.\n\n  For each, rename it to keep its rows, or drop it and add the new one:\n    --rename orders=invoices --drop invoices\.legacy   or   --drop orders\n    --rename users\.nick=handle                        or   --drop users\.nick\n  Add the flags you picked to:\n    reylo migrate new tidy --drop posts\.old_a,posts\.old_b\n");
    let (keeps, drops) = (picked(&out, |_| true), picked(&out, |_| false));
    let made = |p: &P| p.files().into_iter().filter(|f| f.contains("_tidy")).collect::<Vec<_>>();
    // The second command, as printed, is one the next create takes.
    let choices = choices_of(&drops);
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tidy", &choices, log).await).await, 0, "{}", p.peek());
    let sql: String = made(&p).iter().map(|f| std::fs::read_to_string(p.dir.join("migrations").join(f)).unwrap()).collect();
    assert!(sql.contains("drop table orders") && !sql.contains(" rename "), "{sql}");
    for f in made(&p) {
        std::fs::remove_file(p.dir.join("migrations").join(f)).unwrap();
    }
    p.output();
    // So is the first, and it keeps the rows.
    let choices = choices_of(&keeps);
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tidy", &choices, log).await).await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.rows("select handle from users").await[0].text("handle"), "ada");
    assert_eq!(p.rows("select a from invoices").await[0].text("a"), "x");
    p.output();
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
}

async fn create_asks_about_a_column_replaced_by_one_of_another_type(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    p.file("20261027100000_t.sql", &format!("create table users (id {id}, score {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into users (score) values (7)").await;
    p.output();
    // A number goes and text comes: as much a question as a rename of one type to another of its kind.
    p.schema(&format!("create table users (id {id}, label {text});"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "label", &Default::default(), log).await).await, 1);
    let out = p.output();
    // On Postgres and MySQL the rename rewrites the table, so the command that takes it says
    // --blocking too, and the refusal says what that accepts.
    let (why, rename) = match p.d {
        Dialect::Postgres => (r"  As a rename, it rewrites users, copying every row while it blocks reads and writes to it\.\n\n", r"reylo migrate new label \\\n      --rename users\.score=label \\\n      --blocking"),
        Dialect::Mysql => (r"  As a rename, it rewrites users, copying every row while it blocks writes to it\.\n\n", r"reylo migrate new label \\\n      --rename users\.score=label \\\n      --blocking"),
        Dialect::Sqlite => ("", r"reylo migrate new label --rename users\.score=label"),
    };
    assert_has!(&out, &format!(r"✗ This migration can't tell a rename from a new column\n\n    - column users\.score [^\n]+\n    \+ column users\.label [^\n]+\n\n{why}  To rename the column, keeping its rows:\n    {rename}\n  To drop the column and its data, and add a new one:\n    reylo migrate new label --drop users\.score\n"));
    assert!(p.files().iter().all(|f| !f.contains("_label")), "nothing is written");
    let command = |out: &str, after: &str| out.lines().skip_while(|l| !l.contains(after)).skip(1).take_while(|l| l.starts_with("    ")).collect::<Vec<_>>().join("\n");
    // Taken as new, the old column is dropped.
    let choices = choices_of(&command(&out, "To drop the column"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "label", &choices, log).await).await, 0, "{}", p.peek());
    for f in p.files().into_iter().filter(|f| f.contains("_label")) {
        std::fs::remove_file(p.dir.join("migrations").join(f)).unwrap();
    }
    p.output();
    // Taken as a rename, its values are converted, keeping its rows, as the command says.
    let choices = choices_of(&command(&out, "To rename the column"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "label", &choices, log).await).await, 0, "{}", p.peek());
    assert_has!(p.output(), r"\n  ~ column users\.\[-score-\]\{\+label\+\} ");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.rows("select label from users").await[0].text("label"), "7");
    p.output();
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
}

/// A suggested command's flags, as a POSIX shell reads the command.
fn choices_of_shell(command: &str) -> reylo_migrate::Choices {
    let out = std::process::Command::new("sh").args(["-c", &command.replacen("reylo migrate new", "printf '%s\\n'", 1)]).output().unwrap();
    let words: Vec<String> = String::from_utf8(out.stdout).unwrap().lines().map(String::from).collect();
    let mut choices = reylo_migrate::Choices::default();
    choices.blocking = words.iter().any(|w| w == "--blocking");
    for pair in words.windows(2) {
        match pair[0].as_str() {
            "--drop" => choices.drop.extend(pair[1].split(',').map(String::from)),
            "--rename" => {
                let (old, new) = pair[1].split_once('=').unwrap();
                choices.rename.push((old.to_string(), new.to_string()));
            }
            _ => {}
        }
    }
    choices
}

async fn commands_create_prints_for_names_with_spaces_run_as_printed(mut p: P) {
    if cfg!(windows) {
        return;
    }
    let (id, text) = (p.t.id, p.t.text);
    let d = p.d;
    let q = |name: &str| if d == Dialect::Mysql { format!("`{name}`") } else { format!("\"{name}\"") };
    let (short, long) = if p.d == Dialect::Sqlite { (text.to_string(), format!("{text} not null default ''")) } else { ("varchar(20)".to_string(), "varchar(40)".to_string()) };
    let before = format!("create table {} (id {id}, {} {short});", q("My Users"), q("Full Name"));
    p.file("20261027100000_t.sql", &before);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.output();
    p.schema(&format!("create table {} (id {id}, {} {long});", q("My Users"), q("Display Name")));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "names", &Default::default(), log).await).await, 1);
    let out = p.output();
    let command = |out: &str, after: &str| out.lines().skip_while(|l| !l.contains(after)).skip(1).take_while(|l| l.starts_with("    ")).collect::<Vec<_>>().join("\n");
    let (keeps, drops) = (command(&out, "To rename the column"), command(&out, "To drop the column"));
    let choices = choices_of_shell(&drops);
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "names", &choices, log).await).await, 0, "{drops}\n{}", p.peek());
    for f in p.files().into_iter().filter(|f| f.contains("_names")) {
        std::fs::remove_file(p.dir.join("migrations").join(f)).unwrap();
    }
    p.output();
    let choices = choices_of_shell(&keeps);
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "names", &choices, log).await).await, 0, "{keeps}\n{}", p.peek());
    p.output();
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
}

async fn deploy_compares_with_what_dev_replayed(mut p: P) {
    let (int, text) = (p.t.int, p.t.text);
    p.linear();
    p.schema(&format!("create table a (id {int});"));
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.output();
    // As dev left it: nothing to note, and nothing to refuse.
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"^✓ Up to date$");
    // What sync adds is no migration's, so deploy sees that the database changed, as after a deploy.
    p.schema(&format!("create table a (id {int}, note {text});"));
    assert_eq!(p.db_push().await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.migrate().await, 1);
    let out = p.output();
    assert_has!(&out, "was changed outside its migrations");
    assert_has!(&out, r"The schema changed since the lock was written");
    // dev keeps what sync made, says the migrations don't make it, and keeps the lock's last checksum
    // of schema.sql, so status still refuses; deploy still sees it isn't the migrations'.
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"» The schema is ahead of the migrations\n  To write the changes to a migration file:\n    reylo migrate new <name>");
    assert_eq!(p.status().await, 1);
    assert_has!(p.output(), r"The schema changed since the lock was written");
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), "was changed outside its migrations");
    // Written as a migration, it is the migrations'.
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "note", &Default::default(), log).await).await, 0, "{}", p.peek());
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"^✓ Up to date$");
}

/// What dev says of schema.sql where an object changed rather than came, a view's text: edited, the
/// two disagree, and the next migration writes the change; pushed, the database has schema.sql, so
/// schema.sql is ahead.
async fn dev_says_schema_sql_is_ahead_once_a_changed_object_is_pushed(mut p: P) {
    let int = p.t.int;
    let declared = |columns: &str| format!("create table t (id {int}, n {int}); create view v as select {columns} from t;");
    p.linear();
    p.schema(&declared("id"));
    // A file each, since MySQL runs each statement alone.
    p.file("20261027100000_t.sql", &format!("create table t (id {int}, n {int});"));
    p.file("20261027100100_v.sql", "create view v as select id from t;");
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.output();
    p.schema(&declared("id, n"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"! The migrations and \S*schema\.sql disagree\n  To write what \S*schema\.sql has to a migration file:\n    reylo migrate new <name>");
    assert_eq!(p.db_push().await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"✓ Up to date with the migrations\n» The schema is ahead of the migrations\n  To write the changes to a migration file:\n    reylo migrate new <name>");
    // Written, the migration is recorded, and the two agree.
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "v", &Default::default(), log).await).await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"recorded: already in place");
    assert!(!has(&out, "ahead|disagree"), "{out}");
}

async fn a_broken_draft_keeps_what_sync_made(mut p: P) {
    let (int, text) = (p.t.int, p.t.text);
    p.linear();
    p.schema(&format!("create table a (id {int});"));
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.schema(&format!("create table a (id {int}); create table notes (body {text});"));
    assert_eq!(p.db_push().await, 0, "{}", p.peek());
    p.exec("insert into notes values ('kept')").await;
    p.output();
    p.file("20261027110000_b.sql", "insert into nope values (1);");
    assert_eq!(p.dev().await, 1);
    let out = p.output();
    assert_has!(&out, r"Replaying 20261027110000_b\.sql in a shadow database fails");
    assert!(!out.contains("Undid"), "{out}");
    assert_eq!(p.rows("select * from notes").await.len(), 1);
}

async fn a_migrations_directory_that_isnt_there_is_refused(mut p: P) {
    let int = p.t.int;
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.exec("insert into a values (1)").await;
    p.output();
    let elsewhere = |o: &Options| {
        let mut o = o.clone();
        o.dir = o.root.join("reylo-typo").join("migrations");
        o.lock = o.root.join("reylo-typo").join(reylo_migrate::lock::LOCK);
        o
    };
    assert_eq!(p.run(async |o, log| reylo_migrate::dev(&elsewhere(o), false, log).await).await, 1);
    assert_has!(p.output(), r"There's no migrations directory at \S*reylo-typo/migrations\n  This database ran 1 migration from it");
    assert_eq!(p.run(async |o, log| reylo_migrate::history_push(&elsewhere(o), log).await).await, 1);
    assert_has!(p.output(), "There's no migrations directory at");
    assert_eq!(p.rows("select * from a").await.len(), 1);
    assert_eq!(p.history().await, ["20261027100000_a.sql"]);
}

async fn a_run_cut_off_goes_on_where_its_schema_is_unchanged(mut p: P) {
    let int = p.t.int;
    if p.d == Dialect::Mysql {
        return;
    }
    // Cut off after `last` committed, before the run read its fingerprint: on Postgres its connection
    // ends in `next`, so the read fails; on SQLite, the row reads as such a run leaves it.
    let migrations = p.dir.join("migrations");
    let cut_off = async |p: &mut P, last: &str, next: &str| {
        if p.d == Dialect::Postgres {
            p.file(next, "select pg_terminate_backend(pg_backend_pid());");
            assert_eq!(p.migrate().await, 1, "{}", p.peek());
            std::fs::remove_file(migrations.join(next)).unwrap();
        } else {
            assert_eq!(p.migrate().await, 0, "{}", p.peek());
            let mut db = Db::open(&p.opts()).await.unwrap();
            let mark = reylo_migrate::commands::pending_mark(&mut db).await.unwrap();
            db.close().await;
            p.exec(&format!("update __reylo_migrations set fingerprint = '{mark}' where name = '{last}'")).await;
        }
        p.output();
    };
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    p.file("20261027110000_b.sql", &format!("create table b (id {int});"));
    cut_off(&mut p, "20261027110000_b.sql", "20261027120000_c.sql").await;
    assert_eq!(p.history().await, ["20261027100000_a.sql", "20261027110000_b.sql"]);
    // Nothing changed the schema since: the next run goes on, and stores the fingerprint.
    p.status().await;
    assert_has!(p.output(), "Unchanged since a run was cut off after its last migration");
    p.file("20261027120000_c.sql", &format!("create table c (id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), "Applied 1 migration");
    let rows = p.rows("select name from __reylo_migrations where fingerprint not like 'pending%'").await;
    assert_eq!(rows.iter().map(|r| r.text("name")).collect::<Vec<_>>(), ["20261027120000_c.sql"]);
    // Something did: refused.
    p.file("20261027130000_d.sql", &format!("create table d (id {int});"));
    cut_off(&mut p, "20261027130000_d.sql", "20261027140000_e.sql").await;
    p.exec(&format!("create table stray (id {int})")).await;
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), r"A run was cut off after 20261027130000_d\.sql applied");
}

async fn a_rewind_that_fails_leaves_the_row_before_it_unverified(mut p: P) {
    let int = p.t.int;
    // MySQL reads the schema before each migration, so each row holds its own fingerprint.
    if p.d == Dialect::Mysql {
        return;
    }
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    p.file("20261027110000_b.sql", &format!("create table b (id {int});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    // b changes into one that fails on this database, not in the empty shadows: dev rewinds b, and a
    // is the newest row when its replacement fails.
    p.exec("insert into a values (1), (1)").await;
    p.file("20261027110000_b.sql", &format!("create table b (id {int}); create unique index a_id on a (id);"));
    p.output();
    assert_eq!(p.dev().await, 1, "{}", p.peek());
    assert_eq!(p.history().await, ["20261027100000_a.sql"]);
    // A change since is not taken for what a left.
    p.exec(&format!("create table stray (id {int})")).await;
    p.output();
    assert_eq!(p.migrate().await, 1, "{}", p.peek());
    assert_has!(p.output(), "cut off|changed outside");
}

async fn init_on_an_empty_database_writes_an_empty_schema_and_lock(mut p: P) {
    assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
    assert_has!(p.output(), r"✓ Started \S+ for an empty database\n  \S+\n  ├── schema\.sql +empty: declare your tables here");
    assert!(p.files().is_empty(), "{:?}", p.files());
    assert_eq!(p.history().await, Vec::<String>::new());
    // Linear from the first migration: the lock is there, empty, and status passes.
    p.strict = true;
    assert!(p.locked().is_empty());
    assert_eq!(p.status().await, 0, "{}", p.peek());
    // The first migration is create's.
    p.schema(&format!("create table t (id {});", p.t.int));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "t", &Default::default(), log).await).await, 0, "{}", p.peek());
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_eq!(p.history().await.len(), 1);
}

async fn deploy_refuses_any_schema_with_no_history(mut p: P) {
    p.exec("create view v as select 1 as x").await;
    p.file("20261027100000_t.sql", &format!("create table t (id {});", p.t.int));
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), "This database has a schema, and no migration history");
}

async fn create_renames_a_table_keeping_its_rows(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, email {text});"));
    p.file("20261027110000_posts.sql", &format!("create table posts (id {int}, user_id {int}, foreign key (user_id) references users (id));"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into users (email) values ('a'); insert into posts values (1, 1)").await;
    let sql_of = |p: &P, name: &str| p.files().into_iter().filter(|f| f.ends_with(&format!("_{name}.sql"))).map(|f| std::fs::read_to_string(p.dir.join("migrations").join(f)).unwrap()).collect::<String>();
    // One table goes while one of its columns comes: renamed, and the foreign key on it follows.
    p.schema(&format!("create table accounts (id {id}, email {text}); create table posts (id {int}, user_id {int}, foreign key (user_id) references accounts (id));"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "accounts", &Default::default(), log).await).await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"✓ Wrote [^\n]+\n  ~ table \[-users-\]\{\+accounts\+\}[^\n]*\n(?:  [-+~] [^\n]+\n)*\n  ! Written as a rename, keeping its rows\n    If they're different tables, delete the file, then:");
    assert!(!out.contains("column users.email") && !out.contains("column accounts.email"), "a column that only moves with its table isn't listed: {out}");
    let sql = sql_of(&p, "accounts");
    assert!(sql.contains("rename to accounts") && !sql.contains("drop table"), "{sql}");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.rows("select email from accounts").await[0].text("email"), "a");
    p.output();
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    p.output();
    // A table renamed with its columns changed is a drop, unless --rename states it.
    p.schema(&format!("create table members (id {id}, address {text}, joined {int}); create table posts (id {int}, user_id {int}, foreign key (user_id) references members (id));"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "members", &Default::default(), log).await).await, 1);
    // On MySQL, the key on posts made again to reference members copies posts, which the drop's
    // command takes --blocking for.
    let drop = if p.d == Dialect::Mysql { r"\n  It also blocks writes to posts while it reads every row\.\n\n  To drop it:\n    reylo migrate new members \\\n      --drop accounts \\\n      --blocking\n" } else { r"\n\n  To drop it:\n    reylo migrate new members --drop accounts\n" };
    assert_has!(p.output(), &format!(r"drops the table accounts and its rows{drop}  If it was renamed instead, as members:\n    reylo migrate new members --rename accounts=<new name>"));
    let choices = reylo_migrate::Choices { drop: vec![], rename: vec![("accounts".into(), "members".into()), ("accounts.email".into(), "address".into())], ..Default::default() };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "members", &choices, log).await).await, 0, "{}", p.peek());
    // Both renames read as renames, the column's under the table's new name, named as --rename names
    // it through the table's old name.
    let out = p.output();
    assert_has!(&out, r"  ~ table \[-accounts-\]\{\+members\+\}\n");
    assert_has!(&out, r"  ~ column members\.\[-email-\]\{\+address\+\} ");
    assert!(!out.contains("column members.address") && !out.contains("column accounts.email") && !out.contains("column members.email"), "{out}");
    p.output();
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.rows("select address from members").await[0].text("address"), "a");
    p.exec("insert into members (address) values ('b')").await;
    assert_eq!(p.rows("select distinct id from members").await.len(), 2);
    p.output();
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
}

async fn create_renames_a_not_null_column_with_its_table(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    p.file("20261027100000_accounts.sql", &format!("create table accounts (id {id}, email {text} not null);"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into accounts (email) values ('a')").await;
    p.output();
    p.schema(&format!("create table members (id {id}, address {text} not null, joined {int});"));
    let made = |p: &P| p.files().into_iter().filter(|f| f.contains("_members")).collect::<Vec<_>>();
    let take_back = |p: &P| {
        for f in made(p) {
            std::fs::remove_file(p.dir.join("migrations").join(f)).unwrap();
        }
    };
    // With the table's rename stated, its not null column replaced by one just like it is renamed
    // too, as in a table that kept its name: on Postgres its not null, named after the table, takes
    // the new table's name with it.
    let choices = reylo_migrate::Choices { rename: vec![("accounts".into(), "members".into())], ..Default::default() };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "members", &choices, log).await).await, 0, "{}", p.peek());
    assert_has!(p.output(), r"  ~ column members\.\[-email-\]\{\+address\+\} ");
    take_back(&p);
    // The column's rename stated through either table name reaches schema.sql.
    for old in ["members.email", "accounts.email"] {
        let choices = reylo_migrate::Choices { rename: vec![("accounts".into(), "members".into()), (old.into(), "address".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "members", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        let sql = made(&p).iter().map(|f| std::fs::read_to_string(p.dir.join("migrations").join(f)).unwrap()).collect::<String>();
        assert!(sql.matches("_not_null to members_address_not_null").count() <= 1, "the not null is renamed once: {sql}");
        if old == "members.email" {
            take_back(&p);
        }
    }
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.rows("select address from members").await[0].text("address"), "a");
    p.output();
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    p.output();
    // A new name long enough that Postgres shortens the not null's name made from it.
    let long = "a".repeat(49);
    p.schema(&format!("create table {long} (id {id}, contact {text} not null, joined {int}, since {int});"));
    let choices = reylo_migrate::Choices { rename: vec![("members".into(), long.clone())], ..Default::default() };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "long", &choices, log).await).await, 0, "{}", p.peek());
    assert_has!(p.output(), &format!(r"  ~ column {long}\.\[-address-\]\{{\+contact\+\}} "));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.rows(&format!("select contact from {long}")).await[0].text("contact"), "a");
    p.output();
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
}

async fn create_offers_renames_only_to_what_the_dropped_ones_table_adds(mut p: P) {
    let (id, text) = (p.t.id, p.t.text);
    p.file("20261027100000_t.sql", &format!("create table users (id {id}, middle_name {text}, nick {text});"));
    p.file("20261027110000_t.sql", &format!("create table tags (id {id});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.output();
    p.schema(&format!("create table users (id {id}, handle {text}); create table tags (id {id}, label {text});"));
    // A column another table adds is no name for a rename, which stays in its table.
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tidy", &Default::default(), log).await).await, 1);
    assert_has!(p.output(), r"  If any were renamed instead, as users\.handle:\n    reylo migrate new tidy \\\n      --rename users\.middle_name=<new name> \\\n      --rename users\.nick=<new name>\n");
    // Nor is one a stated rename already takes: with nothing left to rename to, none is offered.
    let choices = reylo_migrate::Choices { rename: vec![("users.nick".into(), "handle".into())], ..Default::default() };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tidy", &choices, log).await).await, 1);
    let out = p.output();
    assert_has!(&out, r"This migration drops the column users\.middle_name and its data\n\n  To drop it:\n");
    assert!(!out.contains("renamed instead"), "{out}");
}

async fn create_offers_no_rename_to_a_column_another_table_renames_onto(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    let statements = |title: &str, profile: &str| [format!("create table users (id {id}, email {text} not null, name {text}{profile});"), format!("create table posts (id {id}, author_id {int} not null references users (id), {title} {text} not null, body {text});"), format!("create index posts_author_idx on posts (author_id, {title});"), format!("create view published_posts as select id, author_id, {title} from posts where body is not null;")];
    // One file per statement, which MySQL commits each on its own.
    for (k, statement) in statements("title", &format!(", profile {text}")).iter().enumerate() {
        p.file(&format!("2026102710000{k}_t{k}.sql"), statement);
    }
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into users (email) values ('a'); insert into posts (author_id, title, body) values (1, 'Hello', 'x')").await;
    p.output();
    // One table drops a column while another renames one, its index and view following: the rename
    // is assumed, and its new name, another table's column, is no name for the drop's rename.
    p.schema(&statements("heading", "").join(" "));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "step3", &Default::default(), log).await).await, 1);
    let out = p.output();
    assert_has!(&out, r"This migration drops the column users\.profile and its data\n");
    assert_has!(&out, r"reylo migrate new step3 [^\n]*--drop users\.profile");
    assert!(!out.contains("renamed instead") && !out.contains("posts.heading") && !out.contains("posts.title"), "{out}");
    let choices = reylo_migrate::Choices { drop: vec!["users.profile".into()], ..Default::default() };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "step3", &choices, log).await).await, 0, "{}", p.peek());
    assert_has!(p.output(), r"\n  ~ column posts\.\[-title-\]\{\+heading\+\} ");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.rows("select heading from published_posts").await[0].text("heading"), "Hello");
}

async fn db_push_renames_a_table_keeping_its_rows(mut p: P) {
    let (id, text) = (p.t.id, p.t.text);
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, email {text});"));
    p.schema(&format!("create table users (id {id}, email {text});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    p.exec("insert into users (email) values ('a')").await;
    p.output();
    p.schema(&format!("create table accounts (id {id}, email {text});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"Pushing \S*schema\.sql to \S+\n    ! users is renamed accounts, keeping its rows[^\n]*\n  \+ alter table \S*users\S* rename to \S*accounts\S*;");
    assert_eq!(p.rows("select email from accounts").await[0].text("email"), "a");
    // The migration written for it is recorded, and the row stays.
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "accounts", &Default::default(), log).await).await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"\d{14}_accounts\.sql +recorded[\s\S]*This database matches \S*schema\.sql and its migrations");
    assert_eq!(p.rows("select email from accounts").await[0].text("email"), "a");
}

async fn db_push_renames_a_table_back_when_it_rewinds(mut p: P) {
    let (id, text) = (p.t.id, p.t.text);
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, email {text});"));
    p.schema(&format!("create table users (id {id}, email {text});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    p.exec("insert into users (email) values ('a')").await;
    // A branch renames the table; then main again, without it.
    p.file("20261027110000_accounts.sql", "alter table users rename to accounts;");
    p.schema(&format!("create table accounts (id {id}, email {text});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    std::fs::remove_file(p.dir.join("migrations").join("20261027110000_accounts.sql")).unwrap();
    p.schema(&format!("create table users (id {id}, email {text});"));
    p.output();
    // On Postgres the push renamed the primary key after the table, which nothing on main explains.
    if p.dev().await == 1 {
        let out = p.output();
        assert_has!(&out, r"✗ This database was changed outside \S*schema\.sql and its migrations");
        p.exec(&undo_in(&out)).await;
    }
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"↺ 20261027110000_accounts\.sql +rewound: it isn't in the migrations");
    assert!(!out.contains("drops"), "{out}");
    assert_eq!(p.rows("select email from users").await[0].text("email"), "a");
}

async fn db_push_keeps_what_it_records_when_it_undoes_the_rest(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    p.file("20261027100000_users.sql", &format!("create table users (id {id});"));
    p.schema(&format!("create table users (id {id}); create table notes (id {int}, body {text}); create table tags (id {int});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    p.exec("insert into notes values (1, 'kept')").await;
    // notes is written as made; tags as something else, which can't run on what sync made.
    p.file("20261027110000_notes.sql", &format!("create table notes (id {int}, body {text});"));
    p.file("20261027120000_tags.sql", &format!("create table tags (id {int}, label {text});"));
    p.schema(&format!("create table users (id {id}); create table notes (id {int}, body {text}); create table tags (id {int}, label {text});"));
    p.output();
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"↺ Undid what no migration made, so that 20261027120000_tags\.sql runs on what the migrations make\n    ! Dropped tags\n");
    assert_has!(&out, r"20261027110000_notes\.sql +recorded[\s\S]*✓ 20261027120000_tags\.sql +applied");
    assert_eq!(p.rows("select body from notes").await[0].text("body"), "kept");
    assert_eq!(p.history().await, ["20261027100000_users.sql", "20261027110000_notes.sql", "20261027120000_tags.sql"]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
}

async fn db_push_runs_or_records_a_migration_cut_off(mut p: P) {
    let (id, int) = (p.t.id, p.t.int);
    p.file("20261027100000_users.sql", &format!("create table users (id {id});"));
    p.schema(&format!("create table users (id {id}); create table a (id {int}); create table b (id {int});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    // Two runs cut off with their rows marked started: one whose change landed, one whose didn't.
    p.exec("drop table a; drop table b").await;
    let (a, b) = (format!("create table a (id {int});"), format!("create table b (id {int});"));
    p.file("20261027110000_a.sql", &a);
    p.file("20261027120000_b.sql", &b);
    p.exec(&a).await;
    let column = if p.d == Dialect::Mysql { "`sql`" } else { "sql" };
    for (name, sql) in [("20261027110000_a.sql", &a), ("20261027120000_b.sql", &b)] {
        p.exec(&format!("insert into __reylo_migrations (name, {column}, applied_at) values ('{name}', '{sql}', null)")).await;
    }
    p.output();
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"20261027110000_a\.sql +recorded: already in place[\s\S]*✓ 20261027120000_b\.sql +applied");
    assert_eq!(p.history().await, ["20261027100000_users.sql", "20261027110000_a.sql", "20261027120000_b.sql"]);
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"This database matches \S*schema\.sql and its migrations");
}

async fn db_push_rewinds_only_what_the_rewound_migrations_made(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    p.file("20261027100000_users.sql", &format!("create table users (id {id});"));
    p.file("20261027110000_tags.sql", &format!("create table tags (id {int});"));
    p.schema(&format!("create table users (id {id}); create table tags (id {int}); create table notes (id {int}, body {text});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    p.exec("insert into notes values (1, 'kept')").await;
    // Back on a branch without tags: it goes, and notes, which schema.sql still declares, stays.
    std::fs::remove_file(p.dir.join("migrations").join("20261027110000_tags.sql")).unwrap();
    p.schema(&format!("create table users (id {id}); create table notes (id {int}, body {text});"));
    p.output();
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"↺ 20261027110000_tags\.sql +rewound: it isn't in the migrations\n    ! Dropped tags\n");
    assert!(!out.contains("notes"), "{out}");
    assert_eq!(p.rows("select body from notes").await[0].text("body"), "kept");
    assert!(p.try_rows("select 1 from tags").await.is_err());
}

/// What `reylo db push` is told: `--drop`, each `--rename <old>=<new>`, and `--force`.
fn push(drop: &[&str], rename: &[(&str, &str)], force: bool) -> reylo_migrate::Push {
    let choices = reylo_migrate::Choices { drop: drop.iter().map(|d| d.to_string()).collect(), rename: rename.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(), ..Default::default() };
    reylo_migrate::Push { choices, force }
}

async fn db_push_refuses_a_drop_until_drop_names_it(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, name {text});"));
    p.schema(&format!("create table users (id {id}, name {text}, bio {text}); create table tags (id {int});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    p.exec("insert into users (name, bio) values ('ada', 'hi')").await;
    p.exec("insert into tags values (1)").await;
    p.output();
    // schema.sql no longer has them: refused, and nothing changes.
    p.schema(&format!("create table users (id {id}, name {text});"));
    assert_eq!(p.db_push().await, 1);
    let out = p.output();
    assert_has!(&out, r"✗ This push drops the (table tags, and the column users\.bio|column users\.bio, and the table tags), with their data\n\n  To drop them:\n    reylo db push --drop (tags,users\.bio|users\.bio,tags)\n  To accept every destructive change:\n    reylo db push --force\n");
    assert_has!(&out, "Nothing was changed.");
    assert_eq!(p.rows("select * from tags").await.len(), 1);
    assert_eq!(p.rows("select bio from users").await[0].text("bio"), "hi");
    // Watching, the refusal is printed and waited out like any run's, and its fixes keep watching.
    assert_eq!(p.run(async |o, log| reylo_migrate::db_push_watch(o, false, &Default::default(), Some(1), log).await).await, 1);
    assert_has!(p.output(), r"This push drops[\s\S]*reylo db push --watch --drop ");
    // One named: the other is still refused, and the fix keeps what was named.
    assert_eq!(p.db_push_with(&push(&["tags"], &[], false)).await, 1);
    assert_has!(p.output(), r"✗ This push drops the column users\.bio and its data\n\n  To drop it:\n    reylo db push --drop tags,users\.bio\n  To accept every destructive change:\n    reylo db push --force\n");
    assert_eq!(p.rows("select * from tags").await.len(), 1);
    // A name that names nothing it drops is refused, as a typo would be.
    assert_eq!(p.db_push_with(&push(&["tags", "users.bi"], &[], false)).await, 1);
    assert_has!(p.output(), r"The --drop flag names users\.bi, which this push doesn't drop");
    // Both named: dropped, saying what they held.
    assert_eq!(p.db_push_with(&push(&["tags", "users.bio"], &[], false)).await, 0, "{}", p.peek());
    assert_has!(p.output(), r"Pushing \S*schema\.sql to [\s\S]*! Dropped [^\n]*tags \(1 row\)[\s\S]*✓ This database matches \S*schema\.sql");
    assert!(p.try_rows("select 1 from tags").await.is_err());
    assert!(p.try_rows("select bio from users").await.is_err());
    assert_eq!(p.rows("select name from users").await[0].text("name"), "ada");
}

async fn db_push_force_accepts_every_destructive_change(mut p: P) {
    let (id, text) = (p.t.id, p.t.text);
    // Each database's change that keeps the column but loses what its values hold.
    let (before, after, why) = match p.d {
        Dialect::Postgres => ("timestamptz", "date", r"Column users\.stamp changes from timestamp with time zone to date, which keeps only the date"),
        Dialect::Mysql => ("datetime(6)", "datetime", r"Column users\.stamp changes from datetime\(6\) to datetime, which rounds to whole seconds"),
        Dialect::Sqlite => ("text", "integer", r"Column users\.stamp changes from text to integer, which stores text that reads as a number as that number"),
    };
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, stamp {before}, bio {text});"));
    p.schema(&format!("create table users (id {id}, stamp {before}, bio {text});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    p.exec("insert into users (bio) values ('hi')").await;
    p.output();
    // A type change that loses values is refused as a drop is, each with its own fix.
    p.schema(&format!("create table users (id {id}, stamp {after});"));
    assert_eq!(p.db_push().await, 1);
    let out = p.output();
    assert_has!(&out, r"✗ This push drops the column users\.bio and its data\n\n  To drop it:\n    reylo db push --drop users\.bio\n");
    assert_has!(&out, &format!(r"✗ {why}[^\n]*\n\n  To accept that:\n    reylo db push --drop users\.stamp\n  To accept every destructive change:\n    reylo db push --force\n"));
    assert_eq!(p.rows("select bio from users").await[0].text("bio"), "hi");
    // The narrowing alone, the drop named: still refused.
    assert_eq!(p.db_push_with(&push(&["users.bio"], &[], false)).await, 1);
    assert_has!(p.output(), &format!(r"✗ {why}"));
    assert!(p.try_rows("select bio from users").await.is_ok());
    // --force accepts both at once.
    assert_eq!(p.db_push_with(&push(&[], &[], true)).await, 0, "{}", p.peek());
    assert_has!(p.output(), r"Pushing \S*schema\.sql to [\s\S]*✓ This database matches \S*schema\.sql");
    assert!(p.try_rows("select bio from users").await.is_err());
    // A name --drop gives that names nothing still stands with --force.
    p.schema(&format!("create table users (id {id});"));
    assert_eq!(p.db_push_with(&push(&["users.stam"], &[], true)).await, 1);
    assert_has!(p.output(), r"The --drop flag names users\.stam, which this push doesn't drop");
    assert!(p.try_rows("select stamp from users").await.is_ok());
}

async fn db_push_takes_a_rename_it_is_told_of(mut p: P) {
    let (id, text) = (p.t.id, p.t.text);
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, middle_name {text}, legacy {text});"));
    p.schema(&format!("create table users (id {id}, middle_name {text}, legacy {text});"));
    assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
    p.exec("insert into users (middle_name, legacy) values ('m', 'l')").await;
    p.output();
    // Two text columns go and one comes: which, if any, was renamed is for the person to say.
    p.schema(&format!("create table users (id {id}, nickname {text});"));
    assert_eq!(p.db_push().await, 1);
    assert_has!(p.output(), r"✗ This push drops the columns users\.legacy and users\.middle_name, with their data\n\n  To drop them:\n    reylo db push --drop users\.legacy,users\.middle_name\n  If any were renamed instead, as users\.nickname:\n    reylo db push \\\n      --rename users\.legacy=<new name> \\\n      --rename users\.middle_name=<new name>\n  To accept every destructive change:\n    reylo db push --force\n");
    // A rename named alone leaves the other column's drop refused.
    assert_eq!(p.db_push_with(&push(&[], &[("users.middle_name", "nickname")], false)).await, 1);
    assert_has!(p.output(), r"✗ This push drops the column users\.legacy and its data\n\n  To drop it:\n    reylo db push \\\n      --drop users\.legacy \\\n      --rename users\.middle_name=nickname\n");
    // Stated: renamed, keeping its rows, and the other dropped.
    assert_eq!(p.db_push_with(&push(&["users.legacy"], &[("users.middle_name", "nickname")], false)).await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.rows("select nickname from users").await[0].text("nickname"), "m");
    assert!(p.try_rows("select legacy from users").await.is_err());
}

async fn create_guesses_no_rename_a_table_that_stays_could_explain(mut p: P) {
    let (id, text) = (p.t.id, p.t.text);
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, email {text});"));
    p.file("20261027110000_accounts.sql", &format!("create table accounts (id {id}, email {text});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    // accounts became members and users became accounts, or users became members: no telling.
    p.schema(&format!("create table accounts (id {id}, email {text}); create table members (id {id}, email {text});"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "members", &Default::default(), log).await).await, 1);
    assert_has!(p.output(), r"drops the table users and its rows[\s\S]*reylo migrate new members --rename users=<new name>");
    // Stated, it is done as stated, and names ignore case.
    let choices = reylo_migrate::Choices { drop: vec![], rename: vec![("USERS".into(), "Members".into())], ..Default::default() };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "members", &choices, log).await).await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
}

async fn deploy_builds_no_shadow_database_and_trusts_the_fingerprint(mut p: P) {
    let (id, int) = (p.t.id, p.t.int);
    reylo_migrate::cache::set_enabled(Some(true));
    reylo_migrate::cache::take_stats();
    // A first run, a later one, and one with nothing to run replay nothing at all.
    p.file("20261027100000_users.sql", &format!("create table users (id {id});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"Applying to [^\n]+\n  ✓ 20261027100000_users\.sql  applied · \d+ ms");
    p.file("20261027110000_tags.sql", &format!("create table tags (id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"✓ 20261027110000_tags\.sql  applied");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"^✓ Up to date$");
    let stats = reylo_migrate::cache::take_stats();
    reylo_migrate::cache::set_enabled(None);
    assert_eq!((stats.replayed, stats.copies), (0, 0), "{stats:?}");
    // A change by hand no longer has the fingerprint: refused, and check says how it differs.
    p.exec(&format!("create table stray (id {int})")).await;
    assert_eq!(p.migrate().await, 1);
    let out = p.output();
    assert_has!(&out, r"✗ This database was changed outside its migrations\n\n  \+ table stray[^\n]*\n      id [^\n]+\n\n  If a migration makes this change, record it here, running nothing:\n    reylo migrate history push\n  If none does, write one and run it in development, then record it here:\n    reylo migrate new <name>\n    reylo migrate dev\n    reylo migrate history push\n  To see which migrations it matches:\n    reylo migrate deploy --dry-run\n\nNothing was changed\.");
    p.schema(&format!("create table users (id {id}); create table tags (id {int});"));
    assert_eq!(p.dry_run().await, 1);
    assert_has!(p.output(), "This database was changed outside its migrations");
    // Undone, it has the fingerprint again.
    p.exec("drop table stray").await;
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), "Up to date");
}

async fn deploy_goes_on_where_it_has_no_fingerprint_to_compare(mut p: P) {
    let int = p.t.int;
    p.file("20261027100000_users.sql", &format!("create table users (id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.output();
    // A catalog reader of another version, or a new major version of the server: read another way, a
    // fingerprint says nothing.
    p.exec("update __reylo_migrations set fingerprint = 'old:1:abc'").await;
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"» This database can't be compared here with what its last migration left\n  To compare it with a dry run:\n    reylo migrate deploy --dry-run[\s\S]*Up to date");
    // It is stored again, so a change after that is seen.
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    let out = p.output();
    assert!(!out.contains("no fingerprint"), "{out}");
    p.exec(&format!("create table stray (id {int})")).await;
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), "was changed outside its migrations");
    p.exec("drop table stray").await;
    // A pending migration runs on one with none, and stores it.
    p.exec("update __reylo_migrations set fingerprint = null").await;
    p.file("20261027110000_tags.sql", &format!("create table tags (id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"can't be compared here with what its last migration left[\s\S]*applied");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"^✓ Up to date$");
}

async fn deploy_refuses_tables_with_no_history(mut p: P) {
    let int = p.t.int;
    p.exec(&format!("create table legacy (id {int})")).await;
    p.file("20261027100000_users.sql", &format!("create table users (id {int});"));
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), r"✗ This database has a schema, and no migration history\n\n  Where the migrations describe it, record them, running nothing:\n    reylo migrate history push\n  Where they don't, start the project's migrations from it:\n    reylo migrate init");
    assert_eq!(p.try_rows("select name from __reylo_migrations").await.map(|r| r.len()).unwrap_or(0), 0);
}

async fn create_names_a_type_change_that_loses_values_in_drop(mut p: P) {
    let id = p.t.id;
    // Each database's change that keeps the column but loses what its values hold.
    let (before, after, why) = match p.d {
        Dialect::Postgres => ("timestamptz", "date", r"Column users\.stamp changes from timestamp with time zone to date, which keeps only the date"),
        Dialect::Mysql => ("datetime(6)", "datetime", r"Column users\.stamp changes from datetime\(6\) to datetime, which rounds to whole seconds"),
        Dialect::Sqlite => ("text", "integer", r"Column users\.stamp changes from text to integer, which stores text that reads as a number as that number"),
    };
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, stamp {before});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.schema(&format!("create table users (id {id}, stamp {after});"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "narrow", &Default::default(), log).await).await, 1);
    let out = p.output();
    // The change copies the table on Postgres and MySQL, so the command says --blocking too, and
    // the refusal says what that accepts.
    let (rewrites, accept) = match p.d {
        Dialect::Postgres => (r"  It also rewrites users, copying every row while it blocks reads and writes to it\.\n", r"reylo migrate new narrow \\\n      --drop users\.stamp \\\n      --blocking\n"),
        Dialect::Mysql => (r"  It also rewrites users, copying every row while it blocks writes to it\.\n", r"reylo migrate new narrow \\\n      --drop users\.stamp \\\n      --blocking\n"),
        Dialect::Sqlite => ("", r"reylo migrate new narrow --drop users\.stamp\n"),
    };
    assert_has!(&out, &format!(r"✗ {why}[^\n]*\n{rewrites}\n  To accept that:\n    {accept}"));
    assert!(p.files().iter().all(|f| !f.contains("_narrow")), "nothing is written");
    let choices = reylo_migrate::Choices { drop: vec!["users.stamp".into()], rename: vec![], blocking: p.d != Dialect::Sqlite };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "narrow", &choices, log).await).await, 0, "{}", p.peek());
    let sql = p.files().into_iter().filter(|f| f.contains("_narrow")).map(|f| std::fs::read_to_string(p.dir.join("migrations").join(f)).unwrap()).collect::<String>();
    assert!(sql.contains("-- loses data: users.stamp changes from"), "{sql}");
    // A widening change needs nothing.
    let wider = match p.d {
        Dialect::Postgres => "timestamptz",
        Dialect::Mysql => "datetime(6)",
        Dialect::Sqlite => "text",
    };
    for f in p.files().into_iter().filter(|f| f.contains("_narrow")) {
        std::fs::remove_file(p.dir.join("migrations").join(f)).unwrap();
    }
    p.file("20261027110000_wider.sql", &format!("{};", match p.d {
        Dialect::Postgres => "alter table users alter column stamp type timestamp(3) with time zone",
        Dialect::Mysql => "alter table users modify stamp datetime(3)",
        Dialect::Sqlite => "alter table users add column note text",
    }));
    p.schema(&format!("create table users (id {id}, stamp {wider}{});", if p.d == Dialect::Sqlite { ", note text" } else { "" }));
    p.output();
    // MySQL widens a datetime's fractions by copying the table, which --blocking accepts.
    let choices = reylo_migrate::Choices { blocking: p.d == Dialect::Mysql, ..Default::default() };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "widen", &choices, log).await).await, 0, "{}", p.peek());
}

async fn create_assumes_a_rename_where_one_goes_and_one_comes(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    let not_null = format!("{text} not null default ''");
    let (short, long) = if p.d == Dialect::Sqlite { (text, not_null.as_str()) } else { ("varchar(20)", "varchar(40)") };
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, nick {short});"));
    p.file("20261027110000_accounts.sql", &format!("create table accounts (id {int}, email {short}, name {short});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into users (nick) values ('ada'); insert into accounts values (1, 'a@b.co', 'Ada')").await;
    // One column goes and one of its family comes: renamed, its type changed. One table goes and
    // one comes with most of its columns: renamed, and given the new one.
    p.schema(&format!("create table users (id {id}, handle {long}); create table members (id {int}, email {short}, name {short}, joined {int});"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "renames", &Default::default(), log).await).await, 1);
    let out = p.output();
    assert_has!(&out, r"✗ This migration can't tell renames from new tables and columns\n\n    - table accounts\n    \+ table members\n    \+ column members\.joined [^\n]+\n    - column users\.nick [^\n]+\n    \+ column users\.handle [^\n]+\n\n  For each, rename it to keep its rows, or drop it and add the new one:\n    --rename accounts=members    or   --drop accounts\n    --rename users\.nick=handle   or   --drop users\.nick\n  Add the flags you picked to:\n    reylo migrate new renames\n\nNothing was changed\.");
    assert_eq!(p.files().len(), 2, "nothing is written");
    // The second command, which takes them for new ones.
    let choices = reylo_migrate::Choices { drop: vec!["accounts".into(), "users.nick".into()], rename: vec![], ..Default::default() };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "renames", &choices, log).await).await, 0, "{}", p.peek());
    let made: Vec<String> = p.files().into_iter().filter(|f| f.contains("_renames")).collect();
    let sql: String = made.iter().map(|f| std::fs::read_to_string(p.dir.join("migrations").join(f)).unwrap()).collect();
    assert!(sql.contains("drop table accounts") && !sql.contains(" rename "), "{sql}");
    for f in made {
        std::fs::remove_file(p.dir.join("migrations").join(f)).unwrap();
    }
    p.output();
    // The first, which takes them for renames.
    let choices = reylo_migrate::Choices { drop: vec![], rename: vec![("accounts".into(), "members".into()), ("users.nick".into(), "handle".into())], ..Default::default() };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "renames", &choices, log).await).await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.rows("select handle from users").await[0].text("handle"), "ada");
    assert_eq!(p.rows("select name from members").await[0].text("name"), "Ada");
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
}

async fn db_push_watch_syncs_again_when_schema_sql_changes(mut p: P) {
    let (id, text) = (p.t.id, p.t.text);
    p.file("20261027100000_users.sql", &format!("create table users (id {id});"));
    p.schema(&format!("create table users (id {id});"));
    assert_eq!(p.run(async |o, log| reylo_migrate::dev(o, false, log).await).await, 0, "{}", p.peek());
    p.output();
    // A save to schema.sql a moment after the first run starts the second.
    let schema = p.dir.join("schema.sql");
    let later = format!("create table users (id {id}, bio {text});");
    let saver = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1200));
        std::fs::write(schema, later).unwrap();
    });
    assert_eq!(p.run(async |o, log| reylo_migrate::db_push_watch(o, false, &Default::default(), Some(2), log).await).await, 0, "{}", p.peek());
    saver.join().unwrap();
    let out = p.output();
    assert_has!(&out, r"Watching \S*schema\.sql and \S*migrations: each save pushes again");
    assert_has!(&out, r"This database matches \S*schema\.sql and its migrations[\s\S]*\d\d:\d\d:\d\d[\s\S]*Pushing \S*schema\.sql to \S+\n  \+ alter table users add column bio");
    assert!(p.try_rows("select bio from users").await.is_ok());
    assert_eq!(p.run(async |o, log| reylo_migrate::db_push_watch(o, true, &Default::default(), Some(1), log).await).await, 1);
    assert_has!(p.output(), "CI is set, and reylo db push --watch is for development databases");
}

async fn a_lock_whose_order_is_not_the_names_survives_one_run(mut p: P) {
    let int = p.t.int;
    p.linear();
    p.file("20261027100000_base.sql", &format!("create table base (id {int});"));
    p.file("20261027110000_mine.sql", &format!("create table mine (id {int});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    // Main locked an migration named after mine; mine runs after it, in one run with it.
    p.file("20261027120000_theirs.sql", &format!("create table theirs (id {int});"));
    p.lock(&["20261027100000_base.sql", "20261027120000_theirs.sql"]);
    for _ in 0..3 {
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_eq!(p.locked(), ["20261027100000_base.sql", "20261027120000_theirs.sql", "20261027110000_mine.sql"]);
        assert_eq!(p.history().await, ["20261027100000_base.sql", "20261027120000_theirs.sql", "20261027110000_mine.sql"]);
        p.output();
    }
    // Another database given that lock runs it in its order, and is up to date after.
    let q = P::new(p.d).await.unwrap();
    let dev = std::mem::replace(&mut p.url, q.url.clone());
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), "Up to date");
    p.url = dev;
}

async fn a_fixed_migration_no_database_ran_is_locked_again(mut p: P) {
    let (int, text) = (p.t.int, p.t.text);
    p.linear();
    p.schema(&format!("create table base (id {int}); create table theirs (id {int});"));
    p.file("20261027100000_base.sql", &format!("create table base (id {int});"));
    p.file("20261027120000_theirs.sql", &format!("create table theirs (id {int});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    // Staging runs it as it was.
    let dev_url = p.url.clone();
    let staging = P::new(p.d).await.unwrap();
    p.url = staging.url.clone();
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.url = dev_url;
    p.output();
    // Fixed afterwards, as one that failed on production, which recorded nothing of it: dev locks it
    // again as it now reads, a database that never ran it runs the fix, and one that ran it as it
    // was refuses the edit.
    p.file("20261027120000_theirs.sql", &format!("create table theirs (id {int}, note {text});"));
    p.schema(&format!("create table base (id {int}); create table theirs (id {int}, note {text});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"✓ Wrote \S*reylo\.lock\n  ! Locked again as it now reads: 20261027120000_theirs\.sql");
    assert_eq!(p.status().await, 0, "{}", p.peek());
    let production = P::new(p.d).await.unwrap();
    p.url = production.url.clone();
    p.output();
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.rows("select note from theirs").await.len(), 0, "the fix ran");
    p.url = staging.url.clone();
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), r"Migration 20261027120000_theirs\.sql changed after this database applied it");
}

async fn init_locks_the_baseline_in_a_locked_project(mut p: P) {
    let (id, text) = (p.t.id, p.t.text);
    p.linear();
    p.exec(&format!("create table users (id {id}, email {text} not null)")).await;
    assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.locked(), ["init.sql"]);
    assert_eq!(p.history().await, ["init.sql"]);
    assert_eq!(p.status().await, 0, "{}", p.peek());
    assert_has!(p.output(), "The migrations match");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
}

async fn history_push_keeps_a_data_migration_that_ran(mut p: P) {
    let int = p.t.int;
    p.schema(&format!("create table t (x {int});"));
    p.file("20261027100000_t.sql", &format!("create table t (x {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into t values (1)").await;
    p.file("20261027110000_backfill.sql", "update t set x = x * 10;");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.history_push().await, 0, "{}", p.peek());
    let out = p.output();
    assert!(!out.contains("taken out") && !out.contains("pending"), "{out}");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.rows("select x from t").await[0].text("x"), "10");
}

async fn create_guesses_no_rename_of_unrelated_tables(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    p.file("20261027100000_sessions.sql", &format!("create table sessions (id {id}, user_id {int}, created_at {text});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    // Sharing only the columns most tables have, two tables are two.
    p.schema(&format!("create table audit_events (id {id}, user_id {int}, created_at {text}, kind {text});"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "audit", &Default::default(), log).await).await, 1);
    assert_has!(p.output(), r"drops the table sessions and its rows");
}

async fn the_drift_advice_in_a_locked_project_locks_the_migration_first(mut p: P) {
    let (id, int) = (p.t.id, p.t.int);
    p.linear();
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, n {int});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.exec("create index users_n on users (n)").await;
    p.output();
    p.schema(&format!("create table users (id {id}, n {int});"));
    assert_eq!(p.dry_run().await, 1);
    assert_has!(p.output(), r"To keep it, add it to \S*schema\.sql and write its migration in development:\n    reylo migrate new <name>\n    reylo migrate dev\n  Then record it here, running nothing:\n    reylo migrate history push");
}

async fn wipe_deletes_rows_and_keeps_the_schema(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    // MySQL commits each DDL statement on its own, so each is a file of its own.
    let users = format!("create table users (id {id}, name {text});");
    p.file("20261027100000_users.sql", &users);
    p.file("20261027110000_posts.sql", &format!("create table posts (id {int}, user_id {int} references users (id));"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into users (name) values ('a'), ('b'); insert into posts values (1, 1)").await;
    p.output();
    assert_eq!(p.run(async |o, log| reylo_migrate::db_wipe(o, false, log).await).await, 0, "{}", p.peek());
    assert_has!(p.output(), r"Deleted every row of 2 tables on \S+[^\n]*\n» The schema and the history stay");
    assert!(p.rows("select * from users").await.is_empty() && p.rows("select * from posts").await.is_empty());
    assert_eq!(p.history().await, ["20261027100000_users.sql", "20261027110000_posts.sql"]);
    // Identities start again.
    p.exec("insert into users (name) values ('c')").await;
    assert_eq!(p.rows("select id from users").await[0].text("id"), "1");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_has!(p.output(), "Up to date");
}

async fn create_states_what_it_drops_and_renames(mut p: P) {
    let (id, text) = (p.t.id, p.t.text);
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, middle_name {text}, legacy {text});"));
    p.schema(&format!("create table users (id {id}, middle_name {text}, legacy {text});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into users (middle_name, legacy) values ('m', 'l')").await;
    p.schema(&format!("create table users (id {id}, nickname {text});"));
    let create = |drop: &[&str], rename: &[(&str, &str)]| reylo_migrate::Choices { drop: drop.iter().map(|d| d.to_string()).collect(), rename: rename.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(), ..Default::default() };
    let made = |p: &P| p.files().into_iter().filter(|f| f.contains("_tidy")).collect::<Vec<_>>();
    let take_back = |p: &P| {
        for f in made(p) {
            std::fs::remove_file(p.dir.join("migrations").join(f)).unwrap();
        }
    };
    // Two text columns go and one comes: no rename is guessed, and nothing is dropped unstated.
    let choices = create(&[], &[]);
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tidy", &choices, log).await).await, 1);
    let out = p.output();
    assert_has!(&out, r"This migration drops the columns users\.legacy and users\.middle_name, with their data\n\n  To drop them:\n    reylo migrate new tidy --drop users\.legacy,users\.middle_name\n  If any were renamed instead, as users\.nickname:\n    reylo migrate new tidy \\\n      --rename users\.legacy=<new name> \\\n      --rename users\.middle_name=<new name>");
    assert!(made(&p).is_empty(), "nothing is written");
    // Dropping one leaves the other alone with its definition: renamed, keeping its rows.
    let choices = create(&["users.legacy"], &[]);
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tidy", &choices, log).await).await, 0, "{}", p.peek());
    assert_has!(p.output(), r"  ~ column users\.\[-middle_name-\]\{\+nickname\+\}[^\n]*\n(?:  [-+~] [^\n]+\n)*\n(?:  ! [^\n]+\n)*  ! Written as a rename, keeping its rows\n    If they're different columns, delete the file, then:\n      reylo migrate new tidy --drop users\.legacy,users\.middle_name");
    let sql = made(&p).iter().map(|f| std::fs::read_to_string(p.dir.join("migrations").join(f)).unwrap()).collect::<String>();
    assert!(sql.contains("rename column middle_name to nickname") && sql.contains("drop column legacy"), "{sql}");
    take_back(&p);
    // Both named: both dropped, nickname added.
    let choices = create(&["users.legacy", "users.middle_name"], &[]);
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tidy", &choices, log).await).await, 0, "{}", p.peek());
    let sql = made(&p).iter().map(|f| std::fs::read_to_string(p.dir.join("migrations").join(f)).unwrap()).collect::<String>();
    assert!(!sql.contains("rename") && sql.contains("drop column middle_name"), "{sql}");
    take_back(&p);
    p.output();
    // A name --drop gives that the change does not drop is refused, as a typo would be.
    let choices = create(&["users.legacy", "users.middle_nam"], &[]);
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tidy", &choices, log).await).await, 1);
    assert_has!(p.output(), r"The --drop flag names users\.middle_nam, which this migration doesn't drop");
    // A stated rename, with a type change, keeps the rows; the data moves by assignment.
    let narrow = if p.d == Dialect::Sqlite { "text" } else { "varchar(40)" };
    p.schema(&format!("create table users (id {id}, nickname {narrow});"));
    // The type change copies the table on Postgres and MySQL, which --blocking accepts.
    let choices = reylo_migrate::Choices { blocking: true, ..create(&["users.legacy"], &[("users.middle_name", "nickname")]) };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tidy", &choices, log).await).await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.rows("select nickname from users").await[0].text("nickname"), "m");
}

async fn two_applies_at_once_apply_each_migration_once(mut p: P) {
    let (int, text) = (p.t.int, p.t.text);
    for k in 0..5u64 {
        p.file(&format!("{}_t{k}.sql", 20261027100000u64 + k), &format!("create table t{k} (id {int}, note {text});"));
    }
    p.relock();
    let opts = p.opts();
    // Two instances of an app, each migrating as it starts, on one database.
    let runs: Vec<(i32, String)> = std::thread::scope(|s| {
        let started: Vec<_> = (0..2)
            .map(|_| {
                let opts = &opts;
                s.spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                    let mut lines = Vec::new();
                    let code = rt.block_on(reylo_migrate::deploy(opts, &mut |l: &str| lines.push(l.to_string()))).unwrap();
                    (code, lines.join("\n"))
                })
            })
            .collect();
        started.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for (code, out) in &runs {
        assert_eq!(*code, 0, "{out}");
    }
    let applied: usize = runs.iter().map(|(_, out)| out.matches(" applied ·").count()).sum();
    assert_eq!(applied, 5, "each migration ran once: {runs:?}");
    assert!(runs.iter().any(|(_, out)| out.contains("Up to date")), "{runs:?}");
    assert_eq!(p.history().await.len(), 5);
    p.output();
}

async fn concurrent_runs_building_one_cache_both_succeed(mut p: P) {
    let (int, text) = (p.t.int, p.t.text);
    for k in 0..20u64 {
        p.file(&format!("{}_t{k}.sql", 20261027100000u64 + k), &format!("create table t{k} (id {int}, note {text});"));
    }
    p.schema(&(0..20).map(|k| format!("create table t{k} (id {int}, note {text});")).collect::<Vec<_>>().join("\n"));
    // A second target of the same project, whose shadows share the first's keys.
    let q = P::new(p.d).await.unwrap();
    p.relock();
    let both = [p.opts(), Options { url: q.url.clone(), ..p.opts() }];
    for round in 0..3 {
        let _ = std::fs::remove_dir_all(p.dir.join(".cache"));
        forget(&p.url).await;
        forget(&q.url).await;
        let runs: Vec<(i32, Vec<String>)> = std::thread::scope(|s| {
            let started: Vec<_> = both
                .iter()
                .map(|o| {
                    s.spawn(move || {
                        reylo_migrate::cache::set_enabled(Some(true));
                        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                        let mut lines = Vec::new();
                        let code = rt.block_on(reylo_migrate::dry_run(o, &mut |l: &str| lines.push(l.to_string()))).unwrap();
                        (code, lines)
                    })
                })
                .collect();
            started.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (code, lines) in &runs {
            assert_eq!(*code, 0, "round {round}:\n{}", lines.join("\n"));
        }
        assert_eq!(same_run(&runs[0].1.join("\n")), same_run(&runs[1].1.join("\n")), "round {round}");
    }
    // What they kept answers the next check.
    reylo_migrate::cache::set_enabled(Some(true));
    reylo_migrate::cache::take_stats();
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    assert_eq!(reylo_migrate::cache::take_stats().replayed, 0);
    reylo_migrate::cache::set_enabled(None);
    forget(&q.url).await;
}

async fn create_drafts_one_migration_cached_or_not(mut p: P) {
    let (id, int, text) = (p.t.id, p.t.int, p.t.text);
    p.file("20261027100000_users.sql", &format!("create table users (id {id}, email {text});"));
    p.file("20261027110000_posts.sql", &format!("create table posts (id {int});"));
    p.schema(&format!("create table users (id {id}, email {text}); create table posts (id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.output();
    p.schema(&format!("create table users (id {id}, email {text}, nick {text}); create table posts (id {int}, title {text});"));
    // Each way, `create` writes the same files (on MySQL one per statement) and prints the same;
    // they are taken back each time.
    let mut drafts = Vec::new();
    for cache in [false, true, true] {
        reylo_migrate::cache::set_enabled(Some(cache));
        let code = p.run(async |o, log| reylo_migrate::create(o, "more", &Default::default(), log).await).await;
        reylo_migrate::cache::set_enabled(None);
        let made: Vec<String> = p.files().into_iter().filter(|f| has(f, r"_more(_\d+)?\.sql$")).collect();
        let texts: Vec<String> = made.iter().map(|f| std::fs::read_to_string(p.dir.join("migrations").join(f)).unwrap()).collect();
        for f in &made {
            std::fs::remove_file(p.dir.join("migrations").join(f)).unwrap();
        }
        let out = regex::Regex::new(r"\d{14}_more").unwrap().replace_all(&same_run(&p.output()), "<time>_more").into_owned();
        drafts.push((code, out, texts));
    }
    assert_eq!(drafts[0].0, 0, "{:?}", drafts[0]);
    assert_eq!(drafts[1], drafts[0], "a cold cache changed the draft");
    assert_eq!(drafts[2], drafts[0], "a warm cache changed the draft");
    // The draft, written, checks out, from what `create` kept.
    reylo_migrate::cache::set_enabled(Some(true));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "more", &Default::default(), log).await).await, 0, "{}", p.peek());
    p.output();
    reylo_migrate::cache::set_enabled(None);
    let (code, out) = p.check_every_way().await;
    assert_eq!(code, 0, "{out}");
}

// ---------------------------------------------------------------------------------------------
// Adversarial cases, on the server databases they concern.

macro_rules! on {
    (Turso, $($scenario:ident),* $(,)?) => {$(
        #[tokio::test(flavor = "current_thread")]
        async fn $scenario() {
            if let Some(p) = P::turso().await { adversarial::$scenario(p).await }
        }
    )*};
    ($dialect:ident, $($scenario:ident),* $(,)?) => {$(
        #[tokio::test(flavor = "current_thread")]
        async fn $scenario() {
            if let Some(p) = P::new(Dialect::$dialect).await { let url = p.url.clone(); adversarial::$scenario(p).await; forget(&url).await }
        }
    )*};
}

on!(
    Postgres,
    a_table_renamed_to_a_name_postgres_quotes_or_shortens_keeps_its_constraints,
    a_table_renamed_keeps_the_sequences_its_columns_use,
    a_table_renamed_keeps_the_counter_its_column_draws_from,
    a_suggested_command_says_blocking_where_its_migration_needs_it,
    a_column_renamed_with_its_declared_not_null_renames_it_once,
    a_suggested_blocking_names_the_rewrite_it_accepts,
    a_not_null_renamed_with_its_table_leaves_a_held_name_alone,
    a_database_search_path_resolves_names_as_the_target_does,
    a_guess_within_a_guessed_table_keeps_its_line_under_a_search_path,
    a_database_time_zone_does_not_change_defaults,
    grants_compare_without_their_grantor,
    a_migration_that_creates_a_role_is_refused,
    a_role_grant_is_refused_by_deploy_too,
    a_role_in_schema_sql_is_refused_before_any_shadow,
    a_role_a_database_ran_is_refused_before_its_replay,
    an_object_the_comparison_does_not_read_is_not_a_match,
    a_failed_concurrent_index_leaves_nothing,
    a_failed_lone_statement_may_leave_its_change,
    a_platforms_objects_are_left_out,
    reylo_run_runs_inline_sql_and_files,
    history_push_leaves_a_data_migration_pending,
    a_platforms_default_privileges_are_not_drift,
    function_privileges_and_index_comments_are_compared,
    a_comment_made_by_hand_stops_nothing,
    a_comment_in_schema_sql_is_written,
    a_server_setting_change_misses_the_cache,
    a_migrations_session_setting_is_never_carried_by_the_cache,
    init_reads_a_function_dumped_before_the_table_it_reads,
    init_locks_a_schema_that_uses_a_hash_operator,
    init_locks_a_schema_with_a_carriage_return_in_a_string,
    a_platforms_schemas_and_extensions_are_in_every_shadow,
    a_platforms_extension_in_pg_catalog_is_not_the_projects,
    a_role_that_is_no_superuser_runs_every_command,
    a_platform_roles_default_privileges_are_the_platforms,
    a_skipped_grant_runs_after_the_rest,
    a_skipped_migration_found_by_what_a_later_one_changes,
    a_skipped_migration_moves_only_where_both_orders_agree_before_what_follows,
    a_skipped_function_moves_past_no_column_it_fills,
    a_skipped_migration_moves_past_no_partition_it_leaves,
    a_skipped_function_moves_past_no_index_it_keys,
    a_skipped_migration_is_found_where_another_changes_the_same_column,
    a_history_out_of_the_locks_order_moves_nothing_more,
    the_history_table_takes_no_default_privileges,
    a_role_that_cannot_create_databases_is_told_of_shadow_url,
    a_public_schema_its_role_owns_is_a_new_databases,
    a_servers_hint_is_in_its_error,
    init_refused_leaves_no_project_behind,
    what_a_superuser_out_of_reach_owns_is_the_platforms,
    tables_a_superuser_out_of_reach_made_are_the_projects,
    the_platforms_objects_are_in_every_shadow_as_a_role_of_its_own,
    a_run_never_makes_a_role,
    a_projects_extension_in_pg_catalog_is_written,
    a_hypertable_is_said_to_be_unread,
    a_dump_makes_a_schema_its_extensions_share_once,
    init_introspect_and_new_need_no_pg_dump,
    introspect_with_no_check_and_no_pg_dump_says_how_to_get_one,
    init_reads_every_schema_a_view_crosses,
    introspect_of_some_schemas_is_their_dump,
    the_dump_reads_a_schema_named_with_capitals,
    new_without_schema_sql_replays_beside_the_environments_extensions,
    new_without_schema_sql_makes_an_extension_added_by_hand,
    new_lists_none_of_an_extensions_own_objects,
    new_without_schema_sql_keeps_only_the_extensions_the_history_needs,
    new_without_schema_sql_makes_an_extension_no_migration_uses,
    new_without_schema_sql_keeps_an_extension_a_migration_branches_on,
    new_without_schema_sql_makes_an_extension_beside_two_the_history_needs_either_of,
    new_without_schema_sql_refuses_an_extension_in_a_schema_a_migration_renamed,
    new_without_schema_sql_makes_an_extension_in_a_schema_a_migration_makes,
    a_pushed_extension_is_recorded_once_its_migration_makes_it,
    an_extension_in_a_projects_schema_is_the_projects_whoever_owns_it,
    an_extension_a_platform_role_owns_in_a_projects_schema_is_the_projects,
    init_writes_the_privileges_on_an_extensions_schema,
    a_platforms_schemas_by_name_are_left_out_whoever_owns_them,
    a_projects_auth_and_storage_are_its_own_on_a_server_of_no_platform,
    a_change_by_hand_is_shown_alone_beside_a_function_of_many_lines,
    an_enum_value_added_is_ahead_on_the_side_that_added_it,
    a_comment_a_migration_adds_is_the_migrations,
    a_temporary_tables_constraints_and_policies_are_no_drift,
);
on!(Mysql, a_migration_that_names_a_database_is_refused);
on!(
    Postgres,
    a_partition_of_a_run_time_partition_is_data,
    an_extension_in_a_schema_of_its_own_is_dumped_after_it,
    an_extension_left_in_a_schema_of_the_projects_own_stops_no_push,
    a_partition_dropped_at_run_time_is_data,
    a_key_renamed_keeps_a_partition_made_at_run_time,
    a_table_made_partitioned_is_refused_for_what_postgres_cant_change,
    the_lines_that_keep_a_remade_tables_rows_follow_the_change,
    a_remade_tables_sequences_move_past_the_rows_put_back,
    a_partition_kept_as_a_table_of_its_own_keeps_its_rows,
    a_sequence_moved_past_unindexed_rows_asks_for_blocking,
    a_sequence_read_is_checked_without_moving_the_files_sequences,
    init_creates_the_extensions_its_schema_uses,
    init_writes_the_default_privileges_and_grants_a_migration_made,
    init_writes_a_database_of_grants_alone,
    init_writes_the_event_triggers_it_compares,
    init_writes_the_publications_it_compares,
    init_reads_an_in_list_on_a_varchar_column_as_it_reads_back,
    init_reads_a_row_comparison_as_it_reads_back,
    a_dump_keeps_a_function_body_as_written,
    a_history_that_does_not_replay_is_refused_before_anything_changes,
    without_a_lock_a_history_out_of_name_order_is_held_to_name_order,
);
on!(Mysql, a_recurring_event_is_in_the_dump);
on!(Mysql, init_makes_a_function_before_the_view_that_calls_it);
on!(Postgres, init_writes_the_foreign_servers_its_tables_use);
on!(Postgres, init_keeps_a_publications_generated_columns, init_makes_a_wrapper_after_the_functions_it_names, init_writes_a_wrappers_and_servers_grants_owner_and_comment);
on!(Sqlite, a_virtual_tables_own_tables_are_not_in_the_dump, an_external_content_index_is_rebuilt_from_its_rows);
mod on_mysql {
    use super::*;
    on!(Mysql, a_comment_made_by_hand_stops_nothing, a_comment_in_schema_sql_is_written, a_role_that_cannot_create_databases_is_told_of_shadow_url, a_grant_is_written_pushed_and_left_by_no_shadow, grants_an_account_cannot_read_back_are_refused, an_account_or_role_grant_in_schema_sql_is_refused_before_any_shadow);
}
on!(Sqlite, a_rebuild_keeps_every_row_where_foreign_keys_are_on, a_rebuild_keeps_tables_that_reference_each_other, libsqls_own_sql_migrates, a_libsql_history_replays_on_a_branch_without_it, a_table_renamed_keeps_its_rows_key_and_counter, a_turso_engines_own_tables_are_tursos);
mod on_turso {
    use super::*;
    on!(Turso, a_virtual_tables_own_tables_are_not_in_the_dump, an_external_content_index_is_rebuilt_from_its_rows, a_rebuild_keeps_every_row_where_foreign_keys_are_on, a_rebuild_keeps_tables_that_reference_each_other, a_vector_indexs_own_tables_are_libsqls, libsqls_own_sql_migrates, a_libsql_history_replays_on_a_branch_without_it);
}
on!(Mysql, a_failure_that_leaves_part_of_itself_is_seen_by_the_next_deploy);
on!(Mysql, a_table_comment_is_no_clause_of_its_table);
on!(Mysql, a_reserved_word_stays_quoted);
on!(Mysql, a_partition_added_at_run_time_is_data);
on!(
    Postgres,
    a_narrowing_type_change_fails_rather_than_cuts,
    a_number_to_a_short_varchar_fails_rather_than_cuts,
    a_replaced_column_is_a_stated_drop,
    a_rename_to_a_name_that_needs_quotes,
    an_index_left_invalid_by_a_cut_off_build_is_dropped_and_built_again,
    a_renamed_tables_sequence_keeps_counting,
    a_table_renamed_as_it_gains_a_column_keeps_its_rows_and_index,
    an_enum_label_a_partition_uses_is_committed_first,
    an_enum_label_a_default_uses_is_committed_first,
    a_push_that_fails_past_an_enum_commit_keeps_what_it_drops,
    a_push_that_fails_past_an_enum_commit_undoes_its_renames,
    a_foreign_table_is_renamed_as_stated,
    a_partitioned_table_is_renamed_with_its_partitions,
    a_table_renamed_onto_a_views_name_drops_the_view_first,
    partitions_made_at_run_time_are_data,
    the_fingerprint_leaves_out_only_what_is_a_childs,
    a_comment_above_a_cut_off_build_is_read_past,
    table_guesses_name_their_schema,
    a_childs_column_is_renamed_with_its_parents,
    row_counts_are_the_dropped_tables_own,
    a_run_that_did_something_else_than_its_replay_is_found_by_check,
    more_type_changes_that_lose_values_are_named_in_drop,
    a_schemas_table_is_never_read_as_a_renamed_tables_column,
    new_writes_constraints_and_indexes_that_block_no_writes,
    new_refuses_a_table_rewrite_until_blocking,
    deploy_waits_out_a_lock_another_session_holds,
    deploys_at_once_wait_out_a_concurrent_index_build,
    a_deploy_waits_for_the_run_lock_no_longer_than_its_sessions_timeouts,
    new_refuses_a_check_on_a_partitioned_table_until_blocking,
    new_refuses_an_index_built_again_until_blocking,
    a_concurrent_reindex_is_said_to_block_nothing,
    new_builds_a_changed_index_before_it_drops_the_old_one,
    a_changed_unique_index_that_fails_to_build_leaves_the_old_one,
    new_adds_a_key_over_an_index_built_concurrently,
    new_builds_an_index_on_a_materialized_view_concurrently,
    new_renames_an_index_changed_only_in_its_name,
    new_drops_a_foreign_key_on_a_replaced_key_with_it,
    dev_rewinds_a_changed_index,
    a_primary_key_added_over_an_index_says_it_checks_for_nulls,
    new_renames_a_column_under_its_indexes_keys_and_views,
    a_renamed_key_column_whose_collation_changes_keeps_its_foreign_key,
    a_rename_carries_its_constraints_sequence_and_views,
    a_rename_that_cannot_run_first_is_planned_as_before,
    a_renamed_key_column_whose_stored_expression_changes_keeps_its_foreign_key,
    a_name_given_to_a_new_index_keeps_the_old_one_until_the_last_build,
    a_foreign_key_added_back_under_another_name_holds_through_its_keys_swap,
);

#[tokio::test(flavor = "current_thread")]
async fn a_sqlite_numeric_column_made_integer_needs_no_drop() {
    let mut p = P::new(Dialect::Sqlite).await.unwrap();
    p.file("20261027100000_t.sql", "create table t (id integer primary key, v real);");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    // SQLite keeps 1.5 in an INTEGER column, so nothing is lost.
    p.schema("create table t (id integer primary key, v integer);");
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "ints", &Default::default(), log).await).await, 0, "{}", p.peek());
}

#[tokio::test(flavor = "current_thread")]
async fn wipe_keeps_full_text_and_rtree_tables_whole() {
    let mut p = P::new(Dialect::Sqlite).await.unwrap();
    p.file("20261027100000_docs.sql", "create virtual table docs using fts5(body);");
    p.file("20261027110000_box.sql", "create virtual table box using rtree(id, x0, x1);");
    p.file("20261027120000_notes.sql", "create table notes (id integer primary key, body text); create virtual table bare using fts5(body, content=''); create virtual table indexed using fts5(body, content='notes', content_rowid='id');");
    let schema = "create virtual table docs using fts5(body); create virtual table box using rtree(id, x0, x1); create table notes (id integer primary key, body text); create virtual table bare using fts5(body, content=''); create virtual table indexed using fts5(body, content='notes', content_rowid='id');";
    p.schema(schema);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into docs values ('hello world'); insert into box values (1, 0, 1); insert into bare values ('hello'); insert into notes values (1, 'hello'); insert into indexed (rowid, body) values (1, 'hello')").await;
    p.output();
    assert_eq!(p.run(async |o, log| reylo_migrate::db_wipe(o, false, log).await).await, 0, "{}", p.peek());
    assert_has!(p.output(), r"Deleted every row of 5 tables");
    assert!(p.rows("select rowid from bare where bare match 'hello'").await.is_empty(), "a contentless index is emptied");
    assert!(p.rows("select rowid from indexed where indexed match 'hello'").await.is_empty(), "an external-content index is emptied with its content");
    assert!(p.rows("select * from docs").await.is_empty() && p.rows("select * from box").await.is_empty());
    // Each still works: their own tables were emptied by them, not under them.
    p.exec("insert into docs values ('again'); insert into box values (2, 0, 1)").await;
    assert_eq!(p.rows("select * from docs where docs match 'again'").await.len(), 1);
    assert_eq!(p.rows("select id from box where x0 <= 0.5 and x1 >= 0.5").await.len(), 1);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
}

#[tokio::test(flavor = "current_thread")]
async fn a_database_whose_default_collation_differs() {
    if let Some(mut p) = P::with_create(Dialect::Mysql, "character set utf8mb4 collate utf8mb4_unicode_ci").await {
        p.schema("create table t (name varchar(10));");
        p.file("20261027100000_t.sql", "create table t (name varchar(10));");
        assert_eq!(p.migrate().await, 0, "the shadows must take the target's default collation:\n{}", p.peek());
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
    }
}

/// A lock that is there and can't be read is refused by every command, never read as no lock, which
/// would deploy a draft with no checksum to hold it to.
#[tokio::test(flavor = "current_thread")]
async fn a_lock_that_cant_be_read_is_refused() {
    let mut p = P::new(Dialect::Sqlite).await.unwrap();
    p.linear();
    p.schema("create table a (id integer);");
    p.file("20261027100000_a.sql", "create table a (id integer);");
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.file("20261027110000_draft.sql", "create table b (id integer);");
    let lock = p.dir.join(reylo_migrate::lock::LOCK);
    std::fs::write(&lock, b"migrations:\n  - name: 20261027100000_a.sql\n    checksum: \xff\n").unwrap();
    p.output();
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), r"✗ Reylo can't read \S*reylo\.lock: [^\n]*UTF-8");
    std::fs::remove_file(&lock).unwrap();
    std::fs::create_dir(&lock).unwrap();
    assert_eq!(p.migrate().await, 1);
    assert_eq!(p.status().await, 1);
    assert_eq!(p.dev().await, 1);
    assert_eq!(p.output().matches("Reylo can't read").count(), 3);
    assert_eq!(p.history().await, ["20261027100000_a.sql"]);
}

/// A lock a newer Reylo wrote is refused by every command, and none writes over it: history pull
/// and lock would otherwise write it again from one database's history, by this version's rules,
/// losing what that database hasn't run.
#[tokio::test(flavor = "current_thread")]
async fn a_lock_a_newer_reylo_wrote_is_refused_and_kept() {
    let mut p = P::new(Dialect::Sqlite).await.unwrap();
    p.linear();
    p.schema("create table a (id integer);");
    p.file("20261027100000_a.sql", "create table a (id integer);");
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.file("20261027110000_b.sql", "create table b (id integer);");
    let lock = p.dir.join(reylo_migrate::lock::LOCK);
    let newer = "version: 2\nmigrations:\n  - name: 20261027100000_a.sql\n    checksum: 0123abcd\n  - name: 20261027110000_b.sql\n    checksum: 4567cdef\n    after: 20261027100000_a.sql\n";
    std::fs::write(&lock, newer).unwrap();
    p.output();
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), r"✗ A newer Reylo wrote \S*reylo\.lock[\s\S]*It is a lock of version 2, and Reylo \S+ reads version 1 and older\.[\s\S]*Upgrade Reylo to read it\.");
    assert_eq!(p.status().await, 1);
    assert_has!(p.output(), r"✗ A newer Reylo wrote");
    assert_eq!(p.dev().await, 1);
    assert_has!(p.output(), r"✗ A newer Reylo wrote");
    assert_eq!(p.run(async |o, log| reylo_migrate::history_pull(o, log).await).await, 1);
    assert_has!(p.output(), r"✗ A newer Reylo wrote");
    assert_eq!(p.run(async |o, log| reylo_migrate::lock_from_history(o, log).await).await, 1);
    assert_has!(p.output(), r"✗ A newer Reylo wrote");
    assert_eq!(std::fs::read_to_string(&lock).unwrap(), newer);
    assert_eq!(p.history().await, ["20261027100000_a.sql"]);
}

/// mysqldump writes a view as one statement over several executable comments, which MySQL runs:
/// each migration runs it, and a database that loses it is told apart. A comment gated above the
/// server's version runs nothing, and is no error.
#[tokio::test(flavor = "current_thread")]
async fn a_view_mysqldump_wrote_in_executable_comments_runs() {
    let Some(mut p) = P::new(Dialect::Mysql).await else { return };
    let view = "/*!50001 CREATE ALGORITHM=UNDEFINED */\n/*!50013 SQL SECURITY INVOKER */\n/*!50001 VIEW `v` AS select `t`.`id` AS `id` from `t` */;\n";
    p.linear();
    p.schema(&format!("create table t (id bigint primary key);\n{view}"));
    p.file("20261027100000_t.sql", "create table t (id bigint primary key);");
    p.file("20261027110000_v.sql", view);
    p.file("20261027120000_later.sql", "/*!99999 create table later (id bigint) */;");
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    let tables = |rows: Vec<Row>| rows.iter().map(|r| format!("{} {}", r.text("name"), r.text("kind"))).collect::<Vec<_>>();
    let listed = "select table_name as name, table_type as kind from information_schema.tables where table_schema = database() and table_name <> '__reylo_migrations' order by table_name";
    assert_eq!(tables(p.rows(listed).await), ["t BASE TABLE", "v VIEW"]);
    p.exec("/*!99999 select 1 */").await;
    p.output();
    assert_eq!(p.status().await, 0, "{}", p.peek());
    p.output();
    p.exec("drop view v").await;
    assert_eq!(p.status().await, 1);
    assert_has!(p.output(), r"✗ This database was changed outside its migrations\n\n[\s\S]*\n  - view v as select `t`\.`id` AS `id` from `t`");
}

/// A COMMIT behind a comment gated above the server's version runs on that server, which skips the
/// comment: the migration is refused as that server reads it, before any of it runs.
#[tokio::test(flavor = "current_thread")]
async fn a_commit_a_server_reads_past_a_version_comment_is_refused() {
    let Some(mut p) = P::new(Dialect::Mysql).await else { return };
    p.file("20261027100000_t.sql", "create table t (id bigint primary key);");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.file("20261027110000_rows.sql", "insert into t values (1);\n/*!99999 SELECT */ COMMIT;\ninsert into t values (1);");
    p.output();
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), r"✗ Migration 20261027110000_rows\.sql can't run atomically\n  It has its own commit");
    assert!(p.rows("select id from t").await.is_empty(), "none of it ran");
    assert_eq!(p.history().await, ["20261027100000_t.sql"]);
}

/// A server that skips a version comment ends it at its `*/` without reading a `#` in it, and runs
/// the COMMIT after it: the migration is refused as that server reads it, before any of it runs.
#[tokio::test(flavor = "current_thread")]
async fn a_commit_after_a_skipped_comment_that_holds_a_line_comment_is_refused() {
    let Some(mut p) = P::new(Dialect::Mysql).await else { return };
    p.file("20261027100000_t.sql", "create table t (id bigint primary key);");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.file("20261027110000_rows.sql", "insert into t values (1);\nSELECT 1 /*!99999 # */; COMMIT;\ninsert into t values (1);");
    p.output();
    assert_eq!(p.migrate().await, 1);
    assert_has!(p.output(), r"✗ Migration 20261027110000_rows\.sql can't run atomically\n  It has its own commit");
    assert!(p.rows("select id from t").await.is_empty(), "none of it ran");
    assert_eq!(p.history().await, ["20261027100000_t.sql"]);
}

/// A change of a column's type that can't hold one of its values, written where the server's
/// sql_mode is strict, deployed on a session whose sql_mode isn't (a server set so, as a host's
/// session may be): it fails on that value and keeps the row, as under strict mode, rather than
/// storing another value in its place. Each change runs on its own, since one statement fails on
/// the first.
#[tokio::test(flavor = "current_thread")]
async fn a_change_that_cant_hold_a_value_fails_on_it_whatever_the_sql_mode() {
    for (before, after, row, error) in [
        ("enum('sad','ok','happy') not null default 'ok'", "enum('ok','happy') not null default 'ok'", "'sad'", r"Data truncated for column 'c' at row 1"),
        ("set('a','b','c')", "set('a','b')", "'a,c'", r"Data truncated for column 'c' at row 1"),
        ("varchar(10)", "varchar(3)", "'abcdefghij'", r"Data truncated for column 'c' at row 1"),
        ("int", "smallint", "100000", r"Out of range value for column 'c' at row 1"),
    ] {
        let Some(mut p) = P::new(Dialect::Mysql).await else { return };
        p.schema(&format!("create table t (id int primary key, c {before});"));
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "t", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec(&format!("insert into t values (1, {row})")).await;
        let kept = p.rows("select cast(c as char) as c from t").await[0].text("c").to_string();
        p.schema(&format!("create table t (id int primary key, c {after});"));
        // The change copies the table, which --blocking accepts.
        let blocking = reylo_migrate::Choices { blocking: true, ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "narrow", &blocking, log).await).await, 0, "{before} to {after}: {}", p.peek());
        p.relock();
        let mut db = Db::connect(&p.url, &p.dir).await.unwrap();
        db.exec("set session sql_mode = 'NO_ENGINE_SUBSTITUTION'").await.unwrap();
        let ran = reylo_migrate::deploy_on_host(&mut db, &p.opts()).await.unwrap();
        let mode = db.rows("select @@session.sql_mode as m").await.unwrap()[0].text("m").to_string();
        db.close().await;
        let out = ran.lines.join("\n");
        assert_eq!(ran.code, 1, "{before} to {after} must fail on {row}:\n{out}");
        assert_has!(&out, error);
        assert_eq!(p.rows("select cast(c as char) as c from t").await[0].text("c"), kept, "{before} to {after} kept the row");
        assert_eq!(mode, "NO_ENGINE_SUBSTITUTION", "the session's own sql_mode is back after the change");
    }
}

/// MySQL has no `create or replace trigger`: a trigger that changes is dropped and made again in one
/// migration, under `lock tables` on its table, so that no write to the table runs between the two
/// without it. A change to the table it needs goes in a migration before. A migration of that shape
/// whose `create trigger` fails unlocks the session's tables, records nothing, and says its first
/// statements may be in place.
#[tokio::test(flavor = "current_thread")]
async fn a_changed_trigger_is_made_again_under_a_lock_on_its_table() {
    let Some(mut p) = P::new(Dialect::Mysql).await else { return };
    let table = "create table users (id int primary key, n int not null default 0, updated_at datetime);\n";
    p.schema(&format!("{table}create trigger users_touch before update on users for each row set new.updated_at = now();\n"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "users", &Default::default(), log).await).await, 0, "{}", p.peek());
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.exec("insert into users (id) values (1)").await;
    let read = |p: &P| {
        let path = p.dir.join("migrations").join(p.files().last().unwrap());
        std::fs::read_to_string(path).unwrap()
    };
    let before = p.files().len();
    p.schema(&format!("{table}create trigger users_touch before update on users for each row set new.updated_at = utc_timestamp();\n"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "change_trigger", &Default::default(), log).await).await, 0, "{}", p.peek());
    assert_eq!(p.files().len(), before + 1, "one migration for the change:\n{}", p.peek());
    assert_eq!(read(&p), "lock tables users write;\ndrop trigger users_touch;\ncreate trigger users_touch before update on users for each row set new.updated_at = utc_timestamp();\nunlock tables;\n");
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_eq!(p.status().await, 0, "{}", p.peek());
    let body = "select action_statement as s from information_schema.triggers where trigger_schema = database() and trigger_name = 'users_touch'";
    assert_eq!(p.rows(body).await[0].text("s"), "set new.updated_at = utc_timestamp()");

    // With a column the new trigger sets: the column first, under the lock with the swap.
    let table = "create table users (id int primary key, n int not null default 0, updated_at datetime, touches int not null default 0);\n";
    p.schema(&format!("{table}create trigger users_touch before update on users for each row set new.touches = old.touches + 1;\n"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "count_touches", &Default::default(), log).await).await, 0, "{}", p.peek());
    assert_eq!(p.files().len(), before + 2, "{}", p.peek());
    assert_eq!(read(&p), "lock tables users write;\nalter table users add column touches int not null default '0';\ndrop trigger users_touch;\ncreate trigger users_touch before update on users for each row set new.touches = old.touches + 1;\nunlock tables;\n");
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.exec("update users set n = n + 1").await;
    assert_eq!(p.rows("select touches from users").await[0].text("touches"), "1");

    // A column renamed that the old trigger reads: the rename goes under the lock with the swap, so
    // no write meets the old trigger on the renamed column.
    let table = "create table users (id int primary key, n int not null default 0, updated_at datetime, hits int not null default 0);\n";
    p.schema(&format!("{table}create trigger users_touch before update on users for each row set new.hits = old.hits + 1;\n"));
    let before = p.files().len();
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "rename_touches", &Default::default(), log).await).await, 0, "{}", p.peek());
    assert_eq!(p.files().len(), before + 1, "{}", p.peek());
    assert!(read(&p).ends_with("lock tables users write;\nalter table users rename column touches to hits;\ndrop trigger users_touch;\ncreate trigger users_touch before update on users for each row set new.hits = old.hits + 1;\nunlock tables;\n"), "{}", read(&p));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.exec("update users set n = n + 1").await;
    assert_eq!(p.rows("select hits from users").await[0].text("hits"), "2");

    // A swap whose create fails, run on a session the test keeps, which must come back unlocked.
    p.file("20991231000000_bad_trigger.sql", "lock tables users write;\ndrop trigger users_touch;\ncreate trigger users_touch before update on users for each row set new.missing = 1;\nunlock tables;\n");
    p.relock();
    let mut db = Db::connect(&p.url, &p.dir).await.unwrap();
    let ran = reylo_migrate::deploy_on_host(&mut db, &p.opts()).await.unwrap();
    let out = ran.lines.join("\n");
    assert_eq!(ran.code, 1, "{out}");
    assert_has!(&out, r"Unknown column 'missing' in 'NEW'");
    assert_has!(&out, r"MySQL commits each statement, so its first statements may be in place");
    let recorded = db.rows("select count(*) as n from __reylo_migrations where name = '20991231000000_bad_trigger.sql'").await.unwrap_or_else(|e| panic!("the session's tables are still locked: {e}"));
    assert_eq!(recorded[0].text("n"), "0");
    db.close().await;
}

/// Tables a migration makes, with keys between them, which MySQL adds as statements after both are
/// made: the tables have no rows, so nothing blocks, in the migration's check or in a dry run.
#[tokio::test(flavor = "current_thread")]
async fn new_tables_with_keys_between_them_need_no_blocking() {
    let Some(mut p) = P::new(Dialect::Mysql).await else { return };
    p.schema("create table users (id int primary key, best_post int);\ncreate table posts (id int primary key, user_id int, foreign key (user_id) references users (id));\nalter table users add foreign key (best_post) references posts (id);\n");
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "base", &Default::default(), log).await).await, 0, "{}", p.peek());
    p.output();
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    assert!(!p.output().contains(" It "), "nothing pending blocks");
    assert_eq!(p.dev().await, 0, "{}", p.peek());
}

/// A table renamed in a pending migration keeps its rows: the dry run says what a later pending
/// migration blocks on it, as on a table of the old name.
#[tokio::test(flavor = "current_thread")]
async fn a_dry_run_says_what_blocks_a_table_renamed_before() {
    let Some(mut p) = P::new(Dialect::Mysql).await else { return };
    p.file("20261027100000_users.sql", "create table users (id int primary key, n bigint);");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into users values (1, 2)").await;
    p.file("20261027110000_rename.sql", "rename table users to accounts;");
    p.file("20261027120000_narrow.sql", "alter table accounts modify column n int;");
    p.schema("create table accounts (id int primary key, n int);");
    p.output();
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"_narrow\.sql\n      ! It rewrites accounts, copying every row while it blocks writes to it");
}

/// A dry run says what each pending migration drops, one written by hand too, with what this
/// database holds of it; a column renamed keeps its rows, and isn't said to be dropped.
#[tokio::test(flavor = "current_thread")]
async fn a_dry_run_says_what_pending_migrations_drop() {
    for d in [Dialect::Postgres, Dialect::Mysql, Dialect::Sqlite] {
        let Some(mut p) = P::new(d).await else { continue };
        p.file("20261027100000_users.sql", "create table users (id int primary key, bio varchar(20), nick varchar(20), n bigint);");
        p.file("20261027100001_tags.sql", "create table tags (id int primary key);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into users values (1, 'a', null, 1); insert into users values (2, null, null, 2); insert into users values (3, 'c', null, 3); insert into tags values (1)").await;
        p.file("20261027110000_drop_bio.sql", "alter table users drop column bio;");
        p.file("20261027110001_drop_nick.sql", "alter table users drop column nick;");
        p.file("20261027110002_drop_tags.sql", "drop table tags;");
        p.file("20261027110003_rename_n.sql", "alter table users rename column n to amount;");
        p.schema("create table users (id int primary key, amount bigint);\n");
        p.output();
        assert_eq!(p.dry_run().await, 0, "{d:?}: {}", p.peek());
        let out = p.output();
        assert_has!(&out, r"_drop_bio\.sql\n      ! It drops users\.bio, which holds a value in 2 rows on this database\n");
        assert_has!(&out, r"_drop_nick\.sql\n      ! It drops users\.nick, which holds no value on this database\n");
        assert_has!(&out, r"_drop_tags\.sql\n      ! It drops the table tags and its 1 row on this database\n");
        assert!(out.trim_end().ends_with("_rename_n.sql"), "{d:?}: a rename keeps its rows: {out}");
    }
}

/// A dry run checks each constraint a pending migration adds against the rows this database holds,
/// and fails where some break it, listing them. One whose table a pending migration changes the
/// rows of first is left to the deploy: the rows it meets are those the change leaves.
#[tokio::test(flavor = "current_thread")]
async fn a_dry_run_checks_new_constraints_against_this_databases_rows() {
    for d in [Dialect::Postgres, Dialect::Mysql] {
        let Some(mut p) = P::new(d).await else { continue };
        p.file("20261027100000_teams.sql", "create table teams (id int primary key);");
        p.file("20261027100001_users.sql", "create table users (id int primary key, email varchar(100), plan varchar(10), team_id int);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into teams values (1); insert into users values (1, 'a@x', 'free', 1); insert into users values (2, 'a@x', 'gold', null); insert into users values (3, null, 'pro', 9)").await;
        p.file("20261027110000_plan.sql", "alter table users add constraint users_plan check (plan in ('free', 'pro'));");
        p.file("20261027110001_email.sql", "create unique index users_email on users (email);");
        p.file("20261027110002_team.sql", "alter table users add constraint users_team foreign key (team_id) references teams (id);");
        let not_null = if d == Dialect::Mysql { "alter table users modify email varchar(100) not null;" } else { "alter table users alter column email set not null;" };
        p.file("20261027110003_email_not_null.sql", not_null);
        p.schema("create table teams (id int primary key);\ncreate table users (id int primary key, email varchar(100) not null, plan varchar(10), team_id int, constraint users_plan check (plan in ('free', 'pro')), constraint users_team foreign key (team_id) references teams (id));\ncreate unique index users_email on users (email);\n");
        p.output();
        assert_eq!(p.dry_run().await, 1, "{d:?}: {}", p.peek());
        let out = p.output();
        assert_has!(&out, r"_plan\.sql\n(      ! [^\n]*\n)*      ✗ It fails on this database: 1 row breaks users_plan\n        List them:\n          select \* from [^\n]+\n");
        assert_has!(&out, r"_email\.sql\n(      ! [^\n]*\n)*      ✗ It fails on this database: 1 value repeats in users_email\n");
        assert_has!(&out, r"_team\.sql\n(      ! [^\n]*\n)*      ✗ It fails on this database: 1 row breaks users_team\n");
        assert_has!(&out, r"_email_not_null\.sql\n(      ! [^\n]*\n)*      ✗ It fails on this database: 1 row has nulls in users\.email\n");
        assert_has!(&out, r"✗ The deploy would fail on rows this database holds\n");

        // A backfill before them: the constraints meet the rows it leaves, which no read of this
        // database can say, so the deploy is left to find out.
        p.file("20261027105959_backfill.sql", "update users set plan = 'free', email = concat('u', id, '@x'), team_id = 1;");
        p.output();
        assert_eq!(p.dry_run().await, 0, "{d:?}: {}", p.peek());
        assert!(!p.output().contains("✗"), "{d:?}: the backfill changes the rows first");
    }
}

/// A table dropped and made again as it was reads as unchanged in the catalog, while its rows are
/// gone: the dry run says so, from the migration's statements.
#[tokio::test(flavor = "current_thread")]
async fn a_dry_run_says_a_table_made_again_loses_its_rows() {
    for d in [Dialect::Postgres, Dialect::Sqlite] {
        let Some(mut p) = P::new(d).await else { continue };
        let logs = "create table logs (id int primary key, note varchar(20));";
        p.file("20261027100000_logs.sql", logs);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into logs values (1, 'a'); insert into logs values (2, 'b')").await;
        p.file("20261027110000_reset_logs.sql", &format!("drop table logs;\n{logs}"));
        p.schema(logs);
        p.output();
        assert_eq!(p.dry_run().await, 0, "{d:?}: {}", p.peek());
        assert_has!(p.output(), r"_reset_logs\.sql\n(      ! [^\n]*\n)*      ! It drops the table logs and its 2 rows on this database");
    }
}

/// A composite unique key is checked on every column of it; a foreign key whose parent rows a
/// pending migration adds first is left to the deploy, which meets them; and so is a NOT NULL on a
/// column dropped and added again, which holds none of the target's values.
#[tokio::test(flavor = "current_thread")]
async fn a_dry_run_checks_composite_keys_and_waits_on_a_parent_filled_first() {
    for d in [Dialect::Postgres, Dialect::Mysql] {
        let Some(mut p) = P::new(d).await else { continue };
        p.file("20261027100000_pairs.sql", "create table pairs (id int primary key, a int, b int);");
        p.file("20261027100001_teams.sql", "create table teams (id int primary key);");
        p.file("20261027100002_members.sql", "create table members (id int primary key, team_id int);");
        p.file("20261027100003_tags.sql", "create table tags (id int primary key, status varchar(20));");
        p.file("20261027100004_codes.sql", "create table codes (id int primary key, code varchar(10));");
        p.file("20261027100005_holders.sql", "create table holders (id int primary key, code varchar(10));");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into codes values (1, 'a'); insert into holders values (1, 'x')").await;
        // A key whose referenced column is dropped and added again meets the values that leaves.
        p.file("20261027105800_drop_code.sql", "alter table codes drop column code;");
        p.file("20261027105801_add_code.sql", "alter table codes add column code varchar(10) not null default 'x';");
        p.file("20261027105802_code_key.sql", "alter table codes add constraint codes_code unique (code);");
        p.file("20261027105803_holders_code.sql", "alter table holders add constraint holders_code foreign key (code) references codes (code);");
        p.exec("insert into pairs values (1, 1, 1); insert into pairs values (2, 1, 1); insert into pairs values (3, 1, null); insert into pairs values (4, 1, 2); insert into teams values (1); insert into members values (1, 9); insert into tags values (1, null)").await;
        // A column dropped and added again under its name holds none of the target's values.
        p.file("20261027105900_drop_status.sql", "alter table tags drop column status;");
        p.file("20261027105901_add_status.sql", "alter table tags add column status varchar(20) not null default 'active';");
        p.file("20261027110000_pairs_ab.sql", "create unique index pairs_ab on pairs (a, b);");
        p.file("20261027110001_team_9.sql", "insert into teams values (9);");
        p.file("20261027110002_members_team.sql", "alter table members add constraint members_team foreign key (team_id) references teams (id);");
        p.schema("create table pairs (id int primary key, a int, b int);\ncreate unique index pairs_ab on pairs (a, b);\ncreate table teams (id int primary key);\ncreate table members (id int primary key, team_id int, constraint members_team foreign key (team_id) references teams (id));\ncreate table tags (id int primary key, status varchar(20) not null default 'active');\ncreate table codes (id int primary key, code varchar(10) not null default 'x', constraint codes_code unique (code));\ncreate table holders (id int primary key, code varchar(10), constraint holders_code foreign key (code) references codes (code));\n");
        p.output();
        assert_eq!(p.dry_run().await, 1, "{d:?}: {}", p.peek());
        let out = p.output();
        assert_has!(&out, r"_pairs_ab\.sql\n(      ! [^\n]*\n)*      ✗ It fails on this database: 1 value repeats in pairs_ab\n");
        assert!(!out.contains("nulls in tags.status"), "{d:?}: the column added again holds none of the old values: {out}");
        assert!(!out.contains("breaks holders_code"), "{d:?}: the referenced column added again holds none of the old values: {out}");
        assert!(!out.contains("breaks members_team"), "{d:?}: the parent row comes first: {out}");
    }
}

/// On Postgres: a check whose function writes is read in a read-only transaction, which refuses the
/// write, so the target is left as it was and the check unsaid; a type change that rewrites values
/// before a check of the same file meets them is left to the deploy; and a NOT NULL added `not valid`
/// holds no row to it until a later migration validates it.
#[tokio::test(flavor = "current_thread")]
async fn a_dry_run_reads_postgres_rows_only_as_the_deploy_meets_them() {
    let Some(mut p) = P::new(Dialect::Postgres).await else { return };
    let version: u32 = p.rows("show server_version_num").await[0].text("server_version_num").parse().unwrap();
    let base = "create table audit (n int);\ncreate function plan_ok(p text) returns boolean language plpgsql as $$ begin insert into audit values (1); return p in ('free', 'pro'); end $$;\ncreate table users (id int primary key, plan text, email text);";
    p.file("20261027100000_base.sql", base);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into users values (1, 'Free', null), (2, 'gold', 'a@x')").await;
    p.file("20261027110002_plan_ok.sql", "alter table users add constraint users_plan_ok check (plan_ok(plan));");
    p.file("20261027110003_lower_plan.sql", "alter table users alter column plan type varchar(10) using lower(plan);\nalter table users add constraint users_plan_lower check (plan = lower(plan));");
    let mut schema = format!("{base}\nalter table users alter column plan type varchar(10);\nalter table users add constraint users_plan_ok check (plan_ok(plan));\nalter table users add constraint users_plan_lower check (plan = lower(plan));\n");
    if version >= 180000 {
        p.file("20261027110000_email_not_valid.sql", "alter table users add constraint users_email_nn not null email not valid;");
        p.file("20261027110001_email_validate.sql", "alter table users validate constraint users_email_nn;");
        schema.push_str("alter table users add constraint users_email_nn not null email;\n");
    }
    p.schema(&schema);
    p.output();
    let code = p.dry_run().await;
    let out = p.output();
    assert_eq!(p.rows("select count(*) as n from audit").await[0].text("n"), "0", "the target is left as it was: {out}");
    assert!(!out.contains("users_plan_ok") && !out.contains("users_plan_lower"), "{out}");
    if version >= 180000 {
        assert_eq!(code, 1, "{out}");
        assert_has!(&out, r"_email_not_valid\.sql\n    · \d+_email_validate\.sql\n(      ! [^\n]*\n)*      ✗ It fails on this database: 1 row has nulls in users\.email\n");
    } else {
        assert_eq!(code, 0, "{out}");
    }
}

/// A check beside a drop of another column of its table, in one migration on Postgres and in two
/// on MySQL, is checked against the target's rows: the column it reads is as the target has it.
#[tokio::test(flavor = "current_thread")]
async fn a_dry_run_checks_a_constraint_beside_a_drop() {
    for d in [Dialect::Postgres, Dialect::Mysql] {
        let Some(mut p) = P::new(d).await else { continue };
        p.file("20261027100000_users.sql", "create table users (id int primary key, bio varchar(20), plan varchar(10));");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into users values (1, 'a', 'free'); insert into users values (2, null, 'team')").await;
        // A backtick inside a string names no column.
        let check = "alter table users add constraint users_plan_check check (plan in ('free', 'pro', '`code`'));";
        if d == Dialect::Mysql {
            p.file("20261027110000_drop_bio.sql", "alter table users drop column bio;");
            p.file("20261027110001_plan_check.sql", check);
        } else {
            p.file("20261027110001_plan_check.sql", &format!("alter table users drop column bio;\n{check}"));
        }
        p.schema("create table users (id int primary key, plan varchar(10), constraint users_plan_check check (plan in ('free', 'pro', '`code`')));\n");
        p.output();
        assert_eq!(p.dry_run().await, 1, "{d:?}: {}", p.peek());
        assert_has!(p.output(), r"_plan_check\.sql\n(      ! [^\n]*\n)*      ✗ It fails on this database: 1 row breaks users_plan_check\n");
    }
}

/// On Postgres: a check that reads the whole row as well as a column reads every column, so one
/// added before it leaves it to the deploy; and `"Plan"` and `plan` are two columns, so a type change
/// of the one isn't hidden by the other.
#[tokio::test(flavor = "current_thread")]
async fn a_dry_run_reads_postgres_columns_as_postgres_names_them() {
    let Some(mut p) = P::new(Dialect::Postgres).await else { return };
    let base = "create table accounts (id int primary key);\ncreate table plans (id int primary key, \"Plan\" text, plan text);";
    p.file("20261027100000_base.sql", base);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into accounts values (1); insert into plans values (1, 'Free', 'x')").await;
    p.file("20261027110000_flag.sql", "alter table accounts add column flag boolean;");
    p.file("20261027110001_flag_check.sql", "alter table accounts add constraint accounts_flag check (id > 0 and to_jsonb(accounts.*) ? 'flag');");
    p.file("20261027110002_plan_lower.sql", "alter table plans alter column \"Plan\" type varchar(10) using lower(\"Plan\");\nalter table plans add constraint plans_plan_lower check (\"Plan\" = lower(\"Plan\"));");
    p.schema("create table accounts (id int primary key, flag boolean, constraint accounts_flag check (id > 0 and to_jsonb(accounts.*) ? 'flag'));\ncreate table plans (id int primary key, \"Plan\" varchar(10), plan text, constraint plans_plan_lower check (\"Plan\" = lower(\"Plan\")));\n");
    p.output();
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    let out = p.output();
    assert!(!out.contains("✗"), "{out}");
}

/// On Postgres: a drop names its table by schema, so one of two tables of a name is said to be
/// dropped alone; a `check … no inherit` holds its own table's rows; and a NOT NULL on a partitioned
/// table after a pending migration detaches a partition meets the rows the partitions left keep.
#[tokio::test(flavor = "current_thread")]
async fn a_dry_run_reads_postgres_tables_by_schema_and_partition() {
    let Some(mut p) = P::new(Dialect::Postgres).await else { return };
    let base = "create schema billing;\ncreate schema analytics;\ncreate table billing.logs (id int primary key);\ncreate table analytics.logs (id int primary key);\ncreate table users (id int primary key, plan text);\ncreate table m (id int, k int, v int) partition by list (k);\ncreate table m1 partition of m for values in (1);\ncreate table m2 partition of m for values in (2);";
    p.file("20261027100000_base.sql", base);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.exec("insert into billing.logs values (1), (2); insert into analytics.logs values (1); insert into users values (1, 'gold'); insert into m values (1, 1, null), (2, 2, 5)").await;
    p.file("20261027110000_reset_billing_logs.sql", "drop table billing.logs;\ncreate table billing.logs (id int primary key);");
    p.file("20261027110001_plan_free.sql", "alter table users add constraint users_plan_free check (plan = 'free') no inherit;");
    p.file("20261027110002_detach_m1.sql", "alter table m detach partition m1;");
    p.file("20261027110003_v_not_null.sql", "alter table m alter column v set not null;");
    p.schema("create schema billing;\ncreate schema analytics;\ncreate table billing.logs (id int primary key);\ncreate table analytics.logs (id int primary key);\ncreate table users (id int primary key, plan text, constraint users_plan_free check (plan = 'free') no inherit);\ncreate table m (id int, k int, v int not null) partition by list (k);\ncreate table m1 (id int, k int, v int);\ncreate table m2 partition of m for values in (2);\n");
    p.output();
    assert_eq!(p.dry_run().await, 1, "{}", p.peek());
    let out = p.output();
    assert_has!(&out, r"_reset_billing_logs\.sql\n(      ! [^\n]*\n)*      ! It drops the table billing\.logs and its 2 rows on this database\n");
    assert!(!out.contains("analytics.logs"), "{out}");
    assert_has!(&out, r"_plan_free\.sql\n(      ! [^\n]*\n)*      ✗ It fails on this database: 1 row breaks users_plan_free\n        List them:\n          select \* from only users ");
    assert!(!out.contains("nulls in m.v"), "the detached partition's rows aren't m's: {out}");
}

/// A trigger swapped under a lock with a change to its table: the change runs first, so one that
/// fails on a row leaves the old trigger as it was; and one MySQL can't make by changing the
/// table's definition alone blocks reads and writes under the lock, so it needs `--blocking`, as
/// the dry run says. The empty copy the check tries it on takes a name no table has.
#[tokio::test(flavor = "current_thread")]
async fn a_trigger_swapped_with_a_change_to_its_table() {
    let Some(mut p) = P::new(Dialect::Mysql).await else { return };
    let table = |columns: &str, by: &str| format!("create table __reylo_lock_probe_0 (id int primary key);\ncreate view __reylo_lock_probe_1 as select 1 as x;\ncreate table users (id int primary key, {columns});\ncreate trigger users_touch before update on users for each row set new.{by} = old.{by} + 1;\n");
    p.schema(&table("n int, mood enum('a','b') not null default 'a', touches int not null default 0", "touches"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "users", &Default::default(), log).await).await, 0, "{}", p.peek());
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.exec("insert into __reylo_lock_probe_0 values (7); insert into users values (1, 0, 'b', 0)").await;
    let blocking = reylo_migrate::Choices { blocking: true, ..Default::default() };
    let read = |p: &P| std::fs::read_to_string(p.dir.join("migrations").join(p.files().last().unwrap())).unwrap();

    // A rename the trigger reads, and a column made not null, which rebuilds the table.
    p.schema(&table("n int not null, mood enum('a','b') not null default 'a', hits int not null default 0", "hits"));
    p.output();
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "rename_hits", &Default::default(), log).await).await, 1, "{}", p.peek());
    assert_has!(p.output(), r"✗ This migration rewrites users, copying every row while it blocks reads and writes to it\n[^✗]*    reylo migrate new rename_hits --blocking");
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "rename_hits", &blocking, log).await).await, 0, "{}", p.peek());
    let sql = read(&p);
    assert!(sql.contains("lock tables users write;\nalter table users") && sql.contains("rename column touches to hits") && sql.ends_with("drop trigger users_touch;\ncreate trigger users_touch before update on users for each row set new.hits = old.hits + 1;\nunlock tables;\n"), "{sql}");
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    assert_has!(p.output(), r"_rename_hits\.sql\n      ! It rewrites users, copying every row while it blocks reads and writes to it");
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    assert_eq!(p.rows("select id from __reylo_lock_probe_0").await.len(), 1, "the table named as the check's copy would be stays");

    // A change that fails on the row: the old trigger stays, and still runs.
    p.schema(&table("n int not null, mood enum('a') not null default 'a', hits int not null default 0", "hits").replace("old.hits + 1", "old.hits + 2"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "drop_b", &blocking, log).await).await, 0, "{}", p.peek());
    assert!(read(&p).starts_with("lock tables users write;\nalter table users modify column mood enum('a')"), "{}", read(&p));
    p.output();
    assert_eq!(p.dev().await, 1);
    assert_has!(p.output(), r"Data truncated for column 'mood' at row 1");
    p.exec("update users set n = n + 1").await;
    assert_eq!(p.rows("select hits from users").await[0].text("hits"), "1", "the old trigger ran");
}

/// A foreign key added to a trigger's table as the trigger changes: under the lock with the swap, the
/// table it references locked too, so the old trigger runs until the swap; and where the key fails on
/// a row, the old trigger stays.
#[tokio::test(flavor = "current_thread")]
async fn a_trigger_swapped_with_a_key_added_to_its_table() {
    let Some(mut p) = P::new(Dialect::Mysql).await else { return };
    let schema = |key: &str, by: u32| format!("create table accounts (id int primary key);\ncreate table users (id int primary key, account_id int, touches int not null default 0{key});\ncreate trigger users_touch before update on users for each row set new.touches = old.touches + {by};\n");
    p.schema(&schema("", 1));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "users", &Default::default(), log).await).await, 0, "{}", p.peek());
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.exec("insert into accounts values (1); insert into users values (1, 2, 0)").await;
    p.schema(&schema(", foreign key (account_id) references accounts (id)", 2));
    // MySQL adds a key by copying the table, which --blocking accepts.
    let blocking = reylo_migrate::Choices { blocking: true, ..Default::default() };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "key", &blocking, log).await).await, 0, "{}", p.peek());
    let sql = std::fs::read_to_string(p.dir.join("migrations").join(p.files().last().unwrap())).unwrap();
    assert!(sql.starts_with("lock tables users write, accounts read;\n") && sql.contains("\nalter table users add constraint users_ibfk_1 foreign key (account_id) references accounts (id);\n") && sql.ends_with("drop trigger users_touch;\ncreate trigger users_touch before update on users for each row set new.touches = old.touches + 2;\nunlock tables;\n"), "{sql}");
    p.output();
    assert_eq!(p.dev().await, 1);
    assert_has!(p.output(), r"foreign key constraint fails");
    p.exec("update users set account_id = 1").await;
    assert_eq!(p.rows("select touches from users").await[0].text("touches"), "1", "the old trigger ran");
}

/// A partition taken out of a MySQL table drops its rows with it, as a Postgres partition does: the
/// migration is refused until `--drop` names it, then drops it in place. One added at the end is
/// added in place.
#[tokio::test(flavor = "current_thread")]
async fn a_partition_taken_out_is_dropped_once_drop_names_it() {
    let Some(mut p) = P::new(Dialect::Mysql).await else { return };
    let table = |parts: &str| format!("create table measurements (id bigint not null, taken_on date not null, primary key (id, taken_on)) partition by range (year(taken_on)) ({parts});\n");
    p.schema(&table("partition p2026 values less than (2027), partition p2027 values less than (2028)"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "measurements", &Default::default(), log).await).await, 0, "{}", p.peek());
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    let read = |p: &P| std::fs::read_to_string(p.dir.join("migrations").join(p.files().last().unwrap())).unwrap();
    p.schema(&table("partition p2026 values less than (2027), partition p2027 values less than (2028), partition pmax values less than maxvalue"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "add_pmax", &Default::default(), log).await).await, 0, "{}", p.peek());
    assert_eq!(read(&p), "alter table measurements add partition (partition pmax values less than maxvalue engine = innodb);\n");
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.exec("insert into measurements values (1, '2026-06-01'), (2, '2027-06-01'), (3, '2031-06-01')").await;
    p.schema(&table("partition p2027 values less than (2028), partition pmax values less than maxvalue"));
    p.output();
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "drop_p2026", &Default::default(), log).await).await, 1);
    assert_has!(p.output(), r"✗ This migration drops the partition measurements\.p2026 and its rows\n\n  To drop it:\n    reylo migrate new drop_p2026 --drop measurements\.p2026\n");
    assert_eq!(p.files().len(), 2, "nothing was written");
    let drop = reylo_migrate::Choices { drop: vec!["measurements.p2026".into()], ..Default::default() };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "drop_p2026", &drop, log).await).await, 0, "{}", p.peek());
    assert_has!(p.output(), r"! Drops the partition measurements\.p2026 and its rows");
    assert_eq!(read(&p), "-- loses data: measurements.p2026\nalter table measurements drop partition p2026;\n");
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    let ids: Vec<String> = p.rows("select id from measurements order by id").await.iter().map(|r| r.text("id").to_string()).collect();
    assert_eq!(ids, ["2", "3"]);
}

/// A column and a partition of one table that share a name, both taken out: `--drop` names neither,
/// since one flag for both would drop the partition's rows for a column's sake.
#[tokio::test(flavor = "current_thread")]
async fn a_drop_that_names_a_column_and_a_partition_alike_is_refused() {
    let Some(mut p) = P::new(Dialect::Mysql).await else { return };
    let parts = "partition by range (id) (partition p1 values less than (10), partition p2 values less than (20))";
    p.schema(&format!("create table t (id int primary key, p1 int) {parts};\n"));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "t", &Default::default(), log).await).await, 0, "{}", p.peek());
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.exec("insert into t values (1, 1), (15, 2)").await;
    p.schema("create table t (id int primary key) partition by range (id) (partition p2 values less than (20));\n");
    p.output();
    let drop = reylo_migrate::Choices { drop: vec!["t.p1".into()], ..Default::default() };
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "both", &drop, log).await).await, 1);
    assert_has!(p.output(), r"✗ The --drop flag names t\.p1, which is both the partition t\.p1 and the column t\.p1\n  Make one of the changes in schema\.sql and write its migration, then the other\.");
    // Nor does the refusal of the drop offer a command that names them alike.
    let drop = reylo_migrate::Choices::default();
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "both", &drop, log).await).await, 1);
    let out = p.output();
    assert_has!(&out, r"✗ This migration drops the partition t\.p1, and the column t\.p1, with their data\n  A partition and a column share the name t\.p1: make one of the changes in schema\.sql and write its migration, then the other\.");
    assert!(!out.contains("--drop t.p1"), "{out}");
    assert_eq!(p.rows("select count(*) as n from t").await[0].text("n"), "2");
}

/// A MySQL change that copies its table while it blocks writes is refused until `--blocking`
/// accepts it, as MySQL itself answers for the statement: a removed enum value, a column's new
/// type, a table partitioned anew. One MySQL makes letting writes through (a value added at an
/// enum's end) is not.
#[tokio::test(flavor = "current_thread")]
async fn a_mysql_change_that_copies_its_table_is_refused_until_blocking() {
    let Some(mut p) = P::new(Dialect::Mysql).await else { return };
    let table = |labels: &str, n: &str, parts: &str| format!("create table users (id int not null, mood enum({labels}) not null default 'ok', n {n}, primary key (id)){parts};\n");
    let blocking = reylo_migrate::Choices { blocking: true, ..Default::default() };
    p.schema(&table("'sad','ok'", "int", ""));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "users", &Default::default(), log).await).await, 0, "{}", p.peek());
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    p.exec("insert into users values (1, 'ok', 2)").await;
    p.schema(&table("'sad','ok','happy'", "int", ""));
    assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "add_happy", &Default::default(), log).await).await, 0, "{}", p.peek());
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    for (name, labels, n, parts, what) in [("remove_sad", "'ok','happy'", "int", "", "rewrites users, copying every row while it blocks writes to it"), ("widen_n", "'ok','happy'", "bigint", "", "rewrites users, copying every row while it blocks writes to it"), ("partition_users", "'ok','happy'", "bigint", " partition by hash (id) partitions 2", "partitions users anew, copying every row while it blocks writes to it")] {
        p.schema(&table(labels, n, parts));
        p.output();
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, name, &Default::default(), log).await).await, 1, "{name}: {}", p.peek());
        assert_has!(p.output(), &format!(r"✗ This migration {what}\n[^✗]*Where the table is small enough to wait for:\n    reylo migrate new {name} --blocking"));
        assert!(p.files().iter().all(|f| !f.contains(name)), "{name}: nothing is written");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, name, &blocking, log).await).await, 0, "{name}: {}", p.peek());
        assert_has!(p.output(), &format!("! It {what}"));
        // The dry run says so of the pending migration, and passes.
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_has!(p.output(), &format!(r"_{name}\.sql\n      ! It {what}"));
        assert_eq!(p.dev().await, 0, "{}", p.peek());
    }
    assert_eq!(p.rows("select mood, n from users").await.iter().map(|r| format!("{} {}", r.text("mood"), r.text("n"))).collect::<Vec<_>>(), ["ok 2"]);
    // A migration written by hand that names its own lock: the dry run says what it blocks all the same.
    p.schema(&table("'ok','happy'", "int", " partition by hash (id) partitions 2"));
    for (name, lock, what) in [("by_hand_default", "lock = default", "rewrites users, copying every row while it blocks writes to it"), ("by_hand_shared", "lock=shared", "rewrites users, copying every row while it blocks writes to it"), ("by_hand_exclusive", "lock=exclusive", "rewrites users, copying every row while it blocks reads and writes to it")] {
        p.file(&format!("20991231000000_{name}.sql"), &format!("alter table users modify column n int, {lock};"));
        p.relock();
        p.output();
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_has!(p.output(), &format!(r"_{name}\.sql\n      ! It {what}"));
        std::fs::remove_file(p.dir.join("migrations").join(format!("20991231000000_{name}.sql"))).unwrap();
    }
}

mod adversarial {
    use super::*;

    /// A table renamed to a name Postgres must quote, or one so long that it shortens the names it
    /// makes from it: what Postgres named after the table takes the name it would make from the new
    /// one, so nothing is made again and the migration reaches the target.
    pub async fn a_table_renamed_to_a_name_postgres_quotes_or_shortens_keeps_its_constraints(mut p: P) {
        let table = |name: &str| format!("create table {name} (id integer generated always as identity primary key, email text not null unique, note text check (note <> ''), ref_id integer references refs (id)); create index on {name} (note);");
        let refs = "create table refs (id integer primary key);";
        p.file("20261027100000_t.sql", &format!("{refs} {}", table("ledger")));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into refs values (1); insert into ledger (email, note, ref_id) values ('a', 'n', 1)").await;
        p.output();
        let long = "a".repeat(63);
        // A sequence named by hand as Postgres would once have been thought to name it, which a
        // rename leaves alone.
        let wide = "w".repeat(60);
        let other = |name: &str| format!("create table {name} (id integer generated always as identity (sequence name {wide}_id) primary key);");
        p.file("20261027110000_t.sql", &other(&wide));
        p.schema(&format!("{refs} {} {}", table("ledger"), other(&wide)));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec(&format!("insert into {wide} default values")).await;
        p.output();
        let mut old = "ledger".to_string();
        for (written, named) in [(r#""123""#.to_string(), "123".to_string()), (long.clone(), long.clone()), ("ledger".to_string(), "ledger".to_string())] {
            let before = p.files().len();
            let (other_old, other_new) = if named == "ledger" { ("short".to_string(), wide.clone()) } else if named == "123" { (wide.clone(), "short".to_string()) } else { ("short".to_string(), "short".to_string()) };
            p.schema(&format!("{refs} {} {}", table(&written), other(&other_new)));
            let mut rename = vec![(old.clone(), named.clone())];
            if other_old != other_new {
                rename.push((other_old, other_new));
            }
            old = named.clone();
            let choices = reylo_migrate::Choices { rename, ..Default::default() };
            assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "renamed", &choices, log).await).await, 0, "{}", p.peek());
            let out = p.output();
            assert!(!out.contains("doesn't reach"), "{out}");
            assert!(out.lines().all(|l| !l.trim_start().starts_with("- ") && !l.trim_start().starts_with("+ ")), "nothing is dropped or made again: {out}");
            assert_eq!(p.files().len(), before + 1);
            assert_eq!(p.migrate().await, 0, "{}", p.peek());
            assert_eq!(p.rows(&format!("select email from {written}")).await[0].text("email"), "a");
            p.exec(&format!("insert into {} default values", if named == "ledger" { &wide } else { "short" })).await;
            p.output();
            assert_eq!(p.dry_run().await, 0, "{}", p.peek());
            p.output();
        }
        // An index the target keeps under its name, while another table holds the name a rename
        // would give it; and an identity whose table's name holds a quote, its sequence named by
        // Postgres.
        let kept = format!("{}_x_idx", "c".repeat(57));
        let quoted = format!("q\"{}", "q".repeat(61));
        let written = format!("\"{}\"", quoted.replace('"', "\"\""));
        let rest = format!("{refs} {} {}", table("ledger"), other(&wide));
        p.file("20991231000000_t.sql", &format!("create table {} (x integer); create index on {} (x); create table holder (x integer); create index dest_x_idx on holder (x); create table {written} (id integer generated always as identity, note text);", "c".repeat(63), "c".repeat(63)));
        p.schema(&format!("{rest} create table {} (x integer); create index {kept} on {} (x); create table holder (x integer); create index dest_x_idx on holder (x); create table {written} (id integer generated always as identity, note text);", "c".repeat(63), "c".repeat(63)));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec(&format!("insert into {written} (note) values ('n')")).await;
        p.output();
        p.schema(&format!("{rest} create table dest (x integer); create index {kept} on dest (x); create table holder (x integer); create index dest_x_idx on holder (x); create table plain (id integer generated always as identity, note text);"));
        let choices = reylo_migrate::Choices { rename: vec![("c".repeat(63), "dest".into()), (written.clone(), "plain".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "edges", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into plain (note) values ('m')").await;
        assert_eq!(p.rows("select id from plain order by id").await.iter().map(|r| r.text("id").to_string()).collect::<Vec<_>>(), ["1", "2"]);
        p.output();
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    /// A table renamed with the sequences its columns use, whatever names the target gives others:
    /// each is renamed with its table, keeping its count.
    pub async fn a_table_renamed_keeps_the_sequences_its_columns_use(mut p: P) {
        // A serial's sequence, whose old name the target gives a sequence of its own: the serial's is
        // renamed with its table, keeping its count, and the other made.
        let rest = String::new();
        p.file("20991231000002_t.sql", "create table source (id serial primary key, note text);");
        p.schema(&format!("{rest} create table source (id serial primary key, note text);"));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into source (note) values ('a')").await;
        p.output();
        p.schema(&format!("{rest} create table target (id serial primary key, note text); create sequence source_id_seq;"));
        let choices = reylo_migrate::Choices { rename: vec![("source".into(), "target".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "serial", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into target (note) values ('b')").await;
        assert_eq!(p.rows("select id from target order by id").await.iter().map(|r| r.text("id").to_string()).collect::<Vec<_>>(), ["1", "2"]);
        p.output();
        // A serial's sequence the renamed table's column still takes its default from, though it no
        // longer owns it: renamed, keeping its count.
        let rest = format!("{rest} create table target (id serial primary key, note text); create sequence source_id_seq;");
        p.file("20991231000004_t.sql", "create table origin (id serial primary key, note text);");
        p.schema(&format!("{rest} create table origin (id serial primary key, note text);"));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into origin (note) values ('a')").await;
        p.output();
        p.schema(&format!("{rest} create sequence moved_id_seq as integer; create table moved (id integer default nextval('moved_id_seq') primary key, note text);"));
        let choices = reylo_migrate::Choices { rename: vec![("origin".into(), "moved".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "unowned", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into moved (note) values ('b')").await;
        assert_eq!(p.rows("select id from moved order by id").await.iter().map(|r| r.text("id").to_string()).collect::<Vec<_>>(), ["1", "2"]);
        p.output();
        // A new name that holds the sequence's old one, beside a sequence of its own under the old
        // name; and a sequence the target gives the name a rename would, which the renamed column
        // no longer uses.
        let rest = format!("{rest} create sequence moved_id_seq as integer; create table moved (id integer default nextval('moved_id_seq') primary key, note text);");
        p.file("20991231000006_t.sql", "create table first (id serial primary key, note text); create table second (id serial primary key, note text); create sequence later_id_seq;");
        p.schema(&format!("{rest} create table first (id serial primary key, note text); create table second (id serial primary key, note text); create sequence later_id_seq;"));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into first (note) values ('a'); insert into second (note) values ('a')").await;
        p.output();
        p.schema(&format!("{rest} create table first_id_seq_more (id serial primary key, note text); create sequence first_id_seq; create table later (id integer primary key, note text); create sequence later_id_seq;"));
        let choices = reylo_migrate::Choices { rename: vec![("first".into(), "first_id_seq_more".into()), ("second".into(), "later".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "names", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into first_id_seq_more (note) values ('b')").await;
        assert_eq!(p.rows("select id from first_id_seq_more order by id").await.iter().map(|r| r.text("id").to_string()).collect::<Vec<_>>(), ["1", "2"]);
        p.output();
        // A sequence another column of the table takes values from, which it still does after.
        let rest = format!("{rest} create table first_id_seq_more (id serial primary key, note text); create sequence first_id_seq; create table later (id integer primary key, note text); create sequence later_id_seq;");
        p.file("20991231000008_t.sql", "create table counted (id serial primary key, ticket integer unique default nextval('counted_id_seq'));");
        p.schema(&format!("{rest} create table counted (id serial primary key, ticket integer unique default nextval('counted_id_seq'));"));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into counted (id) values (100)").await;
        p.output();
        p.schema(&format!("{rest} create sequence tallied_id_seq as integer; create table tallied (id integer primary key, ticket integer unique default nextval('tallied_id_seq'));"));
        let choices = reylo_migrate::Choices { rename: vec![("counted".into(), "tallied".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tallied", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into tallied (id) values (101)").await;
        assert_eq!(p.rows("select ticket from tallied order by id").await.iter().map(|r| r.text("ticket").to_string()).collect::<Vec<_>>(), ["1", "2"]);
    }

    /// A table renamed while its columns change which sequence they draw from: the count stays with
    /// the column that draws from it.
    pub async fn a_table_renamed_keeps_the_counter_its_column_draws_from(mut p: P) {
        // A sequence its column owns under the new name, though another column names it under the
        // old: renamed, keeping its count.
        let rest = String::new();
        p.file("20991231000010_t.sql", "create table owned (id serial primary key, seq_ref regclass default 'owned_id_seq'::regclass);");
        p.schema(&format!("{rest} create table owned (id serial primary key, seq_ref regclass default 'owned_id_seq'::regclass);"));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into owned default values").await;
        p.output();
        p.schema(&format!("{rest} create sequence owned_id_seq as integer; create table held (id serial primary key, seq_ref regclass default 'owned_id_seq'::regclass);"));
        let choices = reylo_migrate::Choices { rename: vec![("owned".into(), "held".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "held", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into held default values").await;
        assert_eq!(p.rows("select id from held order by id").await.iter().map(|r| r.text("id").to_string()).collect::<Vec<_>>(), ["1", "2"]);
        p.output();
        // A sequence the column that owned it keeps using, unowned, while the new name goes to a
        // sequence another column owns: the column keeps its count.
        let rest = format!("{rest} create sequence owned_id_seq as integer; create table held (id serial primary key, seq_ref regclass default 'owned_id_seq'::regclass);");
        p.file("20991231000012_t.sql", "create table donor (id serial primary key, ticket integer default nextval('donor_id_seq'));");
        p.schema(&format!("{rest} create table donor (id serial primary key, ticket integer default nextval('donor_id_seq'));"));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into donor (ticket) values (100)").await;
        p.output();
        p.schema(&format!("{rest} create sequence donor_id_seq as integer; create sequence taker_id_seq as integer; create table taker (id integer primary key default nextval('donor_id_seq'), ticket integer default nextval('taker_id_seq')); alter sequence taker_id_seq owned by taker.ticket;"));
        let choices = reylo_migrate::Choices { rename: vec![("donor".into(), "taker".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "taker", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into taker default values").await;
        assert_eq!(p.rows("select id, ticket from taker where id <> 1").await.iter().map(|r| format!("{} {}", r.text("id"), r.text("ticket"))).collect::<Vec<_>>(), ["2 1"]);
        p.output();
        // A sequence its owner stops drawing from, while another column keeps drawing from it under
        // the new name and only names it under the old: that column keeps its count.
        let rest = format!("{rest} create sequence donor_id_seq as integer; create sequence taker_id_seq as integer; create table taker (id integer primary key default nextval('donor_id_seq'), ticket integer default nextval('taker_id_seq')); alter sequence taker_id_seq owned by taker.ticket;");
        p.file("20991231000014_t.sql", "create table giver (id serial primary key, ticket integer unique default nextval('giver_id_seq'), seq_ref regclass default 'giver_id_seq'::regclass);");
        p.schema(&format!("{rest} create table giver (id serial primary key, ticket integer unique default nextval('giver_id_seq'), seq_ref regclass default 'giver_id_seq'::regclass);"));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into giver (id) values (100)").await;
        p.output();
        p.schema(&format!("{rest} create sequence giver_id_seq as integer; create sequence getter_id_seq as integer; create table getter (id integer primary key, ticket integer unique default nextval('getter_id_seq'), seq_ref regclass default 'giver_id_seq'::regclass); alter sequence getter_id_seq owned by getter.ticket;"));
        let choices = reylo_migrate::Choices { rename: vec![("giver".into(), "getter".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "getter", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into getter (id) values (101)").await;
        assert_eq!(p.rows("select ticket from getter order by id").await.iter().map(|r| r.text("ticket").to_string()).collect::<Vec<_>>(), ["1", "2"]);
        p.output();
        // A default that names one sequence and draws from another: the count is the one it draws.
        let rest = format!("{rest} create sequence giver_id_seq as integer; create sequence getter_id_seq as integer; create table getter (id integer primary key, ticket integer unique default nextval('getter_id_seq'), seq_ref regclass default 'giver_id_seq'::regclass); alter sequence getter_id_seq owned by getter.ticket;");
        p.file("20991231000016_t.sql", "create sequence lender_id_seq; create table lender (id text primary key default ('lender_id_seq'::regclass::text || '-' || nextval('lender_id_seq')::text)); alter sequence lender_id_seq owned by lender.id;");
        p.schema(&format!("{rest} create sequence lender_id_seq; create table lender (id text primary key default ('lender_id_seq'::regclass::text || '-' || nextval('lender_id_seq')::text)); alter sequence lender_id_seq owned by lender.id;"));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into lender default values").await;
        p.output();
        p.schema(&format!("{rest} create sequence lender_id_seq; create sequence borrower_id_seq; create table borrower (id text primary key default ('lender_id_seq'::regclass::text || '-' || nextval('borrower_id_seq')::text)); alter sequence borrower_id_seq owned by borrower.id;"));
        let choices = reylo_migrate::Choices { rename: vec![("lender".into(), "borrower".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "borrower", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into borrower default values").await;
        assert_eq!(p.rows("select id from borrower order by id").await.iter().map(|r| r.text("id").to_string()).collect::<Vec<_>>(), ["lender_id_seq-1", "lender_id_seq-2"]);
        p.output();
        // A new name holding a quote, which a default's string literal writes twice.
        let rest = format!("{rest} create sequence lender_id_seq; create sequence borrower_id_seq; create table borrower (id text primary key default ('lender_id_seq'::regclass::text || '-' || nextval('borrower_id_seq')::text)); alter sequence borrower_id_seq owned by borrower.id;");
        p.file("20991231000018_t.sql", "create sequence payer_id_seq; create table payer (id bigint primary key default nextval('payer_id_seq'), seq_ref regclass default 'payer_id_seq'::regclass); alter sequence payer_id_seq owned by payer.id;");
        p.schema(&format!("{rest} create sequence payer_id_seq; create table payer (id bigint primary key default nextval('payer_id_seq'), seq_ref regclass default 'payer_id_seq'::regclass); alter sequence payer_id_seq owned by payer.id;"));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into payer default values").await;
        p.output();
        p.schema(&format!(r#"{rest} create sequence payer_id_seq; create sequence "payee's_id_seq"; create table "payee's" (id bigint primary key default nextval('"payee''s_id_seq"'), seq_ref regclass default 'payer_id_seq'::regclass);"#));
        let choices = reylo_migrate::Choices { rename: vec![("payer".into(), "payee's".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "payee", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec(r#"insert into "payee's" default values"#).await;
        assert_eq!(p.rows(r#"select id from "payee's" order by id"#).await.iter().map(|r| r.text("id").to_string()).collect::<Vec<_>>(), ["1", "2"]);
    }

    /// A refusal's suggested command carries --blocking where the migration it writes rewrites a
    /// table, so that it is not refused again for that: a line of several guesses, and a type change
    /// that loses values.
    pub async fn a_suggested_command_says_blocking_where_its_migration_needs_it(mut p: P) {
        p.file("20261027100000_t.sql", "create table users (id integer primary key, score integer); create table posts (id integer primary key, title varchar(100)); create table events (id integer primary key, starts_at timestamptz);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into users values (1, 7); insert into posts values (1, 'a'); insert into events values (1, now())").await;
        p.output();
        p.schema("create table users (id integer primary key, label text); create table posts (id integer primary key, heading varchar(200)); create table events (id integer primary key, starts_at timestamptz);");
        // integer to text rewrites users; a longer varchar rewrites nothing.
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "both", &Default::default(), log).await).await, 1);
        let out = p.output();
        assert_has!(&out, r"\n    --rename posts\.title=heading +   or   --drop posts\.title\n    --rename users\.score=label --blocking   or   --drop users\.score\n");
        let all = picked(&out, |_| true);
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "both", &choices_of(&all), log).await).await, 0, "{}", p.peek());
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        // A type change that loses values, and rewrites the table.
        p.schema("create table users (id integer primary key, label text); create table posts (id integer primary key, heading varchar(200)); create table events (id integer primary key, starts_at date);");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "dates", &Default::default(), log).await).await, 1);
        let out = p.output();
        assert_has!(&out, r"  To accept that:\n    reylo migrate new dates \\\n      --drop events\.starts_at \\\n      --blocking\n");
        let command = out.lines().skip_while(|l| !l.contains("To accept that")).skip(1).take_while(|l| l.starts_with("    ")).collect::<Vec<_>>().join("\n");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "dates", &choices_of(&command), log).await).await, 0, "{}", p.peek());
    }

    /// A rewrite the suggested commands' --blocking accepts is named before them, though it is no
    /// part of what the refusal asks.
    pub async fn a_suggested_blocking_names_the_rewrite_it_accepts(mut p: P) {
        p.file("20261027100000_t.sql", "create table users (id integer primary key, n integer); create table posts (id integer primary key, title varchar(100));");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into users values (1, 7); insert into posts values (1, 'a')").await;
        p.output();
        p.schema("create table users (id integer primary key, n bigint); create table posts (id integer primary key, heading varchar(200));");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "widen", &Default::default(), log).await).await, 1);
        let out = p.output();
        assert_has!(&out, r"\n\n  As a rename, it rewrites users, copying every row while it blocks reads and writes to it\.\n  As a drop, it rewrites users, copying every row while it blocks reads and writes to it\.\n\n  To rename the column, keeping its rows:\n    reylo migrate new widen \\\n      --rename posts\.title=heading \\\n      --blocking\n  To drop the column and its data, and add a new one:\n    reylo migrate new widen \\\n      --drop posts\.title \\\n      --blocking\n");
    }

    /// A table renamed with a not null column it drops, where the name Postgres would give that
    /// column's not null under the new table is held by another's: the held name stays where it is.
    pub async fn a_not_null_renamed_with_its_table_leaves_a_held_name_alone(mut p: P) {
        p.file("20261027100000_t.sql", "create table accounts (id integer primary key, email text not null, handle text constraint members_email_not_null not null);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into accounts values (1, 'a', 'h')").await;
        p.output();
        p.schema("create table members (id integer primary key, handle text constraint members_email_not_null not null);");
        let choices = reylo_migrate::Choices { drop: vec!["accounts.email".into()], rename: vec![("accounts".into(), "members".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "members", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        let held = p.rows("select a.attname from pg_constraint c join pg_attribute a on a.attrelid = c.conrelid and a.attnum = c.conkey[1] where c.conrelid = 'members'::regclass and c.conname = 'members_email_not_null'").await;
        assert_eq!(held.iter().map(|r| r.text("attname").to_string()).collect::<Vec<_>>(), ["handle"]);
        assert_eq!(p.rows("select handle from members").await[0].text("handle"), "h");
        p.output();
        // The name held by a check the table inherits.
        let members = "create table members (id integer primary key, handle text constraint members_email_not_null not null);";
        p.file("20991231000000_t.sql", "create table parent (handle text, constraint people_email_not_null check (handle <> '')); create table users (email text not null) inherits (parent);");
        p.schema(&format!("{members} create table parent (handle text, constraint people_email_not_null check (handle <> '')); create table users (email text not null) inherits (parent);"));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into users values ('h', 'e')").await;
        p.output();
        p.schema(&format!("{members} create table parent (handle text, constraint people_email_not_null check (handle <> '')); create table people () inherits (parent);"));
        let choices = reylo_migrate::Choices { drop: vec!["users.email".into()], rename: vec![("users".into(), "people".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "people", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select conname from pg_constraint where conrelid = 'people'::regclass and contype = 'c'").await.iter().map(|r| r.text("conname").to_string()).collect::<Vec<_>>(), ["people_email_not_null"]);
        assert_eq!(p.rows("select handle from people").await[0].text("handle"), "h");
        p.output();
        // A new name of mixed case, whose not null's name Postgres would not fold to a held one.
        let people = "create table parent (handle text, constraint people_email_not_null check (handle <> '')); create table people () inherits (parent);";
        let folk = "create table folk (handle text, constraint kin_email_not_null check (handle <> ''));";
        p.file("20991231000002_t.sql", "create table folk (handle text, constraint kin_email_not_null check (handle <> '')); create table relatives (email text not null) inherits (folk);");
        p.schema(&format!("{members} {people} {folk} create table relatives (email text not null) inherits (folk);"));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into relatives values ('h', 'e')").await;
        p.output();
        p.schema(&format!(r#"{members} {people} {folk} create table "Kin" () inherits (folk);"#));
        let choices = reylo_migrate::Choices { drop: vec!["relatives.email".into()], rename: vec![("relatives".into(), "Kin".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "kin", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.rows(r#"select handle from "Kin""#).await[0].text("handle"), "h");
        p.output();
        // A new name that opens with a digit, and one at Postgres's 63 bytes, whose dropped
        // columns' not nulls Postgres would name past its limit.
        let kin = r#"create table "Kin" () inherits (folk);"#;
        let long = "a".repeat(63);
        p.file("20991231000004_t.sql", "create table ledger (note text, email text not null); create table wide (note text, email text not null, handle text not null);");
        p.schema(&format!("{members} {people} {folk} {kin} create table ledger (note text, email text not null); create table wide (note text, email text not null, handle text not null);"));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        p.schema(&format!(r#"{members} {people} {folk} {kin} create table "123" (note text); create table {long} (note text);"#));
        let choices = reylo_migrate::Choices { drop: vec!["ledger.email".into(), "wide.email".into(), "wide.handle".into()], rename: vec![("ledger".into(), "123".into()), ("wide".into(), long.clone())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "names", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        // A column whose name holds quotes, renamed with its table beside a column added; and two
        // columns dropped whose not nulls' names Postgres would cut short alike.
        let before = format!(r#"{members} {people} {folk} {kin} create table "123" (note text); create table {long} (note text);"#);
        let edge = "b".repeat(63);
        p.file("20991231000006_t.sql", r#"create table quoted (id integer primary key, """email""" text default 'missing' not null); create table cut (note text, abcdefghijklmnopqrstuvwxyzxa text not null, abcdefghijklmnopqrstuvwxyzxb text not null);"#);
        p.schema(&format!(r#"{before} create table quoted (id integer primary key, """email""" text default 'missing' not null); create table cut (note text, abcdefghijklmnopqrstuvwxyzxa text not null, abcdefghijklmnopqrstuvwxyzxb text not null);"#));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into quoted values (1, 'original')").await;
        p.output();
        p.schema(&format!("{before} create table renamed (id integer primary key, x text default 'missing' not null, joined integer); create table {edge} (note text);"));
        let choices = reylo_migrate::Choices { drop: vec!["cut.abcdefghijklmnopqrstuvwxyzxa".into(), "cut.abcdefghijklmnopqrstuvwxyzxb".into()], rename: vec![("quoted".into(), "renamed".into()), ("cut".into(), edge.clone())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "edges", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select x from renamed").await[0].text("x"), "original");
        p.output();
        // Of two such columns, one dropped and one renamed beside a column added: the renamed one's
        // not null takes the name Postgres numbers, and the rename is still inferred.
        let before = format!("{before} create table renamed (id integer primary key, x text default 'missing' not null, joined integer); create table {edge} (note text);");
        let wider = "c".repeat(63);
        p.file("20991231000008_t.sql", "create table clash (note text, abcdefghijklmnopqrstuvwxyzxa text default 'missing' not null, abcdefghijklmnopqrstuvwxyzxb text default 'missing' not null);");
        p.schema(&format!("{before} create table clash (note text, abcdefghijklmnopqrstuvwxyzxa text default 'missing' not null, abcdefghijklmnopqrstuvwxyzxb text default 'missing' not null);"));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into clash values ('n', 'discard', 'original')").await;
        p.output();
        p.schema(&format!("{before} create table {wider} (note text, x text default 'missing' not null, joined integer);"));
        let choices = reylo_migrate::Choices { drop: vec!["clash.abcdefghijklmnopqrstuvwxyzxa".into()], rename: vec![("clash".into(), wider.clone())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "clash", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.rows(&format!("select x from {wider}")).await[0].text("x"), "original");
        p.output();
        // A dropped column's not null is given no name a kept column's not null is renamed to.
        let before = format!("{before} create table {wider} (note text, x text default 'missing' not null, joined integer);");
        p.file("20991231000010_t.sql", "create table src (a text not null, b text constraint src_a_not_null1 not null, constraint dst_a_not_null check (b <> ''));");
        p.schema(&format!("{before} create table src (a text not null, b text constraint src_a_not_null1 not null, constraint dst_a_not_null check (b <> ''));"));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into src values ('a', 'b')").await;
        p.output();
        p.schema(&format!("{before} create table dst (b text constraint dst_a_not_null1 not null, constraint dst_a_not_null check (b <> ''));"));
        let choices = reylo_migrate::Choices { drop: vec!["src.a".into()], rename: vec![("src".into(), "dst".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "dst", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select b from dst").await[0].text("b"), "b");
    }

    /// A column whose not null has a name of its own, renamed with a change to its definition: the
    /// rename names its not null as the target does, once.
    pub async fn a_column_renamed_with_its_declared_not_null_renames_it_once(mut p: P) {
        p.file("20261027100000_t.sql", "create table users (id integer primary key, email text constraint email_required not null);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into users values (1, 'a')").await;
        p.output();
        p.schema("create table users (id integer primary key, address text default '' constraint address_required not null);");
        let choices = reylo_migrate::Choices { rename: vec![("users.email".into(), "address".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "address", &choices, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("doesn't reach"), "{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select conname from pg_constraint where conrelid = 'users'::regclass and contype = 'n'").await.iter().filter(|r| r.text("conname") == "address_required").count(), 1);
        assert_eq!(p.rows("select address from users").await[0].text("address"), "a");
    }

    /// The role a hosted Postgres gives (Neon's, Supabase's): it may create databases and is no
    /// superuser, so no catalog only a superuser reads is in its reach.
    pub async fn a_role_that_is_no_superuser_runs_every_command(mut p: P) {
        p.exec("do $$ begin create role madv_hosted login createdb; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec(&format!("alter database {} owner to madv_hosted", p.name)).await;
        // A shadow another role's run left on the server, which this role may not drop.
        let theirs = format!("reylo_shadow_theirs{}", COUNTER.fetch_add(1, Ordering::Relaxed));
        p.exec(&format!("create database {theirs}")).await;
        let admin = p.url.clone();
        p.url = as_role(&p.url, "madv_hosted");
        p.exec("create table users (id int primary key)").await;
        let opts = p.opts();
        assert_eq!(reylo_migrate::init(&opts, &[], &mut |_: &str| {}).await.unwrap_or_else(|e| panic!("init: {e}")), 0);
        p.strict = true;
        p.schema(&format!("{}\ncreate table posts (id int primary key, user_id int references users (id));\n", std::fs::read_to_string(p.dir.join("schema.sql")).unwrap()));
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "posts", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.status().await, 0, "{}", p.peek());
        p.schema(&format!("{}\nalter table posts add column title text;\n", std::fs::read_to_string(p.dir.join("schema.sql")).unwrap()));
        assert_eq!(p.db_push().await, 0, "{}", p.peek());
        let mut db = Db::connect(&admin, &p.dir).await.unwrap();
        let _ = db.exec(&format!("drop database if exists {theirs}")).await;
        db.close().await;
    }

    /// A platform's role's default privileges, as Neon's API gives every database it makes
    /// (`cloud_admin`'s, granting to `neon_superuser`), which the role that runs the migrations may
    /// not set: init writes schema.sql as `migrate new` writes, and a change by hand is shown alone.
    pub async fn a_platform_roles_default_privileges_are_the_platforms(mut p: P) {
        p.exec("do $$ begin create role madv_hosted login createdb; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec("do $$ begin create role cloud_admin; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec("do $$ begin create role madv_platform_users; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec(&format!("alter database {} owner to madv_hosted", p.name)).await;
        p.exec("alter default privileges for role cloud_admin in schema public grant select, insert, update, delete on tables to madv_platform_users with grant option").await;
        p.exec("alter default privileges for role cloud_admin in schema public grant usage, select on sequences to madv_platform_users with grant option").await;
        p.url = as_role(&p.url, "madv_hosted");
        p.exec("create table users (id bigint generated always as identity primary key, email text not null)").await;
        let opts = p.opts();
        let mut out = Vec::new();
        assert_eq!(reylo_migrate::init(&opts, &[], &mut |l: &str| out.push(l.to_string())).await.unwrap_or_else(|e| panic!("init: {e}")), 0, "{}", out.join("\n"));
        let schema = std::fs::read_to_string(p.dir.join("schema.sql")).unwrap();
        assert!(schema.starts_with("create table users (") && !schema.contains("cloud_admin"), "{schema}");
        assert_eq!(introspect(&opts).await, schema);
        p.strict = true;
        assert_eq!(p.status().await, 0, "{}", p.peek());
        p.output();
        p.exec("alter table users add column hand text").await;
        assert_eq!(p.dry_run().await, 1);
        let out = p.output();
        assert_has!(&out, r"alter table users add column hand text");
        assert!(!out.contains("cloud_admin"), "{out}");
    }

    /// A migration that changes only a privilege, which a database made by another tool never ran,
    /// commutes with every table made after it: the rest are recorded, and it runs last.
    pub async fn a_skipped_grant_runs_after_the_rest(mut p: P) {
        p.linear();
        let (a, usage, c) = ("20261027100000_a.sql", "20261027110000_public_schema_usage.sql", "20261027120000_c.sql");
        p.file(a, "create table a (id int primary key);");
        p.file(usage, "revoke usage on schema public from public;");
        p.file(c, "create table c (id int primary key);");
        p.lock(&[a, usage, c]);
        p.exec("create table a (id int primary key); create table c (id int primary key);").await;
        p.output();
        assert_eq!(p.history_push().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"✓ Wrote \S*reylo\.lock: 20261027110000_public_schema_usage\.sql now runs after 20261027120000_c\.sql, which this database ran without it");
        assert_eq!(p.locked(), [a, c, usage]);
        assert_eq!(p.history().await, [a, c]);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.history().await, [a, c, usage]);
        let acl = p.rows("select coalesce(nspacl::text, '') as acl from pg_namespace where nspname = 'public'").await;
        assert!(!acl[0].text("acl").contains("\"=U"), "{}", acl[0].text("acl"));
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    /// A skipped migration whose change a later one renames is found by the run that stops before
    /// the rename, where its change is all that differs.
    pub async fn a_skipped_migration_found_by_what_a_later_one_changes(mut p: P) {
        p.linear();
        let (a, b, c, d) = ("20261027100000_t.sql", "20261027110000_t_b.sql", "20261027120000_c.sql", "20261027130000_t_bb.sql");
        p.file(a, "create table t (id int primary key);");
        p.file(b, "alter table t add column b int;");
        p.file(c, "create table c (id int primary key);");
        p.file(d, "alter table t rename column b to bb;");
        p.lock(&[a, b, c, d]);
        p.exec("create table t (id int primary key); create table c (id int primary key);").await;
        assert_eq!(p.history_push().await, 0, "{}", p.peek());
        assert_eq!(p.locked(), [a, c, b, d]);
        assert_eq!(p.history().await, [a, c]);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.history().await, [a, c, b, d]);
    }

    /// Two orders that end alike can still differ where a later migration fills rows: the skipped
    /// one moves only where both agree once each has run the migrations it moves past.
    pub async fn a_skipped_migration_moves_only_where_both_orders_agree_before_what_follows(mut p: P) {
        p.linear();
        let (a, b, c, d, e) = ("20261027100000_t.sql", "20261027110000_b.sql", "20261027120000_c.sql", "20261027130000_row.sql", "20261027140000_e.sql");
        p.file(a, "create table t (id int primary key, n int);");
        p.file(b, "create index t_n on t (n); alter table t alter column n set default 1;");
        p.file(c, "alter table t alter column n set default 2;");
        p.file(d, "insert into t (id) values (1);");
        p.file(e, "alter table t alter column n set default 3;");
        p.lock(&[a, b, c, d, e]);
        p.exec("create table t (id int primary key, n int); alter table t alter column n set default 2;").await;
        p.output();
        assert_eq!(p.history_push().await, 1);
        assert_has!(p.output(), r"✗ No run of the migrations makes this database's schema\n[\s\S]*It has what they make without 20261027110000_b\.sql\. Run after 20261027120000_c\.sql, the migrations end differently, so it can't move there\.");
        assert_eq!(p.locked(), [a, b, c, d, e]);
    }

    /// A column added with a default that calls a function fills the rows a table has with what the
    /// function returns then: a skipped change to the function never moves past it, and is made here.
    pub async fn a_skipped_function_moves_past_no_column_it_fills(mut p: P) {
        p.linear();
        let (a, b, c) = ("20261027100000_t.sql", "20261027110000_f.sql", "20261027120000_v.sql");
        p.file(a, "create table t (id int primary key); create function f() returns int language sql immutable as $$ select 1 $$;");
        p.file(b, "create or replace function f() returns int language sql immutable as $$ select 2 $$;");
        p.file(c, "alter table t add column v int default f();");
        p.lock(&[a, b, c]);
        p.exec("create table t (id int primary key); create function f() returns int language sql immutable as $$ select 1 $$; insert into t values (1); alter table t add column v int default f();").await;
        p.output();
        assert_eq!(p.history_push().await, 1);
        assert_has!(p.output(), r"✗ This database has what the migrations make without 20261027110000_f\.sql\n  It ran 20261027120000_v\.sql without it\. Migration 20261027120000_v\.sql changes data, which can run differently without it, so it can't move after it\.\n\n  Make the change 20261027110000_f\.sql makes here, then redo what 20261027120000_v\.sql wrote or built as it would after it, and record them all, running nothing:\n    reylo migrate history push --url <url>");
        assert_eq!(p.locked(), [a, b, c]);
        p.exec("create or replace function f() returns int language sql immutable as $$ select 2 $$; update t set v = f();").await;
        assert_eq!(p.history_push().await, 0, "{}", p.peek());
        assert_eq!(p.history().await, [a, b, c]);
    }

    /// A partition one order leaves and the other doesn't decides where the next insert lands: the
    /// two orders differ there, and the skipped migration doesn't move.
    pub async fn a_skipped_migration_moves_past_no_partition_it_leaves(mut p: P) {
        p.linear();
        let (a, b, c, d, e) = ("20261027100000_m.sql", "20261027110000_m_p.sql", "20261027120000_c.sql", "20261027130000_row.sql", "20261027140000_e.sql");
        p.file(a, "create table m (id int, v int) partition by range (id); create table m_default partition of m default; create table audit (id int);");
        p.file(b, "create table m_p partition of m for values from (0) to (10); create index audit_id on audit (id);");
        p.file(c, "drop table if exists m_p; create table c (id int);");
        p.file(d, "insert into m values (1, 1); create table d (id int);");
        p.file(e, "drop table if exists m_p;");
        p.lock(&[a, b, c, d, e]);
        p.exec("create table m (id int, v int) partition by range (id); create table m_default partition of m default; create table audit (id int); create table c (id int);").await;
        p.output();
        assert_eq!(p.history_push().await, 1);
        assert_has!(p.output(), r"It has what they make without 20261027110000_m_p\.sql\. Run after 20261027120000_c\.sql, the migrations end differently, so it can't move there\.");
        assert_eq!(p.locked(), [a, b, c, d, e]);
    }

    /// An expression index stores the keys its function returns when it is built: a skipped change to
    /// the function never moves past it, or an index scan would miss rows a fresh database finds.
    pub async fn a_skipped_function_moves_past_no_index_it_keys(mut p: P) {
        p.linear();
        let (a, b, c) = ("20261027100000_items.sql", "20261027110000_key.sql", "20261027120000_index.sql");
        let made = "create table items (id int primary key, n int); insert into items values (1, 10); create function idx_key(int) returns int language plpgsql immutable as $$ begin return $1 + 1; end $$;";
        p.file(a, made);
        p.file(b, "create or replace function idx_key(int) returns int language plpgsql immutable as $$ begin return $1 + 2; end $$; alter table items alter column n set not null;");
        p.file(c, "alter table items alter column n set default 0; create index items_expr on items (idx_key(id));");
        p.lock(&[a, b, c]);
        p.exec(&format!("{made} alter table items alter column n set default 0; create index items_expr on items (idx_key(id));")).await;
        p.output();
        assert_eq!(p.history_push().await, 1);
        assert_has!(p.output(), r"✗ This database has what the migrations make without 20261027110000_key\.sql\n  It ran 20261027120000_index\.sql without it\. Migration 20261027120000_index\.sql changes data[^\n]*\n\n  Make the change 20261027110000_key\.sql makes here, then redo what 20261027120000_index\.sql wrote or built as it would after it");
        assert_eq!(p.locked(), [a, b, c]);
        // Following it: the change, the index built again, and all three recorded.
        p.exec("create or replace function idx_key(int) returns int language plpgsql immutable as $$ begin return $1 + 2; end $$; alter table items alter column n set not null; reindex index items_expr;").await;
        assert_eq!(p.history_push().await, 0, "{}", p.peek());
        assert_eq!(p.history().await, [a, b, c]);
    }

    /// A column's default and its nullability share a line of the catalog: a skipped migration that
    /// makes the column not null is still found where a later one gives it a default.
    pub async fn a_skipped_migration_is_found_where_another_changes_the_same_column(mut p: P) {
        p.linear();
        let (a, b, c) = ("20261027100000_t.sql", "20261027110000_b.sql", "20261027120000_c.sql");
        p.file(a, "create table t (id int primary key, n int);");
        p.file(b, "create index t_n on t (n); alter table t alter column n set not null;");
        p.file(c, "alter table t alter column n set default 2; create table c (id int);");
        p.lock(&[a, b, c]);
        p.exec("create table t (id int primary key, n int); alter table t alter column n set default 2; create table c (id int);").await;
        assert_eq!(p.history_push().await, 0, "{}", p.peek());
        assert_eq!(p.locked(), [a, c, b]);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.history().await, [a, c, b]);
    }

    /// A database whose history ran the lock's migrations in another order, after another database
    /// moved one, moves nothing in the lock again: deploy says to make the change it lacks and record
    /// the lock's order.
    pub async fn a_history_out_of_the_locks_order_moves_nothing_more(mut p: P) {
        p.linear();
        let (a, b, c, d) = ("20261027100000_a.sql", "20261027110000_b.sql", "20261027120000_c.sql", "20261027130000_d.sql");
        for (name, table) in [(a, "a"), (b, "b"), (c, "c")] {
            p.file(name, &format!("create table {table} (id int primary key);"));
        }
        p.lock(&[a, b, c]);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        // Production skipped b, and its history push moved b after d.
        p.file(d, "create table d (id int primary key);");
        p.lock(&[a, c, d, b]);
        p.output();
        assert_eq!(p.history_push().await, 1);
        assert_has!(p.output(), r"✗ No run of the migrations makes this database's schema");
        assert_eq!(p.locked(), [a, c, d, b]);
        assert_eq!(p.migrate().await, 1);
        assert_has!(p.output(), r"✗ This database applied 20261027110000_b\.sql where \S*reylo\.lock lists 20261027120000_c\.sql\n  Every database runs the migrations in the lock's order, and this one ran them in another, without 20261027130000_d\.sql\.\n\n  On a development database, apply them in the lock's order:\n    reylo migrate dev\n  On any other, make the change 20261027130000_d\.sql makes there by hand, then record them all, running nothing:\n    reylo migrate history push --url <url>");
        p.exec("create table d (id int primary key)").await;
        assert_eq!(p.history_push().await, 0, "{}", p.peek());
        assert_eq!(p.history().await, [a, c, d, b]);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
    }

    /// Reylo's history table is made with no privilege but its owner's, whatever the database's
    /// default privileges grant on a new table: an app's role may write the app's tables, never which
    /// migrations ran. A grant made on it by hand stays.
    pub async fn the_history_table_takes_no_default_privileges(mut p: P) {
        p.exec("do $$ begin create role madv_readwrite; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec("alter default privileges in schema public grant select, insert, update, delete on tables to madv_readwrite; alter default privileges in schema public grant select on tables to public").await;
        let may = async |p: &P, table: &str, role: &str, privilege: &str| p.rows(&format!("select has_table_privilege('{role}', '{table}', '{privilege}') as ok")).await[0].flag("ok");
        // A database made before Reylo's history, recorded as it stands.
        p.file("20261027100000_a.sql", "create table a (id int primary key);");
        p.exec("create table a (id int primary key)").await;
        assert_eq!(p.history_push().await, 0, "{}", p.peek());
        assert!(may(&p, "a", "madv_readwrite", "insert").await, "the app's own table keeps its grants");
        for privilege in ["insert", "update", "delete", "select"] {
            assert!(!may(&p, "__reylo_migrations", "madv_readwrite", privilege).await, "madv_readwrite may {privilege} the history");
        }
        let others = p.rows("select count(*)::int as n from pg_class c cross join aclexplode(c.relacl) a where c.oid = '__reylo_migrations'::regclass and a.grantee <> c.relowner").await;
        assert_eq!(others[0].text("n"), "0", "the history grants to a role but its owner");
        // A grant made by hand on the history afterwards is kept by the runs after it.
        p.exec("grant select on __reylo_migrations to madv_readwrite").await;
        p.file("20261027110000_b.sql", "create table b (id int primary key);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert!(may(&p, "__reylo_migrations", "madv_readwrite", "select").await);
        assert!(!may(&p, "__reylo_migrations", "madv_readwrite", "insert").await);
        assert!(may(&p, "b", "madv_readwrite", "insert").await);
    }

    /// A role that may not create a database (PlanetScale's, some managed ones') can make no shadow:
    /// the refusal names shadow_url, and `init` leaves no project behind. introspect prints the
    /// database's own dump instead, and says why on stderr; with a shadow_url that makes no shadow
    /// either, it is refused.
    pub async fn a_role_that_cannot_create_databases_is_told_of_shadow_url(mut p: P) {
        let role = "madv_nocreate";
        match p.d {
            Dialect::Postgres => {
                p.exec(&format!("do $$ begin create role {role} login nocreatedb; exception when duplicate_object or unique_violation then null; end $$")).await;
                p.exec(&format!("alter database {} owner to {role}", p.name)).await;
            }
            _ => p.exec(&format!("create user if not exists '{role}'@'%'; grant all on `{}`.* to '{role}'@'%'", p.name)).await,
        }
        p.url = as_role(&p.url, role);
        p.exec("create table users (id int primary key)").await;
        let opts = p.opts();
        let e = reylo_migrate::init(&opts, &[], &mut |_: &str| {}).await.expect_err("no shadow database can be made");
        assert_has!(e.0, "shadow_url");
        assert!(!p.dir.join("schema.sql").exists(), "init left schema.sql");
        assert!(!p.dir.join(reylo_migrate::lock::LOCK).exists(), "init left the lock");
        assert!(p.files().is_empty(), "init left {:?}", p.files());
        let mut said = Vec::new();
        let text = match reylo_migrate::db_introspect(&opts, &[], &mut |l: &str| said.push(l.to_string())).await {
            Ok(Ok(text)) => text,
            other => panic!("introspect: {:?}\n{}", other.map_err(|e| e.0), said.join("\n")),
        };
        assert_has!(text, "(?i)create table");
        assert_has!(said.join("\n"), r"^» Printing (pg_dump's output|the database's own dump), not Reylo's own SQL\n  No shadow database can be made to check Reylo's own SQL for it: [\s\S]+shadow_url[\s\S]*$");
        // A shadow_url that answers nothing, and one whose role can't create a database either.
        let nowhere = if p.d == Dialect::Postgres { "postgres://postgres@127.0.0.1:1/none" } else { "mysql://root@127.0.0.1:1/none" };
        for shadow_url in [nowhere.to_string(), p.url.clone()] {
            let mut said = Vec::new();
            let result = reylo_migrate::db_introspect(&Options { shadow_url: Some(shadow_url.clone()), ..p.opts() }, &[], &mut |l: &str| said.push(l.to_string())).await;
            assert!(matches!(result, Ok(Err(1))), "{shadow_url}: {}", said.join("\n"));
            assert_has!(said.join("\n"), r"^✗ No shadow database can be made on the server shadow_url names\n  [\s\S]+\n  Reylo's own SQL is checked on a shadow database before it's printed\.\n  Make sure that server is running and its role can create a database, or set shadow_url in \.reylo\.yaml to one that can\.\n\nNothing was changed\.$");
        }
        p.schema("create table users (id int primary key);");
        p.file("20261027100000_users.sql", "create table users (id int primary key);");
        let e = reylo_migrate::dry_run(&opts, &mut |_: &str| {}).await.expect_err("no shadow database can be made");
        assert_has!(e.0, "shadow_url");
    }

    /// Where no shadow can be made, the schema is the dump, which makes each extension's schema before
    /// it: once, however many extensions share it (Supabase keeps three in `extensions`).
    pub async fn a_dump_makes_a_schema_its_extensions_share_once(mut p: P) {
        with_no_shadows(&mut p).await;
        p.exec("create schema extensions; create extension citext with schema extensions; create extension pgcrypto with schema extensions; create table users (id int primary key)").await;
        let text = introspect(&p.opts()).await;
        assert_eq!(text.matches("CREATE SCHEMA IF NOT EXISTS extensions").count(), 1, "{text}");
        assert!(text.contains("CREATE EXTENSION IF NOT EXISTS citext WITH SCHEMA extensions") && text.contains("CREATE EXTENSION IF NOT EXISTS pgcrypto WITH SCHEMA extensions"), "{text}");
    }

    /// With no pg_dump anywhere, init, introspect and a `migrate new` with no schema.sql write Reylo's
    /// own SQL, checked against the database on a shadow: only the dump they fall back on needs one.
    pub async fn init_introspect_and_new_need_no_pg_dump(mut p: P) {
        reylo_migrate::db::set_pg_dump_dirs(Some(vec![]));
        p.exec("create table users (id int primary key, email text not null); create view emails as select email from users").await;
        let text = introspect(&p.opts()).await;
        assert_has!(text.clone(), r"create table users \(");
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        assert_eq!(std::fs::read_to_string(p.dir.join("schema.sql")).unwrap(), text);
        assert_eq!(p.history().await, ["init.sql"]);
        // With no schema.sql, the migration is what the database has beyond the migrations.
        std::fs::remove_file(p.dir.join("schema.sql")).unwrap();
        p.exec("alter table users add column bio text").await;
        p.output();
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "bio", &Default::default(), log).await).await, 0, "{}", p.peek());
        let written = p.files().into_iter().find(|f| f.ends_with("_bio.sql")).unwrap_or_else(|| panic!("no migration written: {}", p.peek()));
        assert_has!(std::fs::read_to_string(p.dir.join("migrations").join(written)).unwrap(), r"(?i)add column bio text");
        reylo_migrate::db::set_pg_dump_dirs(None);
    }

    /// Where Reylo's own SQL can't be checked (no shadow database can be made here) and no pg_dump can
    /// dump the database instead, introspect says why, what it found and how to get one.
    pub async fn introspect_with_no_check_and_no_pg_dump_says_how_to_get_one(mut p: P) {
        with_no_shadows(&mut p).await;
        p.exec("create table users (id int primary key)").await;
        let opts = p.opts();
        let mut dirs = vec![];
        // A pg_dump too old to read the server, which is named.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let bin = p.dir.join("old-bin");
            std::fs::create_dir_all(&bin).unwrap();
            std::fs::write(bin.join("pg_dump"), "#!/bin/sh\necho 'pg_dump (PostgreSQL) 9.6.24'\n").unwrap();
            std::fs::set_permissions(bin.join("pg_dump"), std::fs::Permissions::from_mode(0o755)).unwrap();
            dirs.push(bin);
        }
        reylo_migrate::db::set_pg_dump_dirs(Some(dirs));
        let mut said = Vec::new();
        let result = reylo_migrate::db_introspect(&opts, &[], &mut |l: &str| said.push(l.to_string())).await;
        reylo_migrate::db::set_pg_dump_dirs(None);
        let said = said.join("\n");
        assert!(matches!(result, Ok(Err(1))), "{said}");
        assert_has!(said.clone(), r"^✗ Writing this schema as SQL needs pg_dump (\d+) or newer\n  No shadow database can be made to check Reylo's own SQL for it: ");
        #[cfg(unix)]
        assert_has!(said.clone(), r"\n  The pg_dump at \S+/old-bin/pg_dump is version 9, which can't read Postgres \d+\n");
        assert_has!(said.clone(), r"\n  Reylo looks for it in PGBIN, then where Postgres installs it, then on PATH\.\n");
        assert_has!(said, r"\n  Install pg_dump \d+, then run it again:\n    \S[^\n]*\n\nNothing was changed\.$");
    }

    /// A view in `public` on a table in another schema, which has a key to `public`: init reads every
    /// schema, so its init.sql makes both and a new database replays it. --schema naming one of them
    /// is refused before anything is written, with the command that names both, and introspect
    /// --schema reads what the schema it names refers to.
    pub async fn init_reads_every_schema_a_view_crosses(mut p: P) {
        p.exec("create schema billing; create table customers (id serial primary key, name text not null); create table billing.invoices (id serial primary key, customer_id int references customers (id), total numeric(10,2)); create view unpaid as select c.name, i.total from customers c join billing.invoices i on i.customer_id = c.id").await;
        for (named, other) in [("public", "billing"), ("billing", "public")] {
            assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[named.to_string()], log).await).await, 1, "{}", p.peek());
            assert_has!(p.output(), &format!(r"^✗ This database's schema is in {other} too, which --schema leaves out\n[\s\S]*\n  Name every schema it's in:\n    reylo migrate init --schema {named} --schema {other}\n\nNothing was changed\.$"));
            assert!(!p.dir.join("schema.sql").exists() && p.files().is_empty(), "init wrote files");
        }
        // A refusal leaves what was there, a link that leads nowhere included.
        #[cfg(unix)]
        {
            let link = p.dir.join("linked");
            std::os::unix::fs::symlink(p.dir.join("nowhere"), &link).unwrap();
            let opts = Options { dir: link.join("migrations"), schema: link.join("schema.sql"), lock: link.join(reylo_migrate::lock::LOCK), cache: link.join(".cache"), ..p.opts() };
            assert_eq!(reylo_migrate::init(&opts, &["public".to_string()], &mut |_: &str| {}).await.unwrap(), 1);
            assert!(std::fs::symlink_metadata(&link).is_ok(), "init took the link away");
        }
        let mut said = Vec::new();
        let text = match reylo_migrate::db_introspect(&p.opts(), &["public".to_string()], &mut |l: &str| said.push(l.to_string())).await {
            Ok(Ok(text)) => text,
            other => panic!("introspect: {:?}\n{}", other.map_err(|e| e.0), said.join("\n")),
        };
        assert_eq!(said.join("\n"), "» Reading billing too, which public refers to");
        assert_has!(text.clone(), r"create table billing\.invoices \(");
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        assert_eq!(std::fs::read_to_string(p.dir.join("schema.sql")).unwrap(), text);
        let q = P::new(p.d).await.unwrap();
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(q.history().await, ["init.sql"]);
        assert_eq!(q.rows("select 1 from information_schema.views where table_name = 'unpaid'").await.len(), 1);
    }

    /// The dump reads a schema whose name has capitals as it is named: pg_dump folds what --schema
    /// gives it unless it is quoted.
    pub async fn the_dump_reads_a_schema_named_with_capitals(mut p: P) {
        with_no_shadows(&mut p).await;
        p.exec(r#"create schema "CaseSensitive"; create table "CaseSensitive".items (id int); create table users (id int primary key)"#).await;
        let text = introspect(&p.opts()).await;
        assert!(text.contains(r#"CREATE TABLE "CaseSensitive".items"#) && text.contains("CREATE TABLE public.users"), "{text}");
    }

    /// With no schema.sql, the migration is what the database has beyond its migrations, its own SQL
    /// replayed on shadows that have each extension no migration creates, in its schema: one the
    /// migrations use as the environment gives it, and one made by hand in a schema of its own.
    pub async fn new_without_schema_sql_replays_beside_the_environments_extensions(mut p: P) {
        p.exec("create extension citext").await;
        p.file("20261122100000_users.sql", "create table users (id int primary key, name citext);");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("create schema extensions; create extension pg_trgm with schema extensions; alter table users add column email text").await;
        p.output();
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "email", &Default::default(), log).await).await, 0, "{}", p.peek());
        let written = p.files().into_iter().find(|f| f.ends_with("_email.sql")).unwrap_or_else(|| panic!("no migration written: {}", p.peek()));
        assert_has!(std::fs::read_to_string(p.dir.join("migrations").join(written)).unwrap(), r"(?i)add column email text");
    }

    /// What `migrate new` lists under the file is what the migration changes: an extension, and none
    /// of what the extension makes itself, such as tablefunc's composite types.
    pub async fn new_lists_none_of_an_extensions_own_objects(mut p: P) {
        p.file("20261122100000_users.sql", "create table users (id int primary key);");
        p.schema("create table users (id int primary key);");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.schema("create extension tablefunc; create table users (id int primary key);");
        p.output();
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tablefunc", &Default::default(), log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert_has!(&out, r"✓ Wrote \S*_tablefunc\.sql\n  \+ extension tablefunc ");
        assert!(!out.contains("crosstab"), "{out}");
    }

    /// With no schema.sql, an extension added by hand since the migrations began, and a column of its
    /// type: `migrate new` makes the extension too, after a `migrate dev` that runs nothing, and a new
    /// database runs the migrations.
    pub async fn new_without_schema_sql_makes_an_extension_added_by_hand(mut p: P) {
        p.file("20261122100000_users.sql", "create table users (id int primary key);");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("create schema extensions; create extension citext with schema extensions; alter table users add column email extensions.citext").await;
        p.dev().await;
        p.output();
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "email", &Default::default(), log).await).await, 0, "{}", p.peek());
        let written = p.files().into_iter().find(|f| f.ends_with("_email.sql")).unwrap_or_else(|| panic!("no migration written: {}", p.peek()));
        let text = std::fs::read_to_string(p.dir.join("migrations").join(written)).unwrap();
        assert_has!(&text, r"(?i)create extension (if not exists )?citext");
        assert_has!(&text, r"(?i)add column email extensions\.citext");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        let q = P::new(p.d).await.unwrap();
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(q.rows("select 1 from pg_extension where extname = 'citext'").await.len(), 1);
    }

    /// With no schema.sql, an extension added by hand that a migration then uses without making it,
    /// and another added by hand after, with a column of its type: the history replays only with the
    /// first, so `migrate new` keeps it in its shadows and makes the second, and a new database that
    /// has the first runs the migrations.
    pub async fn new_without_schema_sql_keeps_only_the_extensions_the_history_needs(mut p: P) {
        p.file("20261122100000_users.sql", "create table users (id int primary key);");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("create extension citext").await;
        p.file("20261122100001_name.sql", "alter table users add column name citext;");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("create extension hstore; alter table users add column tags hstore").await;
        p.output();
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tags", &Default::default(), log).await).await, 0, "{}", p.peek());
        let written = p.files().into_iter().find(|f| f.ends_with("_tags.sql")).unwrap_or_else(|| panic!("no migration written: {}", p.peek()));
        let text = std::fs::read_to_string(p.dir.join("migrations").join(written)).unwrap();
        assert_has!(&text, r"(?i)create extension (if not exists )?hstore");
        assert_has!(&text, r"(?i)add column tags (public\.)?hstore");
        assert!(!text.to_lowercase().contains("citext"), "{text}");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        let q = P::new(p.d).await.unwrap();
        q.exec("create extension citext").await;
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(q.rows("select 1 from pg_extension where extname = 'hstore'").await.len(), 1);
    }

    /// With no schema.sql, an extension the database had before its history began that no migration
    /// uses, as after a dump restored it before the history table: `migrate new` makes it too, so a
    /// new database runs the migrations.
    pub async fn new_without_schema_sql_makes_an_extension_no_migration_uses(mut p: P) {
        p.exec("create extension hstore").await;
        p.file("20261122100000_users.sql", "create table users (id int primary key);");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("alter table users add column tags hstore").await;
        p.output();
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tags", &Default::default(), log).await).await, 0, "{}", p.peek());
        let written = p.files().into_iter().find(|f| f.ends_with("_tags.sql")).unwrap_or_else(|| panic!("no migration written: {}", p.peek()));
        assert_has!(std::fs::read_to_string(p.dir.join("migrations").join(written)).unwrap(), r"(?i)create extension (if not exists )?hstore");
        let q = P::new(p.d).await.unwrap();
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
    }

    /// With no schema.sql, a migration that adds a column only where an extension is: the history
    /// replays without the extension but makes less, so `migrate new` keeps it in its shadows and
    /// writes only the new change, which a new database that has the extension runs.
    pub async fn new_without_schema_sql_keeps_an_extension_a_migration_branches_on(mut p: P) {
        p.exec("create extension citext").await;
        p.file("20261122100000_users.sql", "create table users (id int primary key);");
        p.file("20261122100001_email.sql", "do $$ begin if exists (select 1 from pg_extension where extname = 'citext') then alter table users add column email citext; end if; end $$;");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("alter table users add column note text").await;
        p.output();
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "note", &Default::default(), log).await).await, 0, "{}", p.peek());
        let written = p.files().into_iter().find(|f| f.ends_with("_note.sql")).unwrap_or_else(|| panic!("no migration written: {}", p.peek()));
        let text = std::fs::read_to_string(p.dir.join("migrations").join(written)).unwrap();
        assert_has!(&text, r"(?i)add column note text");
        assert!(!text.to_lowercase().contains("email") && !text.to_lowercase().contains("citext"), "{text}");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        let q = P::new(p.d).await.unwrap();
        q.exec("create extension citext").await;
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
    }

    /// With no schema.sql, a history that needs one of two extensions, and a third added by hand: the
    /// third is still the new migration's, and a new database that has the first two runs it.
    pub async fn new_without_schema_sql_makes_an_extension_beside_two_the_history_needs_either_of(mut p: P) {
        p.exec("create extension citext; create extension hstore").await;
        p.file("20261122100000_users.sql", "create table users (id int primary key);");
        p.file(
            "20261122100001_name.sql",
            "do $$ begin if exists (select 1 from pg_extension where extname = 'citext') then alter table users add column name citext; elsif exists (select 1 from pg_extension where extname = 'hstore') then alter table users add column name hstore; else raise exception 'neither citext nor hstore'; end if; end $$;",
        );
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("create extension cube; alter table users add column corner cube").await;
        p.output();
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "corner", &Default::default(), log).await).await, 0, "{}", p.peek());
        let written = p.files().into_iter().find(|f| f.ends_with("_corner.sql")).unwrap_or_else(|| panic!("no migration written: {}", p.peek()));
        let text = std::fs::read_to_string(p.dir.join("migrations").join(written)).unwrap();
        assert_has!(&text, r"(?i)create extension (if not exists )?cube");
        assert!(!text.to_lowercase().contains("add column name"), "{text}");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        let q = P::new(p.d).await.unwrap();
        q.exec("create extension citext; create extension hstore").await;
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(q.rows("select 1 from pg_extension where extname = 'cube'").await.len(), 1);
    }

    /// With no schema.sql, an extension added by hand in the schema a migration renamed another to:
    /// a shadow made with it can't replay the rename, and one made without it can't say which of the
    /// history's extensions the database had when it ran, so `migrate new` refuses and writes nothing.
    pub async fn new_without_schema_sql_refuses_an_extension_in_a_schema_a_migration_renamed(mut p: P) {
        p.file("20261122100000_staging.sql", "create schema staging; create table staging.t (id int primary key);");
        p.file("20261122100001_rename.sql", "alter schema staging rename to extensions;");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("create extension citext with schema extensions; alter table extensions.t add column c extensions.citext").await;
        p.output();
        assert_ne!(p.run(async |o, log| reylo_migrate::create(o, "c", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_has!(p.output(), r#"Replaying 20261122100001_rename\.sql in a shadow database fails\n  schema "extensions" already exists"#);
        assert!(!p.files().iter().any(|f| f.ends_with("_c.sql")), "{:?}", p.files());
    }

    /// With no schema.sql, an extension added by hand in a schema a migration makes: the history
    /// makes the schema with or without it, so it is the new migration's, which a new database runs.
    pub async fn new_without_schema_sql_makes_an_extension_in_a_schema_a_migration_makes(mut p: P) {
        p.file("20261122100000_users.sql", "create schema if not exists \"app data\"; create table \"app data\".users (id int primary key);");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("create extension citext with schema \"app data\"; alter table \"app data\".users add column email \"app data\".citext").await;
        p.output();
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "email", &Default::default(), log).await).await, 0, "{}", p.peek());
        let written = p.files().into_iter().find(|f| f.ends_with("_email.sql")).unwrap_or_else(|| panic!("no migration written: {}", p.peek()));
        assert_has!(std::fs::read_to_string(p.dir.join("migrations").join(written)).unwrap(), r"(?i)create extension (if not exists )?citext");
        let q = P::new(p.d).await.unwrap();
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
    }

    /// An extension schema.sql makes in a schema of its own, pushed to the development database, then
    /// written as a migration: dev records the migration as in place, counting the extensions the
    /// database has, and a new database runs it.
    pub async fn a_pushed_extension_is_recorded_once_its_migration_makes_it(mut p: P) {
        p.schema("create table users (id int primary key);");
        p.file("20261122100000_users.sql", "create table users (id int primary key);");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.schema("create schema extensions; create extension pg_trgm with schema extensions; create table users (id int primary key, email text);");
        assert_eq!(p.db_push().await, 0, "{}", p.peek());
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "trgm", &Default::default(), log).await).await, 0, "{}", p.peek());
        let written = p.files().into_iter().find(|f| f.ends_with("_trgm.sql")).unwrap_or_else(|| panic!("no migration written: {}", p.peek()));
        assert_has!(std::fs::read_to_string(p.dir.join("migrations").join(&written)).unwrap(), r"(?i)create extension (if not exists )?pg_trgm");
        p.output();
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"_trgm\.sql +recorded: already in place");
        let q = P::new(p.d).await.unwrap();
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(q.rows("select 1 from pg_extension where extname = 'pg_trgm'").await.len(), 1);
    }

    /// Introspect of some of the schemas that hold anything is their dump, which Reylo's own SQL,
    /// making every schema, can't write; with no pg_dump, it says so.
    pub async fn introspect_of_some_schemas_is_their_dump(p: P) {
        p.exec("create schema scratch; create table scratch.notes (id int); create schema empty; create table users (id int primary key)").await;
        let public = ["public".to_string()];
        let mut said = Vec::new();
        reylo_migrate::db::set_pg_dump_dirs(Some(vec![]));
        let result = reylo_migrate::db_introspect(&p.opts(), &public, &mut |l: &str| said.push(l.to_string())).await;
        reylo_migrate::db::set_pg_dump_dirs(None);
        assert!(matches!(result, Ok(Err(1))), "{}", said.join("\n"));
        assert_has!(said.join("\n"), r"^✗ Writing this schema as SQL needs pg_dump \d+ or newer\n  Reylo's own SQL makes every schema, and --schema leaves out empty and scratch\n");
        let mut said = Vec::new();
        let text = match reylo_migrate::db_introspect(&p.opts(), &public, &mut |l: &str| said.push(l.to_string())).await {
            Ok(Ok(text)) => text,
            other => panic!("introspect: {:?}\n{}", other.map_err(|e| e.0), said.join("\n")),
        };
        assert!(text.contains("CREATE TABLE public.users") && !text.contains("scratch") && !text.contains("empty"), "{text}");
    }

    /// A database whose `public` its own role owns outright, not through `pg_database_owner`: one made
    /// before Postgres 15 and upgraded, or one a platform hands out (Tiger Cloud's). Its shadows' `public`
    /// is `pg_database_owner`'s, which is the same owner, so no command reports it.
    pub async fn a_public_schema_its_role_owns_is_a_new_databases(mut p: P) {
        p.exec("do $$ begin create role madv_owner login createdb; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec(&format!("alter database {} owner to madv_owner", p.name)).await;
        p.exec("alter schema public owner to madv_owner").await;
        p.url = as_role(&p.url, "madv_owner");
        p.exec("create table users (id int primary key)").await;
        let opts = p.opts();
        let mut out = Vec::new();
        let code = reylo_migrate::init(&opts, &[], &mut |l: &str| out.push(l.to_string())).await.unwrap_or_else(|e| panic!("init: {e}"));
        assert_eq!(code, 0, "{}", out.join("\n"));
        p.strict = true;
        assert_eq!(p.status().await, 0, "{}", p.peek());
        p.schema(&format!("{}\ncreate table posts (id int primary key);\n", std::fs::read_to_string(p.dir.join("schema.sql")).unwrap()));
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "posts", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.status().await, 0, "{}", p.peek());
    }

    /// The server's hint, which often says what to do (a cast here, the setting a platform limits
    /// elsewhere), is part of the error a command prints.
    pub async fn a_servers_hint_is_in_its_error(p: P) {
        p.file("20261027100000_cast.sql", "select lower(1);");
        let opts = p.opts();
        let mut out = Vec::new();
        let result = reylo_migrate::deploy(&opts, &mut |l: &str| out.push(l.to_string())).await;
        let said = format!("{}\n{}", out.join("\n"), result.as_ref().err().map(|e| e.0.clone()).unwrap_or_default());
        assert!(!matches!(result, Ok(0)), "{said}");
        assert_has!(said, r"function lower\(integer\) does not exist\. No function matches the given name and argument types");
    }

    /// An init whose init.sql doesn't replay (here a function another role owns, which the shadow's
    /// role may not give it) is refused, and leaves no project behind: "Nothing was changed" holds,
    /// and init runs again once the shadow can replay it.
    pub async fn init_refused_leaves_no_project_behind(mut p: P) {
        p.exec("do $$ begin create role madv_app login createdb; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec("do $$ begin create role madv_other; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec(&format!("alter database {} owner to madv_app", p.name)).await;
        // Another role's that is no superuser: the project's, which the dump makes and gives it.
        p.exec("create function public.theirs() returns int language sql as 'select 1'; alter function public.theirs() owner to madv_other").await;
        p.url = as_role(&p.url, "madv_app");
        p.exec("create table users (id int primary key)").await;
        // In a directory init makes, as a new project's `reylo/` is.
        let home = p.dir.join("reylo");
        let opts = Options { dir: home.join("migrations"), schema: home.join("schema.sql"), lock: home.join(reylo_migrate::lock::LOCK), cache: home.join(".cache"), ..p.opts() };
        let mut out = Vec::new();
        let code = reylo_migrate::init(&opts, &[], &mut |l: &str| out.push(l.to_string())).await.unwrap_or_else(|e| panic!("init: {e}"));
        let said = out.join("\n");
        assert_eq!(code, 1, "{said}");
        assert_has!(said.clone(), r"Replaying init\.sql in a shadow database fails");
        assert_has!(said.clone(), r"Nothing was changed\.$");
        assert!(!said.contains("Started"), "{said}");
        assert!(!home.exists(), "init left {}:\n{said}", home.display());
        assert!(p.history().await.is_empty(), "init recorded {:?}", p.history().await);
    }

    /// As Tiger Cloud makes a service: its superuser owns a schema of security definer functions, a
    /// function in `public` and default privileges, and the role it hands out can't become it. That is
    /// the platform's: init writes the project's table alone, the shadows leave it out, saying so once,
    /// and every command runs, on this database and on a second one made the same way.
    pub async fn what_a_superuser_out_of_reach_owns_is_the_platforms(mut p: P) {
        let platform = "create schema timescale_functions;
            create function timescale_functions.helper() returns int language sql security definer as 'select 1';
            comment on function timescale_functions.helper() is 'the platform''s';
            grant usage on schema timescale_functions to madv_tiger;
            create function public.create_playground(regclass, boolean) returns int language sql as 'select 1';
            alter default privileges grant usage on schemas to madv_tiger;";
        p.exec("do $$ begin create role madv_tiger login createdb; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec(&format!("alter database {} owner to madv_tiger", p.name)).await;
        p.exec(platform).await;
        let admin = p.url.clone();
        p.url = as_role(&admin, "madv_tiger");
        p.exec("create table users (id int primary key)").await;
        let opts = p.opts();
        let mut out = Vec::new();
        let code = reylo_migrate::init(&opts, &[], &mut |l: &str| out.push(l.to_string())).await.unwrap_or_else(|e| panic!("init: {e}"));
        assert_eq!(code, 0, "{}", out.join("\n"));
        let schema = std::fs::read_to_string(p.dir.join("schema.sql")).unwrap();
        assert!(schema.trim_start().starts_with("create table users (") && schema.matches(';').count() == 1, "the project's table alone:\n{schema}");
        assert_eq!(introspect(&opts).await, schema);
        p.strict = true;
        assert_eq!(p.status().await, 0, "{}", p.peek());
        // A second database, made as the platform makes one, taken to the migrations.
        let second = format!("{}_second", p.name);
        let mut db = Db::connect(&admin, &p.dir).await.unwrap();
        db.exec(&format!("drop database if exists {second} with (force)")).await.unwrap();
        db.exec(&format!("create database {second} owner madv_tiger")).await.unwrap();
        db.close().await;
        let mut db = Db::connect(&with_database(&admin, &second), &p.dir).await.unwrap();
        db.script(platform).await.unwrap();
        db.close().await;
        p.url = as_role(&with_database(&admin, &second), "madv_tiger");
        p.output();
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        let said = p.output();
        assert_eq!(regex::Regex::new(r"Shadow databases leave out what \S+ owns here, the platform's").unwrap().find_iter(&said).count(), 1, "{said}");
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.status().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select count(*) as n from pg_proc where proname in ('helper', 'create_playground')").await[0].text("n"), "2", "the platform's own stay");
        forget(&p.url).await;
        let mut db = Db::connect(&admin, &p.dir).await.unwrap();
        let _ = db.exec(&format!("drop database if exists {second} with (force)")).await;
        db.close().await;
    }

    /// A run on a database with the platform's objects, by a role that may create roles, makes no role
    /// and grants none: a role is the whole server's and outlives the run. The shadows leave the
    /// platform's objects out and say how a role made by hand lets them in.
    pub async fn a_run_never_makes_a_role(mut p: P) {
        p.exec("do $$ begin create role madv_rolemaker login createdb createrole; exception when duplicate_object or unique_violation then null; end $$").await;
        // The role the shadows are made as below, made here by the server's superuser: one made by
        // madv_rolemaker, which may create roles, would make it that role's member, a change of this
        // role's memberships the run never made.
        p.exec("do $$ begin create role madv_hosted login createdb; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec(&format!("alter database {} owner to madv_rolemaker", p.name)).await;
        p.exec("create schema timescale_functions; create function timescale_functions.helper() returns int language sql security definer as 'select 1'; create function public.create_playground() returns int language sql as 'select 1';").await;
        p.url = as_role(&p.url, "madv_rolemaker");
        p.exec("create table users (id int primary key)").await;
        // The roles and memberships no other scenario makes beside this one, and every membership of this role.
        let roles = async |p: &P| {
            let rows = p
                .rows(
                    r"select coalesce(string_agg(rolname, ',' order by rolname), '') as r from pg_roles where rolname not like 'madv\_%' and rolname not in ('cloud_admin', 'supabase_auth_admin', 'reylo_platform')
                    union all select coalesce(string_agg(format('%s>%s:%s', roleid::regrole, member::regrole, admin_option), ',' order by roleid::regrole::text, member::regrole::text), '') from pg_auth_members where member = 'madv_rolemaker'::regrole or roleid = 'madv_rolemaker'::regrole",
                )
                .await;
            rows.iter().map(|r| r.text("r").to_string()).collect::<Vec<_>>()
        };
        let before = roles(&p).await;
        let opts = p.opts();
        let mut out = Vec::new();
        let code = reylo_migrate::init(&opts, &[], &mut |l: &str| out.push(l.to_string())).await.unwrap_or_else(|e| panic!("init: {e}"));
        assert_eq!(code, 0, "{}", out.join("\n"));
        p.strict = true;
        p.output();
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"Shadow databases leave out what \S+ owns here, the platform's, so a migration that uses it fails in them: a role reylo_platform on their server that madv_rolemaker, the role they are made as, may SET ROLE to lets them make it");
        // Shadows made on shadow_url's server, as its role: that role is the one named.
        let shadowed = Options { shadow_url: Some(as_role(&p.url, "madv_hosted")), ..p.opts() };
        let mut out = Vec::new();
        let _ = reylo_migrate::dry_run(&shadowed, &mut |l: &str| out.push(l.to_string())).await.unwrap_or_else(|e| panic!("dry run: {e}"));
        assert_has!(out.join("\n"), r"a role reylo_platform on their server that madv_hosted, the role they are made as, may SET ROLE to lets them make it");
        assert_eq!(roles(&p).await, before, "a run made or granted a role");
    }

    /// The application's tables, made by a superuser on a database that a role that is no superuser
    /// owns and connects as (a local server's `postgres` making them before handing the database
    /// over): they are the project's, not a platform's, though that role can't act as their owner.
    /// `db introspect` prints them, and init, and every command that replays or plans, refuses them,
    /// naming the owner and giving the command that makes the role their owner; once that has run,
    /// init starts from them.
    pub async fn tables_a_superuser_out_of_reach_made_are_the_projects(mut p: P) {
        p.exec("do $$ begin create role madv_app login createdb; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec(&format!("alter database {} owner to madv_app", p.name)).await;
        p.exec(
            "create table customers (id int primary key, name text); insert into customers values (1, 'one'); grant select on customers to madv_app;
            create table orders (id int primary key, customer int references customers); grant select on orders to madv_app;",
        )
        .await;
        let superuser = p.rows("select current_user::text as u").await[0].text("u").to_string();
        // A table of the same name in the schema the owner's `search_path` ("$user", public) finds
        // first: the platform's, which the fix must leave alone.
        let own = format!("\"{}\"", superuser.replace('"', "\"\""));
        p.exec(&format!("create schema {own}; create table {own}.customers (id int)")).await;
        let admin = p.url.clone();
        p.url = as_role(&admin, "madv_app");
        let opts = p.opts();
        let text = introspect(&opts).await;
        assert_has!(&text, r"(?i)create table (public\.)?customers \(");
        assert_has!(&text, r"(?i)create table (public\.)?orders \(");
        let mut out = Vec::new();
        let code = reylo_migrate::init(&opts, &[], &mut |l: &str| out.push(l.to_string())).await.unwrap_or_else(|e| panic!("init: {e}"));
        let said = out.join("\n");
        assert_eq!(code, 1, "{said}");
        let title = format!(r"(?m)^✗ {} owns tables here that madv_app can't act as: customers, orders$", regex::escape(&superuser));
        assert_has!(said.clone(), &title);
        assert_has!(said.clone(), &format!(r"(?m)^  Make madv_app their owner, connected as {}, then start again:$", regex::escape(&superuser)));
        assert_has!(said.clone(), r"Nothing was changed\.$");
        assert!(!p.dir.join("schema.sql").exists(), "init left schema.sql:\n{said}");
        assert!(!p.dir.join(reylo_migrate::lock::LOCK).exists(), "init left the lock");
        assert!(p.files().is_empty(), "init left {:?}", p.files());
        // Every command that replays or plans refuses the same.
        p.schema("create table customers (id int primary key, name text);\n\ncreate table orders (id int primary key, customer int references customers);\n");
        assert_eq!(p.dev().await, 1, "{}", p.peek());
        assert_has!(p.output(), &title);
        assert_eq!(p.dry_run().await, 1, "{}", p.peek());
        assert_has!(p.output(), &title);
        assert_eq!(p.db_push().await, 1, "{}", p.peek());
        assert_has!(p.output(), &title);
        std::fs::remove_file(p.dir.join("schema.sql")).unwrap();
        // The fix, as given: one `reylo run` as the owner.
        let fix = said.lines().find(|l| l.contains("reylo run")).unwrap_or_else(|| panic!("no fix in:\n{said}")).trim().to_string();
        let words: Vec<String> = regex::Regex::new(r"'((?:[^']|'\\'')*)'").unwrap().captures_iter(&fix).map(|c| c[1].replace("'\\''", "'")).collect();
        assert!(fix.starts_with("reylo run --url ") && words.len() == 3, "{fix}");
        assert_eq!(&words[1..], ["alter table public.customers owner to madv_app", "alter table public.orders owner to madv_app"], "{fix}");
        let items: Vec<(String, String)> = words[1..].iter().map(|w| (w.clone(), w.clone())).collect();
        let url = words[0].clone();
        assert_eq!(p.run(async |o, log| reylo_migrate::run_sql(&reylo_migrate::Options { url: url.clone(), ..o.clone() }, &items, log).await).await, 0, "{}", p.peek());
        p.output();
        let mut out = Vec::new();
        let code = reylo_migrate::init(&opts, &[], &mut |l: &str| out.push(l.to_string())).await.unwrap_or_else(|e| panic!("init: {e}"));
        assert_eq!(code, 0, "{}", out.join("\n"));
        let schema = std::fs::read_to_string(p.dir.join("schema.sql")).unwrap();
        assert!(schema.contains("create table customers (") && schema.contains("create table orders ("), "{schema}");
        assert_eq!(introspect(&opts).await, schema);
        p.strict = true;
        assert_eq!(p.status().await, 0, "{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select count(*) as n from customers").await[0].text("n"), "1", "the rows stay");
        // Read from the catalog, since the role connected may not use the owner's schema.
        let owner = format!("select pg_get_userbyid(c.relowner)::text as o from pg_class c join pg_namespace n on n.oid = c.relnamespace where n.nspname = '{}' and c.relname = 'customers'", superuser.replace('\'', "''"));
        assert_eq!(p.rows(&owner).await[0].text("o"), superuser, "the fix moved the owner's own table");
    }

    /// The platform's objects a migration uses (a view over its functions) are made in each shadow as
    /// `reylo_platform`, a role the server's administrator made and the role connected may become:
    /// the migration replays, and no command reports them.
    pub async fn the_platforms_objects_are_in_every_shadow_as_a_role_of_its_own(mut p: P) {
        p.exec("do $$ begin create role madv_tigeradmin login createdb; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec("do $$ begin create role reylo_platform nologin; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec("grant reylo_platform to madv_tigeradmin").await;
        p.exec(&format!("alter database {} owner to madv_tigeradmin", p.name)).await;
        p.exec(
            "create schema timescale_functions; create function timescale_functions.helper() returns int language sql security definer as 'select 1';
            grant usage on schema timescale_functions to madv_tigeradmin; create function public.create_playground() returns int language sql as 'select 2';",
        )
        .await;
        p.url = as_role(&p.url, "madv_tigeradmin");
        p.schema("create view v as select timescale_functions.helper() as h, public.create_playground() as p;");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "v", &Default::default(), log).await).await, 0, "{}", p.peek());
        let written = std::fs::read_to_string(p.dir.join("migrations").join(&p.files()[0])).unwrap();
        assert!(written.contains("create view v") && !written.contains("create schema") && !written.contains("create function"), "{written}");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.status().await, 0, "{}", p.peek());
        let said = p.output();
        assert!(!said.contains("leave out"), "{said}");
        assert_eq!(p.rows("select h, p from v").await[0].text("p"), "2");
    }

    /// A trusted extension a role that is no superuser put in `pg_catalog` itself is the project's:
    /// init writes it, made there, and a deploy to an empty database makes it before what calls it.
    pub async fn a_projects_extension_in_pg_catalog_is_written(mut p: P) {
        p.exec("do $$ begin create role madv_ext login createdb; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec(&format!("alter database {} owner to madv_ext", p.name)).await;
        let admin = p.url.clone();
        p.url = as_role(&admin, "madv_ext");
        p.exec("create extension fuzzystrmatch with schema pg_catalog; create table t (distance int default levenshtein('a', 'b'))").await;
        let opts = p.opts();
        let mut out = Vec::new();
        let code = reylo_migrate::init(&opts, &[], &mut |l: &str| out.push(l.to_string())).await.unwrap_or_else(|e| panic!("init: {e}"));
        assert_eq!(code, 0, "{}", out.join("\n"));
        let schema = std::fs::read_to_string(p.dir.join("schema.sql")).unwrap();
        assert_has!(&schema, r"(?i)create extension if not exists fuzzystrmatch with schema pg_catalog");
        assert!(!has(&schema, r"(?i)create schema[^;]*pg_catalog"), "{schema}");
        p.strict = true;
        assert_eq!(p.status().await, 0, "{}", p.peek());
        // A deploy to an empty database.
        let empty = format!("{}_empty", p.name);
        let mut db = Db::connect(&admin, &p.dir).await.unwrap();
        db.exec(&format!("drop database if exists {empty} with (force)")).await.unwrap();
        db.exec(&format!("create database {empty} owner madv_ext")).await.unwrap();
        db.close().await;
        p.url = as_role(&with_database(&admin, &empty), "madv_ext");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into t default values").await;
        assert_eq!(p.rows("select distance from t").await[0].text("distance"), "1");
        assert_eq!(p.status().await, 0, "{}", p.peek());
        forget(&p.url).await;
        let mut db = Db::connect(&admin, &p.dir).await.unwrap();
        let _ = db.exec(&format!("drop database if exists {empty} with (force)")).await;
        db.close().await;
    }

    /// As Supabase makes a project: its extensions in `extensions`, a schema of the project's role,
    /// owned by a role the shadows don't give them (Supabase makes the one its role creates its
    /// administrator's). An extension in a schema of the project's is the project's, whoever owns it:
    /// init writes it, the shadows make it as the role connected, and nothing drifts.
    pub async fn an_extension_in_a_projects_schema_is_the_projects_whoever_owns_it(mut p: P) {
        p.exec("do $$ begin create role madv_ext login createdb; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec(&format!("alter database {} owner to madv_ext", p.name)).await;
        // A superuser the role connected may not act as makes it, as the platform does.
        p.exec("create schema extensions authorization madv_ext; create extension pgcrypto with schema extensions").await;
        p.url = as_role(&p.url, "madv_ext");
        p.exec("create table tokens (id int primary key, token text default encode(extensions.gen_random_bytes(8), 'hex'))").await;
        let opts = p.opts();
        let mut out = Vec::new();
        let code = reylo_migrate::init(&opts, &[], &mut |l: &str| out.push(l.to_string())).await.unwrap_or_else(|e| panic!("init: {e}"));
        assert_eq!(code, 0, "{}", out.join("\n"));
        let schema = std::fs::read_to_string(p.dir.join("schema.sql")).unwrap();
        assert_has!(&schema, r"(?i)create extension if not exists pgcrypto with schema extensions");
        p.strict = true;
        assert_eq!(p.status().await, 0, "{}", p.peek());
        p.schema(&format!("{schema}\ncreate table keys (id int primary key, key text default encode(extensions.gen_random_bytes(4), 'hex'));\n"));
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "keys", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_eq!(p.status().await, 0, "{}", p.peek());
    }

    /// The reverse: an extension a platform's role owns in a schema of the project's. It is the
    /// project's too: init writes it, so no shadow copies it as the role connected and reads that copy
    /// as a change.
    pub async fn an_extension_a_platform_role_owns_in_a_projects_schema_is_the_projects(mut p: P) {
        p.exec("do $$ begin create role supabase_auth_admin; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec("create schema extensions; create extension citext with schema extensions; update pg_catalog.pg_extension set extowner = 'supabase_auth_admin'::regrole where extname = 'citext'").await;
        p.exec("create table users (id int primary key, email extensions.citext)").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        let schema = std::fs::read_to_string(p.dir.join("schema.sql")).unwrap();
        assert_has!(&schema, r"(?i)create extension if not exists citext with schema extensions");
        p.strict = true;
        assert_eq!(p.status().await, 0, "{}", p.peek());
        p.schema(&format!("{schema}\ncreate table teams (id int primary key, name extensions.citext);\n"));
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "teams", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_eq!(p.status().await, 0, "{}", p.peek());
    }

    /// A function whose definition spans lines (Supabase's `rls_auto_enable`) is one line of the
    /// catalog stored with a fingerprint: a change made by hand is shown alone, by status and by the
    /// deploy it refuses, with no function beside it.
    pub async fn a_change_by_hand_is_shown_alone_beside_a_function_of_many_lines(mut p: P) {
        let sql = "create table users (id int primary key);\ncreate function two() returns int language plpgsql as $$\nbegin\n  return 2;\nend\n$$;\n";
        p.schema(sql);
        p.file("20261027100000_users.sql", sql);
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_eq!(p.status().await, 0, "{}", p.peek());
        p.exec("alter table users add column hand text").await;
        p.output();
        assert_eq!(p.status().await, 1);
        assert_eq!(p.migrate().await, 1);
        let out = p.output();
        assert_has!(&out, r"\+ column users\.hand text");
        assert!(!out.contains("function"), "{out}");
    }

    /// The schema of a project's extension that no dumped schema holds, with its usage given to other
    /// roles: init writes the privileges on it and its owner, which are compared, beside the
    /// `create schema`.
    pub async fn init_writes_the_privileges_on_an_extensions_schema(mut p: P) {
        p.exec("do $$ begin create role madv_ext login createdb; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec("create schema ext authorization madv_ext; grant usage on schema ext to public; create extension citext with schema ext").await;
        p.exec("create table users (id int primary key, email ext.citext)").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        let schema = std::fs::read_to_string(p.dir.join("schema.sql")).unwrap();
        assert_has!(&schema, r"(?i)grant usage on schema ext to public");
        assert_has!(&schema, r"(?i)alter schema ext owner to madv_ext");
        p.strict = true;
        assert_eq!(p.status().await, 0, "{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    /// As Supabase's local stack makes its database: `_realtime`, a schema of the platform's by name
    /// that the project's role owns, and default privileges in `storage`, another. Neither is the
    /// project's: they are no change since its migrations, and a change made by hand is shown without
    /// them.
    pub async fn a_platforms_schemas_by_name_are_left_out_whoever_owns_them(mut p: P) {
        // The database says it is a platform's, as a Supabase server's roles do: a role is the whole
        // server's, and would make every scenario beside this one a platform's.
        p.exec(&format!("alter database {} set reylo.platform_schemas = 'on'", p.name)).await;
        p.exec("create table users (id int primary key)").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        p.exec("create schema _realtime; create schema storage; alter default privileges in schema storage grant select on tables to public").await;
        p.strict = true;
        assert_eq!(p.status().await, 0, "{}", p.peek());
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("create table hand (id int)").await;
        p.output();
        assert_eq!(p.dev().await, 1);
        let out = p.output();
        assert_has!(&out, r"create table hand");
        assert!(!out.contains("_realtime") && !out.contains("storage"), "{out}");
    }

    /// On a server of no platform, the names of a platform's schemas mean nothing: a project's own
    /// `auth` and `storage`, each with a table, are pushed, written as a migration, run and checked
    /// like any schema of its own.
    pub async fn a_projects_auth_and_storage_are_its_own_on_a_server_of_no_platform(mut p: P) {
        let sql = "create schema auth;\ncreate table auth.users (id int primary key);\ncreate schema storage;\ncreate table storage.objects (id int primary key, owner int references auth.users (id));\n";
        p.schema(sql);
        assert_eq!(p.db_push().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select count(*) as n from pg_class c join pg_namespace n on n.oid = c.relnamespace where (n.nspname, c.relname) in (('auth', 'users'), ('storage', 'objects'))").await[0].text("n"), "2");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "auth_and_storage", &Default::default(), log).await).await, 0, "{}", p.peek());
        let written = std::fs::read_to_string(p.dir.join("migrations").join(&p.files()[0])).unwrap();
        assert!(written.contains("create schema auth") && written.contains("create schema storage") && written.contains("auth.users") && written.contains("storage.objects"), "{written}");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.strict = true;
        assert_eq!(p.status().await, 0, "{}", p.peek());
        let q = P::new(p.d).await.unwrap();
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(q.rows("select count(*) as n from pg_namespace where nspname in ('auth', 'storage')").await[0].text("n"), "2");
    }

    /// A TimescaleDB hypertable, which `migrate new` writes as a plain table, is listed as unread by
    /// `migrate new` and `deploy --dry-run`, rather than passing as a match. A stand-in for
    /// TimescaleDB's `timescaledb_information.hypertables` view, which the test server lacks, names it.
    pub async fn a_hypertable_is_said_to_be_unread(mut p: P) {
        let stand_in = "create schema timescaledb_information; create table timescaledb_information.hypertables (hypertable_schema name, hypertable_name name);";
        p.schema(&format!("create table metrics (time timestamptz not null, value float8);\n{stand_in}\ninsert into timescaledb_information.hypertables values ('public', 'metrics');\n"));
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "metrics", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_has!(p.output(), r"(?m)^    hypertable public\.metrics: not read, so a migration written from schema\.sql makes a plain table");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.output();
        p.exec("insert into timescaledb_information.hypertables values ('public', 'metrics')").await;
        p.dry_run().await;
        // A check of the migrations already written gives no advice for writing one.
        assert_has!(p.output(), r"(?m)^    hypertable public\.metrics: not read$");
    }

    pub async fn a_database_search_path_resolves_names_as_the_target_does(mut p: P) {
        p.exec(&format!("alter database {} set search_path = app, public", p.name)).await;
        p.schema("create schema app; create table app.users (id int);");
        p.file("20261027100000_app.sql", "create schema app;");
        p.file("20261027110000_users.sql", "create table users (id int);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.migrate().await, 0, "a second run must not report drift:\n{}", p.peek());
    }

    pub async fn a_guess_within_a_guessed_table_keeps_its_line_under_a_search_path(mut p: P) {
        p.exec(&format!("alter database {} set search_path = a, public", p.name)).await;
        p.file("20261027100000_orders.sql", "create schema a; create table a.orders (id int, x text, y text, z text, nick varchar(20));");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into a.orders (nick) values ('ada')").await;
        p.output();
        p.schema("create schema a; create table a.invoices (id int, x text, y text, z text, handle varchar(40));");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tidy", &Default::default(), log).await).await, 1);
        let out = p.output();
        // The column's guess goes on its table's line, however the schema is shown on each.
        assert_has!(&out, r"\n    --rename a\.orders=a\.invoices --rename \S*nick=handle +or   --drop a\.orders\n  Add the flags you picked to:\n");
        // Each line's rename, as printed, settles every guess and keeps the rows.
        let choices = choices_of(&picked(&out, |_| true));
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tidy", &choices, log).await).await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select handle from a.invoices").await[0].text("handle"), "ada");
    }

    pub async fn a_database_time_zone_does_not_change_defaults(mut p: P) {
        p.exec(&format!("alter database {} set timezone = 'America/Los_Angeles'", p.name)).await;
        p.schema("create table e (at timestamptz default '2026-01-01 00:00:00+00');");
        p.file("20261027100000_e.sql", "create table e (at timestamptz default '2026-01-01 00:00:00+00');");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.migrate().await, 0, "a second run must not report drift:\n{}", p.peek());
    }

    pub async fn grants_compare_without_their_grantor(mut p: P) {
        p.exec("do $$ begin if not exists (select 1 from pg_roles where rolname = 'madv_reader') then create role madv_reader; end if; if not exists (select 1 from pg_roles where rolname = 'madv_deployer') then create role madv_deployer superuser; end if; end $$").await;
        p.schema("create table t (id int); grant select on t to madv_reader;");
        p.file("20261027100000_t.sql", "create table t (id int); grant select on t to madv_reader;");
        // Applied by another role, as a deploy role would: the grantor differs from the shadows'.
        p.exec("set role madv_deployer; create table t (id int); grant select on t to madv_reader; reset role; create table __reylo_migrations (name text primary key, sql text not null, applied_at timestamptz); insert into __reylo_migrations values ('20261027100000_t.sql', 'create table t (id int); grant select on t to madv_reader;', now())").await;
        assert_eq!(p.migrate().await, 0, "expected no drift from the grantor alone:\n{}", p.peek());
    }

    pub async fn a_migration_that_creates_a_role_is_refused(mut p: P) {
        let role = format!("madv_role_{}", std::process::id());
        p.exec(&format!("drop role if exists {role}")).await;
        p.schema("create table t (id int);");
        p.file("20261027100000_t.sql", "create table t (id int);");
        p.file("20261027110000_role.sql", &format!("create role {role};"));
        assert_eq!(p.dry_run().await, 1);
        assert_has!(p.output(), &format!(r"can't be replayed in a shadow database\n\n    create role {role}\n\n  It changes a role, which belongs to the server\.\n  Run it outside the migrations"));
        assert!(p.rows(&format!("select 1 from pg_roles where rolname = '{role}'")).await.is_empty(), "no shadow created the role on the server");
    }

    pub async fn a_role_grant_is_refused_by_deploy_too(mut p: P) {
        let (reader, writer) = (format!("madv_reader_{}", std::process::id()), format!("madv_writer_{}", std::process::id()));
        p.exec(&format!("drop role if exists {writer}; drop role if exists {reader}; create role {reader} nologin; create role {writer} nologin")).await;
        let member = format!("select 1 from pg_auth_members m join pg_roles r on r.oid = m.roleid join pg_roles w on w.oid = m.member where r.rolname = '{reader}' and w.rolname = '{writer}'");
        p.file("20261027100000_t.sql", "create table t (id int);");
        p.file("20261027110000_roles.sql", &format!("grant {reader}\n  to {writer};"));
        // The whole statement, why, and what to do instead.
        let refused = format!(r"✗ Migration 20261027110000_roles\.sql can't be replayed in a shadow database\n\n    grant {reader} to {writer}\n\n  It grants or revokes a role, which belongs to the server\.\n  Run it outside the migrations, with the server's setup\. Reylo doesn't check inside a do block, but every shadow runs one on its server");
        assert_eq!(p.dry_run().await, 1);
        assert_has!(p.output(), &refused);
        // A deploy refuses what its dry run does: once it ran, every command that replays would.
        assert_eq!(p.migrate().await, 1);
        assert_has!(p.output(), &refused);
        assert!(p.rows(&member).await.is_empty(), "nothing was granted");
        assert!(p.rows("select 1 from pg_class where relname = 't'").await.is_empty(), "nothing ran");
        // In a do block, it runs, and every command takes it.
        p.file("20261027110000_roles.sql", &format!("do $$ begin grant {reader} to {writer}; end $$;"));
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert!(!p.rows(&member).await.is_empty(), "the grant ran");
        assert_eq!(p.status().await, 0, "{}", p.peek());
        p.exec(&format!("drop role {writer}; drop role {reader}")).await;
    }

    /// schema.sql is loaded into a shadow by every command that drafts or compares, and a shadow's
    /// server keeps a role made there: each refuses it before any shadow, as it does a migration's.
    pub async fn a_role_in_schema_sql_is_refused_before_any_shadow(mut p: P) {
        let role = format!("madv_schema_role_{}", std::process::id());
        p.exec(&format!("drop role if exists {role}")).await;
        p.linear();
        p.file("20261027100000_t.sql", "create table t (id int);");
        p.schema(&format!("create table t (id int);\ncreate role {role};"));
        let refused = format!(r"✗ \S*schema\.sql can't be replayed in a shadow database\n\n    create role {role}\n\n  It changes a role, which belongs to the server\.\n  Run it outside the migrations");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "add_role", &Default::default(), log).await).await, 1);
        assert_has!(p.output(), &refused);
        assert_eq!(p.files(), ["20261027100000_t.sql"], "no migration was written");
        assert_eq!(p.dev().await, 1);
        assert_has!(p.output(), &refused);
        assert_eq!(p.dry_run().await, 1);
        assert_has!(p.output(), &refused);
        assert_eq!(p.status().await, 1);
        assert_has!(p.output(), &refused);
        assert_eq!(p.history_push().await, 1);
        assert_has!(p.output(), &refused);
        std::fs::remove_file(p.dir.join(reylo_migrate::lock::LOCK)).unwrap();
        assert_eq!(p.run(async |o, log| reylo_migrate::lock_from_history(o, log).await).await, 1);
        assert_has!(p.output(), &refused);
        // A migration's role is refused by `migrate new` too, which replays every migration.
        p.schema("create table t (id int);");
        p.file("20261027110000_role.sql", &format!("create role {role};"));
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "more", &Default::default(), log).await).await, 1);
        assert_has!(p.output(), &format!(r"✗ Migration 20261027110000_role\.sql can't be replayed in a shadow database\n\n    create role {role}\n"));
        assert!(p.rows(&format!("select 1 from pg_roles where rolname = '{role}'")).await.is_empty(), "no shadow created the role on the server");
    }

    /// `history pull`, and `dev` rewinding a migration edited since, replay what the database ran
    /// rather than the files, so a role an older run or another tool recorded is refused there.
    pub async fn a_role_a_database_ran_is_refused_before_its_replay(mut p: P) {
        let role = format!("madv_ran_role_{}", std::process::id());
        p.exec(&format!("drop role if exists {role}")).await;
        p.schema("create table t (id int);");
        p.file("20261027100000_t.sql", "create table t (id int);");
        p.exec(&format!("create table t (id int); create table __reylo_migrations (name text primary key, sql text not null, applied_at timestamptz); insert into __reylo_migrations values ('20261027100000_t.sql', 'create table t (id int); create role {role};', now())")).await;
        let refused = format!(r"✗ Migration 20261027100000_t\.sql, as this database ran it, can't be replayed in a shadow database\n\n    create role {role}\n\n  It changes a role, which belongs to the server\.");
        assert_eq!(p.run(async |o, log| reylo_migrate::history_pull(o, log).await).await, 1);
        assert_has!(p.output(), &refused);
        assert_eq!(p.dev().await, 1);
        assert_has!(p.output(), &refused);
        assert!(p.rows(&format!("select 1 from pg_roles where rolname = '{role}'")).await.is_empty(), "no shadow created the role on the server");
    }

    pub async fn an_object_the_comparison_does_not_read_is_not_a_match(mut p: P) {
        p.schema("create table t (x int); create aggregate total (int) (sfunc = int4pl, stype = int);");
        p.file("20261027100000_t.sql", "create table t (x int);");
        assert_eq!(p.dry_run().await, 1, "schema.sql declares an aggregate no migration makes:\n{}", p.peek());
    }

    pub async fn a_failed_concurrent_index_leaves_nothing(mut p: P) {
        p.schema("create table t (x int);");
        p.file("20261027100000_t.sql", "create table t (x int);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into t values (1), (1)").await;
        p.file("20261027110000_unique.sql", "create unique index concurrently t_x on t (x);");
        p.schema("create table t (x int); create unique index t_x on t (x);");
        p.output();
        assert_eq!(p.migrate().await, 1);
        assert_has!(p.output(), r"✗ The migration 20261027110000_unique\.sql failed on rows that break t_x\n  could not create unique index[^\n]*\n  Nothing was recorded, so the next deploy runs it again\.\n\n  List the rows that break it:\n    select x, count\(\*\) from t where x is not null group by x having count\(\*\) > 1;\n  Fix them here, then deploy again:\n    reylo migrate deploy\n");
        assert!(p.rows("select 1 from pg_class where relname = 't_x'").await.is_empty(), "the invalid index was dropped");
        p.exec("delete from t where ctid = (select max(ctid) from t)").await;
        assert_eq!(p.migrate().await, 0, "the file runs again as written:\n{}", p.peek());
    }

    pub async fn a_failed_lone_statement_may_leave_its_change(mut p: P) {
        p.schema("create table t (x int);");
        p.file("20261027100000_t.sql", "create table t (x int);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        // Run outside a transaction, a statement other than an index build may leave part of itself,
        // as a detach left pending: deploy never says it rolled back.
        p.file("20261027110000_drop.sql", "drop index concurrently t_missing;");
        p.output();
        assert_eq!(p.migrate().await, 1);
        let out = p.output();
        assert_has!(&out, r"✗ The migration 20261027110000_drop\.sql failed\n  [^\n]*t_missing[^\n]*\n  It runs outside a transaction, so its change may be in place\.\n\n  See what is:\n    reylo migrate deploy --dry-run$");
        assert!(!out.contains("rolled back"), "{out}");
    }

    pub async fn a_platforms_objects_are_left_out(mut p: P) {
        // A Supabase-like platform, made by hand: its roles, its schema, its table.
        p.exec("do $$ begin create role supabase_auth_admin; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec("create schema auth authorization supabase_auth_admin; create table auth.users (id int primary key); alter table auth.users owner to supabase_auth_admin; create function auth.uid() returns int language sql as 'select 1'; alter function auth.uid() owner to supabase_auth_admin").await;
        p.schema("create table profiles (id int primary key);");
        p.file("20261029100000_profiles.sql", "create table profiles (id int primary key);");
        assert_eq!(p.migrate().await, 0, "the platform's schema, table and function are not drift:\n{}", p.peek());
        // The project's own objects on the platform's table are the project's: one added by hand is drift.
        p.exec("create policy own on auth.users using (true)").await;
        p.output();
        assert_eq!(p.dry_run().await, 1);
        assert_has!(p.output(), r"This database was changed outside its migrations[\s\S]*policy own on auth\.users");
    }

    pub async fn a_platforms_schemas_and_extensions_are_in_every_shadow(mut p: P) {
        // As Supabase makes a project: a schema a platform's role owns, with the table a project's
        // key and trigger name, and an extension in a schema of the project's role.
        p.exec("do $$ begin create role supabase_auth_admin; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec("create schema auth authorization supabase_auth_admin; create table auth.users (id uuid primary key); alter table auth.users owner to supabase_auth_admin").await;
        p.exec("create schema extensions; create extension pgcrypto with schema extensions").await;
        p.schema(
            "create table profiles (id uuid primary key references auth.users on delete cascade, token text default encode(extensions.gen_random_bytes(8), 'hex'));
            create function handle_new_user() returns trigger language plpgsql as $$ begin insert into public.profiles (id) values (new.id); return new; end $$;
            create trigger on_auth_user_created after insert on auth.users for each row execute function handle_new_user();",
        );
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "profiles", &Default::default(), log).await).await, 0, "{}", p.peek());
        let written = std::fs::read_to_string(p.dir.join("migrations").join(&p.files()[0])).unwrap();
        assert!(written.contains("references auth.users") && written.contains("on auth.users"), "{written}");
        assert!(!written.contains("create schema auth") && !written.contains("create table auth.users") && !written.contains("create extension"), "{written}");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select 1 from pg_extension where extname = 'pgcrypto'").await.len(), 1, "the platform's extension stays");
        p.exec("insert into auth.users values ('00000000-0000-0000-0000-000000000001')").await;
        assert_eq!(p.rows("select token from profiles").await.len(), 1, "the trigger and the default ran");
    }

    /// As Azure makes its `postgres` database: an extension its superuser made in `pg_catalog`
    /// (`pgaadauth`), adopted by a role that is no superuser. No file holds it, and every command runs.
    pub async fn a_platforms_extension_in_pg_catalog_is_not_the_projects(mut p: P) {
        p.exec("do $$ begin create role madv_azuresu superuser; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec("do $$ begin create role madv_azure login createdb; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec("set role madv_azuresu; create extension fuzzystrmatch with schema pg_catalog; reset role").await;
        p.exec(&format!("alter database {} owner to madv_azure", p.name)).await;
        p.url = as_role(&p.url, "madv_azure");
        p.exec("create table users (id int primary key, email text not null)").await;
        let opts = p.opts();
        let mut out = Vec::new();
        let code = reylo_migrate::init(&opts, &[], &mut |l: &str| out.push(l.to_string())).await.unwrap_or_else(|e| panic!("init: {e}"));
        assert_eq!(code, 0, "{}", out.join("\n"));
        let text = std::fs::read_to_string(p.dir.join("schema.sql")).unwrap();
        assert!(!text.contains("pg_catalog") && !text.contains("fuzzystrmatch"), "{text}");
        p.strict = true;
        assert_eq!(p.status().await, 0, "{}", p.peek());
        p.schema(&format!("{text}\ncreate table posts (id int primary key, user_id int references users (id));\n"));
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "posts", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.status().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select 1 from pg_extension where extname = 'fuzzystrmatch'").await.len(), 1, "the server's extension stays");
    }

    pub async fn init_reads_a_function_dumped_before_the_table_it_reads(mut p: P) {
        // pg_dump writes functions before tables, and tables before the keys between them.
        p.exec("create table users (id int primary key, best_post int); create table posts (id int primary key, author int references users); alter table users add foreign key (best_post) references posts").await;
        p.exec("create function user_count() returns bigint language sql as $$ select count(*) from users $$; create table quotas (n int check (n <= user_count() + 100))").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        let text = std::fs::read_to_string(p.dir.join("schema.sql")).unwrap();
        assert!(text.contains("function user_count()") && text.contains("create table quotas"), "{text}");
        assert_eq!(introspect(&p.opts()).await, text);
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    /// Postgres reads `#` as an operator, which the lock keeps everything after as written: init.sql
    /// is locked as the checks read it back.
    pub async fn init_locks_a_schema_that_uses_a_hash_operator(mut p: P) {
        p.exec("create table t (id int primary key, j jsonb, a int, b int, check ((a # b) >= 0)); create index t_j on t ((j #>> '{k}'))").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        assert_eq!(p.history().await, ["init.sql"]);
        p.strict = true;
        assert_eq!(p.status().await, 0, "{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    /// A string the dump writes with a bare carriage return in it: init.sql is locked as the checks read
    /// it back, line endings and all.
    pub async fn init_locks_a_schema_with_a_carriage_return_in_a_string(mut p: P) {
        p.exec("create table t (c text, constraint t_c_check check (btrim(c, E' \\t\\r\\n') <> ''))").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        assert!(std::fs::read_to_string(p.dir.join("migrations/init.sql")).unwrap().contains('\r'));
        assert_eq!(p.history().await, ["init.sql"]);
        p.strict = true;
        assert_eq!(p.status().await, 0, "{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    pub async fn reylo_run_runs_inline_sql_and_files(mut p: P) {
        let items = [("create".to_string(), "create table t (x int); insert into t values (1)".to_string()), ("seed.sql".to_string(), "insert into t values (2)".to_string())];
        assert_eq!(p.run(async |o, log| reylo_migrate::run_sql(o, &items, log).await).await, 0, "{}", p.peek());
        assert_eq!(p.rows("select x from t order by x").await.iter().map(|r| r.text("x").to_string()).collect::<Vec<_>>(), ["1", "2"]);
        assert_eq!(p.rows("select to_regclass('__reylo_migrations') as t").await[0].get("t"), None, "records nothing");
        // What each statement returns is shown under its line, its columns where it returned no row.
        p.output();
        let items = [("rows".to_string(), "select x from t order by x; select x from t where false; update t set x = x returning x".to_string())];
        assert_eq!(p.run(async |o, log| reylo_migrate::run_sql(o, &items, log).await).await, 0, "{}", p.peek());
        assert_eq!(p.output(), "✓ rows\n  x\n  ─\n  1\n  2\n  Returned 2 rows\n\n  x\n  ─\n  Returned 0 rows\n\n  x\n  ─\n  1\n  2\n  Returned 2 rows");
    }

    pub async fn history_push_leaves_a_data_migration_pending(mut p: P) {
        p.schema("create table t (x int);");
        p.file("20261027100000_t.sql", "create table t (x int);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into t values (1)").await;
        p.file("20261027110000_backfill.sql", "update t set x = x * 10;");
        p.output();
        assert_eq!(p.history_push().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"» This database hasn't run 20261027110000_backfill\.sql\n  To run it:\n    reylo migrate deploy");
        assert_eq!(p.rows("select x from t").await[0].text("x"), "1", "history push ran nothing");
    }

    pub async fn a_platforms_default_privileges_are_not_drift(mut p: P) {
        // As a platform sets them before any migration: every new table and function its API's role may use.
        p.exec("do $$ begin create role madv_api; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec("alter default privileges in schema public grant select on tables to madv_api; alter default privileges grant execute on functions to madv_api").await;
        p.schema("create table t (id int); create function f() returns int language sql as 'select 1';");
        p.file("20261030100000_t.sql", "create table t (id int); create function f() returns int language sql as 'select 1';");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.migrate().await, 0, "the grants the defaults made are not drift:\n{}", p.peek());
        assert!(!p.rows("select 1 from information_schema.role_table_grants where grantee = 'madv_api' and table_name = 't'").await.is_empty(), "the target granted it");
    }

    pub async fn function_privileges_and_index_comments_are_compared(mut p: P) {
        p.schema("create table t (x int); create index t_x on t (x); comment on index t_x is 'by x'; create function f() returns int language sql as 'select 1'; revoke execute on function f() from public;");
        p.file("20261030100000_t.sql", "create table t (x int); create index t_x on t (x); create function f() returns int language sql as 'select 1';");
        assert_eq!(p.dry_run().await, 1);
        let out = p.output();
        assert!(has(&out, r"revoke execute on function f\(\) from public"), "{out}");
        assert!(has(&out, r"comment on index t_x is 'by x'"), "{out}");
    }

    /// MySQL grants on a table and a column are written in a migration, checked, and pushed back
    /// where one is taken away by hand. MySQL keeps a dropped database's grants, and no shadow's
    /// outlives it.
    pub async fn a_grant_is_written_pushed_and_left_by_no_shadow(mut p: P) {
        p.exec("create user if not exists 'madv_reader'@'%'").await;
        let shadows = "select count(*) as n from mysql.tables_priv where user = 'madv_reader' and db like 'reylo\\_shadow\\_%'";
        p.schema("create table t (id int primary key, note text); grant select on t to 'madv_reader'@'%';");
        p.file("20261120100000_t.sql", "create table t (id int primary key, note text);");
        p.file("20261120100001_read.sql", "grant select on t to 'madv_reader'@'%';");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.output();
        p.schema("create table t (id int primary key, note text); grant select on t to 'madv_reader'@'%'; grant update (note) on t to 'madv_reader'@'%';");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "note", &Default::default(), log).await).await, 0, "{}", p.peek());
        p.output();
        let written = p.files().into_iter().find(|f| f.ends_with("_note.sql")).unwrap();
        let text = std::fs::read_to_string(p.dir.join("migrations").join(&written)).unwrap();
        assert!(has(&text, r"grant update \(note\) on t to `madv_reader`@`%`"), "{text}");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.output();
        let (code, out) = p.check_every_way().await;
        assert_eq!(code, 0, "{out}");
        p.exec("revoke select on t from 'madv_reader'@'%'").await;
        assert_eq!(p.db_push().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"grant select on t to `madv_reader`@`%`");
        assert_eq!(p.rows("select count(*) as n from mysql.tables_priv where user = 'madv_reader' and db = database() and table_priv = 'Select'").await[0].text("n"), "1");
        assert_eq!(p.rows(shadows).await[0].text("n"), "0", "a shadow's grants outlived it");
    }

    /// An account made in schema.sql, or a role granted to one, would be made on the shadow's server
    /// by `migrate new`: refused before any shadow, as in a migration.
    pub async fn an_account_or_role_grant_in_schema_sql_is_refused_before_any_shadow(mut p: P) {
        let (user, role) = (format!("madv_schema_user_{}", std::process::id()), format!("madv_schema_role_{}", std::process::id()));
        p.exec(&format!("drop user if exists '{user}'@'%'; drop role if exists '{role}'")).await;
        p.linear();
        p.file("20261027100000_t.sql", "create table t (id int primary key);");
        p.schema(&format!("create table t (id int primary key);\ncreate user '{user}'@'%';"));
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "add_user", &Default::default(), log).await).await, 1);
        assert_has!(p.output(), r"✗ \S*schema\.sql can't be replayed in a shadow database\n\n    create user '…'@'…'\n");
        assert_eq!(p.dev().await, 1);
        assert_has!(p.output(), r"✗ \S*schema\.sql can't be replayed in a shadow database");
        assert!(p.rows(&format!("select 1 from mysql.user where user = '{user}'")).await.is_empty(), "no shadow created the account on the server");
        // A role's grant to an account that exists is the server's too.
        p.exec(&format!("create role '{role}'; create user '{user}'@'%'")).await;
        p.schema(&format!("create table t (id int primary key);\ngrant '{role}' to '{user}'@'%';"));
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "add_grant", &Default::default(), log).await).await, 1);
        assert_has!(p.output(), r"✗ \S*schema\.sql can't be replayed in a shadow database\n\n    grant '…' to '…'@'…'\n\n  It grants or revokes a role, which belongs to the server\.");
        assert_eq!(p.files(), ["20261027100000_t.sql"], "no migration was written");
        assert!(p.rows(&format!("select 1 from mysql.role_edges where from_user = '{role}' and to_user = '{user}'")).await.is_empty(), "nothing was granted");
        p.exec(&format!("drop user '{user}'@'%'; drop role '{role}'")).await;
    }

    /// An account with its database's privileges alone sees only its own grants in MySQL, so the
    /// grants schema.sql makes would be planned from nothing: they are refused, naming what to grant.
    pub async fn grants_an_account_cannot_read_back_are_refused(mut p: P) {
        let role = "madv_noread";
        p.exec(&format!("create user if not exists '{role}'@'%'; create user if not exists 'madv_reader'@'%'; grant all on `{}`.* to '{role}'@'%' with grant option", p.name)).await;
        p.url = as_role(&p.url, role);
        p.schema("create table t (id int primary key); grant select on t to 'madv_reader'@'%';");
        assert_eq!(p.db_push().await, 1);
        let out = p.output();
        assert_has!(&out, r"✗ Can't plan the grants in schema\.sql");
        assert_has!(&out, r"grant select on mysql\.tables_priv to 'madv_noread'@'%';");
        // Nor does a migration written from it leave them out.
        assert_ne!(p.run(async |o, log| reylo_migrate::create(o, "t", &Default::default(), log).await).await, 0);
        assert_has!(p.output(), r"Can't plan the grants in schema\.sql");
        assert!(p.files().is_empty(), "a migration was written: {:?}", p.files());
    }

    pub async fn a_comment_made_by_hand_stops_nothing(mut p: P) {
        let by_hand = if p.d == Dialect::Mysql { "alter table t comment = 'by hand'" } else { "comment on table t is 'by hand'" };
        p.schema("create table t (id int);");
        p.file("20261027100000_t.sql", "create table t (id int);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        p.exec(by_hand).await;
        // No drift: the next deploy runs, the replay agrees, the history stands and development leaves it be.
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        let (code, out) = p.check_every_way().await;
        assert_eq!(code, 0, "{out}");
        assert_eq!(p.history_push().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_has!(p.output(), "Up to date");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
    }

    /// A temporary table is its session's: its key, its checks, its policies, its statistics (made in
    /// `public`, the first schema of the search path) and their comments are gone when the run that
    /// made them ends, and are in no fingerprint and no comparison.
    pub async fn a_temporary_tables_constraints_and_policies_are_no_drift(mut p: P) {
        p.schema("create table t (id int primary key);");
        p.file(
            "20261027100000_t.sql",
            "create table t (id int primary key);
            create temp table scratch (key int primary key, n int check (n > 0));
            comment on constraint scratch_pkey on scratch is 'for this session';
            create policy everyone on scratch using (true);
            comment on policy everyone on scratch is 'for this session';
            create statistics scratch_stats on key, n from scratch;
            alter statistics scratch_stats set statistics 500;
            comment on statistics scratch_stats is 'for this session';",
        );
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_has!(p.output(), "Up to date");
        let (code, out) = p.check_every_way().await;
        assert_eq!(code, 0, "{out}");
        assert_eq!(p.history_push().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_has!(p.output(), "Up to date");
    }

    /// An enum is one line of the catalog, so a value added changes that line on both sides: schema.sql's
    /// enum with a value more is schema.sql ahead, pushed or not, and a migration's is the migrations'.
    pub async fn an_enum_value_added_is_ahead_on_the_side_that_added_it(mut p: P) {
        let declared = |values: &str| format!("create type visibility as enum ({values}); create table bookmarks (id int primary key, visibility visibility not null default 'private');");
        p.linear();
        p.schema(&declared("'private', 'public'"));
        p.file("20261027100000_bookmarks.sql", &declared("'private', 'public'"));
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.output();
        p.schema(&declared("'private', 'team', 'public'"));
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"» The schema is ahead of the migrations\n  To write the changes to a migration file:\n    reylo migrate new <name>");
        assert_eq!(p.dry_run().await, 1, "{}", p.peek());
        assert_has!(p.output(), r"The schema is ahead of the migrations\n\n[^\n]*add value 'team' after 'private'");
        assert_eq!(p.db_push().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"✓ Up to date with the migrations\n» The schema is ahead of the migrations\n  To write the changes to a migration file:\n    reylo migrate new <name>");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "team", &Default::default(), log).await).await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!has(&out, "ahead|disagree"), "{out}");
        // A value a migration adds and schema.sql doesn't declare is the migrations'.
        p.file("20261027100100_archived.sql", "alter type visibility add value 'archived';");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"The migrations make what \S*schema\.sql doesn't declare");
        assert_eq!(p.dry_run().await, 1, "{}", p.peek());
        assert_has!(p.output(), r"The migrations make what \S*schema\.sql doesn't declare");
    }

    /// A comment a migration makes and schema.sql doesn't declare is the migrations', though this
    /// database, whose comments aren't compared, matches schema.sql as well as the migrations.
    pub async fn a_comment_a_migration_adds_is_the_migrations(mut p: P) {
        p.linear();
        p.schema("create table t (id int);");
        p.file("20261027100000_t.sql", "create table t (id int);");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.output();
        p.file("20261027100100_doc.sql", "comment on table t is 'the table';");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        let out = p.output();
        assert_has!(&out, r"The migrations make what \S*schema\.sql doesn't declare");
        assert!(!has(&out, "ahead"), "{out}");
    }

    pub async fn a_comment_in_schema_sql_is_written(mut p: P) {
        let declared = |text: &str| if p.d == Dialect::Mysql { format!("create table t (id int) comment '{text}';") } else { format!("create table t (id int); comment on table t is '{text}';") };
        let (first, second) = (declared("the table"), declared("the only table"));
        p.schema("create table t (id int);");
        p.file("20261027100000_t.sql", "create table t (id int);");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.output();
        // In a migration, and until one writes it, the migrations don't reach schema.sql.
        p.schema(&first);
        assert_eq!(p.dry_run().await, 1, "{}", p.peek());
        p.output();
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "doc", &Default::default(), log).await).await, 0, "{}", p.peek());
        p.output();
        let written = p.files().into_iter().find(|f| f.ends_with("_doc.sql")).unwrap();
        let text = std::fs::read_to_string(p.dir.join("migrations").join(&written)).unwrap();
        assert!(has(&text, "the table"), "{text}");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.output();
        let (code, out) = p.check_every_way().await;
        assert_eq!(code, 0, "{out}");
        // And by sync, which takes the database to schema.sql, a comment alone included.
        p.schema(&second);
        assert_eq!(p.db_push().await, 0, "{}", p.peek());
        assert_has!(p.output(), "the only table");
    }

    pub async fn a_server_setting_change_misses_the_cache(mut p: P) {
        p.schema("create schema app; create table t (id int);");
        p.file("20261027100000_app.sql", "create schema app;");
        p.file("20261027110000_t.sql", "create table t (id int);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.check_every_way().await.0, 0);
        // The same server, through sessions whose search_path is app's: every replay now puts t in
        // app, which the live table is not in. A cache keyed without the session's settings would
        // answer that nothing changed.
        let plain = p.url.clone();
        p.url = format!("{plain}?options=-c%20search_path%3Dapp%2Cpublic");
        let (code, out) = p.check_every_way().await;
        assert_eq!(code, 1);
        assert_has!(out, r"This database was changed outside its migrations");
        forget(&p.url).await;
        // The target's own database setting, which every shadow takes, likewise.
        p.url = plain;
        p.exec(&format!("alter database {} set search_path = app, public", p.name)).await;
        let (code, out) = p.check_every_way().await;
        assert_eq!(code, 1);
        assert_has!(out, r"This database was changed outside its migrations");
    }

    pub async fn a_migrations_session_setting_is_never_carried_by_the_cache(mut p: P) {
        // The first migration leaves its session's search_path on app, which puts the second's
        // table there in a replay; the second sets it back. A shadow copied from a cache opens a new
        // session, without it.
        p.file("20261027100000_app.sql", "create schema app; set search_path = app, public;");
        p.file("20261027110000_t.sql", "create table t (id int); reset search_path;");
        p.file("20261027120000_u.sql", "create table u (id int);");
        p.schema("create schema app; create table app.t (id int); create table u (id int);");
        assert_eq!(p.check_every_way().await.0, 0);
        // The target applies the first alone, so the check's history stops in the middle.
        let later = ["20261027110000_t.sql", "20261027120000_u.sql"].map(|f| (f, std::fs::read_to_string(p.dir.join("migrations").join(f)).unwrap()));
        for (f, _) in &later {
            std::fs::remove_file(p.dir.join("migrations").join(f)).unwrap();
        }
        p.schema("create schema app;");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        for (f, sql) in &later {
            p.file(f, sql);
        }
        p.schema("create schema app; create table app.t (id int); create table u (id int);");
        p.check_every_way().await;
    }

    pub async fn new_writes_constraints_and_indexes_that_block_no_writes(mut p: P) {
        p.schema("create table users (id int primary key, n int, code text, email text);");
        p.file("20261027100000_users.sql", "create table users (id int primary key, n int, code text, email text);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into users values (1, 2, 'a', 'a@x'), (2, 3, 'b', 'b@x')").await;
        p.output();
        p.schema(
            "create table users (id int primary key, n int not null constraint n_pos check (n > 0), code text, email text constraint users_email_key unique);
            create index users_code_idx on users (code);",
        );
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "tighten", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_has!(p.output(), r"✓ Wrote \d+ migrations, so that none blocks writes to a table while it reads every row:");
        let made: Vec<String> = p.files().into_iter().filter(|f| f.contains("_tighten")).map(|f| std::fs::read_to_string(p.dir.join("migrations").join(f)).unwrap()).collect();
        let all = made.join("-- next\n");
        let statements = |f: &str| f.lines().filter(|l| !l.starts_with("--") && !l.trim().is_empty()).count();
        // Each constraint added without reading a row, each index built concurrently in a file of
        // its own, and the constraints validated in the last file, with the column made not null.
        assert!(all.contains("check (n > 0) not valid") && all.contains("check (n is not null) not valid"), "{all}");
        let concurrent: Vec<&String> = made.iter().filter(|f| f.contains(" concurrently ")).collect();
        assert!(concurrent.len() == 2 && concurrent.iter().all(|f| statements(f) == 1), "{all}");
        assert!(all.contains("add constraint users_email_key unique using index users_email_key"), "{all}");
        let last = made.last().unwrap();
        assert!(last.contains("validate constraint n_pos") && last.contains("set not null"), "{all}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert!(!p.output().contains(" It "), "nothing pending holds a lock");
    }

    /// The files `reylo migrate new <name>` wrote, in order, each as its text.
    fn written(p: &P, name: &str) -> Vec<String> {
        p.files().into_iter().filter(|f| f.contains(&format!("_{name}"))).map(|f| std::fs::read_to_string(p.dir.join("migrations").join(f)).unwrap()).collect()
    }

    /// Each index of the database, with its definition and whether it is valid.
    async fn indexes(p: &P) -> Vec<String> {
        p.rows("select c.relname || ' ' || i.indisvalid || ' ' || pg_get_indexdef(i.indexrelid) as line from pg_index i join pg_class c on c.oid = i.indexrelid join pg_namespace n on n.oid = c.relnamespace where n.nspname = 'public' order by 1")
            .await
            .iter()
            .map(|r| r.text("line").to_string())
            .collect()
    }

    pub async fn new_builds_a_changed_index_before_it_drops_the_old_one(mut p: P) {
        let v1 = "create table users (id int primary key, email text not null);
            create table posts (id int primary key, created_at timestamptz not null, score int);
            create table invites (id int primary key, email text, team_id int, constraint invites_email_team unique nulls not distinct (email, team_id));
            create unique index users_email_lower on users (lower(email));
            create index posts_recent on posts (created_at desc, score asc nulls first);";
        p.schema(v1);
        p.file("20261027100000_v1.sql", v1);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into users values (1, 'a@x'), (2, 'b@x'); insert into posts values (1, now(), 1); insert into invites values (1, 'a@x', null)").await;
        p.output();
        p.schema(
            "create table users (id int primary key, email text not null);
            create table posts (id int primary key, created_at timestamptz not null, score int);
            create table invites (id int primary key, email text, team_id int, constraint invites_email_team unique (email, team_id));
            create unique index users_email_lower on users (lower(trim(email)));
            create index posts_recent on posts (created_at asc, score desc nulls last);",
        );
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "change", &Default::default(), log).await).await, 0, "{}", p.peek());
        let made = written(&p, "change");
        let all = made.join("-- next\n");
        // Each replacement built concurrently under a name of its own, alone, before anything is
        // dropped; then the old one dropped in the transaction that takes the new one in its place.
        for (build, swap) in [
            ("create unique index concurrently users_email_lower_new on users (lower(trim(email)));", "drop index users_email_lower;\nalter index users_email_lower_new rename to users_email_lower;"),
            ("create index concurrently posts_recent_new on posts (created_at asc, score desc nulls last);", "drop index posts_recent;\nalter index posts_recent_new rename to posts_recent;"),
            ("create unique index concurrently invites_email_team_new on invites (email, team_id);", "alter table invites drop constraint invites_email_team;\nalter table invites add constraint invites_email_team unique using index invites_email_team_new;"),
        ] {
            let at = made.iter().position(|f| f.trim() == build).unwrap_or_else(|| panic!("no file builds {build}:\n{all}"));
            assert!(made[at + 1].contains(swap), "the file after {build} takes it in place:\n{all}");
            assert!(made[..at].iter().all(|f| !f.contains(swap.lines().next().unwrap())), "nothing is dropped before its replacement is built:\n{all}");
        }
        // Deployed up to the first build alone, the old index still holds.
        let later: Vec<(String, String)> = p.files().into_iter().filter(|f| f.contains("_change")).skip(1).map(|f| (f.clone(), std::fs::read_to_string(p.dir.join("migrations").join(&f)).unwrap())).collect();
        for (f, _) in &later {
            std::fs::remove_file(p.dir.join("migrations").join(f)).unwrap();
        }
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert!(p.try_rows("insert into users values (3, 'A@x')").await.is_err(), "the old unique index still holds");
        for (f, sql) in &later {
            p.file(f, sql);
        }
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.check_every_way().await.0, 0, "the migrations reach schema.sql");
        let now = indexes(&p).await;
        assert!(now.iter().all(|l| !l.contains("_new") && l.contains(" true ")), "{now:#?}");
        assert!(now.iter().any(|l| l.starts_with("users_email_lower true CREATE UNIQUE INDEX users_email_lower ON public.users USING btree (lower(TRIM(BOTH FROM email)))")), "{now:#?}");
    }

    pub async fn a_changed_unique_index_that_fails_to_build_leaves_the_old_one(mut p: P) {
        let v1 = "create table users (id int primary key, org int not null, email text not null); create unique index users_email on users (org, email);";
        p.schema(v1);
        p.file("20261027100000_v1.sql", v1);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        p.schema("create table users (id int primary key, org int not null, email text not null); create unique index users_email on users (email);");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "by_email", &Default::default(), log).await).await, 0, "{}", p.peek());
        p.output();
        // Rows the new key refuses, written before the deploy.
        p.exec("insert into users values (1, 1, 'a'), (2, 2, 'a')").await;
        assert_eq!(p.migrate().await, 1, "the build fails on the duplicates");
        p.output();
        let now = indexes(&p).await;
        assert!(now.iter().any(|l| l.starts_with("users_email true ") && l.contains("(org, email)")), "the old index is there, and valid: {now:#?}");
        assert!(now.iter().all(|l| !l.contains("users_email_new")), "the failed build left nothing: {now:#?}");
        assert!(p.try_rows("insert into users values (3, 1, 'a')").await.is_err(), "the old key still holds");
        p.exec("delete from users where id = 2").await;
        assert_eq!(p.migrate().await, 0, "the next deploy runs it again:\n{}", p.peek());
        p.output();
        assert_eq!(p.check_every_way().await.0, 0);
    }

    pub async fn new_adds_a_key_over_an_index_built_concurrently(mut p: P) {
        let v1 = "create table teams (id int not null, name text not null, code text); create table users (id int primary key, team_id int);";
        p.schema(v1);
        p.file("20261027100000_v1.sql", v1);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into teams values (1, 'a', 'x'), (2, 'b', null)").await;
        p.output();
        p.schema(
            "create table teams (id int not null, name text not null, code text,
              constraint teams_pkey primary key (id),
              constraint teams_code_key unique nulls not distinct (code) include (name));
            create table users (id int primary key, team_id int references teams (id) deferrable initially deferred);",
        );
        // No --blocking: each key's index is built concurrently, then taken by the key.
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "keys", &Default::default(), log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("deferred deferrable"), "a deferral is shown once:\n{out}");
        assert_has!(&out, r"\+ constraint users_team_id_fkey on users: foreign key \(team_id\) references teams\(id\) deferrable initially deferred(\n|$)");
        let all = written(&p, "keys").join("-- next\n");
        for statement in [
            "create unique index concurrently teams_pkey on teams (id);",
            "alter table teams add constraint teams_pkey primary key using index teams_pkey;",
            "create unique index concurrently teams_code_key on teams (code) include (name) nulls not distinct;",
            "alter table teams add constraint teams_code_key unique using index teams_code_key;",
        ] {
            assert!(all.contains(statement), "{statement}:\n{all}");
        }
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.check_every_way().await.0, 0, "the migrations reach schema.sql");
        // A deferrable key still needs --blocking: an index built first would check each row at once.
        p.schema(
            "create table teams (id int not null, name text not null, code text,
              constraint teams_pkey primary key (id),
              constraint teams_code_key unique nulls not distinct (code) include (name),
              constraint teams_name_key unique (name) deferrable);
            create table users (id int primary key, team_id int references teams (id) deferrable initially deferred);",
        );
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "deferrable", &Default::default(), log).await).await, 1);
        assert_has!(p.output(), r"reylo migrate new deferrable --blocking");
        let blocking = reylo_migrate::Choices { blocking: true, ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "deferrable", &blocking, log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert_has!(&out, r"\+ constraint teams_name_key on teams: unique \(name\) deferrable(\n|$)");
        assert!(written(&p, "deferrable").concat().contains("alter table teams add constraint teams_name_key unique (name) deferrable;"), "{}", written(&p, "deferrable").concat());
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.check_every_way().await.0, 0);
    }

    pub async fn new_builds_an_index_on_a_materialized_view_concurrently(mut p: P) {
        let v1 = "create table bookings (room int not null, total numeric not null);
            create materialized view room_totals as select room, sum(total) as total from bookings group by room;
            create unique index room_totals_room on room_totals (room);";
        p.schema(v1);
        p.file("20261027100000_v1.sql", v1);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into bookings values (1, 10), (2, 20); refresh materialized view room_totals").await;
        p.output();
        let v2 = format!("{v1}\ncreate index room_totals_total on room_totals (total desc);");
        p.schema(&v2);
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "by_total", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_eq!(written(&p, "by_total"), vec!["create index concurrently room_totals_total on room_totals (total desc);\n".to_string()]);
        let out = p.output();
        assert!(!out.contains(" It "), "nothing holds a lock:\n{out}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.check_every_way().await.0, 0);
        // A changed query makes the view again, which still blocks.
        p.schema(&v2.replace("sum(total) as total", "sum(total) as total, count(*) as n"));
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "count", &Default::default(), log).await).await, 1);
        assert_has!(p.output(), r"reylo migrate new count --blocking");
    }

    pub async fn new_renames_an_index_changed_only_in_its_name(mut p: P) {
        let v1 = "create table t (id int primary key, n int); create index t_a on t (n);";
        p.schema(v1);
        p.file("20261027100000_v1.sql", v1);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into t values (1, 1)").await;
        p.output();
        p.schema("create table t (id int primary key, n int); create index t_b on t (n);");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "rename", &Default::default(), log).await).await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("keeping its rows"), "{out}");
        assert_eq!(written(&p, "rename"), vec!["alter index t_a rename to t_b;\n".to_string()]);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.check_every_way().await.0, 0);
        // An index and a constraint that trade names are no rename.
        p.schema("create table t (id int primary key, n int, m int, constraint t_y unique (m)); create index t_b on t (n);");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "key", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        p.schema("create table t (id int primary key, n int, m int, constraint t_b unique (m)); create index t_y on t (n);");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "trade", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.check_every_way().await.0, 0);
    }

    pub async fn new_drops_a_foreign_key_on_a_replaced_key_with_it(mut p: P) {
        let v1 = "create table t (id int primary key, code text, constraint t_code_key unique (code)); create table u (id int primary key, code text references t (code));";
        p.schema(v1);
        p.file("20261027100000_v1.sql", v1);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into t values (1, 'a'); insert into u values (1, 'a')").await;
        p.output();
        p.schema("create table t (id int primary key, code text, constraint t_code_key unique nulls not distinct (code)); create table u (id int primary key, code text references t (code));");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "nulls", &Default::default(), log).await).await, 0, "{}", p.peek());
        let made = written(&p, "nulls");
        assert_eq!(made[0], "create unique index concurrently t_code_key_new on t (code) nulls not distinct;\n", "{made:#?}");
        // The key's foreign key goes in the transaction that replaces the key, and comes back there.
        assert!(made[1].starts_with("alter table u drop constraint u_code_fkey;\nalter table t drop constraint t_code_key;\nalter table t add constraint t_code_key unique using index t_code_key_new;\n"), "{made:#?}");
        assert!(made[1].contains("not valid") && made[2].contains("validate constraint u_code_fkey"), "{made:#?}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.check_every_way().await.0, 0);
    }

    pub async fn dev_rewinds_a_changed_index(mut p: P) {
        let v1 = "create table users (id int primary key, email text not null); create unique index users_email_lower on users (lower(email));";
        p.schema(v1);
        p.file("20261027100000_v1.sql", v1);
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("insert into users values (1, 'a@x')").await;
        p.schema("create table users (id int primary key, email text not null); create unique index users_email_lower on users (lower(trim(email)));");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "trim", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.output();
        // Both files taken back out, and schema.sql with them: dev rewinds the pair.
        for f in p.files().into_iter().filter(|f| f.contains("_trim")) {
            std::fs::remove_file(p.dir.join("migrations").join(f)).unwrap();
        }
        p.schema(v1);
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.output();
        let now = indexes(&p).await;
        assert!(now.iter().any(|l| l.starts_with("users_email_lower true ") && l.ends_with("(lower(email))")), "{now:#?}");
        assert!(now.iter().all(|l| !l.contains("_new")), "{now:#?}");
        assert_eq!(p.rows("select count(*)::text as n from users").await[0].text("n"), "1", "a rewind keeps the rows");
    }

    pub async fn new_renames_a_column_under_its_indexes_keys_and_views(mut p: P) {
        let v1 = "create table posts (pid int primary key, title text, constraint pid_pos check (pid > 0));
            create index posts_title on posts (title, pid);
            create table comments (id int primary key, post int references posts (pid));
            create view titles as select pid, title from posts;";
        p.schema(v1);
        p.file("20261027100000_v1.sql", v1);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into posts values (1, 'a'); insert into comments values (1, 1)").await;
        p.output();
        p.schema(
            "create table posts (post_id int primary key, title text, constraint pid_pos check (post_id > 0));
            create index posts_title on posts (title, post_id);
            create table comments (id int primary key, post int references posts (post_id));
            create view titles as select post_id as pid, title from posts;",
        );
        let choices = reylo_migrate::Choices { rename: vec![("posts.pid".into(), "post_id".into())], ..Default::default() };
        // The rename alone: the server renames the column in each index, key, check and view.
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "rename_pid", &choices, log).await).await, 0, "{}", p.peek());
        // The summary is the rename alone.
        let out = p.output();
        assert_has!(&out, r"~ column posts\.\[-pid-\]\{\+post_id\+\}");
        for line in ["constraint", "index", "view"] {
            assert!(!out.contains(&format!("+ {line}")) && !out.contains(&format!("- {line}")), "{out}");
        }
        let all = written(&p, "rename_pid").join("-- next\n");
        assert!(all.contains("alter table posts rename column pid to post_id;"), "{all}");
        for rebuilt in ["drop ", "create ", "add constraint", "concurrently"] {
            assert!(!all.contains(rebuilt), "nothing is built again ({rebuilt}):\n{all}");
        }
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.check_every_way().await.0, 0);
        assert_eq!(p.rows("select count(*)::text as n from comments").await[0].text("n"), "1");
    }

    /// `v1`, migrated with rows, then `v2` written by `migrate new` with `rename`, deployed and checked.
    async fn renamed_to(p: &mut P, v1: &str, rows: &str, v2: &str, rename: (&str, &str)) {
        p.schema(v1);
        p.file("20261027100000_v1.sql", v1);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec(rows).await;
        p.output();
        p.schema(v2);
        let choices = reylo_migrate::Choices { rename: vec![(rename.0.into(), rename.1.into())], blocking: true, ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "rename", &choices, log).await).await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.check_every_way().await.0, 0);
    }

    pub async fn a_rename_carries_its_constraints_sequence_and_views(mut p: P) {
        let v1 = "create table t (id serial primary key, a int not null unique check (a > 0));
            create table u (id int primary key, a int references t (a));
            create view v as select a from t;
            create view w as select * from t;";
        p.schema(v1);
        p.file("20261027100000_v1.sql", v1);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into t (a) values (1), (2); insert into u values (1, 1)").await;
        p.output();
        // The constraints named after the column, its serial's sequence and the views' columns take
        // the new names, as a schema written anew names them.
        p.schema(
            "create table t (tid serial primary key, x int not null unique check (x > 0));
            create table u (id int primary key, a int references t (x));
            create view v as select x from t;
            create view w as select * from t;",
        );
        let choices = reylo_migrate::Choices { rename: vec![("t.a".into(), "x".into()), ("t.id".into(), "tid".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "rename", &choices, log).await).await, 0, "{}", p.peek());
        p.output();
        let all = written(&p, "rename").join("-- next\n");
        for kept in ["rename constraint t_a_key to t_x_key", "rename constraint t_a_check to t_x_check", "alter sequence t_id_seq rename to t_tid_seq", "alter view v rename column a to x", "alter view w rename column a to x", "alter view w rename column id to tid"] {
            assert!(all.contains(kept), "{kept}:\n{all}");
        }
        for rebuilt in ["drop ", "create ", "add constraint", "concurrently"] {
            assert!(!all.contains(rebuilt), "nothing is built again ({rebuilt}):\n{all}");
        }
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.check_every_way().await.0, 0);
        // The sequence goes on from where it was.
        p.exec("insert into t (x) values (3)").await;
        assert_eq!(p.rows("select max(tid)::text as n from t").await[0].text("n"), "3");
    }

    pub async fn a_rename_that_cannot_run_first_is_planned_as_before(mut p: P) {
        // The not null's new name is a check's the plan drops: the rename runs where the plan has it.
        let v1 = "create table t (id int primary key, a int not null, c int, constraint t_x_not_null check (c > 0));
            create table s (id int primary key, b int, d int, constraint s_b_pos check (b > 0));";
        p.schema(v1);
        p.file("20261027100000_v1.sql", v1);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into t values (1, 1, 1); insert into s values (1, 1, 1)").await;
        p.output();
        // And one column's type changes as it is renamed, beside a rename that keeps its values.
        p.schema(
            "create table t (id int primary key, x int not null, c int);
            create table s (id int primary key, b2 int, d2 bigint, constraint s_b_pos check (b2 > 0));",
        );
        let choices = reylo_migrate::Choices { rename: vec![("t.a".into(), "x".into()), ("s.b".into(), "b2".into()), ("s.d".into(), "d2".into())], blocking: true, ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "mixed", &choices, log).await).await, 0, "{}", p.peek());
        p.output();
        let all = written(&p, "mixed").join("-- next\n");
        assert!(!all.contains("drop constraint s_b_pos"), "the check on the column that keeps its values stays:\n{all}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.check_every_way().await.0, 0);
    }

    pub async fn a_renamed_key_column_whose_collation_changes_keeps_its_foreign_key(mut p: P) {
        // The foreign key goes before both columns' collations change, and comes back after.
        let collation = "create collation ci (provider = icu, locale = 'und-u-ks-level2', deterministic = false);";
        renamed_to(
            &mut p,
            &format!("{collation} create table parents (old_id text primary key); create table children (id int primary key, parent_id text references parents(old_id));"),
            "insert into parents values ('a'); insert into children values (1, 'a')",
            &format!("{collation} create table parents (id text collate ci primary key); create table children (id int primary key, parent_id text collate ci references parents(id));"),
            ("parents.old_id", "id"),
        )
        .await;
    }

    pub async fn a_renamed_key_column_whose_stored_expression_changes_keeps_its_foreign_key(mut p: P) {
        renamed_to(
            &mut p,
            "create table parents (raw int not null, old_code int generated always as (raw + 1) stored, constraint parents_code_key unique (old_code));
            create table children (raw int not null, parent_code int generated always as (raw + 1) stored, constraint children_parent_code_fkey foreign key (parent_code) references parents(old_code));",
            "insert into parents values (1); insert into children values (1)",
            "create table parents (raw int not null, code int generated always as (raw + 2) stored, constraint parents_code_key unique (code));
            create table children (raw int not null, parent_code int generated always as (raw + 2) stored, constraint children_parent_code_fkey foreign key (parent_code) references parents(code));",
            ("parents.old_code", "code"),
        )
        .await;
    }

    pub async fn a_foreign_key_added_back_under_another_name_holds_through_its_keys_swap(mut p: P) {
        let v1 = "create table parent (id int not null, constraint parent_id_key unique (id));
            create table child (id int primary key, parent_id int, constraint child_fk_old foreign key (parent_id) references parent (id));";
        p.schema(v1);
        p.file("20261027100000_v1.sql", v1);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into parent values (1); insert into child values (1, 1)").await;
        p.output();
        p.schema(
            "create table parent (id int not null, constraint parent_pkey primary key (id));
            create table child (id int primary key, parent_id int, constraint child_fk_new foreign key (parent_id) references parent (id));",
        );
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "pkey", &Default::default(), log).await).await, 0, "{}", p.peek());
        p.output();
        // Deployed a file at a time, a child row with no parent is refused after each.
        let made: Vec<(String, String)> = p.files().into_iter().filter(|f| f.contains("_pkey")).map(|f| (f.clone(), std::fs::read_to_string(p.dir.join("migrations").join(&f)).unwrap())).collect();
        for (f, _) in &made {
            std::fs::remove_file(p.dir.join("migrations").join(f)).unwrap();
        }
        for (k, (f, sql)) in made.iter().enumerate() {
            p.file(f, sql);
            assert_eq!(p.migrate().await, 0, "{}", p.peek());
            p.output();
            assert!(p.try_rows(&format!("insert into child values ({}, 999)", 100 + k)).await.is_err(), "a child with no parent is refused after {f}");
        }
        assert_eq!(p.check_every_way().await.0, 0);
    }

    pub async fn a_name_given_to_a_new_index_keeps_the_old_one_until_the_last_build(mut p: P) {
        let v1 = "create table accounts (id int primary key, email text, handle text); create unique index accounts_a on accounts (email);";
        p.schema(v1);
        p.file("20261027100000_v1.sql", v1);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into accounts values (1, 'e1', 'h1')").await;
        p.output();
        // The name moves to a new definition, and the old one to another name.
        p.schema("create table accounts (id int primary key, email text, handle text); create unique index accounts_a on accounts (handle); create unique index accounts_z on accounts (email);");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "move", &Default::default(), log).await).await, 0, "{}", p.peek());
        let made: Vec<String> = p.files().into_iter().filter(|f| f.contains("_move")).collect();
        let last = (made.last().unwrap().clone(), std::fs::read_to_string(p.dir.join("migrations").join(made.last().unwrap())).unwrap());
        assert!(last.1.contains("drop index accounts_a;"), "the old one goes last: {last:?}");
        std::fs::remove_file(p.dir.join("migrations").join(&last.0)).unwrap();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert!(p.try_rows("insert into accounts values (2, 'e1', 'h2')").await.is_err(), "email stays unique until the swap");
        p.file(&last.0, &last.1);
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.check_every_way().await.0, 0);
    }

    pub async fn a_primary_key_added_over_an_index_says_it_checks_for_nulls(mut p: P) {
        p.file(
            "20261027100000_t.sql",
            "create table t (id int, n int); create table s (id int, constraint s_id_present check (id is not null)); create table r (id int);
            create table q (id int); create table other (id int); create index other_idx on other (id); create table z (id int, constraint z_id_present check (id is not null));
            create schema a; create schema b; create table a.u (id int); create unique index same on a.u (id); create table b.t (id int); create unique index same on b.t (id);
            create table p (id int); create unique index p_idx on p (id);
            create table a.q (id int); create unique index q_idx on a.q (id); create table b.v (id int); create index v_idx on b.v (id);",
        );
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into t values (1, 1); insert into s values (1); insert into r values (1); insert into q values (1); insert into z values (1); insert into b.t values (1); insert into p values (1); insert into a.q values (1)").await;
        // Written by hand: the key's column isn't yet not null, so adding the key reads every row,
        // unless a validated check proves it; the index included column isn't read, and an index
        // renamed before the key takes it is the same index.
        p.file("20261027110000_index.sql", "create unique index concurrently t_new on t (id) include (n);");
        p.file("20261027120000_key.sql", "alter table t add constraint t_pkey primary key using index t_new;");
        p.file("20261027130000_s_index.sql", "create unique index concurrently s_new on s (id);");
        p.file("20261027140000_s_key.sql", "alter table s add constraint s_pkey primary key using index s_new;");
        p.file("20261027150000_r_index.sql", "create unique index concurrently r_old on r (id);");
        p.file("20261027160000_r_key.sql", "alter index r_old rename to r_new;\nalter table r add constraint r_pkey primary key using index r_new;");
        // A name another index takes after the key's index left it, and a proof dropped first.
        p.file("20261027170000_q_index.sql", "create unique index concurrently q_idx on q (id);");
        p.file("20261027180000_q_key.sql", "alter index q_idx rename to q_tmp;\nalter index other_idx rename to q_idx;\nalter table q add constraint q_pkey primary key using index q_tmp;");
        p.file("20261027190000_z_index.sql", "create unique index concurrently z_new on z (id);");
        p.file("20261027200000_z_key.sql", "alter table z drop constraint z_id_present;\nalter table z add constraint z_pkey primary key using index z_new;");
        // An index of the same name in another schema, renamed, is another index.
        p.file("20261027210000_b_key.sql", "alter index a.same rename to same2;\nalter table b.t add constraint t_pk primary key using index same;");
        // And one renamed with its schema written, then named without it.
        p.file("20261027220000_p_key.sql", "alter index public.p_idx rename to p_tmp;\nalter table p add constraint p_pk primary key using index p_tmp;");
        // And one renamed by its schema into a name two schemas took earlier in the file.
        p.file("20261027230000_q_key.sql", "alter index b.v_idx rename to idx;\nalter index a.q_idx rename to idx;\nalter index a.idx rename to q_tmp;\nalter table a.q add constraint q_pk primary key using index q_tmp;");
        p.schema(
            "create table t (id int not null, n int, constraint t_pkey primary key (id) include (n));
            create table s (id int not null, constraint s_id_present check (id is not null), constraint s_pkey primary key (id));
            create table r (id int not null, constraint r_pkey primary key (id));
            create table q (id int not null, constraint q_pkey primary key (id));
            create table other (id int); create index q_idx on other (id);
            create table z (id int not null, constraint z_pkey primary key (id));
            create schema a; create schema b; create table a.u (id int); create unique index same2 on a.u (id); create table b.t (id int not null, constraint t_pk primary key (id));
            create table p (id int not null, constraint p_pk primary key (id));
            create table a.q (id int not null, constraint q_pk primary key (id)); create table b.v (id int); create index idx on b.v (id);",
        );
        p.output();
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        let out = p.output();
        assert_has!(&out, r"_key\.sql\n      ! It checks every row of t for a null in id while it blocks reads and writes to it");
        assert_has!(&out, r"_r_key\.sql\n      ! It checks every row of r for a null in id while it blocks reads and writes to it");
        assert_has!(&out, r"_q_key\.sql\n      ! It checks every row of q for a null in id while it blocks reads and writes to it");
        assert_has!(&out, r"_z_key\.sql\n      ! It checks every row of z for a null in id while it blocks reads and writes to it");
        assert_has!(&out, r"_b_key\.sql\n      ! It checks every row of b\.t for a null in id while it blocks reads and writes to it");
        assert_has!(&out, r"_p_key\.sql\n      ! It checks every row of p for a null in id while it blocks reads and writes to it");
        assert_has!(&out, r"_q_key\.sql\n      ! It checks every row of a\.q for a null in id while it blocks reads and writes to it");
        assert!(!out.contains("a null in n ") && !out.contains("every row of s for a null"), "{out}");
    }

    pub async fn new_refuses_a_table_rewrite_until_blocking(mut p: P) {
        p.schema("create table users (id int primary key, n int);");
        p.file("20261027100000_users.sql", "create table users (id int primary key, n int);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into users values (1, 2)").await;
        p.output();
        p.schema("create table users (id int primary key, n bigint);");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "widen", &Default::default(), log).await).await, 1);
        assert_has!(p.output(), r"✗ This migration rewrites users, copying every row while it blocks reads and writes to it\n[^✗]*Where the table is small enough to wait for:\n    reylo migrate new widen --blocking");
        assert!(p.files().iter().all(|f| !f.contains("_widen")), "nothing is written");
        let blocking = reylo_migrate::Choices { blocking: true, ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "widen", &blocking, log).await).await, 0, "{}", p.peek());
        assert_has!(p.output(), r"! It rewrites users");
        // The dry run says so of the pending migration, and passes.
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"_widen\.sql\n      ! It rewrites users, copying every row");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
    }

    pub async fn new_refuses_a_check_on_a_partitioned_table_until_blocking(mut p: P) {
        let v1 = "create table users (id int, n int) partition by range (id); create table users0 partition of users for values from (0) to (1000000);";
        p.schema(v1);
        p.file("20261027100000_users.sql", v1);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into users values (1, 2)").await;
        p.output();
        // A check on a partitioned table reads every partition's rows, and has no shape that doesn't.
        p.schema("create table users (id int, n int, constraint pos check (n > 0)) partition by range (id); create table users0 partition of users for values from (0) to (1000000);");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "pos", &Default::default(), log).await).await, 1);
        assert_has!(p.output(), r"✗ This migration checks every row of users while it blocks reads and writes to it");
        let blocking = reylo_migrate::Choices { blocking: true, ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "pos", &blocking, log).await).await, 0, "{}", p.peek());
    }

    pub async fn new_refuses_an_index_built_again_until_blocking(mut p: P) {
        let v1 = r#"create table users (id int primary key, name text collate "C"); create index users_name on users (name);"#;
        p.schema(v1);
        p.file("20261027100000_users.sql", v1);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into users values (1, 'a')").await;
        p.output();
        // Another collation keeps the table's storage but builds its index again.
        p.schema(r#"create table users (id int primary key, name text collate "POSIX"); create index users_name on users (name);"#);
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "posix", &Default::default(), log).await).await, 1);
        assert_has!(p.output(), r"✗ This migration builds users_name again, reading every row of users while it blocks reads and writes to it");
    }

    pub async fn a_concurrent_reindex_is_said_to_block_nothing(mut p: P) {
        p.file("20261027100000_users.sql", "create table users (id int primary key, n int); create index users_n on users (n);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.file("20261027110000_reindex.sql", "reindex index concurrently users_n;");
        p.file("20261027120000_reindex_table.sql", "reindex table concurrently users;");
        p.output();
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        let out = p.output();
        assert!(out.contains("2 pending") && !out.contains(" It "), "{out}");
        // One that turns the option off builds the index again under a lock that blocks writes.
        p.file("20261027130000_reindex_off.sql", "reindex (concurrently false) index users_n;");
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"_reindex_off\.sql\n      ! It builds users_n again");
    }

    pub async fn deploy_waits_out_a_lock_another_session_holds(mut p: P) {
        p.file("20261027100000_users.sql", "create table users (id int primary key);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.file("20261027110000_bio.sql", "alter table users add column bio text;");
        // Another session reads the table for two seconds: the migration's statement gives up
        // waiting after half a second each time, so the app's queries never queue behind it, and
        // the file runs again until the reader is done.
        let mut reader = Db::connect(&p.url, &p.dir).await.unwrap();
        reader.exec("begin; lock table users in access share mode").await.unwrap();
        let release = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            reader.exec("commit").await.unwrap();
            reader.close().await;
        });
        let at = std::time::Instant::now();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert!(at.elapsed() >= std::time::Duration::from_secs(2), "the migration waited for the reader");
        release.await.unwrap();
        assert_eq!(p.rows("select count(*) as n from information_schema.columns where table_name = 'users' and column_name = 'bio'").await[0].text("n"), "1");
    }

    pub async fn deploys_at_once_wait_out_a_concurrent_index_build(mut p: P) {
        p.file("20261027100000_users.sql", "create table users (id int primary key, created_at timestamptz not null default now());");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into users select g, now() - g * interval '1 minute' from generate_series(1, 100000) g").await;
        p.file("20261027110000_users_created.sql", "create index concurrently users_created on users (created_at);");
        p.relock();
        let opts = p.opts();
        // Three instances of an app start while another run holds the lock, each deploying. The one
        // that takes the lock next builds the index concurrently, which waits out every snapshot older
        // than its own: a run still waiting for the lock must hold none, or the two deadlock.
        let (url, dir) = (&p.url, &p.dir);
        let runs: Vec<(i32, String)> = std::thread::scope(|s| {
            let (locked, held) = std::sync::mpsc::channel();
            s.spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                rt.block_on(async {
                    let mut other = Db::connect(url, dir).await.unwrap();
                    other.exec("select pg_advisory_lock(hashtextextended('reylo_migrate', 0))").await.unwrap();
                    locked.send(()).unwrap();
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    other.close().await;
                });
            });
            held.recv().unwrap();
            let started: Vec<_> = (0..3)
                .map(|_| {
                    let opts = &opts;
                    s.spawn(move || {
                        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                        let mut lines = Vec::new();
                        let code = rt.block_on(reylo_migrate::deploy(opts, &mut |l: &str| lines.push(l.to_string()))).unwrap_or_else(|e| {
                            lines.push(e.to_string());
                            1
                        });
                        (code, lines.join("\n"))
                    })
                })
                .collect();
            started.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (code, out) in &runs {
            assert_eq!(*code, 0, "{out}");
        }
        assert_eq!(runs.iter().map(|(_, out)| out.matches(" applied ·").count()).sum::<usize>(), 1, "{runs:?}");
        assert_eq!(runs.iter().filter(|(_, out)| out.contains("Up to date")).count(), 2, "{runs:?}");
        assert_eq!(p.rows("select indisvalid::text as v from pg_index where indexrelid = 'users_created'::regclass").await[0].text("v"), "true");
    }

    pub async fn a_deploy_waits_for_the_run_lock_no_longer_than_its_sessions_timeouts(mut p: P) {
        p.file("20261027100000_users.sql", "create table users (id int primary key);");
        p.relock();
        let opts = p.opts();
        let mut other = Db::connect(&p.url, &p.dir).await.unwrap();
        other.exec("select pg_advisory_lock(hashtextextended('reylo_migrate', 0))").await.unwrap();
        // Another run holds the lock for longer than either timeout: the deploy gives up at the
        // timeout, as a statement blocked on the lock would, running nothing.
        for setting in ["lock_timeout", "statement_timeout"] {
            p.exec(&format!("alter database {} set {setting} = '300ms'", p.name)).await;
            let at = std::time::Instant::now();
            let waited = tokio::time::timeout(std::time::Duration::from_secs(10), reylo_migrate::deploy(&opts, &mut |_: &str| {})).await.expect("the deploy gave up at its timeout");
            let e = waited.expect_err("the lock was held");
            assert!(e.to_string().contains(&format!("{setting} of 300ms")), "{e}");
            assert!(at.elapsed() < std::time::Duration::from_secs(2), "{setting}: {:?}", at.elapsed());
            p.exec(&format!("alter database {} reset {setting}", p.name)).await;
        }
        assert!(p.history().await.is_empty());
        other.close().await;
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
    }

    pub async fn a_narrowing_type_change_fails_rather_than_cuts(mut p: P) {
        p.schema("create table t (code text);");
        p.file("20261027100000_t.sql", "create table t (code text);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into t values ('abcdef')").await;
        // create writes the change as the server's assignment conversion, which refuses a value
        // the new type cannot hold; an explicit cast to varchar(3) would cut it to 'abc'.
        p.schema("create table t (code varchar(3));");
        let blocking = reylo_migrate::Choices { blocking: true, ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "narrow", &blocking, log).await).await, 0, "{}", p.peek());
        let made = p.files().into_iter().find(|f| f.ends_with("_narrow.sql")).unwrap();
        let sql = std::fs::read_to_string(p.dir.join("migrations").join(&made)).unwrap();
        assert!(!sql.contains("using"), "{sql}");
        p.output();
        assert_eq!(p.migrate().await, 1, "the value does not fit");
        assert_has!(p.output(), "too long");
        assert_eq!(p.rows("select code from t").await[0].text("code"), "abcdef");
    }

    pub async fn a_number_to_a_short_varchar_fails_rather_than_cuts(mut p: P) {
        p.schema("create table t (code int);");
        p.file("20261027100000_t.sql", "create table t (code int);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into t values (12345)").await;
        // An integer reaches a string type by I/O conversion, which no pg_cast row lists and which
        // is an assignment cast: no USING, so the value that doesn't fit is refused, not cut.
        p.schema("create table t (code varchar(3));");
        let blocking = reylo_migrate::Choices { blocking: true, ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "narrow", &blocking, log).await).await, 0, "{}", p.peek());
        let made = p.files().into_iter().find(|f| f.ends_with("_narrow.sql")).unwrap();
        let sql = std::fs::read_to_string(p.dir.join("migrations").join(&made)).unwrap();
        assert!(!sql.contains("using"), "{sql}");
        p.output();
        assert_eq!(p.migrate().await, 1, "the value does not fit");
        assert_has!(p.output(), "too long");
        assert_eq!(p.rows("select code from t").await[0].text("code"), "12345");
    }

    pub async fn a_replaced_column_is_a_stated_drop(mut p: P) {
        p.schema("create table t (a int, total int generated always as (a * 2) stored);");
        p.file("20261027100000_t.sql", "create table t (a int, total int generated always as (a * 2) stored);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into t (a) values (2)").await;
        // Generated to plain is made in place, and the values stay.
        p.schema("create table t (a int, total int);");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "plain", &Default::default(), log).await).await, 0, "{}", p.peek());
        let made = p.files().into_iter().find(|f| f.ends_with("_plain.sql")).unwrap();
        let sql = std::fs::read_to_string(p.dir.join("migrations").join(&made)).unwrap();
        assert!(sql.contains("drop expression") && !sql.contains("drop column"), "{sql}");
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select total from t").await[0].text("total"), "4");
        // Plain to generated replaces the column, and its values with it: --drop must say so.
        p.schema("create table t (a int, total int generated always as (a * 3) stored);");
        p.output();
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "generated", &Default::default(), log).await).await, 1);
        assert_has!(p.output(), r"This migration drops the column t\.total and its data");
        let choices = reylo_migrate::Choices { drop: vec!["t.total".into()], rename: vec![], blocking: true };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "generated", &choices, log).await).await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select total from t").await[0].text("total"), "6");
    }

    pub async fn a_rename_to_a_name_that_needs_quotes(mut p: P) {
        p.schema("create table users (id int, n int);");
        p.file("20261027100000_users.sql", "create table users (id int, n int);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into users values (1, 42)").await;
        p.schema(r#"create table users (id int, "Total" bigint);"#);
        let choices = reylo_migrate::Choices { drop: vec![], rename: vec![("users.n".into(), "Total".into())], blocking: true };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "total", &choices, log).await).await, 0, "{}", p.peek());
        let made = p.files().into_iter().find(|f| f.ends_with("_total.sql")).unwrap();
        let sql = std::fs::read_to_string(p.dir.join("migrations").join(&made)).unwrap();
        assert!(sql.contains(r#"rename column n to "Total""#), "{sql}");
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.rows(r#"select "Total" as v from users"#).await[0].text("v"), "42");
    }

    pub async fn an_index_left_invalid_by_a_cut_off_build_is_dropped_and_built_again(mut p: P) {
        p.schema("create table t (x int); create unique index t_x on t (x);");
        p.file("20261027100000_t.sql", "create table t (x int);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into t values (1), (1)").await;
        // The run was cut off mid-build: its row marked started, and the index it left invalid.
        let build = "create unique index concurrently t_x on t (x);";
        p.file("20261027110000_unique.sql", build);
        p.exec(&format!("insert into __reylo_migrations (name, sql, applied_at) values ('20261027110000_unique.sql', '{build}', null)")).await;
        let mut db = Db::connect(&p.url, &p.dir).await.unwrap();
        assert!(db.script(build).await.is_err());
        db.close().await;
        assert_eq!(p.rows("select 1 from pg_index where not indisvalid").await.len(), 1);
        p.exec("delete from t where ctid = (select max(ctid) from t)").await;
        p.output();
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"! A cut-off run of 20261027110000_unique\.sql left public\.t_x invalid\n  Drop it and run the migration again:\n    reylo migrate deploy");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        let out = p.output();
        assert_has!(&out, r"Dropped public\.t_x, which 20261027110000_unique\.sql left invalid when it was cut off");
        assert_has!(&out, r"✓ 20261027110000_unique\.sql +applied");
        assert!(p.rows("select 1 from pg_index where not indisvalid").await.is_empty());
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_has!(p.output(), "Up to date");
    }

    pub async fn an_enum_label_a_partition_uses_is_committed_first(mut p: P) {
        let v1 = "create type region as enum ('eu', 'us'); create table t (id int, r region) partition by list (r); create table t_eu partition of t for values in ('eu');";
        let v2 = "create type region as enum ('eu', 'us', 'ap'); create table t (id int, r region) partition by list (r); create table t_eu partition of t for values in ('eu'); create table t_ap partition of t for values in ('ap');";
        p.schema(v1);
        p.file("20261027100000_regions.sql", v1);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into t values (1, 'eu')").await;
        // Postgres refuses a label added and used in one transaction, and the type can't be made
        // again under a partition key: the label is a migration of its own, before the partition.
        p.schema(v2);
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "ap", &Default::default(), log).await).await, 0, "{}", p.peek());
        let made: Vec<String> = p.files().into_iter().filter(|f| f.contains("_ap")).collect();
        assert_eq!(made.len(), 2, "{made:?}");
        let first = std::fs::read_to_string(p.dir.join("migrations").join(&made[0])).unwrap();
        let second = std::fs::read_to_string(p.dir.join("migrations").join(&made[1])).unwrap();
        assert!(first.contains("add value 'ap'") && !first.contains("partition of"), "{first}");
        assert!(second.contains("partition of t for values in ('ap')"), "{second}");
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into t values (2, 'ap')").await;
        p.output();
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    pub async fn an_enum_label_a_default_uses_is_committed_first(mut p: P) {
        let v1 = "create type visibility as enum ('private', 'public'); create table bookmarks (id int primary key, visibility visibility not null default 'private'); create view public_bookmarks as select id from bookmarks where visibility = 'public';";
        let v2 = "create type visibility as enum ('private', 'public', 'org'); create table bookmarks (id int primary key, visibility visibility not null default 'org'); create view public_bookmarks as select id from bookmarks where visibility = 'public';";
        p.schema(v1);
        p.file("20261027100000_bookmarks.sql", v1);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into bookmarks values (1, 'public')").await;
        // Postgres refuses a label added and used in one transaction: the label is a migration of
        // its own, before the default, which copies no table and needs no `--blocking`.
        p.schema(v2);
        p.output();
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "org_default", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_has!(p.output(), r"✓ Wrote 2 migrations, since Postgres can't use a new enum value in the migration that adds it:");
        let made: Vec<String> = p.files().into_iter().filter(|f| f.contains("_org_default")).collect();
        assert_eq!(made.len(), 2, "{made:?}");
        let first = std::fs::read_to_string(p.dir.join("migrations").join(&made[0])).unwrap();
        let second = std::fs::read_to_string(p.dir.join("migrations").join(&made[1])).unwrap();
        assert_eq!(first.trim(), "alter type visibility add value 'org' after 'public';");
        assert_eq!(second.trim(), "alter table bookmarks alter column visibility set default 'org';");
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into bookmarks (id) values (2)").await;
        assert_eq!(p.rows("select 1 from bookmarks where visibility = 'org'").await.len(), 1);
    }

    pub async fn a_push_that_fails_past_an_enum_commit_keeps_what_it_drops(mut p: P) {
        let v1 = "create type mood as enum ('a'); create table items (i int, m mood default 'a'); create table old_notes (body text); create table x (c mood); create view v as select c from x;";
        p.file("20261027100000_items.sql", v1);
        p.schema(v1);
        assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
        p.exec("insert into items values (-1, 'a')").await;
        p.exec("insert into old_notes values ('kept')").await;
        p.output();
        // The new label commits first, alone: the check its row fails after it undoes the drop, and
        // the column's move off the type with the view over it.
        p.schema("create type mood as enum ('a', 'b'); create table items (i int check (i > 0), m mood default 'b'); create table x (c text); create view v as select c from x;");
        assert_eq!(p.db_push_with(&push(&["old_notes"], &[], false)).await, 1, "{}", p.peek());
        assert_eq!(p.rows("select body from old_notes").await.len(), 1);
        assert_eq!(p.rows("select 1 from pg_views where viewname = 'v'").await.len(), 1);
        assert_eq!(p.rows("select 1 from information_schema.columns where table_name = 'x' and udt_name = 'mood'").await.len(), 1);
        assert_eq!(p.rows("select 1 from pg_enum where enumlabel = 'b'").await.len(), 1);
    }

    pub async fn a_push_that_fails_past_an_enum_commit_undoes_its_renames(mut p: P) {
        p.file("20261027100000_data.sql", "create type mood as enum ('a'); create table items (i int, m mood default 'a'); create table t_data (id int, name text); create view data as select * from t_data; create view q as select * from data;");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into t_data values (1, 'a')").await;
        p.exec("insert into items values (-1, 'a')").await;
        p.output();
        // The new label commits first, alone: the rename, and the views it drops, run after it, and
        // the check its row fails undoes them.
        p.schema("create type mood as enum ('a', 'b'); create table items (i int check (i > 0), m mood default 'b'); create table data (id int, name text); create view q as select * from data;");
        assert_eq!(p.db_push_with(&push(&[], &[("t_data", "data")], false)).await, 1, "{}", p.peek());
        assert_eq!(p.rows("select name from t_data").await.len(), 1);
        assert_eq!(p.rows("select 1 from pg_views where viewname in ('data', 'q')").await.len(), 2);
        assert_eq!(p.rows("select 1 from pg_enum where enumlabel = 'b'").await.len(), 1);
    }

    pub async fn a_renamed_tables_sequence_keeps_counting(mut p: P) {
        p.schema("create table users (id serial primary key, email text unique, n integer generated always as identity);");
        p.file("20261027100000_users.sql", "create table users (id serial primary key, email text unique, n integer generated always as identity);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into users (email) values ('a'), ('b'), ('c')").await;
        // What Postgres named after the table is renamed with it: its sequence keeps its value.
        p.schema("create table accounts (id serial primary key, email text unique, n integer generated always as identity);");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "accounts", &Default::default(), log).await).await, 0, "{}", p.peek());
        let made = p.files().into_iter().find(|f| f.ends_with("_accounts.sql")).unwrap();
        let sql = std::fs::read_to_string(p.dir.join("migrations").join(&made)).unwrap();
        for part in ["alter table users rename to accounts", "rename constraint users_pkey to accounts_pkey", "rename constraint users_email_key to accounts_email_key", "alter sequence users_id_seq rename to accounts_id_seq", "alter sequence users_n_seq rename to accounts_n_seq"] {
            assert!(sql.contains(part), "{part} in:\n{sql}");
        }
        assert!(!sql.lines().any(|l| !l.starts_with("--") && (l.contains("create") || l.contains("drop"))), "{sql}");
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into accounts (email) values ('d')").await;
        assert_eq!(p.rows("select id from accounts where email = 'd'").await[0].text("id"), "4");
        // And its columns' not nulls, which Postgres 18 names after the table too.
        for r in p.rows("select conname from pg_constraint where conrelid = 'accounts'::regclass and contype = 'n'").await {
            assert!(r.text("conname").starts_with("accounts_"), "{} kept the old table's name", r.text("conname"));
        }
        p.output();
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    pub async fn a_table_renamed_as_it_gains_a_column_keeps_its_rows_and_index(mut p: P) {
        let schema = "create table employee (id int primary key, name text, title text); create index on employee (name);";
        p.schema(schema);
        p.file("20261027100000_employee.sql", schema);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into employee values (1, 'a', 'x')").await;
        // Most of its columns by name: taken for the same table, but with a column added, the person
        // says which it is.
        p.schema("create table staff (id int primary key, name text, title text, status text default 'active'); create index on staff (name);");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "staff", &Default::default(), log).await).await, 1, "{}", p.peek());
        assert!(p.output().contains("--rename employee=staff"));
        // Renamed, with its unnamed index, then the new column added.
        let choices = reylo_migrate::Choices { drop: vec![], rename: vec![("employee".into(), "staff".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "staff", &choices, log).await).await, 0, "{}", p.peek());
        let made = p.files().into_iter().find(|f| f.ends_with("_staff.sql")).unwrap();
        let sql = std::fs::read_to_string(p.dir.join("migrations").join(&made)).unwrap();
        for part in ["alter table employee rename to staff", "alter index employee_name_idx rename to staff_name_idx", "add column status"] {
            assert!(sql.contains(part), "{part} in:\n{sql}");
        }
        assert!(!sql.lines().any(|l| !l.starts_with("--") && l.contains("drop table")), "{sql}");
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select name from staff where id = 1").await[0].text("name"), "a");
        p.output();
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    pub async fn a_foreign_table_is_renamed_as_stated(mut p: P) {
        let server = "create foreign data wrapper madv_w; create server madv_s foreign data wrapper madv_w;";
        p.file("20261027100000_users.sql", &format!("{server} create foreign table users (id int, email text) server madv_s;"));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.schema(&format!("{server} create foreign table accounts (id int, email text) server madv_s;"));
        // Its rows are its server's: dropping it loses none here, and its rename is a foreign table's.
        let choices = reylo_migrate::Choices { drop: vec![], rename: vec![("users".into(), "accounts".into())], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "accounts", &choices, log).await).await, 0, "{}", p.peek());
        let made = p.files().into_iter().find(|f| f.ends_with("_accounts.sql")).unwrap();
        let sql = std::fs::read_to_string(p.dir.join("migrations").join(&made)).unwrap();
        assert!(sql.contains("alter foreign table users rename to accounts"), "{sql}");
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    pub async fn a_table_renamed_onto_a_views_name_drops_the_view_first(mut p: P) {
        p.file("20261027100000_data.sql", "create table t_data (id int, name text); create view data as select * from t_data; create view q as select * from data;");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into t_data values (1, 'a')").await;
        // The view goes, with the view on it, before the table takes its name; the plan makes q again.
        p.schema("create table data (id int, name text); create view q as select * from data;");
        assert_eq!(p.db_push().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select name from q").await.len(), 1);
    }

    pub async fn a_partitioned_table_is_renamed_with_its_partitions(mut p: P) {
        p.file("20261027100000_events.sql", "create table events (id int generated always as identity, at date, primary key (id, at)) partition by range (at); create table events_2024 partition of events for values from ('2024-01-01') to ('2025-01-01');");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into events (at) values ('2024-06-01')").await;
        p.schema("create table logs (id int generated always as identity, at date, primary key (id, at)) partition by range (at); create table logs_2024 partition of logs for values from ('2024-01-01') to ('2025-01-01');");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "logs", &Default::default(), log).await).await, 0, "{}", p.peek());
        let made = p.files().into_iter().find(|f| f.ends_with("_logs.sql")).unwrap();
        let sql = std::fs::read_to_string(p.dir.join("migrations").join(&made)).unwrap();
        assert!(sql.contains("alter table events rename to logs") && sql.contains("alter table events_2024 rename to logs_2024") && sql.contains("rename to logs_2024_pkey"), "{sql}");
        // The identity's sequence is the parent's alone, renamed with it.
        assert!(sql.contains("alter sequence events_id_seq rename to logs_id_seq") && !sql.contains("events_2024_id_seq"), "{sql}");
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select id from logs_2024").await.len(), 1);
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    pub async fn a_run_that_did_something_else_than_its_replay_is_found_by_check(mut p: P) {
        p.schema("create table t (id int);");
        p.file("20261027100000_t.sql", "create table t (id int);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into t values (1)").await;
        // DDL that depends on the rows: an index here, none in the empty replay.
        p.file("20261027110000_idx.sql", "do $$ begin if exists (select 1 from t) then create index t_id on t (id); end if; end $$;");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        // Its result is what deploy compares with next; check holds it against the replay.
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"^✓ Up to date$");
        assert_eq!(p.dry_run().await, 1);
        assert_has!(p.output(), r"This database was changed outside its migrations[\s\S]*t_id");
    }

    pub async fn more_type_changes_that_lose_values_are_named_in_drop(mut p: P) {
        p.file("20261027100000_t.sql", "create table t (f double precision, s text, j json, n numeric(10,0), i interval);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.schema("create table t (f numeric(10,2), s integer, j jsonb, n bigint, i interval day);");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "narrow", &Default::default(), log).await).await, 1);
        let out = p.output();
        for line in [r"t\.f changes from double precision to numeric\(10,2\), which rounds to 2 decimal places", r"t\.s changes from text to integer, which stores text that reads as a number", r"t\.j changes from json to jsonb, which keeps the JSON's values", r"t\.i changes from interval to interval day, which keeps only the day of each interval"] {
            assert_has!(&out, line);
        }
        assert!(!out.contains("t.n changes"), "numeric(10,0) holds whole numbers: {out}");
    }

    pub async fn partitions_made_at_run_time_are_data(mut p: P) {
        let schema = "create table events (id int, at date) partition by range (at); create table events_2024 partition of events for values from ('2024-01-01') to ('2025-01-01'); create table metrics (id int, v int); create table tagged (tag text);";
        p.schema(schema);
        p.file("20261027100000_events.sql", schema);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        // pg_partman makes next year's partition, and TimescaleDB-like children inherit a table.
        p.exec("create table events_2025 partition of events for values from ('2025-01-01') to ('2026-01-01'); insert into events values (1, '2025-03-01'); create table metrics_c1 () inherits (metrics); insert into metrics_c1 values (1, 2); create table metrics_c2 () inherits (metrics, tagged); insert into metrics_c2 values (1, 2, 'x')").await;
        p.output();
        // The fingerprint leaves children out, so nothing has changed for deploy, or for check.
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"^✓ Up to date$");
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        p.output();
        // Development keeps them too, with their rows.
        assert_eq!(p.dev_then_push().await, 0, "{}", p.peek());
        let out = p.output();
        assert!(!out.contains("events_2025") && !out.contains("metrics_c1") && !out.contains("metrics_c2"), "{out}");
        assert_eq!(p.rows("select * from events_2025").await.len(), 1);
        assert_eq!(p.rows("select * from metrics_c1").await.len(), 1);
        assert_eq!(p.rows("select * from metrics_c2").await.len(), 1);
        // A partition a migration made, dropped at run time (a retention job's), is data too.
        p.exec("drop table events_2024").await;
        p.output();
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
    }

    pub async fn a_schemas_table_is_never_read_as_a_renamed_tables_column(mut p: P) {
        p.file("20261027100000_t.sql", "create schema billing; create table billing.invoices (id int); create table billing (id int, total int);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.schema("create schema billing; create table ledger (id int, total int);");
        let choices = reylo_migrate::Choices { drop: vec!["billing.invoices".into()], rename: vec![], ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "ledger", &choices, log).await).await, 0, "{}", p.peek());
        let made = p.files().into_iter().find(|f| f.ends_with("_ledger.sql")).unwrap();
        let sql = std::fs::read_to_string(p.dir.join("migrations").join(&made)).unwrap();
        assert!(sql.contains("alter table billing rename to ledger") && sql.contains("drop table billing.invoices"), "{sql}");
    }

    pub async fn the_fingerprint_leaves_out_only_what_is_a_childs(mut p: P) {
        // A schema named as a table, and a view over a partition: neither line is the child's.
        let schema = "create schema users; create table ev (id int, at date, orders int) partition by range (at); create table ev_2024 partition of ev for values from ('2024-01-01') to ('2025-01-01'); create table users.orders (id int, at date, orders int); create table users (orders text); create view v as select id from ev_2024 where id > 0;";
        p.schema(schema);
        p.file("20261027100000_t.sql", schema);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        p.exec("create or replace view v as select id from ev_2024 where id > 100").await;
        assert_eq!(p.migrate().await, 1, "{}", p.peek());
        assert_has!(p.output(), "was changed outside its migrations");
        p.exec("create or replace view v as select id from ev_2024 where id > 0").await;
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        // A child's own lines are out, its indexes and triggers too.
        p.exec("create table ev_2025 partition of ev for values from ('2025-01-01') to ('2026-01-01'); create index ev_2025_id on ev_2025 (id)").await;
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"^✓ Up to date$");
    }

    pub async fn a_comment_above_a_cut_off_build_is_read_past(mut p: P) {
        p.schema("create table t (v int); create unique index t_v on t (v);");
        p.file("20261027100000_t.sql", "create table t (v int);");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        let lone = "-- speed up lookups by v\ncreate unique index concurrently t_v on t (v);";
        p.file("20261027110000_v.sql", lone);
        // The build cut off: a row marked started, and the index left invalid (two equal rows).
        p.exec("insert into t values (1), (1)").await;
        let _ = p.try_rows("create unique index concurrently t_v on t (v)").await;
        p.exec("delete from t").await;
        let mut db = Db::connect(&p.url, &p.dir).await.unwrap();
        db.exec_with(&format!("insert into __reylo_migrations (name, sql, applied_at) values ({}, {}, null)", db.param(1), db.param(2)), &["20261027110000_v.sql", lone]).await.unwrap();
        db.close().await;
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"Dropped public\.t_v, which 20261027110000_v\.sql left invalid[\s\S]*✓ 20261027110000_v\.sql +applied");
    }

    pub async fn table_guesses_name_their_schema(mut p: P) {
        let tables = |t: &str, extra: &str| format!("create table a.{t} (id int, x int, y int, z int{extra}); create table b.{t} (id int, x int, y int, z int{extra});");
        p.file("20261027100000_t.sql", &format!("create schema a; create schema b; {}", tables("orders", ", old int")));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        p.schema(&format!("create schema a; create schema b; {}", tables("invoices", ", new_one text")));
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "names", &Default::default(), log).await).await, 1);
        let out = p.output();
        assert_has!(&out, r"    --rename a\.orders=a\.invoices --drop a\.invoices\.old   or   --drop a\.orders\n    --rename b\.orders=b\.invoices --drop b\.invoices\.old   or   --drop b\.orders\n");
        let keeps = picked(&out, |_| true);
        let choices = choices_of(&keeps);
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "names", &choices, log).await).await, 0, "{keeps}\n{}", p.peek());
    }

    pub async fn a_childs_column_is_renamed_with_its_parents(mut p: P) {
        let before = "create table ev (id int, at date, val varchar(10)) partition by range (at); create table ev_2024 partition of ev for values from ('2024-01-01') to ('2025-01-01'); create table par (id int, val varchar(10)); create table kid () inherits (par);";
        p.file("20261027100000_t.sql", before);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into ev values (1, '2024-03-01', 'x'); insert into kid values (1, 'y')").await;
        p.output();
        p.schema(&before.replace("val varchar(10)", "amount varchar(20)"));
        // Each parent's rename is listed, once; its children go with it.
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "amount", &Default::default(), log).await).await, 1);
        let out = p.output();
        assert_has!(&out, r"can't tell renames from new columns");
        assert!(!out.contains("ev_2024.val") && !out.contains("kid."), "{out}");
        let keeps = picked(&out, |_| true);
        let choices = choices_of(&keeps);
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "amount", &choices, log).await).await, 0, "{keeps}\n{}", p.peek());
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select amount from ev_2024").await[0].text("amount"), "x");
        assert_eq!(p.rows("select amount from kid").await[0].text("amount"), "y");
        p.output();
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    pub async fn row_counts_are_the_dropped_tables_own(mut p: P) {
        p.schema("create schema a; create table a.orders (id int primary key);");
        p.file("20261027100000_a.sql", "create schema a; create table a.orders (id int primary key);");
        p.file("20261027110000_o.sql", "create table orders (id int primary key);");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("insert into a.orders values (1), (2), (3)").await;
        std::fs::remove_file(p.dir.join("migrations").join("20261027110000_o.sql")).unwrap();
        p.output();
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"(?m)! Dropped orders$");
        assert_eq!(p.rows("select * from a.orders").await.len(), 3);
    }

    pub async fn a_partition_added_at_run_time_is_data(mut p: P) {
        let table = "create table sales (id int not null, y int not null, primary key (id, y)) partition by range (y) (partition p2025 values less than (2026), partition p2026 values less than (2027));";
        p.schema(table);
        p.file("20261027100000_sales.sql", table);
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        // A job adds next year's partition: data, as a Postgres partition made at run time is.
        p.exec("alter table sales add partition (partition p2027 values less than (2028))").await;
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_has!(p.output(), "Up to date");
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        p.output();
        // A declared one dropped at run time, as a retention job drops the oldest, is data too.
        p.exec("alter table sales drop partition p2026").await;
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
    }

    pub async fn a_failure_that_leaves_part_of_itself_is_seen_by_the_next_deploy(mut p: P) {
        p.file("20261027100000_t.sql", "create table t (id int primary key);");
        p.file("20261027110000_proc.sql", "create procedure p() begin create table t2 (id int); select * from nope; end;");
        p.file("20261027120000_call.sql", "call p();");
        assert_eq!(p.migrate().await, 1);
        let out = p.output();
        assert_has!(&out, r"✓ 20261027110000_proc\.sql +applied[\s\S]*✗ 20261027120000_call\.sql  failed\n");
        assert!(!out.contains("nothing of it was applied"), "{out}");
        assert_eq!(p.rows("select * from t2").await.len(), 0);
        // What it left isn't what the last migration that ran left.
        assert_eq!(p.migrate().await, 1);
        assert_has!(p.output(), "was changed outside its migrations");
    }

    pub async fn a_table_comment_is_no_clause_of_its_table(mut p: P) {
        p.file("20261027100000_t.sql", "create table users (id int); create table a (id int) comment 'a partition of users by region';");
        // MySQL commits each statement: one to a file.
        std::fs::remove_file(p.dir.join("migrations").join("20261027100000_t.sql")).unwrap();
        p.file("20261027100000_t.sql", "create table users (id int);");
        p.file("20261027100001_a.sql", "create table a (id int) comment 'a partition of users by region';");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.output();
        p.exec("alter table a add column note int").await;
        assert_eq!(p.migrate().await, 1, "{}", p.peek());
        assert_has!(p.output(), "was changed outside its migrations");
    }

    pub async fn a_reserved_word_stays_quoted(mut p: P) {
        p.schema("create table `order` (id int primary key, `group` varchar(20));");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "order", &Default::default(), log).await).await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select * from `order`").await.len(), 0);
    }

    pub async fn a_table_renamed_keeps_its_rows_key_and_counter(mut p: P) {
        let tables = |name: &str| format!("create table profiles (id integer primary key autoincrement not null); create table `{name}` (id integer primary key autoincrement not null, profile_id integer, foreign key (id) references profiles (id));");
        p.schema(&tables("table"));
        p.file("20261027100000_t.sql", &tables("table"));
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        p.exec("insert into profiles default values; insert into profiles default values; insert into `table` (id, profile_id) values (1, 1), (2, 2)").await;
        // The same table under another name: renamed, its rows, key and counter kept.
        p.schema(&tables("table1"));
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "table1", &Default::default(), log).await).await, 0, "{}", p.peek());
        let made = p.files().into_iter().find(|f| f.ends_with("_table1.sql")).unwrap();
        let sql = std::fs::read_to_string(p.dir.join("migrations").join(&made)).unwrap();
        assert!(sql.contains("rename to") && !sql.lines().any(|l| !l.starts_with("--") && l.contains("drop table")), "{sql}");
        p.output();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select count(*) as n from table1").await[0].text("n"), "2");
        p.output();
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    pub async fn a_rebuild_keeps_every_row_where_foreign_keys_are_on(mut p: P) {
        // A parent, and tables that reference it every way: cascade, set null, none, at a second
        // level, through a key that is a unique index of its own, one the plan keeps and one it
        // renames, each naming its column's collation, and itself; each with what a rebuild takes
        // down and makes again.
        let tables = |check: &str, key: &str| {
            format!(
                "create table parents (id integer primary key, boss integer references parents (id) on delete cascade, name text{check});
create table kids (id integer primary key autoincrement, parent_id integer references parents (id) on delete cascade, note text);
create index kids_parent on kids (parent_id);
create table pets (id integer primary key, parent_id integer references parents (id) on delete set null);
create table chores (id integer primary key, kid_id integer references kids (id));
create table toys (kid_id integer references kids (id) on delete cascade, name text);
create table a_links (parent_id integer references parents (id), kid_id integer references kids (id));
create table accounts (id integer primary key, parent_id integer references parents (id), email text not null collate nocase);
create unique index {key} on accounts (email collate nocase);
create table logins (id integer primary key, email text collate nocase references accounts (email) on delete cascade);
create unique index logins_email on logins (email collate nocase);
create table sessions (id integer primary key, email text references logins (email));
create view kid_names as select kids.note, parents.name from kids join parents on parents.id = kids.parent_id;
create trigger kids_note after insert on kids begin update parents set name = name where id = new.parent_id; end;"
            )
        };
        p.schema(&tables("", "accounts_email"));
        p.file("20261027100000_t.sql", &tables("", "accounts_email"));
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("insert into parents values (1, null, 'a'), (2, 1, 'b'); insert into kids values (7, 1, 'x'), (8, 2, 'y'), (9, 2, 'z'); insert into pets values (1, 1), (2, 2); insert into chores values (1, 7), (2, 8); insert into toys values (9, 'ball'); insert into a_links values (1, 7); insert into accounts values (1, 1, 'a@example.com'); insert into logins values (1, 'a@example.com'); insert into sessions values (1, 'a@example.com')").await;
        p.output();
        // A check forces the parent's rebuild, and one that names its table the rebuild that sets
        // every row aside, since such a table can be made under no other name. Each time the key
        // of accounts takes another name, so the plan drops it before the rebuild and makes it after.
        for (check, name, next, key) in [(", check (length(name) > 0)", "check_name", "10", "accounts_email_2"), (", constraint named check (\"parents\".\"name\" <> '')", "check_named", "11", "accounts_email_3")] {
            p.schema(&tables(check, key));
            assert_eq!(p.run(async |o, log| reylo_migrate::create(o, name, &Default::default(), log).await).await, 0, "{}", p.peek());
            p.output();
            let made = p.files().into_iter().find(|f| f.ends_with(&format!("_{name}.sql"))).unwrap();
            let sql = std::fs::read_to_string(p.dir.join("migrations").join(&made)).unwrap();
            // No pragma, and no temporary table, which a Durable Object refuses.
            assert!(!sql.contains("pragma") && !sql.contains("temp table"), "{sql}");
            // The file as any runner would run it: on a connection with foreign keys on, in a transaction.
            let mut db = Db::connect(&p.url, &p.dir).await.unwrap();
            assert_eq!(db.rows("pragma foreign_keys").await.unwrap()[0].first().map(|v| v.to_string()), Some("1".to_string()));
            db.exec("begin").await.unwrap();
            db.script(&sql).await.unwrap_or_else(|e| panic!("{e}\n{sql}"));
            db.exec("commit").await.unwrap();
            assert!(db.rows("pragma foreign_key_check").await.unwrap().is_empty());
            let count = |name: String, rows: Vec<Row>| (name, rows[0].text("n").to_string());
            for (table, n) in [("parents", "2"), ("kids", "3"), ("pets", "2"), ("chores", "2"), ("toys", "1"), ("a_links", "1"), ("accounts", "1"), ("logins", "1"), ("sessions", "1")] {
                let rows = db.rows(&format!("select count(*) as n from {table}")).await.unwrap();
                assert_eq!(count(table.into(), rows), (table.to_string(), n.to_string()), "{sql}");
            }
            assert_eq!(db.rows("select count(*) as n from pets where parent_id is null").await.unwrap()[0].text("n"), "0", "{sql}");
            assert_eq!(db.rows("select count(*) as n from parents where boss = 1").await.unwrap()[0].text("n"), "1", "{sql}");
            assert_eq!(db.rows("select count(*) as n from kid_names").await.unwrap()[0].text("n"), "3", "{sql}");
            // AUTOINCREMENT's counter came along, and the keys still hold.
            db.exec("insert into kids (parent_id, note) values (1, 'next')").await.unwrap();
            assert_eq!(db.rows("select max(id) as n from kids").await.unwrap()[0].text("n"), next);
            assert!(db.exec("insert into kids (parent_id) values (99)").await.is_err(), "the key to the new parents holds");
            db.exec("delete from kids where note = 'next'").await.unwrap();
            // The key that is an index stands under its own name, and nothing the rebuild made for itself is left.
            assert!(db.exec("insert into logins values (2, 'nobody@example.com')").await.is_err(), "the key to the new accounts holds");
            assert!(db.exec("insert into sessions values (2, 'nobody@example.com')").await.is_err(), "the key to the new logins holds");
            assert_eq!(db.rows("select group_concat(name, ' ') as n from (select name from sqlite_master where type = 'index' and tbl_name in ('accounts', 'logins') order by name)").await.unwrap()[0].text("n"), format!("{key} logins_email"), "{sql}");
            assert_eq!(db.rows("select count(*) as n from sqlite_master where name like 'new\\_%' escape '\\' or name like 'reylo\\_aside\\_%' escape '\\'").await.unwrap()[0].text("n"), "0", "{sql}");
            db.close().await;
            // The database reads as the file says, and as schema.sql does.
            assert_eq!(p.dev().await, 0, "{}", p.peek());
            p.output();
            assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        }
    }

    pub async fn a_rebuild_keeps_tables_that_reference_each_other(mut p: P) {
        let tables = |check: &str| format!("create table a (id integer primary key, b_id integer references b (id), name text{check}); create table b (id integer primary key, a_id integer references a (id) on delete cascade);");
        p.schema(&tables(""));
        p.file("20261027100000_t.sql", &tables(""));
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("insert into a values (1, null, 'x'); insert into b values (1, 1); update a set b_id = 1").await;
        p.output();
        p.schema(&tables(", check (length(name) > 0)"));
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "check_name", &Default::default(), log).await).await, 0, "{}", p.peek());
        p.output();
        let made = p.files().into_iter().find(|f| f.ends_with("_check_name.sql")).unwrap();
        let sql = std::fs::read_to_string(p.dir.join("migrations").join(&made)).unwrap();
        assert!(sql.contains("pragma defer_foreign_keys = on"), "{sql}");
        let mut db = Db::connect(&p.url, &p.dir).await.unwrap();
        db.exec("begin").await.unwrap();
        db.script(&sql).await.unwrap_or_else(|e| panic!("{e}\n{sql}"));
        db.exec("commit").await.unwrap();
        assert!(db.rows("pragma foreign_key_check").await.unwrap().is_empty());
        assert_eq!(db.rows("select b_id from a").await.unwrap()[0].text("b_id"), "1", "{sql}");
        assert_eq!(db.rows("select a_id from b").await.unwrap()[0].text("a_id"), "1", "{sql}");
        db.close().await;
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    pub async fn a_migration_that_names_a_database_is_refused(mut p: P) {
        p.schema("create table t (id int);");
        p.file("20261027100000_t.sql", &format!("create table {}.t (id int);", p.name));
        assert_eq!(p.dry_run().await, 1);
        assert_has!(p.output(), "names the database");
        assert!(p.rows(&format!("select 1 from information_schema.tables where table_schema = '{}' and table_name = 't'", p.name)).await.is_empty());
        // Where no keyword before the name says it is a table's: an insert with no `into`.
        p.file("20261027100000_t.sql", "create table t (id int);");
        p.exec("create table t (id int)").await;
        p.file("20261027110000_row.sql", &format!("insert {}.t values (1);", p.name));
        assert_eq!(p.dry_run().await, 1);
        assert_has!(p.output(), &format!(r"Migration 20261027110000_row\.sql can't be replayed in a shadow database\n\n    insert {}\.t values \(1\)\n\n  It names the database {}: in a shadow", p.name, p.name));
        assert!(p.rows("select id from t").await.is_empty(), "no shadow wrote into the database it named");
    }

    pub async fn a_partition_of_a_run_time_partition_is_data(mut p: P) {
        p.schema("create table ev (id int, at date not null, region text not null) partition by range (at);");
        p.file("20261027100000_ev.sql", "create table ev (id int, at date not null, region text not null) partition by range (at);");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("create table ev_2026 partition of ev for values from ('2026-01-01') to ('2027-01-01') partition by list (region); create table ev_2026_eu partition of ev_2026 for values in ('eu'); insert into ev values (1, '2026-03-01', 'eu');").await;
        p.schema("create table ev (id int, at date not null, region text not null) partition by range (at); create table other (id int);");
        assert_eq!(p.db_push().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select count(*) as n from ev").await[0].text("n"), "1");
        assert_eq!(p.dry_run().await, 1, "{}", p.peek());
        assert!(!p.output().contains("ev_2026"));
    }

    pub async fn a_partition_dropped_at_run_time_is_data(mut p: P) {
        let ddl = "create table ev (id int, at date not null) partition by range (at); create table ev_2025 partition of ev for values from ('2025-01-01') to ('2026-01-01');";
        p.schema(ddl);
        p.file("20261027100000_ev.sql", ddl);
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("drop table ev_2025").await;
        p.output();
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
    }

    /// The refusal of a migration that creates a table again, read as a person would: its last
    /// command run (with `--blocking` too where that run asks for it), and the lines it prints pasted
    /// at the top and the end of the file it writes, which then runs. `renames` are stated each time.
    async fn keep_the_rows(p: &mut P, name: &str, renames: &[(&str, &str)]) -> String {
        let rename: Vec<(String, String)> = renames.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
        let choices = reylo_migrate::Choices { rename: rename.clone(), ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, name, &choices, log).await).await, 1);
        let out = p.output();
        assert!(!out.contains("renamed instead"), "{out}");
        // The last command, its lines joined where it runs on.
        let lines: Vec<&str> = out.lines().collect();
        let at = lines.iter().rposition(|l| l.trim_start().starts_with(&format!("reylo migrate new {name}"))).unwrap_or_else(|| panic!("{out}"));
        let mut command = String::new();
        for l in &lines[at..] {
            command.push_str(l.trim().trim_end_matches('\\').trim_end());
            command.push(' ');
            if !l.trim_end().ends_with('\\') {
                break;
            }
        }
        let drop: Vec<String> = command.split_once(" --drop ").map(|(_, d)| d.split(' ').next().unwrap_or_default().split(',').map(String::from).collect()).unwrap_or_default();
        // Every refusal's lines, each under the heading it is printed under.
        let (mut top, mut end, mut under) = (String::new(), String::new(), 0);
        for l in out.lines().map(str::trim) {
            match l {
                "-- at the top" => under = 1,
                _ if l.starts_with("-- right after it creates ") => under = 2,
                "" => under = 0,
                _ if under == 1 => top.push_str(&format!("{l}\n")),
                _ if under == 2 => end.push_str(&format!("{l}\n")),
                _ => {}
            }
        }
        assert!(!top.is_empty() && !end.is_empty(), "{out}");
        let mut choices = reylo_migrate::Choices { drop, rename, blocking: command.contains(" --blocking"), ..Default::default() };
        // A change of key rebuilds the partitions' keys, which is refused until --blocking accepts it.
        if p.run(async |o, log| reylo_migrate::create(o, name, &choices, log).await).await != 0 {
            assert!(!choices.blocking && p.output().contains("--blocking"), "{}", p.peek());
            choices.blocking = true;
            assert_eq!(p.run(async |o, log| reylo_migrate::create(o, name, &choices, log).await).await, 0, "{}", p.peek());
        }
        p.output();
        let file = p.dir.join("migrations").join(p.files().into_iter().find(|f| f.ends_with(&format!("_{name}.sql"))).unwrap());
        let sql = std::fs::read_to_string(&file).unwrap();
        // Right after the statements that create the tables again.
        let made = sql.rfind("\ncreate table ").map(|at| at + sql[at..].find(";\n").unwrap() + 2).unwrap_or_else(|| panic!("{sql}"));
        std::fs::write(&file, format!("{top}{}{end}{}", &sql[..made], &sql[made..])).unwrap();
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.output();
        assert_eq!(p.status().await, 0, "{}", p.peek());
        p.output();
        out
    }

    /// A table made partitioned, made plain, or given another partition key is created again, which
    /// Postgres can't do in place: refused as that, with the statements that keep its rows, one
    /// command that writes the migration, and no rename offered.
    pub async fn a_table_made_partitioned_is_refused_for_what_postgres_cant_change(mut p: P) {
        let plain = "create table users (id int generated always as identity primary key, name text); create table logs (id int, msg text);";
        let hashed = "create table users (id int generated always as identity primary key, name text) partition by hash (id); create table users_p0 partition of users for values with (modulus 2, remainder 0); create table users_p1 partition of users for values with (modulus 2, remainder 1); create table logs (id int, msg text);";
        p.schema(plain);
        p.file("20261027100000_users.sql", plain);
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("insert into users (name) select 'u' || g from generate_series(1, 10) g").await;
        p.output();
        // Each time the rows stay, and the next id comes past them.
        let kept = async |p: &mut P| {
            assert_eq!(p.rows("select count(*) as n from users").await[0].text("n"), "10");
            assert_eq!(p.rows("insert into users (name) values ('next') returning id").await[0].text("id"), "11");
            p.exec("delete from users where name = 'next'").await;
        };
        p.schema(hashed);
        let out = keep_the_rows(&mut p, "partition_users", &[]).await;
        kept(&mut p).await;
        assert_has!(&out, r"✗ Postgres can't make users a partitioned table in place\n  A table is partitioned only when it's created, so this migration drops users and its rows to create it again\.\n  To keep the rows, add these lines to the migration it writes:\n\n    -- at the top\n    lock table users in exclusive mode;\n    create table users_rows as select \* from users;\n    -- right after it creates users and its partitions\n    insert into users \(id, name\) overriding system value select id, name from users_rows;\n    select setval\(pg_get_serial_sequence\('users', 'id'\), coalesce\(\(select max\(id\)::numeric \+ 1 from users having max\(id\)::numeric \+ 1 between seqmin and seqmax and max\(id\)::numeric \+ 1 > seqstart\), seqstart\)::bigint, false\) from pg_sequence where seqrelid = pg_get_serial_sequence\('users', 'id'\)::regclass;\n    drop table users_rows;\n\n  To write it, dropping the rows:\n    reylo migrate new partition_users --drop users\n");
        // Made plain again: its partitions go first, so the copy at the top is what keeps the rows.
        p.schema(plain);
        let out = keep_the_rows(&mut p, "unpartition", &[]).await;
        kept(&mut p).await;
        assert_has!(&out, r"✗ Postgres can't make users a plain table in place\n  A partitioned table is made plain only by creating it again, so this migration drops users, its partitions and their rows\.\n");
        assert_has!(&out, r"reylo migrate new unpartition --drop users_p0,users_p1,users\n");
        // Given another partition key beside a column dropped: one refusal for the table and its
        // partitions, and one command writes it all.
        p.schema(hashed);
        keep_the_rows(&mut p, "partition_again", &[]).await;
        kept(&mut p).await;
        p.schema("create table users (id int generated always as identity, name text not null, primary key (id, name)) partition by hash (name); create table users_p0 partition of users for values with (modulus 2, remainder 0); create table users_p1 partition of users for values with (modulus 2, remainder 1); create table logs (id int);");
        let out = keep_the_rows(&mut p, "rekey", &[]).await;
        kept(&mut p).await;
        assert_has!(&out, r"✗ Postgres can't change the partition key of users in place\n  A partition key is set only when its table is created, so this migration drops users, its partitions and their rows to create them again\.\n");
        assert_eq!(out.matches('✗').count(), 2, "{out}");
        assert_eq!(out.matches("reylo migrate new rekey").count(), 1, "{out}");
    }

    /// The lines that keep a remade table's rows put each value back where the schema now has it: in
    /// a column renamed with it, cast to a type it changes to, with its identity at the start the
    /// schema gives it, and a partition the schema keeps as a table of its own keeping its own rows.
    pub async fn the_lines_that_keep_a_remade_tables_rows_follow_the_change(mut p: P) {
        let before = "create table users (id int generated always as identity primary key, full_name text not null, score varchar(20), total numeric generated always as (id * 2) stored);";
        p.schema(before);
        p.file("20261027100000_users.sql", before);
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("insert into users (full_name, score) select 'u' || g, g::text from generate_series(1, 10) g").await;
        p.output();
        let users = "create table users (id int generated always as identity (start with 1000), display_name text not null, score integer, total numeric generated always as (id * 3) stored, primary key (id)) partition by hash (id); create table users_p0 partition of users for values with (modulus 2, remainder 0); create table users_p1 partition of users for values with (modulus 2, remainder 1);";
        p.schema(users);
        let out = Box::pin(keep_the_rows(&mut p, "partition_users", &[("users.full_name", "display_name")])).await;
        assert_has!(&out, r"insert into users \(display_name, id, score\) overriding system value select full_name, id, score::integer from users_rows;");
        assert_eq!(p.rows("select count(*) as n from users where display_name = 'u' || id and score = id and total = id * 3").await[0].text("n"), "10");
        assert_eq!(p.rows("insert into users (display_name) values ('next') returning id").await[0].text("id"), "1000");
    }

    /// Each sequence a remade table's rows are put back under moves past them where the migration
    /// makes it again (a serial's, made with the table or new), and keeps its place where it outlives
    /// the table; one whose column makes something else of its values is left to be moved by hand.
    pub async fn a_remade_tables_sequences_move_past_the_rows_put_back(mut p: P) {
        let before = "create table counters (id serial primary key, n text); create table serials (id bigint primary key, n text); create sequence ticket_no; create table tickets (id bigint primary key default nextval('ticket_no'), n text); create sequence invoices_no; create table invoices (code text primary key default 'INV-' || nextval('invoices_no'), n text); alter sequence invoices_no owned by invoices.code; create table smalls (id smallint primary key, n text);";
        p.schema(before);
        p.file("20261027100000_tables.sql", before);
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.output();
        p.exec("insert into counters (n) select 'c' || g from generate_series(1, 10) g; insert into serials select g, 's' || g from generate_series(1, 10) g; insert into tickets (n) select 't' || g from generate_series(1, 10) g; select setval('ticket_no', 100); insert into invoices (n) select 'i' || g from generate_series(1, 3) g; insert into smalls values (32767, 'top')").await;
        // Each made again: a serial key, whose sequence the migration makes the column's only after
        // the rows are back; a key given a serial; a sequence the table only called, which it comes
        // to own, and which keeps its place, as other tables may have taken its values; and a text
        // column its own sequence feeds through an expression, which is left to be moved by hand.
        p.schema("create table counters (id serial primary key, n text) partition by hash (id); create table counters_0 partition of counters for values with (modulus 1, remainder 0); \
            create table serials (id bigserial primary key, n text) partition by hash (id); create table serials_0 partition of serials for values with (modulus 1, remainder 0); \
            create sequence ticket_no; create table tickets (id bigint primary key default nextval('ticket_no'), n text) partition by hash (id); create table tickets_0 partition of tickets for values with (modulus 1, remainder 0); alter sequence ticket_no owned by tickets.id; \
            create sequence invoices_no; create table invoices (code text primary key default 'INV-' || nextval('invoices_no'), n text) partition by hash (code); create table invoices_0 partition of invoices for values with (modulus 1, remainder 0); alter sequence invoices_no owned by invoices.code; \
            create sequence smalls_no as bigint; create table smalls (id smallint primary key default nextval('smalls_no'), n text) partition by hash (id); create table smalls_0 partition of smalls for values with (modulus 1, remainder 0); alter sequence smalls_no owned by smalls.id;");
        let out = Box::pin(keep_the_rows(&mut p, "partition_all", &[])).await;
        assert!(!out.contains("setval('ticket_no'"), "{out}");
        assert_has!(&out, r"-- and move the sequence invoices_no past the values put back in code");
        assert_eq!(p.rows("insert into counters (n) values ('next') returning id").await[0].text("id"), "11");
        assert_eq!(p.rows("insert into serials (n) values ('next') returning id").await[0].text("id"), "11");
        assert_eq!(p.rows("select nextval('ticket_no') as n").await[0].text("n"), "101");
        assert_eq!(p.rows("select count(*) as n from invoices").await[0].text("n"), "3");
        // A sequence that counts wider than its key may find no value past the rows the key holds:
        // it is left to be moved by hand.
        assert_has!(&out, r"-- and move the sequence smalls_no past the values put back in id");
        assert_eq!(p.rows("insert into smalls (n) values ('next') returning id").await[0].text("id"), "1");
    }

    /// A list-partitioned table made plain, one of its partitions kept as a table of its own, keeps
    /// that partition's rows apart, those of its own partitions included.
    pub async fn a_partition_kept_as_a_table_of_its_own_keeps_its_rows(mut p: P) {
        // A list-partitioned table made plain, one of its partitions kept as a table of its own, whose
        // rows are in a partition of its own.
        let regions = "create table regions (id int generated always as identity, region text not null, name text, primary key (id, region)) partition by list (region); create table regions_eu partition of regions for values in ('eu'); create table regions_us partition of regions for values in ('us') partition by hash (id); create table regions_us_0 partition of regions_us for values with (modulus 1, remainder 0);";
        p.schema(regions);
        p.file("20261027100000_regions.sql", regions);
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.output();
        p.exec("insert into regions (region, name) select case when g <= 5 then 'eu' else 'us' end, 'r' || g from generate_series(1, 10) g").await;
        p.schema("create table regions (id int generated always as identity, region text not null, name text, primary key (id, region)); create table regions_us (id int generated always as identity, region text not null, name text, primary key (id, region)) partition by hash (id); create table regions_us_0 partition of regions_us for values with (modulus 1, remainder 0);");
        let out = Box::pin(keep_the_rows(&mut p, "unpartition_regions", &[])).await;
        assert_has!(&out, r"create table regions_rows as select \* from regions where tableoid not in \(select relid from pg_partition_tree\('regions_us'::regclass\)\);");
        assert_eq!(p.rows("select count(*) as n from regions where region = 'eu'").await[0].text("n"), "5");
        assert_eq!(p.rows("select count(*) as n from regions where region = 'us'").await[0].text("n"), "0");
        assert_eq!(p.rows("select count(*) as n from only regions_us_0 where region = 'us'").await[0].text("n"), "5");
    }

    /// A start moved on an identity no index leads with reads every row of its table, which is
    /// locked against writes while it does, so the migration asks for --blocking.
    pub async fn a_sequence_moved_past_unindexed_rows_asks_for_blocking(mut p: P) {
        // A partial index serves no max over every row; a column renamed in the same change is
        // found where the block reads it.
        let before = "create table log (id bigint generated always as identity, msg text); create table events (id bigint generated always as identity primary key, msg text); create table active (id bigint generated always as identity, live boolean); create index active_live on active (id) where live; create table renamed (id bigint generated always as identity, msg text); create sequence stage_no; create table stage (id bigint, n text); create sequence later_no; create table later (id bigint, n text); create sequence fed_no; create table fed (id bigint default nextval('fed_no'), n text); \
            create sequence order_no; create table orders (id bigint primary key, secondary_id bigint default nextval('order_no')); alter sequence order_no owned by orders.id; \
            create sequence idx_no; create table idx (id bigint default nextval('idx_no')); create index \"idx$id\" on idx (id); \
            create sequence dropdef_no; create table dropdef (id bigint default nextval('dropdef_no')); \
            create sequence rn_no; create table rn (id bigint default nextval('rn_no')); \
            create sequence pk_no; create table pk (id bigint constraint pk_key primary key default nextval('pk_no')); \
            create sequence part_no; create table part (id bigint, constraint part_key primary key (id)) partition by range (id); create table part_1 partition of part (id default nextval('part_no')) for values from (0) to (100000); \
            create sequence kept_no; create table kept (id bigint constraint kept_key primary key default nextval('kept_no')); create table kept_items (kept_id bigint constraint kept_items_fk references kept (id));";
        p.schema(before);
        p.file("20261027100000_log.sql", before);
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("insert into log (msg) select 'm' || g from generate_series(1, 10) g; insert into events (msg) select 'e' || g from generate_series(1, 10) g; insert into stage values (2000, 's'); insert into later values (2000, 'l'); insert into fed values (2000, 'f')").await;
        p.output();
        // A default the change gives a column is set after the read, which so doesn't reach it; one
        // already there is read.
        p.schema("create sequence later_no start with 1000; create table later (id bigint default nextval('later_no'), n text); create sequence fed_no start with 1000; create table fed (id bigint default nextval('fed_no'), n text); \
            create sequence order_no start with 1000; create table orders (id bigint primary key, secondary_id bigint default nextval('order_no')); alter sequence order_no owned by orders.id; \
            create sequence idx_no start with 1000; create table idx (id bigint default nextval('idx_no')); \
            create sequence dropdef_no start with 1000; create table dropdef (id bigint); \
            create sequence rn_no start with 1000; create table rn (new_id bigint default nextval('rn_no')); \
            create sequence pk_no start with 1000; create table pk (id bigint not null default nextval('pk_no')); \
            create sequence part_no start with 1000; create table part (id bigint not null) partition by range (id); create table part_1 partition of part (id default nextval('part_no')) for values from (0) to (100000); \
            create sequence kept_no start with 1000; create table kept (id bigint constraint kept_key primary key default nextval('kept_no')); create table kept_items (kept_id bigint); \
            create sequence stage_no start with 1000; create table stage (id bigint, n text); alter sequence stage_no owned by stage.id; create table log (id bigint generated always as identity (start with 1000), msg text); create table events (id bigint generated always as identity (start with 1000) primary key, msg text); create table active (id bigint generated always as identity (start with 1000), live boolean); create index active_live on active (id) where live; create table renamed (new_id bigint generated always as identity (start with 1000), msg text);");
        let rename = vec![("renamed.id".to_string(), "new_id".to_string()), ("rn.id".to_string(), "new_id".to_string())];
        let choices = reylo_migrate::Choices { rename: rename.clone(), ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "move_ids", &choices, log).await).await, 1);
        let out = p.output();
        assert!(!out.contains("of later while"), "{out}");
        // An indexed column the sequence owns hides no other column it feeds; a column renamed
        // ahead of the read is still read.
        for table in ["log", "active", "renamed", "fed", "orders", "rn"] {
            assert_has!(&out, &format!(r"reads every row of {table} while it blocks writes to it, to move a sequence past them"));
        }
        assert!(!out.contains("of events while"), "an indexed column is read through its index: {out}");
        assert!(!out.contains("of stage while"), "a table the sequence comes to own after the read isn't read: {out}");
        let choices = reylo_migrate::Choices { rename, blocking: true, ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "move_ids", &choices, log).await).await, 0, "{}", p.peek());
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_eq!(p.rows("insert into log (msg) values ('next') returning id").await[0].text("id"), "1000");
        assert_eq!(p.rows("select nextval('stage_no') as n").await[0].text("n"), "1000");
        assert_eq!(p.rows("select nextval('fed_no') as n").await[0].text("n"), "2001");
        // A default the change drops is read only where it is dropped after the read.
        let file = std::fs::read_to_string(p.dir.join("migrations").join(p.files().into_iter().find(|f| f.ends_with("_move_ids.sql")).unwrap())).unwrap();
        let at = |text: &str| file.lines().position(|l| l.contains(text)).unwrap_or_else(|| panic!("{file}"));
        let read_first = at("'public.dropdef_no'") < at("alter column id drop default");
        assert_eq!(out.contains("of dropdef while"), read_first, "{file}");
        // An index the change drops serves the read only where it is dropped after it, whatever its
        // name holds.
        let dropped_first = file.lines().position(|l| l.contains("drop index") && l.contains("idx$id")).is_some_and(|d| d < at("'public.idx_no'"));
        assert_eq!(out.contains("of idx while"), dropped_first, "{file}");
        // So does a key the change drops, whose index goes with it.
        let key_first = file.lines().position(|l| l.contains("drop constraint pk_key")).is_some_and(|d| d < at("'public.pk_no'"));
        assert_eq!(out.contains("of pk while"), key_first, "{file}");
        // A partitioned table's key takes its partitions' indexes with it, whatever they are named.
        let parent_first = file.lines().position(|l| l.contains("drop constraint part_key")).is_some_and(|d| d < at("'public.part_no'"));
        assert_eq!(out.contains("of part_1 while"), parent_first, "{file}");
        // A foreign key dropped takes no index: the key it references still serves the read.
        assert!(!out.contains("of kept while"), "{out}");
    }

    /// The check of a sequence's read reads the catalog and runs nothing of the file but the file:
    /// a dry run that deploys to another database files that take a serial's values around the read
    /// runs them as the deploy would, and still finds the read.
    pub async fn a_sequence_read_is_checked_without_moving_the_files_sequences(mut p: P) {
        let tables = |start: &str| format!("create table parent (id serial primary key); create table child (parent_id integer references parent (id)); create table log (id bigint generated always as identity{start}, msg text);");
        p.schema(&tables(""));
        p.file("20261027100000_tables.sql", &tables(""));
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "seed", &Default::default(), log).await).await, 0, "{}", p.peek());
        let named = |p: &P, name: &str| p.dir.join("migrations").join(p.files().into_iter().find(|f| f.ends_with(&format!("_{name}.sql"))).unwrap());
        std::fs::write(named(&p, "seed"), "insert into parent default values;\n").unwrap();
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.output();
        p.schema(&tables(" (start with 1000)"));
        let choices = reylo_migrate::Choices { blocking: true, ..Default::default() };
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "move_log", &choices, log).await).await, 0, "{}", p.peek());
        let file = named(&p, "move_log");
        let sql = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, format!("{sql}\ninsert into parent default values;\ninsert into child values (2);\n")).unwrap();
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.output();
        let q = P::new(p.d).await.unwrap();
        p.url = q.url.clone();
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert_has!(p.output(), r"reads every row of log while it blocks writes to it, to move a sequence past them");
    }

    /// A partitioned table whose key column is renamed keeps a partition made at run time, which
    /// the schema doesn't declare, with its rows: the column is renamed in it too.
    pub async fn a_key_renamed_keeps_a_partition_made_at_run_time(mut p: P) {
        let before = "create table m (old_id int, n text) partition by range (old_id); create table m_1 partition of m for values from (0) to (100);";
        p.schema(before);
        p.file("20261027100000_m.sql", before);
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("create table m_runtime partition of m for values from (100) to (200); insert into m values (150, 'kept')").await;
        p.output();
        p.schema("create table m (new_id int, n text) partition by range (new_id); create table m_1 partition of m for values from (0) to (100);");
        assert_eq!(p.db_push().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select n from m_runtime where new_id = 150").await[0].text("n"), "kept");
    }

    pub async fn init_creates_the_extensions_its_schema_uses(mut p: P) {
        p.exec("create extension citext; create table u (id int primary key, email citext);").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        assert_has!(std::fs::read_to_string(p.dir.join("migrations/init.sql")).unwrap(), r"(?i)create extension (if not exists )?citext");
        let q = P::new(p.d).await.unwrap();
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(q.history().await, ["init.sql"]);
    }

    /// Default privileges and grants a migration made, as Prisma's migrations make them for a role a
    /// `do` block makes: init.sql makes them, which a new database lacks, and names the role, which a
    /// new server may lack. A new database deployed from it has the grants this one has.
    pub async fn init_writes_the_default_privileges_and_grants_a_migration_made(mut p: P) {
        p.exec("do $$ begin create role madv_ops_read nologin; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec("create table mints (id int primary key); create table users (id int primary key)").await;
        p.exec("grant usage on schema public to madv_ops_read; grant select, insert on all tables in schema public to madv_ops_read; alter default privileges in schema public grant select, insert on tables to madv_ops_read; revoke insert on mints from madv_ops_read").await;
        p.output();
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        assert_has!(p.output(), r"» init\.sql names the role madv_ops_read: make it on each new server before it runs there, since a migration may not");
        let init = std::fs::read_to_string(p.dir.join("migrations/init.sql")).unwrap();
        assert_has!(&init, r"(?m)^alter default privileges in schema public grant insert on tables to madv_ops_read;$");
        assert_has!(&init, r"(?m)^alter default privileges in schema public grant select on tables to madv_ops_read;$");
        assert_has!(&init, r"(?m)^grant usage on schema public to madv_ops_read;$");
        p.strict = true;
        assert_eq!(p.status().await, 0, "{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        let q = P::new(p.d).await.unwrap();
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        let grants = "select string_agg(table_name || ':' || privilege_type, ',' order by table_name, privilege_type) as g from information_schema.role_table_grants where grantee = 'madv_ops_read'";
        assert_eq!(q.rows(grants).await[0].text("g"), "mints:SELECT,users:INSERT,users:SELECT");
        assert_eq!(q.rows("select has_schema_privilege('madv_ops_read', 'public', 'usage')::text as u").await[0].text("u"), "true");
        q.exec("create table later (id int)").await;
        assert_eq!(q.rows("select has_table_privilege('madv_ops_read', 'later', 'insert')::text as i").await[0].text("i"), "true", "the default privileges made");
    }

    /// A database whose only change from a new one is a grant on `public`: init writes it, and
    /// introspect prints it, rather than starting an empty project.
    pub async fn init_writes_a_database_of_grants_alone(mut p: P) {
        p.exec("do $$ begin create role madv_ops_read nologin; exception when duplicate_object or unique_violation then null; end $$").await;
        p.exec("grant create on schema public to madv_ops_read").await;
        assert_has!(introspect(&p.opts()).await, r"(?m)^grant create on schema public to madv_ops_read;$");
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        assert_has!(std::fs::read_to_string(p.dir.join("migrations/init.sql")).unwrap(), r"(?m)^grant create on schema public to madv_ops_read;$");
    }

    pub async fn an_extension_in_a_schema_of_its_own_is_dumped_after_it(mut p: P) {
        p.exec("create schema app; create extension citext with schema app; create table app.u (id int primary key, email app.citext);").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &["app".to_string()], log).await).await, 0, "{}", p.peek());
        let q = P::new(p.d).await.unwrap();
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(q.history().await, ["init.sql"]);
    }

    /// An event trigger the project's role owns (Supabase's `ensure_rls`, which turns on row security
    /// for each table made in `public`) is the project's: init writes it, so shadows make it too, and
    /// a table a later migration makes has row security in both.
    pub async fn init_writes_the_event_triggers_it_compares(mut p: P) {
        p.exec("create function rls_auto_enable() returns event_trigger language plpgsql as $$ declare cmd record; begin for cmd in select * from pg_event_trigger_ddl_commands() where command_tag = 'CREATE TABLE' and schema_name = 'public' loop execute format('alter table %s enable row level security', cmd.object_identity); end loop; end $$").await;
        p.exec("create event trigger ensure_rls on ddl_command_end when tag in ('CREATE TABLE', 'CREATE TABLE AS', 'SELECT INTO') execute function rls_auto_enable()").await;
        p.exec("create table users (id int primary key)").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        assert_has!(std::fs::read_to_string(p.dir.join("migrations/init.sql")).unwrap(), r"(?i)create event trigger ensure_rls on ddl_command_end");
        p.schema(&format!("{}\ncreate table posts (id int primary key);\n", std::fs::read_to_string(p.dir.join("schema.sql")).unwrap()));
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "posts", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select 1 from pg_class where relname = 'posts' and relrowsecurity").await.len(), 1, "the trigger turned on the new table's row security");
        assert_eq!(p.status().await, 0, "the shadows made the trigger, so the new table's row security is no drift:\n{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    /// Publications the project's role owns (Supabase's empty `supabase_realtime`, and one naming a
    /// table's columns and rows): init writes them, and a new database replays them.
    pub async fn init_writes_the_publications_it_compares(mut p: P) {
        p.exec("create table users (id int primary key, email text, active bool)").await;
        p.exec("create publication supabase_realtime").await;
        p.exec("create publication active_users for table users (id, email) where (id > 0) with (publish = 'insert, update')").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        let init = std::fs::read_to_string(p.dir.join("migrations/init.sql")).unwrap();
        assert_has!(init.clone(), r"(?i)create publication supabase_realtime");
        assert_has!(init, r"(?i)create publication active_users");
        assert_eq!(p.status().await, 0, "{}", p.peek());
        let q = P::new(p.d).await.unwrap();
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        let made: Vec<String> = q.rows("select pubname::text as n from pg_publication order by 1").await.iter().map(|r| r.text("n").to_string()).collect();
        assert_eq!(made, ["active_users", "supabase_realtime"]);
    }

    /// A varchar column's `in (…)` renders as a cast of the whole array, which Postgres reads back as
    /// a cast of each element: a database made from that rendering is still the same schema, so init
    /// starts the project, with such a list in a check, a partial index's predicate and a policy,
    /// and a later migration leaves them alone. A comment that holds such text is compared as
    /// written.
    pub async fn init_reads_an_in_list_on_a_varchar_column_as_it_reads_back(mut p: P) {
        p.exec("create table items (id int primary key, status varchar(10) not null check (status in ('open', 'closed')))").await;
        p.exec("create index open_items on items (id) where status not in ('closed', 'gone')").await;
        p.exec("alter table items enable row level security").await;
        p.exec("create policy open_only on items using (status in ('open', 'new')) with check (status in ('open', 'closed'))").await;
        // A call in the list is a value too, as the literals beside it say.
        p.exec("create function vf() returns varchar language sql as 'select ''c''::varchar'").await;
        p.exec("create table tags (name varchar(10) check (name in ('a', 'b', vf())), label varchar(10) check (label in ('a', vf() collate \"C\")))").await;
        p.exec("comment on table items is 'Example: \"((a AND b) AND c)\"'").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        assert_eq!(p.status().await, 0, "{}", p.peek());
        // A comment's text is compared as written, and a change to it is a migration.
        let schema = std::fs::read_to_string(p.dir.join("schema.sql")).unwrap();
        assert!(schema.contains("((a AND b) AND c)"), "{schema}");
        p.schema(&format!("{}\ncreate table notes (id int primary key);\n", schema.replace("((a AND b) AND c)", "(a AND b AND c)")));
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "notes", &Default::default(), log).await).await, 0, "{}", p.peek());
        let written = p.files().into_iter().find(|f| f.ends_with("_notes.sql")).unwrap();
        let sql = std::fs::read_to_string(p.dir.join("migrations").join(&written)).unwrap();
        assert!(sql.contains("(a AND b AND c)"), "the comment's change is in the migration:\n{sql}");
        assert!(!sql.contains("check") && !sql.contains("index") && !sql.contains("policy"), "the migration remakes no check, index or policy:\n{sql}");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_eq!(p.status().await, 0, "{}", p.peek());
    }

    /// A row comparison of three or more columns is stored as ORs nested to the left, which
    /// Postgres reads back as one: init starts the project, with such a comparison in a check, a
    /// trigger's condition and a partial index's predicate.
    pub async fn init_reads_a_row_comparison_as_it_reads_back(mut p: P) {
        p.exec(
            "create table t (id int primary key, a int, b int, c int, d int, e int, f int,
              constraint c_distinct check ((a, b, c) is distinct from (d, e, f)),
              constraint c_not_distinct check ((a, b, c) is not distinct from (d, e, f)),
              constraint c_four check ((a, b, c, id) is distinct from (d, e, f, id)),
              constraint c_eq check ((a, b, c) = (d, e, f) or a is null),
              constraint c_ne check ((a, b, c) <> (d, e, f) or a is null),
              constraint c_lt check ((a, b, c) < (d, e, f) or a is null),
              constraint c_mixed check (((a, b, c) is distinct from (d, e, f)) and a > 0 and b > 0))",
        )
        .await;
        p.exec("create function f() returns trigger language plpgsql as $$ begin return new; end $$").await;
        p.exec("create trigger t_changed after update of a, b, c on t for each row when ((old.a, old.b, old.c) is distinct from (new.a, new.b, new.c)) execute function f()").await;
        p.exec("create trigger t_same after update on t for each row when ((old.a, old.b, old.c) is not distinct from (new.a, new.b, new.c)) execute function f()").await;
        p.exec("create index t_pi on t (id) where (a, b, c) is distinct from (d, e, f)").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        assert_eq!(p.status().await, 0, "{}", p.peek());
    }

    pub async fn an_extension_left_in_a_schema_of_the_projects_own_stops_no_push(mut p: P) {
        p.schema("create schema ext;\ncreate extension pg_trgm with schema ext;\ncreate table t (id int);");
        assert_eq!(p.db_push().await, 0, "{}", p.peek());
        // schema.sql stops declaring the extension, which the database keeps, and still makes its
        // schema: the shadow leaves the extension out, as it does one the project makes.
        p.schema("create schema ext;\ncreate table t (id int, n int);");
        assert_eq!(p.db_push().await, 0, "{}", p.peek());
        assert_eq!(p.rows("select count(*)::text as n from information_schema.columns where table_name = 't' and column_name = 'n'").await[0].text("n"), "1");
    }

    pub async fn a_dump_keeps_a_function_body_as_written(mut p: P) {
        p.file("20261027100000_counters.sql", "create table counters (n int);");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("create function bump() returns void language sql as $$\nupdate counters\nSET n = n + 1;\n$$;\ncreate function two_blank() returns text language sql as $$\nselect 'a\n\n\nb'\n$$;").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "bump", &Default::default(), log).await).await, 0, "{}", p.peek());
        let written = p.files().into_iter().find(|f| f.ends_with("_bump.sql")).unwrap();
        let sql = std::fs::read_to_string(p.dir.join("migrations").join(written)).unwrap();
        assert!(sql.contains("update counters\nSET n = n + 1;") && sql.contains("select 'a\n\n\nb'"), "{sql}");
    }

    pub async fn a_history_that_does_not_replay_is_refused_before_anything_changes(mut p: P) {
        p.file("20261027100000_one.sql", "create table t (id int primary key);");
        p.file("20261027110000_two.sql", "do $$ begin if current_database() not like 'mtest%' then raise exception 'only on the app database'; end if; end $$; create table b (id int);");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("create table hack (x int); insert into hack values (42);").await;
        std::fs::remove_file(p.dir.join("migrations/20261027110000_two.sql")).unwrap();
        p.output();
        assert_eq!(p.dev().await, 1);
        assert_has!(p.output(), r"✗ Replaying 20261027110000_two\.sql in a shadow database, as this database ran it, fails");
        assert_eq!(p.rows("select x from hack").await.len(), 1);
    }

    pub async fn without_a_lock_a_history_out_of_name_order_is_held_to_name_order(mut p: P) {
        p.linear();
        p.file("20261027100000_t.sql", "create table t (v int);");
        p.file("20261027130000_b.sql", "alter table t alter column v set default 2;");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        // A branch's migration, written earlier and merged later, runs after b while the lock lists it.
        p.file("20261027120000_a.sql", "alter table t alter column v set default 1;");
        p.lock(&["20261027100000_t.sql", "20261027130000_b.sql", "20261027120000_a.sql"]);
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        std::fs::remove_file(p.dir.join(reylo_migrate::lock::LOCK)).unwrap();
        p.output();
        assert_eq!(p.dev().await, 1);
        assert_has!(p.output(), r"✗ This database ran its migrations in another order than a fresh database runs them, and the two end differently");
        assert_eq!(p.dry_run().await, 1);
    }

    pub async fn a_recurring_event_is_in_the_dump(mut p: P) {
        p.file("20261027100000_t.sql", "create table t (id int primary key);");
        p.file("20261027110000_nightly.sql", "create event nightly on schedule every 1 day do delete from t;");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("alter table t add column extra int").await;
        p.output();
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "extra", &Default::default(), log).await).await, 0, "{}", p.peek());
        let texts: String = p.files().iter().filter(|f| f.contains("_extra")).map(|f| std::fs::read_to_string(p.dir.join("migrations").join(f)).unwrap()).collect();
        assert!(!texts.to_lowercase().contains("drop event"), "{texts}");
    }

    /// A view that calls a stored function: MySQL refuses the view until the function exists.
    pub async fn init_makes_a_function_before_the_view_that_calls_it(mut p: P) {
        p.exec("create table t (id int primary key, n int)").await;
        p.exec("create function twice(x int) returns int deterministic no sql return x * 2").await;
        p.exec("create view v as select id, twice(n) as n2 from t").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        let q = P::new(p.d).await.unwrap();
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(q.history().await, ["init.sql"]);
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    /// A foreign server is in no schema, so a dump of the schemas leaves it out, and with it what its
    /// foreign tables are made on.
    pub async fn init_writes_the_foreign_servers_its_tables_use(mut p: P) {
        p.exec("create foreign data wrapper madv_w options (debug 'true'); create server madv_s type 'pg' version '18' foreign data wrapper madv_w options (host 'db.example', dbname 'app')").await;
        p.exec("create foreign table users (id int, email text) server madv_s options (table_name 'users'); create table notes (id int primary key, body text)").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        assert_has!(std::fs::read_to_string(p.dir.join("migrations/init.sql")).unwrap(), r"(?i)create server madv_s");
        let q = P::new(p.d).await.unwrap();
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(q.history().await, ["init.sql"]);
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    /// A Postgres 18 publication that publishes its tables' stored generated columns: init writes the
    /// setting, and a new database's publication publishes them too.
    pub async fn init_keeps_a_publications_generated_columns(mut p: P) {
        if p.rows("select 1 where current_setting('server_version_num')::int >= 180000").await.is_empty() {
            eprintln!("skipped: publish_generated_columns is Postgres 18's");
            return;
        }
        p.exec("create table t (id int primary key, n int generated always as (id * 2) stored)").await;
        p.exec("create publication stored_pub for table t with (publish_generated_columns = stored)").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        assert_has!(std::fs::read_to_string(p.dir.join("migrations/init.sql")).unwrap(), r"(?i)publish_generated_columns = stored");
        assert_eq!(p.status().await, 0, "{}", p.peek());
        let q = P::new(p.d).await.unwrap();
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(q.rows("select pubgencols::text as g from pg_publication where pubname = 'stored_pub'").await[0].text("g"), "s");
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    /// A wrapper's validator is a function of the schema, which pg_dump writes with the others: init
    /// makes the wrapper after it, and the wrapper's server before the foreign table made on it.
    pub async fn init_makes_a_wrapper_after_the_functions_it_names(mut p: P) {
        p.exec("create function validate_options(text[], oid) returns void language plpgsql as $$ begin return; end $$").await;
        p.exec("create foreign data wrapper review_fdw validator validate_options; create server review_remote foreign data wrapper review_fdw").await;
        p.exec("create foreign table remote_users (id int) server review_remote; create table t (id int)").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        let q = P::new(p.d).await.unwrap();
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(q.history().await, ["init.sql"]);
        assert_eq!(q.rows("select 1 from pg_foreign_data_wrapper where fdwname = 'review_fdw' and fdwvalidator = 'validate_options'::regproc").await.len(), 1);
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    /// What the comparison reads of a wrapper and a server beside their definitions: their grants, an
    /// owner that isn't the runner, and their comments. The roles are Postgres's own, so that no role
    /// is made.
    pub async fn init_writes_a_wrappers_and_servers_grants_owner_and_comment(mut p: P) {
        p.exec("create table t (id int); create foreign data wrapper review_fdw; create server review_remote foreign data wrapper review_fdw").await;
        p.exec("grant usage on foreign data wrapper review_fdw to pg_monitor; comment on foreign data wrapper review_fdw is 'remote wrapper'").await;
        p.exec("grant usage on foreign server review_remote to pg_monitor with grant option; comment on server review_remote is 'remote server'; alter server review_remote owner to pg_read_all_data").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::init(o, &[], log).await).await, 0, "{}", p.peek());
        let q = P::new(p.d).await.unwrap();
        p.url = q.url.clone();
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        let read = "select format('%s %s %s %s', pg_get_userbyid(srvowner), has_server_privilege('pg_monitor', oid, 'usage with grant option'), obj_description(oid, 'pg_foreign_server'),
            (select format('%s %s', has_foreign_data_wrapper_privilege('pg_monitor', w.oid, 'usage'), obj_description(w.oid, 'pg_foreign_data_wrapper')) from pg_foreign_data_wrapper w where w.oid = srvfdw)) as s
          from pg_foreign_server where srvname = 'review_remote'";
        assert_eq!(q.rows(read).await[0].text("s"), "pg_read_all_data t remote server t remote wrapper");
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    }

    /// A libSQL vector index keeps itself in tables of its own: `<index>_shadow`, with its index, and
    /// `libsql_vector_meta_shadow`, which outlives the last index. They are libSQL's, as a virtual
    /// table's own tables are SQLite's, so the catalog, the dump and `wipe` leave them alone, while a
    /// table of the user's with a name like theirs is read as any other.
    pub async fn a_vector_indexs_own_tables_are_libsqls(mut p: P) {
        p.file("20261027100000_docs.sql", "create table docs (id integer primary key, emb F32_BLOB(3)); create table x_shadow (a integer);");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        // Reylo's SQLite can't make the index, so it is made by hand, as on a database Reylo adopts.
        p.exec("create index docs_emb on docs (libsql_vector_idx(emb))").await;
        p.exec("insert into docs (emb) values (vector32('[1,2,3]')), (vector32('[3,2,1]'))").await;
        let own = ["docs_emb_shadow", "libsql_vector_meta_shadow"];
        let sql = introspect(&p.opts()).await;
        assert!(sql.contains("docs_emb on docs") && sql.contains("x_shadow") && !own.iter().any(|t| sql.contains(t)), "{sql}");
        let mut db = Db::connect(&p.url, &p.dir).await.unwrap();
        let lines = catalog(&mut db).await.unwrap();
        db.close().await;
        assert!(lines.iter().any(|l| l.contains("x_shadow")) && !lines.iter().any(|l| own.iter().any(|t| l.contains(t))), "{lines:#?}");
        // Emptied, the index still works: its own tables are libSQL's to empty.
        assert_eq!(p.run(async |o, log| reylo_migrate::db_wipe(o, false, log).await).await, 0, "{}", p.peek());
        p.exec("insert into docs (emb) values (vector32('[1,2,3]'))").await;
        assert_eq!(p.rows("select count(*) as n from vector_top_k('docs_emb', vector32('[1,2,3]'), 1)").await[0].text("n"), "1");
        // Dropped, the index leaves its settings table behind, which is still libSQL's.
        p.exec("drop index docs_emb").await;
        assert_eq!(p.status().await, 0, "{}", p.peek());
    }

    /// The tables of Turso's engine in a file, by name: each `AUTOINCREMENT` table's sequence, with
    /// its row.
    async fn turso_tables(p: &P) -> Vec<String> {
        let mut out = Vec::new();
        for r in p.rows(r"select name from sqlite_schema where name like '\_\_turso\_internal\_%' escape '\' and name not like '%stray' order by name").await {
            let name = r.text("name").to_string();
            let row = p.rows(&format!("select value || ' ' || is_called as v from \"{name}\"")).await.iter().map(|r| r.text("v").to_string()).collect::<Vec<_>>().join(", ");
            out.push(format!("{} {row}", name.trim_start_matches("__turso_internal_seq___turso_internal_autoincrement_")));
        }
        out
    }

    /// Turso's engine writes tables of its own into a SQLite file as ordinary ones, each
    /// `AUTOINCREMENT` table's sequence among them (`__turso_internal_seq___turso_internal_autoincrement_<table>`),
    /// drops and renames that with its table, and refuses to drop it by name. Every `__turso_internal_`
    /// table is the engine's: no introspection, plan, status or wipe reads one, and as tables come, go,
    /// are rebuilt and renamed, each `AUTOINCREMENT` table keeps one sequence, named after it.
    pub async fn a_turso_engines_own_tables_are_tursos(mut p: P) {
        let sequence = |t: &str| {
            let name = format!("\"__turso_internal_seq___turso_internal_autoincrement_{t}\"");
            format!("create table {name}(value INTEGER PRIMARY KEY,is_called INTEGER,start INTEGER,inc INTEGER,min INTEGER,max INTEGER,cycle INTEGER); insert into {name} values (1, 0, 1, 1, 1, 9223372036854775807, 0);")
        };
        let v1 = "create table posts (id integer primary key autoincrement, body text);\ncreate table notes (id integer primary key autoincrement, body text);\ncreate table tags (id integer primary key, name text);";
        p.schema(v1);
        p.file("20261027100000_v1.sql", v1);
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        // The file as the engine leaves it when it made the tables and wrote rows.
        p.exec(&format!("{} {} insert into posts (body) values ('a'), ('b');", sequence("posts"), sequence("notes"))).await;
        // A table of that name that the engine didn't define is never dropped or emptied.
        let stray = "__turso_internal_seq___turso_internal_autoincrement_stray";
        p.exec(&format!("create table {stray} (note text); insert into {stray} values ('kept');")).await;
        let sql = introspect(&p.opts()).await;
        assert!(sql.contains("create table posts") && !sql.contains("__turso"), "{sql}");
        let mut db = Db::connect(&p.url, &p.dir).await.unwrap();
        let lines = catalog(&mut db).await.unwrap();
        db.close().await;
        assert!(!lines.iter().any(|l| l.contains("__turso")), "{lines:#?}");
        p.output();
        assert_eq!(p.status().await, 0, "{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        // `notes` goes, `posts` is rebuilt, `tags` is rebuilt with AUTOINCREMENT, and `events` comes.
        p.schema("create table posts (id integer primary key autoincrement, body text check (length(body) < 100));\ncreate table tags (id integer primary key autoincrement, name text);\ncreate table events (id integer primary key autoincrement);");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "v2", &reylo_migrate::Choices { drop: vec!["notes".into()], ..Default::default() }, log).await).await, 0, "{}", p.peek());
        let made = p.files().into_iter().find(|f| f.ends_with("_v2.sql")).unwrap();
        let sql = std::fs::read_to_string(p.dir.join("migrations").join(&made)).unwrap();
        assert!(sql.contains("drop table notes") && !sql.contains("__turso"), "{sql}");
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(turso_tables(&p).await, ["events 1 0", "posts 1 0", "tags 1 0"]);
        // A run that fails partway still leaves each table it made with a sequence.
        let code = p.run(async |o, log| reylo_migrate::run_sql(o, &[("make".into(), "create table drafts (id integer primary key autoincrement); begin; create table scraps (id integer primary key autoincrement); select * from missing;".into())], log).await).await;
        assert_eq!(code, 1, "{}", p.peek());
        assert_eq!(turso_tables(&p).await, ["drafts 1 0", "events 1 0", "posts 1 0", "tags 1 0"]);
        // And so does one that ends with its transaction open, which closing rolls back.
        let code = p.run(async |o, log| reylo_migrate::run_sql(o, &[("make".into(), "create table sketches (id integer primary key autoincrement); begin; create table scraps (id integer primary key autoincrement);".into())], log).await).await;
        assert_eq!(code, 0, "{}", p.peek());
        assert_eq!(turso_tables(&p).await, ["drafts 1 0", "events 1 0", "posts 1 0", "sketches 1 0", "tags 1 0"]);
        p.exec("drop table drafts; drop table sketches").await;
        p.exec("insert into posts (body) values ('c')").await;
        assert_eq!(p.rows("select max(id) as n from posts").await[0].text("n"), "3");
        // Renamed, `posts` takes its counter, which is `sqlite_sequence`'s, and its sequence is renamed.
        p.schema("create table articles (id integer primary key autoincrement, body text check (length(body) < 100));\ncreate table tags (id integer primary key autoincrement, name text);\ncreate table events (id integer primary key autoincrement);");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "v3", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_eq!(p.migrate().await, 0, "{}", p.peek());
        assert_eq!(turso_tables(&p).await, ["articles 1 0", "events 1 0", "tags 1 0"]);
        p.exec("insert into articles (body) values ('d')").await;
        assert_eq!(p.rows("select max(id) as n from articles").await[0].text("n"), "4");
        // Wiped, each sequence starts again, as `sqlite_sequence` does.
        p.exec("update \"__turso_internal_seq___turso_internal_autoincrement_articles\" set value = 4, is_called = 1").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::db_wipe(o, false, log).await).await, 0, "{}", p.peek());
        assert_eq!(turso_tables(&p).await, ["articles 1 0", "events 1 0", "tags 1 0"]);
        // Reset, the tables are made again, each with its sequence.
        assert_eq!(p.run(async |o, log| reylo_migrate::db_reset(o, false, log).await).await, 0, "{}", p.peek());
        assert_eq!(turso_tables(&p).await, ["articles 1 0", "events 1 0", "tags 1 0"]);
        assert_eq!(p.rows("pragma integrity_check").await[0].text("integrity_check"), "ok");
        assert_eq!(p.rows(&format!("select note from {stray}")).await[0].text("note"), "kept");
        p.output();
        assert_eq!(p.status().await, 0, "{}", p.peek());
    }

    pub async fn a_virtual_tables_own_tables_are_not_in_the_dump(mut p: P) {
        p.file("20261027100000_docs.sql", "create table docs (id integer primary key, body text); create virtual table docs_fts using fts5(body, content='docs', content_rowid='id');");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        let sql = introspect(&p.opts()).await;
        assert!(!sql.contains("docs_fts_data"), "{sql}");
        p.exec("alter table docs add column extra int").await;
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "extra", &Default::default(), log).await).await, 0, "{}", p.peek());
    }

    /// An external-content full-text table holds an index of its content table's rows and none of
    /// its own: made over a table that has rows, or made again with other options, the migration
    /// rebuilds the index from them, needs no `--drop`, and a search finds the rows that were there.
    pub async fn an_external_content_index_is_rebuilt_from_its_rows(mut p: P) {
        let notes = "create table notes (id integer primary key, body text not null);";
        p.schema(notes);
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "notes", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("insert into notes values (1, 'running quickly'), (5, 'walking slowly')").await;
        let last = |p: &P| std::fs::read_to_string(p.dir.join("migrations").join(p.files().last().unwrap())).unwrap();
        for (name, options) in [("search", ""), ("porter", ", tokenize='porter'")] {
            p.schema(&format!("{notes}\ncreate virtual table notes_fts using fts5(body, content='notes', content_rowid='id'{options});"));
            p.output();
            assert_eq!(p.run(async |o, log| reylo_migrate::create(o, name, &Default::default(), log).await).await, 0, "{}", p.peek());
            let sql = last(&p);
            assert!(sql.contains("insert into notes_fts (notes_fts) values ('rebuild');") && !sql.contains("loses data"), "{sql}");
            assert!(!p.output().contains("Drops the table"));
            assert_eq!(p.dev().await, 0, "{}", p.peek());
        }
        // Porter's stemmer finds "running" for "run", which only the rebuilt index holds.
        assert_eq!(p.rows("select rowid from notes_fts where notes_fts match 'run'").await.iter().map(|r| r.text("rowid").to_string()).collect::<Vec<_>>(), ["1"]);
        p.exec("insert into notes_fts (notes_fts, rank) values ('integrity-check', 1)").await;
        assert_eq!(p.status().await, 0, "{}", p.peek());
    }

    /// libSQL's own SQL, a vector index and `alter table … alter column`, which Reylo's SQLite
    /// refuses: the project's development file and its shadows run on libSQL's engine, so each
    /// command takes it, and the index's own tables are libSQL's, never drift.
    pub async fn libsqls_own_sql_migrates(mut p: P) {
        p.linear();
        p.schema("create table docs (id integer primary key, emb F32_BLOB(3));\ncreate index docs_emb on docs (libsql_vector_idx(emb));");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "docs", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        p.exec("insert into docs (id, emb) values (1, vector('[1,2,3]'))").await;
        assert_eq!(p.rows("select id from vector_top_k('docs_emb', vector('[1,2,3]'), 1)").await[0].text("id"), "1");
        assert_eq!(p.status().await, 0, "{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        // A column made not null in place, as libSQL does it, written by hand.
        p.file("20991231000000_emb_not_null.sql", "alter table docs alter column emb to emb F32_BLOB(3) not null;");
        p.schema("create table docs (id integer primary key, emb F32_BLOB(3) not null);\ncreate index docs_emb on docs (libsql_vector_idx(emb));");
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        assert_eq!(p.status().await, 0, "{}", p.peek());
        assert_eq!(p.dry_run().await, 0, "{}", p.peek());
        assert!(p.try_rows("insert into docs (id, emb) values (2, null) returning id").await.is_err(), "the column is not null");
        let sql = introspect(&p.opts()).await;
        assert!(sql.contains("libsql_vector_idx") && !sql.contains("_shadow"), "{sql}");
    }

    /// A database that ran a migration of libSQL's own SQL stays libSQL's on a branch whose files
    /// have none: its history replays on libSQL's engine, so it is pulled back and rewound.
    pub async fn a_libsql_history_replays_on_a_branch_without_it(mut p: P) {
        p.schema("create table docs (id integer primary key, emb F32_BLOB(3));\ncreate index docs_emb on docs (libsql_vector_idx(emb));");
        assert_eq!(p.run(async |o, log| reylo_migrate::create(o, "docs", &Default::default(), log).await).await, 0, "{}", p.peek());
        assert_eq!(p.dev().await, 0, "{}", p.peek());
        let files = p.files();
        // Another branch: no migration of it, and a schema.sql of SQLite's alone.
        for f in &files {
            std::fs::remove_file(p.dir.join("migrations").join(f)).unwrap();
        }
        p.schema("");
        assert_eq!(p.run(async |o, log| reylo_migrate::history_pull(o, log).await).await, 0, "{}", p.peek());
        assert_eq!(p.files(), files, "the migration is written back");
        for f in &files {
            std::fs::remove_file(p.dir.join("migrations").join(f)).unwrap();
        }
        assert_eq!(p.dev().await, 0, "{}", p.peek());
    }
}

// reylo run on a SQLite file that isn't there refuses, as the migrations' commands do, rather than
// make an empty database at a mistyped path.
#[tokio::test(flavor = "current_thread")]
async fn reylo_run_refuses_a_file_that_is_not_there() {
    let Some(mut p) = P::new(Dialect::Sqlite).await else { return };
    let url = format!("sqlite:{}", p.dir.join("prod.sqlitee").display());
    let items = [("create".to_string(), "create table audit (seen timestamp)".to_string())];
    let code = p.run(async |o, log| reylo_migrate::run_sql(&reylo_migrate::Options { url: url.clone(), ..o.clone() }, &items, log).await).await;
    assert_eq!(code, 1, "{}", p.peek());
    assert_has!(p.output(), r"✗ There's no SQLite database at [^\n]*\n\nNothing was changed\.");
    assert!(!p.dir.join("prod.sqlitee").exists(), "a file was made");
}

/// A libSQL server ends a stream left idle: one outside a transaction is opened again, with the
/// pragmas the connection set (a migration turns foreign keys off before its transaction, so that a
/// table it rebuilds cascades nothing), and one inside a transaction is an error, since the server
/// rolled it back.
#[tokio::test(flavor = "current_thread")]
async fn a_turso_connection_left_idle_opens_again_outside_a_transaction() {
    let Some(p) = P::turso().await else { return };
    let mut idle = Db::connect(&p.url, &p.dir).await.unwrap();
    let mut open = Db::connect(&p.url, &p.dir).await.unwrap();
    idle.exec("create table t (a integer)").await.unwrap();
    idle.exec("pragma foreign_keys = off").await.unwrap();
    open.exec("begin").await.unwrap();
    open.exec("insert into t values (1)").await.unwrap();
    // sqld ends a stream idle for ten seconds.
    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    assert_eq!(idle.rows("pragma foreign_keys").await.unwrap()[0].text("foreign_keys"), "0", "the new stream keeps the connection's pragmas");
    assert_eq!(idle.rows("select count(*) as n from t").await.unwrap()[0].text("n"), "0");
    let e = open.exec("commit").await.unwrap_err();
    assert!(e.0.contains("idle too long"), "{e}");
    assert_eq!(idle.rows("select count(*) as n from t").await.unwrap()[0].text("n"), "0", "the transaction was rolled back");
}

/// `deploy --dry-run -- <command>`, the command a shell runs, with its exit code, and what it saw.
async fn dry_run_then(p: &mut P, script: &str) -> i32 {
    let command = vec!["sh".to_string(), "-c".to_string(), script.to_string()];
    p.run(async |o, log| reylo_migrate::dry_run_then(o, Some(&command), log).await).await
}

async fn a_dry_run_builds_on_a_shadow_of_what_deploy_leaves(mut p: P) {
    let (id, text) = (p.t.id, p.t.text);
    p.file("20990101000000_users.sql", &format!("create table users (id {id}, email {text});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.file("20990101010000_nick.sql", &format!("alter table users add column nick {text};"));
    p.output();
    let dir = p.dir.display().to_string();
    // The command sees the shadow's URL, a SQLite shadow as a file it can copy, and its exit code is the run's.
    let script = format!(r#"printf %s "$REYLO_ANALYSIS_URL" > '{dir}/url.txt'; case "$REYLO_ANALYSIS_URL" in sqlite:*) cp "${{REYLO_ANALYSIS_URL#sqlite:}}" '{dir}/seen.db';; esac; exit 7"#);
    assert_eq!(dry_run_then(&mut p, &script).await, 7, "{}", p.peek());
    let url = std::fs::read_to_string(p.dir.join("url.txt")).unwrap();
    assert_ne!(url, p.url, "the command must read a shadow, never the database itself");
    match p.d {
        // The shadow had both migrations, and is gone with the command.
        Dialect::Sqlite => {
            assert!(!std::path::Path::new(url.trim_start_matches("sqlite:")).exists(), "{url}");
            let mut seen = Db::connect(&format!("sqlite:{dir}/seen.db"), &p.dir).await.unwrap();
            let columns: Vec<String> = seen.rows("select name from pragma_table_info('users')").await.unwrap().iter().map(|r| r.text("name").to_string()).collect();
            seen.close().await;
            assert_eq!(columns, ["id", "email", "nick"]);
        }
        _ => {
            let name = url.rsplit('/').next().unwrap().split('?').next().unwrap().to_string();
            assert!(name.starts_with("reylo_shadow_"), "{url}");
            let left = if p.d == Dialect::Postgres { format!("select count(*) as n from pg_database where datname = '{name}'") } else { format!("select count(*) as n from information_schema.schemata where schema_name = '{name}'") };
            assert_eq!(p.rows(&left).await[0].text("n"), "0", "{url}");
        }
    }
    // Nothing ran here: the deploy after the build runs the pending migration.
    assert_eq!(p.history().await, ["20990101000000_users.sql"]);
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    assert_eq!(p.history().await, ["20990101000000_users.sql", "20990101010000_nick.sql"]);
}

async fn a_dry_run_that_refuses_runs_no_command(mut p: P) {
    let (id, text) = (p.t.id, p.t.text);
    p.file("20990101000000_users.sql", &format!("create table users (id {id}, email {text});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    // An applied migration edited since, which the deploy after the build would refuse.
    p.file("20990101000000_users.sql", &format!("create table users (id {id}, email {text}, nick {text});"));
    p.output();
    let ran = p.dir.join("ran");
    assert_eq!(dry_run_then(&mut p, &format!("touch '{}'", ran.display())).await, 1, "{}", p.peek());
    assert!(!ran.exists(), "{}", p.peek());
}

async fn a_dry_run_runs_no_command_for_a_schema_sql_the_deploy_refuses(mut p: P) {
    let int = p.t.int;
    p.linear();
    p.schema(&format!("create table a (id {int});"));
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    assert_eq!(p.dev().await, 0, "{}", p.peek());
    // The same schema, written otherwise: the migrations still reach it, and the lock's checksum no longer holds.
    p.schema(&format!("CREATE TABLE a (id {int});"));
    p.output();
    let ran = p.dir.join("ran");
    assert_eq!(dry_run_then(&mut p, &format!("touch '{}'", ran.display())).await, 1, "{}", p.peek());
    assert!(!ran.exists(), "{}", p.peek());
    assert_has!(p.output(), r"The schema changed since the lock was written");
}

/// A first deploy's SQLite file, which the dry run opens and removes, is the command's once it runs.
async fn a_first_deploys_sqlite_file_is_left_to_the_command(mut p: P) {
    if p.d != Dialect::Sqlite || p.server.is_some() {
        return;
    }
    let file = p.url.trim_start_matches("sqlite:").to_string();
    let _ = std::fs::remove_file(&file);
    p.file("20990101000000_users.sql", &format!("create table users (id {});", p.t.id));
    assert_eq!(dry_run_then(&mut p, &format!("printf kept > '{file}'")).await, 0, "{}", p.peek());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "kept");
    // With nothing to run, the file the dry run made is gone after it.
    std::fs::remove_file(&file).unwrap();
    assert_eq!(p.dry_run().await, 0, "{}", p.peek());
    assert!(!std::path::Path::new(&file).exists());
}

/// A cut-off run whose change landed, on a database with no fingerprint to hold it to, which the
/// deploy after the build runs again: the build runs where that succeeds, and not where it fails.
async fn a_landed_run_the_deploy_retries(mut p: P, idempotent: bool) {
    if p.d == Dialect::Sqlite || (idempotent && p.d != Dialect::Postgres) {
        return;
    }
    let int = p.t.int;
    let lone = match (p.d, idempotent) {
        (Dialect::Postgres, true) => "create index concurrently if not exists a_id on a (id)",
        (Dialect::Postgres, false) => "create index concurrently a_id on a (id)",
        _ => "create index a_id on a (id)",
    };
    p.file("20261027100000_a.sql", &format!("create table a (id {int});"));
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.file("20261027110000_index.sql", &format!("{lone};"));
    let mut db = Db::connect(&p.url, &p.dir).await.unwrap();
    let column = if p.d == Dialect::Mysql { "`sql`" } else { "sql" };
    let insert = format!("insert into __reylo_migrations (name, {column}, applied_at) values ({}, {}, null)", db.param(1), db.param(2));
    db.exec_with(&insert, &["20261027110000_index.sql", &format!("{lone};")]).await.unwrap();
    db.exec(lone).await.unwrap();
    db.exec("update __reylo_migrations set fingerprint = null").await.unwrap();
    db.close().await;
    p.output();
    let ran = p.dir.join("ran");
    let code = dry_run_then(&mut p, &format!("touch '{}'", ran.display())).await;
    assert_eq!((code, ran.exists()), (if idempotent { 0 } else { 1 }, idempotent), "{}", p.peek());
    // The deploy agrees.
    assert_eq!(p.migrate().await, if idempotent { 0 } else { 1 }, "{}", p.peek());
}

async fn a_landed_run_the_deploy_retries_and_fails_stops_the_build(p: P) {
    a_landed_run_the_deploy_retries(p, false).await
}

async fn a_landed_run_the_deploy_retries_cleanly_lets_the_build_run(p: P) {
    a_landed_run_the_deploy_retries(p, true).await
}

/// A pending migration that the deploy runs before a landed run's retry, and that makes the retry
/// fail: the shadow retries it in the same place, so the build runs exactly where the deploy succeeds.
async fn a_landed_run_is_retried_where_the_deploy_retries_it(mut p: P) {
    if p.d != Dialect::Postgres {
        return;
    }
    let lone = "create unique index concurrently if not exists a_id on a (id)";
    p.file("20261027100000_a.sql", "create table a (id int);");
    assert_eq!(p.migrate().await, 0, "{}", p.peek());
    p.file("20261027120000_index.sql", &format!("{lone};"));
    let mut db = Db::connect(&p.url, &p.dir).await.unwrap();
    db.exec_with("insert into __reylo_migrations (name, sql, applied_at) values ($1, $2, null)", &["20261027120000_index.sql", &format!("{lone};")]).await.unwrap();
    db.exec(lone).await.unwrap();
    db.exec("update __reylo_migrations set fingerprint = null").await.unwrap();
    db.close().await;
    // Before the index in run order: it drops what landed, and writes rows the index refuses.
    p.file("20261027110000_dup.sql", "drop index a_id; insert into a values (1), (1);");
    p.output();
    let ran = p.dir.join("ran");
    let code = dry_run_then(&mut p, &format!("touch '{}'", ran.display())).await;
    let built = ran.exists();
    let deployed = p.migrate().await;
    assert_eq!((code == 0, built), (deployed == 0, deployed == 0), "the build runs where the deploy succeeds: {}", p.peek());
}

for_each_dialect!(
    a_landed_run_is_retried_where_the_deploy_retries_it,
    a_landed_run_the_deploy_retries_and_fails_stops_the_build,
    a_landed_run_the_deploy_retries_cleanly_lets_the_build_run,
    a_dry_run_builds_on_a_shadow_of_what_deploy_leaves,
    a_dry_run_that_refuses_runs_no_command,
    a_dry_run_runs_no_command_for_a_schema_sql_the_deploy_refuses,
    a_first_deploys_sqlite_file_is_left_to_the_command,
);

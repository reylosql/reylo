//! The shadows a command builds. Each is a lane: steps (`schema.sql`, or migrations) that a command
//! adds in order and whose catalog it reads at points between them. A lane is only made a database
//! when the cache (`cache`) cannot answer: its lines at a prefix come from the cache where a run
//! kept them, and its database starts as the longest cached prefix of its steps and replays the
//! rest. Without the cache, every lane is a new shadow that runs every step, as it always was.

use crate::cache::{count, snapshot, Cache, Hash};
use crate::catalog::catalog;
use crate::commands::{run, Migration};
use crate::db::{Db, Server, Shadow};
use crate::terminal::Spinner;
use crate::{Dialect, Error};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub(crate) struct Step {
    pub label: String,
    pub sql: String,
    /// `schema.sql` runs as written; a migration runs as `deploy` runs it, atomically.
    pub schema: bool,
}

impl Step {
    pub(crate) fn script(label: &str, sql: &str) -> Step {
        Step { label: label.into(), sql: sql.into(), schema: true }
    }
    pub(crate) fn migration(name: &str, sql: &str) -> Step {
        Step { label: name.into(), sql: sql.into(), schema: false }
    }
    pub(crate) fn of(m: &Migration) -> Step {
        Step::migration(&m.name, &m.sql)
    }
}

/// What the spinner says while a shadow database is made or read, before a step runs on it.
const SPINNING: &str = "Checking on a shadow database";

/// A step that failed: its label and why.
pub(crate) type Failed = (String, String);

/// What [`Shadows::before`] did for a step that makes pg_cron.
pub(crate) enum PgCron {
    Unneeded,
    Named,
    /// Why the shadow couldn't take pg_cron's name.
    Refused(String),
}

struct Lane {
    steps: Vec<Step>,
    /// The cache's key of each prefix: `keys[i]` is that of the first `i` steps.
    keys: Vec<Hash>,
    shadow: Option<Shadow>,
    /// How many steps the shadow has run, and how many it started with (a cached prefix's).
    at: usize,
    from: usize,
    /// The shadow's session as it opened.
    baseline: Option<String>,
    /// Whether the cache may answer for this lane, or keep what it reaches: not once a step failed,
    /// its session was seen to differ from a new one's, or a command wrote to it.
    caching: bool,
    /// Prefixes to keep as caches on the way, where the next check is likely to start.
    marks: Vec<usize>,
    /// The lines at a prefix, once read.
    memo: Option<(usize, Vec<String>)>,
}

pub(crate) struct Shadows<'s> {
    pub(crate) server: &'s mut Server,
    cache: Option<Cache>,
    lanes: Vec<Lane>,
}

impl<'s> Shadows<'s> {
    /// The shadows of one run on `server`, for the target at `url`; `texts` are the SQL of every
    /// step the run may replay.
    pub(crate) async fn new(server: &'s mut Server, url: &str, root: &Path, cache: &Path, texts: &[&str]) -> Shadows<'s> {
        let cache = Cache::open(server, url, root, cache, texts).await;
        Shadows { server, cache, lanes: vec![] }
    }

    pub(crate) fn lane(&mut self) -> usize {
        let keys = self.cache.as_ref().map(|c| vec![c.base()]).unwrap_or_default();
        self.lanes.push(Lane { steps: vec![], keys, shadow: None, at: 0, from: 0, baseline: None, caching: self.cache.is_some(), marks: vec![], memo: None });
        self.lanes.len() - 1
    }

    pub(crate) fn push(&mut self, lane: usize, steps: impl IntoIterator<Item = Step>) {
        let lane = &mut self.lanes[lane];
        for step in steps {
            if let Some(prev) = lane.keys.last() {
                let key = Cache::step(prev, step.schema, &step.label, &step.sql);
                lane.keys.push(key);
            }
            lane.steps.push(step);
        }
    }

    /// Keeps the lane's database at its current end as a cache, should it run past it.
    pub(crate) fn mark(&mut self, lane: usize) {
        let lane = &mut self.lanes[lane];
        lane.marks.push(lane.steps.len());
    }

    /// The catalog's lines after the lane's steps, or the step that failed.
    pub(crate) async fn lines(&mut self, l: usize) -> Result<Result<Vec<String>, Failed>, Error> {
        let n = self.lanes[l].steps.len();
        if let Some((at, lines)) = &self.lanes[l].memo {
            if *at == n {
                return Ok(Ok(lines.clone()));
            }
        }
        let here = self.lanes[l].shadow.is_some() && self.lanes[l].at == n;
        if let (Some(cache), true, false) = (&self.cache, self.lanes[l].caching, here) {
            // Lines are kept only where the session was as a new one, so the read they stand for
            // left it as it was.
            if let Some(lines) = cache.read_lines(&self.lanes[l].keys[n]) {
                count(|s| s.lines += 1);
                self.lanes[l].memo = Some((n, lines.clone()));
                return Ok(Ok(lines));
            }
        }
        let spinning = crate::terminal::spinner(SPINNING);
        if let Err(failed) = self.advance(l, n, &spinning).await? {
            return Ok(Err(failed));
        }
        let mut keep = false;
        if self.cache.is_some() && self.lanes[l].caching && clean(&mut self.lanes[l]).await {
            keep = true;
            if here {
                if let Some(lines) = self.cache.as_ref().and_then(|c| c.read_lines(&self.lanes[l].keys[n])) {
                    count(|s| s.lines += 1);
                    self.lanes[l].memo = Some((n, lines.clone()));
                    return Ok(Ok(lines));
                }
            }
        }
        let lane = &mut self.lanes[l];
        let Some(shadow) = lane.shadow.as_mut() else { return Err(Error("a lane with no shadow".into())) };
        let lines = catalog(&mut shadow.db).await?;
        if keep {
            if let Some(cache) = &self.cache {
                cache.write_lines(&lane.keys[n], &lines);
            }
        }
        lane.memo = Some((n, lines.clone()));
        Ok(Ok(lines))
    }

    /// Makes the lane a database that has run all its steps, for a command to read.
    pub(crate) async fn open(&mut self, l: usize) -> Result<Result<(), Failed>, Error> {
        let n = self.lanes[l].steps.len();
        let spinning = crate::terminal::spinner(SPINNING);
        self.advance(l, n, &spinning).await
    }

    /// The lane's database, once `open`.
    pub(crate) fn db(&mut self, l: usize) -> &mut Db {
        &mut self.lanes[l].shadow.as_mut().expect("an open lane").db
    }

    /// A URL another process opens the lane's database at, once `open`; on SQLite a copy at `file`.
    pub(crate) async fn url(&mut self, l: usize, file: &Path) -> Result<String, Error> {
        let shadow = self.lanes[l].shadow.as_mut().expect("an open lane");
        self.server.url_of(shadow, file).await
    }

    /// The command writes to the lane's database itself: it is no longer the replay of its steps.
    pub(crate) fn taint(&mut self, l: usize) {
        self.lanes[l].caching = false;
        self.lanes[l].memo = None;
    }

    /// Brings the lane's database to `n` steps: from the longest cached prefix, else a new shadow;
    /// `spinning` says which step runs.
    async fn advance(&mut self, l: usize, n: usize, spinning: &Spinner) -> Result<Result<(), Failed>, Error> {
        let d = self.server.dialect();
        if self.lanes[l].shadow.is_none() {
            let mut started = None;
            if let (Some(cache), true) = (&mut self.cache, self.lanes[l].caching) {
                let keys = &self.lanes[l].keys[1..=n];
                for k in cache.databases(self.server, keys).await.unwrap_or_default() {
                    if let Some(shadow) = cache.copy_of(self.server, &keys[k]).await {
                        count(|s| s.copies += 1);
                        started = Some((shadow, k + 1));
                        break;
                    }
                }
            }
            let (mut shadow, k) = match started {
                Some(s) => s,
                None => (self.server.shadow().await?, 0),
            };
            // A prefix that ends with a migration leaves SQLite's foreign keys on, as `run` does.
            if k > 0 && d == Dialect::Sqlite && !self.lanes[l].steps[k - 1].schema {
                shadow.db.exec("pragma foreign_keys = on").await?;
            }
            let lane = &mut self.lanes[l];
            lane.shadow = Some(shadow);
            lane.at = k;
            lane.from = k;
            if self.cache.is_some() && lane.caching {
                lane.baseline = Some(snapshot(&mut lane.shadow.as_mut().expect("a shadow").db).await.unwrap_or_default());
            }
        }
        while self.lanes[l].at < n {
            let i = self.lanes[l].at;
            let step = self.lanes[l].steps[i].clone();
            spinning.say(format!("Replaying {} on a shadow database", step.label));
            let pg_cron = self.before(l, &step.sql).await?;
            let lane = &mut self.lanes[l];
            let db = &mut lane.shadow.as_mut().expect("a shadow").db;
            let result = if step.schema { db.script(&step.sql).await } else { run(db, &Migration { name: step.label.clone(), time: String::new(), path: PathBuf::new(), sql: step.sql.clone() }).await };
            count(|s| s.replayed += 1);
            lane.at = i + 1;
            let result = self.after(l, pg_cron, result).await?;
            let lane = &mut self.lanes[l];
            if let Err(e) = result {
                // The rest was never run: the lane goes on from the failure, as a shadow does.
                lane.caching = false;
                lane.steps.truncate(i + 1);
                lane.keys.truncate((i + 2).min(lane.keys.len()));
                lane.marks.retain(|&m| m <= i + 1);
                return Ok(Err((step.label, e.0)));
            }
            let at = lane.at;
            if lane.marks.contains(&at) && lane.caching && at > lane.from {
                self.keep_on_the_way(l).await?;
            }
        }
        Ok(Ok(()))
    }

    /// Readies lane `l`'s shadow to run `sql`. Where it makes pg_cron, which makes itself only in the
    /// database `cron.database_name` names, the shadow goes by the one other name pg_cron takes until
    /// [`Shadows::after`] names it back.
    pub(crate) async fn before(&mut self, l: usize, sql: &str) -> Result<PgCron, Error> {
        if self.server.dialect() != Dialect::Postgres || !sql.to_ascii_lowercase().contains("pg_cron") || !crate::commands::extensions_in(sql).iter().any(|e| e == "pg_cron") {
            return Ok(PgCron::Unneeded);
        }
        let again = crate::commands::drops_pg_cron(sql);
        let Some(shadow) = self.lanes[l].shadow.take() else { return Ok(PgCron::Unneeded) };
        let (shadow, named) = self.server.name_for_pg_cron(shadow, again).await?;
        self.lanes[l].shadow = Some(shadow);
        Ok(match named {
            Ok(true) => PgCron::Named,
            Ok(false) => PgCron::Unneeded,
            Err(why) => PgCron::Refused(why),
        })
    }

    /// The lane's shadow under its own name again after [`Shadows::before`], with `result`, what ran
    /// on it, and on a failure where pg_cron's name couldn't be taken, why.
    pub(crate) async fn after(&mut self, l: usize, pg_cron: PgCron, result: Result<(), Error>) -> Result<Result<(), Error>, Error> {
        match pg_cron {
            PgCron::Unneeded => Ok(result),
            PgCron::Named => {
                let shadow = self.lanes[l].shadow.take().expect("a shadow");
                self.lanes[l].shadow = Some(self.server.unname_for_pg_cron(shadow).await?);
                Ok(result)
            }
            PgCron::Refused(why) => Ok(result.map_err(|e| if !e.0.contains("cron") { e } else { Error(format!("{}\npg_cron makes itself only in the database cron.database_name names, or in one named {}, and the shadow couldn't take that name: {why}", e.0, crate::db::PG_CRON_NAME)) })),
        }
    }

    /// Runs the migration `m` on lane `l`'s database, for a command that writes to it.
    pub(crate) async fn run(&mut self, l: usize, m: &Migration) -> Result<(), Error> {
        let pg_cron = self.before(l, &m.sql).await?;
        let result = run(self.db(l), m).await;
        self.after(l, pg_cron, result).await?
    }

    /// At a mark: the database is copied into the cache, where none holds its prefix, before the
    /// lane runs on.
    async fn keep_on_the_way(&mut self, l: usize) -> Result<(), Error> {
        let Some(cache) = &mut self.cache else { return Ok(()) };
        if !cache.copies {
            return Ok(());
        }
        let lane = &mut self.lanes[l];
        let key = lane.keys[lane.at];
        if !cache.databases(self.server, &[key]).await.unwrap_or_default().is_empty() || !clean(lane).await {
            return Ok(());
        }
        let shadow = lane.shadow.take().expect("a shadow");
        let mut shadow = cache.save_copy(self.server, shadow, &key).await?;
        lane.baseline = Some(snapshot(&mut shadow.db).await.unwrap_or_default());
        lane.shadow = Some(shadow);
        Ok(())
    }

    /// Drops every shadow, but keeps as caches those that reached a prefix no cache holds.
    pub(crate) async fn finish(mut self) {
        for k in 0..self.lanes.len() {
            if self.lanes[k].shadow.is_none() {
                continue;
            }
            let key = self.worth_keeping(k).await;
            let shadow = self.lanes[k].shadow.take().expect("a shadow");
            let left = match (key, &mut self.cache) {
                (Some(key), Some(cache)) => cache.save(self.server, shadow, &key).await,
                _ => Some(shadow),
            };
            if let Some(shadow) = left {
                self.server.drop_shadow(shadow).await;
            }
        }
        if let Some(cache) = self.cache {
            cache.finish(self.server).await;
        }
    }

    /// The key a done lane's database is kept under: where it ran past where it started, purely as
    /// the replay of its steps, to a prefix no cache holds.
    async fn worth_keeping(&mut self, k: usize) -> Option<Hash> {
        let cache = self.cache.as_ref()?;
        let lane = &mut self.lanes[k];
        if !cache.copies || !lane.caching || lane.at <= lane.from {
            return None;
        }
        let key = *lane.keys.get(lane.at)?;
        if !cache.databases(self.server, &[key]).await.ok()?.is_empty() {
            return None;
        }
        clean(lane).await.then_some(key)
    }
}

/// Whether the lane's session is as it opened; a lane whose is not keeps nothing from then on.
async fn clean(lane: &mut Lane) -> bool {
    let (Some(shadow), Some(baseline)) = (lane.shadow.as_mut(), lane.baseline.as_ref()) else { return false };
    let same = snapshot(&mut shadow.db).await.is_ok_and(|now| &now == baseline);
    if !same {
        lane.caching = false;
    }
    same
}

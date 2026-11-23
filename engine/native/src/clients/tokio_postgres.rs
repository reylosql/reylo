//! The Postgres engine's needs answered over a `tokio-postgres` client: a describe with the
//! statement's parameter types and each column's type, table and column number, and a catalog
//! query with its parameters bound by the types the server infers for them. The needs of a round
//! are asked together, which the client pipelines over its one connection, and a catalog query kept
//! prepared (`Prepared`) is sent without its prepare, so it waits one round trip rather than two.

use crate::replay::ConcurrentAnswerer;
use crate::{AnswerCache, Analysis, AsyncAnswerer, DbError, HostError, ReplayError};
use ::tokio_postgres::types::{ToSql, Type};
use ::tokio_postgres::{Client, Statement};
use reylo_engine_core::database::Database;
use reylo_engine_postgres::Postgres;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio::sync::OnceCell;

/// What a Postgres describe is answered with.
pub type Describe = <Postgres as Database>::Describe;

/// The analysis of `sql`, its needs answered over `client`, each round's together.
pub async fn analyze(client: &Client, sql: &str, cache: &mut AnswerCache<Describe>) -> Result<Analysis, ReplayError<HostError>> {
    crate::replay::run_concurrent::<Postgres, _>(sql, &Answers(client), cache).await
}

/// As [`analyze`], each catalog query prepared once over `client` and kept in `prepared` for the
/// analyses after it on the same client.
pub async fn analyze_prepared(client: &Client, prepared: &Prepared, sql: &str, cache: &mut AnswerCache<Describe>) -> Result<Analysis, ReplayError<HostError>> {
    crate::replay::run_concurrent::<Postgres, _>(sql, &PreparedAnswers(client, prepared), cache).await
}

/// The catalog queries prepared over one client, by their text. A text several needs of one round
/// ask for is prepared once, the others waiting on that prepare. The statements belong to the client
/// they were prepared on, so a client opened again starts with an empty one.
#[derive(Default)]
pub struct Prepared(Mutex<HashMap<String, Arc<OnceCell<Statement>>>>);

impl Prepared {
    async fn statement(&self, client: &Client, sql: &str) -> Result<Statement, ::tokio_postgres::Error> {
        let kept = self.0.lock().unwrap_or_else(|e| e.into_inner()).entry(sql.to_string()).or_default().clone();
        kept.get_or_try_init(|| client.prepare(sql)).await.cloned()
    }
}

/// The engine's needs, answered over a client, each catalog query prepared again each time.
pub struct Answers<'a>(pub &'a Client);

/// The engine's needs, answered over a client, each catalog query prepared once in a [`Prepared`].
pub struct PreparedAnswers<'a>(pub &'a Client, pub &'a Prepared);

impl AsyncAnswerer<Describe> for Answers<'_> {
    type Error = HostError;

    fn describe(&mut self, sql: &str) -> impl Future<Output = Result<Result<Describe, DbError>, HostError>> + Send {
        ConcurrentAnswerer::describe(&*self, sql)
    }

    fn query(&mut self, sql: &str, params: &[Value]) -> impl Future<Output = Result<Result<Vec<Vec<Value>>, DbError>, HostError>> + Send {
        ConcurrentAnswerer::query(&*self, sql, params)
    }
}

impl ConcurrentAnswerer<Describe> for Answers<'_> {
    type Error = HostError;

    fn describe(&self, sql: &str) -> impl Future<Output = Result<Result<Describe, DbError>, HostError>> + Send {
        describe(self.0, sql)
    }

    fn query(&self, sql: &str, params: &[Value]) -> impl Future<Output = Result<Result<Vec<Vec<Value>>, DbError>, HostError>> + Send {
        query(self.0, None, sql, params)
    }

    // A prepare dropped after its Parse is sent has no statement to close, so on a session that goes on it is left to finish.
    fn ends_session(&self, error: &HostError) -> bool {
        self.0.is_closed() || error.0.starts_with(ENDED)
    }
}

impl AsyncAnswerer<Describe> for PreparedAnswers<'_> {
    type Error = HostError;

    fn describe(&mut self, sql: &str) -> impl Future<Output = Result<Result<Describe, DbError>, HostError>> + Send {
        ConcurrentAnswerer::describe(&*self, sql)
    }

    fn query(&mut self, sql: &str, params: &[Value]) -> impl Future<Output = Result<Result<Vec<Vec<Value>>, DbError>, HostError>> + Send {
        ConcurrentAnswerer::query(&*self, sql, params)
    }
}

impl ConcurrentAnswerer<Describe> for PreparedAnswers<'_> {
    type Error = HostError;

    fn describe(&self, sql: &str) -> impl Future<Output = Result<Result<Describe, DbError>, HostError>> + Send {
        describe(self.0, sql)
    }

    fn query(&self, sql: &str, params: &[Value]) -> impl Future<Output = Result<Result<Vec<Vec<Value>>, DbError>, HostError>> + Send {
        query(self.0, Some(self.1), sql, params)
    }

    fn ends_session(&self, error: &HostError) -> bool {
        self.0.is_closed() || error.0.starts_with(ENDED)
    }
}

/// A statement described over `client`: its parameters' types, and each column's type, table and column number.
async fn describe(client: &Client, sql: &str) -> Result<Result<Describe, DbError>, HostError> {
    match client.prepare(sql).await {
        Ok(stmt) => {
            let params: Vec<u32> = stmt.params().iter().map(Type::oid).collect();
            let columns: Vec<Value> = stmt
                .columns()
                .iter()
                .map(|c| json!({ "name": c.name(), "oid": c.type_().oid(), "table": c.table_oid().unwrap_or(0), "attnum": c.column_id().map_or(0, i32::from), "typmod": c.type_modifier() }))
                .collect();
            serde_json::from_value(json!({ "params": params, "columns": columns })).map(Ok).map_err(|e| HostError(e.to_string()))
        }
        Err(e) => match e.as_db_error() {
            // The session's end rather than the statement's answer: the replay ends as on a
            // lost connection, and nothing is kept as the statement's error.
            Some(_) if ended(&e) => Err(HostError(format!("{ENDED}: {}", pg_message(&e)))),
            // Another session's lock on a table the statement names, waited for until `lock_timeout`:
            // not the statement's answer either, so nothing is kept as its error, and the next
            // analysis asks again.
            Some(d) if *d.code() == ::tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE => Err(HostError(locked(client).await)),
            Some(d) => {
                let position = match d.position() {
                    Some(::tokio_postgres::error::ErrorPosition::Original(p)) => Some(*p as usize),
                    _ => None,
                };
                Ok(Err(DbError { message: d.message().to_string(), code: Some(d.code().code().to_string()), position }))
            }
            // Not the database's answer: the connection, which ends the replay.
            None => Err(HostError(format!("the describe did not reach the server: {e}"))),
        },
    }
}

/// The sessions of the database that hold, or wait for, a lock on a table that keeps a statement from
/// being described: the lock modes that conflict with reading a table or writing its rows. Each with
/// its table, its client, its state and what it runs, the holders first.
const LOCKED_BY: &str = "select l.relation::pg_catalog.regclass::pg_catalog.text, l.pid, a.application_name, a.state, a.query from pg_catalog.pg_locks l left join pg_catalog.pg_stat_activity a on a.pid OPERATOR(pg_catalog.=) l.pid where l.locktype OPERATOR(pg_catalog.=) 'relation' and l.mode OPERATOR(pg_catalog.=) any (array['ShareLock', 'ShareRowExclusiveLock', 'ExclusiveLock', 'AccessExclusiveLock']::pg_catalog.text[]) and l.pid OPERATOR(pg_catalog.<>) pg_catalog.pg_backend_pid() and l.database OPERATOR(pg_catalog.=) (select oid from pg_catalog.pg_database where datname OPERATOR(pg_catalog.=) pg_catalog.current_database()) order by l.granted desc, l.pid limit 3";

/// Why a describe waited out `lock_timeout`, naming the sessions that hold the lock where the server
/// shows them.
async fn locked(client: &Client) -> String {
    let holders = client.query(LOCKED_BY, &[]).await.map(|rows| rows.iter().map(holder).collect::<Vec<_>>()).unwrap_or_default();
    let by = if holders.is_empty() { String::new() } else { format!(": {}", holders.join("; ")) };
    format!("A table this statement names is locked by another session, and the analysis stopped waiting for it at lock_timeout{by}. Run it again once the lock is released")
}

/// One session of [`LOCKED_BY`], as `users by pid 4242 (psql, idle in transaction: alter table users add column email text)`.
fn holder(row: &::tokio_postgres::Row) -> String {
    let table: Option<String> = row.get(0);
    let pid: i32 = row.get(1);
    let about: Vec<String> = [row.get::<_, Option<String>>(2), row.get::<_, Option<String>>(3)].into_iter().flatten().filter(|s| !s.is_empty()).collect();
    let query: String = row.get::<_, Option<String>>(4).unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
    let query = if query.chars().count() > 80 { format!("{}…", query.chars().take(80).collect::<String>()) } else { query };
    let detail = match (about.is_empty(), query.is_empty()) {
        (true, true) => String::new(),
        (false, true) => format!(" ({})", about.join(", ")),
        (true, false) => format!(" ({query})"),
        (false, false) => format!(" ({}: {query})", about.join(", ")),
    };
    format!("{} by pid {pid}{detail}", table.unwrap_or_else(|| "a table".to_string()))
}

/// The rows of a catalog query over `client`, its statement taken from `prepared` where there is one.
async fn query(client: &Client, prepared: Option<&Prepared>, sql: &str, params: &[Value]) -> Result<Result<Vec<Vec<Value>>, DbError>, HostError> {
    let failed = |what: &str, e: ::tokio_postgres::Error| HostError(if ended(&e) { format!("{ENDED}: {}", pg_message(&e)) } else { format!("catalog query {what}: {}\n    {sql}", pg_message(&e)) });
    let stmt = match prepared {
        Some(prepared) => prepared.statement(client, sql).await,
        None => client.prepare(sql).await,
    }
    .map_err(|e| failed("failed to prepare", e))?;
    let mut values: Vec<Box<dyn ToSql + Sync + Send>> = Vec::new();
    for (ty, value) in stmt.params().iter().zip(params) {
        values.push(bind(ty, value).ok_or_else(|| HostError(format!("cannot bind {value} as {ty} for the catalog query\n    {sql}")))?);
    }
    let refs: Vec<&(dyn ToSql + Sync)> = values.iter().map(|v| v.as_ref() as &(dyn ToSql + Sync)).collect();
    let rows = client.query(&stmt, &refs).await.map_err(|e| failed("failed", e))?;
    Ok(Ok(rows.iter().map(|row| (0..row.len()).map(|i| cell(row, i)).collect()).collect()))
}

/// One parameter of a catalog query, by the type the server inferred from the cast in its text.
fn bind(ty: &Type, value: &Value) -> Option<Box<dyn ToSql + Sync + Send>> {
    let items = || value.as_array().cloned().unwrap_or_default();
    Some(match *ty {
        Type::OID_ARRAY => Box::new(items().iter().map(|v| v.as_u64().map(|n| n as u32)).collect::<Option<Vec<u32>>>()?),
        Type::INT2_ARRAY => Box::new(items().iter().map(|v| v.as_i64().map(|n| n as i16)).collect::<Option<Vec<i16>>>()?),
        Type::INT4_ARRAY => Box::new(items().iter().map(|v| v.as_i64().map(|n| n as i32)).collect::<Option<Vec<i32>>>()?),
        Type::INT8_ARRAY => Box::new(items().iter().map(|v| v.as_i64()).collect::<Option<Vec<i64>>>()?),
        Type::TEXT_ARRAY | Type::NAME_ARRAY | Type::VARCHAR_ARRAY => Box::new(items().iter().map(|v| v.as_str().map(String::from)).collect::<Option<Vec<String>>>()?),
        Type::TEXT | Type::VARCHAR | Type::NAME => Box::new(value.as_str().map(String::from).unwrap_or_else(|| value.to_string())),
        Type::OID => Box::new(value.as_u64()? as u32),
        Type::INT2 => Box::new(value.as_i64()? as i16),
        Type::INT4 => Box::new(value.as_i64()? as i32),
        Type::INT8 => Box::new(value.as_i64()?),
        Type::BOOL => Box::new(value.as_bool()?),
        _ => return None,
    })
}

/// One value of a catalog row as JSON, by its column's type.
fn cell(row: &::tokio_postgres::Row, i: usize) -> Value {
    fn array<T: Into<Value>>(items: Option<Vec<Option<T>>>) -> Option<Value> {
        items.map(|items| Value::Array(items.into_iter().map(|v| v.map_or(Value::Null, Into::into)).collect()))
    }
    let value = match *row.columns()[i].type_() {
        Type::BOOL => row.get::<_, Option<bool>>(i).map(Value::from),
        Type::INT2 => row.get::<_, Option<i16>>(i).map(Value::from),
        Type::INT4 => row.get::<_, Option<i32>>(i).map(Value::from),
        Type::INT8 => row.get::<_, Option<i64>>(i).map(Value::from),
        Type::OID => row.get::<_, Option<u32>>(i).map(Value::from),
        Type::TEXT | Type::VARCHAR | Type::NAME | Type::BPCHAR | Type::CHAR => row.get::<_, Option<String>>(i).map(Value::from),
        Type::TEXT_ARRAY | Type::VARCHAR_ARRAY | Type::NAME_ARRAY => array(row.get::<_, Option<Vec<Option<String>>>>(i)),
        Type::INT2_ARRAY => array(row.get::<_, Option<Vec<Option<i16>>>>(i)),
        Type::INT4_ARRAY => array(row.get::<_, Option<Vec<Option<i32>>>>(i)),
        Type::INT8_ARRAY => array(row.get::<_, Option<Vec<Option<i64>>>>(i)),
        Type::OID_ARRAY => array(row.get::<_, Option<Vec<Option<u32>>>>(i)),
        Type::BOOL_ARRAY => array(row.get::<_, Option<Vec<Option<bool>>>>(i)),
        // What the server-side extension answers with: its version, a whole analysis and a round's needs.
        Type::JSON | Type::JSONB => row.get::<_, Option<Value>>(i),
        ref other => panic!("a catalog query answered a column of type {other}, which no host binds"),
    };
    value.unwrap_or(Value::Null)
}

/// How a host error that is the session's end opens: the server sent a FATAL or PANIC error, or one
/// of the connection-exception class 08, and closes the connection (a restart, an administrator's
/// `pg_terminate_backend`, a database dropped).
pub const ENDED: &str = "the server ended the session";

/// Whether `e` is the server ending the session, rather than its answer to what it was asked.
pub fn ended(e: &::tokio_postgres::Error) -> bool {
    e.as_db_error().is_some_and(|d| matches!(d.severity(), "FATAL" | "PANIC") || d.code().code().starts_with("08"))
}

fn pg_message(e: &::tokio_postgres::Error) -> String {
    e.as_db_error().map(|d| format!("{} ({})", d.message(), d.code().code())).unwrap_or_else(|| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The futures' outputs, the futures polled together as a round's needs are.
    async fn together<F: Future>(futures: Vec<F>) -> Vec<F::Output> {
        let mut futures: Vec<_> = futures.into_iter().map(Box::pin).collect();
        let mut outputs: Vec<Option<F::Output>> = futures.iter().map(|_| None).collect();
        std::future::poll_fn(|cx| {
            for (future, output) in futures.iter_mut().zip(outputs.iter_mut()) {
                if output.is_none() {
                    if let std::task::Poll::Ready(value) = future.as_mut().poll(cx) {
                        *output = Some(value);
                    }
                }
            }
            if outputs.iter().all(Option::is_some) { std::task::Poll::Ready(()) } else { std::task::Poll::Pending }
        })
        .await;
        outputs.into_iter().map(|output| output.expect("settled")).collect()
    }

    /// Run against the server `REYLO_TEST_PG` names, skipped where it is not set.
    #[test]
    fn a_catalog_query_asked_several_times_in_one_round_is_prepared_once() {
        let Ok(url) = std::env::var("REYLO_TEST_PG") else {
            eprintln!("skipped: REYLO_TEST_PG is not set");
            return;
        };
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime");
        runtime.block_on(async {
            let (client, connection) = ::tokio_postgres::connect(&url, ::tokio_postgres::NoTls).await.expect("the test server");
            tokio::spawn(connection);
            let prepared = Prepared::default();
            let answers = PreparedAnswers(&client, &prepared);
            // Each answer counts the statements the session holds as it runs: one, from the first
            // round on, however many of the round ask for the same text.
            let sql = "select $1::pg_catalog.int4, (select count(*)::pg_catalog.int4 from pg_catalog.pg_prepared_statements)";
            let params: Vec<Vec<Value>> = (0..10).map(|i| vec![json!(i)]).collect();
            for _ in 0..2 {
                let answered = together(params.iter().map(|p| ConcurrentAnswerer::query(&answers, sql, p)).collect()).await;
                let rows: Vec<Vec<Vec<Value>>> = answered.into_iter().map(|a| a.expect("no host error").expect("no database error")).collect();
                assert_eq!(rows, (0..10).map(|i| vec![vec![json!(i), json!(1)]]).collect::<Vec<_>>());
            }
        });
    }
}

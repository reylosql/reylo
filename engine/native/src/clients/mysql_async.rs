//! The MySQL engine's needs answered over a `mysql_async` connection: a describe from the
//! server's prepare, its column and parameter definitions as mysql2 reports them, and a catalog
//! query with its values written into the text, as mysql2's `query()` writes them.

use crate::{server_errors, AnswerCache, Analysis, AsyncAnswerer, DbError, HostError, ReplayError};
use ::mysql_async::prelude::Queryable;
use ::mysql_async::{Conn, Value as My};
use reylo_engine_core::database::Database;
use reylo_engine_mysql::Mysql;
use serde_json::{json, Value};
use std::future::Future;

/// The error a server answers a lock waited for until `lock_wait_timeout` with.
const LOCK_WAIT_TIMEOUT: u16 = 1205;

/// The error a server answers a statement `KILL QUERY` interrupted with.
const QUERY_INTERRUPTED: u16 = 1317;

/// What a MySQL describe is answered with.
pub type Describe = <Mysql as Database>::Describe;

/// The analysis of `sql`, its needs answered over `conn`.
pub async fn analyze(conn: &mut Conn, sql: &str, cache: &mut AnswerCache<Describe>) -> Result<Analysis, ReplayError<HostError>> {
    crate::replay::run_async::<Mysql, _>(sql, &mut Answers(conn), cache).await
}

/// The engine's needs, answered over a connection.
pub struct Answers<'a>(pub &'a mut Conn);

impl AsyncAnswerer<Describe> for Answers<'_> {
    type Error = HostError;

    fn describe(&mut self, sql: &str) -> impl Future<Output = Result<Result<Describe, DbError>, HostError>> + Send {
        let conn = &mut *self.0;
        async move {
            match conn.prep(sql).await {
                Ok(statement) => {
                    let columns: Vec<Value> = statement.columns().iter().map(|c| json!({ "name": c.name_str(), "type": c.column_type() as u8, "flags": c.flags().bits(), "charset": c.character_set(), "length": c.column_length(), "decimals": c.decimals(), "schema": c.schema_str(), "org_table": c.org_table_str(), "org_name": c.org_name_str() })).collect();
                    let params: Vec<Value> = statement.params().iter().map(|p| json!({ "type": p.column_type() as u8, "flags": p.flags().bits(), "charset": p.character_set() })).collect();
                    conn.close(statement).await.map_err(|e| HostError(e.to_string()))?;
                    serde_json::from_value(json!({ "columns": columns, "params": params })).map(Ok).map_err(|e| HostError(e.to_string()))
                }
                // Another session's metadata lock on a table the statement names, waited for until
                // `lock_wait_timeout` (ER_LOCK_WAIT_TIMEOUT): not the statement's answer, so nothing is
                // kept as its error, and the next analysis asks again.
                Err(::mysql_async::Error::Server(e)) if e.code == LOCK_WAIT_TIMEOUT => Err(HostError("A table this statement names is locked by another session, and the analysis stopped waiting for it at lock_wait_timeout. Run it again once the lock is released: SHOW PROCESSLIST lists the session that holds it".to_string())),
                // A prepare interrupted (ER_QUERY_INTERRUPTED), as by `KILL QUERY` while it waited on
                // such a lock: a prepare runs nothing, so the interruption says nothing of the
                // statement either, and nothing is kept as its error.
                Err(::mysql_async::Error::Server(e)) if e.code == QUERY_INTERRUPTED => Err(HostError(format!("The server interrupted the analysis of this statement: {} ({})", e.message, e.code))),
                // Any other error that is not the statement's, as a deadlock, the server's memory, its
                // limit on prepared statements or a Vitess tablet not serving: the next analysis asks again.
                Err(::mysql_async::Error::Server(e)) if !server_errors::mysql(e.code, Some(&e.state), Some(&e.message)) => Err(HostError(format!("The server failed the analysis of this statement for a reason of its own, so run it again: {} ({})", e.message, e.code))),
                // The server's error number, which the engine names in its message.
                Err(::mysql_async::Error::Server(e)) => Ok(Err(DbError { message: e.message, code: Some(e.code.to_string()), position: None })),
                Err(e) => Err(HostError(format!("the prepare did not reach the server: {e}"))),
            }
        }
    }

    fn query(&mut self, sql: &str, params: &[Value]) -> impl Future<Output = Result<Result<Vec<Vec<Value>>, DbError>, HostError>> + Send {
        let conn = &mut *self.0;
        async move {
            let text = interpolate(sql, params).ok_or_else(|| HostError(format!("cannot write {params:?} into the catalog query\n    {sql}")))?;
            let mut result = conn.query_iter(text).await.map_err(|e| HostError(format!("catalog query failed: {}\n    {sql}", message(&e))))?;
            let types: Vec<u8> = result.columns_ref().iter().map(|c| c.column_type() as u8).collect();
            let mut rows = Vec::new();
            while let Some(row) = result.next().await.map_err(|e| HostError(format!("catalog query failed: {}\n    {sql}", message(&e))))? {
                rows.push(row.unwrap().into_iter().zip(&types).map(|(value, code)| decoded(value, *code)).collect());
            }
            Ok(Ok(rows))
        }
    }
}

/// A catalog query with its values written into the text, as mysql2's `query()` writes them
/// (`sqlstring`): each `?` in turn, a string quoted with its special characters escaped.
pub fn interpolate(sql: &str, params: &[Value]) -> Option<String> {
    let mut out = String::with_capacity(sql.len());
    let mut params = params.iter();
    for c in sql.chars() {
        if c != '?' {
            out.push(c);
            continue;
        }
        match params.next()? {
            Value::Null => out.push_str("NULL"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Number(n) => out.push_str(&n.to_string()),
            Value::String(s) => out.push_str(&quote(s)),
            _ => return None,
        }
    }
    params.next().is_none().then_some(out)
}

/// A string as a MySQL literal, by `sqlstring`'s escapes.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\0' => out.push_str("\\0"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{1a}' => out.push_str("\\Z"),
            '"' => out.push_str("\\\""),
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

/// One value of a text result as mysql2 hands it to the host: a number for the integer and
/// floating types, a string for the rest.
pub fn decoded(value: My, code: u8) -> Value {
    match value {
        My::NULL => Value::Null,
        My::Bytes(bytes) => {
            let text = String::from_utf8_lossy(&bytes).to_string();
            match code {
                1 | 2 | 3 | 8 | 9 | 13 => text.parse::<i64>().map(Value::from).or_else(|_| text.parse::<u64>().map(Value::from)).unwrap_or(Value::String(text)),
                4 | 5 => text.parse::<f64>().map(Value::from).unwrap_or(Value::String(text)),
                _ => Value::String(text),
            }
        }
        My::Int(n) => json!(n),
        My::UInt(n) => json!(n),
        My::Float(f) => json!(f),
        My::Double(f) => json!(f),
        other => Value::String(other.as_sql(true)),
    }
}

/// An error as a message: the server's with its code, any other as it prints itself.
pub fn message(e: &::mysql_async::Error) -> String {
    match e {
        ::mysql_async::Error::Server(e) => format!("{} ({})", e.message, e.code),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_catalog_query_takes_its_values_as_mysql2_writes_them() {
        assert_eq!(interpolate("select find_in_set(t, ?)", &[json!("users,it's\\")]).as_deref(), Some("select find_in_set(t, 'users,it\\'s\\\\')"));
        assert_eq!(interpolate("select ?, ?", &[json!(1), Value::Null]).as_deref(), Some("select 1, NULL"));
        assert_eq!(interpolate("select ?", &[]), None);
        assert_eq!(decoded(My::Bytes(b"12".to_vec()), 8), json!(12));
        assert_eq!(decoded(My::Bytes(b"12".to_vec()), 253), json!("12"));
        assert_eq!(decoded(My::NULL, 253), Value::Null);
    }
}

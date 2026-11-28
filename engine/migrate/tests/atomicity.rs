//! The corpus of SQLite migrations `tests/atomicity.json`, each with the first statement that controls
//! the transaction, held to `atomicity_problem`; the run time's reader, `transactionControl` in
//! src/migration-text.ts, is held to the same file.

use reylo_migrate::{sql::atomicity_problem, Dialect};

#[test]
fn sqlite_transaction_control_is_the_corpus() {
    let corpus: serde_json::Value = serde_json::from_str(include_str!("atomicity.json")).unwrap();
    let mut wrong = Vec::new();
    for case in corpus["cases"].as_array().unwrap() {
        let sql = case["sql"].as_str().unwrap();
        let want = case["control"].as_str();
        let found = atomicity_problem(sql, Dialect::Sqlite).and_then(|why| why.strip_prefix("It has its own ").and_then(|rest| rest.split([' ', ':']).next()).map(str::to_string));
        if found.as_deref() != want {
            wrong.push(format!("{sql:?}: expected {want:?}, found {found:?}"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

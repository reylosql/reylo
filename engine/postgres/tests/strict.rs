//! The engine's lists of Postgres built-ins that are NULL for a NULL argument, held to the
//! server's own catalog, on the server `REYLO_TEST_PG` names (skipped where it is not set): each
//! function of `POSTGRES_STRICT` and each operator of `POSTGRES_STRICT_OPERATORS` is strict
//! (`proisstrict`) in every overload `pg_catalog` has, a cast into a type of `POSTGRES_CASTS` is
//! by a strict function or by none, and each function of `POSTGRES_FIRST_ARG` is NULL for a NULL
//! first argument and for no other.

use reylo_engine_postgres::infer::{POSTGRES_CASTS, POSTGRES_FIRST_ARG, POSTGRES_STRICT, POSTGRES_STRICT_OPERATORS};

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|n| n.to_string()).collect()
}

/// The first column of each row `sql` answers, given `list` as its parameter.
async fn listed(client: &tokio_postgres::Client, sql: &str, list: Vec<String>) -> Vec<String> {
    client.query(sql, &[&list]).await.expect(sql).iter().map(|r| r.get::<_, String>(0)).collect()
}

#[test]
fn every_listed_built_in_is_strict_in_the_catalog() {
    let Ok(url) = std::env::var("REYLO_TEST_PG") else {
        eprintln!("skipped: REYLO_TEST_PG is not set");
        return;
    };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime");
    runtime.block_on(async {
        let (client, connection) = tokio_postgres::connect(&url, tokio_postgres::NoTls).await.expect("the test server");
        let _connection = tokio::spawn(connection);
        let functions = listed(
            &client,
            "select p.oid::pg_catalog.regprocedure::pg_catalog.text from pg_catalog.pg_proc p where p.pronamespace = 'pg_catalog'::pg_catalog.regnamespace and p.proname = any($1) and (not p.proisstrict or p.prokind <> 'f')",
            names(POSTGRES_STRICT),
        )
        .await;
        assert_eq!(functions, Vec::<String>::new(), "overloads of POSTGRES_STRICT that are not strict functions");
        // A name the catalog lacks would pass the checks above, which list what is wrong.
        let missing = listed(&client, "select n from pg_catalog.unnest($1::pg_catalog.text[]) n where not exists (select from pg_catalog.pg_proc p where p.proname = n and p.pronamespace = 'pg_catalog'::pg_catalog.regnamespace)", names(POSTGRES_STRICT).into_iter().chain(names(POSTGRES_FIRST_ARG)).collect()).await;
        assert_eq!(missing, Vec::<String>::new(), "functions pg_catalog does not have");
        let missing = listed(&client, "select n from pg_catalog.unnest($1::pg_catalog.text[]) n where not exists (select from pg_catalog.pg_operator o where o.oprname = n and o.oprnamespace = 'pg_catalog'::pg_catalog.regnamespace)", names(POSTGRES_STRICT_OPERATORS)).await;
        assert_eq!(missing, Vec::<String>::new(), "operators pg_catalog does not have");
        let missing = listed(&client, "select n from pg_catalog.unnest($1::pg_catalog.text[]) n where not exists (select from pg_catalog.pg_type t where t.typname = n and t.typnamespace = 'pg_catalog'::pg_catalog.regnamespace)", POSTGRES_CASTS.iter().map(|(name, _)| name.to_string()).collect()).await;
        assert_eq!(missing, Vec::<String>::new(), "types pg_catalog does not have");
        let operators = listed(
            &client,
            "select o.oid::pg_catalog.regoperator::pg_catalog.text from pg_catalog.pg_operator o join pg_catalog.pg_proc p on p.oid = o.oprcode where o.oprnamespace = 'pg_catalog'::pg_catalog.regnamespace and o.oprname = any($1) and not p.proisstrict",
            names(POSTGRES_STRICT_OPERATORS),
        )
        .await;
        assert_eq!(operators, Vec::<String>::new(), "overloads of POSTGRES_STRICT_OPERATORS that are not strict");
        let casts = listed(
            &client,
            "select c.castfunc::pg_catalog.regprocedure::pg_catalog.text from pg_catalog.pg_cast c join pg_catalog.pg_type t on t.oid = c.casttarget join pg_catalog.pg_proc p on p.oid = c.castfunc where t.typnamespace = 'pg_catalog'::pg_catalog.regnamespace and t.typname = any($1) and not p.proisstrict",
            POSTGRES_CASTS.iter().map(|(name, _)| name.to_string()).collect(),
        )
        .await;
        assert_eq!(casts, Vec::<String>::new(), "casts into a type of POSTGRES_CASTS by a function that is not strict");
        for name in POSTGRES_FIRST_ARG {
            for rest in ["", ", 'a'", ", 'a', NULL", ", NULL, 'b'"] {
                let sql = format!("select pg_catalog.{name}(NULL::pg_catalog.text{rest}) is null");
                // `concat_ws` takes a value after its separator.
                if *name == "concat_ws" && rest.is_empty() {
                    continue;
                }
                assert!(client.query_one(&sql, &[]).await.expect(&sql).get::<_, bool>(0), "{sql}");
            }
            // And never NULL for a first argument that is not: the engine reads the call as non-null there.
            for rest in [", NULL", ", NULL, 'b'", ", 'a', NULL"] {
                for first in ["''", "'x'", "'%s'", "'%s %s'", "'%L'"] {
                    let sql = format!("select pg_catalog.{name}({first}::pg_catalog.text{rest}) is null");
                    // A format the arguments do not fill is an error, which is no NULL.
                    if let Ok(row) = client.query_one(&sql, &[]).await {
                        assert!(!row.get::<_, bool>(0), "{sql}");
                    }
                }
            }
        }
    });
}

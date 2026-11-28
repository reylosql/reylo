//! `STRICT_FUNCTIONS` measured on the server `REYLO_TEST_MYSQL` names (skipped where it is not
//! set): each row's function, at every number of arguments the row covers, with an argument the row
//! names NULL and the others drawn from values chosen to give a value in its place (numbers at their
//! bounds, empty and non-empty text and binary strings, JSON documents and paths, dates, times and
//! time zones, a point, formats and patterns), which it never does; and each row's function NULL
//! for some such call, so that a function the server lacks, or a stored function of the name in its
//! place, fails the row.

use mysql_async::prelude::Queryable;
use reylo_engine_mysql::mysql_nulls::{Strict, STRICT_FUNCTIONS};

const VALUES: &[&str] = &[
    "NULL", "0", "1", "-1", "2", "3", "0.5", "-0.5", "1e308", "9223372036854775807", "-9223372036854775808", "202001", "''", "' '", "'a'", "'abc'", "'A'", "'b'", "'é'", "'a,b'",
    "'%'", "'_'", "'1'", "'0'", "'-1'", "'2.5'", "'%Y'", "'%H'", "'$'", "'$.a'", "'$[0]'", "'$[*]'", "'[1]'", "'{\"a\":1}'", "'null'", "'\"a\"'", "'one'", "'all'",
    "'2020-01-02'", "'2020-01-02 03:04:05'", "'12:00:00'", "'+00:00'", "'SYSTEM'", "'en_US'", "'1.2.3.4'", "'::1'", "x''", "x'00'", "x'0102030405060708090a0b0c0d0e0f10'",
    "'POINT(1 2)'", "st_geomfromtext('POINT(1 2)')", "cast('[1]' as json)", "cast('{\"a\":1}' as json)",
];

/// `k` arguments drawn from [`VALUES`], 200 calls' worth, seeded by the call they are for, so the
/// same on every run whatever the other rows are.
fn draws(k: usize, call: &str) -> Vec<Vec<&'static str>> {
    let mut seed = call.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    let seed = &mut seed;
    (0..200)
        .map(|_| {
            (0..k)
                .map(|_| {
                    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    VALUES[(*seed >> 33) as usize % VALUES.len()]
                })
                .collect()
        })
        .collect()
}

/// The call as SQL: a unit first for `timestampadd` and `timestampdiff`, and an interval second for
/// `date_add` and its kin.
fn call(name: &str, args: &[&str]) -> String {
    match name {
        "timestampadd" | "timestampdiff" => format!("{name}(DAY, {}, {})", args[1], args[2]),
        "date_add" | "date_sub" | "adddate" | "subdate" => format!("{name}({}, interval {} day)", args[0], args[1]),
        _ => format!("{name}({})", args.join(", ")),
    }
}

#[test]
fn each_row_is_null_for_a_null_argument_as_the_server_answers() {
    let Ok(url) = std::env::var("REYLO_TEST_MYSQL") else {
        eprintln!("skipped: REYLO_TEST_MYSQL is not set");
        return;
    };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime");
    runtime.block_on(async {
        let pool = mysql_async::Pool::new(url.as_str());
        let mut conn = pool.get_conn().await.expect("the test server");
        let version: String = conn.query_first("select version()").await.unwrap().unwrap();
        let (mut wrong, mut unanswered) = (Vec::new(), Vec::new());
        for (name, fewest, most, strict) in STRICT_FUNCTIONS {
            let mut answered = false;
            for count in *fewest..=*most {
                let positions: Vec<usize> = match strict {
                    Strict::Every => (0..count).collect(),
                    Strict::At(positions) => positions.iter().copied().filter(|p| *p < count).collect(),
                    Strict::Last => vec![count - 1],
                };
                for i in positions {
                    for mut args in draws(count - 1, &format!("{name} {count} {i}")) {
                        args.insert(i, "NULL");
                        let sql = call(name, &args);
                        // A call the server refuses says nothing.
                        match conn.query_first::<i64, _>(format!("select ({sql}) is null")).await {
                            Ok(Some(1)) => answered = true,
                            Ok(_) => wrong.push(sql),
                            Err(_) => {}
                        }
                    }
                }
            }
            if !answered {
                unanswered.push(*name);
            }
        }
        drop(conn);
        pool.disconnect().await.unwrap();
        assert!(wrong.is_empty(), "MySQL {version} answers these with a value, for a NULL argument the table names: {wrong:#?}");
        assert!(unanswered.is_empty(), "MySQL {version} answers no call of these with NULL: {unanswered:#?}");
    });
}

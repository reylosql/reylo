# reylo-sql

Lossless SQL syntax trees. Every byte of the text belongs to a token or to the trivia before one, so a tree prints back what it read, and invalid SQL still has a tree.

```rust
use reylo_sql::{parse, Dialect};

let tree = parse(Dialect::Postgres, "select id from users where");
assert_eq!(tree.print(), "select id from users where");
assert_eq!(tree.errors[0].message, "syntax error at end of input");
```

| Dialect | Read with | Errors |
| --- | --- | --- |
| Postgres | Tables from Postgres 18.6's `gram.y`, a port of `scan.l` | Postgres's own |
| SQLite | Tables from SQLite 3.53.4's `parse.y`, a port of `tokenize.c` | SQLite's own |
| MySQL | Tables from TiDB's `parser.y`, with the MySQL 8.4 syntax it lacks, a port of TiDB's lexer | MySQL's wording |
| ClickHouse, SQL Server | The dialect's lexer, with statements, clauses and parentheses | What doesn't end or balance |

MySQL's own grammar is GPL, so nothing here is generated or derived from it. Its syntax comes from TiDB's grammar (Apache 2.0), and what TiDB lacks is written from MySQL's reference manual in `gen/mysql_grammar.py`.

## Regenerating

Each `generated` file comes from a pinned upstream version, and is never edited.

```sh
python3 -I engine/syntax/gen/postgres.py --postgres <postgres> --libpg-query <libpg_query> --pg-query <pg_query.rs>
python3 -I engine/syntax/gen/plpgsql.py --postgres <postgres> --libpg-query <libpg_query>
python3 -I engine/syntax/gen/sqlite.py --sqlite <sqlite-src-3530400> --amalgamation engine/sqlite/.sqlite/sqlite-amalgamation-3530400/sqlite3.c
python3 -I engine/syntax/gen/mysql.py --tidb <tidb>
```

Postgres's, PL/pgSQL's and MySQL's need Bison 3.8.2.

# Comparison

Runs every tool that infers types from hand-written SQL, and the ORMs and query builders Prisma, Drizzle, Kysely and TypeORM, on the same queries against real servers, and grades what each one answers. The marks in the docs' comparison come from here: nothing in the table is written by hand.

```sh
./run.sh                    # servers, every tool; writes results.json and RESULTS.md
./run.sh --only sqlc,reylo  # those tools only, merged into the results
```

It needs Homebrew's `postgresql@18` (with the JSON Schema validators) and `mysql@8.4`, or `PG18_BIN` and `MYSQL_BIN`; `sqlc` (`brew install sqlc`), rustup's toolchain and sqlite3. `SQLX_TS_BIN` names a `sqlx-ts` binary; without one, `run.sh` builds its tagged release. The servers take `COMPARE_PG_PORT` (57700) and `COMPARE_MYSQL_PORT` (57701), and `run.sh` stops them at the end.

## How a mark is computed

[probes.ts](probes.ts) holds the schema's queries and the right answer to each, and [grade.ts](grade.ts) turns a tool's answers into a mark. A tool's adapter in [tools/](tools/) runs the tool as a user would and reads back what it wrote: sqlc's Go, the TypeScript that PgTyped, Prisma and sqlx-ts generate, the row type SafeQL's ESLint fix writes, the description sqlx's macros read, and for Reylo the analysis the type check runs on this checkout, read as the types the mapper renders. An ORM's adapter ([orm.ts](tools/orm.ts)) writes each query in the ORM's own API, on the schema its users get from the database (`prisma db pull`, `drizzle-kit pull`, `kysely-codegen`, TypeORM's entities by hand), and the type checker reads back what the ORM types it as.

- A mark is yes when every query on every database the tool supports is right, no when none is, and partly otherwise.
- It is no as well when a tool gives every expression or parameter the same answer, since that reads nothing from the query, even though the answer is right for some.
- An ORM's query that its API cannot say is its raw SQL, typed as the ORM leaves it: a row typed `unknown` or `any` is wrong. Its parameters count as typed where the type check refuses a value of the wrong type, as SafeQL's do.
- Aggregates, window functions, subqueries, WITH queries, set operations, returned rows and enums run for Reylo and the ORMs alone, since only the ORM table shows them. There a column typed `unknown` or `any` is wrong whatever null it admits, and an enum must name its values.
- A tool's cardinality counts only where the tool works it out. A tool that has the user declare it (`:one`) or pick it per call (`fetch_one`) answers no.
- No codegen reads `git status` after a run: nothing to commit is yes, a changed source file is partly, new files are no. A tool with a second way that generates nothing, run for real, is partly.
- Live database runs the tool once with no server to reach, to see whether it needs one, and likewise any setting that has it ask one, which is partly at best, and checks what it says of a function's result and an operator's.

Postgres's rows are seeded and analyzed, so a tool that reads the query plan sees the plan a real table gets. Reylo is this checkout's own: its `src/` and the engine binary the checkout's build finds (`REYLO_ENGINE_PATH`, else `target/release/reylo`). A run also writes `site/lib/compare-results.json`, which the docs' table reads.

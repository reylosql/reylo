# Patches

What `gen/postgres.py` and `gen/plpgsql.py` apply over their pinned inputs, since libpg_query and pg_query.rs have no Postgres 19 yet. Each applies with no fuzz, so a patch that no longer fits stops the generator.

| Patch | Over | Holds |
| --- | --- | --- |
| `libpg_query-postgres-19.patch` | libpg_query at `LIBPG_QUERY` | `protobuf/pg_query.proto`, the node definitions (`src/include/pg_query_enum_defs.c`, `pg_query_outfuncs_defs.c`, `pg_query_outfuncs_conds.c`), the PL/pgSQL type catalog (`src/include/pg_query_pg_type.c`), and the deparser (`src/postgres_deparse.c`) taught Postgres 19's syntax |
| `pg_query-postgres-19.patch` | pg_query.rs at `PG_QUERY` | `src/protobuf.rs`, and the node walker (`src/node_enum.rs`, `src/node_ref.rs`) with Postgres 19's statements |
| `ignore_respect_bare_labels.patch` | `gram.y`, after libpg_query's patches | `IGNORE_LA` and `RESPECT_LA` in `null_treatment`, and `ignore` and `respect` back among the bare column labels, which `src/postgres/scan.rs` reads before `nulls` after a closing parenthesis |

To remake the first two for another Postgres tag, then set `PATCHED_TAG`:

1. Unpack that Postgres release, apply libpg_query's `patches/*.patch` to it, run `./configure` and `make -C src/backend generated-headers`.
2. In a libpg_query checkout at `LIBPG_QUERY`: `ruby scripts/extract_headers.rb <postgres>`, then `ruby scripts/generate_protobuf_and_funcs.rb`.
3. Run `scripts/extract_pg_types.rb` with its input paths pointed at `<postgres>/src/include/catalog/` (`postgres.bki`, `pg_type_d.h`, `pg_collation_d.h`).
4. Teach `src/postgres_deparse.c` the release's new syntax and nodes, and check it against the release's `gram.y`.
5. `git diff -- protobuf/pg_query.proto src/include/pg_query_enum_defs.c src/include/pg_query_outfuncs_conds.c src/include/pg_query_outfuncs_defs.c src/include/pg_query_pg_type.c src/postgres_deparse.c` is `libpg_query-postgres-19.patch`.
6. In a pg_query.rs checkout at `PG_QUERY`: write `src/protobuf.rs` with prost-build 0.13.5 from that `pg_query.proto`, with `type_attribute(".", "#[derive(serde::Serialize)]")` as pg_query.rs's `build.rs` sets it, and add the release's new statements to `src/node_ref.rs` and `src/node_enum.rs`. `git diff -- src/protobuf.rs src/node_enum.rs src/node_ref.rs` is `pg_query-postgres-19.patch`.

Once libpg_query and pg_query.rs support Postgres 19, pin them there and delete the first two patches. The third stays as long as Reylo reads Postgres 14 to 18's statements with 19's grammar.

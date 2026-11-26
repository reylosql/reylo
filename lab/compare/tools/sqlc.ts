// sqlc: `sqlc generate` with sqlc's TypeScript plugin, sqlc-gen-typescript, writes a TypeScript file whose row and argument interfaces are what it inferred. Each statement runs alone, since one error stops a whole run.
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { join, resolve } from "node:path";
import type { Db } from "../probes.ts";
import { failed, gitDelta, gitInit, readTsDeclarations, readTsType, rewrite, sh, workdir, type Answer, type Statement, type Tool } from "./lib.ts";

const schemaDir = resolve(import.meta.dirname, "../schema");
const PLUGIN = { version: "0.1.3", sha256: "287df8f6cc06377d67ad5ba02c9e0f00c585509881434d15ea8bd9fc751a9368" };
const version = `${sh("sqlc", ["version"]).out.trim()}, sqlc-gen-typescript ${PLUGIN.version}`;
const ENGINE: Partial<Record<Db, string>> = { postgres: "postgresql", mysql: "mysql" };
// The plugin's driver for each database. Its released versions have none for SQLite: `better-sqlite3` is on its main branch only.
const DRIVER: Partial<Record<Db, string>> = { postgres: "pg", mysql: "mysql2" };

// The function `q` takes its parameters as `QArgs`, in the statement's order, and returns `QRow`s.
function readQuery(path: string, wantColumns: string[] | undefined): Answer {
  const d = readTsDeclarations(path);
  if (!/export async function q\(/.test(readFileSync(path, "utf8"))) return failed("no function generated");
  const args = Array.isArray(d.QArgs) ? d.QArgs : [];
  const row = Array.isArray(d.QRow) ? d.QRow : [];
  const params = args.map((f) => { const t = readTsType(f.type, d); return { ...t, nullable: t.nullable || f.optional }; });
  const columns = row.map((f, i) => ({ name: wantColumns?.find((c) => c.toLowerCase() === f.name.toLowerCase()) ?? wantColumns?.[i] ?? f.name, ...readTsType(f.type, d) }));
  return { ok: true, params, columns, cardinality: null };
}

// Each statement runs alone, on its own project. `databaseUrl` adds the `database:` setting, which has sqlc ask that server.
async function run(db: Db, statements: Statement[], databaseUrl?: string) {
  const answers: Record<string, Answer> = {};
  let wrote: string[] = [];
  const database = databaseUrl ? `\n    database:\n      uri: ${databaseUrl}` : "";
  for (const s of statements) {
    const dir = workdir("sqlc");
    writeFileSync(join(dir, "schema.sql"), readFileSync(join(schemaDir, `${db}.sql`), "utf8"));
    const sql = rewrite(s.sql, db === "postgres" ? (n) => `$${n}` : () => "?");
    const returns = /^\s*select|returning/i.test(sql);
    writeFileSync(join(dir, "query.sql"), `-- name: Q ${returns ? ":many" : ":exec"}\n${sql};\n`);
    writeFileSync(join(dir, "sqlc.yaml"), `version: "2"\nplugins:\n  - name: ts\n    wasm:\n      url: https://downloads.sqlc.dev/plugin/sqlc-gen-typescript_${PLUGIN.version}.wasm\n      sha256: ${PLUGIN.sha256}\nsql:\n  - engine: ${ENGINE[db]}\n    schema: schema.sql\n    queries: query.sql${database}\n    codegen:\n      - out: db\n        plugin: ts\n        options:\n          runtime: node\n          driver: ${DRIVER[db]}\n`);
    gitInit(dir);
    const generated = sh("sqlc", ["generate"], { cwd: dir });
    const out = join(dir, "db", "query_sql.ts");
    if (generated.status !== 0 || !existsSync(out)) { answers[s.key] = failed(generated.all); continue; }
    answers[s.key] = readQuery(out, s.probe.columns?.map((c) => c.name));
    wrote = gitDelta(dir);
  }
  return { answers, wrote };
}

export const sqlc: Tool = {
  id: "sqlc",
  name: "sqlc",
  version,
  dbs: ["postgres", "mysql"],
  cardinalityNote: "Declared by hand on each query: `:one`, `:many` or `:exec`. The annotation decides the function's return type, and sqlc accepts any of them.",
  optInNote: "By default it reads your schema files and never connects. A `database:` setting asks Postgres for types, but not for nullability.",
  async withDatabaseSetting(statements, urls) {
    return (await run("postgres", statements, urls.postgres)).answers;
  },
  async analyze(db, statements) {
    return run(db, statements);
  },
};

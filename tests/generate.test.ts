// `reylo generate typescript` (src/generate.ts): every .sql file the paths name as one TypeScript
// module, which type-checks with no content mapper and runs with no build step of Reylo's.
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, symlinkSync, writeFileSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { exportName, generate, sqlFiles } from "../src/generate.ts";
import { closeSessions, DRIVERS, pgLive, postgresDatabase, ROOT } from "./helpers.ts";

const CLI = join(ROOT, "src", "cli.ts");
const TSC = join(ROOT, "node_modules", "typescript", "bin", "tsc");
const ENV = { PATH: process.env.PATH, ...(process.env.REYLO_ENGINE_PATH ? { REYLO_ENGINE_PATH: process.env.REYLO_ENGINE_PATH } : {}) };

/** A package that reaches reylo through its node_modules, with `files` written in it and, where `sqlite` is given, a SQLite database its `.env` names, made by that script. */
function project(files: Record<string, string>, options: { sqlite?: string; env?: string; drivers?: string[] } = {}): string {
  const root = realpathSync(mkdtempSync(join(tmpdir(), "reylo-generate-")));
  writeFileSync(join(root, "package.json"), `{ "type": "module" }`);
  mkdirSync(join(root, "node_modules"));
  symlinkSync(ROOT, join(root, "node_modules", "reylo"));
  for (const driver of options.drivers ?? []) symlinkSync(join(DRIVERS, driver), join(root, "node_modules", driver));
  if (options.sqlite !== undefined) {
    writeFileSync(join(root, ".env"), "DATABASE_URL=file:./app.db\n");
    const db = new DatabaseSync(join(root, "app.db"));
    db.exec(options.sqlite);
    db.close();
  }
  if (options.env !== undefined) writeFileSync(join(root, ".env"), options.env);
  for (const [name, text] of Object.entries(files)) {
    mkdirSync(dirname(join(root, name)), { recursive: true });
    writeFileSync(join(root, name), text);
  }
  return root;
}

/** `tsc` over the project's `.ts` files with no content mapper, as TypeScript 5 and 6 read the module: what it prints. */
function typecheck(root: string): string {
  writeFileSync(join(root, "tsconfig.json"), JSON.stringify({ compilerOptions: { target: "esnext", module: "nodenext", strict: true, noEmit: true, skipLibCheck: true, allowImportingTsExtensions: true, erasableSyntaxOnly: true, verbatimModuleSyntax: true, noUnusedLocals: true, typeRoots: [join(ROOT, "node_modules", "@types")], types: ["node"] }, include: ["**/*.ts"], exclude: ["node_modules"] }));
  const r = spawnSync(process.execPath, [TSC, "-p", "tsconfig.json", "--pretty", "false"], { cwd: root, encoding: "utf8" });
  return (r.stdout + r.stderr).trim();
}

function reylo(cwd: string, args: string[]) {
  const r = spawnSync(process.execPath, [CLI, ...args], { cwd, encoding: "utf8", env: ENV });
  return { status: r.status, out: r.stdout, err: r.stderr };
}

const SQLITE = "create table users (id integer primary key, name text not null, bio text); insert into users values (1, 'ada', null), (2, 'grace', 'admiral')";

test("an export is named after its file in camelCase", () => {
  assert.equal(exportName("get_user"), "getUser");
  assert.equal(exportName("get-user"), "getUser");
  assert.equal(exportName("GetUser"), "getUser");
  assert.equal(exportName("getUser"), "getUser");
  assert.equal(exportName("HTTPStatus"), "httpStatus");
  assert.equal(exportName("user2Posts"), "user2Posts");
  assert.equal(exportName("list users.v2"), "listUsersV2");
  assert.equal(exportName("1st"), undefined);
  assert.equal(exportName("é"), undefined);
  assert.equal(exportName("__"), undefined);
});

test("the files a path names: a file, a directory searched, or a glob", () => {
  const root = project({ "src/a.sql": "", "src/users/get.sql": "", "src/posts/list.SQL": "", "src/users/notes.md": "", "src/target/skip.sql": "", "src/.hidden/skip.sql": "", "src/.server/kept.sql": "", "other/x.sql": "" });
  const names = (found: string[] | string) => (typeof found === "string" ? found : found.map((f) => f.slice(root.length + 1).replaceAll("\\", "/")));
  assert.deepEqual(names(sqlFiles("src", root)), ["src/.server/kept.sql", "src/a.sql", "src/posts/list.SQL", "src/users/get.sql"]);
  // A glob's wildcard matches no hidden directory, as a shell's does.
  assert.deepEqual(names(sqlFiles("src/**/*.sql", root)), ["src/a.sql", "src/posts/list.SQL", "src/users/get.sql"]);
  assert.deepEqual(names(sqlFiles("src/u*", root)), ["src/users/get.sql"]);
  assert.deepEqual(names(sqlFiles("other/x.sql", root)), ["other/x.sql"]);
  assert.equal(sqlFiles("nowhere", root), "nowhere does not exist");
  // A glob the Rust generator would refuse is refused, rather than matching nothing.
  assert.equal(sqlFiles("src/[bad", root), "src/[bad is not a glob Reylo reads: invalid range pattern");
  assert.equal(sqlFiles("src/[!]", root), "src/[!] is not a glob Reylo reads: invalid range pattern");
  assert.equal(sqlFiles("src/[a/*.sql]", root), "src/[a/*.sql] is not a glob Reylo reads: invalid range pattern");
  assert.equal(sqlFiles("src/***/*.sql", root), "src/***/*.sql is not a glob Reylo reads: wildcards are either regular `*` or recursive `**`");
  assert.equal(sqlFiles("src/a**.sql", root), "src/a**.sql is not a glob Reylo reads: recursive wildcards must form a single path component");
  assert.deepEqual(names(sqlFiles("src/[a]*.sql", root)), ["src/a.sql"]);
  assert.deepEqual(names(sqlFiles("**/x.sql", root)), ["other/x.sql"]);
});

test("reylo generate typescript writes one module that type-checks with no content mapper and runs with no build step", async () => {
  const root = project(
    {
      "src/queries/users/get_user.sql": "-- One user, by id.\nselect id, name, bio from users where id = :id\n",
      "src/queries/users/list-users.sql": "select id, name from users order by id\n",
      "src/main.ts": [
        `import { DatabaseSync } from "node:sqlite";`,
        `import { getUser, listUsers } from "./queries.ts";`,
        `const db = new DatabaseSync("app.db");`,
        `const user: getUser.Row | null = getUser.runSync(db, { id: 2 });`,
        `const users: listUsers.Row[] = await listUsers.run(db);`,
        `const params: getUser.Params = { id: 1 };`,
        `// @ts-expect-error id is a number`,
        `getUser.runSync(db, { id: "1" });`,
        `// @ts-expect-error a column the statement does not return`,
        `users[0]?.bio;`,
        `console.log(JSON.stringify({ user, users, params, source: getUser.source }));`,
        ``,
      ].join("\n"),
    },
    { sqlite: SQLITE },
  );
  const r = reylo(root, ["generate", "typescript", "src/**/*.sql", "--out", "src/queries.ts"]);
  assert.equal(r.status, 0, r.err);
  assert.equal(r.out, "✓ Wrote src/queries.ts\n");
  const text = readFileSync(join(root, "src", "queries.ts"), "utf8");
  assert.match(text, /^\/\/ Written by `reylo generate typescript` from \*\*\/\*\.sql: edit the queries and run it again, never this file\.\n/);
  // The statement's leading comment documents its export, as it does an import's.
  assert.match(text, /\/\*\*\n \* One user, by id\.\n[\s\S]*\*\/\nexport const getUser = reylo\.defineSqliteQuery\(\{\n  sql: /);
  assert.match(text, /\nexport declare namespace getUser \{\n  type Params = \{ id: number \};\n  type Row = \{ id: number; name: string; bio: string \| null \};\n/);
  assert.equal(typecheck(root), "");
  const run = spawnSync(process.execPath, [join(root, "src", "main.ts")], { cwd: root, encoding: "utf8" });
  assert.equal(run.status, 0, run.stderr);
  assert.deepEqual(JSON.parse(run.stdout), { user: { id: 2, name: "grace", bio: "admiral" }, users: [{ id: 1, name: "ada" }, { id: 2, name: "grace" }], params: { id: 1 }, source: "src/queries/users/get_user.sql" });
  // A second run writes nothing.
  assert.deepEqual(reylo(root, ["generate", "typescript", "src/**/*.sql", "--out", "src/queries.ts"]), { status: 0, out: "» No change to src/queries.ts\n", err: "" });
});

test("reylo generate typescript writes nothing where a name is taken twice or reserved, a statement fails, or the file is not its own", async () => {
  const root = project(
    {
      "q/users/get_user.sql": "select id from users where id = :id\n",
      "q/admin/get-user.sql": "select id from users where id = :id\n",
      "q/delete.sql": "delete from users where id = :id\n",
      "broken/list.sql": "select nme from users\n",
      "mine.ts": "export {};\n",
    },
    { sqlite: SQLITE },
  );
  const names = await generate(["q"], join(root, "out.ts"), root);
  assert.deepEqual(names, { ok: false, errors: ["q/delete.sql: delete is a word JavaScript reserves: rename the file", "q/admin/get-user.sql and q/users/get_user.sql are both the export getUser: rename one"] });
  assert.deepEqual(await generate(["broken"], join(root, "out.ts"), root), { ok: false, errors: ["broken/list.sql:1:8: no such column: nme"] });
  assert.equal(existsSync(join(root, "out.ts")), false);
  assert.deepEqual(await generate(["q/users"], join(root, "mine.ts"), root), { ok: false, errors: ["mine.ts exists and reylo generate did not write it: move it, or name another file with --out"] });
  await closeSessions();
  const usage = reylo(root, ["generate", "typescript", "q/users"]);
  assert.deepEqual(usage, { status: 2, out: "", err: "reylo generate: name the file to write with --out, as --out src/queries.ts (reylo generate typescript --help)\n" });
});

test("Postgres: the module carries JSON shapes and enum types, and runs on a driver", { skip: pgLive }, async () => {
  const setup = "create type mood as enum ('happy', 'sad'); create table people (id int primary key, name text not null, mood mood not null, tags jsonb); insert into people values (1, 'ada', 'happy', '[\"x\"]')";
  const { url, drop } = await postgresDatabase("generate", setup);
  try {
    const root = project(
      {
        "sql/get_person.sql": "select id, name, mood, tags, json_build_object('id', id, 'name', name) as card from people where id = :id\n",
        "main.ts": [
          `import postgres from "postgres";`,
          `import { getPerson } from "./queries.ts";`,
          `const sql = postgres(process.env.DATABASE_URL!);`,
          `const person = await getPerson.run(sql, { id: 1 });`,
          `const mood: "happy" | "sad" | undefined = person?.mood;`,
          `const card: { id: number; name: string } | undefined = person?.card;`,
          `console.log(JSON.stringify({ mood, card, tags: person?.tags }));`,
          `await sql.end();`,
          ``,
        ].join("\n"),
      },
      { env: `DATABASE_URL=${url}\n`, drivers: ["postgres"] },
    );
    const r = reylo(root, ["generate", "typescript", "sql", "--out", "queries.ts"]);
    assert.equal(r.status, 0, r.err);
    assert.equal(typecheck(root), "");
    const run = spawnSync(process.execPath, [join(root, "main.ts")], { cwd: root, encoding: "utf8", env: { ...process.env, DATABASE_URL: url } });
    assert.equal(run.status, 0, run.stderr);
    assert.deepEqual(JSON.parse(run.stdout), { mood: "happy", card: { id: 1, name: "ada" }, tags: ["x"] });
  } finally {
    await drop();
  }
});

test("Postgres: a codec's library is found where the module imports it, not only beside the .sql files", { skip: pgLive }, async () => {
  const { url, drop } = await postgresDatabase("generate_codecs", `create table prices (id int primary key, price numeric(20,2) not null); comment on column prices.price is 'Not import("geojson"), which a geometry is typed with'`);
  try {
    // The queries are one package's and the module another's, beside it.
    const root = project({
      "app/package.json": `{ "type": "module" }`,
      "app/.env": `DATABASE_URL=${url}\n`,
      "app/.reylo.yaml": "js:\n  codecs:\n    decimal: decimal.js\n    geometry: GeoJSON\n",
      "app/sql/get_price.sql": "select price from prices where id = :id\n",
      "web/package.json": `{ "type": "module" }`,
    });
    mkdirSync(join(root, "app", "node_modules"));
    symlinkSync(join(ROOT, "node_modules", "decimal.js"), join(root, "app", "node_modules", "decimal.js"));
    const out = join(root, "web", "queries.ts");
    assert.deepEqual(await generate(["app/sql"], out, root), { ok: false, errors: ["web/queries.ts: js.codecs.decimal is decimal.js in the project file, and no decimal.js is installed where this file can import it: install it (npm install decimal.js), or set js.codecs.decimal to string."] });
    assert.equal(existsSync(out), false);
    mkdirSync(join(root, "web", "node_modules"));
    symlinkSync(join(ROOT, "node_modules", "decimal.js"), join(root, "web", "node_modules", "decimal.js"));
    assert.deepEqual(await generate(["app/sql"], out, root), { ok: true, path: out, changed: true });
    assert.match(readFileSync(out, "utf8"), /^import ReyloDecimal from "decimal\.js";$/m);
    // A column's comment that names GeoJSON's types asks for no @types/geojson.
    assert.match(readFileSync(out, "utf8"), /Not import\("geojson"\)/);
  } finally {
    await closeSessions();
    await drop();
  }
});

// `reylo analyze --format typescript`, the binary's port of the content mapper's renderer, held to
// the renderer itself (src/module-types.ts, src/render.ts, src/type-format.ts) over every statement
// of the oracle's cases and the spec's examples, each analyzed on a database of its own, and with
// the project file's every codec on. The content mapper's own tests check the rest: each statement
// they type is also printed by the binary and compared (`transform` in tests/helpers.ts).
import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { pathToFileURL } from "node:url";
import { Session } from "../src/analyze.ts";
import type { Codecs } from "../src/codecs.ts";
import type { Analysis, Dialect } from "../src/engine-core.ts";
import { nativeBinary } from "../src/native.ts";
import { closeSessions, mysqlDatabase, mysqlLive, pgLive, postgresDatabase, ROOT } from "./helpers.ts";
import { typescriptOf } from "./typescript-format.ts";

/** Every codec of `js.codecs` set to a type other than its default. */
const CODECS: Codecs = { decimal: "number", bytes: "string", date: "Temporal.PlainDate", time: "Temporal.PlainTime", datetime: "Temporal.PlainDateTime", instant: "Temporal.Instant", duration: "Temporal.Duration", set: "Array", vector: "TypedArray", geometry: "GeoJSON", inet: "ip-address" };

/** A package with no project file, and one whose project file sets every codec and lets Postgres arrays nest. */
function projects(): { plain: string; codecs: string } {
  const plain = mkdtempSync(join(tmpdir(), "reylo-ts-plain-"));
  writeFileSync(join(plain, "package.json"), "{}");
  const codecs = mkdtempSync(join(tmpdir(), "reylo-ts-codecs-"));
  writeFileSync(join(codecs, "package.json"), "{}");
  writeFileSync(join(codecs, ".reylo.yaml"), `allow_nested_arrays: true\njs:\n  codecs:\n${Object.entries(CODECS).map(([k, v]) => `    ${k}: ${v}\n`).join("")}`);
  return { plain, codecs };
}

/** What `reylo analyze --format typescript` prints for each file, run in `cwd` on `url`, by the file's path. */
function binary(cwd: string, url: string, files: string[]): Promise<Map<string, string>> {
  return new Promise((resolve, reject) => {
    const child = spawn(nativeBinary(), ["analyze", "--format", "typescript", "--url", url, ...files], { cwd, env: { ...process.env, DATABASE_URL: "", REYLO_CONFIG: "" }, stdio: ["ignore", "pipe", "ignore"] });
    let out = "";
    child.stdout.setEncoding("utf8").on("data", (chunk: string) => (out += chunk));
    child.on("error", reject);
    // `close`, not `exit`: the output is whole only once the pipe has drained.
    child.on("close", (status) => {
      if (status !== 0 && status !== 1) return reject(new Error(`reylo analyze exited with ${status} on ${files[0]}`));
      const printed = new Map<string, string>();
      // One file prints its types alone.
      if (files.length === 1) return resolve(printed.set(files[0]!, out));
      // Several each open with `// <file>`, which no line of a type does, a blank line after the one before.
      for (const block of out.split(/^\/\/ /m).filter(Boolean)) {
        const newline = block.indexOf("\n");
        printed.set(block.slice(0, newline), block.slice(newline + 1).replace(/\n\n$/, "\n"));
      }
      resolve(printed);
    });
  });
}

type Case = { name: string; dialect: Dialect; setup: string; files: string[] };

/** The oracle's cases, each statement a file beside the case's `setup.sql`. */
function oracleCases(): Case[] {
  const dir = join(ROOT, "oracle", "cases");
  return readdirSync(dir).flatMap((name): Case[] => {
    const at = join(dir, name);
    if (!existsSync(join(at, "setup.sql"))) return [];
    const dialect = (existsSync(join(at, "dialect")) ? readFileSync(join(at, "dialect"), "utf8").trim() : "postgres") as Dialect;
    if (!["postgres", "mysql", "sqlite"].includes(dialect)) return [];
    const files = readdirSync(at).filter((f) => f.endsWith(".sql") && f !== "setup.sql").sort().map((f) => join(at, f));
    return files.length ? [{ name, dialect, setup: readFileSync(join(at, "setup.sql"), "utf8"), files }] : [];
  });
}

/** The spec's examples of the databases Reylo runs, each its schema and one statement, written into files of their own. */
function specExamples(): Case[] {
  const examples = JSON.parse(readFileSync(join(ROOT, "spec", "examples.json"), "utf8")) as Record<string, { database: string; setup?: string; statement: string }>;
  const dir = mkdtempSync(join(tmpdir(), "reylo-ts-spec-"));
  return Object.entries(examples).flatMap(([name, e]): Case[] => {
    const dialect = e.database as Dialect;
    if (!["postgres", "mysql", "sqlite"].includes(dialect)) return [];
    const file = join(dir, `${name}.sql`);
    writeFileSync(file, e.statement);
    return [{ name: `spec ${name}`, dialect, setup: e.setup ?? "", files: [file] }];
  });
}

/** A database of the case's own with its setup run, or why it cannot have one here (an extension the server lacks). */
async function databaseOf(c: Case, n: number): Promise<{ url: string; drop(): Promise<void> } | string> {
  try {
    if (c.dialect === "postgres") return await postgresDatabase(`reylo_ts_${n}`, c.setup);
    if (c.dialect === "mysql") return await mysqlDatabase(`reylo_ts_${n}`, c.setup);
    const file = join(mkdtempSync(join(tmpdir(), "reylo-ts-sqlite-")), "app.db");
    const db = new DatabaseSync(file);
    try {
      db.exec(c.setup);
    } finally {
      db.close();
    }
    return { url: pathToFileURL(file).href, drop: async () => {} };
  } catch (e) {
    return (e as Error).message;
  }
}

/** Each case's statements printed by the binary under both projects, against the renderer's text of the content mapper's own analysis of each. */
async function check(cases: Case[]): Promise<{ statements: number; skipped: string[]; differences: string[] }> {
  const { plain, codecs } = projects();
  const skipped: string[] = [];
  const differences: string[] = [];
  let statements = 0;
  let next = 0;
  const work = async () => {
    for (let n = next++; n < cases.length; n = next++) {
      const c = cases[n]!;
      const db = await databaseOf(c, n);
      if (typeof db === "string") {
        skipped.push(`${c.name}: ${db.split("\n")[0]}`);
        continue;
      }
      const session = await Session.open(plain, { ...process.env, DATABASE_URL: db.url, REYLO_CONFIG: "" });
      try {
        const [bare, coded] = await Promise.all([binary(plain, db.url, c.files), binary(codecs, db.url, c.files)]);
        for (const file of c.files) {
          const sql = readFileSync(file, "utf8");
          const outcome = await session.analyze(sql);
          const analysis: Analysis | undefined = outcome.ok ? outcome.analysis : undefined;
          for (const [printed, options] of [[bare, { codecs: {}, allowNestedArrays: false }], [coded, { codecs: CODECS, allowNestedArrays: true }]] as const) {
            const expected = typescriptOf(sql, analysis, c.dialect, options.codecs, options.allowNestedArrays);
            const actual = printed.get(file);
            statements++;
            if (actual !== expected) differences.push(`${c.name}/${file.split("/").at(-1)}${options.allowNestedArrays ? " (codecs)" : ""}:\n--- renderer\n${expected}--- reylo analyze\n${actual ?? "(nothing)\n"}`);
          }
        }
      } finally {
        await session.close();
        await db.drop();
      }
    }
  };
  await Promise.all(Array.from({ length: 6 }, work));
  return { statements, skipped, differences };
}

const live = { postgres: pgLive, mysql: mysqlLive, sqlite: false } as const;

for (const dialect of ["postgres", "mysql", "sqlite"] as const) {
  test(`analyze --format typescript prints each ${dialect} statement of the oracle and the spec as the content mapper declares it`, { skip: live[dialect], timeout: 600_000 }, async (t) => {
    const cases = [...oracleCases(), ...specExamples()].filter((c) => c.dialect === dialect);
    const { statements, skipped, differences } = await check(cases);
    await closeSessions();
    t.diagnostic(`${statements / 2} statements in ${cases.length - skipped.length} of ${cases.length} cases, each with and without codecs; skipped: ${skipped.join("; ") || "none"}`);
    assert.ok(cases.length - skipped.length >= cases.length / 2, `most cases must run here: ${skipped.join("\n")}`);
    assert.equal(differences.length, 0, `${differences.length} statements differ:\n${differences.slice(0, 20).join("\n")}`);
  });
}

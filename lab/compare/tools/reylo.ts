// Reylo, as this checkout builds it: each statement goes through the analysis the type check runs (src/analyze.ts's `Session`, on the engine binary the checkout's build finds), and its parameters and row are read from the TypeScript the mapper renders for them (src/render.ts).
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { Session } from "../../../src/analyze.ts";
import { analysisWithBases } from "../../../src/engine-type.ts";
import { ident, orNull, renderParamType, renderType } from "../../../src/render.ts";
import type { Card } from "../probes.ts";
import { failed, gitDelta, gitInit, readTsType, rewrite, workdir, type Answer, type Tool } from "./lib.ts";

const version: string = JSON.parse(readFileSync(resolve(import.meta.dirname, "../../../package.json"), "utf8")).version;
const CARD: Record<string, Card> = { one: "one", at_most_one: "maybeOne", many: "many", none: "none" };

export const reylo: Tool = {
  id: "reylo",
  name: "Reylo",
  version,
  dbs: ["postgres", "mysql", "sqlite"],
  async analyze(db, statements, urls) {
    const dir = workdir("reylo");
    mkdirSync(join(dir, "q"));
    writeFileSync(join(dir, "package.json"), '{"name":"p","private":true,"type":"module"}\n');
    statements.forEach((s, i) => writeFileSync(join(dir, "q", `s${i}.sql`), `${rewrite(s.sql, (n) => `:p${n}`)}\n`));
    gitInit(dir);
    const url = db === "postgres" ? urls.postgres : db === "mysql" ? urls.mysql : `file:${urls.sqlite}`;
    const answers: Record<string, Answer> = {};
    let session: Session | undefined;
    try {
      session = await Session.open(dir, { ...process.env, DATABASE_URL: url });
      for (const [i, s] of statements.entries()) {
        const outcome = await session.analyze(`${rewrite(s.sql, (n) => `:p${n}`)}\n`).catch((e: Error) => ({ ok: false as const, message: e.message }));
        if (!outcome.ok) { answers[s.key] = failed(`s${i}.sql: ${(outcome as { message: string }).message}`); continue; }
        const a = analysisWithBases(outcome.analysis);
        if (a.diagnostics.length) { answers[s.key] = failed(`s${i}.sql: ${a.diagnostics.map((d) => d.message).join("; ")}`); continue; }
        // Each type as the mapper writes it, and its null as the column's or the parameter's.
        const column = (c: (typeof a.columns)[number]) => (c.nullable ? orNull(renderType(c.type, false, db)) : renderType(c.type, false, db));
        const param = (q: (typeof a.params)[number]) => (q.nullable ? orNull(renderParamType(q.type, db, false, q.members)) : renderParamType(q.type, db, false, q.members));
        const params = a.params.map((q) => ({ ...readTsType(param(q)), nullable: !!q.nullable }));
        const columns = a.columns.map((c) => ({ name: c.name, ...readTsType(column(c)), nullable: !!c.nullable }));
        const shown = {
          params: a.params.length ? `{ ${a.params.map((q) => `${ident(q.name)}${q.nullable || q.optional ? "?" : ""}: ${param(q)}`).join("; ")} }` : "Record<never, never>",
          row: a.columns.length ? `{ ${a.columns.map((c) => `${ident(c.name)}: ${column(c)}`).join("; ")} }` : "never",
        };
        answers[s.key] = { ok: true, params, columns, cardinality: CARD[a.cardinality] ?? null, shown };
      }
    } catch (e) {
      for (const s of statements) answers[s.key] ??= failed((e as Error).message);
    } finally {
      await session?.close();
    }
    return { answers, wrote: gitDelta(dir) };
  },
};

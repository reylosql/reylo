// Prisma Client: `prisma db pull` writes the database's tables as models in `schema.prisma`, and `prisma generate` writes the client each query is built on. What the client has no query for (a join, most expressions) is `$queryRaw`, which Prisma types `unknown`; its typed SQL is TypedSQL, a column of its own.
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import type { Db } from "../probes.ts";
import { gitDelta, gitInit, sh, workdir } from "./lib.ts";
import { ormTool, projectDir, root, versionOf, type Entry } from "./orm.ts";

const PROVIDER: Record<Db, string> = { postgres: "postgresql", mysql: "mysql", sqlite: "sqlite" };
// The probe's own SQL through `$queryRaw`, each parameter a template value.
const raw = (sql: string) => `prisma.$queryRaw\`${sql.replace(/\$(\d+)/g, "$${$$$1}")}\``;

const queries: Record<string, Entry> = {
  "outer-joins:left": raw,
  "outer-joins:left-sorted": raw,
  "outer-joins:left-where": raw,
  "outer-joins:left-subquery": raw,
  "outer-joins:right": raw,
  "outer-joins:full": raw,

  "expressions:count": { code: "prisma.posts.count()", value: true },
  "expressions:max": { code: "prisma.posts.aggregate({ _max: { id: true } })", pick: "_max.id" },
  "expressions:sum": { code: "prisma.posts.aggregate({ _sum: { score: true } })", pick: "_sum.score" },
  "expressions:coalesce": raw,
  "expressions:concat": raw,
  "expressions:concat-nullable": raw,
  "expressions:upper": raw,
  "expressions:upper-nullable": raw,
  "expressions:add": raw,
  "expressions:add-nullable": raw,
  "expressions:nullif": raw,
  "expressions:case-no-else": raw,
  "expressions:is-null": raw,
  "expressions:subquery-count": raw,
  "expressions:exists": raw,
  "expressions:row-number": raw,
  "expressions:lag": raw,
  "expressions:union": raw,
  "expressions:grouped-max": { code: "prisma.posts.groupBy({ by: ['author_id'], _max: { id: true } })", pick: "_max.id" },

  "parameter-types:eq-int": "prisma.users.findMany({ select: { name: true }, where: { id: $1 } })",
  "parameter-types:eq-text": "prisma.users.findMany({ select: { id: true }, where: { name: $1 } })",
  "parameter-types:insert": "prisma.posts.create({ data: { id: $1, author_id: $2, title: $3, score: $4 } })",
  "parameter-types:limit": "prisma.users.findMany({ select: { id: true }, take: $1 })",
  // Prisma has no LIKE; `contains` is its filter on part of a string.
  "parameter-types:like": "prisma.users.findMany({ select: { id: true }, where: { name: { contains: $1 } } })",
  "parameter-types:uuid": "prisma.users.findMany({ select: { id: true }, where: { external_id: $1 } })",
  "parameter-types:timestamp": "prisma.posts.findMany({ select: { id: true }, where: { created_at: { gt: $1 } } })",

  "parameter-nullability:insert": "prisma.users.create({ data: { id: $1, email: $2, name: $3, nickname: $4 } })",
  "parameter-nullability:update": "prisma.users.update({ where: { id: $2 }, data: { nickname: $1 } })",
  "parameter-nullability:eq-key": "prisma.users.findMany({ select: { name: true }, where: { id: $1 } })",
  // In a Prisma filter, null means `is null`, a query that is right to write: there is no `= $1` that should refuse it.
  "parameter-nullability:eq-nullable": null,

  // Rows nested in a row are a relation's `select`.
  "json-shapes:object": "prisma.posts.findMany({ select: { users: { select: { id: true, name: true } } } })",
  "json-shapes:aggregate": "prisma.users.findMany({ select: { posts: { select: { id: true, title: true } } } })",

  "json-schema:column": "prisma.events.findMany({ select: { payload: true } })",
  "json-schema:parameter": "prisma.events.create({ data: { id: $1, payload: $2 } })",

  "aggregates:grouped-count": { code: "prisma.posts.groupBy({ by: ['author_id'], _count: { _all: true } })", paths: { author_id: "author_id", n: "_count._all" } },
  "aggregates:grouped-min": { code: "prisma.posts.groupBy({ by: ['author_id'], _min: { title: true } })", paths: { author_id: "author_id", v: "_min.title" } },
  "aggregates:having": { code: "prisma.posts.groupBy({ by: ['author_id'], _sum: { score: true }, having: { id: { _count: { gt: 1 } } } })", paths: { author_id: "author_id", v: "_sum.score" } },
  "aggregates:avg": { code: "prisma.posts.aggregate({ _avg: { score: true } })", pick: "_avg.score" },
  // Prisma counts rows, or a column's non-null values, but not its distinct values.
  "aggregates:count-distinct": raw,

  "windows:row-number": raw,
  "windows:rank": raw,
  "windows:running-sum": raw,
  "windows:lag": raw,

  // A relation's row count is `_count`; there is no other column a subquery can compute.
  "subqueries:scalar-count": { code: "prisma.users.findMany({ select: { name: true, _count: { select: { posts: true } } } })", paths: { name: "name", v: "_count.posts" } },
  "subqueries:scalar-max": raw,
  "subqueries:exists": raw,
  "subqueries:in": "prisma.users.findMany({ select: { name: true }, where: { posts: { some: {} } } })",
  "subqueries:derived": raw,

  "with-queries:join": raw,
  "with-queries:left-join": raw,
  "with-queries:recursive": raw,

  "set-operations:union": raw,
  "set-operations:union-nullable": raw,
  "set-operations:except": raw,

  "returning:insert": "prisma.posts.create({ data: { id: $1, author_id: $2, title: $3 }, select: { id: true, score: true } })",
  "returning:update": "prisma.users.update({ where: { id: $2 }, data: { nickname: $1 }, select: { name: true, nickname: true } })",
  "returning:delete": "prisma.posts.delete({ where: { id: $1 }, select: { title: true } })",
  "returning:upsert": "prisma.users.upsert({ where: { id: $1 }, create: { id: $1, email: $2, name: $3 }, update: { name: $3 }, select: { id: true, nickname: true } })",

  "enums:column": "prisma.users.findMany({ select: { role: true } })",
  "enums:derived": raw,

  "smoke:smoke": "prisma.users.findMany({ select: { id: true, name: true } })",
};

export const prisma = ormTool({
  id: "prisma",
  name: "Prisma",
  version: versionOf("prisma"),
  dbs: ["postgres", "mysql", "sqlite"],
  cardinalityNote: "Each call picks how many rows it takes: `findMany` returns an array, and `findUnique` and `findFirst` at most one row.",
  queries,
  raw: /\$queryRaw/,
  project(db, urls) {
    const dir = workdir("prisma");
    projectDir(dir);
    mkdirSync(join(dir, "prisma"));
    writeFileSync(join(dir, "prisma.config.ts"), 'import { defineConfig } from "prisma/config";\nexport default defineConfig({ schema: "prisma/schema.prisma", datasource: { url: process.env.DATABASE_URL! } });\n');
    writeFileSync(join(dir, "prisma/schema.prisma"), `generator client {\n  provider = "prisma-client"\n  output = "../generated/prisma"\n}\ndatasource db {\n  provider = "${PROVIDER[db]}"\n}\n`);
    gitInit(dir);
    const env = { DATABASE_URL: db === "postgres" ? urls.postgres : db === "mysql" ? urls.mysql : `file:${urls.sqlite}` };
    const bin = join(root, "node_modules/.bin/prisma");
    for (const args of [["db", "pull"], ["generate"]]) {
      const r = sh(bin, args, { cwd: dir, env });
      if (r.status !== 0) throw new Error(`prisma ${args.join(" ")} on ${db}: ${r.all}`);
    }
    const wrote = gitDelta(dir);
    const prelude = [`import { PrismaClient } from "./generated/prisma/client";`, `declare const prisma: PrismaClient;`].join("\n");
    return { dir, prelude, wrote };
  },
});

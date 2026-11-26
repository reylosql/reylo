// Drizzle: `drizzle-kit pull` writes the schema and its relations from the database into TypeScript, and each query is built on them. An expression Drizzle has no helper for is its `sql` fragment, untyped as Drizzle leaves it.
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import type { Db } from "../probes.ts";
import { gitDelta, gitInit, sh, workdir } from "./lib.ts";
import { ormTool, projectDir, root, versionOf, type Entry } from "./orm.ts";

const DIALECT: Record<Db, string> = { postgres: "postgresql", mysql: "mysql", sqlite: "sqlite" };
const CLIENT: Record<Db, [string, string]> = { postgres: ["NodePgDatabase", "drizzle-orm/node-postgres"], mysql: ["MySql2Database", "drizzle-orm/mysql2"], sqlite: ["BetterSQLite3Database", "drizzle-orm/better-sqlite3"] };

const bySql = (pg: string, mysql: string) => ({ postgres: pg, sqlite: pg, mysql });
const LEFT = "db.select({ name: users.name, title: posts.title }).from(users).leftJoin(posts, eq(posts.authorId, users.id))";
const queries: Record<string, Entry> = {
  "outer-joins:left": LEFT,
  "outer-joins:left-sorted": `${LEFT}.orderBy(posts.title)`,
  "outer-joins:left-where": `${LEFT}.where(eq(users.id, $1))`,
  "outer-joins:left-narrowed": `${LEFT}.where(gt(posts.score, 0))`,
  "outer-joins:left-subquery": "{ const x = db.select({ authorId: posts.authorId, title: posts.title }).from(posts).as('x'); return db.select({ name: users.name, title: x.title }).from(users).leftJoin(x, eq(x.authorId, users.id)); }",
  "outer-joins:right": "db.select({ name: users.name, title: posts.title }).from(posts).rightJoin(users, eq(users.id, posts.authorId))",
  "outer-joins:full": "db.select({ name: users.name, title: posts.title }).from(users).fullJoin(posts, eq(posts.authorId, users.id))",

  "expressions:count": "db.select({ v: count() }).from(posts)",
  "expressions:max": "db.select({ v: max(posts.id) }).from(posts)",
  "expressions:sum": "db.select({ v: sum(posts.score) }).from(posts)",
  "expressions:coalesce": "db.select({ v: sql`coalesce(${users.nickname}, 'x')` }).from(users)",
  "expressions:concat": bySql("db.select({ v: sql`${users.name} || '!'` }).from(users)", "db.select({ v: sql`concat(${users.name}, '!')` }).from(users)"),
  "expressions:concat-nullable": bySql("db.select({ v: sql`${users.nickname} || '!'` }).from(users)", "db.select({ v: sql`concat(${users.nickname}, '!')` }).from(users)"),
  "expressions:upper": "db.select({ v: sql`upper(${users.name})` }).from(users)",
  "expressions:upper-nullable": "db.select({ v: sql`upper(${users.nickname})` }).from(users)",
  "expressions:add": "db.select({ v: sql`${posts.id} + 1` }).from(posts)",
  "expressions:add-nullable": "db.select({ v: sql`${posts.score} + 1` }).from(posts)",
  "expressions:nullif": "db.select({ v: sql`nullif(${users.name}, 'x')` }).from(users)",
  "expressions:case-no-else": "db.select({ v: sql`case when ${users.id} > 0 then ${users.name} end` }).from(users)",
  "expressions:is-null": "db.select({ v: isNull(users.nickname) }).from(users)",
  "expressions:subquery-count": "db.select({ v: db.$count(posts) }).from(users)",
  "expressions:exists": "db.select({ v: exists(db.select({ one: sql`1` }).from(posts)) }).from(users)",
  "expressions:row-number": "db.select({ v: sql`row_number() over (order by ${users.id})` }).from(users)",
  "expressions:lag": "db.select({ v: sql`lag(${users.id}) over (order by ${users.id})` }).from(users)",
  "expressions:union": "db.select({ v: users.id }).from(users).unionAll(db.select({ v: posts.score }).from(posts))",
  "expressions:grouped-max": "db.select({ v: max(posts.id) }).from(posts).groupBy(posts.authorId)",

  "parameter-types:eq-int": "db.select({ name: users.name }).from(users).where(eq(users.id, $1))",
  "parameter-types:eq-text": "db.select({ id: users.id }).from(users).where(eq(users.name, $1))",
  "parameter-types:insert": "db.insert(posts).values({ id: $1, authorId: $2, title: $3, score: $4 })",
  "parameter-types:limit": "db.select({ id: users.id }).from(users).limit($1)",
  "parameter-types:like": "db.select({ id: users.id }).from(users).where(like(users.name, $1))",
  "parameter-types:uuid": "db.select({ id: users.id }).from(users).where(eq(users.externalId, $1))",
  "parameter-types:timestamp": "db.select({ id: posts.id }).from(posts).where(gt(posts.createdAt, $1))",

  "parameter-nullability:insert": "db.insert(users).values({ id: $1, email: $2, name: $3, nickname: $4 })",
  "parameter-nullability:update": "db.update(users).set({ nickname: $1 }).where(eq(users.id, $2))",
  "parameter-nullability:eq-key": "db.select({ name: users.name }).from(users).where(eq(users.id, $1))",
  "parameter-nullability:eq-nullable": "db.select({ id: users.id }).from(users).where(eq(users.nickname, $1))",

  // A nested object is a nested selection; rows nested in a row are the relational query's `with`.
  "json-shapes:object": "db.select({ v: { id: users.id, name: users.name } }).from(users)",
  "json-shapes:aggregate": "db.query.users.findMany({ columns: {}, with: { posts: { columns: { id: true, title: true } } } })",
  "json-shapes:grouped-aggregate": {
    postgres: "db.select({ name: users.name, v: sql`jsonb_agg(jsonb_build_object('id', ${posts.id}, 'title', ${posts.title}))` }).from(users).innerJoin(posts, eq(posts.authorId, users.id)).groupBy(users.id)",
    mysql: "db.select({ name: users.name, v: sql`json_arrayagg(json_object('id', ${posts.id}, 'title', ${posts.title}))` }).from(users).innerJoin(posts, eq(posts.authorId, users.id)).groupBy(users.id, users.name)",
    sqlite: "db.select({ name: users.name, v: sql`json_group_array(json_object('id', ${posts.id}, 'title', ${posts.title}))` }).from(users).innerJoin(posts, eq(posts.authorId, users.id)).groupBy(users.id)",
  },
  "json-shapes:correlated-rows": "db.select({ name: users.name, v: sql`(select jsonb_agg(p order by p.id) from ${posts} p where p.author_id = ${users.id})` }).from(users)",

  "json-schema:column": "db.select({ payload: events.payload }).from(events)",
  "json-schema:parameter": "db.insert(events).values({ id: $1, payload: $2 })",

  "aggregates:grouped-count": "db.select({ author_id: posts.authorId, n: count() }).from(posts).groupBy(posts.authorId)",
  "aggregates:grouped-min": "db.select({ author_id: posts.authorId, v: min(posts.title) }).from(posts).groupBy(posts.authorId)",
  "aggregates:having": "db.select({ author_id: posts.authorId, v: sum(posts.score) }).from(posts).groupBy(posts.authorId).having(gt(count(), 1))",
  "aggregates:avg": "db.select({ v: avg(posts.score) }).from(posts)",
  "aggregates:count-distinct": "db.select({ v: countDistinct(posts.authorId) }).from(posts)",

  // Drizzle has no window functions: each is its `sql` fragment.
  "windows:row-number": "db.select({ id: posts.id, v: sql`row_number() over (partition by ${posts.authorId} order by ${posts.id})` }).from(posts)",
  "windows:rank": "db.select({ id: posts.id, v: sql`rank() over (order by ${posts.score})` }).from(posts)",
  "windows:running-sum": "db.select({ id: posts.id, v: sql`sum(${posts.score}) over (partition by ${posts.authorId} order by ${posts.id})` }).from(posts)",
  "windows:lag": "db.select({ id: posts.id, v: sql`lag(${posts.title}) over (order by ${posts.id})` }).from(posts)",

  "subqueries:dashboard": {
    postgres: "{ const recent = db.$with('recent').as(db.select({ id: posts.id, title: posts.title }).from(posts).orderBy(desc(posts.id)).limit(5)); return db.with(recent).select({ user_count: db.$count(users), recent_posts: sql`(select coalesce(jsonb_agg(jsonb_build_object('id', r.id, 'title', r.title)), '[]') from ${recent} r)` }).from(sql`(select 1) as _`); }",
    mysql: "{ const recent = db.$with('recent').as(db.select({ id: posts.id, title: posts.title }).from(posts).orderBy(desc(posts.id)).limit(5)); return db.with(recent).select({ user_count: db.$count(users), recent_posts: sql`(select coalesce(json_arrayagg(json_object('id', r.id, 'title', r.title)), json_array()) from ${recent} r)` }).from(sql`(select 1) as _`); }",
    sqlite: "{ const recent = db.$with('recent').as(db.select({ id: posts.id, title: posts.title }).from(posts).orderBy(desc(posts.id)).limit(5)); return db.with(recent).select({ user_count: db.$count(users), recent_posts: sql`(select json_group_array(json_object('id', r.id, 'title', r.title)) from ${recent} r)` }).from(sql`(select 1) as _`); }",
  },
  "subqueries:lateral-latest": "{ const p = db.select({ title: posts.title, score: posts.score }).from(posts).where(eq(posts.authorId, users.id)).orderBy(desc(posts.createdAt)).limit(1).as('p'); return db.select({ name: users.name, title: p.title, score: p.score }).from(users).leftJoinLateral(p, sql`true`); }",
  "subqueries:scalar-count": "db.select({ name: users.name, v: db.$count(posts, eq(posts.authorId, users.id)) }).from(users)",
  "subqueries:scalar-max": "db.select({ name: users.name, v: sql`(select max(${posts.title}) from ${posts} where ${posts.authorId} = ${users.id})` }).from(users)",
  "subqueries:exists": "db.select({ name: users.name, v: exists(db.select({ one: sql`1` }).from(posts).where(eq(posts.authorId, users.id))) }).from(users)",
  "subqueries:in": "db.select({ name: users.name }).from(users).where(inArray(users.id, db.select({ id: posts.authorId }).from(posts)))",
  "subqueries:derived": "{ const x = db.select({ author_id: posts.authorId, n: count().as('n') }).from(posts).groupBy(posts.authorId).as('x'); return db.select({ author_id: x.author_id, n: x.n }).from(x); }",

  "with-queries:join": "{ const counts = db.$with('counts').as(db.select({ authorId: posts.authorId, n: count().as('n') }).from(posts).groupBy(posts.authorId)); return db.with(counts).select({ name: users.name, n: counts.n }).from(users).innerJoin(counts, eq(counts.authorId, users.id)); }",
  "with-queries:left-join": "{ const counts = db.$with('counts').as(db.select({ authorId: posts.authorId, n: count().as('n') }).from(posts).groupBy(posts.authorId)); return db.with(counts).select({ name: users.name, n: counts.n }).from(users).leftJoin(counts, eq(counts.authorId, users.id)); }",
  // Drizzle has no recursive WITH: the query is its `sql` fragment.
  "with-queries:recursive": "db.select({ n: sql`n` }).from(sql`(with recursive nums (n) as (select 1 union all select n + 1 from nums where n < 5) select n from nums) as nums`)",

  "set-operations:union": "db.select({ v: users.name }).from(users).union(db.select({ v: posts.title }).from(posts))",
  "set-operations:union-nullable": "db.select({ v: users.nickname }).from(users).unionAll(db.select({ v: posts.title }).from(posts))",
  "set-operations:union-widening": "db.select({ v: users.id }).from(users).unionAll(db.select({ v: posts.score }).from(posts))",
  "set-operations:except": "db.select({ v: users.id }).from(users).except(db.select({ v: posts.authorId }).from(posts))",

  "returning:insert": "db.insert(posts).values({ id: $1, authorId: $2, title: $3 }).returning({ id: posts.id, score: posts.score })",
  "returning:update": "db.update(users).set({ nickname: $1 }).where(eq(users.id, $2)).returning({ name: users.name, nickname: users.nickname })",
  "returning:update-narrowed": "db.update(users).set({ nickname: 'anon' }).where(eq(users.id, $1)).returning({ nickname: users.nickname })",
  "returning:delete": "db.delete(posts).where(eq(posts.id, $1)).returning({ title: posts.title })",
  "returning:upsert": "db.insert(users).values({ id: $1, email: $2, name: $3 }).onConflictDoUpdate({ target: users.id, set: { name: sql`excluded.name` } }).returning({ id: users.id, nickname: users.nickname })",

  "enums:column": "db.select({ role: users.role }).from(users)",
  "enums:derived": "{ const x = db.select({ r: users.role }).from(users).as('x'); return db.select({ r: x.r }).from(x); }",
  "enums:check-union": "db.select({ status: tickets.status }).from(tickets)",
  "enums:literal-union": "db.select({ kind: sql`'user'`.as('kind'), label: users.name }).from(users).unionAll(db.select({ kind: sql`'post'`.as('kind'), label: posts.title }).from(posts))",

  "smoke:smoke": "db.select({ id: users.id, name: users.name }).from(users)",
};

export const drizzle = ormTool({
  id: "drizzle",
  name: "Drizzle",
  version: versionOf("drizzle-orm"),
  dbs: ["postgres", "mysql", "sqlite"],
  cardinalityNote: "Each call picks how many rows it takes: a query returns an array, and `findFirst` at most one row.",
  queries,
  raw: /sql`/,
  project(db, urls) {
    const dir = workdir("drizzle");
    projectDir(dir);
    const credentials = db === "sqlite" ? { url: urls.sqlite } : { url: db === "postgres" ? urls.postgres : urls.mysql };
    writeFileSync(join(dir, "drizzle.config.ts"), `import { defineConfig } from "drizzle-kit";\nexport default defineConfig({ dialect: "${DIALECT[db]}", out: "./db", dbCredentials: ${JSON.stringify(credentials)} });\n`);
    gitInit(dir);
    const pull = sh(join(root, "node_modules/.bin/drizzle-kit"), ["pull"], { cwd: dir });
    if (pull.status !== 0) throw new Error(`drizzle-kit pull on ${db}: ${pull.all}`);
    const wrote = gitDelta(dir);
    const [type, from] = CLIENT[db];
    const tables = db === "postgres" ? "users, posts, events, tickets" : "users, posts";
    const prelude = [
      `import { eq, gt, desc, like, isNull, exists, inArray, count, countDistinct, max, min, sum, avg, sql } from "drizzle-orm";`,
      `import type { ${type} } from "${from}";`,
      `import * as schema from "./db/schema";`,
      `import * as relations from "./db/relations";`,
      `import { ${tables} } from "./db/schema";`,
      `declare const db: ${type}<typeof schema & typeof relations>;`,
    ].join("\n");
    return { dir, prelude, wrote };
  },
});

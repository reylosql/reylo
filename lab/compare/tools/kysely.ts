// Kysely: `kysely-codegen` writes the database's tables as TypeScript interfaces, and each query is built on them. A function Kysely has no typed helper for is its `eb.fn(…)`, untyped as Kysely leaves it.
import { join } from "node:path";
import type { Db } from "../probes.ts";
import { gitDelta, gitInit, sh, workdir } from "./lib.ts";
import { ormTool, projectDir, root, versionOf, type Entry } from "./orm.ts";

const HELPERS: Record<Db, string> = { postgres: "postgres", mysql: "mysql", sqlite: "sqlite" };

const LEFT = "db.selectFrom('users as u').leftJoin('posts as p', 'p.author_id', 'u.id').select(['u.name', 'p.title'])";
const one = (expr: string, from = "users") => `db.selectFrom('${from}').select((eb) => ${expr}.as('v')).execute()`;
const noFrom = (expr: string) => `db.selectNoFrom((eb) => ${expr}.as('v')).execute()`;
const queries: Record<string, Entry> = {
  "outer-joins:left": `${LEFT}.execute()`,
  "outer-joins:left-sorted": `${LEFT}.orderBy('p.title').execute()`,
  "outer-joins:left-where": `${LEFT}.where('u.id', '=', $1).execute()`,
  "outer-joins:left-narrowed": `${LEFT}.where('p.score', '>', 0).execute()`,
  "outer-joins:left-subquery": "db.selectFrom('users as u').leftJoin(db.selectFrom('posts').select(['author_id', 'title']).as('x'), 'x.author_id', 'u.id').select(['u.name', 'x.title']).execute()",
  "outer-joins:right": "db.selectFrom('posts as p').rightJoin('users as u', 'u.id', 'p.author_id').select(['u.name', 'p.title']).execute()",
  "outer-joins:full": "db.selectFrom('users as u').fullJoin('posts as p', 'p.author_id', 'u.id').select(['u.name', 'p.title']).execute()",

  "expressions:count": one("eb.fn.countAll()", "posts"),
  "expressions:max": one("eb.fn.max('id')", "posts"),
  "expressions:sum": one("eb.fn.sum('score')", "posts"),
  "expressions:coalesce": one("eb.fn.coalesce('nickname', eb.val('x'))"),
  "expressions:concat": { postgres: one("eb('name', '||', '!')"), sqlite: one("eb('name', '||', '!')"), mysql: one("eb.fn('concat', ['name', eb.val('!')])") },
  "expressions:concat-nullable": { postgres: one("eb('nickname', '||', '!')"), sqlite: one("eb('nickname', '||', '!')"), mysql: one("eb.fn('concat', ['nickname', eb.val('!')])") },
  "expressions:upper": one("eb.fn('upper', ['name'])"),
  "expressions:upper-nullable": one("eb.fn('upper', ['nickname'])"),
  "expressions:add": one("eb('id', '+', 1)", "posts"),
  "expressions:add-nullable": one("eb('score', '+', 1)", "posts"),
  "expressions:nullif": one("eb.fn('nullif', ['name', eb.val('x')])"),
  "expressions:case-no-else": one("eb.case().when('id', '>', 0).then(eb.ref('name')).end()"),
  "expressions:is-null": one("eb('nickname', 'is', null)"),
  "expressions:subquery-count": noFrom("eb.selectFrom('posts').select(eb.fn.countAll().as('c'))"),
  "expressions:exists": noFrom("eb.exists(eb.selectFrom('posts').select(eb.lit(1).as('one')))"),
  "expressions:row-number": one("eb.fn.agg('row_number').over((ob) => ob.orderBy('id'))"),
  "expressions:lag": one("eb.fn.agg('lag', ['id']).over((ob) => ob.orderBy('id'))"),
  "expressions:union": "db.selectFrom('users').select('id as v').unionAll(db.selectFrom('posts').select('score as v')).execute()",
  "expressions:grouped-max": "db.selectFrom('posts').select((eb) => eb.fn.max('id').as('v')).groupBy('author_id').execute()",

  "parameter-types:eq-int": "db.selectFrom('users').select('name').where('id', '=', $1).execute()",
  "parameter-types:eq-text": "db.selectFrom('users').select('id').where('name', '=', $1).execute()",
  "parameter-types:insert": "db.insertInto('posts').values({ id: $1, author_id: $2, title: $3, score: $4 }).execute()",
  "parameter-types:limit": "db.selectFrom('users').select('id').limit($1).execute()",
  "parameter-types:like": "db.selectFrom('users').select('id').where('name', 'like', $1).execute()",
  "parameter-types:uuid": "db.selectFrom('users').select('id').where('external_id', '=', $1).execute()",
  "parameter-types:timestamp": "db.selectFrom('posts').select('id').where('created_at', '>', $1).execute()",

  "parameter-nullability:insert": "db.insertInto('users').values({ id: $1, email: $2, name: $3, nickname: $4 }).execute()",
  "parameter-nullability:update": "db.updateTable('users').set({ nickname: $1 }).where('id', '=', $2).execute()",
  "parameter-nullability:eq-key": "db.selectFrom('users').select('name').where('id', '=', $1).execute()",
  "parameter-nullability:eq-nullable": "db.selectFrom('users').select('id').where('nickname', '=', $1).execute()",

  "json-shapes:object": one("jsonBuildObject({ id: eb.ref('id'), name: eb.ref('name') })"),
  "json-shapes:aggregate": noFrom("jsonArrayFrom(eb.selectFrom('posts').select(['id', 'title']))"),
  "json-shapes:grouped-aggregate": {
    postgres: "db.selectFrom('users as u').innerJoin('posts as p', 'p.author_id', 'u.id').select((eb) => ['u.name', eb.fn.jsonAgg(jsonBuildObject({ id: eb.ref('p.id'), title: eb.ref('p.title') })).as('v')]).groupBy('u.id').execute()",
    mysql: "db.selectFrom('users as u').innerJoin('posts as p', 'p.author_id', 'u.id').select((eb) => ['u.name', eb.fn('json_arrayagg', [jsonBuildObject({ id: eb.ref('p.id'), title: eb.ref('p.title') })]).as('v')]).groupBy(['u.id', 'u.name']).execute()",
    sqlite: "db.selectFrom('users as u').innerJoin('posts as p', 'p.author_id', 'u.id').select((eb) => ['u.name', eb.fn('json_group_array', [jsonBuildObject({ id: eb.ref('p.id'), title: eb.ref('p.title') })]).as('v')]).groupBy('u.id').execute()",
  },
  "json-shapes:correlated-rows": "db.selectFrom('users as u').select((eb) => ['u.name', eb.selectFrom('posts as p').select((sub) => sub.fn.jsonAgg('p').as('a')).whereRef('p.author_id', '=', 'u.id').as('v')]).execute()",

  "json-schema:column": "db.selectFrom('events').select('payload').execute()",
  "json-schema:parameter": "db.insertInto('events').values({ id: $1, payload: $2 }).execute()",

  "aggregates:grouped-count": "db.selectFrom('posts').select((eb) => ['author_id', eb.fn.countAll().as('n')]).groupBy('author_id').execute()",
  "aggregates:grouped-min": "db.selectFrom('posts').select((eb) => ['author_id', eb.fn.min('title').as('v')]).groupBy('author_id').execute()",
  "aggregates:having": "db.selectFrom('posts').select((eb) => ['author_id', eb.fn.sum('score').as('v')]).groupBy('author_id').having((eb) => eb.fn.countAll(), '>', 1).execute()",
  "aggregates:avg": one("eb.fn.avg('score')", "posts"),
  "aggregates:count-distinct": one("eb.fn.count('author_id').distinct()", "posts"),

  "windows:row-number": "db.selectFrom('posts').select((eb) => ['id', eb.fn.agg('row_number').over((ob) => ob.partitionBy('author_id').orderBy('id')).as('v')]).execute()",
  "windows:rank": "db.selectFrom('posts').select((eb) => ['id', eb.fn.agg('rank').over((ob) => ob.orderBy('score')).as('v')]).execute()",
  "windows:running-sum": "db.selectFrom('posts').select((eb) => ['id', eb.fn.sum('score').over((ob) => ob.partitionBy('author_id').orderBy('id')).as('v')]).execute()",
  "windows:lag": "db.selectFrom('posts').select((eb) => ['id', eb.fn.agg('lag', ['title']).over((ob) => ob.orderBy('id')).as('v')]).execute()",

  "subqueries:dashboard": "db.with('recent', (db) => db.selectFrom('posts').select(['id', 'title']).orderBy('id', 'desc').limit(5)).selectNoFrom((eb) => [eb.selectFrom('users').select(eb.fn.countAll().as('c')).as('user_count'), jsonArrayFrom(eb.selectFrom('recent').select(['id', 'title'])).as('recent_posts')]).execute()",
  "subqueries:lateral-latest": "db.selectFrom('users as u').leftJoinLateral((eb) => eb.selectFrom('posts').select(['title', 'score']).whereRef('author_id', '=', 'u.id').orderBy('created_at', 'desc').limit(1).as('p'), (join) => join.onTrue()).select(['u.name', 'p.title', 'p.score']).execute()",
  "subqueries:scalar-count": "db.selectFrom('users as u').select((eb) => ['u.name', eb.selectFrom('posts as p').select(eb.fn.countAll().as('c')).whereRef('p.author_id', '=', 'u.id').as('v')]).execute()",
  "subqueries:scalar-max": "db.selectFrom('users as u').select((eb) => ['u.name', eb.selectFrom('posts as p').select((sub) => sub.fn.max('p.title').as('m')).whereRef('p.author_id', '=', 'u.id').as('v')]).execute()",
  "subqueries:exists": "db.selectFrom('users as u').select((eb) => ['u.name', eb.exists(eb.selectFrom('posts as p').select(eb.lit(1).as('one')).whereRef('p.author_id', '=', 'u.id')).as('v')]).execute()",
  "subqueries:in": "db.selectFrom('users').select('name').where('id', 'in', db.selectFrom('posts').select('author_id')).execute()",
  "subqueries:derived": "db.selectFrom(db.selectFrom('posts').select((eb) => ['author_id', eb.fn.countAll().as('n')]).groupBy('author_id').as('x')).select(['x.author_id', 'x.n']).execute()",

  "with-queries:join": "db.with('counts', (db) => db.selectFrom('posts').select((eb) => ['author_id', eb.fn.countAll().as('n')]).groupBy('author_id')).selectFrom('users as u').innerJoin('counts as c', 'c.author_id', 'u.id').select(['u.name', 'c.n']).execute()",
  "with-queries:left-join": "db.with('counts', (db) => db.selectFrom('posts').select((eb) => ['author_id', eb.fn.countAll().as('n')]).groupBy('author_id')).selectFrom('users as u').leftJoin('counts as c', 'c.author_id', 'u.id').select(['u.name', 'c.n']).execute()",
  "with-queries:recursive": "db.withRecursive('nums(n)', (db) => db.selectNoFrom((eb) => eb.lit(1).as('n')).unionAll((db) => db.selectFrom('nums').select((eb) => eb('n', '+', 1).as('n')).where('n', '<', 5))).selectFrom('nums').select('n').execute()",

  "set-operations:union": "db.selectFrom('users').select('name as v').union(db.selectFrom('posts').select('title as v')).execute()",
  "set-operations:union-nullable": "db.selectFrom('users').select('nickname as v').unionAll(db.selectFrom('posts').select('title as v')).execute()",
  "set-operations:union-widening": "db.selectFrom('users').select('id as v').unionAll(db.selectFrom('posts').select('score as v')).execute()",
  "set-operations:except": "db.selectFrom('users').select('id as v').except(db.selectFrom('posts').select('author_id as v')).execute()",

  "returning:insert": "db.insertInto('posts').values({ id: $1, author_id: $2, title: $3 }).returning(['id', 'score']).execute()",
  "returning:update": "db.updateTable('users').set({ nickname: $1 }).where('id', '=', $2).returning(['name', 'nickname']).execute()",
  "returning:update-narrowed": "db.updateTable('users').set({ nickname: 'anon' }).where('id', '=', $1).returning('nickname').execute()",
  "returning:delete": "db.deleteFrom('posts').where('id', '=', $1).returning('title').execute()",
  "returning:upsert": "db.insertInto('users').values({ id: $1, email: $2, name: $3 }).onConflict((oc) => oc.column('id').doUpdateSet((eb) => ({ name: eb.ref('excluded.name') }))).returning(['id', 'nickname']).execute()",

  "enums:column": "db.selectFrom('users').select('role').execute()",
  "enums:derived": "db.selectFrom(db.selectFrom('users').select('role as r').as('x')).select('x.r').execute()",
  "enums:check-union": "db.selectFrom('tickets').select('status').execute()",
  "enums:literal-union": "db.selectFrom('users').select(['name as label', sql.lit('user').as('kind')]).unionAll(db.selectFrom('posts').select(['title as label', sql.lit('post').as('kind')])).execute()",

  "smoke:smoke": "db.selectFrom('users').select(['id', 'name']).execute()",
};

export const kysely = ormTool({
  id: "kysely",
  name: "Kysely",
  version: versionOf("kysely"),
  dbs: ["postgres", "mysql", "sqlite"],
  cardinalityNote: "Each call picks how many rows it takes: `execute` returns an array, and `executeTakeFirst` at most one row.",
  queries,
  raw: /eb\.fn\(|eb\.fn\.agg\(|sql`/,
  project(db, urls) {
    const dir = workdir("kysely");
    projectDir(dir);
    gitInit(dir);
    const url = db === "postgres" ? urls.postgres : db === "mysql" ? urls.mysql : urls.sqlite;
    const gen = sh(join(root, "node_modules/.bin/kysely-codegen"), ["--dialect", db, "--url", url, "--out-file", "db.d.ts"], { cwd: dir });
    if (gen.status !== 0) throw new Error(`kysely-codegen on ${db}: ${gen.all}`);
    const wrote = gitDelta(dir);
    const prelude = [
      `import { Kysely, sql } from "kysely";`,
      `import { jsonArrayFrom, jsonBuildObject } from "kysely/helpers/${HELPERS[db]}";`,
      `import type { DB } from "./db";`,
      `declare const db: Kysely<DB>;`,
    ].join("\n");
    return { dir, prelude, wrote };
  },
});

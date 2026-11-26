// TypeORM: the tables are entity classes written by hand, as TypeORM's users write them (it has no command that writes them from a database), and each query is built on them. A query its typed `find` options cannot say goes through the query builder's `getRawMany()` or `query()`, which TypeORM types `any`.
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import type { Db } from "../probes.ts";
import { gitDelta, gitInit, workdir } from "./lib.ts";
import { ormTool, projectDir, versionOf, type Entry } from "./orm.ts";

// The schema's tables as entities, each column typed as the table declares it.
const entities = (db: Db) => `import { Entity, PrimaryColumn, Column, CreateDateColumn, ManyToOne, OneToMany, JoinColumn } from "typeorm";

@Entity("users")
export class User {
  @PrimaryColumn() id!: number;
  @Column({ unique: true }) email!: string;
  @Column() name!: string;
  @Column({ type: "text", nullable: true }) nickname!: string | null;
${db === "postgres" ? `  @Column({ name: "external_id", type: "uuid", nullable: true }) externalId!: string | null;\n` : ""}${db === "sqlite" ? "" : `  @Column({ type: "enum", enum: ["admin", "member"], default: "member" }) role!: "admin" | "member";\n`}  @OneToMany(() => Post, (post) => post.author) posts!: Post[];
}

@Entity("posts")
export class Post {
  @PrimaryColumn() id!: number;
  @Column({ name: "author_id" }) authorId!: number;
  @ManyToOne(() => User, (user) => user.posts) @JoinColumn({ name: "author_id" }) author!: User;
  @Column() title!: string;
  @Column({ type: "int", nullable: true }) score!: number | null;
  @CreateDateColumn({ name: "created_at" }) createdAt!: Date;
}
${db === "postgres" ? `
@Entity("events")
export class Event {
  @PrimaryColumn() id!: number;
  @Column({ type: "jsonb" }) payload!: object;
}

@Entity("tickets")
export class Ticket {
  @PrimaryColumn() id!: number;
  @Column() status!: string;
}
` : ""}`;

// The probe's own SQL through `query()`, each parameter in the driver's placeholder.
const raw = (sql: string) => `ds.query(${JSON.stringify(sql)}, [])`;
const users = "ds.getRepository(User)";
const posts = "ds.getRepository(Post)";
const LEFT = `${users}.createQueryBuilder('u').leftJoin('u.posts', 'p').select(['u.name', 'p.title'])`;

const queries: Record<string, Entry> = {
  "outer-joins:left": `${LEFT}.getRawMany()`,
  "outer-joins:left-sorted": `${LEFT}.orderBy('p.title').getRawMany()`,
  "outer-joins:left-where": `${LEFT}.where('u.id = :id', { id: $1 }).getRawMany()`,
  "outer-joins:left-narrowed": `${LEFT}.where('p.score > 0').getRawMany()`,
  "outer-joins:left-subquery": `${users}.createQueryBuilder('u').leftJoin((qb) => qb.select(['author_id', 'title']).from(Post, 'posts'), 'x', 'x.author_id = u.id').select(['u.name', 'x.title']).getRawMany()`,
  // The query builder has no right or full join.
  "outer-joins:right": raw,
  "outer-joins:full": raw,

  "expressions:count": { code: `${posts}.count()`, value: true },
  "expressions:max": { code: `${posts}.maximum('id')`, value: true },
  "expressions:sum": { code: `${posts}.sum('score')`, value: true },
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
  "expressions:grouped-max": `${posts}.createQueryBuilder('p').select('max(p.id)', 'v').groupBy('p.author_id').getRawMany()`,

  "parameter-types:eq-int": `${users}.find({ select: { name: true }, where: { id: $1 } })`,
  "parameter-types:eq-text": `${users}.find({ select: { id: true }, where: { name: $1 } })`,
  "parameter-types:insert": `${posts}.insert({ id: $1, authorId: $2, title: $3, score: $4 })`,
  "parameter-types:limit": `${users}.find({ select: { id: true }, take: $1 })`,
  "parameter-types:like": `${users}.find({ select: { id: true }, where: { name: Like($1) } })`,
  "parameter-types:uuid": `${users}.find({ select: { id: true }, where: { externalId: $1 } })`,
  "parameter-types:timestamp": `${posts}.find({ select: { id: true }, where: { createdAt: MoreThan($1) } })`,

  "parameter-nullability:insert": `${users}.insert({ id: $1, email: $2, name: $3, nickname: $4 })`,
  "parameter-nullability:update": `${users}.update({ id: $2 }, { nickname: $1 })`,
  "parameter-nullability:eq-key": `${users}.find({ select: { name: true }, where: { id: $1 } })`,
  "parameter-nullability:eq-nullable": `${users}.find({ select: { id: true }, where: { nickname: $1 } })`,

  // Rows nested in a row are a relation the `find` loads.
  "json-shapes:object": { code: `${posts}.find({ relations: { author: true }, select: { author: { id: true, name: true } } })`, pick: "author" },
  "json-shapes:aggregate": { code: `${users}.find({ relations: { posts: true }, select: { posts: { id: true, title: true } } })`, pick: "posts" },
  "json-shapes:grouped-aggregate": raw,
  "json-shapes:correlated-rows": raw,

  "json-schema:column": `ds.getRepository(Event).find({ select: { payload: true } })`,
  "json-schema:parameter": `ds.getRepository(Event).insert({ id: $1, payload: $2 })`,

  // TypeORM's typed `find` has no grouping or aggregate but `count`, `sum`, `maximum` and their kin over a whole table: a grouped query is the query builder's `getRawMany()`.
  "aggregates:grouped-count": `${posts}.createQueryBuilder('p').select('p.author_id', 'author_id').addSelect('count(*)', 'n').groupBy('p.author_id').getRawMany()`,
  "aggregates:grouped-min": `${posts}.createQueryBuilder('p').select('p.author_id', 'author_id').addSelect('min(p.title)', 'v').groupBy('p.author_id').getRawMany()`,
  "aggregates:having": `${posts}.createQueryBuilder('p').select('p.author_id', 'author_id').addSelect('sum(p.score)', 'v').groupBy('p.author_id').having('count(*) > 1').getRawMany()`,
  "aggregates:avg": { code: `${posts}.average('score')`, value: true },
  "aggregates:count-distinct": `${posts}.createQueryBuilder('p').select('count(distinct p.author_id)', 'v').getRawMany()`,

  "windows:row-number": raw,
  "windows:rank": raw,
  "windows:running-sum": raw,
  "windows:lag": raw,

  "subqueries:dashboard": raw,
  "subqueries:lateral-latest": raw,
  "subqueries:scalar-count": `${users}.createQueryBuilder('u').select('u.name', 'name').addSelect((qb) => qb.select('count(*)').from(Post, 'p').where('p.author_id = u.id'), 'v').getRawMany()`,
  "subqueries:scalar-max": `${users}.createQueryBuilder('u').select('u.name', 'name').addSelect((qb) => qb.select('max(p.title)').from(Post, 'p').where('p.author_id = u.id'), 'v').getRawMany()`,
  "subqueries:exists": raw,
  "subqueries:in": `${users}.createQueryBuilder('u').select(['u.name']).where((qb) => 'u.id in ' + qb.subQuery().select('p.author_id').from(Post, 'p').getQuery()).getMany()`,
  "subqueries:derived": `ds.createQueryBuilder().select(['x.author_id', 'x.n']).from((qb) => qb.select('p.author_id', 'author_id').addSelect('count(*)', 'n').from(Post, 'p').groupBy('p.author_id'), 'x').getRawMany()`,

  "with-queries:join": raw,
  "with-queries:left-join": raw,
  "with-queries:recursive": raw,

  "set-operations:union": raw,
  "set-operations:union-nullable": raw,
  "set-operations:union-widening": raw,
  "set-operations:except": raw,

  // \`save\` inserts or updates by primary key and returns the entity, which is TypeORM's way to get a written row back.
  "returning:insert": `${posts}.save({ id: $1, authorId: $2, title: $3 })`,
  "returning:update": `${users}.save({ id: $2, nickname: $1 })`,
  "returning:update-narrowed": { code: `ds.createQueryBuilder().update(User).set({ nickname: 'anon' }).where('id = :id', { id: $1 }).returning(['nickname']).execute()`, paths: { nickname: "raw" } },
  "returning:delete": { code: `ds.createQueryBuilder().delete().from(Post).where('id = :id', { id: $1 }).returning(['title']).execute()`, paths: { title: "raw" } },
  "returning:upsert": `${users}.save({ id: $1, email: $2, name: $3 })`,

  "enums:column": `${users}.find({ select: { role: true } })`,
  "enums:derived": raw,
  "enums:check-union": `ds.getRepository(Ticket).find({ select: { status: true } })`,
  "enums:literal-union": raw,

  "smoke:smoke": `${users}.find({ select: { id: true, name: true } })`,
};

export const typeorm = ormTool({
  id: "typeorm",
  name: "TypeORM",
  version: versionOf("typeorm"),
  dbs: ["postgres", "mysql", "sqlite"],
  cardinalityNote: "Each call picks how many rows it takes: `find` returns an array, and `findOne` at most one row.",
  queries,
  raw: /ds\.query\(|getRaw|returning\(/,
  compilerOptions: { experimentalDecorators: true },
  project(db) {
    const dir = workdir("typeorm");
    projectDir(dir);
    writeFileSync(join(dir, "entities.ts"), entities(db));
    gitInit(dir);
    const prelude = [
      `import { DataSource, Like, MoreThan } from "typeorm";`,
      `import { User, Post${db === "postgres" ? ", Event, Ticket" : ""} } from "./entities";`,
      `declare const ds: DataSource;`,
    ].join("\n");
    return { dir, prelude, wrote: gitDelta(dir) };
  },
});

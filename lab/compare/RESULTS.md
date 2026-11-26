# Results

Written by `./run.sh` on 2026-11-26. Each mark comes from what the tool answered; [probes.ts](probes.ts) holds the queries and the right answers, and [grade.ts](grade.ts) the rules.

| | Reylo | sqlc | PgTyped | TypedSQL | sqlx-ts | SafeQL | sqlx | TypeSQL | Postgres Typed SQL | Drizzle | Kysely | Prisma | TypeORM |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Version | 0.9.0 | v1.31.1 | 2.4.3 | 7.10.1 | 0.49.0 | 5.4.2 | 0.9.0 | 0.24.0 | 0.1.0-beta.22 | 0.45.4 | 0.29.9 | 7.10.1 | 1.1.3 |
| No codegen | yes | no | no | no | no | partly | yes | no | partly | no | no | no | yes |
| Live database | yes (3/3) | partly (3/3) | yes (3/3) | yes (3/3) | partly (1/3) | partly (2/3) | yes (3/3) | yes (3/3) | no (3/3) | no | no | no | no |
| Nullable left joins | yes (20/20) | partly (14/20) | no (1/7) | partly (14/20) | no (3/20) | partly (6/7) | partly (14/20) | partly (11/20) | yes (7/7) | partly (17/20) | partly (17/20) | no (0/20) | no (0/20) |
| Nullable expressions | yes (57/57) | partly (22/57) | no (9/19) | partly (43/57) | partly (27/57) | partly (15/19) | partly (43/57) | partly (49/57) | yes (19/19) | partly (12/57) | partly (28/57) | partly (9/57) | partly (6/57) |
| Parameter types | yes (17/17) | yes (17/17) | yes (7/7) | partly (12/17) | partly (16/17) | partly (5/7) | partly (7/17) | partly (16/17) | yes (7/7) | yes (17/17) | yes (17/17) | yes (17/17) | yes (17/17) |
| Nullable parameters | yes (12/12) | partly (9/12) | no (0/4) | no (4/12) | partly (9/12) | no (0/4) | no (0/12) | yes (12/12) | no (2/4) | yes (12/12) | partly (3/12) | yes (9/9) | yes (12/12) |
| Cardinality | yes (28/28) | no (0/28) | no (0/10) | no (0/28) | no (0/28) | no (0/10) | no (0/28) | partly (16/28) | partly (9/10) | no (0/28) | no (0/28) | no (0/28) | no (0/28) |
| Nested JSON | yes (10/10) | no (0/10) | no (0/4) | no (0/10) | partly (7/10) | yes (4/4) | no (0/10) | partly (3/10) | yes (4/4) | partly (6/10) | partly (8/10) | partly (6/10) | partly (6/10) |
| JSON Schema types | yes (2/2) | no (0/2) | no (0/2) | no (0/2) | no (0/2) | no (0/2) | no (0/2) | no (0/2) | no (0/2) | no (0/2) | no (0/2) | no (0/2) | no (0/2) |
| Aggregates | yes (15/15) | no | no | no | no | no | no | no | no | partly (12/15) | partly (9/15) | partly (9/15) | no (0/15) |
| Window functions | yes (12/12) | no | no | no | no | no | no | no | no | no (0/12) | no (0/12) | no (0/12) | no (0/12) |
| Subqueries | yes (20/20) | no | no | no | no | no | no | no | no | partly (10/20) | partly (14/20) | partly (6/20) | partly (3/20) |
| WITH queries | yes (9/9) | no | no | no | no | no | no | no | no | partly (3/9) | partly (6/9) | no (0/9) | no (0/9) |
| Set operations | yes (12/12) | no | no | no | no | no | no | no | no | partly (9/12) | partly (9/12) | no (0/12) | no (0/12) |
| Returning rows | yes (10/10) | no | no | no | no | no | no | no | no | partly (8/10) | partly (6/10) | partly (8/10) | partly (8/10) |
| Enums | yes (8/8) | no | no | no | no | no | no | no | no | partly (4/8) | partly (4/8) | partly (2/8) | partly (2/8) |
| Postgres | yes | yes | yes | yes | yes | yes | yes | yes | yes | yes | yes | yes | yes |
| MySQL | yes | yes | no | yes | yes | no | yes | yes | no | yes | yes | yes | yes |
| SQLite | yes | yes | no | yes | yes | no | yes | yes | no | yes | yes | yes | yes |

## Reylo 0.9.0

**No codegen**: yes. Nothing to commit after a run.

## sqlc v1.31.1

**No codegen**: no. A run leaves new files to commit: `db/db.go`, `db/models.go`, `db/query.sql.go`.

**Live database**: partly, 3 of 3. By default it reads your schema files and never connects. A `database:` setting asks Postgres for types, but not for nullability.

**Nullable left joins**: partly, 14 of 20

- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: `title` is typed `sql.NullString`, but it is never null
- postgres: `select u.name, x.title from users u left join (select author_id, title from posts) x on x.author_id = u.id`: `title` is typed `string`, but it can be null
- mysql: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: `title` is typed `sql.NullString`, but it is never null
- mysql: `select u.name, x.title from users u left join (select author_id, title from posts) x on x.author_id = u.id`: `title` is typed `string`, but it can be null
- sqlite: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: `title` is typed `sql.NullString`, but it is never null
- sqlite: `select u.name, x.title from users u left join (select author_id, title from posts) x on x.author_id = u.id`: `title` is typed `string`, but it can be null

**Nullable expressions**: partly, 22 of 57

- postgres: `select max(id) as v from posts`: `v` is typed `interface{}`
- postgres: `select sum(score) as v from posts`: `v` is typed `int64`, but it can be null
- postgres: `select name || '!' as v from users`: `v` is typed `interface{}`
- postgres: `select nickname || '!' as v from users`: `v` is typed `interface{}`
- postgres: `select upper(name) as v from users`: `v` is typed `interface{}`
- postgres: `select upper(nickname) as v from users`: `v` is typed `interface{}`
- postgres: `select score + 1 as v from posts`: `v` is typed `int32`, but it can be null
- postgres: `select nullif(name, 'x') as v from users`: `v` is typed `bool`, but it can be null
- postgres: `select case when id > 0 then name end as v from users`: `v` is typed `interface{}`
- postgres: `select nickname is null as v from users`: `v` is typed `interface{}`
- postgres: `select lag(id) over (order by id) as v from users`: `v` is typed `interface{}`
- postgres: `select id as v from users union all select score from posts`: `v` is typed `int32`, but it can be null
- postgres: `select max(id) as v from posts group by author_id`: `v` is typed `interface{}`
- mysql: `select max(id) as v from posts`: `v` is typed `interface{}`
- mysql: `select sum(score) as v from posts`: `v` is typed `interface{}`
- mysql: `select concat(nickname, '!') as v from users`: `v` is typed `string`, but it can be null
- mysql: `select upper(nickname) as v from users`: `v` is typed `string`, but it can be null
- mysql: `select score + 1 as v from posts`: `v` is typed `int32`, but it can be null
- mysql: `select nullif(name, 'x') as v from users`: `v` is typed `interface{}`
- mysql: `select case when id > 0 then name end as v from users`: `v` is typed `interface{}`
- mysql: `select row_number() over (order by id) as v from users`: `v` is typed `interface{}`
- mysql: `select lag(id) over (order by id) as v from users`: `v` is typed `interface{}`
- mysql: `select id as v from users union all select score from posts`: `v` is typed `int32`, but it can be null
- mysql: `select max(id) as v from posts group by author_id`: `v` is typed `interface{}`
- sqlite: `select max(id) as v from posts`: `v` is typed `interface{}`
- sqlite: `select name || '!' as v from users`: `v` is typed `interface{}`
- sqlite: `select nickname || '!' as v from users`: `v` is typed `interface{}`
- sqlite: `select upper(nickname) as v from users`: `v` is typed `string`, but it can be null
- sqlite: `select score + 1 as v from posts`: `v` is typed `int64`, but it can be null
- sqlite: `select nullif(name, 'x') as v from users`: `v` is typed `interface{}`
- sqlite: `select case when id > 0 then name end as v from users`: `v` is typed `interface{}`
- sqlite: `select row_number() over (order by id) as v from users`: `v` is typed `interface{}`
- sqlite: `select lag(id) over (order by id) as v from users`: `v` is typed `interface{}`
- sqlite: `select id as v from users union all select score from posts`: `v` is typed `int64`, but it can be null
- sqlite: `select max(id) as v from posts group by author_id`: `v` is typed `interface{}`

**Nullable parameters**: partly, 9 of 12

- postgres: `select id from users where nickname = $1`: parameter 1 is typed `sql.NullString`, but it takes no null
- mysql: `select id from users where nickname = $1`: parameter 1 is typed `sql.NullString`, but it takes no null
- sqlite: `select id from users where nickname = $1`: parameter 1 is typed `sql.NullString`, but it takes no null

**Cardinality**: no, 0 of 28. Declared by hand on each query: `:one`, `:many` or `:exec`. The annotation decides the method's return type, and sqlc accepts any of them.

- postgres: `select id, name from users where id = $1`: it does not infer one
- postgres: `select id from users where email = $1`: it does not infer one
- postgres: `select count(*) as v from users`: it does not infer one
- postgres: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- postgres: `select id from users`: it does not infer one
- postgres: `select id from users limit 1`: it does not infer one
- postgres: `update users set name = $1 where id = $2 returning id`: it does not infer one
- postgres: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- postgres: `select id from posts where author_id = $1`: it does not infer one
- postgres: `delete from users where id = $1`: it does not infer one
- mysql: `select id, name from users where id = $1`: it does not infer one
- mysql: `select id from users where email = $1`: it does not infer one
- mysql: `select count(*) as v from users`: it does not infer one
- mysql: `select id from users`: it does not infer one
- mysql: `select id from users limit 1`: it does not infer one
- mysql: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- mysql: `select id from posts where author_id = $1`: it does not infer one
- mysql: `delete from users where id = $1`: it does not infer one
- sqlite: `select id, name from users where id = $1`: it does not infer one
- sqlite: `select id from users where email = $1`: it does not infer one
- sqlite: `select count(*) as v from users`: it does not infer one
- sqlite: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- sqlite: `select id from users`: it does not infer one
- sqlite: `select id from users limit 1`: it does not infer one
- sqlite: `update users set name = $1 where id = $2 returning id`: it does not infer one
- sqlite: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- sqlite: `select id from posts where author_id = $1`: it does not infer one
- sqlite: `delete from users where id = $1`: it does not infer one

**Nested JSON**: no, 0 of 10

- postgres: `select json_build_object('id', id, 'name', name) as v from users`: `v` is typed `json.RawMessage`
- postgres: `select json_agg(json_build_object('id', id, 'title', title)) as v from posts`: `v` is typed `json.RawMessage`
- postgres: `select u.name, jsonb_agg(jsonb_build_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id`: `v` is typed `json.RawMessage`
- postgres: `select u.name, (select jsonb_agg(p order by p.id) from posts p where p.author_id = u.id) as v from users u`: `v` is typed `json.RawMessage`
- mysql: `select json_object('id', id, 'name', name) as v from users`: `v` is typed `json.RawMessage`
- mysql: `select json_arrayagg(json_object('id', id, 'title', title)) as v from posts`: `v` is typed `json.RawMessage`
- mysql: `select u.name, json_arrayagg(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id, u.name`: `v` is typed `json.RawMessage`
- sqlite: `select json_object('id', id, 'name', name) as v from users`: `v` is typed `interface{}`
- sqlite: `select json_group_array(json_object('id', id, 'title', title)) as v from posts`: `v` is typed `interface{}`
- sqlite: `select u.name, json_group_array(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id`: `v` is typed `interface{}`

**JSON Schema types**: no, 0 of 2

- postgres: `select payload from events`: `payload` is typed `json.RawMessage`
- postgres: `insert into events (id, payload) values ($1, $2)`: parameter 2 is typed `json.RawMessage`, without its fields

## PgTyped 2.4.3

**No codegen**: no. A run leaves new files to commit: `src/q.queries.ts`.

**Nullable left joins**: no, 1 of 7

- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id`: `title` is typed `string`, but it can be null
- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id order by p.title`: `title` is typed `string`, but it can be null
- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id where u.id = $1`: `title` is typed `string`, but it can be null
- postgres: `select u.name, x.title from users u left join (select author_id, title from posts) x on x.author_id = u.id`: `title` is typed `string`, but it can be null
- postgres: `select u.name, p.title from posts p right join users u on u.id = p.author_id`: `title` is typed `string`, but it can be null
- postgres: `select u.name, p.title from users u full join posts p on p.author_id = u.id`: `name` is typed `string`, but it can be null; `title` is typed `string`, but it can be null

**Nullable expressions**: no, 9 of 19. It gives every expression the same answer.

- postgres: `select count(*) as v from posts`: `v` is typed `string | null`, but it is never null
- postgres: `select coalesce(nickname, 'x') as v from users`: `v` is typed `string | null`, but it is never null
- postgres: `select name || '!' as v from users`: `v` is typed `string | null`, but it is never null
- postgres: `select upper(name) as v from users`: `v` is typed `string | null`, but it is never null
- postgres: `select id + 1 as v from posts`: `v` is typed `number | null`, but it is never null
- postgres: `select nickname is null as v from users`: `v` is typed `boolean | null`, but it is never null
- postgres: `select (select count(*) from posts) as v`: `v` is typed `string | null`, but it is never null
- postgres: `select exists (select 1 from posts) as v`: `v` is typed `boolean | null`, but it is never null
- postgres: `select row_number() over (order by id) as v from users`: `v` is typed `string | null`, but it is never null
- postgres: `select max(id) as v from posts group by author_id`: `v` is typed `number | null`, but it is never null

**Nullable parameters**: no, 0 of 4. It gives every parameter the same answer.

- postgres: `insert into users (id, email, name, nickname) values ($1, $2, $3, $4)`: parameter 1 is typed `optional number | null | void`, but it takes no null; parameter 2 is typed `optional string | null | void`, but it takes no null; parameter 3 is typed `optional string | null | void`, but it takes no null
- postgres: `update users set nickname = $1 where id = $2`: parameter 2 is typed `optional number | null | void`, but it takes no null
- postgres: `select name from users where id = $1`: parameter 1 is typed `optional number | null | void`, but it takes no null
- postgres: `select id from users where nickname = $1`: parameter 1 is typed `optional string | null | void`, but it takes no null

**Cardinality**: no, 0 of 10. A query's `.run()` always returns an array, whatever the statement.

- postgres: `select id, name from users where id = $1`: it does not infer one
- postgres: `select id from users where email = $1`: it does not infer one
- postgres: `select count(*) as v from users`: it does not infer one
- postgres: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- postgres: `select id from users`: it does not infer one
- postgres: `select id from users limit 1`: it does not infer one
- postgres: `update users set name = $1 where id = $2 returning id`: it does not infer one
- postgres: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- postgres: `select id from posts where author_id = $1`: it does not infer one
- postgres: `delete from users where id = $1`: it does not infer one

**Nested JSON**: no, 0 of 4

- postgres: `select json_build_object('id', id, 'name', name) as v from users`: `v` is typed `Json | null`
- postgres: `select json_agg(json_build_object('id', id, 'title', title)) as v from posts`: `v` is typed `Json | null`
- postgres: `select u.name, jsonb_agg(jsonb_build_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id`: `v` is typed `Json | null`
- postgres: `select u.name, (select jsonb_agg(p order by p.id) from posts p where p.author_id = u.id) as v from users u`: `v` is typed `Json | null`

**JSON Schema types**: no, 0 of 2

- postgres: `select payload from events`: `payload` is typed `Json`
- postgres: `insert into events (id, payload) values ($1, $2)`: parameter 2 is typed `optional Json | null | void`, without its fields

**MySQL**: no. It is not available for MySQL.

**SQLite**: no. It is not available for SQLite.

## TypedSQL 7.10.1

**No codegen**: no. A run leaves new files to commit: `generated/prisma/browser.ts`, `generated/prisma/client.ts`, `generated/prisma/commonInputTypes.ts`.

**Nullable left joins**: partly, 14 of 20

- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id`: `name` is typed `string | null`, but it is never null; `title` is typed `string`, but it can be null
- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id order by p.title`: `title` is typed `string`, but it can be null
- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id where u.id = $1`: `title` is typed `string`, but it can be null
- postgres: `select u.name, x.title from users u left join (select author_id, title from posts) x on x.author_id = u.id`: `name` is typed `string | null`, but it is never null; `title` is typed `string`, but it can be null
- postgres: `select u.name, p.title from posts p right join users u on u.id = p.author_id`: `name` is typed `string | null`, but it is never null; `title` is typed `string`, but it can be null
- mysql: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: `title` is typed `string | null`, but it is never null

**Nullable expressions**: partly, 43 of 57

- postgres: `select count(*) as v from posts`: `v` is typed `bigint | null`, but it is never null
- postgres: `select coalesce(nickname, 'x') as v from users`: `v` is typed `string | null`, but it is never null
- postgres: `select name || '!' as v from users`: `v` is typed `string | null`, but it is never null
- postgres: `select upper(name) as v from users`: `v` is typed `string | null`, but it is never null
- postgres: `select id + 1 as v from posts`: `v` is typed `number | null`, but it is never null
- postgres: `select nickname is null as v from users`: `v` is typed `boolean | null`, but it is never null
- postgres: `select (select count(*) from posts) as v`: `v` is typed `bigint | null`, but it is never null
- postgres: `select exists (select 1 from posts) as v`: `v` is typed `boolean | null`, but it is never null
- postgres: `select row_number() over (order by id) as v from users`: `v` is typed `bigint | null`, but it is never null
- postgres: `select max(id) as v from posts group by author_id`: `v` is typed `number | null`, but it is never null
- mysql: `select (select count(*) from posts) as v`: `v` is typed `bigint | null`, but it is never null
- mysql: `select max(id) as v from posts group by author_id`: `v` is typed `number | null`, but it is never null
- sqlite: `select upper(name) as v from users`: `v` is typed `string | null`, but it is never null
- sqlite: `select upper(nickname) as v from users`: `v` is typed `null | null`

**Parameter types**: partly, 12 of 17

- sqlite: `select name from users where id = $1`: parameter 1 is typed `unknown`, but it takes a number
- sqlite: `select id from users where name = $1`: parameter 1 is typed `unknown`, but it takes text
- sqlite: `insert into posts (id, author_id, title, score) values ($1, $2, $3, $4)`: parameter 1 is typed `unknown`, but it takes a number; parameter 2 is typed `unknown`, but it takes a number; parameter 3 is typed `unknown`, but it takes text; parameter 4 is typed `unknown`, but it takes a number
- sqlite: `select id from users limit $1`: parameter 1 is typed `unknown`, but it takes a number
- sqlite: `select id from users where name like $1`: parameter 1 is typed `unknown`, but it takes text

**Nullable parameters**: no, 4 of 12. It gives every parameter the same answer.

- postgres: `insert into users (id, email, name, nickname) values ($1, $2, $3, $4)`: parameter 4 is typed `string`, but it takes null
- postgres: `update users set nickname = $1 where id = $2`: parameter 1 is typed `string`, but it takes null
- mysql: `insert into users (id, email, name, nickname) values ($1, $2, $3, $4)`: parameter 4 is typed `string`, but it takes null
- mysql: `update users set nickname = $1 where id = $2`: parameter 1 is typed `string`, but it takes null
- sqlite: `insert into users (id, email, name, nickname) values ($1, $2, $3, $4)`: parameter 2 is typed `unknown`, but it takes no null; parameter 3 is typed `unknown`, but it takes no null
- sqlite: `update users set nickname = $1 where id = $2`: parameter 2 is typed `unknown`, but it takes no null
- sqlite: `select name from users where id = $1`: parameter 1 is typed `unknown`, but it takes no null
- sqlite: `select id from users where nickname = $1`: parameter 1 is typed `unknown`, but it takes no null

**Cardinality**: no, 0 of 28. Every query returns an array, whatever the statement.

- postgres: `select id, name from users where id = $1`: it does not infer one
- postgres: `select id from users where email = $1`: it does not infer one
- postgres: `select count(*) as v from users`: it does not infer one
- postgres: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- postgres: `select id from users`: it does not infer one
- postgres: `select id from users limit 1`: it does not infer one
- postgres: `update users set name = $1 where id = $2 returning id`: it does not infer one
- postgres: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- postgres: `select id from posts where author_id = $1`: it does not infer one
- postgres: `delete from users where id = $1`: it does not infer one
- mysql: `select id, name from users where id = $1`: it does not infer one
- mysql: `select id from users where email = $1`: it does not infer one
- mysql: `select count(*) as v from users`: it does not infer one
- mysql: `select id from users`: it does not infer one
- mysql: `select id from users limit 1`: it does not infer one
- mysql: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- mysql: `select id from posts where author_id = $1`: it does not infer one
- mysql: `delete from users where id = $1`: it does not infer one
- sqlite: `select id, name from users where id = $1`: it does not infer one
- sqlite: `select id from users where email = $1`: it does not infer one
- sqlite: `select count(*) as v from users`: it does not infer one
- sqlite: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- sqlite: `select id from users`: it does not infer one
- sqlite: `select id from users limit 1`: it does not infer one
- sqlite: `update users set name = $1 where id = $2 returning id`: it does not infer one
- sqlite: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- sqlite: `select id from posts where author_id = $1`: it does not infer one
- sqlite: `delete from users where id = $1`: it does not infer one

**Nested JSON**: no, 0 of 10

- postgres: `select json_build_object('id', id, 'name', name) as v from users`: `v` is typed `$runtime.JsonValue | null`
- postgres: `select json_agg(json_build_object('id', id, 'title', title)) as v from posts`: `v` is typed `$runtime.JsonValue | null`
- postgres: `select u.name, jsonb_agg(jsonb_build_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id`: `v` is typed `$runtime.JsonValue | null`
- postgres: `select u.name, (select jsonb_agg(p order by p.id) from posts p where p.author_id = u.id) as v from users u`: `v` is typed `$runtime.JsonValue | null`
- mysql: `select json_object('id', id, 'name', name) as v from users`: `v` is typed `$runtime.JsonValue | null`
- mysql: `select json_arrayagg(json_object('id', id, 'title', title)) as v from posts`: `v` is typed `$runtime.JsonValue | null`
- mysql: `select u.name, json_arrayagg(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id, u.name`: `v` is typed `$runtime.JsonValue | null`
- sqlite: `select json_object('id', id, 'name', name) as v from users`: `v` is typed `string | null`, without its fields
- sqlite: `select json_group_array(json_object('id', id, 'title', title)) as v from posts`: `v` is typed `string | null`, without its fields
- sqlite: `select u.name, json_group_array(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id`: `v` is typed `string | null`, without its fields

**JSON Schema types**: no, 0 of 2

- postgres: `select payload from events`: `payload` is typed `$runtime.JsonValue`
- postgres: `insert into events (id, payload) values ($1, $2)`: parameter 2 is typed `$runtime.InputJsonObject`, without its fields

## sqlx-ts 0.49.0

**No codegen**: no. A run leaves new files to commit: `q.queries.ts`.

**Live database**: partly, 1 of 3. It connects, but types some expressions itself.

- postgres: `select add_one(id) as v from users`: it types the result `unknown`
- postgres: `select array_agg(id) as v from users`: it types the result `unknown`

**Nullable left joins**: no, 3 of 20

- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id`: `title` is typed `string`, but it can be null
- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id order by p.title`: `title` is typed `string`, but it can be null
- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id where u.id = $1`: `title` is typed `string`, but it can be null
- postgres: `select u.name, x.title from users u left join (select author_id, title from posts) x on x.author_id = u.id`: it reports an error: Error: [E014] Failed to find a table name from a FROM statement: statement: `Default FROM table is not found from the query: SELECT u.name, x.title FROM users u LEFT JOIN (SELECT author_id, title FROM posts) x ON x.author_id = u.id. Ensure your query has a valid FROM clause.`  L
- postgres: `select u.name, p.title from posts p right join users u on u.id = p.author_id`: `title` is typed `string`, but it can be null
- postgres: `select u.name, p.title from users u full join posts p on p.author_id = u.id`: `name` is typed `string`, but it can be null; `title` is typed `string`, but it can be null
- mysql: `select u.name, p.title from users u left join posts p on p.author_id = u.id`: `title` is typed `string`, but it can be null
- mysql: `select u.name, p.title from users u left join posts p on p.author_id = u.id order by p.title`: `title` is typed `string`, but it can be null
- mysql: `select u.name, p.title from users u left join posts p on p.author_id = u.id where u.id = $1`: `title` is typed `string`, but it can be null
- mysql: `select u.name, x.title from users u left join (select author_id, title from posts) x on x.author_id = u.id`: it reports an error: Error: [E014] Failed to find a table name from a FROM statement: statement: `Default FROM table is not found from the query: SELECT u.name, x.title FROM users u LEFT JOIN (SELECT author_id, title FROM posts) x ON x.author_id = u.id. Ensure your query has a valid FROM clause.`  L
- mysql: `select u.name, p.title from posts p right join users u on u.id = p.author_id`: `title` is typed `string`, but it can be null
- sqlite: `select u.name, p.title from users u left join posts p on p.author_id = u.id`: `title` is typed `string`, but it can be null
- sqlite: `select u.name, p.title from users u left join posts p on p.author_id = u.id order by p.title`: `title` is typed `string`, but it can be null
- sqlite: `select u.name, p.title from users u left join posts p on p.author_id = u.id where u.id = $1`: `title` is typed `string`, but it can be null
- sqlite: `select u.name, x.title from users u left join (select author_id, title from posts) x on x.author_id = u.id`: it reports an error: Error: [E014] Failed to find a table name from a FROM statement: statement: `Default FROM table is not found from the query: SELECT u.name, x.title FROM users u LEFT JOIN (SELECT author_id, title FROM posts) x ON x.author_id = u.id. Ensure your query has a valid FROM clause.`  L
- sqlite: `select u.name, p.title from posts p right join users u on u.id = p.author_id`: `title` is typed `string`, but it can be null
- sqlite: `select u.name, p.title from users u full join posts p on p.author_id = u.id`: `name` is typed `string`, but it can be null; `title` is typed `string`, but it can be null

**Nullable expressions**: partly, 27 of 57

- postgres: `select max(id) as v from posts`: `v` is typed `number`, but it can be null
- postgres: `select sum(score) as v from posts`: `v` is typed `number`, but it can be null
- postgres: `select nickname || '!' as v from users`: `v` is typed `string`, but it can be null
- postgres: `select upper(nickname) as v from users`: `v` is typed `string`, but it can be null
- postgres: `select score + 1 as v from posts`: `v` is typed `number`, but it can be null
- postgres: `select nullif(name, 'x') as v from users`: `v` is typed `string`, but it can be null
- postgres: `select case when id > 0 then name end as v from users`: `v` is typed `unknown`
- postgres: `select (select count(*) from posts) as v`: `v` is missing
- postgres: `select lag(id) over (order by id) as v from users`: `v` is typed `number`, but it can be null
- postgres: `select id as v from users union all select score from posts`: it reports an error: Error: Unknown error: `Unknown query type while processing query: SELECT id AS v FROM users UNION ALL SELECT score FROM posts`  Location:     src/ts_generator/generator.rs:199:3
- mysql: `select max(id) as v from posts`: `v` is typed `number`, but it can be null
- mysql: `select sum(score) as v from posts`: `v` is typed `number`, but it can be null
- mysql: `select concat(nickname, '!') as v from users`: `v` is typed `string`, but it can be null
- mysql: `select upper(nickname) as v from users`: `v` is typed `string`, but it can be null
- mysql: `select score + 1 as v from posts`: `v` is typed `number`, but it can be null
- mysql: `select nullif(name, 'x') as v from users`: `v` is typed `string`, but it can be null
- mysql: `select case when id > 0 then name end as v from users`: `v` is typed `unknown`
- mysql: `select (select count(*) from posts) as v`: `v` is missing
- mysql: `select lag(id) over (order by id) as v from users`: `v` is typed `number`, but it can be null
- mysql: `select id as v from users union all select score from posts`: it reports an error: Error: Unknown error: `Unknown query type while processing query: SELECT id AS v FROM users UNION ALL SELECT score FROM posts`  Location:     src/ts_generator/generator.rs:199:3
- sqlite: `select max(id) as v from posts`: `v` is typed `number`, but it can be null
- sqlite: `select sum(score) as v from posts`: `v` is typed `number`, but it can be null
- sqlite: `select nickname || '!' as v from users`: `v` is typed `string`, but it can be null
- sqlite: `select upper(nickname) as v from users`: `v` is typed `string`, but it can be null
- sqlite: `select score + 1 as v from posts`: `v` is typed `number`, but it can be null
- sqlite: `select nullif(name, 'x') as v from users`: `v` is typed `string`, but it can be null
- sqlite: `select case when id > 0 then name end as v from users`: `v` is typed `unknown`
- sqlite: `select (select count(*) from posts) as v`: `v` is missing
- sqlite: `select lag(id) over (order by id) as v from users`: `v` is typed `number`, but it can be null
- sqlite: `select id as v from users union all select score from posts`: it reports an error: Error: Unknown error: `Unknown query type while processing query: SELECT id AS v FROM users UNION ALL SELECT score FROM posts`  Location:     src/ts_generator/generator.rs:199:3

**Parameter types**: partly, 16 of 17

- postgres: `select id from posts where created_at > $1`: parameter 1 is typed `unknown`, but it takes a timestamp

**Nullable parameters**: partly, 9 of 12

- postgres: `select id from users where nickname = $1`: parameter 1 is typed `string | null`, but it takes no null
- mysql: `select id from users where nickname = $1`: parameter 1 is typed `string | null`, but it takes no null
- sqlite: `select id from users where nickname = $1`: parameter 1 is typed `string | null`, but it takes no null

**Cardinality**: no, 0 of 28. It types the row only, and how many rows come back is left to you.

- postgres: `select id, name from users where id = $1`: it does not infer one
- postgres: `select id from users where email = $1`: it does not infer one
- postgres: `select count(*) as v from users`: it does not infer one
- postgres: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- postgres: `select id from users`: it does not infer one
- postgres: `select id from users limit 1`: it does not infer one
- postgres: `update users set name = $1 where id = $2 returning id`: it does not infer one
- postgres: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- postgres: `select id from posts where author_id = $1`: it does not infer one
- postgres: `delete from users where id = $1`: it does not infer one
- mysql: `select id, name from users where id = $1`: it does not infer one
- mysql: `select id from users where email = $1`: it does not infer one
- mysql: `select count(*) as v from users`: it does not infer one
- mysql: `select id from users`: it does not infer one
- mysql: `select id from users limit 1`: it does not infer one
- mysql: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- mysql: `select id from posts where author_id = $1`: it does not infer one
- mysql: `delete from users where id = $1`: it does not infer one
- sqlite: `select id, name from users where id = $1`: it does not infer one
- sqlite: `select id from users where email = $1`: it does not infer one
- sqlite: `select count(*) as v from users`: it does not infer one
- sqlite: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- sqlite: `select id from users`: it does not infer one
- sqlite: `select id from users limit 1`: it does not infer one
- sqlite: `update users set name = $1 where id = $2 returning id`: it does not infer one
- sqlite: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- sqlite: `select id from posts where author_id = $1`: it does not infer one
- sqlite: `delete from users where id = $1`: it does not infer one

**Nested JSON**: partly, 7 of 10

- postgres: `select u.name, (select jsonb_agg(p order by p.id) from posts p where p.author_id = u.id) as v from users u`: `v` is typed `any`
- sqlite: `select json_group_array(json_object('id', id, 'title', title)) as v from posts`: `v` is typed `unknown`
- sqlite: `select u.name, json_group_array(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id`: `v` is typed `unknown`

**JSON Schema types**: no, 0 of 2

- postgres: `select payload from events`: `payload` is typed `object`
- postgres: `insert into events (id, payload) values ($1, $2)`: parameter 2 is typed `object`, without its fields

## SafeQL 5.4.2

**No codegen**: partly. A run leaves changed source to commit: `right.ts`.

**Live database**: partly, 2 of 3. It connects, but types some expressions itself.

- postgres: `select add_one(id) as v from users`: it types the result `unknown | null`

**Nullable left joins**: partly, 6 of 7

- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: `title` is typed `string | null`, but it is never null

**Nullable expressions**: partly, 15 of 19

- postgres: `select upper(nickname) as v from users`: `v` is typed `string`, but it can be null
- postgres: `select nullif(name, 'x') as v from users`: `v` is typed `boolean`, but it can be null
- postgres: `select lag(id) over (order by id) as v from users`: it reports an error: Query has incorrect type annotation. 	Expected: { v: any | null }[] 	  Actual: { v: anyelement | null }[]
- postgres: `select max(id) as v from posts group by author_id`: `v` is typed `number | null`, but it is never null

**Parameter types**: partly, 5 of 7

- postgres: `insert into posts (id, author_id, title, score) values ($1, $2, $3, $4)`: a boolean value passes for parameter 3, which takes text
- postgres: `select id from users where external_id = $1`: it reports an error: Invalid Query: operator does not exist: uuid = text

**Nullable parameters**: no, 0 of 4. It gives every parameter the same answer.

- postgres: `insert into users (id, email, name, nickname) values ($1, $2, $3, $4)`: a null value passes for parameter 1, which takes no null; a null value passes for parameter 2, which takes no null; a null value passes for parameter 3, which takes no null
- postgres: `update users set nickname = $1 where id = $2`: a null value passes for parameter 2, which takes no null
- postgres: `select name from users where id = $1`: a null value passes for parameter 1, which takes no null
- postgres: `select id from users where nickname = $1`: a null value passes for parameter 1, which takes no null

**Cardinality**: no, 0 of 10. Every query is typed as an array, unless your own transform says otherwise.

- postgres: `select id, name from users where id = $1`: it does not infer one
- postgres: `select id from users where email = $1`: it does not infer one
- postgres: `select count(*) as v from users`: it does not infer one
- postgres: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- postgres: `select id from users`: it does not infer one
- postgres: `select id from users limit 1`: it does not infer one
- postgres: `update users set name = $1 where id = $2 returning id`: it does not infer one
- postgres: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- postgres: `select id from posts where author_id = $1`: it does not infer one
- postgres: `delete from users where id = $1`: it does not infer one

**JSON Schema types**: no, 0 of 2

- postgres: `select payload from events`: `payload` is typed `any`
- postgres: `insert into events (id, payload) values ($1, $2)`: it reports an error: Invalid Query: The type "{ x: number; }" has no corresponding PostgreSQL type. Please add it manually using the "overrides.types" option: ```ts {   "connections": {     ...,     "overrides": {       "types": {         "PG TYPE (e.g. 'date')": "{ x: number; }"       }     }   } }

**MySQL**: no. It is not available for MySQL.

**SQLite**: no. It is not available for SQLite.

## sqlx 0.9.0

**No codegen**: yes. Nothing to commit after a run.

**Nullable left joins**: partly, 14 of 20

- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id`: `name` is typed `Option<TEXT>`, but it is never null; `title` is typed `TEXT`, but it can be null
- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id order by p.title`: `title` is typed `TEXT`, but it can be null
- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id where u.id = $1`: `title` is typed `TEXT`, but it can be null
- postgres: `select u.name, x.title from users u left join (select author_id, title from posts) x on x.author_id = u.id`: `name` is typed `Option<TEXT>`, but it is never null; `title` is typed `TEXT`, but it can be null
- postgres: `select u.name, p.title from posts p right join users u on u.id = p.author_id`: `name` is typed `Option<TEXT>`, but it is never null; `title` is typed `TEXT`, but it can be null
- mysql: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: `title` is typed `Option<VARCHAR>`, but it is never null

**Nullable expressions**: partly, 43 of 57

- postgres: `select count(*) as v from posts`: `v` is typed `Option<INT8>`, but it is never null
- postgres: `select coalesce(nickname, 'x') as v from users`: `v` is typed `Option<TEXT>`, but it is never null
- postgres: `select name || '!' as v from users`: `v` is typed `Option<TEXT>`, but it is never null
- postgres: `select upper(name) as v from users`: `v` is typed `Option<TEXT>`, but it is never null
- postgres: `select id + 1 as v from posts`: `v` is typed `Option<INT4>`, but it is never null
- postgres: `select nickname is null as v from users`: `v` is typed `Option<BOOL>`, but it is never null
- postgres: `select (select count(*) from posts) as v`: `v` is typed `Option<INT8>`, but it is never null
- postgres: `select exists (select 1 from posts) as v`: `v` is typed `Option<BOOL>`, but it is never null
- postgres: `select row_number() over (order by id) as v from users`: `v` is typed `Option<INT8>`, but it is never null
- postgres: `select max(id) as v from posts group by author_id`: `v` is typed `Option<INT4>`, but it is never null
- mysql: `select (select count(*) from posts) as v`: `v` is typed `Option<BIGINT>`, but it is never null
- mysql: `select max(id) as v from posts group by author_id`: `v` is typed `Option<INT>`, but it is never null
- sqlite: `select upper(name) as v from users`: `v` is typed `Option<TEXT>`, but it is never null
- sqlite: `select upper(nickname) as v from users`: `v` is typed `Option<NULL>`

**Parameter types**: partly, 7 of 17

- mysql: `select name from users where id = $1`: any value passes for parameter 1, which takes a number
- mysql: `select id from users where name = $1`: any value passes for parameter 1, which takes text
- mysql: `insert into posts (id, author_id, title, score) values ($1, $2, $3, $4)`: any value passes for parameter 1, which takes a number; any value passes for parameter 2, which takes a number; any value passes for parameter 3, which takes text; any value passes for parameter 4, which takes a number
- mysql: `select id from users limit $1`: any value passes for parameter 1, which takes a number
- mysql: `select id from users where name like $1`: any value passes for parameter 1, which takes text
- sqlite: `select name from users where id = $1`: any value passes for parameter 1, which takes a number
- sqlite: `select id from users where name = $1`: any value passes for parameter 1, which takes text
- sqlite: `insert into posts (id, author_id, title, score) values ($1, $2, $3, $4)`: any value passes for parameter 1, which takes a number; any value passes for parameter 2, which takes a number; any value passes for parameter 3, which takes text; any value passes for parameter 4, which takes a number
- sqlite: `select id from users limit $1`: any value passes for parameter 1, which takes a number
- sqlite: `select id from users where name like $1`: any value passes for parameter 1, which takes text

**Nullable parameters**: no, 0 of 12. It gives every parameter the same answer.

- postgres: `insert into users (id, email, name, nickname) values ($1, $2, $3, $4)`: a null value passes for parameter 1, which takes no null; a null value passes for parameter 2, which takes no null; a null value passes for parameter 3, which takes no null
- postgres: `update users set nickname = $1 where id = $2`: a null value passes for parameter 2, which takes no null
- postgres: `select name from users where id = $1`: a null value passes for parameter 1, which takes no null
- postgres: `select id from users where nickname = $1`: a null value passes for parameter 1, which takes no null
- mysql: `insert into users (id, email, name, nickname) values ($1, $2, $3, $4)`: a null value passes for parameter 1, which takes no null; a null value passes for parameter 2, which takes no null; a null value passes for parameter 3, which takes no null
- mysql: `update users set nickname = $1 where id = $2`: a null value passes for parameter 2, which takes no null
- mysql: `select name from users where id = $1`: a null value passes for parameter 1, which takes no null
- mysql: `select id from users where nickname = $1`: a null value passes for parameter 1, which takes no null
- sqlite: `insert into users (id, email, name, nickname) values ($1, $2, $3, $4)`: a null value passes for parameter 2, which takes no null; a null value passes for parameter 3, which takes no null
- sqlite: `update users set nickname = $1 where id = $2`: a null value passes for parameter 2, which takes no null
- sqlite: `select name from users where id = $1`: a null value passes for parameter 1, which takes no null
- sqlite: `select id from users where nickname = $1`: a null value passes for parameter 1, which takes no null

**Cardinality**: no, 0 of 28. You pick it on each call: `fetch_one`, `fetch_optional` or `fetch_all`.

- postgres: `select id, name from users where id = $1`: it does not infer one
- postgres: `select id from users where email = $1`: it does not infer one
- postgres: `select count(*) as v from users`: it does not infer one
- postgres: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- postgres: `select id from users`: it does not infer one
- postgres: `select id from users limit 1`: it does not infer one
- postgres: `update users set name = $1 where id = $2 returning id`: it does not infer one
- postgres: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- postgres: `select id from posts where author_id = $1`: it does not infer one
- postgres: `delete from users where id = $1`: it does not infer one
- mysql: `select id, name from users where id = $1`: it does not infer one
- mysql: `select id from users where email = $1`: it does not infer one
- mysql: `select count(*) as v from users`: it does not infer one
- mysql: `select id from users`: it does not infer one
- mysql: `select id from users limit 1`: it does not infer one
- mysql: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- mysql: `select id from posts where author_id = $1`: it does not infer one
- mysql: `delete from users where id = $1`: it does not infer one
- sqlite: `select id, name from users where id = $1`: it does not infer one
- sqlite: `select id from users where email = $1`: it does not infer one
- sqlite: `select count(*) as v from users`: it does not infer one
- sqlite: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- sqlite: `select id from users`: it does not infer one
- sqlite: `select id from users limit 1`: it does not infer one
- sqlite: `update users set name = $1 where id = $2 returning id`: it does not infer one
- sqlite: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- sqlite: `select id from posts where author_id = $1`: it does not infer one
- sqlite: `delete from users where id = $1`: it does not infer one

**Nested JSON**: no, 0 of 10

- postgres: `select json_build_object('id', id, 'name', name) as v from users`: `v` is typed `Option<JSON>`
- postgres: `select json_agg(json_build_object('id', id, 'title', title)) as v from posts`: `v` is typed `Option<JSON>`
- postgres: `select u.name, jsonb_agg(jsonb_build_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id`: `v` is typed `Option<JSONB>`
- postgres: `select u.name, (select jsonb_agg(p order by p.id) from posts p where p.author_id = u.id) as v from users u`: `v` is typed `Option<JSONB>`
- mysql: `select json_object('id', id, 'name', name) as v from users`: `v` is typed `Option<JSON>`
- mysql: `select json_arrayagg(json_object('id', id, 'title', title)) as v from posts`: `v` is typed `Option<JSON>`
- mysql: `select u.name, json_arrayagg(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id, u.name`: `v` is typed `Option<JSON>`
- sqlite: `select json_object('id', id, 'name', name) as v from users`: `v` is typed `Option<TEXT>`, without its fields
- sqlite: `select json_group_array(json_object('id', id, 'title', title)) as v from posts`: `v` is typed `Option<TEXT>`, without its fields
- sqlite: `select u.name, json_group_array(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id`: `v` is typed `Option<TEXT>`, without its fields

**JSON Schema types**: no, 0 of 2

- postgres: `select payload from events`: `payload` is typed `JSONB`
- postgres: `insert into events (id, payload) values ($1, $2)`: parameter 2 is typed `JSONB or Option<JSONB>`, without its fields

## TypeSQL 0.24.0

**No codegen**: no. A run leaves new files to commit: `sql/index.ts`, `sql/q0.ts`, `sql/q1.ts`.

**Nullable left joins**: partly, 11 of 20

- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: `title` is typed `string | null`, but it is never null
- postgres: `select u.name, p.title from posts p right join users u on u.id = p.author_id`: `title` is typed `string`, but it can be null
- postgres: `select u.name, p.title from users u full join posts p on p.author_id = u.id`: `name` is typed `string`, but it can be null; `title` is typed `string`, but it can be null
- mysql: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: `title` is typed `string | null`, but it is never null
- mysql: `select u.name, p.title from posts p right join users u on u.id = p.author_id`: `name` is typed `string | null`, but it is never null; `title` is typed `string`, but it can be null
- sqlite: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: `title` is typed `string | null`, but it is never null
- sqlite: `select u.name, x.title from users u left join (select author_id, title from posts) x on x.author_id = u.id`: `title` is typed `string`, but it can be null
- sqlite: `select u.name, p.title from posts p right join users u on u.id = p.author_id`: `title` is typed `string`, but it can be null
- sqlite: `select u.name, p.title from users u full join posts p on p.author_id = u.id`: `name` is typed `string`, but it can be null; `title` is typed `string`, but it can be null

**Nullable expressions**: partly, 49 of 57

- postgres: `select upper(name) as v from users`: `v` is typed `string | null`, but it is never null
- postgres: `select (select count(*) from posts) as v`: `v` is typed `string | null`, but it is never null
- postgres: `select max(id) as v from posts group by author_id`: `v` is typed `number | null`, but it is never null
- mysql: `select (select count(*) from posts) as v`: `v` is typed `number | null`, but it is never null
- mysql: `select max(id) as v from posts group by author_id`: `v` is typed `number | null`, but it is never null
- sqlite: `select max(id) as v from posts`: `v` is typed `number`, but it can be null
- sqlite: `select (select count(*) from posts) as v`: `v` is typed `number | null`, but it is never null
- sqlite: `select id as v from users union all select score from posts`: `v` is typed `number`, but it can be null

**Parameter types**: partly, 16 of 17

- postgres: `select id from users limit $1`: parameter 1 is typed `string`, but it takes a number

**Cardinality**: partly, 16 of 28

- postgres: `select id from users where email = $1`: it says many rows, but this returns at most one row
- postgres: `select count(*) as v from users`: it says at most one row, but this returns exactly one row
- postgres: `update users set name = $1 where id = $2 returning id`: it says exactly one row, but this returns at most one row
- postgres: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it says many rows, but this returns at most one row
- mysql: `select count(*) as v from users`: it says at most one row, but this returns exactly one row
- mysql: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it says many rows, but this returns at most one row
- sqlite: `select id, name from users where id = $1`: it says many rows, but this returns at most one row
- sqlite: `select id from users where email = $1`: it says many rows, but this returns at most one row
- sqlite: `select count(*) as v from users`: it says at most one row, but this returns exactly one row
- sqlite: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it says at most one row, but this returns exactly one row
- sqlite: `update users set name = $1 where id = $2 returning id`: it says many rows, but this returns at most one row
- sqlite: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it says many rows, but this returns at most one row

**Nested JSON**: partly, 3 of 10

- postgres: `select u.name, (select jsonb_agg(p order by p.id) from posts p where p.author_id = u.id) as v from users u`: `v` is typed `Q50VType[]`, but it can be null
- mysql: `select json_object('id', id, 'name', name) as v from users`: it reports an error: it marks this query as invalid SQL
- mysql: `select json_arrayagg(json_object('id', id, 'title', title)) as v from posts`: it reports an error: it marks this query as invalid SQL
- mysql: `select u.name, json_arrayagg(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id, u.name`: it reports an error: it marks this query as invalid SQL
- sqlite: `select json_object('id', id, 'name', name) as v from users`: it reports an error: it marks this query as invalid SQL
- sqlite: `select json_group_array(json_object('id', id, 'title', title)) as v from posts`: it reports an error: it marks this query as invalid SQL
- sqlite: `select u.name, json_group_array(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id`: it reports an error: it marks this query as invalid SQL

**JSON Schema types**: no, 0 of 2

- postgres: `select payload from events`: `payload` is typed `any`
- postgres: `insert into events (id, payload) values ($1, $2)`: parameter 2 is typed `any`, without its fields

## Postgres Typed SQL 0.1.0-beta.22

**No codegen**: partly. By default it writes files to commit. An experimental workflow on TypeScript 7.1 types the import with nothing generated.

**Live database**: no, 3 of 3. It loads your schema file into an embedded Postgres, and never connects to your database.

**Nullable parameters**: no, 2 of 4. It gives every parameter the same answer.

- postgres: `insert into users (id, email, name, nickname) values ($1, $2, $3, $4)`: parameter 4 is typed `string`, but it takes null
- postgres: `update users set nickname = $1 where id = $2`: parameter 1 is typed `string`, but it takes null

**Cardinality**: partly, 9 of 10

- postgres: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it says many rows, but this returns exactly one row

**JSON Schema types**: no, 0 of 2

- postgres: `select payload from events`: `payload` is typed `DbJsonSelected`
- postgres: `insert into events (id, payload) values ($1, $2)`: parameter 2 is typed `DbJsonParameter`, without its fields

**MySQL**: no. It is not available for MySQL.

**SQLite**: no. It is not available for SQLite.

## Drizzle 0.45.4

**No codegen**: no. A run leaves new files to commit: `db/0000_legal_microchip.sql`, `db/meta/0000_snapshot.json`, `db/meta/_journal.json`.

**Live database**: no. It types each query from the schema kept in your code, and never asks the database.

**Nullable left joins**: partly, 17 of 20

- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: `title` is typed `string | null`, but it is never null
- mysql: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: `title` is typed `string | null`, but it is never null
- sqlite: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: `title` is typed `string | null`, but it is never null

**Nullable expressions**: partly, 12 of 57

- postgres: `select coalesce(nickname, 'x') as v from users`: `v` is typed `unknown`
- postgres: `select name || '!' as v from users`: `v` is typed `unknown`
- postgres: `select nickname || '!' as v from users`: `v` is typed `unknown`
- postgres: `select upper(name) as v from users`: `v` is typed `unknown`
- postgres: `select upper(nickname) as v from users`: `v` is typed `unknown`
- postgres: `select id + 1 as v from posts`: `v` is typed `unknown`
- postgres: `select score + 1 as v from posts`: `v` is typed `unknown`
- postgres: `select nullif(name, 'x') as v from users`: `v` is typed `unknown`
- postgres: `select case when id > 0 then name end as v from users`: `v` is typed `unknown`
- postgres: `select nickname is null as v from users`: `v` is typed `unknown`
- postgres: `select exists (select 1 from posts) as v`: `v` is typed `unknown`
- postgres: `select row_number() over (order by id) as v from users`: `v` is typed `unknown`
- postgres: `select lag(id) over (order by id) as v from users`: `v` is typed `unknown`
- postgres: `select id as v from users union all select score from posts`: it reports an error: Argument of type 'PgSelectBase<"posts", { v: PgColumn<{ name: "score"; tableName: "posts"; dataType: "number"; columnType: "PgInteger"; data: number; driverParam: string | number; notNull: false; hasDefault: false; isPrimaryKey: false; ... 5 more ...; generated: undefined; }, {}
- postgres: `select max(id) as v from posts group by author_id`: `v` is typed `number | null`, but it is never null
- mysql: `select coalesce(nickname, 'x') as v from users`: `v` is typed `unknown`
- mysql: `select concat(name, '!') as v from users`: `v` is typed `unknown`
- mysql: `select concat(nickname, '!') as v from users`: `v` is typed `unknown`
- mysql: `select upper(name) as v from users`: `v` is typed `unknown`
- mysql: `select upper(nickname) as v from users`: `v` is typed `unknown`
- mysql: `select id + 1 as v from posts`: `v` is typed `unknown`
- mysql: `select score + 1 as v from posts`: `v` is typed `unknown`
- mysql: `select nullif(name, 'x') as v from users`: `v` is typed `unknown`
- mysql: `select case when id > 0 then name end as v from users`: `v` is typed `unknown`
- mysql: `select nickname is null as v from users`: `v` is typed `unknown`
- mysql: `select exists (select 1 from posts) as v`: `v` is typed `unknown`
- mysql: `select row_number() over (order by id) as v from users`: `v` is typed `unknown`
- mysql: `select lag(id) over (order by id) as v from users`: `v` is typed `unknown`
- mysql: `select id as v from users union all select score from posts`: it reports an error: Argument of type 'MySqlSelectBase<"posts", { v: MySqlColumn<{ name: "score"; tableName: "posts"; dataType: "number"; columnType: "MySqlInt"; data: number; driverParam: string | number; notNull: false; hasDefault: false; isPrimaryKey: false; ... 5 more ...; generated: undefined; 
- mysql: `select max(id) as v from posts group by author_id`: `v` is typed `number | null`, but it is never null
- sqlite: `select coalesce(nickname, 'x') as v from users`: `v` is typed `unknown`
- sqlite: `select name || '!' as v from users`: `v` is typed `unknown`
- sqlite: `select nickname || '!' as v from users`: `v` is typed `unknown`
- sqlite: `select upper(name) as v from users`: `v` is typed `unknown`
- sqlite: `select upper(nickname) as v from users`: `v` is typed `unknown`
- sqlite: `select id + 1 as v from posts`: `v` is typed `unknown`
- sqlite: `select score + 1 as v from posts`: `v` is typed `unknown`
- sqlite: `select nullif(name, 'x') as v from users`: `v` is typed `unknown`
- sqlite: `select case when id > 0 then name end as v from users`: `v` is typed `unknown`
- sqlite: `select nickname is null as v from users`: `v` is typed `unknown`
- sqlite: `select exists (select 1 from posts) as v`: `v` is typed `unknown`
- sqlite: `select row_number() over (order by id) as v from users`: `v` is typed `unknown`
- sqlite: `select lag(id) over (order by id) as v from users`: `v` is typed `unknown`
- sqlite: `select id as v from users union all select score from posts`: it reports an error: Argument of type 'SQLiteSelectBase<"posts", "sync", RunResult, { v: SQLiteColumn<{ name: "score"; tableName: "posts"; dataType: "number"; columnType: "SQLiteInteger"; data: number; driverParam: number; notNull: false; hasDefault: false; isPrimaryKey: false; ... 5 more ...; gener
- sqlite: `select max(id) as v from posts group by author_id`: `v` is typed `number | null`, but it is never null

**Cardinality**: no, 0 of 28. Each call picks how many rows it takes: a query returns an array, and `findFirst` at most one row.

- postgres: `select id, name from users where id = $1`: it does not infer one
- postgres: `select id from users where email = $1`: it does not infer one
- postgres: `select count(*) as v from users`: it does not infer one
- postgres: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- postgres: `select id from users`: it does not infer one
- postgres: `select id from users limit 1`: it does not infer one
- postgres: `update users set name = $1 where id = $2 returning id`: it does not infer one
- postgres: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- postgres: `select id from posts where author_id = $1`: it does not infer one
- postgres: `delete from users where id = $1`: it does not infer one
- mysql: `select id, name from users where id = $1`: it does not infer one
- mysql: `select id from users where email = $1`: it does not infer one
- mysql: `select count(*) as v from users`: it does not infer one
- mysql: `select id from users`: it does not infer one
- mysql: `select id from users limit 1`: it does not infer one
- mysql: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- mysql: `select id from posts where author_id = $1`: it does not infer one
- mysql: `delete from users where id = $1`: it does not infer one
- sqlite: `select id, name from users where id = $1`: it does not infer one
- sqlite: `select id from users where email = $1`: it does not infer one
- sqlite: `select count(*) as v from users`: it does not infer one
- sqlite: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- sqlite: `select id from users`: it does not infer one
- sqlite: `select id from users limit 1`: it does not infer one
- sqlite: `update users set name = $1 where id = $2 returning id`: it does not infer one
- sqlite: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- sqlite: `select id from posts where author_id = $1`: it does not infer one
- sqlite: `delete from users where id = $1`: it does not infer one

**Nested JSON**: partly, 6 of 10

- postgres: `select u.name, jsonb_agg(jsonb_build_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id`: `v` is typed `unknown`
- postgres: `select u.name, (select jsonb_agg(p order by p.id) from posts p where p.author_id = u.id) as v from users u`: `v` is typed `unknown`
- mysql: `select u.name, json_arrayagg(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id, u.name`: `v` is typed `unknown`
- sqlite: `select u.name, json_group_array(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id`: `v` is typed `unknown`

**JSON Schema types**: no, 0 of 2

- postgres: `select payload from events`: `payload` is typed `unknown`
- postgres: `insert into events (id, payload) values ($1, $2)`: parameter 2 is typed `{ x: number }`, without its fields

**Aggregates**: partly, 12 of 15

- postgres: `select author_id, min(title) as v from posts group by author_id`: `v` is typed `string | null`, but it is never null
- mysql: `select author_id, min(title) as v from posts group by author_id`: `v` is typed `string | null`, but it is never null
- sqlite: `select author_id, min(title) as v from posts group by author_id`: `v` is typed `string | null`, but it is never null

**Window functions**: no, 0 of 12

- postgres: `select id, row_number() over (partition by author_id order by id) as v from posts`: `v` is typed `unknown`
- postgres: `select id, rank() over (order by score) as v from posts`: `v` is typed `unknown`
- postgres: `select id, sum(score) over (partition by author_id order by id) as v from posts`: `v` is typed `unknown`
- postgres: `select id, lag(title) over (order by id) as v from posts`: `v` is typed `unknown`
- mysql: `select id, row_number() over (partition by author_id order by id) as v from posts`: `v` is typed `unknown`
- mysql: `select id, rank() over (order by score) as v from posts`: `v` is typed `unknown`
- mysql: `select id, sum(score) over (partition by author_id order by id) as v from posts`: `v` is typed `unknown`
- mysql: `select id, lag(title) over (order by id) as v from posts`: `v` is typed `unknown`
- sqlite: `select id, row_number() over (partition by author_id order by id) as v from posts`: `v` is typed `unknown`
- sqlite: `select id, rank() over (order by score) as v from posts`: `v` is typed `unknown`
- sqlite: `select id, sum(score) over (partition by author_id order by id) as v from posts`: `v` is typed `unknown`
- sqlite: `select id, lag(title) over (order by id) as v from posts`: `v` is typed `unknown`

**Subqueries**: partly, 10 of 20

- postgres: `with recent as (select id, title from posts order by id desc limit 5) select (select count(*)::int from users) as user_count, (select coalesce(jsonb_agg(jsonb_build_object('id', r.id, 'title', r.title)), '[]') from recent r) as recent_posts`: `recent_posts` is typed `unknown`
- postgres: `select u.name, (select max(p.title) from posts p where p.author_id = u.id) as v from users u`: `v` is typed `unknown`
- postgres: `select u.name, exists (select 1 from posts p where p.author_id = u.id) as v from users u`: `v` is typed `unknown`
- mysql: `with recent as (select id, title from posts order by id desc limit 5) select (select count(*) from users) as user_count, (select coalesce(json_arrayagg(json_object('id', r.id, 'title', r.title)), json_array()) from recent r) as recent_posts`: `recent_posts` is typed `unknown`
- mysql: `select u.name, p.title, p.score from users u left join lateral (select title, score from posts where author_id = u.id order by created_at desc limit 1) p on true`: `title` is typed `unknown`; `score` is typed `unknown`
- mysql: `select u.name, (select max(p.title) from posts p where p.author_id = u.id) as v from users u`: `v` is typed `unknown`
- mysql: `select u.name, exists (select 1 from posts p where p.author_id = u.id) as v from users u`: `v` is typed `unknown`
- sqlite: `with recent as (select id, title from posts order by id desc limit 5) select (select count(*) from users) as user_count, (select json_group_array(json_object('id', r.id, 'title', r.title)) from recent r) as recent_posts`: `recent_posts` is typed `unknown`
- sqlite: `select u.name, (select max(p.title) from posts p where p.author_id = u.id) as v from users u`: `v` is typed `unknown`
- sqlite: `select u.name, exists (select 1 from posts p where p.author_id = u.id) as v from users u`: `v` is typed `unknown`

**WITH queries**: partly, 3 of 9

- postgres: `with counts as (select author_id, count(*) as n from posts group by author_id) select u.name, c.n from users u left join counts c on c.author_id = u.id`: `n` is typed `number`, but it can be null
- postgres: `with recursive nums (n) as (select 1 union all select n + 1 from nums where n < 5) select n from nums`: `n` is typed `unknown`
- mysql: `with counts as (select author_id, count(*) as n from posts group by author_id) select u.name, c.n from users u left join counts c on c.author_id = u.id`: `n` is typed `number`, but it can be null
- mysql: `with recursive nums (n) as (select 1 union all select n + 1 from nums where n < 5) select n from nums`: `n` is typed `unknown`
- sqlite: `with counts as (select author_id, count(*) as n from posts group by author_id) select u.name, c.n from users u left join counts c on c.author_id = u.id`: `n` is typed `number`, but it can be null
- sqlite: `with recursive nums (n) as (select 1 union all select n + 1 from nums where n < 5) select n from nums`: `n` is typed `unknown`

**Set operations**: partly, 9 of 12

- postgres: `select id as v from users union all select score from posts`: it reports an error: Argument of type 'PgSelectBase<"posts", { v: PgColumn<{ name: "score"; tableName: "posts"; dataType: "number"; columnType: "PgInteger"; data: number; driverParam: string | number; notNull: false; hasDefault: false; isPrimaryKey: false; ... 5 more ...; generated: undefined; }, {}
- mysql: `select id as v from users union all select score from posts`: it reports an error: Argument of type 'MySqlSelectBase<"posts", { v: MySqlColumn<{ name: "score"; tableName: "posts"; dataType: "number"; columnType: "MySqlInt"; data: number; driverParam: string | number; notNull: false; hasDefault: false; isPrimaryKey: false; ... 5 more ...; generated: undefined; 
- sqlite: `select id as v from users union all select score from posts`: it reports an error: Argument of type 'SQLiteSelectBase<"posts", "sync", RunResult, { v: SQLiteColumn<{ name: "score"; tableName: "posts"; dataType: "number"; columnType: "SQLiteInteger"; data: number; driverParam: number; notNull: false; hasDefault: false; isPrimaryKey: false; ... 5 more ...; gener

**Returning rows**: partly, 8 of 10

- postgres: `update users set nickname = 'anon' where id = $1 returning nickname`: `nickname` is typed `string | null`, but it is never null
- sqlite: `update users set nickname = 'anon' where id = $1 returning nickname`: `nickname` is typed `string | null`, but it is never null

**Enums**: partly, 4 of 8

- postgres: `select status from tickets`: `status` is typed `string`, without its values
- postgres: `select 'user' as kind, name as label from users union all select 'post', title from posts`: `kind` is typed `unknown`
- mysql: `select 'user' as kind, name as label from users union all select 'post', title from posts`: `kind` is typed `unknown`
- sqlite: `select 'user' as kind, name as label from users union all select 'post', title from posts`: `kind` is typed `unknown`

## Kysely 0.29.9

**No codegen**: no. A run leaves new files to commit: `db.d.ts`.

**Live database**: no. It types each query from the schema kept in your code, and never asks the database.

**Nullable left joins**: partly, 17 of 20

- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: `title` is typed `string | null`, but it is never null
- mysql: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: `title` is typed `string | null`, but it is never null
- sqlite: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: `title` is typed `string | null`, but it is never null

**Nullable expressions**: partly, 28 of 57

- postgres: `select max(id) as v from posts`: `v` is typed `number`, but it can be null
- postgres: `select sum(score) as v from posts`: `v` is typed `string | number | bigint`, but it can be null
- postgres: `select upper(name) as v from users`: `v` is typed `unknown`
- postgres: `select upper(nickname) as v from users`: `v` is typed `unknown`
- postgres: `select nullif(name, 'x') as v from users`: `v` is typed `unknown`
- postgres: `select (select count(*) from posts) as v`: `v` is typed `string | number | bigint | null`, but it is never null
- postgres: `select row_number() over (order by id) as v from users`: `v` is typed `unknown`
- postgres: `select lag(id) over (order by id) as v from users`: `v` is typed `unknown`
- postgres: `select id as v from users union all select score from posts`: it reports an error: Argument of type 'SelectQueryBuilder<DB, "posts", { v: number | null; }>' is not assignable to parameter of type 'SetOperandExpression<DB, { v: number; }>'.   Type 'SelectQueryBuilder<DB, "posts", { v: number | null; }>' is not assignable to type 'Expression<{ v: number; }>'.   
- mysql: `select max(id) as v from posts`: `v` is typed `number`, but it can be null
- mysql: `select sum(score) as v from posts`: `v` is typed `string | number | bigint`, but it can be null
- mysql: `select concat(name, '!') as v from users`: `v` is typed `unknown`
- mysql: `select concat(nickname, '!') as v from users`: `v` is typed `unknown`
- mysql: `select upper(name) as v from users`: `v` is typed `unknown`
- mysql: `select upper(nickname) as v from users`: `v` is typed `unknown`
- mysql: `select nullif(name, 'x') as v from users`: `v` is typed `unknown`
- mysql: `select (select count(*) from posts) as v`: `v` is typed `string | number | bigint | null`, but it is never null
- mysql: `select row_number() over (order by id) as v from users`: `v` is typed `unknown`
- mysql: `select lag(id) over (order by id) as v from users`: `v` is typed `unknown`
- mysql: `select id as v from users union all select score from posts`: it reports an error: Argument of type 'SelectQueryBuilder<DB, "posts", { v: number | null; }>' is not assignable to parameter of type 'SetOperandExpression<DB, { v: number; }>'.   Type 'SelectQueryBuilder<DB, "posts", { v: number | null; }>' is not assignable to type 'Expression<{ v: number; }>'.   
- sqlite: `select sum(score) as v from posts`: `v` is typed `string | number | bigint`, but it can be null
- sqlite: `select upper(name) as v from users`: `v` is typed `unknown`
- sqlite: `select upper(nickname) as v from users`: `v` is typed `unknown`
- sqlite: `select id + 1 as v from posts`: `v` is typed `number | null`, but it is never null
- sqlite: `select nullif(name, 'x') as v from users`: `v` is typed `unknown`
- sqlite: `select (select count(*) from posts) as v`: `v` is typed `string | number | bigint | null`, but it is never null
- sqlite: `select row_number() over (order by id) as v from users`: `v` is typed `unknown`
- sqlite: `select lag(id) over (order by id) as v from users`: `v` is typed `unknown`
- sqlite: `select max(id) as v from posts group by author_id`: `v` is typed `number | null`, but it is never null

**Nullable parameters**: partly, 3 of 12

- postgres: `update users set nickname = $1 where id = $2`: a null value passes for parameter 2, which takes no null
- postgres: `select name from users where id = $1`: a null value passes for parameter 1, which takes no null
- postgres: `select id from users where nickname = $1`: a null value passes for parameter 1, which takes no null
- mysql: `update users set nickname = $1 where id = $2`: a null value passes for parameter 2, which takes no null
- mysql: `select name from users where id = $1`: a null value passes for parameter 1, which takes no null
- mysql: `select id from users where nickname = $1`: a null value passes for parameter 1, which takes no null
- sqlite: `update users set nickname = $1 where id = $2`: a null value passes for parameter 2, which takes no null
- sqlite: `select name from users where id = $1`: a null value passes for parameter 1, which takes no null
- sqlite: `select id from users where nickname = $1`: a null value passes for parameter 1, which takes no null

**Cardinality**: no, 0 of 28. Each call picks how many rows it takes: `execute` returns an array, and `executeTakeFirst` at most one row.

- postgres: `select id, name from users where id = $1`: it does not infer one
- postgres: `select id from users where email = $1`: it does not infer one
- postgres: `select count(*) as v from users`: it does not infer one
- postgres: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- postgres: `select id from users`: it does not infer one
- postgres: `select id from users limit 1`: it does not infer one
- postgres: `update users set name = $1 where id = $2 returning id`: it does not infer one
- postgres: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- postgres: `select id from posts where author_id = $1`: it does not infer one
- postgres: `delete from users where id = $1`: it does not infer one
- mysql: `select id, name from users where id = $1`: it does not infer one
- mysql: `select id from users where email = $1`: it does not infer one
- mysql: `select count(*) as v from users`: it does not infer one
- mysql: `select id from users`: it does not infer one
- mysql: `select id from users limit 1`: it does not infer one
- mysql: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- mysql: `select id from posts where author_id = $1`: it does not infer one
- mysql: `delete from users where id = $1`: it does not infer one
- sqlite: `select id, name from users where id = $1`: it does not infer one
- sqlite: `select id from users where email = $1`: it does not infer one
- sqlite: `select count(*) as v from users`: it does not infer one
- sqlite: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- sqlite: `select id from users`: it does not infer one
- sqlite: `select id from users limit 1`: it does not infer one
- sqlite: `update users set name = $1 where id = $2 returning id`: it does not infer one
- sqlite: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- sqlite: `select id from posts where author_id = $1`: it does not infer one
- sqlite: `delete from users where id = $1`: it does not infer one

**Nested JSON**: partly, 8 of 10

- mysql: `select u.name, json_arrayagg(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id, u.name`: `v` is typed `unknown`
- sqlite: `select u.name, json_group_array(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id`: `v` is typed `unknown`

**JSON Schema types**: no, 0 of 2

- postgres: `select payload from events`: `payload` is typed `JsonArray | JsonObject | JsonPrimitive`, without its fields
- postgres: `insert into events (id, payload) values ($1, $2)`: parameter 2 is typed `{ x: number }`, without its fields

**Aggregates**: partly, 9 of 15

- postgres: `select author_id, sum(score) as v from posts group by author_id having count(*) > 1`: `v` is typed `string | number | bigint`, but it can be null
- postgres: `select avg(score) as v from posts`: `v` is typed `string | number`, but it can be null
- mysql: `select author_id, sum(score) as v from posts group by author_id having count(*) > 1`: `v` is typed `string | number | bigint`, but it can be null
- mysql: `select avg(score) as v from posts`: `v` is typed `string | number`, but it can be null
- sqlite: `select author_id, sum(score) as v from posts group by author_id having count(*) > 1`: `v` is typed `string | number | bigint`, but it can be null
- sqlite: `select avg(score) as v from posts`: `v` is typed `string | number`, but it can be null

**Window functions**: no, 0 of 12

- postgres: `select id, row_number() over (partition by author_id order by id) as v from posts`: `v` is typed `unknown`
- postgres: `select id, rank() over (order by score) as v from posts`: `v` is typed `unknown`
- postgres: `select id, sum(score) over (partition by author_id order by id) as v from posts`: `v` is typed `string | number | bigint`, but it can be null
- postgres: `select id, lag(title) over (order by id) as v from posts`: `v` is typed `unknown`
- mysql: `select id, row_number() over (partition by author_id order by id) as v from posts`: `v` is typed `unknown`
- mysql: `select id, rank() over (order by score) as v from posts`: `v` is typed `unknown`
- mysql: `select id, sum(score) over (partition by author_id order by id) as v from posts`: `v` is typed `string | number | bigint`, but it can be null
- mysql: `select id, lag(title) over (order by id) as v from posts`: `v` is typed `unknown`
- sqlite: `select id, row_number() over (partition by author_id order by id) as v from posts`: `id` is typed `number | null`, but it is never null; `v` is typed `unknown`
- sqlite: `select id, rank() over (order by score) as v from posts`: `id` is typed `number | null`, but it is never null; `v` is typed `unknown`
- sqlite: `select id, sum(score) over (partition by author_id order by id) as v from posts`: `id` is typed `number | null`, but it is never null; `v` is typed `string | number | bigint`, but it can be null
- sqlite: `select id, lag(title) over (order by id) as v from posts`: `id` is typed `number | null`, but it is never null; `v` is typed `unknown`

**Subqueries**: partly, 14 of 20

- postgres: `with recent as (select id, title from posts order by id desc limit 5) select (select count(*)::int from users) as user_count, (select coalesce(jsonb_agg(jsonb_build_object('id', r.id, 'title', r.title)), '[]') from recent r) as recent_posts`: `user_count` is typed `string | number | bigint | null`, but it is never null
- postgres: `select u.name, (select count(*) from posts p where p.author_id = u.id) as v from users u`: `v` is typed `string | number | bigint | null`, but it is never null
- mysql: `with recent as (select id, title from posts order by id desc limit 5) select (select count(*) from users) as user_count, (select coalesce(json_arrayagg(json_object('id', r.id, 'title', r.title)), json_array()) from recent r) as recent_posts`: `user_count` is typed `string | number | bigint | null`, but it is never null
- mysql: `select u.name, (select count(*) from posts p where p.author_id = u.id) as v from users u`: `v` is typed `string | number | bigint | null`, but it is never null
- sqlite: `with recent as (select id, title from posts order by id desc limit 5) select (select count(*) from users) as user_count, (select json_group_array(json_object('id', r.id, 'title', r.title)) from recent r) as recent_posts`: `user_count` is typed `string | number | bigint | null`, but it is never null
- sqlite: `select u.name, (select count(*) from posts p where p.author_id = u.id) as v from users u`: `v` is typed `string | number | bigint | null`, but it is never null

**WITH queries**: partly, 6 of 9

- postgres: `with recursive nums (n) as (select 1 union all select n + 1 from nums where n < 5) select n from nums`: `n` is typed literal `1`, not a number
- mysql: `with recursive nums (n) as (select 1 union all select n + 1 from nums where n < 5) select n from nums`: `n` is typed literal `1`, not a number
- sqlite: `with recursive nums (n) as (select 1 union all select n + 1 from nums where n < 5) select n from nums`: `n` is typed literal `1`, not a number

**Set operations**: partly, 9 of 12

- postgres: `select id as v from users union all select score from posts`: it reports an error: Argument of type 'SelectQueryBuilder<DB, "posts", { v: number | null; }>' is not assignable to parameter of type 'SetOperandExpression<DB, { v: number; }>'.   Type 'SelectQueryBuilder<DB, "posts", { v: number | null; }>' is not assignable to type 'Expression<{ v: number; }>'.   
- mysql: `select id as v from users union all select score from posts`: it reports an error: Argument of type 'SelectQueryBuilder<DB, "posts", { v: number | null; }>' is not assignable to parameter of type 'SetOperandExpression<DB, { v: number; }>'.   Type 'SelectQueryBuilder<DB, "posts", { v: number | null; }>' is not assignable to type 'Expression<{ v: number; }>'.   
- sqlite: `select id as v from users except select author_id from posts`: `v` is typed `number | null`, but it is never null

**Returning rows**: partly, 6 of 10

- postgres: `update users set nickname = 'anon' where id = $1 returning nickname`: `nickname` is typed `string | null`, but it is never null
- sqlite: `insert into posts (id, author_id, title) values ($1, $2, $3) returning id, score`: `id` is typed `number | null`, but it is never null
- sqlite: `update users set nickname = 'anon' where id = $1 returning nickname`: `nickname` is typed `string | null`, but it is never null
- sqlite: `insert into users (id, email, name) values ($1, $2, $3) on conflict (id) do update set name = excluded.name returning id, nickname`: `id` is typed `number | null`, but it is never null

**Enums**: partly, 4 of 8

- postgres: `select status from tickets`: `status` is typed `string`, without its values
- postgres: `select 'user' as kind, name as label from users union all select 'post', title from posts`: it reports an error: Argument of type '"user"' is not assignable to parameter of type 'number | boolean | null'.; Argument of type '"post"' is not assignable to parameter of type 'number | boolean | null'.
- mysql: `select 'user' as kind, name as label from users union all select 'post', title from posts`: it reports an error: Argument of type '"user"' is not assignable to parameter of type 'number | boolean | null'.; Argument of type '"post"' is not assignable to parameter of type 'number | boolean | null'.
- sqlite: `select 'user' as kind, name as label from users union all select 'post', title from posts`: it reports an error: Argument of type '"user"' is not assignable to parameter of type 'number | boolean | null'.; Argument of type '"post"' is not assignable to parameter of type 'number | boolean | null'.

## Prisma 7.10.1

**No codegen**: no. A run leaves new files to commit: `prisma/schema.prisma`, `generated/prisma/browser.ts`, `generated/prisma/client.ts`.

**Live database**: no. It types each query from the schema kept in your code, and never asks the database.

**Nullable left joins**: no, 0 of 20

- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id`: it types each row as `unknown`
- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id order by p.title`: it types each row as `unknown`
- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id where u.id = $1`: it types each row as `unknown`
- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: it types each row as `unknown`
- postgres: `select u.name, x.title from users u left join (select author_id, title from posts) x on x.author_id = u.id`: it types each row as `unknown`
- postgres: `select u.name, p.title from posts p right join users u on u.id = p.author_id`: it types each row as `unknown`
- postgres: `select u.name, p.title from users u full join posts p on p.author_id = u.id`: it types each row as `unknown`
- mysql: `select u.name, p.title from users u left join posts p on p.author_id = u.id`: it types each row as `unknown`
- mysql: `select u.name, p.title from users u left join posts p on p.author_id = u.id order by p.title`: it types each row as `unknown`
- mysql: `select u.name, p.title from users u left join posts p on p.author_id = u.id where u.id = $1`: it types each row as `unknown`
- mysql: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: it types each row as `unknown`
- mysql: `select u.name, x.title from users u left join (select author_id, title from posts) x on x.author_id = u.id`: it types each row as `unknown`
- mysql: `select u.name, p.title from posts p right join users u on u.id = p.author_id`: it types each row as `unknown`
- sqlite: `select u.name, p.title from users u left join posts p on p.author_id = u.id`: it types each row as `unknown`
- sqlite: `select u.name, p.title from users u left join posts p on p.author_id = u.id order by p.title`: it types each row as `unknown`
- sqlite: `select u.name, p.title from users u left join posts p on p.author_id = u.id where u.id = $1`: it types each row as `unknown`
- sqlite: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: it types each row as `unknown`
- sqlite: `select u.name, x.title from users u left join (select author_id, title from posts) x on x.author_id = u.id`: it types each row as `unknown`
- sqlite: `select u.name, p.title from posts p right join users u on u.id = p.author_id`: it types each row as `unknown`
- sqlite: `select u.name, p.title from users u full join posts p on p.author_id = u.id`: it types each row as `unknown`

**Nullable expressions**: partly, 9 of 57

- postgres: `select coalesce(nickname, 'x') as v from users`: it types each row as `unknown`
- postgres: `select name || '!' as v from users`: it types each row as `unknown`
- postgres: `select nickname || '!' as v from users`: it types each row as `unknown`
- postgres: `select upper(name) as v from users`: it types each row as `unknown`
- postgres: `select upper(nickname) as v from users`: it types each row as `unknown`
- postgres: `select id + 1 as v from posts`: it types each row as `unknown`
- postgres: `select score + 1 as v from posts`: it types each row as `unknown`
- postgres: `select nullif(name, 'x') as v from users`: it types each row as `unknown`
- postgres: `select case when id > 0 then name end as v from users`: it types each row as `unknown`
- postgres: `select nickname is null as v from users`: it types each row as `unknown`
- postgres: `select (select count(*) from posts) as v`: it types each row as `unknown`
- postgres: `select exists (select 1 from posts) as v`: it types each row as `unknown`
- postgres: `select row_number() over (order by id) as v from users`: it types each row as `unknown`
- postgres: `select lag(id) over (order by id) as v from users`: it types each row as `unknown`
- postgres: `select id as v from users union all select score from posts`: it types each row as `unknown`
- postgres: `select max(id) as v from posts group by author_id`: `v` is typed `number | null`, but it is never null
- mysql: `select coalesce(nickname, 'x') as v from users`: it types each row as `unknown`
- mysql: `select concat(name, '!') as v from users`: it types each row as `unknown`
- mysql: `select concat(nickname, '!') as v from users`: it types each row as `unknown`
- mysql: `select upper(name) as v from users`: it types each row as `unknown`
- mysql: `select upper(nickname) as v from users`: it types each row as `unknown`
- mysql: `select id + 1 as v from posts`: it types each row as `unknown`
- mysql: `select score + 1 as v from posts`: it types each row as `unknown`
- mysql: `select nullif(name, 'x') as v from users`: it types each row as `unknown`
- mysql: `select case when id > 0 then name end as v from users`: it types each row as `unknown`
- mysql: `select nickname is null as v from users`: it types each row as `unknown`
- mysql: `select (select count(*) from posts) as v`: it types each row as `unknown`
- mysql: `select exists (select 1 from posts) as v`: it types each row as `unknown`
- mysql: `select row_number() over (order by id) as v from users`: it types each row as `unknown`
- mysql: `select lag(id) over (order by id) as v from users`: it types each row as `unknown`
- mysql: `select id as v from users union all select score from posts`: it types each row as `unknown`
- mysql: `select max(id) as v from posts group by author_id`: `v` is typed `number | null`, but it is never null
- sqlite: `select coalesce(nickname, 'x') as v from users`: it types each row as `unknown`
- sqlite: `select name || '!' as v from users`: it types each row as `unknown`
- sqlite: `select nickname || '!' as v from users`: it types each row as `unknown`
- sqlite: `select upper(name) as v from users`: it types each row as `unknown`
- sqlite: `select upper(nickname) as v from users`: it types each row as `unknown`
- sqlite: `select id + 1 as v from posts`: it types each row as `unknown`
- sqlite: `select score + 1 as v from posts`: it types each row as `unknown`
- sqlite: `select nullif(name, 'x') as v from users`: it types each row as `unknown`
- sqlite: `select case when id > 0 then name end as v from users`: it types each row as `unknown`
- sqlite: `select nickname is null as v from users`: it types each row as `unknown`
- sqlite: `select (select count(*) from posts) as v`: it types each row as `unknown`
- sqlite: `select exists (select 1 from posts) as v`: it types each row as `unknown`
- sqlite: `select row_number() over (order by id) as v from users`: it types each row as `unknown`
- sqlite: `select lag(id) over (order by id) as v from users`: it types each row as `unknown`
- sqlite: `select id as v from users union all select score from posts`: it types each row as `unknown`
- sqlite: `select max(id) as v from posts group by author_id`: `v` is typed `number | null`, but it is never null

**Cardinality**: no, 0 of 28. Each call picks how many rows it takes: `findMany` returns an array, and `findUnique` and `findFirst` at most one row.

- postgres: `select id, name from users where id = $1`: it does not infer one
- postgres: `select id from users where email = $1`: it does not infer one
- postgres: `select count(*) as v from users`: it does not infer one
- postgres: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- postgres: `select id from users`: it does not infer one
- postgres: `select id from users limit 1`: it does not infer one
- postgres: `update users set name = $1 where id = $2 returning id`: it does not infer one
- postgres: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- postgres: `select id from posts where author_id = $1`: it does not infer one
- postgres: `delete from users where id = $1`: it does not infer one
- mysql: `select id, name from users where id = $1`: it does not infer one
- mysql: `select id from users where email = $1`: it does not infer one
- mysql: `select count(*) as v from users`: it does not infer one
- mysql: `select id from users`: it does not infer one
- mysql: `select id from users limit 1`: it does not infer one
- mysql: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- mysql: `select id from posts where author_id = $1`: it does not infer one
- mysql: `delete from users where id = $1`: it does not infer one
- sqlite: `select id, name from users where id = $1`: it does not infer one
- sqlite: `select id from users where email = $1`: it does not infer one
- sqlite: `select count(*) as v from users`: it does not infer one
- sqlite: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- sqlite: `select id from users`: it does not infer one
- sqlite: `select id from users limit 1`: it does not infer one
- sqlite: `update users set name = $1 where id = $2 returning id`: it does not infer one
- sqlite: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- sqlite: `select id from posts where author_id = $1`: it does not infer one
- sqlite: `delete from users where id = $1`: it does not infer one

**Nested JSON**: partly, 6 of 10

- postgres: `select u.name, jsonb_agg(jsonb_build_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id`: it types each row as `unknown`
- postgres: `select u.name, (select jsonb_agg(p order by p.id) from posts p where p.author_id = u.id) as v from users u`: it types each row as `unknown`
- mysql: `select u.name, json_arrayagg(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id, u.name`: it types each row as `unknown`
- sqlite: `select u.name, json_group_array(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id`: it types each row as `unknown`

**JSON Schema types**: no, 0 of 2

- postgres: `select payload from events`: `payload` is typed `string | number | boolean | JsonObject | JsonArray | null`, without its fields
- postgres: `insert into events (id, payload) values ($1, $2)`: parameter 2 is typed `{ x: number }`, without its fields

**Aggregates**: partly, 9 of 15

- postgres: `select author_id, min(title) as v from posts group by author_id`: `v` is typed `string | null`, but it is never null
- postgres: `select count(distinct author_id) as v from posts`: it types each row as `unknown`
- mysql: `select author_id, min(title) as v from posts group by author_id`: `v` is typed `string | null`, but it is never null
- mysql: `select count(distinct author_id) as v from posts`: it types each row as `unknown`
- sqlite: `select author_id, min(title) as v from posts group by author_id`: `v` is typed `string | null`, but it is never null
- sqlite: `select count(distinct author_id) as v from posts`: it types each row as `unknown`

**Window functions**: no, 0 of 12

- postgres: `select id, row_number() over (partition by author_id order by id) as v from posts`: it types each row as `unknown`
- postgres: `select id, rank() over (order by score) as v from posts`: it types each row as `unknown`
- postgres: `select id, sum(score) over (partition by author_id order by id) as v from posts`: it types each row as `unknown`
- postgres: `select id, lag(title) over (order by id) as v from posts`: it types each row as `unknown`
- mysql: `select id, row_number() over (partition by author_id order by id) as v from posts`: it types each row as `unknown`
- mysql: `select id, rank() over (order by score) as v from posts`: it types each row as `unknown`
- mysql: `select id, sum(score) over (partition by author_id order by id) as v from posts`: it types each row as `unknown`
- mysql: `select id, lag(title) over (order by id) as v from posts`: it types each row as `unknown`
- sqlite: `select id, row_number() over (partition by author_id order by id) as v from posts`: it types each row as `unknown`
- sqlite: `select id, rank() over (order by score) as v from posts`: it types each row as `unknown`
- sqlite: `select id, sum(score) over (partition by author_id order by id) as v from posts`: it types each row as `unknown`
- sqlite: `select id, lag(title) over (order by id) as v from posts`: it types each row as `unknown`

**Subqueries**: partly, 6 of 20

- postgres: `with recent as (select id, title from posts order by id desc limit 5) select (select count(*)::int from users) as user_count, (select coalesce(jsonb_agg(jsonb_build_object('id', r.id, 'title', r.title)), '[]') from recent r) as recent_posts`: it types each row as `unknown`
- postgres: `select u.name, p.title, p.score from users u left join lateral (select title, score from posts where author_id = u.id order by created_at desc limit 1) p on true`: it types each row as `unknown`
- postgres: `select u.name, (select max(p.title) from posts p where p.author_id = u.id) as v from users u`: it types each row as `unknown`
- postgres: `select u.name, exists (select 1 from posts p where p.author_id = u.id) as v from users u`: it types each row as `unknown`
- postgres: `select x.author_id, x.n from (select author_id, count(*) as n from posts group by author_id) x`: it types each row as `unknown`
- mysql: `with recent as (select id, title from posts order by id desc limit 5) select (select count(*) from users) as user_count, (select coalesce(json_arrayagg(json_object('id', r.id, 'title', r.title)), json_array()) from recent r) as recent_posts`: it types each row as `unknown`
- mysql: `select u.name, p.title, p.score from users u left join lateral (select title, score from posts where author_id = u.id order by created_at desc limit 1) p on true`: it types each row as `unknown`
- mysql: `select u.name, (select max(p.title) from posts p where p.author_id = u.id) as v from users u`: it types each row as `unknown`
- mysql: `select u.name, exists (select 1 from posts p where p.author_id = u.id) as v from users u`: it types each row as `unknown`
- mysql: `select x.author_id, x.n from (select author_id, count(*) as n from posts group by author_id) x`: it types each row as `unknown`
- sqlite: `with recent as (select id, title from posts order by id desc limit 5) select (select count(*) from users) as user_count, (select json_group_array(json_object('id', r.id, 'title', r.title)) from recent r) as recent_posts`: it types each row as `unknown`
- sqlite: `select u.name, (select max(p.title) from posts p where p.author_id = u.id) as v from users u`: it types each row as `unknown`
- sqlite: `select u.name, exists (select 1 from posts p where p.author_id = u.id) as v from users u`: it types each row as `unknown`
- sqlite: `select x.author_id, x.n from (select author_id, count(*) as n from posts group by author_id) x`: it types each row as `unknown`

**WITH queries**: no, 0 of 9

- postgres: `with counts as (select author_id, count(*) as n from posts group by author_id) select u.name, c.n from users u join counts c on c.author_id = u.id`: it types each row as `unknown`
- postgres: `with counts as (select author_id, count(*) as n from posts group by author_id) select u.name, c.n from users u left join counts c on c.author_id = u.id`: it types each row as `unknown`
- postgres: `with recursive nums (n) as (select 1 union all select n + 1 from nums where n < 5) select n from nums`: it types each row as `unknown`
- mysql: `with counts as (select author_id, count(*) as n from posts group by author_id) select u.name, c.n from users u join counts c on c.author_id = u.id`: it types each row as `unknown`
- mysql: `with counts as (select author_id, count(*) as n from posts group by author_id) select u.name, c.n from users u left join counts c on c.author_id = u.id`: it types each row as `unknown`
- mysql: `with recursive nums (n) as (select 1 union all select n + 1 from nums where n < 5) select n from nums`: it types each row as `unknown`
- sqlite: `with counts as (select author_id, count(*) as n from posts group by author_id) select u.name, c.n from users u join counts c on c.author_id = u.id`: it types each row as `unknown`
- sqlite: `with counts as (select author_id, count(*) as n from posts group by author_id) select u.name, c.n from users u left join counts c on c.author_id = u.id`: it types each row as `unknown`
- sqlite: `with recursive nums (n) as (select 1 union all select n + 1 from nums where n < 5) select n from nums`: it types each row as `unknown`

**Set operations**: no, 0 of 12

- postgres: `select name as v from users union select title from posts`: it types each row as `unknown`
- postgres: `select nickname as v from users union all select title from posts`: it types each row as `unknown`
- postgres: `select id as v from users union all select score from posts`: it types each row as `unknown`
- postgres: `select id as v from users except select author_id from posts`: it types each row as `unknown`
- mysql: `select name as v from users union select title from posts`: it types each row as `unknown`
- mysql: `select nickname as v from users union all select title from posts`: it types each row as `unknown`
- mysql: `select id as v from users union all select score from posts`: it types each row as `unknown`
- mysql: `select id as v from users except select author_id from posts`: it types each row as `unknown`
- sqlite: `select name as v from users union select title from posts`: it types each row as `unknown`
- sqlite: `select nickname as v from users union all select title from posts`: it types each row as `unknown`
- sqlite: `select id as v from users union all select score from posts`: it types each row as `unknown`
- sqlite: `select id as v from users except select author_id from posts`: it types each row as `unknown`

**Returning rows**: partly, 8 of 10

- postgres: `update users set nickname = 'anon' where id = $1 returning nickname`: `nickname` is typed `string | null`, but it is never null
- sqlite: `update users set nickname = 'anon' where id = $1 returning nickname`: `nickname` is typed `string | null`, but it is never null

**Enums**: partly, 2 of 8

- postgres: `select x.r from (select role as r from users) x`: it types each row as `unknown`
- postgres: `select status from tickets`: `status` is typed `string`, without its values
- postgres: `select 'user' as kind, name as label from users union all select 'post', title from posts`: it types each row as `unknown`
- mysql: `select x.r from (select role as r from users) x`: it types each row as `unknown`
- mysql: `select 'user' as kind, name as label from users union all select 'post', title from posts`: it types each row as `unknown`
- sqlite: `select 'user' as kind, name as label from users union all select 'post', title from posts`: it types each row as `unknown`

## TypeORM 1.1.3

**No codegen**: yes. Nothing to commit after a run.

**Live database**: no. It types each query from the schema kept in your code, and never asks the database.

**Nullable left joins**: no, 0 of 20

- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id`: it types each row as `any`
- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id order by p.title`: it types each row as `any`
- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id where u.id = $1`: it types each row as `any`
- postgres: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: it types each row as `any`
- postgres: `select u.name, x.title from users u left join (select author_id, title from posts) x on x.author_id = u.id`: it types each row as `any`
- postgres: `select u.name, p.title from posts p right join users u on u.id = p.author_id`: it types each row as `any`
- postgres: `select u.name, p.title from users u full join posts p on p.author_id = u.id`: it types each row as `any`
- mysql: `select u.name, p.title from users u left join posts p on p.author_id = u.id`: it types each row as `any`
- mysql: `select u.name, p.title from users u left join posts p on p.author_id = u.id order by p.title`: it types each row as `any`
- mysql: `select u.name, p.title from users u left join posts p on p.author_id = u.id where u.id = $1`: it types each row as `any`
- mysql: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: it types each row as `any`
- mysql: `select u.name, x.title from users u left join (select author_id, title from posts) x on x.author_id = u.id`: it types each row as `any`
- mysql: `select u.name, p.title from posts p right join users u on u.id = p.author_id`: it types each row as `any`
- sqlite: `select u.name, p.title from users u left join posts p on p.author_id = u.id`: it types each row as `any`
- sqlite: `select u.name, p.title from users u left join posts p on p.author_id = u.id order by p.title`: it types each row as `any`
- sqlite: `select u.name, p.title from users u left join posts p on p.author_id = u.id where u.id = $1`: it types each row as `any`
- sqlite: `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`: it types each row as `any`
- sqlite: `select u.name, x.title from users u left join (select author_id, title from posts) x on x.author_id = u.id`: it types each row as `any`
- sqlite: `select u.name, p.title from posts p right join users u on u.id = p.author_id`: it types each row as `any`
- sqlite: `select u.name, p.title from users u full join posts p on p.author_id = u.id`: it types each row as `any`

**Nullable expressions**: partly, 6 of 57

- postgres: `select sum(score) as v from posts`: it reports an error: Argument of type '"score"' is not assignable to parameter of type 'PickKeysByType<Post, number>'.
- postgres: `select coalesce(nickname, 'x') as v from users`: it types each row as `any`
- postgres: `select name || '!' as v from users`: it types each row as `any`
- postgres: `select nickname || '!' as v from users`: it types each row as `any`
- postgres: `select upper(name) as v from users`: it types each row as `any`
- postgres: `select upper(nickname) as v from users`: it types each row as `any`
- postgres: `select id + 1 as v from posts`: it types each row as `any`
- postgres: `select score + 1 as v from posts`: it types each row as `any`
- postgres: `select nullif(name, 'x') as v from users`: it types each row as `any`
- postgres: `select case when id > 0 then name end as v from users`: it types each row as `any`
- postgres: `select nickname is null as v from users`: it types each row as `any`
- postgres: `select (select count(*) from posts) as v`: it types each row as `any`
- postgres: `select exists (select 1 from posts) as v`: it types each row as `any`
- postgres: `select row_number() over (order by id) as v from users`: it types each row as `any`
- postgres: `select lag(id) over (order by id) as v from users`: it types each row as `any`
- postgres: `select id as v from users union all select score from posts`: it types each row as `any`
- postgres: `select max(id) as v from posts group by author_id`: it types each row as `any`
- mysql: `select sum(score) as v from posts`: it reports an error: Argument of type '"score"' is not assignable to parameter of type 'PickKeysByType<Post, number>'.
- mysql: `select coalesce(nickname, 'x') as v from users`: it types each row as `any`
- mysql: `select concat(name, '!') as v from users`: it types each row as `any`
- mysql: `select concat(nickname, '!') as v from users`: it types each row as `any`
- mysql: `select upper(name) as v from users`: it types each row as `any`
- mysql: `select upper(nickname) as v from users`: it types each row as `any`
- mysql: `select id + 1 as v from posts`: it types each row as `any`
- mysql: `select score + 1 as v from posts`: it types each row as `any`
- mysql: `select nullif(name, 'x') as v from users`: it types each row as `any`
- mysql: `select case when id > 0 then name end as v from users`: it types each row as `any`
- mysql: `select nickname is null as v from users`: it types each row as `any`
- mysql: `select (select count(*) from posts) as v`: it types each row as `any`
- mysql: `select exists (select 1 from posts) as v`: it types each row as `any`
- mysql: `select row_number() over (order by id) as v from users`: it types each row as `any`
- mysql: `select lag(id) over (order by id) as v from users`: it types each row as `any`
- mysql: `select id as v from users union all select score from posts`: it types each row as `any`
- mysql: `select max(id) as v from posts group by author_id`: it types each row as `any`
- sqlite: `select sum(score) as v from posts`: it reports an error: Argument of type '"score"' is not assignable to parameter of type 'PickKeysByType<Post, number>'.
- sqlite: `select coalesce(nickname, 'x') as v from users`: it types each row as `any`
- sqlite: `select name || '!' as v from users`: it types each row as `any`
- sqlite: `select nickname || '!' as v from users`: it types each row as `any`
- sqlite: `select upper(name) as v from users`: it types each row as `any`
- sqlite: `select upper(nickname) as v from users`: it types each row as `any`
- sqlite: `select id + 1 as v from posts`: it types each row as `any`
- sqlite: `select score + 1 as v from posts`: it types each row as `any`
- sqlite: `select nullif(name, 'x') as v from users`: it types each row as `any`
- sqlite: `select case when id > 0 then name end as v from users`: it types each row as `any`
- sqlite: `select nickname is null as v from users`: it types each row as `any`
- sqlite: `select (select count(*) from posts) as v`: it types each row as `any`
- sqlite: `select exists (select 1 from posts) as v`: it types each row as `any`
- sqlite: `select row_number() over (order by id) as v from users`: it types each row as `any`
- sqlite: `select lag(id) over (order by id) as v from users`: it types each row as `any`
- sqlite: `select id as v from users union all select score from posts`: it types each row as `any`
- sqlite: `select max(id) as v from posts group by author_id`: it types each row as `any`

**Cardinality**: no, 0 of 28. Each call picks how many rows it takes: `find` returns an array, and `findOne` at most one row.

- postgres: `select id, name from users where id = $1`: it does not infer one
- postgres: `select id from users where email = $1`: it does not infer one
- postgres: `select count(*) as v from users`: it does not infer one
- postgres: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- postgres: `select id from users`: it does not infer one
- postgres: `select id from users limit 1`: it does not infer one
- postgres: `update users set name = $1 where id = $2 returning id`: it does not infer one
- postgres: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- postgres: `select id from posts where author_id = $1`: it does not infer one
- postgres: `delete from users where id = $1`: it does not infer one
- mysql: `select id, name from users where id = $1`: it does not infer one
- mysql: `select id from users where email = $1`: it does not infer one
- mysql: `select count(*) as v from users`: it does not infer one
- mysql: `select id from users`: it does not infer one
- mysql: `select id from users limit 1`: it does not infer one
- mysql: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- mysql: `select id from posts where author_id = $1`: it does not infer one
- mysql: `delete from users where id = $1`: it does not infer one
- sqlite: `select id, name from users where id = $1`: it does not infer one
- sqlite: `select id from users where email = $1`: it does not infer one
- sqlite: `select count(*) as v from users`: it does not infer one
- sqlite: `insert into users (id, email, name) values ($1, $2, $3) returning id`: it does not infer one
- sqlite: `select id from users`: it does not infer one
- sqlite: `select id from users limit 1`: it does not infer one
- sqlite: `update users set name = $1 where id = $2 returning id`: it does not infer one
- sqlite: `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: it does not infer one
- sqlite: `select id from posts where author_id = $1`: it does not infer one
- sqlite: `delete from users where id = $1`: it does not infer one

**Nested JSON**: partly, 6 of 10

- postgres: `select u.name, jsonb_agg(jsonb_build_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id`: it types each row as `any`
- postgres: `select u.name, (select jsonb_agg(p order by p.id) from posts p where p.author_id = u.id) as v from users u`: it types each row as `any`
- mysql: `select u.name, json_arrayagg(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id, u.name`: it types each row as `any`
- sqlite: `select u.name, json_group_array(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id`: it types each row as `any`

**JSON Schema types**: no, 0 of 2

- postgres: `select payload from events`: `payload` is typed `object`
- postgres: `insert into events (id, payload) values ($1, $2)`: parameter 2 is typed `{ x: number }`, without its fields

**Aggregates**: no, 0 of 15

- postgres: `select author_id, count(*) as n from posts group by author_id`: it types each row as `any`
- postgres: `select author_id, min(title) as v from posts group by author_id`: it types each row as `any`
- postgres: `select author_id, sum(score) as v from posts group by author_id having count(*) > 1`: it types each row as `any`
- postgres: `select avg(score) as v from posts`: it reports an error: Argument of type '"score"' is not assignable to parameter of type 'PickKeysByType<Post, number>'.
- postgres: `select count(distinct author_id) as v from posts`: it types each row as `any`
- mysql: `select author_id, count(*) as n from posts group by author_id`: it types each row as `any`
- mysql: `select author_id, min(title) as v from posts group by author_id`: it types each row as `any`
- mysql: `select author_id, sum(score) as v from posts group by author_id having count(*) > 1`: it types each row as `any`
- mysql: `select avg(score) as v from posts`: it reports an error: Argument of type '"score"' is not assignable to parameter of type 'PickKeysByType<Post, number>'.
- mysql: `select count(distinct author_id) as v from posts`: it types each row as `any`
- sqlite: `select author_id, count(*) as n from posts group by author_id`: it types each row as `any`
- sqlite: `select author_id, min(title) as v from posts group by author_id`: it types each row as `any`
- sqlite: `select author_id, sum(score) as v from posts group by author_id having count(*) > 1`: it types each row as `any`
- sqlite: `select avg(score) as v from posts`: it reports an error: Argument of type '"score"' is not assignable to parameter of type 'PickKeysByType<Post, number>'.
- sqlite: `select count(distinct author_id) as v from posts`: it types each row as `any`

**Window functions**: no, 0 of 12

- postgres: `select id, row_number() over (partition by author_id order by id) as v from posts`: it types each row as `any`
- postgres: `select id, rank() over (order by score) as v from posts`: it types each row as `any`
- postgres: `select id, sum(score) over (partition by author_id order by id) as v from posts`: it types each row as `any`
- postgres: `select id, lag(title) over (order by id) as v from posts`: it types each row as `any`
- mysql: `select id, row_number() over (partition by author_id order by id) as v from posts`: it types each row as `any`
- mysql: `select id, rank() over (order by score) as v from posts`: it types each row as `any`
- mysql: `select id, sum(score) over (partition by author_id order by id) as v from posts`: it types each row as `any`
- mysql: `select id, lag(title) over (order by id) as v from posts`: it types each row as `any`
- sqlite: `select id, row_number() over (partition by author_id order by id) as v from posts`: it types each row as `any`
- sqlite: `select id, rank() over (order by score) as v from posts`: it types each row as `any`
- sqlite: `select id, sum(score) over (partition by author_id order by id) as v from posts`: it types each row as `any`
- sqlite: `select id, lag(title) over (order by id) as v from posts`: it types each row as `any`

**Subqueries**: partly, 3 of 20

- postgres: `with recent as (select id, title from posts order by id desc limit 5) select (select count(*)::int from users) as user_count, (select coalesce(jsonb_agg(jsonb_build_object('id', r.id, 'title', r.title)), '[]') from recent r) as recent_posts`: it types each row as `any`
- postgres: `select u.name, p.title, p.score from users u left join lateral (select title, score from posts where author_id = u.id order by created_at desc limit 1) p on true`: it types each row as `any`
- postgres: `select u.name, (select count(*) from posts p where p.author_id = u.id) as v from users u`: it types each row as `any`
- postgres: `select u.name, (select max(p.title) from posts p where p.author_id = u.id) as v from users u`: it types each row as `any`
- postgres: `select u.name, exists (select 1 from posts p where p.author_id = u.id) as v from users u`: it types each row as `any`
- postgres: `select x.author_id, x.n from (select author_id, count(*) as n from posts group by author_id) x`: it types each row as `any`
- mysql: `with recent as (select id, title from posts order by id desc limit 5) select (select count(*) from users) as user_count, (select coalesce(json_arrayagg(json_object('id', r.id, 'title', r.title)), json_array()) from recent r) as recent_posts`: it types each row as `any`
- mysql: `select u.name, p.title, p.score from users u left join lateral (select title, score from posts where author_id = u.id order by created_at desc limit 1) p on true`: it types each row as `any`
- mysql: `select u.name, (select count(*) from posts p where p.author_id = u.id) as v from users u`: it types each row as `any`
- mysql: `select u.name, (select max(p.title) from posts p where p.author_id = u.id) as v from users u`: it types each row as `any`
- mysql: `select u.name, exists (select 1 from posts p where p.author_id = u.id) as v from users u`: it types each row as `any`
- mysql: `select x.author_id, x.n from (select author_id, count(*) as n from posts group by author_id) x`: it types each row as `any`
- sqlite: `with recent as (select id, title from posts order by id desc limit 5) select (select count(*) from users) as user_count, (select json_group_array(json_object('id', r.id, 'title', r.title)) from recent r) as recent_posts`: it types each row as `any`
- sqlite: `select u.name, (select count(*) from posts p where p.author_id = u.id) as v from users u`: it types each row as `any`
- sqlite: `select u.name, (select max(p.title) from posts p where p.author_id = u.id) as v from users u`: it types each row as `any`
- sqlite: `select u.name, exists (select 1 from posts p where p.author_id = u.id) as v from users u`: it types each row as `any`
- sqlite: `select x.author_id, x.n from (select author_id, count(*) as n from posts group by author_id) x`: it types each row as `any`

**WITH queries**: no, 0 of 9

- postgres: `with counts as (select author_id, count(*) as n from posts group by author_id) select u.name, c.n from users u join counts c on c.author_id = u.id`: it types each row as `any`
- postgres: `with counts as (select author_id, count(*) as n from posts group by author_id) select u.name, c.n from users u left join counts c on c.author_id = u.id`: it types each row as `any`
- postgres: `with recursive nums (n) as (select 1 union all select n + 1 from nums where n < 5) select n from nums`: it types each row as `any`
- mysql: `with counts as (select author_id, count(*) as n from posts group by author_id) select u.name, c.n from users u join counts c on c.author_id = u.id`: it types each row as `any`
- mysql: `with counts as (select author_id, count(*) as n from posts group by author_id) select u.name, c.n from users u left join counts c on c.author_id = u.id`: it types each row as `any`
- mysql: `with recursive nums (n) as (select 1 union all select n + 1 from nums where n < 5) select n from nums`: it types each row as `any`
- sqlite: `with counts as (select author_id, count(*) as n from posts group by author_id) select u.name, c.n from users u join counts c on c.author_id = u.id`: it types each row as `any`
- sqlite: `with counts as (select author_id, count(*) as n from posts group by author_id) select u.name, c.n from users u left join counts c on c.author_id = u.id`: it types each row as `any`
- sqlite: `with recursive nums (n) as (select 1 union all select n + 1 from nums where n < 5) select n from nums`: it types each row as `any`

**Set operations**: no, 0 of 12

- postgres: `select name as v from users union select title from posts`: it types each row as `any`
- postgres: `select nickname as v from users union all select title from posts`: it types each row as `any`
- postgres: `select id as v from users union all select score from posts`: it types each row as `any`
- postgres: `select id as v from users except select author_id from posts`: it types each row as `any`
- mysql: `select name as v from users union select title from posts`: it types each row as `any`
- mysql: `select nickname as v from users union all select title from posts`: it types each row as `any`
- mysql: `select id as v from users union all select score from posts`: it types each row as `any`
- mysql: `select id as v from users except select author_id from posts`: it types each row as `any`
- sqlite: `select name as v from users union select title from posts`: it types each row as `any`
- sqlite: `select nickname as v from users union all select title from posts`: it types each row as `any`
- sqlite: `select id as v from users union all select score from posts`: it types each row as `any`
- sqlite: `select id as v from users except select author_id from posts`: it types each row as `any`

**Returning rows**: partly, 8 of 10

- postgres: `delete from posts where id = $1 returning title`: `title` is typed `any`
- sqlite: `delete from posts where id = $1 returning title`: `title` is typed `any`

**Enums**: partly, 2 of 8

- postgres: `select x.r from (select role as r from users) x`: it types each row as `any`
- postgres: `select status from tickets`: `status` is typed `string`, without its values
- postgres: `select 'user' as kind, name as label from users union all select 'post', title from posts`: it types each row as `any`
- mysql: `select x.r from (select role as r from users) x`: it types each row as `any`
- mysql: `select 'user' as kind, name as label from users union all select 'post', title from posts`: it types each row as `any`
- sqlite: `select 'user' as kind, name as label from users union all select 'post', title from posts`: it types each row as `any`

## The queries

Every tool gets each query on each database it supports. [probes.ts](probes.ts) holds them with their right answers.

**outer-joins**

- `select u.name, p.title from users u left join posts p on p.author_id = u.id`
- `select u.name, p.title from users u left join posts p on p.author_id = u.id order by p.title`
- `select u.name, p.title from users u left join posts p on p.author_id = u.id where u.id = $1`
- `select u.name, p.title from users u left join posts p on p.author_id = u.id where p.score > 0`
- `select u.name, x.title from users u left join (select author_id, title from posts) x on x.author_id = u.id`
- `select u.name, p.title from posts p right join users u on u.id = p.author_id`
- `select u.name, p.title from users u full join posts p on p.author_id = u.id` (Postgres and SQLite)

**expressions**

- `select count(*) as v from posts`
- `select max(id) as v from posts`
- `select sum(score) as v from posts`
- `select coalesce(nickname, 'x') as v from users`
- `select name || '!' as v from users` (Postgres and SQLite)
- `select concat(name, '!') as v from users` (MySQL)
- `select nickname || '!' as v from users` (Postgres and SQLite)
- `select concat(nickname, '!') as v from users` (MySQL)
- `select upper(name) as v from users`
- `select upper(nickname) as v from users`
- `select id + 1 as v from posts`
- `select score + 1 as v from posts`
- `select nullif(name, 'x') as v from users`
- `select case when id > 0 then name end as v from users`
- `select nickname is null as v from users`
- `select (select count(*) from posts) as v`
- `select exists (select 1 from posts) as v`
- `select row_number() over (order by id) as v from users`
- `select lag(id) over (order by id) as v from users`
- `select id as v from users union all select score from posts`
- `select max(id) as v from posts group by author_id`

**parameter-types**

- `select name from users where id = $1`
- `select id from users where name = $1`
- `insert into posts (id, author_id, title, score) values ($1, $2, $3, $4)`
- `select id from users limit $1`
- `select id from users where name like $1`
- `select id from users where external_id = $1` (Postgres)
- `select id from posts where created_at > $1` (Postgres)

**parameter-nullability**

- `insert into users (id, email, name, nickname) values ($1, $2, $3, $4)`
- `update users set nickname = $1 where id = $2`
- `select name from users where id = $1`
- `select id from users where nickname = $1`

**cardinality**

- `select id, name from users where id = $1`: at most one row
- `select id from users where email = $1`: at most one row
- `select count(*) as v from users`: exactly one row
- `insert into users (id, email, name) values ($1, $2, $3) returning id` (Postgres and SQLite): exactly one row
- `select id from users`: many rows
- `select id from users limit 1`: at most one row
- `update users set name = $1 where id = $2 returning id` (Postgres and SQLite): at most one row
- `select p.title from posts p join users u on u.id = p.author_id where p.id = $1`: at most one row
- `select id from posts where author_id = $1`: many rows
- `delete from users where id = $1`: no rows

**json-shapes**

- `select json_build_object('id', id, 'name', name) as v from users` (Postgres)
- `select json_object('id', id, 'name', name) as v from users` (MySQL and SQLite)
- `select json_agg(json_build_object('id', id, 'title', title)) as v from posts` (Postgres)
- `select json_arrayagg(json_object('id', id, 'title', title)) as v from posts` (MySQL)
- `select json_group_array(json_object('id', id, 'title', title)) as v from posts` (SQLite)
- `select u.name, jsonb_agg(jsonb_build_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id` (Postgres)
- `select u.name, json_arrayagg(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id, u.name` (MySQL)
- `select u.name, json_group_array(json_object('id', p.id, 'title', p.title)) as v from users u join posts p on p.author_id = u.id group by u.id` (SQLite)
- `select u.name, (select jsonb_agg(p order by p.id) from posts p where p.author_id = u.id) as v from users u` (Postgres)

**json-schema**

- `select payload from events` (Postgres)
- `insert into events (id, payload) values ($1, $2)` (Postgres)

**from-the-database**

- `select add_one(id) as v from users` (Postgres)
- `select id::numeric / 2 as v from users` (Postgres)
- `select array_agg(id) as v from users` (Postgres)

**aggregates**

- `select author_id, count(*) as n from posts group by author_id`
- `select author_id, min(title) as v from posts group by author_id`
- `select author_id, sum(score) as v from posts group by author_id having count(*) > 1`
- `select avg(score) as v from posts`
- `select count(distinct author_id) as v from posts`

**windows**

- `select id, row_number() over (partition by author_id order by id) as v from posts`
- `select id, rank() over (order by score) as v from posts`
- `select id, sum(score) over (partition by author_id order by id) as v from posts`
- `select id, lag(title) over (order by id) as v from posts`

**subqueries**

- `with recent as (select id, title from posts order by id desc limit 5) select (select count(*)::int from users) as user_count, (select coalesce(jsonb_agg(jsonb_build_object('id', r.id, 'title', r.title)), '[]') from recent r) as recent_posts` (Postgres)
- `with recent as (select id, title from posts order by id desc limit 5) select (select count(*) from users) as user_count, (select coalesce(json_arrayagg(json_object('id', r.id, 'title', r.title)), json_array()) from recent r) as recent_posts` (MySQL)
- `with recent as (select id, title from posts order by id desc limit 5) select (select count(*) from users) as user_count, (select json_group_array(json_object('id', r.id, 'title', r.title)) from recent r) as recent_posts` (SQLite)
- `select u.name, p.title, p.score from users u left join lateral (select title, score from posts where author_id = u.id order by created_at desc limit 1) p on true` (Postgres and MySQL)
- `select u.name, (select count(*) from posts p where p.author_id = u.id) as v from users u`
- `select u.name, (select max(p.title) from posts p where p.author_id = u.id) as v from users u`
- `select u.name, exists (select 1 from posts p where p.author_id = u.id) as v from users u`
- `select name from users where id in (select author_id from posts)`
- `select x.author_id, x.n from (select author_id, count(*) as n from posts group by author_id) x`

**with-queries**

- `with counts as (select author_id, count(*) as n from posts group by author_id) select u.name, c.n from users u join counts c on c.author_id = u.id`
- `with counts as (select author_id, count(*) as n from posts group by author_id) select u.name, c.n from users u left join counts c on c.author_id = u.id`
- `with recursive nums (n) as (select 1 union all select n + 1 from nums where n < 5) select n from nums`

**set-operations**

- `select name as v from users union select title from posts`
- `select nickname as v from users union all select title from posts`
- `select id as v from users union all select score from posts`
- `select id as v from users except select author_id from posts`

**returning**

- `insert into posts (id, author_id, title) values ($1, $2, $3) returning id, score` (Postgres and SQLite)
- `update users set nickname = $1 where id = $2 returning name, nickname` (Postgres and SQLite)
- `update users set nickname = 'anon' where id = $1 returning nickname` (Postgres and SQLite)
- `delete from posts where id = $1 returning title` (Postgres and SQLite)
- `insert into users (id, email, name) values ($1, $2, $3) on conflict (id) do update set name = excluded.name returning id, nickname` (Postgres and SQLite)

**enums**

- `select role from users` (Postgres and MySQL)
- `select x.r from (select role as r from users) x` (Postgres and MySQL)
- `select status from tickets` (Postgres)
- `select 'user' as kind, name as label from users union all select 'post', title from posts`

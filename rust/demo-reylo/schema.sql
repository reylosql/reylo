-- The demo's schema, with the rows it reads. demo-codegen reads it too.
create extension if not exists hstore;
create extension if not exists vector;
create type mood as enum ('ok', 'sad');
create table users (id int primary key, name text not null, nickname text, mood mood, joined timestamptz not null default now(), token uuid not null default gen_random_uuid(), balance numeric(10, 2) not null default 0);
create table posts (id serial primary key, author_id int not null references users (id), title text not null, tags text[] not null default '{}', status text not null default 'draft' check (status in ('draft', 'published')));
create table tasks (id serial primary key, state text not null default 'todo' check (state in ('todo', 'doing', 'done')));
-- A column a CHECK lists 200 values for, v0 to v199: casting between two of its enums stays within the compiler's default recursion limit.
do $$ begin execute format('create table codes (code text not null check (code in (%s)))', (select string_agg(quote_literal('v' || i), ', ' order by i) from generate_series(0, 199) i)); end $$;
insert into users (id, name, nickname, mood) values (1, 'ada', 'countess', 'ok'), (2, 'bob', null, 'sad');
insert into posts (author_id, title, tags) values (1, 'hello', '{intro}'), (1, 'again', '{intro,null}');
-- The geometric types and hstore, each sqlx's own type.
create table places (id int primary key, pin point not null, area box not null, route path not null, outline polygon not null, reach circle not null, edge lseg not null, axis line not null, attrs hstore not null);
insert into places values (1, '(1.5,-2)', '((3,4),(1,2))', '[(0,0),(1,1)]', '((0,0),(1,0),(0,1))', '<(1,2),3>', '[(0,0),(2,2)]', '{1,-1,0}', 'color => red, size => NULL');
-- pgvector's vectors, as the pgvector crate's Vector.
create table items (id int primary key, embedding vector(3) not null);
insert into items values (1, '[1,2.5,-3]'), (2, '[0,0,1]');

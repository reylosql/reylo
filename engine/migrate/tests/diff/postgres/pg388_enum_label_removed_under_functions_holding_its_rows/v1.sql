create type status as enum ('draft', 'live', 'gone');
create table posts (id integer primary key, status status not null default 'draft');
create function post_key(p posts) returns text language sql stable return 'post-' || p.id;
create type q as (m status, n integer);
create function nothing() returns q language sql immutable return null::q;

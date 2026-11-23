-- A user's `~~` of two texts that answers NULL, with public before pg_catalog on the search path,
-- and an implicit cast of a user's into a built-in type, so that a built-in call proves its rule
-- only over operands whose types the build names. `max` of a `name` or a `varchar` is the built-in
-- `max(text)`, so a `like` over it is the user's `~~`.
create type boxed as (v int);
create function boxed_text(boxed) returns text language sql as 'select null::text';
create cast (boxed as text) with function boxed_text(boxed) as implicit;
create function like_null(text, text) returns bool language sql immutable as 'select null::bool';
create operator public.~~ (leftarg = text, rightarg = text, function = like_null);
do $$ begin execute format('alter database %I set search_path = public, pg_catalog', current_database()); end $$;
create table names (id int primary key, n name not null, v varchar(10) not null, k int not null);
insert into names values (1, 'x', 'x', 1);

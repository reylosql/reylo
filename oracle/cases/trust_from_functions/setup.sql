-- A user's overloads of set-returning built-ins a FROM item calls, NULL where the built-ins never
-- are: an untyped literal prefers the string category, so each answers a call over one.
create table docs (id int primary key, body jsonb not null);
create function public.jsonb_each_text(text) returns table(key text, value text) language sql immutable as $$ select null::text, null::text $$;
create function public.jsonb_object_keys(text) returns setof text language sql immutable as $$ select null::text $$;

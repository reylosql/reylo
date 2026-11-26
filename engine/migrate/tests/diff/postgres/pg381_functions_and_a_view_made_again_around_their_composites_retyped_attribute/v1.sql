create type p as (a integer, b text);
create table t (id integer primary key, c p);
create function two() returns p language sql immutable return row(2, 'two')::p;
create function b_of(x p) returns text language sql immutable return (x).b;
create function named(x p default row(0, 'none')::p) returns text language plpgsql immutable as $$ begin return x.b; end $$;
create function untouched(x p) returns integer language sql immutable return 1;
create view threes as select (row(3, 'three')::p).b as b;

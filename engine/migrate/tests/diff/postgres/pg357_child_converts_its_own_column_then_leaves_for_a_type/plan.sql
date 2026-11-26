create type ty as (
  id integer,
  c p
);
alter table child alter column c type text using c::text;
alter type p alter attribute a type bigint;
alter table child alter column c type p using c::text::p;
alter table child no inherit parent;
alter table child of ty;

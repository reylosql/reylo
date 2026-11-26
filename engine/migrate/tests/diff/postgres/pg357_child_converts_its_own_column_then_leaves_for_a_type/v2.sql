create type p as (a bigint);
create type ty as (id int, c p);
create table parent (id int);
create table child of ty;

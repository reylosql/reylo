create type ty as (id int);
create table parent (a int generated always as (b + 1) stored, b int, id int);
create table child of ty;

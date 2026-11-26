create table parent (id int, secret text default ' generated always as (');
create type ty as (id int);
create table child of ty;

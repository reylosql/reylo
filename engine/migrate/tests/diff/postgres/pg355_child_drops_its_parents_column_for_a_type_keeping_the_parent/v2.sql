create table parent (id int, secret text);
create type ty as (id int);
create table child of ty;

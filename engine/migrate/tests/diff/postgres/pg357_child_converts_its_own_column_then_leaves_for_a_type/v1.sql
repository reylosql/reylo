create type p as (a int);
create table parent (id int);
create table child (c p) inherits (parent);

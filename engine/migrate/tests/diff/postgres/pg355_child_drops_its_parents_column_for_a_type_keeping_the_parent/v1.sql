create table parent (id int, secret text default ' generated always as (');
create table child (id int, secret text default ' generated always as (') inherits (parent);

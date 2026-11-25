create table parent (id int, secret text);
create table child (id int, secret text) inherits (parent);

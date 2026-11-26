create table grandparent (id int);
create table middle () inherits (grandparent);
create table leaf (id int);

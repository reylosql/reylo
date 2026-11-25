create table t (new integer, check (t.new > 0));
create table u (id integer primary key, new integer check (main.u.new > 0));

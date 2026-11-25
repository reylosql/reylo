create table t (old integer, check (t.old > 0));
create table u (id integer primary key, old integer check (main.u.old > 0));

create table a (id integer primary key, bcode int references b (code));
create table b (id integer primary key, code int, aid int references a (id));
create unique index b_code on b (code);

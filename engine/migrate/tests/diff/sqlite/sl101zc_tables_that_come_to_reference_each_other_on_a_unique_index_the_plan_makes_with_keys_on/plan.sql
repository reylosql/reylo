-- these tables reference each other, so no order fills them: their keys wait for the commit
pragma defer_foreign_keys = on;
create table new_a (id integer primary key, bcode int references new_b (code));
create table new_b (id integer primary key, code int, aid int references new_a (id));
create unique index new_b_code on new_b (code);
insert into new_a (id, bcode) select id, bcode from a;
insert into new_b (id, code, aid) select id, code, aid from b;
drop table b;
drop table a;
alter table new_a rename to a;
alter table new_b rename to b;
drop index new_b_code;
create unique index b_code on b (code);

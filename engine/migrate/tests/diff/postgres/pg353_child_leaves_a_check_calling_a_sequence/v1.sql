create sequence check_seq;
create table parent (id int, constraint ck check (id >= nextval('check_seq'::regclass)));
create table child (id int, constraint ck check (id >= nextval('check_seq'::regclass)));
alter table child inherit parent;

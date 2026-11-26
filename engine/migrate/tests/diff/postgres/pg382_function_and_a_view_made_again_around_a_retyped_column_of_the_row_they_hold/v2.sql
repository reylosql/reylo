create table t (id integer primary key, n bigint);
create function blank() returns t language sql stable return row(0, 0)::t;
create view blanks as select row(0, 0)::t as r;

create function zero() returns integer language sql immutable return 0;
create table t (id integer primary key, n bigint default zero());
create function blank() returns t language sql stable return row(0, 0)::t;
create function blank_id() returns integer language sql stable return (row(0, 0)::t).id;

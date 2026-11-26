-- loses data: lit_a
-- loses data: lit
-- loses data: tree_1
-- loses data: tree_2
-- loses data: tree
-- loses data: typed_1
-- loses data: typed
drop table lit_a;
drop table lit;
drop table tree_1;
drop table tree_2;
drop table tree;
drop table typed_1;
drop table typed;
create table lit (
  new_id text
) partition by list (((new_id || 'new_id'::text)));
create table lit_a partition of lit for values in ('anew_id');
create table tree (
  new_id integer,
  n text
) partition by range (new_id);
create table tree_1 partition of tree for values from (0) to (100);
create table tree_2 (
  new_id integer,
  n text
);
create table typed (
  new_id bigint
) partition by range (new_id);
create table typed_1 partition of typed for values from ('0') to ('100');

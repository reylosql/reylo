create table lit (new_id text) partition by list ((new_id || 'new_id'));
create table lit_a partition of lit for values in ('anew_id');
create table typed (new_id bigint) partition by range (new_id);
create table typed_1 partition of typed for values from (0) to (100);
create table tree (new_id int, n text) partition by range (new_id);
create table tree_1 partition of tree for values from (0) to (100);
create table tree_2 (new_id int, n text);

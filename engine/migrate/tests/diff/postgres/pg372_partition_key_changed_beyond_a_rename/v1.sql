create table lit (old_id text) partition by list ((old_id || 'old_id'));
create table lit_a partition of lit for values in ('aold_id');
create table typed (old_id int) partition by range (old_id);
create table typed_1 partition of typed for values from (0) to (100);
create table tree (old_id int, n text) partition by range (old_id);
create table tree_1 partition of tree for values from (0) to (100);
create table tree_2 partition of tree for values from (100) to (200);

create table q (new_id int, n text) partition by range (new_id);
create table q_1 partition of q for values from (0) to (100);

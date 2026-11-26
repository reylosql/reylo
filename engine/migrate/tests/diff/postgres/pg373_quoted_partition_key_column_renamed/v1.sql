create table q ("old""id" int, n text) partition by range ("old""id");
create table q_1 partition of q for values from (0) to (100);

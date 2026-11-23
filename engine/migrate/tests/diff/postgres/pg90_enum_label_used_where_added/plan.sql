-- file 1 of 2
alter type mood add value 'meh' after 'sad';
alter type st add value 'c' after 'b';
-- file 2 of 2
alter table t add column m2 mood default 'meh';
alter table t alter column s set default 'c';

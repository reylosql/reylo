-- file 1 of 2
alter type mood add value 'b,c' after 'a';
-- file 2 of 2
alter table t alter column m set default '{"b,c"}'::mood[];

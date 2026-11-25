alter table t drop column m;
alter type mood rename to mood__old;
create type mood as enum ('a', 'b');
drop type mood__old;
alter table t add column m mood generated always as ('a'::mood) stored;
alter table t1 alter column m set expression as ('b'::mood);

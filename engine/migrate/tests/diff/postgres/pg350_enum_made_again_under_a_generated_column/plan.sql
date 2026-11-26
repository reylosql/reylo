alter table t drop column m;
alter type mood rename to mood__old;
create type mood as enum ('a', 'b');
alter table t alter column n type mood using n::text::mood;
drop type mood__old;
alter table t add column m mood generated always as ('a'::mood) stored;

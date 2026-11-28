-- Syntax Postgres 19 adds: ON CONFLICT … DO SELECT returns the row that conflicts, as it stands, or none for one its WHERE rejects; IGNORE NULLS and RESPECT NULLS on a window function.
create table kv (k int primary key, v text, n int not null default 0);
insert into kv values (1, 'one', 1), (2, null, 2);

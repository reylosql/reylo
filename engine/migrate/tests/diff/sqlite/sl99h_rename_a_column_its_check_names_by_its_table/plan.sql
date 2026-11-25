-- t.old is renamed new, keeping its rows. If they are two columns, drop the one and add the other instead.
alter table t rename column old to new;
-- u.old is renamed new, keeping its rows. If they are two columns, drop the one and add the other instead.
alter table u rename column old to new;

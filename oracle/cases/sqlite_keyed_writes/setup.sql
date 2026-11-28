-- A write returns a row per row it writes. An INSERT ... SELECT writes a row per row its SELECT returns, so it returns one where the SELECT does: one with no FROM, or a WITH query of one row; at most one under a WHERE, a LIMIT 1 or a pinned key; and at most one again where a conflict or a trigger can skip the row. An UPDATE ... FROM writes each target row once, whatever it joins, so a key of the target pinned through the join, or to a subquery's value, bounds what it returns.
create table accounts (id integer primary key, name text not null);
create table users (id integer primary key, account_id integer not null references accounts (id), email text unique, name text);
create table notes (id integer primary key, user_id integer not null, body text not null);
create table skipping (id integer primary key, name text not null);
create trigger skip before insert on skipping begin select raise(ignore) where new.name = 'skip'; end;
insert into accounts values (1, 'a'), (2, 'b');
insert into users values (1, 1, 'a@x', 'n'), (2, 2, null, null), (3, 1, 'c@x', 'c');
insert into notes values (1, 1, 'x'), (2, 1, 'y'), (3, 2, 'z'), (4, 3, 'c'), (5, 1, 'n');

-- What Postgres reads as a statement's text: a carriage return ends a `--` comment as a line feed
-- does, a vertical tab is whitespace (from Postgres 17), a name and a dollar quote's tag take every
-- byte past ASCII and a name a `$`, a block comment nests, and a no-break space is a character of
-- the name it ends.
create table t (id int primary key, tags int[] not null, café int not null, fin int not null);

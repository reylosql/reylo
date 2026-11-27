-- What SQLite reads as a statement's text: a block comment ends at its first `*/`, a `--` comment
-- runs to the line feed, a name takes a `$` and every byte past ASCII, `e'…'` is a name and a
-- string, and a no-break space is a character of the name it ends.
create table t (id integer primary key, e integer not null, "a$$b" integer not null);

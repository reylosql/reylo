-- What MySQL reads as a statement's text: `--` before a control character, DEL included, starts a
-- comment, a vertical tab is whitespace, a name takes a `$`, and a no-break space is a character
-- of the name it ends.
create table t (id int primary key, `a$$b` int not null);

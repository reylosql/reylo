-- SQLite converts a value to a number before it computes with it, and an INTEGER stays one:
-- ceil, floor and trunc hand an INTEGER back as it is, and sum of text that reads as integers, or
-- s + 0, is an INTEGER. A REAL on either side of an operator makes it a REAL, and coalesce, CASE
-- and max hand back each value's own storage class. abs converts nothing: text gives a REAL, and an
-- INTEGER stays one. The table is not STRICT, so the NUMERIC column d, the column v with no type
-- and the BLOB column b hold either.
create table t (id integer primary key, n integer not null, x real not null, s text, d decimal(10, 2), v, b blob);
insert into t values (1, 9007199254740993, 1.5, '9007199254740993', 9007199254740993, 9007199254740993, 9007199254740993), (2, -3, -2.5, '2.5', 1.25, 2.5, x'01'), (3, 4, 4.0, 'abc', 'abc', 'abc', 'abc');

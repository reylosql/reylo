-- `year(d)` is a YEAR, which MySQL before 9.2 merges with a TINYINT into a TINYINT too, so 2026
-- arrives as -22 and 9999 as 15, on a database with no YEAR column. Where a statement names a
-- YEAR, a TINYINT computed in a derived table is of no known type; where nothing reaches one, it
-- keeps its type.
create table t (id int primary key, d date not null, ti tinyint);
insert into t values (1, '9999-12-31', -128), (2, '2026-01-01', null), (3, '1000-01-01', 127);

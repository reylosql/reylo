-- MySQL before 9.2 merges a YEAR with a TINYINT, in coalesce, ifnull, if, case, greatest and least
-- and in a UNION's column, into a TINYINT, which the binary protocol sends in one byte: 2026
-- arrives as -22, and a UNION stores 127. The prepare describes such a column as it does two
-- TINYINTs merged, so where a YEAR may reach it the column is of no known type: an expression of
-- the select list where a YEAR is among the values it merges, and a column computed in a view, a
-- derived table, a WITH query or a UNION. A document holds the whole year, as the SMALLINT MySQL
-- 9.2 merges them into. Two TINYINTs merged, and a YEAR beside a SMALLINT, keep their types.
create table x (id int primary key, y year, ti tinyint, tu tinyint unsigned, b boolean, s smallint);
create view merged as select id, coalesce(y, ti) as v from x;
insert into x values (1, 1901, -128, 0, false, 1), (2, 2026, null, null, null, null), (3, 2155, 127, 255, true, 2);

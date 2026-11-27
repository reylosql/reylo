-- MySQL reads a WITH query's names again where the query is read, so inside a derived table, a
-- LATERAL one, a subquery or another WITH query whose own WITH declares a name, the WITH query
-- reads that name as the inner one: here `t4` is the table where the outer WITH stands and a query
-- of NULLs inside.
create table t3 (k int not null);
create table t4 (k int not null, x int not null);

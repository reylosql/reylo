-- A set of no rows in an inline window's PARTITION BY leaves a scalar subquery no row, so an array of it holds NULL. PostgreSQL 19 refuses a set-returning call in a window definition.
select 1;

-- max of an int4 is the built-in max(int4), and an int4's = is the built-in's.
select max(id) = 1 as result from names group by k

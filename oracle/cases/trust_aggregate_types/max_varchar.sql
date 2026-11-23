-- max(varchar) is max(text) too.
select max(v) like 'x' as result from names group by k

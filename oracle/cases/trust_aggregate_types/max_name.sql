-- max(name) is max(text), whose like is the user's ~~.
select coalesce(max(n), 'y'::name) like 'x'::text as result from names

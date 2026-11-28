-- MySQL 9.6 moved md5 and sha1 out of the server, which then refuses this statement unless a
-- component, a loadable function or a stored function answers for them.
select md5(name) as p, sha1(name) as q from t where n between -1 and 2

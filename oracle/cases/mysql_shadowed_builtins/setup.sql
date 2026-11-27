-- MySQL 9.6 moved md5, sha and sha1 out of the server, into a component that is not installed by
-- default. There a call of one written without a schema is answered by a loadable function of the
-- name or, as here, by the database's stored function, which need not be NULL for NULL: no rule of
-- the built-in holds of it.
create table users (id int auto_increment primary key, name varchar(20) not null, email varchar(40) unique);
create table posts (id int auto_increment primary key, user_id int not null);
create function md5(s text) returns text deterministic no sql return coalesce(s, 'n');
create function sha(s text) returns text deterministic no sql return coalesce(s, 'n');
create function sha1(s text) returns text deterministic no sql return coalesce(s, 'n');

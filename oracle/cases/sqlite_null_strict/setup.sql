-- Outer joins whose WHERE calls a built-in or an operator on a column of the null-extended side,
-- joined past the seeded keys (`p.user_id + 1000`) so that the rows a post finds are the ones that
-- hold the edge values, NULL among them, and most posts find none.
-- Where SQLite answers NULL for that column NULL, the WHERE rejects the null extension and the
-- column is not NULL; where some other argument settles the answer, both stay nullable.
create table users (id integer primary key, name text not null, email text, score real, doc text, flags integer, born text);
create table posts (id integer primary key, user_id integer not null, title text not null);

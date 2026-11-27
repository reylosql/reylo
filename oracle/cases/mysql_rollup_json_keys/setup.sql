-- WITH ROLLUP nulls each GROUP BY item in its super-aggregate rows, and with it each expression of
-- the select list the server takes for that item, at any depth. It compares them as it resolves
-- them: a key in another letter case, an alias inside an expression, in another letter case past
-- ASCII or written with leading spaces, an item with no alias named by its text, and a column of
-- a derived table, a WITH query or a view, a subquery's too, that selects the expression are the
-- same one. Without ONLY_FULL_GROUP_BY, such an expression may stand inside another beside its
-- key. The whole key is NULL, a fallback it holds for NULL too, and a window call reads those rows.
create table posts (id int primary key, title varchar(20) not null);
create view titled as select id, title, json_object('t', title) as k from posts;

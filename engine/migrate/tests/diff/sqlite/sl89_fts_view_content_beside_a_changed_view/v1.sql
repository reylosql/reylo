create table a (id integer primary key, body text);
create view w as select 1 as k;
create view s as select a.id, a.body from a join w on w.k = 1;
create virtual table search using fts5(body, content='s', content_rowid='id');

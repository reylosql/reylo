create table docs_content (id integer primary key, body text);
create table other (id integer primary key, body text);
create virtual table docs using fts5(body, content='docs_content', content_rowid='id');

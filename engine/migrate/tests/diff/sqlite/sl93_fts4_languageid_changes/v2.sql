create table notes (id integer primary key, body text, lid integer generated always as (2) virtual);
create virtual table notes_fts using fts4(body, content='notes', languageid='lid');

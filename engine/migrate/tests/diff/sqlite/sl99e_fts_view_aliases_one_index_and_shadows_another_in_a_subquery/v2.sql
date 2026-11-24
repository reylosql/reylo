create table a_input (id integer primary key, extra blob);
create table z_notes (id integer primary key, body text, search_id integer generated always as (id + 20) virtual);
create view z_source as select search_id as id, body from z_notes as filtered;
create virtual table z_root using fts5(body, content='z_source', content_rowid='id');
create view hits as select z_root.rowid as rowid, z_root.body from a_input join z_root on z_root.rowid % 10 = a_input.id where z_root match 'published' and exists (with z_root as (select 1) select * from z_root);
create virtual table filtered using fts4(body, content='hits');

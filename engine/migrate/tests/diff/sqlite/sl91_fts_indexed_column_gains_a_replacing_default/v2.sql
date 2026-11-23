create table notes (id integer primary key, body text not null on conflict replace default 'substitute');
create virtual table notes_fts using fts5(body, content='notes', content_rowid='id');

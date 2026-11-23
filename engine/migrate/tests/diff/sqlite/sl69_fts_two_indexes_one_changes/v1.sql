create table notes (title text, body text);
create virtual table body_fts using fts5(body, content='notes');
create virtual table title_fts using fts4(title, content="notes");

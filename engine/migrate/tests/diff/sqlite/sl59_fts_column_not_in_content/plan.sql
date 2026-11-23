create virtual table notes_fts using fts5(body, tags unindexed, content='notes', content_rowid='id');

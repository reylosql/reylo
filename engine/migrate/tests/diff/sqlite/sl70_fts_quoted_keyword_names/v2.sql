create table "My Notes" (Id integer primary key, "Body Text" text);
create virtual table "order" using fts5("Body Text", content='My Notes');

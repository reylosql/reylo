create table "My Notes" ("Body Text" text);
create virtual table "order" using fts5("Body Text", content='My Notes');

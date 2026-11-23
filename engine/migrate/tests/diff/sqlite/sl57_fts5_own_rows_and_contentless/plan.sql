-- loses data: bare
-- loses data: docs
drop table bare;
drop table docs;
create virtual table bare using fts5(body, content='', tokenize='porter');
create virtual table docs using fts5(body, tokenize='porter');

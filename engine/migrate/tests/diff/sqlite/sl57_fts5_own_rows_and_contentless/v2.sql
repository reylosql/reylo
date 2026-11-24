create virtual table docs using fts5(body, tokenize='porter');
create virtual table bare using fts5(body, content='', tokenize='porter');

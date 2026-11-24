create table accounts (id int primary key, code int default 2, constraint accounts_code_key unique (code));

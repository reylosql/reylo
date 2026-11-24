create table accounts (id int primary key, old_code int default 1, constraint accounts_code_key unique (old_code));

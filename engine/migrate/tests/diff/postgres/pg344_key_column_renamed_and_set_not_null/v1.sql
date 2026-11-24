create table accounts (id int primary key, old_code int, constraint accounts_code_key unique (old_code));

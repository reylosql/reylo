create table accounts (id int primary key, code int not null, constraint accounts_code_key unique (code));

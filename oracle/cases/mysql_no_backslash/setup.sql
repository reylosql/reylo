-- Under NO_BACKSLASH_ESCAPES a backslash is a character like any other, so `'x\'` is a whole
-- string and what follows it is SQL.
create table users (id int primary key, email varchar(40) not null unique, name varchar(40) not null);
-- A table whose name holds a quote, which a catalog query writes doubled, as every mode reads it.
create table `book's` (id int primary key, u16 varchar(4) character set utf16, plain varchar(4));

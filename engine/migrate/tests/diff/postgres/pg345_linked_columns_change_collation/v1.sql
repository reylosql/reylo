create collation ci (provider = icu, locale = 'und-u-ks-level2', deterministic = false);
create table parents (id text primary key);
create table children (id int primary key, parent_id text references parents(id));

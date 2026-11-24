create collation ci (provider = icu, locale = 'und-u-ks-level2', deterministic = false);
create table parents (id text collate ci primary key);
create table children (id int primary key, parent_id text collate ci references parents(id));

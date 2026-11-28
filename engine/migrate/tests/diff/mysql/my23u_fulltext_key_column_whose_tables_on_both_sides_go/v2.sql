create table a (id int primary key, seq int not null auto_increment, body text, unique key (seq), fulltext key body_ft (body));

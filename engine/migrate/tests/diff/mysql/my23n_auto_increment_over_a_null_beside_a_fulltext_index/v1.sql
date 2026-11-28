create table p (id int primary key); create table c (id int primary key, seq int, body text, unique key (seq), fulltext key body_ft (body), constraint c_seq foreign key (seq) references p (id));

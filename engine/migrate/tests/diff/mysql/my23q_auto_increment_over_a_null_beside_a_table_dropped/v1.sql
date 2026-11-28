create table p (id int primary key); create table spare (id int primary key); create table c (id int primary key, seq int, constraint c_seq foreign key (seq) references p (id) on delete set null);

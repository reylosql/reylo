create table p (id int primary key); create table c (id int primary key, seq int, foreign key (seq) references p (id));

create table p (id int primary key, u int, unique key (u)); create table c (id int primary key, pu int, foreign key (pu) references p (u));

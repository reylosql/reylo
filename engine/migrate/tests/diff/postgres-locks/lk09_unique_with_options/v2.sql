create table users (id bigint primary key, n int not null, constraint users_n_key unique nulls not distinct (n) include (id));

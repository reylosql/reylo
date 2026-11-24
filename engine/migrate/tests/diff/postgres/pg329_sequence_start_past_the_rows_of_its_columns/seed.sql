insert into orders (n) values ('a'), ('b');
insert into orders select g, 'imported' from generate_series(1000, 1100) g;
insert into things (n) values ('a');
insert into things select g, 'imported' from generate_series(1000, 1100) g;

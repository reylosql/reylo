insert into base_v (n) select 'x' from generate_series(1, 5);
insert into base select g, 'hidden' from generate_series(100, 200) g;
insert into orders (n) select 'x' from generate_series(1, 5);
insert into orders select g, 'imported' from generate_series(100, 200) g;

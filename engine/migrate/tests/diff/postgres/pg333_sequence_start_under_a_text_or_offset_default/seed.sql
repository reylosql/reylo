insert into tags (n) select 'x' from generate_series(1, 20);
insert into tags (code, n) select g::text, 'imported' from generate_series(100, 1100) g;
insert into far (n) select 'x' from generate_series(1, 5);

insert into both_moved (n) values ('a'), ('b');
insert into both_moved select g, 'imported' from generate_series(100, 200) g;
insert into start_only (n) values ('a'), ('b');
insert into start_only select g, 'imported' from generate_series(100, 200) g;

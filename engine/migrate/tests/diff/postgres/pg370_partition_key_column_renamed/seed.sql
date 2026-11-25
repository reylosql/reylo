insert into m select g, date '2025-01-01' + g from generate_series(1, 50) g;
insert into m select g, date '2027-01-01' + g from generate_series(51, 100) g;
insert into h select g, 'eu' from generate_series(1, 20) g;

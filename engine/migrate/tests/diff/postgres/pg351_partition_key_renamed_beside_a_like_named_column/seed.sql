insert into m select g, date '2025-01-01' + g, g from generate_series(1, 20) g;

insert into events (at, payload) select date '2026-01-01' + (g % 300), 'x' from generate_series(1, 1000) g;
insert into log (msg) select 'y' from generate_series(1, 1000) g;

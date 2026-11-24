insert into explicit (n) values ('a'), ('b');
insert into explicit (id, n) values (2000, 'c');
insert into ahead (n) select 'r' from generate_series(1, 1500);
insert into lowered (n) values ('a');
select setval(pg_get_serial_sequence('jumped', 'id'), 5000, false);
select setval('counter_ahead', 3000);

-- a ring buffer that has wrapped: ids 51..100 and 1..10, the sequence at 10
insert into ring (n) select 'r' from generate_series(1, 100);
delete from ring where id <= 50;
insert into ring (n) select 'r' from generate_series(1, 10);
-- a sentinel row at the type's top
insert into sentinel (n) values ('a');
insert into sentinel values (2147483647, 'sentinel');
-- an imported row past the sequence's maxvalue
insert into capped (n) values ('a');
insert into capped values (5000, 'imported');
-- a row at the type's bottom
insert into down (n) values ('a');
insert into down overriding system value values (-2147483648, 'floor');

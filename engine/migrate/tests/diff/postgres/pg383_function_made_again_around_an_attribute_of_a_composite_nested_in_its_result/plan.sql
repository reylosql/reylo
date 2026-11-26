set check_function_bodies = off;
drop function listed();
drop function nested();
drop function wrapped();
alter type p alter attribute a type bigint;
create function listed() returns p[] language sql immutable return array[row(1)::p, row(2)::p];
create function nested() returns q language sql immutable return row(row(1)::p, 'one')::q;
create function wrapped() returns d language sql immutable return row(3)::p::d;
reset check_function_bodies;

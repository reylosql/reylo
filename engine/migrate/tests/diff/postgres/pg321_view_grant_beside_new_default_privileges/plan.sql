create schema reports;
create table bookings (
  id integer not null,
  room integer not null,
  total numeric not null,
  constraint bookings_pkey primary key (id)
);
create table reports.daily (
  day date not null,
  constraint daily_pkey primary key (day)
);
create view room_totals as select room, sum(total) as total from bookings group by room;
create view rooms as select distinct room from bookings;
alter default privileges grant select on tables to diff_reporting;
alter default privileges in schema reports grant insert on tables to diff_app;
grant insert on table bookings to diff_app;
grant insert on table reports.daily to diff_app;
grant select on table bookings to diff_app;
grant select on table room_totals to diff_reporting;
grant select on table rooms to diff_reporting;
grant select on table reports.daily to diff_reporting;
revoke select on table bookings from diff_reporting;
grant update (total) on table bookings to diff_app;

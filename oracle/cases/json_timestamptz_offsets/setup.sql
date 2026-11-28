-- A timestamptz inside a document, which Postgres writes in the session's zone: under a POSIX zone
-- whose offset reaches 168 hours, and an hour more in its summer, the offset's hours take three
-- digits (`2026-07-08T12:59:00+168:59`).
set timezone = 'XXX-167:59YYY';
create table visit (id int primary key, at timestamptz not null);
insert into visit values (1, '2026-01-01 12:00+00'), (2, '2026-07-01 12:00+00'), (3, '2026-07-01 12:00:00.5+05:30');

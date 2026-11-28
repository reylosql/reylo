-- A time with a time zone inside a document, which Postgres writes as its type's own text: its
-- offset in hours, with its minutes and its seconds where they are not zero (`12:00:00+00`),
-- whereas a timestamptz's there always has its minutes (`2026-01-01T12:00:00+00:00`).
create table shift (id int primary key, starts timetz not null);
insert into shift values (1, '12:00:00+00'), (2, '09:00:00-08'), (3, '12:00:00.25+05:30'), (4, '18:00:00-03:30'), (5, '12:00:00+05:30:15'), (6, '12:00:00-04:56:02'), (7, '24:00:00+00');

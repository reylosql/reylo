create table events (at date not null, id bigint generated always as identity, payload text, primary key (at, id)) partition by range (at);
create table events_2026 partition of events for values from ('2026-01-01') to ('2027-01-01');
create table log (id bigint generated always as identity, msg text);

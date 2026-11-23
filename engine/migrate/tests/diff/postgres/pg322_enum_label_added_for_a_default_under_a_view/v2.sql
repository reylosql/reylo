create type visibility as enum ('private', 'unlisted', 'public', 'team', 'org');
create table users (id integer primary key, handle text not null);
create table bookmarks (
  id integer primary key,
  owner_id integer not null references users (id),
  url text not null,
  visibility visibility not null default 'org'
);
create view public_bookmarks as
  select b.id, b.url, u.handle as owner
  from bookmarks b join users u on u.id = b.owner_id
  where b.visibility = 'public';
create materialized view bookmark_counts as select visibility, count(*) as n from bookmarks group by visibility;

create type visibility as enum ('private', 'unlisted', 'public', 'team', 'org', 'friends');
create domain shared_visibility as visibility check (value <> 'private');
create table users (id integer primary key, handle text not null);
create table bookmarks (
  id integer primary key,
  owner_id integer not null references users (id),
  url text not null,
  visibility visibility not null default 'private' check (visibility <> 'team'),
  shared shared_visibility
);
alter table bookmarks enable row level security;
create policy see_public on bookmarks using (visibility = 'public');
create index bookmarks_public on bookmarks (id) where visibility = 'public';
create function is_public(v visibility) returns boolean language sql immutable return v = 'public';
create function touch() returns trigger language plpgsql as $$ begin return new; end $$;
create trigger bookmarks_private before update on bookmarks for each row when (new.visibility = 'private') execute function touch();
create view public_bookmarks as
  select b.id, b.url, u.handle as owner
  from bookmarks b join users u on u.id = b.owner_id
  where b.visibility = 'public';
grant select on table public_bookmarks to diff_reporting;
comment on view public_bookmarks is 'Bookmarks anyone may see';
create materialized view bookmark_counts as select visibility, count(*) as n from bookmarks group by visibility;

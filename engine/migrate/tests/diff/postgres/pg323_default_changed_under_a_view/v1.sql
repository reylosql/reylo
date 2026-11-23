create table scores (
  id integer primary key,
  score integer not null default 0,
  doubled integer generated always as (score * 2) stored
);
alter table scores enable row level security;
create policy positive on scores using (score > 0);
create function touch() returns trigger language plpgsql as $$ begin return new; end $$;
create trigger scores_zero before update on scores for each row when (new.score = 0) execute function touch();
create view high_scores as select id, score from scores where score > 10;

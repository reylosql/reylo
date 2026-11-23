do $$ begin if not exists (select from pg_roles where rolname = 'diff_reporting') then create role diff_reporting; end if; end $$;
do $$ begin if not exists (select from pg_roles where rolname = 'diff_app') then create role diff_app; end if; end $$;

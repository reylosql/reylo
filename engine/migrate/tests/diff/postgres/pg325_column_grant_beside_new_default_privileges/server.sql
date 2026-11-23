do $$ begin if not exists (select from pg_roles where rolname = 'diff_reader') then create role diff_reader; end if; end $$;

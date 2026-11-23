do $$ begin if not exists (select from pg_roles where rolname = 'diff_reader') then create role diff_reader; end if; end $$;
do $$ begin if not exists (select from pg_roles where rolname = 'diff_owner') then create role diff_owner; end if; end $$;

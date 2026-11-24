do $$ begin
  -- moved before the alter that locks each table, where no index leads with its id
  if concat_ws(' ', nextval(pg_get_serial_sequence('events', 'id')), nextval(pg_get_serial_sequence('log', 'id'))) <> '5000000 5000000' then
    raise exception 'a start was not moved';
  end if;
end $$;

do $$
declare
  got text := concat_ws(' ',
    nextval(pg_get_serial_sequence('ring', 'id')), nextval(pg_get_serial_sequence('sentinel', 'id')),
    nextval(pg_get_serial_sequence('capped', 'id')), nextval(pg_get_serial_sequence('down', 'id')));
begin
  -- rows past the bounds, or at the end of the type, are passed over: ring has wrapped and stays, the others move to the start
  if got <> '11 10 10 -10' then
    raise exception 'the next values are %', got;
  end if;
end $$;

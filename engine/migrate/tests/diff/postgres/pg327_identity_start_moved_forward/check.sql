do $$
declare
  got text := concat_ws(' ',
    nextval(pg_get_serial_sequence('fresh', 'id')), nextval(pg_get_serial_sequence('explicit', 'id')), nextval(pg_get_serial_sequence('ahead', 'id')),
    nextval(pg_get_serial_sequence('lowered', 'id')), nextval(pg_get_serial_sequence('jumped', 'id')), nextval(pg_get_serial_sequence('down', 'id')),
    nextval('counter'), nextval('counter_ahead'));
begin
  -- a new start where the sequence hasn't reached it, past the rows, and never back
  if got <> '1000 2001 1501 101 5000 -1000 1000 3001' then
    raise exception 'the next values are %', got;
  end if;
end $$;

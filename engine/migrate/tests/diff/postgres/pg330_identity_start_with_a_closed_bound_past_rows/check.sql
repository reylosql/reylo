do $$
declare
  got text := concat_ws(' ', nextval(pg_get_serial_sequence('start_only', 'id')), nextval(pg_get_serial_sequence('both_moved', 'id')));
begin
  -- each moves past the rows, the one whose minvalue closed in to its start too
  if got <> '201 201' then
    raise exception 'the next values are %', got;
  end if;
end $$;

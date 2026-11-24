do $$
declare
  got text := concat_ws(' ', nextval('things_id_seq'), nextval('order_no'));
begin
  -- past the rows of the column each gives values to, a serial's and a default's
  if got <> '1101 1101' then
    raise exception 'the next values are % (a value of 1000 to 1100 collides with a row)', got;
  end if;
end $$;

do $$
declare got bigint := nextval('inv_no');
begin
  -- a default that makes text of it says nothing of where its rows are: it stays where it was
  if got <> 6 then raise exception 'the next value is %', got; end if;
end $$;

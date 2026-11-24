do $$
declare
  tag bigint := nextval('tag_no');
  far bigint := nextval('far_no');
begin
  -- neither column holds the sequence's values as they are, so neither sequence moves
  if tag <> 21 or exists (select from tags where code = tag::text) then
    raise exception 'tag_no gives %', tag;
  end if;
  if far <> 6 then
    raise exception 'far_no gives %', far;
  end if;
end $$;

do $$
declare
  v bigint := nextval('vseq');
  d bigint := nextval('dseq');
begin
  -- a view's default and a domain's say nothing of where the rows are: each stays where it was
  if v <> 6 or d <> 6 then
    raise exception 'vseq gives %, dseq gives %', v, d;
  end if;
end $$;

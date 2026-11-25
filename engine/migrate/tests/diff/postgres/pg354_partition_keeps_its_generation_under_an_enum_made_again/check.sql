do $$ begin
  if (select m from t1 where id = 1)::text is distinct from 'b' then
    raise exception 'the partition lost its own generation expression';
  end if;
end $$;

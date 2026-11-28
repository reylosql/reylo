select jsonb_agg(s.starts order by s.id) as starts from shift s

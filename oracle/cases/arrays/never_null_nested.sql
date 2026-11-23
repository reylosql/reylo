select array_agg(array[jsonb_build_object('a', m.id)]) as grid, array_agg(case when m.nickname is not null then concat(m.nickname) end) as maybe, array_agg(m.nickname || 'x') as joined from members m

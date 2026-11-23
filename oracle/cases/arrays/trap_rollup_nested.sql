select array[[length(m.email)::int]] as a from members m group by rollup (length(m.email)::int)

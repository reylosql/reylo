select jsonb_agg(v.at order by v.id) as ats from visit v

-- Over a jsonb column the built-in answers.
select e.key from docs d, jsonb_each(d.body) e

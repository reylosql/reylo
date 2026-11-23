select array[array_append(t.scores::int[], 2)] as a from teams t group by grouping sets ((1, array_append(t.scores::int[], 2)), (1))

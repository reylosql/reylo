select array[array_append(t.scores, 2147483648::bigint)] as a, array[t.scores || 2147483648::bigint] as b from teams t

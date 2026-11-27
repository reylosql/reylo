select x.v from lo a, lateral (select A.v from LI A where a.id = 1) x where a.id = 1

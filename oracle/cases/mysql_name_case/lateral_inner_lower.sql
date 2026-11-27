select x.v from lo A, lateral (select a.v from LI a where A.id = 1) x where A.id = 1

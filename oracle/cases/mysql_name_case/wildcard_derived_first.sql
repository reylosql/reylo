select d.kid from (select x.* from (select id as kid, id as u2 from KX) X, kx x where u = u2) d where kid = 1

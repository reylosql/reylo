select d.kid from (select x.* from kx x, (select id as kid, id as u2 from KX) X where u = u2) d where kid = 1

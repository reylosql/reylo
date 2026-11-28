with touched as (update parent set y = 1 where id = 0 returning id) insert into parent (id, x, y) values (:id, :x, :y)

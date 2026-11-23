insert into req (id, name) values (:id, :name) on conflict (id) do update set name = excluded.name

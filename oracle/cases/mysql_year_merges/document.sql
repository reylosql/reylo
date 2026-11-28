select json_object('v', coalesce(y, ti), 't', ti) as o, :n as n from x

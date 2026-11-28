select json_object('plain', d.plain, 'u16', d.u16) as doc
from (select u16 as plain, plain as u16 from wide_sets) d

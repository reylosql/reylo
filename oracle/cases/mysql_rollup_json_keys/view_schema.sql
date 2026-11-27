select json_object('t', title) as o, count(*) as n from titled group by reylo_oracle_mysql_rollup_json_keys.titled.k with rollup

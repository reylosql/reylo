select d.x from E D, json_table('["zz"]', '$[*]' columns (x enum('small1', 'zz') path '$')) d

-- md5 compares as text where it is the built-in. MySQL 9.6 moved it out of the server, and there
-- the engine reads it as no built-in (mysql_shadowed_builtins).
select id from users where email = md5(:e)

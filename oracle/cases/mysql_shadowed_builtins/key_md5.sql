-- The stored function is no built-in the engine knows how the server compares, so its value bounds
-- no read by the unique key.
select id from users where email = md5(:e)

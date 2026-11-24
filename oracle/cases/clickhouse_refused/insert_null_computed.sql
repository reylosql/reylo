INSERT INTO users (id, name) SELECT {id:UInt64}, nullIf({name:Nullable(String)}, '')

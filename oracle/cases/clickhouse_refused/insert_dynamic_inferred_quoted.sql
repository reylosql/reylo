INSERT INTO things (id, d) SELECT {id:UInt64}, {s:String} SETTINGS `cast_string_to_dynamic_use_inference` = 1

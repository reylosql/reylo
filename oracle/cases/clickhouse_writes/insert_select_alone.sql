INSERT INTO orders (id, user_id, total, note, placed) SELECT {id:UInt64}, {user:UInt64} AS user_id, {total:Decimal(18, 2)}, {note:Nullable(String)}, {placed:Date}

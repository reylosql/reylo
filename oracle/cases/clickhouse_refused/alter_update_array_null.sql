ALTER TABLE things UPDATE tags = arrayMap(x -> nullIf(x, 5), arrayConcat(tags, {more:Array(UInt8)})) WHERE id = {id:UInt64}

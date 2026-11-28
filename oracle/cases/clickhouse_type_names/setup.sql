CREATE TABLE marks (id UInt64, at Tuple(`x.y` UInt8, `select` String), label Enum8('a\nb' = 1, 'c\td' = 2)) ENGINE = MergeTree ORDER BY id;
INSERT INTO marks VALUES (1, (1, 'one'), 'a\nb'), (2, (2, 'two'), 'c\td')

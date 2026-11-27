-- What ClickHouse reads as a statement's text: a heredoc's tag is ASCII letters, digits and `_`, a
-- block comment nests, `#` starts a comment before a space, and a vertical tab and Unicode's spaces
-- are whitespace.
CREATE TABLE t (id UInt64) ENGINE = MergeTree ORDER BY id;

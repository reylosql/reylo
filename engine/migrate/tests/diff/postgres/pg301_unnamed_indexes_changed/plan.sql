-- file 1 of 3
CREATE INDEX concurrently t_a_idx_new ON t USING btree (a) WHERE a > 0;
-- file 2 of 3
drop index t_a_idx;
alter index t_a_idx_new rename to t_a_idx;
-- file 3 of 3
CREATE INDEX concurrently t_b_idx ON t USING btree (b);

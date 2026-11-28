create table t (id int primary key, seq int comment 'not null until rollout', key (seq));

create table t (id int primary key, seq int not null auto_increment comment 'not null until rollout', key (seq));

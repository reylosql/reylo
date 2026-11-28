alter table t modify column seq int comment 'not null until rollout' not null;
alter table t modify column seq int not null auto_increment comment 'not null until rollout';

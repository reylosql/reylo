alter table t modify column seq int not null comment 'not null until rollout';
alter table t modify column seq int not null auto_increment comment 'not null until rollout';

alter table t modify column seq int generated always as ((id + 1)) stored not null;
alter table t modify column seq int not null auto_increment;

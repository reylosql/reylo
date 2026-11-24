alter table parent drop constraint ck;
alter table child no inherit parent;
alter table child drop constraint if exists ck;
drop sequence check_seq;

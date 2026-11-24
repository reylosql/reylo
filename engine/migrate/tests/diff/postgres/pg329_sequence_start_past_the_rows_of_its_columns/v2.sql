create sequence order_no start with 1000;
create table orders (id bigint primary key default nextval('order_no'), n text);
create sequence things_id_seq start with 1000;
create table things (id bigint primary key default nextval('things_id_seq'), n text);
alter sequence things_id_seq owned by things.id;

create sequence order_no;
create table orders (id bigint primary key default nextval('order_no'), n text);
create table things (id bigserial primary key, n text);

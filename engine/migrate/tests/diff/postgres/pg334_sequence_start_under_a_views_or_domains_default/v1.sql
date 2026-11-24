create sequence vseq;
create table base (id bigint primary key, n text);
create view base_v as select id, n from base where n <> 'hidden';
alter view base_v alter column id set default nextval('vseq');
create sequence dseq;
create domain order_id as bigint default nextval('dseq');
create table orders (id order_id primary key, n text);

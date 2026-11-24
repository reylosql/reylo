create sequence tag_no start with 50;
create table tags (code text primary key default nextval('tag_no')::text, n text);
create sequence far_no start with 10;
create table far (id bigint primary key default 1000000 + nextval('far_no'), n text);

create sequence tag_no;
create table tags (code text primary key default nextval('tag_no')::text, n text);
create sequence far_no;
create table far (id bigint primary key default 1000000 + nextval('far_no'), n text);

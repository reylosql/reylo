-- loses data: child.secret
create type ty as (
  id integer
);
alter table child no inherit parent;
alter table child drop column if exists secret;
alter table child of ty;

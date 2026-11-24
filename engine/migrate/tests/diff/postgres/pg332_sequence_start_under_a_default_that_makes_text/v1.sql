create sequence inv_no;
create table invoices (code text primary key default 'INV-' || lpad(nextval('inv_no')::text, 6, '0'), n text);

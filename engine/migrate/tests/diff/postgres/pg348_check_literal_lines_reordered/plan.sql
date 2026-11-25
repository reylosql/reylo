-- file 1 of 2
alter table notes drop constraint notes_body_check;
alter table notes add constraint notes_body_check check (body <> 'head
blue
red
tail') not valid;
-- file 2 of 2
alter table notes validate constraint notes_body_check;

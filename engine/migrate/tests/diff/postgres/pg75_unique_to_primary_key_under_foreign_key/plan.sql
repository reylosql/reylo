-- file 1 of 3
create unique index concurrently cases_pkey on cases (id);
-- file 2 of 3
alter table case_notes drop constraint case_notes_case_id_fkey;
alter table cases drop constraint cases_id_key;
alter table cases add constraint cases_pkey primary key using index cases_pkey;
alter table case_notes add constraint case_notes_case_id_fkey foreign key (case_id) references cases(id) on delete cascade not valid;
-- file 3 of 3
alter table case_notes validate constraint case_notes_case_id_fkey;

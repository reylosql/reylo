insert into notes_fts (notes_fts, rank) values ('integrity-check', 1);
select json(case when not exists (select 1 from notes_fts where notes_fts match 'untitled') then 'bad' else '1' end);

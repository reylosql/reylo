insert into "t0,x" (id) values (1);
insert into "t0,x" (id) values (2);
insert into "t0,x" (id) values (3);
insert into h0 (id, "e0,x", "e1.x") values (1, array['a']::"Mood"[], '{"1"}');
insert into h0 (id, "e0,x", "e1.x") values (2, array['a']::"Mood"[], '{"2"}');
insert into h0 (id, "e0,x", "e1.x") values (3, array['a']::"Mood"[], '{"3"}');

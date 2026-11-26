insert into "t0,x" (id, "c2,x") values (1, 1.5);
insert into "t0,x" (id, "c2,x") values (2, 2.5);
insert into "t0,x" (id, "c2,x") values (3, 3.5);
insert into h0 (id, "c2,x", "e0,x", "e1.x") values (1, 1.5, array['a']::"Mood"[], '{"1"}');
insert into h0 (id, "c2,x", "e0,x", "e1.x") values (2, 2.5, array['a']::"Mood"[], '{"2"}');
insert into h0 (id, "c2,x", "e0,x", "e1.x") values (3, 3.5, array['a']::"Mood"[], '{"3"}');

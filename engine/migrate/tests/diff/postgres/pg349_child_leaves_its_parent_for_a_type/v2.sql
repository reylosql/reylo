create type "Mood" as enum ('a', 'b|c', 'q"r', 'd.e');
create type "ty0""x" as (id integer, "e0,x" "Mood"[], "e1.x" text[]);
create table h0 of "ty0""x";

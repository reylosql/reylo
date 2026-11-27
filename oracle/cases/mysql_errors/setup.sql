-- What `.try()`'s error type is narrowed to on MySQL: the keys, foreign keys, checks and
-- columns a statement writes through, and nothing where it may run a trigger or a stored function.
create table users (id int primary key, email varchar(20) not null unique, age int, constraint adult check (age >= 18));
create table posts (id int primary key, user_id int not null, title varchar(20) not null, constraint posts_user foreign key (user_id) references users (id) on delete cascade);
create table comments (id int primary key, post_id int, constraint comments_post foreign key (post_id) references posts (id) on delete restrict);
create table audited (id int primary key);
create trigger audited_audit before insert on audited for each row set new.id = new.id;
create function tax(n int) returns int deterministic return n * 2;
create view taxed as select id, tax(age) as taxed from users;
create view user_emails as select id, email from users;
create view email_echoes as with recursive r (x, k) as (select email, 1 from users union all select concat(x, x), k + 1 from r where k < 3) select x as echo from r;
create view scores as select id, score as points from (select id, cast(age * 1000 as decimal(4,0)) as score from users limit 1000) d;
create view handles as select u.id, j.handle as nick from users u, json_table(json_array(u.email), '$[*]' columns (handle varchar(4) path '$' error on error)) j;
create view nicks as select id, nick from handles;
create table members (id int primary key);
create table bans (member_id int primary key);
create table ledger (id int primary key);
create trigger members_ledger before delete on members for each row insert into ledger (id) values (old.id);
insert into users (id, email, age) values (1, 'ada@example.com', 30);
insert into posts (id, user_id, title) values (1, 1, 'hello');
insert into comments (id, post_id) values (1, 1);

-- Statements 20 levels deep, each level reading the one below it as a FROM item, one file per
-- shape. Every level asks about the one below it from several places (its keys and then its rows,
-- again for each item a closure starts from, the FROM again for a DISTINCT or a GROUP BY), and the
-- analysis proves each level once. The statements are generated; each file names its shape.
create table todos (id int primary key, owner int not null);

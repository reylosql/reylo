-- Statements 40 levels deep, each level reading the one below it as a FROM item, one file per
-- shape. Every level asks about the one below it from several places (its rows and then its keys,
-- the FROM again for a DISTINCT or a GROUP BY, a WITH query by its name and again as an item), and
-- the analysis proves each level once. The statements are generated; each file names its shape.
create table todos (id int primary key, owner int not null);

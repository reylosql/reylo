-- h.old is renamed h, keeping its rows. If they are two columns, drop the one and add the other instead.
alter table h rename column old to h;

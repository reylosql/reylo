-- An INSERT that leaves out a column each row must write, and what fills the column or skips the row instead: a default, the rowid's alias, a generated column, OR IGNORE, a NOT NULL declared ON CONFLICT IGNORE, a BEFORE trigger, a view's INSTEAD OF trigger. OR REPLACE, a NOT NULL declared ON CONFLICT REPLACE and an upsert still refuse the row, and so do DEFAULT NULL, a UNIQUE declared ON CONFLICT IGNORE and a written rowid. A virtual table checks no NOT NULL it declares.
create table req (id integer primary key, name text not null, email text not null, note text);
create table dflt (id integer primary key, name text not null default 'x', nulled text not null default null, made text not null generated always as (id || 'x'));
create table keyed (k text not null, v text, primary key (k)) without rowid;
create table plain_key (k int primary key, v text);
create table ign (id integer primary key, name text not null on conflict ignore);
create table rep (id integer primary key, name text not null on conflict replace);
create table skipped (id integer primary key, name text not null);
create trigger skip before insert on skipped begin select raise(ignore); end;
create table logged (id integer primary key, name text not null);
create trigger log after insert on logged begin select 1; end;
create view named as select id, name from req;
create trigger named_insert instead of insert on named begin insert into req (id, name, email) values (new.id, new.name, 'x'); end;
create virtual table shapes using geopoly(name text not null);
create table uniq (id integer primary key, name text not null, tag text unique on conflict ignore);
create table plain (name text not null, tag text);

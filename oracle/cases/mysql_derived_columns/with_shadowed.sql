-- An inner WITH query hides an outer one of its name, and the derived table's WHERE still proves
-- its column never NULL.
with x as (select id from users) select d.e from (with x as (select email as e from users where email is not null) select e from x) d

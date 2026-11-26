-- loses data: p0."A1"
alter type p0 drop attribute "A1";
alter table h0 alter column "N0" set default row('7'::text, (7)::bigint)::p0;

select array[(select concat(count(*) over (partition by generate_series(1, 0))))] as a

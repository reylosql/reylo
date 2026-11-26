insert into q select g, 'q' || g from generate_series(1, 20) g;

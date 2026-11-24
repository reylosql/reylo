SELECT count() AS n, sum(count()) OVER w AS total FROM users WINDOW w AS ()

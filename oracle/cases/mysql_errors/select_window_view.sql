select row_number() over (partition by (select count(*) from email_echoes)) as rn from users

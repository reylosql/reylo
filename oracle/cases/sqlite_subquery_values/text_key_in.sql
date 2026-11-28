select tag from tags where tag in (select title from posts where id = :id)

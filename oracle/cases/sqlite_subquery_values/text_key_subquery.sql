select tag from tags where tag = (select title from posts where id = :id)

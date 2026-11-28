insert into users (id, account_id, name) select :id, a.id, :n from accounts a where a.id = :acct returning id

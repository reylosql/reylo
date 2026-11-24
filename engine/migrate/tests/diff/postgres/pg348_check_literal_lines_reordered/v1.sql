create table notes (body text, constraint notes_body_check check (body <> E'head\nred\nblue\ntail'));

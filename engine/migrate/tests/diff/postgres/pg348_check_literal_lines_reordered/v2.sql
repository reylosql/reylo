create table notes (body text, constraint notes_body_check check (body <> E'head\nblue\nred\ntail'));

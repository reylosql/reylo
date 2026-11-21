//! PL/pgSQL's scanner (`pl_scanner.c`) over a port of Postgres's own (`scan.l`): words are
//! keywords only by PL/pgSQL's lists, and a name, or two or three joined by dots, is looked up in
//! the namespace the declarations built.

use super::compile::{Code, Compiler, Lookup, Tok, Val, EOF, R};
use super::tables::{t, NTOKENS, RESERVED, UNRESERVED};
use crate::postgres::generated::{tok, CHAR_TOKENS, NAMES as CORE_NAMES};
use crate::postgres::scan::{Token, Value};
use std::sync::OnceLock;

/// The most tokens pushed back at once (`MAX_PUSHBACKS`).
const MAX_PUSHBACKS: usize = 4;

/// Each core scanner token kind's PL/pgSQL code, where the two grammars share it.
fn core_codes() -> &'static Vec<Option<Code>> {
    static CODES: OnceLock<Vec<Option<Code>>> = OnceLock::new();
    CODES.get_or_init(|| {
        let mut codes = vec![None; CORE_NAMES.len()];
        let names = &super::tables::NAMES[..NTOKENS as usize];
        for (kind, name) in CORE_NAMES.iter().enumerate() {
            if let Some(sym) = names.iter().position(|n| n == name) {
                if sym > 2 {
                    codes[kind] = Some(sym as Code);
                }
            }
        }
        for (c, &kind) in CHAR_TOKENS.iter().enumerate() {
            if kind != 0 {
                let quoted = format!("'{}'", c as u8 as char);
                codes[kind as usize] = Some(names.iter().position(|n| *n == quoted).map_or(super::compile::ch(c as u8), |s| s as Code));
            }
        }
        codes
    })
}

fn reserved(word: &str) -> Option<(Code, &'static str)> {
    RESERVED.iter().find(|k| k.0 == word).map(|k| (k.1, k.0))
}

fn unreserved(word: &str) -> Option<(Code, &'static str)> {
    UNRESERVED.iter().find(|k| k.0 == word).map(|k| (k.1, k.0))
}

/// Whether a code is one of PL/pgSQL's unreserved keywords (`plpgsql_token_is_unreserved_keyword`).
pub(super) fn is_unreserved_keyword(code: Code) -> bool {
    UNRESERVED.iter().any(|k| k.1 == code)
}

impl Compiler<'_> {
    /// The core scanner's next token as PL/pgSQL reads it (`core_yylex` with PL/pgSQL's reserved
    /// words as its keywords, then `internal_yylex`'s operators).
    fn core_token(&mut self) -> R<Tok> {
        loop {
            let Some(token) = self.raw.get(self.raw_at) else {
                if let Some(message) = &self.stop {
                    return Err(message.clone());
                }
                return Ok(Tok { code: EOF, val: Val::None, loc: self.src.len() as i32, leng: 0 });
            };
            self.raw_at += 1;
            if token.kind == tok::SQL_COMMENT || token.kind == tok::C_COMMENT {
                continue;
            }
            return Ok(self.convert(token));
        }
    }

    fn convert(&self, token: &Token) -> Tok {
        let text = &self.src.as_bytes()[token.start as usize..token.end as usize];
        let (loc, leng) = (token.start as i32, (token.end - token.start) as i32);
        let quoted = text.first() == Some(&b'"');
        let word = match &token.value {
            // `N'…'` hands over NCHAR where that is a keyword, and the name `n` where it is not.
            Value::Keyword("nchar") if text.len() == 1 => Some("n".to_string()),
            Value::Keyword(k) => Some(k.to_string()),
            Value::Bytes(b) if token.kind == tok::IDENT => Some(String::from_utf8_lossy(b).into_owned()),
            _ => None,
        };
        if let Some(word) = word {
            if !quoted {
                if let Some((code, k)) = reserved(&word) {
                    return Tok { code, val: Val::Keyword(k), loc, leng };
                }
            }
            return Tok { code: t::IDENT, val: Val::Str(word), loc, leng };
        }
        let code = core_codes().get(token.kind as usize).copied().flatten().unwrap_or(2);
        let val = match &token.value {
            Value::Bytes(b) => Val::Str(String::from_utf8_lossy(b).into_owned()),
            Value::Int(i) => Val::Int(*i),
            Value::Keyword(k) => Val::Keyword(k),
            Value::None => Val::None,
        };
        match code {
            super::compile::OP => match text {
                b"<<" => Tok { code: t::LESS_LESS, val: Val::None, loc, leng },
                b">>" => Tok { code: t::GREATER_GREATER, val: Val::None, loc, leng },
                b"#" => Tok { code: t::HASH, val: Val::None, loc, leng },
                _ => Tok { code, val, loc, leng },
            },
            // The core gives a parameter's number; PL/pgSQL takes it as a name.
            t::PARAM => Tok { code, val: Val::Str(String::from_utf8_lossy(text).into_owned()), loc, leng },
            _ => Tok { code, val, loc, leng },
        }
    }

    /// `internal_yylex`: a pushed-back token, else the core scanner's next.
    fn internal_lex(&mut self) -> R<Tok> {
        match self.pushback.pop() {
            Some(t) => Ok(t),
            None => self.core_token(),
        }
    }

    fn push_back_tok(&mut self, tok: Tok) -> R<()> {
        if self.pushback.len() >= MAX_PUSHBACKS {
            return Err("too many tokens pushed back".to_string());
        }
        self.pushback.push(tok);
        Ok(())
    }

    /// `plpgsql_push_back_token`: gives back the current token, as `code`.
    pub fn push_back(&mut self, code: Code) -> R<()> {
        let mut tok = self.cur.clone();
        tok.code = code;
        tok.leng = self.yyleng;
        self.push_back_tok(tok)
    }

    /// `plpgsql_peek`.
    pub fn peek(&mut self) -> R<Code> {
        let tok = self.internal_lex()?;
        let code = tok.code;
        self.push_back_tok(tok)?;
        Ok(code)
    }

    /// `plpgsql_peek2`: the next two codes, and where the first starts.
    pub fn peek2(&mut self) -> R<(Code, Code, i32)> {
        let a = self.internal_lex()?;
        let b = self.internal_lex()?;
        let out = (a.code, b.code, a.loc);
        self.push_back_tok(b)?;
        self.push_back_tok(a)?;
        Ok(out)
    }

    /// `plpgsql_yylex`: the next token, a name resolved to a variable or a keyword.
    pub fn yylex(&mut self) -> R<Code> {
        let mut t1 = self.internal_lex()?;
        if t1.code == t::IDENT || t1.code == t::PARAM {
            let w1 = t1.str();
            let quoted = self.src.as_bytes().get(t1.loc as usize) == Some(&b'"');
            let t2 = self.internal_lex()?;
            if t2.code == super::compile::DOT {
                let t3 = self.internal_lex()?;
                if t3.code == t::IDENT {
                    let t4 = self.internal_lex()?;
                    if t4.code == super::compile::DOT {
                        let t5 = self.internal_lex()?;
                        if t5.code == t::IDENT {
                            let (w2, w3) = (t3.str(), t5.str());
                            t1.val = match self.parse_tripword(&w1, &w2, &w3) {
                                Ok((dno, idents)) => Val::Datum { dno, ident: None, quoted: false, idents },
                                Err(idents) => Val::CWord(idents),
                            };
                            t1.code = if matches!(t1.val, Val::Datum { .. }) { t::T_DATUM } else { t::T_CWORD };
                            t1.leng = t5.loc - t1.loc + t5.leng;
                        } else {
                            self.push_back_tok(t5)?;
                            self.push_back_tok(t4)?;
                            self.dblword(&mut t1, &w1, &t3);
                        }
                    } else {
                        self.push_back_tok(t4)?;
                        self.dblword(&mut t1, &w1, &t3);
                    }
                } else {
                    self.push_back_tok(t3)?;
                    self.push_back_tok(t2)?;
                    self.word(&mut t1, w1, quoted, true);
                }
            } else {
                let code2 = t2.code;
                self.push_back_tok(t2)?;
                let at_start = matches!(self.yytoken, t::SEMI | t::K_BEGIN | t::K_THEN | t::K_ELSE | t::K_LOOP);
                let lookup = !at_start || code2 == t::EQUALS || code2 == t::COLON_EQUALS || code2 == super::compile::LBRACKET;
                self.word(&mut t1, w1, quoted, lookup);
            }
        }
        self.yyleng = t1.leng;
        self.yytoken = t1.code;
        let code = t1.code;
        self.cur = t1;
        Ok(code)
    }

    fn dblword(&mut self, t1: &mut Tok, w1: &str, t3: &Tok) {
        let w2 = t3.str();
        t1.val = match self.parse_dblword(w1, &w2) {
            Ok(dno) => Val::Datum { dno, ident: None, quoted: false, idents: vec![w1.to_string(), w2] },
            Err(idents) => Val::CWord(idents),
        };
        t1.code = if matches!(t1.val, Val::Datum { .. }) { t::T_DATUM } else { t::T_CWORD };
        t1.leng = t3.loc - t1.loc + t3.leng;
    }

    /// One name: a variable, else an unreserved keyword, else a word.
    fn word(&mut self, t1: &mut Tok, w1: String, quoted: bool, lookup: bool) {
        if let Some(dno) = self.parse_word(&w1, lookup) {
            t1.code = t::T_DATUM;
            t1.val = Val::Datum { dno, ident: Some(w1), quoted, idents: Vec::new() };
        } else if let Some((code, k)) = (!quoted).then(|| unreserved(&w1)).flatten() {
            t1.code = code;
            t1.val = Val::Keyword(k);
        } else {
            t1.code = t::T_WORD;
            t1.val = Val::Word(w1);
        }
    }

    pub fn set_lookup(&mut self, lookup: Lookup) -> Lookup {
        std::mem::replace(&mut self.lookup, lookup)
    }
}

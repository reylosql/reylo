//! PL/pgSQL: its functions read into the tree PL/pgSQL's compiler builds ([`parse_plpgsql`]), and
//! what a BEFORE row trigger does with its row, read off that tree ([`keeps_the_row`]).

#[path = "plpgsql/generated.rs"]
#[allow(dead_code)]
mod tables;
mod comp;
mod compile;
mod gram;
pub mod nodes;
mod scanner;

pub use compile::parse_plpgsql;
use nodes::{Block, Stmt};

/// Whether the body of a BEFORE row trigger keeps every row: each RETURN returns NEW, where
/// `may_return_new`, or OLD, where `may_return_old`, as the record itself; nothing assigns either
/// as a whole, or reads a row into it; and nothing CALLs a procedure, which can write either back
/// through an INOUT parameter, or returns rows of a set. A body PL/pgSQL's compiler refuses
/// answers no, which claims nothing.
pub fn keeps_the_row(body: &str, may_return_new: bool, may_return_old: bool) -> bool {
    // Under `standard_conforming_strings = off`, which a function can set for itself, a backslash
    // before a quote keeps the string open where this reading sees it close.
    if body.contains("\\'") {
        return false;
    }
    let Ok(function) = super::on_deep_stack(|| compile::trigger(body)) else { return false };
    let Some(action) = &function.action else { return false };
    let (new, old) = (function.new_varno, function.old_varno);
    let rules = Rules { whole: [new, old], may_return: [may_return_new.then_some(new), may_return_old.then_some(old)] };
    rules.block(action)
}

struct Rules {
    /// NEW's and OLD's datums.
    whole: [i32; 2],
    /// The datums a RETURN may return.
    may_return: [Option<i32>; 2],
}

impl Rules {
    fn block(&self, b: &Block) -> bool {
        self.all(&b.body) && b.exceptions.as_ref().is_none_or(|x| x.exc_list.iter().all(|e| self.all(&e.action)))
    }

    fn all(&self, stmts: &[Stmt]) -> bool {
        stmts.iter().all(|s| self.stmt(s))
    }

    fn untouched(&self, target: Option<i32>) -> bool {
        target.is_none_or(|t| !self.whole.contains(&t))
    }

    fn stmt(&self, s: &Stmt) -> bool {
        match s {
            Stmt::Block(b) => self.block(b),
            Stmt::Assign { varno, .. } => self.untouched(Some(*varno)),
            Stmt::If { then_body, elsif_list, else_body, .. } => self.all(then_body) && elsif_list.iter().all(|e| self.all(&e.stmts)) && self.all(else_body),
            Stmt::Case { case_when_list, else_stmts, .. } => case_when_list.iter().all(|w| self.all(&w.stmts)) && self.all(else_stmts),
            Stmt::Loop { body, .. } | Stmt::While { body, .. } | Stmt::ForI { body, .. } => self.all(body),
            Stmt::ForS { forq, .. } | Stmt::ForC { forq, .. } | Stmt::DynForS { forq, .. } => self.untouched(Some(forq.var)) && self.all(&forq.body),
            Stmt::ForeachA { varno, body, .. } => self.untouched(Some(*varno)) && self.all(body),
            Stmt::Return { expr, retvarno, .. } => expr.is_none() && self.may_return.contains(&Some(*retvarno)),
            Stmt::ReturnNext { .. } | Stmt::ReturnQuery { .. } | Stmt::Call { .. } => false,
            Stmt::ExecSql { target, .. } | Stmt::DynExecute { target, .. } => self.untouched(*target),
            Stmt::Fetch(f) => self.untouched(f.target),
            Stmt::Exit { .. } | Stmt::Raise { .. } | Stmt::Assert { .. } | Stmt::GetDiag { .. } | Stmt::Open { .. } | Stmt::Close { .. } | Stmt::Perform { .. } | Stmt::Commit { .. } | Stmt::Rollback { .. } => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::keeps_the_row;

    #[test]
    fn answers_no_where_a_word_of_the_block_is_a_name() {
        for body in [
            "declare \"begin\" int; new t; begin return new; end",
            "declare x int := (select begin from (select 1 as begin) s); new t; begin return new; end",
            "declare \"begin\" alias for new; begin \"begin\" := null; return new; end",
            "declare \"declare\" int; begin \"declare\" := 1; return null; begin null; end; end",
            "begin perform case when true then declare else 1 end from (select 1 as declare) s; return null; begin null; end; end",
            "declare \"for\" alias for new; begin \"for\" := null; return new; end",
            "declare \"foreach\" alias for new; begin \"foreach\" := null; return new; end",
            "declare \"strict\" alias for new; begin select 1 into \"strict\"; return new; end",
            "begin perform 'a\\'' ; return null; --';\nreturn new; end",
            "declare scratch widgets%rowtype; begin scratch.id := new.id; return scratch; end",
        ] {
            assert!(!keeps_the_row(body, true, true), "{body}");
        }
    }

    #[test]
    fn keeps_what_it_can_read() {
        for body in ["begin new.a := regexp_replace(new.a, '\\s+', ' '); return new; end", "declare n alias for new; begin n.a := 1; return n; end", "<<outer>> begin return new; end outer;", "declare scratch audit.widgets%rowtype; begin scratch.id := new.id; return new; end", "declare r new%type; begin r := new; r.a := 1; return new; end", "<<outer>> declare scratch widgets%rowtype; begin declare copy outer.scratch%type; begin copy.id := new.id; end; return new; end"] {
            assert!(keeps_the_row(body, true, true), "{body}");
        }
    }
}

//! The namespace and the datums a function's declarations build (`pl_comp.c`, `pl_funcs.c`).

use super::compile::{format_type, Compiler, Lookup, NsItem, R, NS_LABEL, NS_REC, NS_VAR, LABEL_LOOP};
use super::nodes::{Condition, Datum, Rec, RecField, TType, Type, Var};
use super::tables::CONDITIONS;

impl Compiler<'_> {
    pub fn ns_additem(&mut self, kind: u8, itemno: i32, name: &str) {
        self.ns.push(NsItem { kind, itemno, name: name.to_string(), prev: self.ns_top });
        self.ns_top = Some(self.ns.len() - 1);
    }

    pub fn ns_push(&mut self, label: Option<&str>, kind: i32) {
        self.ns_additem(NS_LABEL, kind, label.unwrap_or(""));
    }

    pub fn ns_pop(&mut self) {
        while let Some(top) = self.ns_top {
            let item = &self.ns[top];
            self.ns_top = item.prev;
            if item.kind == NS_LABEL {
                break;
            }
        }
    }

    /// `plpgsql_ns_lookup`: the item a name, or a label and a name, or a record's field, refers to,
    /// and how many of the names it took.
    pub fn ns_lookup(&self, start: Option<usize>, localmode: bool, n1: &str, n2: Option<&str>, n3: Option<&str>) -> Option<(usize, i32)> {
        let mut cur = start;
        while let Some(c) = cur {
            let mut at = c;
            while self.ns[at].kind != NS_LABEL {
                let item = &self.ns[at];
                if item.name == n1 && (n2.is_none() || item.kind != NS_VAR) {
                    return Some((at, 1));
                }
                at = item.prev?;
            }
            let label = at;
            if let Some(n2) = n2 {
                if self.ns[label].name == n1 {
                    let mut at = c;
                    while self.ns[at].kind != NS_LABEL {
                        let item = &self.ns[at];
                        if item.name == n2 && (n3.is_none() || item.kind != NS_VAR) {
                            return Some((at, 2));
                        }
                        at = item.prev?;
                    }
                }
            }
            if localmode {
                break;
            }
            cur = self.ns[label].prev;
        }
        None
    }

    pub fn ns_lookup_label(&self, name: &str) -> Option<usize> {
        let mut cur = self.ns_top;
        while let Some(c) = cur {
            if self.ns[c].kind == NS_LABEL && self.ns[c].name == name {
                return Some(c);
            }
            cur = self.ns[c].prev;
        }
        None
    }

    pub fn ns_find_nearest_loop(&self) -> Option<usize> {
        let mut cur = self.ns_top;
        while let Some(c) = cur {
            if self.ns[c].kind == NS_LABEL && self.ns[c].itemno == LABEL_LOOP {
                return Some(c);
            }
            cur = self.ns[c].prev;
        }
        None
    }

    pub fn add_parameter_name(&mut self, kind: u8, itemno: i32, name: &str) -> R<()> {
        if self.ns_lookup(self.ns_top, true, name, None, None).is_some() {
            return Err(format!("parameter name \"{name}\" used more than once"));
        }
        self.ns_additem(kind, itemno, name);
        Ok(())
    }

    pub fn adddatum(&mut self, datum: Datum) -> i32 {
        self.datums.push(datum);
        let dno = self.datums.len() as i32 - 1;
        if let Datum::Rec(r) = &mut self.datums[dno as usize] {
            r.dno = dno;
        }
        dno
    }

    /// `plpgsql_build_variable`: a scalar variable, or a record for a row type.
    pub fn build_variable(&mut self, refname: &str, lineno: i32, dtype: Type, add2ns: bool) -> R<i32> {
        match dtype.ttype {
            TType::Scalar => {
                let dno = self.adddatum(Datum::Var(Var { refname: refname.to_string(), lineno, datatype: Some(dtype), isconst: false, notnull: false, default_val: None, cursor_explicit_expr: None, cursor_explicit_argrow: 0, cursor_options: 0, promise: 0 }));
                if add2ns {
                    self.ns_additem(NS_VAR, dno, refname);
                }
                Ok(dno)
            }
            TType::Rec => Ok(self.build_record(refname, lineno, add2ns)),
            TType::Pseudo => Err(format!("variable \"{refname}\" has pseudo-type {}", format_type(dtype.typoid)?)),
        }
    }

    pub fn build_record(&mut self, refname: &str, lineno: i32, add2ns: bool) -> i32 {
        let dno = self.adddatum(Datum::Rec(Rec { refname: refname.to_string(), dno: 0, lineno, isconst: false, notnull: false, default_val: None, firstfield: -1 }));
        if add2ns {
            self.ns_additem(NS_REC, dno, refname);
        }
        dno
    }

    /// `plpgsql_build_recfield`: a record's field, made once per name.
    pub fn build_recfield(&mut self, rec: i32, fieldname: &str) -> i32 {
        let Datum::Rec(r) = &self.datums[rec as usize] else { return rec };
        let mut i = r.firstfield;
        while i >= 0 {
            let Datum::RecField(f) = &self.datums[i as usize] else { break };
            if f.fieldname == fieldname {
                return i;
            }
            i = f.nextfield;
        }
        let first = r.firstfield;
        let dno = self.adddatum(Datum::RecField(RecField { fieldname: fieldname.to_string(), recparentno: rec, nextfield: first }));
        if let Datum::Rec(r) = &mut self.datums[rec as usize] {
            r.firstfield = dno;
        }
        dno
    }

    pub fn set_promise(&mut self, dno: i32, promise: i32) {
        if let Datum::Var(v) = &mut self.datums[dno as usize] {
            v.promise = promise;
        }
    }

    pub fn refname(&self, dno: i32) -> String {
        match &self.datums[dno as usize] {
            Datum::Var(v) => v.refname.clone(),
            Datum::Row(r) => r.refname.clone(),
            Datum::Rec(r) => r.refname.clone(),
            Datum::RecField(f) => f.fieldname.clone(),
        }
    }

    /// `plpgsql_add_initdatums`: the variables and records declared since the last call.
    pub fn add_initdatums(&mut self) -> Vec<i32> {
        let out = (self.datums_last..self.datums.len()).filter(|&i| matches!(&self.datums[i], Datum::Var(v) if v.promise == 0) || matches!(self.datums[i], Datum::Rec(_))).map(|i| i as i32).collect();
        self.datums_last = self.datums.len();
        out
    }

    /// `plpgsql_parse_word`: the variable a name refers to, where names are looked up.
    pub fn parse_word(&self, word: &str, lookup: bool) -> Option<i32> {
        if !lookup || self.lookup != Lookup::Normal {
            return None;
        }
        let (item, _) = self.ns_lookup(self.ns_top, false, word, None, None)?;
        let item = &self.ns[item];
        (item.kind == NS_VAR || item.kind == NS_REC).then_some(item.itemno)
    }

    /// `plpgsql_parse_dblword`: `a.b` as a block's variable or a record's field; else its names.
    pub fn parse_dblword(&mut self, w1: &str, w2: &str) -> Result<i32, Vec<String>> {
        let idents = vec![w1.to_string(), w2.to_string()];
        if self.lookup == Lookup::Declare {
            return Err(idents);
        }
        match self.ns_lookup(self.ns_top, false, w1, Some(w2), None) {
            Some((item, used)) => {
                let (kind, itemno) = (self.ns[item].kind, self.ns[item].itemno);
                match kind {
                    NS_VAR => Ok(itemno),
                    NS_REC if used == 1 => Ok(self.build_recfield(itemno, w2)),
                    NS_REC => Ok(itemno),
                    _ => Err(idents),
                }
            }
            None => Err(idents),
        }
    }

    /// `plpgsql_parse_tripword`: `a.b.c` as a record's field, with the names that named it.
    pub fn parse_tripword(&mut self, w1: &str, w2: &str, w3: &str) -> Result<(i32, Vec<String>), Vec<String>> {
        let three = vec![w1.to_string(), w2.to_string(), w3.to_string()];
        if self.lookup == Lookup::Declare {
            return Err(three);
        }
        match self.ns_lookup(self.ns_top, false, w1, Some(w2), Some(w3)) {
            Some((item, used)) if self.ns[item].kind == NS_REC => {
                let rec = self.ns[item].itemno;
                if used == 1 {
                    Ok((self.build_recfield(rec, w2), vec![w1.to_string(), w2.to_string()]))
                } else {
                    Ok((self.build_recfield(rec, w3), three))
                }
            }
            _ => Err(three),
        }
    }
}

/// `plpgsql_parse_err_condition`: one condition per SQLSTATE the name stands for.
pub(super) fn parse_err_condition(name: &str) -> R<Vec<Condition>> {
    if name == "others" {
        return Ok(vec![Condition { condname: name.to_string() }]);
    }
    let n = CONDITIONS.iter().filter(|c| c.0 == name).count();
    if n == 0 {
        return Err(format!("unrecognized exception condition \"{name}\""));
    }
    Ok((0..n).map(|_| Condition { condname: name.to_string() }).collect())
}

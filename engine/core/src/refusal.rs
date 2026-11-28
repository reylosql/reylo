//! Whether a column refuses NULL by a CHECK or by a NOT NULL generated column: what the CHECK or
//! the generated column's expression is wherever the column is NULL and every other column of the
//! row is unknown. A CHECK that is false there refuses NULL in the column (a CHECK that is NULL
//! passes), and a NOT NULL generated column that is NULL there refuses it too. Each database
//! lowers the expression, as its server prints it, to a [`Probe`], settling what only it knows:
//! which calls are its built-ins and NULL wherever an operand is, and which comparisons are of
//! numbers. [`with_null`] then evaluates it as SQL's three-valued logic does.

use std::collections::{HashMap, HashSet};

/// An expression as far as [`with_null`] reads it.
#[derive(Clone, Debug, PartialEq)]
pub enum Probe {
    /// A NULL literal.
    Null,
    /// An integer, or a boolean as 0 and 1, which as a truth value is false for 0 and true for any
    /// other.
    Int(i64),
    /// A value that is never NULL and whose value the probe does not read, as a string literal.
    Value,
    /// A column of the row, named as the database's lowering compares names.
    Column(String),
    /// `x IS NULL`, or `x IS NOT NULL` where `not`: never NULL.
    IsNull { arg: Box<Probe>, not: bool },
    /// `x IS TRUE`, or `x IS NOT TRUE` where `not`: never NULL.
    IsTrue { arg: Box<Probe>, not: bool },
    /// `x IS FALSE`, or `x IS NOT FALSE` where `not`: never NULL.
    IsFalse { arg: Box<Probe>, not: bool },
    /// `a <=> b`, `a IS NOT DISTINCT FROM b` or SQLite's `a IS b`, and their negations where
    /// `not`: never NULL. Two integers are equal as integers only where `exact`.
    Same { left: Box<Probe>, right: Box<Probe>, not: bool, exact: bool },
    /// A comparison, NULL where either side is. Two integers compare as integers only where
    /// `exact`, as the database says they do.
    Compare { op: Cmp, left: Box<Probe>, right: Box<Probe>, exact: bool },
    And(Box<Probe>, Box<Probe>),
    Or(Box<Probe>, Box<Probe>),
    Not(Box<Probe>),
    /// A CASE with no operand, and IF and IIF as one arm and an ELSE: the first arm whose test is
    /// true, else the ELSE, else NULL.
    Case { arms: Vec<(Probe, Probe)>, otherwise: Option<Box<Probe>> },
    /// COALESCE and IFNULL: the first argument that is not NULL.
    Coalesce(Vec<Probe>),
    /// NULLIF: NULL where `value` equals `other`, which two integers do as integers only where
    /// `exact`.
    NullIf { value: Box<Probe>, other: Box<Probe>, exact: bool },
    /// A call or an operator NULL wherever any of these operands is (`a + 1`), whose value the
    /// probe does not read otherwise.
    Strict(Vec<Probe>),
    /// Anything else: any value, NULL among them.
    Unknown,
}

/// A comparison's operator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Cmp {
    Eq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
}

/// What a [`Probe`] is wherever one column is NULL.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Known {
    Null,
    /// This integer, false as a truth value for 0 and true for any other.
    Int(i64),
    /// Some value that is not NULL.
    Value,
    /// Any value, NULL among them.
    Unknown,
}

/// What `probe` is wherever `column` is NULL and every other column unknown.
pub fn with_null(probe: &Probe, column: &str) -> Known {
    use Known::{Int, Null, Unknown, Value};
    let known = |p: &Probe| with_null(p, column);
    let truth = |b: bool| Int(b as i64);
    let flip = |k: Known, not: bool| match k {
        Int(n) => truth((n != 0) != not),
        k => k,
    };
    let given = |k: Known| matches!(k, Int(_) | Value);
    // The value of several branches one of which is taken: theirs where they agree.
    let one = |values: &[Known]| if values.windows(2).all(|w| w[0] == w[1]) { values[0] } else { Unknown };
    match probe {
        Probe::Null => Null,
        Probe::Int(n) => Int(*n),
        Probe::Value => Value,
        Probe::Column(c) => {
            if c == column {
                Null
            } else {
                Unknown
            }
        }
        Probe::IsNull { arg, not } => match known(arg) {
            Null => flip(Int(1), *not),
            Int(_) | Value => flip(Int(0), *not),
            Unknown => Unknown,
        },
        Probe::IsTrue { arg, not } => match known(arg) {
            Null => flip(Int(0), *not),
            Int(n) => flip(truth(n != 0), *not),
            Value | Unknown => Unknown,
        },
        Probe::IsFalse { arg, not } => match known(arg) {
            Null => flip(Int(0), *not),
            Int(n) => flip(truth(n == 0), *not),
            Value | Unknown => Unknown,
        },
        Probe::Same { left, right, not, exact } => match (known(left), known(right)) {
            (Null, Null) => flip(Int(1), *not),
            (Null, k) | (k, Null) if given(k) => flip(Int(0), *not),
            (Int(l), Int(r)) if *exact => flip(truth(l == r), *not),
            _ => Unknown,
        },
        Probe::Compare { op, left, right, exact } => match (known(left), known(right)) {
            (Null, _) | (_, Null) => Null,
            (Int(l), Int(r)) if *exact => truth(match op {
                Cmp::Eq => l == r,
                Cmp::NotEq => l != r,
                Cmp::Lt => l < r,
                Cmp::Gt => l > r,
                Cmp::LtEq => l <= r,
                Cmp::GtEq => l >= r,
            }),
            _ => Unknown,
        },
        // `NULL and 0` is 0, and `NULL or 1` is 1: a side that settles it settles it whatever the
        // other is.
        Probe::And(left, right) => match (known(left), known(right)) {
            (Int(0), _) | (_, Int(0)) => Int(0),
            (Int(_), Int(_)) => Int(1),
            (Null, Int(_)) | (Int(_), Null) | (Null, Null) => Null,
            _ => Unknown,
        },
        Probe::Or(left, right) => match (known(left), known(right)) {
            (Int(l), _) if l != 0 => Int(1),
            (_, Int(r)) if r != 0 => Int(1),
            (Int(_), Int(_)) => Int(0),
            (Null, Int(_)) | (Int(_), Null) | (Null, Null) => Null,
            _ => Unknown,
        },
        Probe::Not(arg) => match known(arg) {
            Int(n) => truth(n == 0),
            Null => Null,
            Value | Unknown => Unknown,
        },
        Probe::Case { arms, otherwise } => {
            let mut results = Vec::new();
            for (test, result) in arms {
                match known(test) {
                    Int(0) | Null => {}
                    Int(_) => {
                        results.push(known(result));
                        return one(&results);
                    }
                    Value | Unknown => results.push(known(result)),
                }
            }
            results.push(otherwise.as_deref().map_or(Null, known));
            one(&results)
        }
        Probe::Coalesce(args) => {
            // The first argument that is not NULL; past one that may be NULL, a later one that is
            // never NULL still makes the whole not NULL.
            let mut maybe = false;
            for arg in args {
                match known(arg) {
                    Null => {}
                    k if given(k) => return if maybe { Value } else { k },
                    _ => maybe = true,
                }
            }
            if maybe { Unknown } else { Null }
        }
        Probe::NullIf { value, other, exact } => match (known(value), known(other)) {
            (Null, _) => Null,
            (value, Null) => value,
            (Int(l), Int(r)) if *exact => {
                if l == r {
                    Null
                } else {
                    Int(l)
                }
            }
            _ => Unknown,
        },
        Probe::Strict(operands) => {
            if operands.iter().any(|o| known(o) == Null) {
                Null
            } else {
                Unknown
            }
        }
        Probe::Unknown => Unknown,
    }
}

/// Whether a CHECK refuses NULL in `column`: it is false wherever the column is NULL.
pub fn check_refuses(check: &Probe, column: &str) -> bool {
    with_null(check, column) == Known::Int(0)
}

/// The columns of a table a NULL in which a NOT NULL generated column refuses: each column its
/// expression is NULL wherever it is NULL, and through a generated column that is, each column
/// that one's expression is NULL for in turn. `columns` names every column of the table, and
/// `generated` each generated column, whether it is NOT NULL, and its expression.
pub fn refused_by_generated(columns: &[String], generated: &[(String, bool, Probe)]) -> HashSet<String> {
    let null_for: HashMap<&str, Vec<&String>> = generated.iter().map(|(name, _, probe)| (name.as_str(), columns.iter().filter(|c| with_null(probe, c) == Known::Null).collect())).collect();
    let mut refused: HashSet<String> = HashSet::new();
    let mut next: Vec<&String> = generated.iter().filter(|(_, not_null, _)| *not_null).flat_map(|(name, _, _)| null_for[name.as_str()].iter().copied()).collect();
    while let Some(column) = next.pop() {
        if refused.insert(column.clone()) {
            next.extend(null_for.get(column.as_str()).into_iter().flatten().copied());
        }
    }
    refused
}

#[cfg(test)]
mod tests {
    use super::*;
    use Probe::*;

    fn col(name: &str) -> Box<Probe> {
        Box::new(Column(name.into()))
    }
    fn int(n: i64) -> Box<Probe> {
        Box::new(Int(n))
    }
    fn coalesce(name: &str, n: i64) -> Box<Probe> {
        Box::new(Coalesce(vec![Column(name.into()), Int(n)]))
    }

    #[test]
    fn a_check_false_wherever_the_column_is_null_refuses_it() {
        let refuses = |check: Probe| check_refuses(&check, "a");
        assert!(refuses(IsNull { arg: col("a"), not: true }));
        assert!(refuses(Compare { op: Cmp::Gt, left: coalesce("a", 0), right: int(0), exact: true }));
        assert!(refuses(Same { left: col("a"), right: int(1), not: false, exact: true }));
        assert!(refuses(IsTrue { arg: col("a"), not: false }));
        assert!(refuses(And(Box::new(Compare { op: Cmp::Gt, left: col("b"), right: int(0), exact: true }), Box::new(IsNull { arg: col("a"), not: true }))));
        // NULL, or true, or settled by another column: the CHECK passes, or may.
        assert!(!refuses(Compare { op: Cmp::Gt, left: col("a"), right: int(0), exact: true }));
        assert!(!refuses(Compare { op: Cmp::Gt, left: coalesce("a", 1), right: int(0), exact: true }));
        assert!(!refuses(IsFalse { arg: col("a"), not: true }));
        assert!(!refuses(Or(Box::new(IsNull { arg: col("a"), not: true }), Box::new(IsNull { arg: col("b"), not: true }))));
        assert!(!refuses(Or(Box::new(IsNull { arg: col("a"), not: false }), Box::new(Compare { op: Cmp::Gt, left: col("a"), right: int(0), exact: true }))));
        // Two integers the database does not compare as integers settle nothing.
        assert!(!refuses(Compare { op: Cmp::Lt, left: coalesce("a", 10), right: coalesce("a", 9), exact: false }));
        assert!(refuses(Not(Box::new(IsNull { arg: col("a"), not: false }))));
    }

    #[test]
    fn coalesce_is_never_null_past_an_unknown_when_a_later_argument_is_not() {
        assert_eq!(with_null(&Coalesce(vec![Column("b".into()), Value]), "a"), Known::Value);
        assert_eq!(with_null(&Coalesce(vec![Column("a".into()), Column("b".into())]), "a"), Known::Unknown);
        assert_eq!(with_null(&Coalesce(vec![Column("a".into()), Null]), "a"), Known::Null);
        assert!(check_refuses(&IsNull { arg: Box::new(Coalesce(vec![Column("a".into()), Column("b".into()), Value])), not: false }, "a"));
    }

    #[test]
    fn a_case_is_the_value_its_arms_agree_on() {
        // `case when a > 0 then 'p' when a <= 0 then 'n' end`: NULL wherever `a` is.
        let arms = vec![(Compare { op: Cmp::Gt, left: col("a"), right: int(0), exact: true }, Value), (Compare { op: Cmp::LtEq, left: col("a"), right: int(0), exact: true }, Value)];
        assert_eq!(with_null(&Case { arms: arms.clone(), otherwise: None }, "a"), Known::Null);
        assert_eq!(with_null(&Case { arms: arms.clone(), otherwise: Some(Box::new(Value)) }, "a"), Known::Value);
        // A test another column settles takes either arm.
        let unsure = vec![(Compare { op: Cmp::Gt, left: col("b"), right: int(0), exact: true }, Null)];
        assert_eq!(with_null(&Case { arms: unsure.clone(), otherwise: None }, "a"), Known::Null);
        assert_eq!(with_null(&Case { arms: unsure, otherwise: Some(int(1)) }, "a"), Known::Unknown);
    }

    #[test]
    fn a_strict_operand_makes_its_call_null() {
        let gp = Strict(vec![Case { arms: vec![(IsNull { arg: col("p"), not: false }, Null)], otherwise: Some(int(1)) }, Int(1)]);
        assert_eq!(with_null(&gp, "p"), Known::Null);
        assert_eq!(with_null(&gp, "q"), Known::Unknown);
        assert_eq!(with_null(&NullIf { value: int(1), other: Box::new(IsNull { arg: col("r"), not: false }), exact: true }, "r"), Known::Null);
    }

    #[test]
    fn a_not_null_generated_column_refuses_null_in_what_it_is_null_for_in_turn() {
        let columns: Vec<String> = ["a", "b", "g1", "g2", "g3"].iter().map(|c| c.to_string()).collect();
        let generated = vec![
            // g1 = a + 1, NULL allowed; g2 = g1 * 2 NOT NULL; g3 = coalesce(b, 0) NOT NULL.
            ("g1".to_string(), false, Strict(vec![Column("a".into()), Int(1)])),
            ("g2".to_string(), true, Strict(vec![Column("g1".into()), Int(2)])),
            ("g3".to_string(), true, Coalesce(vec![Column("b".into()), Int(0)])),
        ];
        let refused = refused_by_generated(&columns, &generated);
        assert_eq!(refused, ["a", "g1"].iter().map(|c| c.to_string()).collect());
    }
}

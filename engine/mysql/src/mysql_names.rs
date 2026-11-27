//! How a statement's name of a table, an alias, a derived table, a WITH query or a schema reaches
//! what it names. MySQL compares these by case where `lower_case_table_names` is 0, as on Linux,
//! and without regard to case where it is 1 or 2, as on Windows and macOS, and the engine does not
//! ask which: `t A join t a` is two FROM items on the first and refused on the others, and a
//! `from c` beside `with C as (…)` reads a table on the first and the WITH query on the others. A
//! name reaches an item here only where it does on both. A column's name is compared without regard
//! to case on every server, and stays folded where it is read.

/// The position, among one scope's names as written, of the one `name` reaches: the only one it
/// matches without regard to case. `None` where none does, and where two do: a server that keeps
/// case takes the one spelled the same, and one that folds names takes whichever has the column,
/// for a table's alias stands beside a derived table's, a WITH query's or a JSON_TABLE's that
/// differs from it in case alone, or not at all.
pub(crate) fn item<'a>(names: impl IntoIterator<Item = &'a str>, name: &str) -> Option<usize> {
    let mut matched = names.into_iter().enumerate().filter(|(_, n)| n.eq_ignore_ascii_case(name)).map(|(i, _)| i);
    match (matched.next(), matched.next()) {
        (Some(i), None) => Some(i),
        _ => None,
    }
}

/// What a bare name in FROM reads, given the names of the WITH queries in scope, outermost first.
#[derive(Debug, PartialEq)]
pub(crate) enum Reads {
    /// The WITH query at this position.
    With(usize),
    /// A table, or a view, of that name.
    Table,
    /// Not known: the innermost WITH query the name matches without regard to case is what a
    /// server that folds names reads, and one that keeps case reads it only where it is spelled the
    /// same, else a WITH query further out or a table.
    Unknown,
}

/// What a bare name in FROM reads among the WITH queries `ctes` names ([`Reads`]).
pub(crate) fn with_query<'a>(ctes: impl IntoIterator<Item = &'a str>, name: &str) -> Reads {
    match ctes.into_iter().enumerate().filter(|(_, c)| c.eq_ignore_ascii_case(name)).last() {
        Some((i, c)) if c == name => Reads::With(i),
        Some(_) => Reads::Unknown,
        None => Reads::Table,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_reaches_what_it_reaches_whether_the_server_keeps_case_or_folds_it() {
        assert_eq!(item(["A", "b"], "a"), Some(0));
        assert_eq!(item(["A", "b"], "A"), Some(0));
        assert_eq!(item(["A", "a"], "A"), None);
        assert_eq!(item(["d", "d"], "d"), None);
        assert_eq!(item(["A"], "c"), None);
        assert_eq!(with_query(["c", "C"], "C"), Reads::With(1));
        assert_eq!(with_query(["C", "c"], "C"), Reads::Unknown);
        assert_eq!(with_query(["C"], "c"), Reads::Unknown);
        assert_eq!(with_query(["C"], "d"), Reads::Table);
    }
}

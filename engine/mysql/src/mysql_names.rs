//! How a statement's name of a table, an alias, a derived table, a WITH query or a schema reaches
//! what it names. MySQL compares these by case where `lower_case_table_names` is 0, as on Linux,
//! and without regard to case where it is 1 or 2, as on Windows and macOS, and the engine does not
//! ask which: `t A join t a` is two FROM items on the first and refused on the others, and a
//! `from c` beside `with C as (…)` reads a table on the first and the WITH query on the others. A
//! name reaches an item here only where it does on both. A column's name is compared without regard
//! to case on every server, and stays folded where it is read.
//!
//! A server that folds names folds letters past ASCII too, comparing with `utf8mb3_tolower_ci`, so
//! `Ä` and `ä` are one name there ([`may_match`]).

/// Whether two names may be one on some server: equal without regard to ASCII case, or, where
/// either holds a letter past ASCII, as many characters long with each pair of characters one
/// that a server's folding may make equal: two past ASCII, or one past ASCII whose own lower or
/// upper case holds the other, as `İ` holds `i` and the Kelvin sign `k`.
pub(crate) fn may_match(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }
    if a.is_ascii() && b.is_ascii() {
        return false;
    }
    let folds = |x: char, y: char| match (x.is_ascii(), y.is_ascii()) {
        (true, true) => x.eq_ignore_ascii_case(&y),
        (false, false) => true,
        (true, false) => y.to_lowercase().chain(y.to_uppercase()).any(|c| c.eq_ignore_ascii_case(&x)),
        (false, true) => x.to_lowercase().chain(x.to_uppercase()).any(|c| c.eq_ignore_ascii_case(&y)),
    };
    a.chars().count() == b.chars().count() && a.chars().zip(b.chars()).all(|(x, y)| folds(x, y))
}

/// The position, among one scope's names as written, of the one `name` reaches: the only one it
/// may match ([`may_match`]). `None` where none does, and where two do: a server that keeps
/// case takes the one spelled the same, and one that folds names takes whichever has the column,
/// for a table's alias stands beside a derived table's, a WITH query's or a JSON_TABLE's that
/// differs from it in case alone, or not at all.
pub(crate) fn item<'a>(names: impl IntoIterator<Item = &'a str>, name: &str) -> Option<usize> {
    let mut matched = names.into_iter().enumerate().filter(|(_, n)| may_match(n, name)).map(|(i, _)| i);
    match (matched.next(), matched.next()) {
        (Some(i), None) => Some(i),
        _ => None,
    }
}

/// The name the catalog's rows give the table a statement names `table`, where no row is under
/// `table` itself: among the names the rows came back under, the one alone that differs from it in
/// ASCII case alone and that the catalog was not `asked` for itself. The catalog compares names as
/// the server does, so such a row came back for `table` on a server that folds names. A name asked
/// beside it may be another table, on a server that keeps case, and so may any name where `table`
/// was never asked for, or was asked and has no table in the catalog, as a TEMPORARY table has not.
pub(crate) fn answered<'a>(names: impl IntoIterator<Item = &'a str>, asked: impl Fn(&str) -> bool, table: &str) -> Option<&'a str> {
    if !asked(table) {
        return None;
    }
    let mut same = names.into_iter().filter(|name| name.eq_ignore_ascii_case(table) && !asked(name));
    match (same.next(), same.next()) {
        (Some(name), None) => Some(name),
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
    /// Not known: the innermost WITH query the name may match ([`may_match`]) is what a
    /// server that folds names reads, and one that keeps case reads it only where it is spelled the
    /// same, else a WITH query further out or a table.
    Unknown,
}

/// What a bare name in FROM reads among the WITH queries `ctes` names ([`Reads`]).
pub(crate) fn with_query<'a>(ctes: impl IntoIterator<Item = &'a str>, name: &str) -> Reads {
    match ctes.into_iter().enumerate().filter(|(_, c)| may_match(c, name)).last() {
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
        // A server that folds names folds `Ä` to `ä`, and `İ` to `i`.
        assert_eq!(item(["Ä", "ä"], "ä"), None);
        assert_eq!(item(["Ä", "b"], "ä"), Some(0));
        assert_eq!(item(["İd", "id"], "id"), None);
        assert_eq!(item(["Äb", "ä"], "ä"), Some(1));
        assert_eq!(with_query(["Ä"], "ä"), Reads::Unknown);
        assert_eq!(with_query(["Ä"], "Ä"), Reads::With(0));
        assert!(may_match("straße", "STRAßE"));
        assert!(!may_match("ä", "äb"));
        assert!(may_match("\u{212A}ey", "key"));
        assert!(!may_match("ä", "a"));
        // A row under another case answers for a name asked only where its own name was not asked.
        let asked = |names: &'static [&'static str]| move |n: &str| names.contains(&n);
        assert_eq!(answered(["Users"], asked(&["users"]), "users"), Some("Users"));
        assert_eq!(answered(["Users"], asked(&["users", "Users"]), "users"), None);
        assert_eq!(answered(["Users"], asked(&["Users"]), "users"), None);
    }
}

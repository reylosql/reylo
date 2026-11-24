//! The modules `#[reylo::query]` expands to, written to one file: `reylo generate rust-sqlx` writes
//! every module to the file `--out` names, and a build script (`reylo_build::rust_sqlx()`) to one
//! file in Cargo's `OUT_DIR`. Each `.sql` file's module sits at the top of the file, named after the
//! file in snake_case, whatever directory it is in, so `get_user.sql` and `getUser.sql` are each
//! `get_user`, holding `Query`, `Row` and `run`; two files with one name fail the run, naming both.
//!
//! A file written beside the crate's sources checks, as the crate compiles, that each `.sql` file
//! still reads as the text its module was written from: an edited query fails the build until it is
//! generated again, with no database.
//!
//! Each Postgres enum type is declared once, in a module named after its schema in the file's
//! `enums` module (`enums::public::Mood`), the `public` schema's also at the top itself (`Mood`), and
//! each query's module names it as its own (`get_user::Mood`), so every query's value of it is one
//! type.

use crate::{analysis, files, Dates, Decimals, Options, Overrides, SharedEnum};
use proc_macro2::{Span, TokenStream, TokenTree};
use quote::quote;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The first line of every file the generator writes, which it looks for before it writes over one.
const MARK: &str = "// Written by `reylo generate rust-sqlx`";

/// The files to read and where to write their modules. Made by [`rust_sqlx`].
#[derive(Clone, Debug, Default)]
pub struct Generator {
    inputs: Vec<String>,
    out: Option<PathBuf>,
    build_script: bool,
}

/// What stopped a generation: every message, one per line, each naming its file.
#[derive(Clone, PartialEq, Eq)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The message itself, so that a build script's `.unwrap()` prints it as written.
impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// A file the generator wrote, and whether its text changed: one it would write as it already
/// reads is left alone, so that its time stays and Cargo compiles nothing again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Written {
    pub path: PathBuf,
    pub changed: bool,
}

/// The modules of sqlx queries, as `#[reylo::query]` writes them.
pub fn rust_sqlx() -> Generator {
    Generator::default()
}

impl Generator {
    /// A `.sql` file, a directory searched for them, or a glob (`src/**/*.sql`), from the working
    /// directory, or from the crate's directory in a build script. With none, the crate's `src/`.
    pub fn input(mut self, path: impl Into<String>) -> Self {
        self.inputs.push(path.into());
        self
    }

    /// The one file every module is written to, which the command line must name. In a build script
    /// it is a name in `OUT_DIR`, `queries.rs` where none is given.
    pub fn out(mut self, file: impl Into<PathBuf>) -> Self {
        self.out = Some(file.into());
        self
    }

    /// For a build script: inputs read from the crate's directory, the one file written into
    /// `OUT_DIR`, and Cargo told which files to watch.
    pub fn in_build_script(mut self) -> Self {
        self.build_script = true;
        self
    }

    /// Reads every input, analyzes each statement and writes the modules: every file, or none
    /// where any statement fails, with every failure in the error.
    pub fn run(self) -> Result<Vec<Written>, Error> {
        let base = if self.build_script {
            PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").map_err(|_| Error("CARGO_MANIFEST_DIR is not set: reylo_build runs in a build script".into()))?)
        } else {
            std::env::current_dir().map_err(|e| Error(format!("cannot read the working directory: {e}")))?
        };
        let out = match (&self.out, self.build_script) {
            (Some(out), _) => out.clone(),
            (None, true) => "queries.rs".into(),
            (None, false) => return Err(Error("name the file to write every module to, as --out src/queries.rs".into())),
        };
        let inputs = if !self.inputs.is_empty() {
            self.inputs.clone()
        } else if self.build_script {
            vec!["src".to_string()]
        } else {
            default_inputs(&base)?
        };
        let mut found: Vec<PathBuf> = Vec::new();
        let mut errors = Vec::new();
        for input in &inputs {
            match files::sql_files(input, &base) {
                Ok(files) => found.extend(files),
                Err(message) => errors.push(message),
            }
        }
        if !errors.is_empty() {
            return Err(Error(errors.join("\n")));
        }
        found.sort();
        found.dedup();
        if found.is_empty() {
            return Err(Error(format!("no .sql file under {}", inputs.join(", "))));
        }

        // Every file's module is named first, so that two files of one name fail before any is analyzed.
        let mut named: BTreeMap<String, &PathBuf> = BTreeMap::new();
        for file in &found {
            let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            match module_name(&stem) {
                None => errors.push(format!("{}: {stem:?} is no Rust module's name: name the file with letters, digits and underscores, starting with a letter, as get_user.sql", shown(&base, file))),
                Some(name) if name == ENUMS => errors.push(format!("{}: the module {ENUMS} holds the enum types: rename the file", shown(&base, file))),
                Some(name) => match named.get(&name) {
                    Some(other) => errors.push(format!("{} and {} are both the module {name}: rename one", shown(&base, other), shown(&base, file))),
                    None => {
                        named.insert(name, file);
                    }
                },
            }
        }
        if !errors.is_empty() {
            return Err(Error(errors.join("\n")));
        }

        let mut modules = Vec::new();
        let mut options = BTreeMap::new();
        for (name, file) in &named {
            match module(file, &shown(&base, file), &mut options) {
                Ok(generated) => modules.push(Module { file: (*file).clone(), name: name.clone(), tokens: generated.tokens, shared: generated.shared }),
                Err(message) => errors.push(message),
            }
        }
        if !errors.is_empty() {
            return Err(Error(errors.join("\n")));
        }

        if self.build_script {
            let out_dir = PathBuf::from(std::env::var("OUT_DIR").map_err(|_| Error("OUT_DIR is not set: reylo_build runs in a build script".into()))?);
            let out = out_dir.join(out);
            let written = write(&out, &one_file(&modules, &inputs, &base, &out, true)?)?;
            for input in &inputs {
                let dir = if files::is_glob(input) { files::glob_root(input) } else { input.as_str() };
                println!("cargo:rerun-if-changed={}", base.join(dir).display());
            }
            for module in &modules {
                println!("cargo:rerun-if-changed={}", module.file.display());
            }
            let mut watched: Vec<PathBuf> = modules.iter().flat_map(|m| analysis::watched(&package_dir(&m.file))).collect();
            watched.sort();
            watched.dedup();
            for file in watched {
                println!("cargo:rerun-if-changed={}", file.display());
            }
            println!("cargo:rerun-if-env-changed=DATABASE_URL");
            return Ok(vec![written]);
        }
        let out = base.join(out);
        Ok(vec![write(&out, &one_file(&modules, &inputs, &base, &out, false)?)?])
    }
}

/// The module that holds each schema's module of Postgres enum types, a name no query's module may take.
const ENUMS: &str = "enums";

/// A statement's module: its file, its name and its items, and the Postgres enum types it reads from
/// the file's schema modules.
struct Module {
    file: PathBuf,
    name: String,
    tokens: TokenStream,
    shared: Vec<SharedEnum>,
}

/// The `src/` of the crate the working directory is in, found from the nearest `Cargo.toml`, and
/// at a workspace's root, every member's.
fn default_inputs(cwd: &Path) -> Result<Vec<String>, Error> {
    let Some(manifest) = cwd.ancestors().map(|d| d.join("Cargo.toml")).find(|m| m.is_file()) else {
        return Err(Error("no Cargo.toml here or above: name the .sql files, the directories or a glob to read".into()));
    };
    let text = std::fs::read_to_string(&manifest).unwrap_or_default();
    let dir = manifest.parent().unwrap_or(cwd);
    if text.lines().any(|line| line.trim() == "[package]") {
        return Ok(vec![dir.join("src").to_string_lossy().into_owned()]);
    }
    let metadata = cargo_metadata(&manifest).ok_or_else(|| Error(format!("cannot read the members of the workspace {}: name the .sql files, the directories or a glob to read", manifest.display())))?;
    let members: Vec<String> = metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["manifest_path"].as_str())
        .map(|m| Path::new(m).parent().unwrap_or(dir).join("src"))
        .filter(|src| src.is_dir())
        .map(|src| src.to_string_lossy().into_owned())
        .collect();
    if members.is_empty() {
        return Err(Error(format!("no member of the workspace {} has a src/ directory", manifest.display())));
    }
    Ok(members)
}

/// One statement's module, or the messages that say why it cannot be written, each naming the
/// file, and the line and column where the engine places it. Its Postgres enum types are the file's
/// schema modules'.
fn module(file: &Path, shown: &str, options: &mut BTreeMap<PathBuf, Options>) -> Result<crate::Generated, String> {
    let sql = std::fs::read_to_string(file).map_err(|e| format!("{shown}: cannot read it: {e}"))?;
    let package = package_dir(file);
    let (dialect, analysis) = analysis::analyze(&package, &sql).map_err(|message| format!("{shown}: {message}"))?;
    if let Some(diagnostics) = analysis["diagnostics"].as_array().filter(|d| !d.is_empty()) {
        return Err(diagnostics.iter().map(|d| diagnostic(shown, &sql, d)).collect::<Vec<_>>().join("\n"));
    }
    let options = *options.entry(package.clone()).or_insert_with(|| crate_options(&package));
    crate::module(dialect, options, &analysis, Span::call_site(), &Overrides::default(), true).map_err(|e| format!("{shown}: {e}"))
}

/// A file's stem as a module's name: its words in snake_case, written raw where that is a keyword
/// of any edition, `gen` included, which syn reads as a name and Rust 2024 reserves. Words are ASCII
/// letters and digits, which `_`, `-`, `.` and spaces separate and a capital starts (`getUser` and
/// `get-user` are each `get_user`; `HTTPStatus` is `http_status`). A stem with any other character,
/// or whose name would start with a digit, has none.
pub fn module_name(stem: &str) -> Option<String> {
    let name = file_words(stem)?.join("_");
    if !name.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    if name != "gen" && syn::parse_str::<syn::Ident>(&name).is_ok() {
        return Some(name);
    }
    let raw = format!("r#{name}");
    syn::parse_str::<syn::Ident>(&raw).is_ok().then_some(raw)
}

/// The words of a file's stem, lowercase: ASCII letters and digits, split at `_`, `-`, `.` and
/// spaces, and before a capital that follows a lowercase letter or a digit, or that starts a word
/// after a run of capitals; none where the stem holds any other character or no word.
fn file_words(stem: &str) -> Option<Vec<String>> {
    if !stem.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ' ')) {
        return None;
    }
    let mut words = Vec::new();
    for part in stem.split(['_', '-', '.', ' ']).filter(|p| !p.is_empty()) {
        let chars: Vec<char> = part.chars().collect();
        let mut word = String::new();
        for (i, &c) in chars.iter().enumerate() {
            let prev = i.checked_sub(1).map(|j| chars[j]);
            let next = chars.get(i + 1).copied();
            let starts = c.is_ascii_uppercase() && prev.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit() || (p.is_ascii_uppercase() && next.is_some_and(|n| n.is_ascii_lowercase())));
            if starts && !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            word.push(c.to_ascii_lowercase());
        }
        words.push(word);
    }
    (!words.is_empty()).then_some(words)
}

/// `file:line:column: message`, the place the engine gives as a byte offset into the statement.
fn diagnostic(shown: &str, sql: &str, d: &Value) -> String {
    let message = d["message"].as_str().unwrap_or("the statement cannot be analyzed");
    let start = d["start"].as_u64().unwrap_or(0) as usize;
    let before = &sql[..start.min(sql.len())];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    format!("{shown}:{line}:{column}: {message}")
}

/// The crate a file belongs to: the nearest directory above it with a `Cargo.toml`, whose project
/// file and `.env` files name the database, as for the macro.
fn package_dir(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap_or(file);
    parent.ancestors().find(|d| d.join("Cargo.toml").is_file()).unwrap_or(parent).to_path_buf()
}

/// The crates the modules may name for dates and times, uuids, decimals and vectors: the features
/// the crate turns on in its `reylo` dependency, as the macro reads them from its own. In the crate's
/// own build script, so are those its features turned on for this build forward to `reylo`
/// (`vector = ["reylo/pgvector"]`), as Cargo says through `CARGO_FEATURE_<NAME>`.
fn crate_options(package: &Path) -> Options {
    let manifest = package.join("Cargo.toml");
    let Some(metadata) = cargo_metadata(&manifest) else { return Options::default() };
    let same = |a: &str| std::fs::canonicalize(a).ok() == std::fs::canonicalize(&manifest).ok();
    let Some(own) = metadata["packages"].as_array().into_iter().flatten().find(|p| p["manifest_path"].as_str().is_some_and(same)) else { return Options::default() };
    let reylo: Vec<&Value> = own["dependencies"].as_array().into_iter().flatten().filter(|d| d["name"].as_str() == Some("reylo")).collect();
    let mut features: Vec<&str> = reylo.iter().flat_map(|d| d["features"].as_array().into_iter().flatten().filter_map(Value::as_str)).collect();
    let building = std::env::var_os("CARGO_MANIFEST_DIR").is_some_and(|d| std::fs::canonicalize(d).ok() == std::fs::canonicalize(package).ok());
    if building {
        let names: Vec<&str> = reylo.iter().map(|d| d["rename"].as_str().unwrap_or("reylo")).collect();
        for (feature, enables) in own["features"].as_object().into_iter().flatten() {
            if std::env::var_os(format!("CARGO_FEATURE_{}", feature.to_uppercase().replace('-', "_"))).is_none() {
                continue;
            }
            for enabled in enables.as_array().into_iter().flatten().filter_map(Value::as_str) {
                let Some((dep, name)) = enabled.split_once('/') else { continue };
                if names.contains(&dep.trim_end_matches('?')) {
                    features.push(name);
                }
            }
        }
    }
    let has = |name: &str| features.contains(&name);
    Options {
        dates: if has("chrono") {
            Some(Dates::Chrono)
        } else if has("time") {
            Some(Dates::Time)
        } else {
            None
        },
        uuid: has("uuid"),
        pgvector: has("pgvector"),
        decimals: if has("rust_decimal") {
            Some(Decimals::RustDecimal)
        } else if has("bigdecimal") {
            Some(Decimals::BigDecimal)
        } else {
            None
        },
    }
}

/// `cargo metadata` of the workspace `manifest` is in, without its dependencies' own: the cargo
/// running a build script, else the one on the PATH.
fn cargo_metadata(manifest: &Path) -> Option<Value> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = std::process::Command::new(cargo).args(["metadata", "--no-deps", "--format-version", "1", "--offline", "--manifest-path"]).arg(manifest).stderr(std::process::Stdio::null()).output().ok()?;
    output.status.success().then(|| serde_json::from_slice(&output.stdout).ok()).flatten()
}

/// The crates a module may name, for a `.sql` file in `dir`: the features of the nearest crate's
/// `reylo` dependency, or none where `dir` is in no crate or cargo cannot read it. Each crate's are
/// read once, into `read`.
pub fn options_for(dir: &Path, read: &mut BTreeMap<PathBuf, Options>) -> Options {
    let Some(package) = dir.ancestors().find(|d| d.join("Cargo.toml").is_file()) else { return Options::default() };
    *read.entry(package.to_path_buf()).or_insert_with(|| crate_options(package))
}

/// One statement's module as `reylo analyze --format rust` prints it: the items its `.rs` file
/// would hold, without the check that reads its `.sql` file, which a module printed has no path to.
pub fn module_text(dialect: crate::Dialect, options: Options, analysis: &Value) -> Result<String, String> {
    let generated = crate::module(dialect, options, analysis, Span::call_site(), &Overrides::default(), false).map_err(|e| e.to_string())?;
    let mut file: syn::File = syn::parse2(generated.tokens).map_err(|e| format!("the generated module does not parse: {e}"))?;
    crate::readable::readable(&mut file);
    Ok(prettyplease::unparse(&file))
}

/// Every module in one file, at its top, then the `enums` module of each schema's enum types. A
/// build script's file needs no check that each `.sql` file reads as it did, since Cargo runs the
/// script again when one changes.
fn one_file(modules: &[Module], inputs: &[String], base: &Path, out: &Path, build_script: bool) -> Result<String, Error> {
    // Each Postgres enum type once, in its schema's module.
    let mut schemas: BTreeMap<String, BTreeMap<String, &SharedEnum>> = BTreeMap::new();
    for shared in modules.iter().flat_map(|m| &m.shared) {
        let schema = schemas.entry(shared.schema.clone()).or_default();
        match schema.get(&shared.name) {
            Some(other) if other.qualified != shared.qualified => return Err(Error(format!("the enum types {} and {} would both be {ENUMS}::{}::{}: rename one in the database, or generate their queries into separate files", other.qualified, shared.qualified, shared.schema, shared.name))),
            _ => {
                schema.insert(shared.name.clone(), shared);
            }
        }
    }
    let mut items = Vec::new();
    for module in modules {
        let ident = syn::parse_str::<syn::Ident>(&module.name).map_err(|e| Error(e.to_string()))?;
        let tokens = enums_from_query(module.tokens.clone());
        let check = if build_script { None } else { Some(check(&module.file, &relative(out.parent().unwrap_or(Path::new("")), &module.file))?) };
        items.push(quote!(pub mod #ident { #check #tokens }));
    }
    let mut tokens = quote!(#(#items)*);
    if !schemas.is_empty() {
        let enums = syn::parse_str::<syn::Ident>(ENUMS).map_err(|e| Error(e.to_string()))?;
        let schema_modules = schemas
            .iter()
            .map(|(schema, enums)| {
                let ident = syn::parse_str::<syn::Ident>(schema).map_err(|e| Error(e.to_string()))?;
                let definitions = enums.values().map(|e| &e.tokens);
                Ok(quote!(#[doc = " The enum types of one database schema, which the queries' modules name."] pub mod #ident { #(#definitions)* }))
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let spread = schemas.contains_key("public").then(|| quote!(#[allow(unused_imports)] pub use self::#enums::public::*;));
        tokens = quote!(#tokens #[doc = " Each database schema's enum types, a module per schema."] pub mod #enums { #(#schema_modules)* } #spread);
    }
    // The inputs from the file's own directory, so that every checkout writes the same header; a
    // build script's file is in OUT_DIR, which no checkout shares, so its inputs read as given.
    let from: Vec<String> = if build_script { inputs.to_vec() } else { inputs.iter().map(|input| relative(out.parent().unwrap_or(Path::new("")), &base.join(input))).map(|p| if p == "." { "its own directory".into() } else { p }).collect() };
    let header = format!("{MARK} from {}: edit the queries and run it again, never this file.\n\n", from.join(", "));
    format_file(header, tokens)
}

/// `tokens`, a query's module, with the stand-in for the file's top, `__reylo_shared_root`, written
/// as the path to the file's `enums` module from it.
fn enums_from_query(tokens: TokenStream) -> TokenStream {
    let enums = syn::Ident::new(ENUMS, Span::call_site());
    tokens
        .into_iter()
        .flat_map(|tree| -> Vec<TokenTree> {
            match tree {
                TokenTree::Ident(ident) if ident == "__reylo_shared_root" => quote!(super::#enums).into_iter().collect(),
                TokenTree::Group(group) => {
                    let mut inner = proc_macro2::Group::new(group.delimiter(), enums_from_query(group.stream()));
                    inner.set_span(group.span());
                    vec![TokenTree::Group(inner)]
                }
                other => vec![other],
            }
        })
        .collect()
}

/// `const _: () = assert!(…)`: the crate fails to compile, saying why, once the `.sql` file at
/// `include` (from the generated file) no longer reads as the text it was written from.
fn check(file: &Path, include: &str) -> Result<TokenStream, Error> {
    // Lines end in LF, whichever way the checkout writes them: `same_text` reads CRLF as LF.
    let sql = std::fs::read_to_string(file).map_err(|e| Error(format!("cannot read {}: {e}", file.display())))?.replace("\r\n", "\n");
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let message = format!("{name} changed after its module was written: run `reylo generate rust-sqlx` again");
    Ok(quote! {
        const _: () = assert!(reylo::__private::same_text(include_str!(#include), #sql), #message);
    })
}

fn format_file(header: String, tokens: TokenStream) -> Result<String, Error> {
    let mut file: syn::File = syn::parse2(tokens).map_err(|e| Error(format!("the generated module does not parse: {e}")))?;
    crate::readable::readable(&mut file);
    Ok(header + &prettyplease::unparse(&file))
}

/// Writes `text` to `path` unless it already reads so; refuses to write over a file the generator
/// did not write.
fn write(path: &Path, text: &str) -> Result<Written, Error> {
    match std::fs::read_to_string(path) {
        Ok(old) if old == text => return Ok(Written { path: path.to_path_buf(), changed: false }),
        Ok(old) if !old.starts_with(MARK) => return Err(Error(format!("{} exists and reylo generate did not write it: move it, or rename the .sql file", path.display()))),
        _ => {}
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| Error(format!("cannot make {}: {e}", dir.display())))?;
    }
    std::fs::write(path, text).map_err(|e| Error(format!("cannot write {}: {e}", path.display())))?;
    Ok(Written { path: path.to_path_buf(), changed: true })
}

/// The deepest directory every one of `dirs` is in.
fn common_dir<'a>(mut dirs: impl Iterator<Item = &'a Path>) -> PathBuf {
    let Some(first) = dirs.next() else { return PathBuf::new() };
    let mut common = first.to_path_buf();
    for dir in dirs {
        while !dir.starts_with(&common) {
            if !common.pop() {
                return PathBuf::new();
            }
        }
    }
    common
}

/// `to` from the directory `from`, with `/` between its parts, as `include_str!` reads it on
/// every system.
fn relative(from: &Path, to: &Path) -> String {
    let common = common_dir([from, to].into_iter());
    let ups = from.strip_prefix(&common).map_or(0, |rest| rest.components().count());
    let down = to.strip_prefix(&common).unwrap_or(to);
    let mut parts: Vec<String> = std::iter::repeat("..".to_string()).take(ups).collect();
    parts.extend(down.components().map(|c| c.as_os_str().to_string_lossy().into_owned()));
    if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    }
}

/// A path from the working directory, written with `/`, as the type check writes it.
fn shown(base: &Path, path: &Path) -> String {
    match path.strip_prefix(base) {
        Ok(rest) => rest.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/"),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_module_reads_the_enum_types_from_the_files_enums_module() {
        let tokens = quote!(pub use __reylo_shared_root::public::Mood as Mood;);
        assert_eq!(enums_from_query(tokens).to_string(), quote!(pub use super::enums::public::Mood as Mood;).to_string());
    }

    #[test]
    fn a_module_is_named_after_its_file_in_snake_case() {
        assert_eq!(module_name("get_user").as_deref(), Some("get_user"));
        assert_eq!(module_name("get-user").as_deref(), Some("get_user"));
        assert_eq!(module_name("getUser").as_deref(), Some("get_user"));
        assert_eq!(module_name("GetUser").as_deref(), Some("get_user"));
        assert_eq!(module_name("HTTPStatus").as_deref(), Some("http_status"));
        assert_eq!(module_name("user2Posts").as_deref(), Some("user2_posts"));
        assert_eq!(module_name("list users.v2").as_deref(), Some("list_users_v2"));
        assert_eq!(module_name("type").as_deref(), Some("r#type"));
        assert_eq!(module_name("Gen").as_deref(), Some("r#gen"));
        assert_eq!(module_name("1st"), None);
        assert_eq!(module_name("self"), None);
        assert_eq!(module_name("é"), None);
        assert_eq!(module_name("__"), None);
    }

    #[test]
    fn a_path_is_written_from_the_file_that_includes_it() {
        assert_eq!(relative(Path::new("/a/src"), Path::new("/a/src/queries/get.sql")), "queries/get.sql");
        assert_eq!(relative(Path::new("/a/src/out"), Path::new("/a/src/queries/get.sql")), "../queries/get.sql");
        assert_eq!(relative(Path::new("/a/src"), Path::new("/a/src")), ".");
    }

    #[test]
    fn the_common_directory_is_the_deepest_one_every_file_is_in() {
        let dirs = [Path::new("/a/src/q/users"), Path::new("/a/src/q/posts"), Path::new("/a/src/q")];
        assert_eq!(common_dir(dirs.into_iter()), PathBuf::from("/a/src/q"));
        assert_eq!(common_dir([Path::new("/a/src/q")].into_iter()), PathBuf::from("/a/src/q"));
    }

    #[test]
    fn a_diagnostic_is_placed_by_line_and_column() {
        let sql = "select id\nfrom userz";
        let d = serde_json::json!({ "message": "relation \"userz\" does not exist", "start": 15 });
        assert_eq!(diagnostic("q/get.sql", sql, &d), "q/get.sql:2:6: relation \"userz\" does not exist");
    }

    #[test]
    fn a_file_it_did_not_write_is_left_alone() {
        let dir = std::env::temp_dir().join(format!("reylo-generate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("get_user.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();
        assert!(write(&path, &format!("{MARK} from get_user.sql\n")).unwrap_err().0.contains("reylo generate did not write it"));
        std::fs::remove_file(&path).unwrap();
        let text = format!("{MARK} from get_user.sql\n");
        assert_eq!(write(&path, &text).unwrap().changed, true);
        assert_eq!(write(&path, &text).unwrap().changed, false);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

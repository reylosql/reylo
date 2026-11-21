#!/usr/bin/env python3
"""Generates PL/pgSQL's parser tables, rules and keywords from Postgres's own grammar.

    python3 -I engine/syntax/gen/plpgsql.py --postgres <checkout> --libpg-query <checkout>

Takes `pl_gram.y` and PL/pgSQL's keyword lists at the tag below, applies the grammar hunks of the
patches libpg_query carries, and runs Bison over the grammar with every action emptied but kept in
place: PL/pgSQL's actions read tokens from the lexer themselves, so the states in which they run
must be the grammar's own, mid-rule actions included. Needs Bison 3.8.2 (BISON names it) and writes
`src/postgres/plpgsql/generated.rs`.
"""
import argparse, os, re, subprocess, sys, tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.dont_write_bytecode = True
from postgres import LIBPG_QUERY, TAG, git, tables

PATCHES = ["04_lexer_comments_as_tokens"]
HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "..", "src", "postgres", "plpgsql", "generated.rs")
GRAMMAR = "src/pl/plpgsql/src/pl_gram.y"


def grammar_patch(text):
    """The hunks of a patch that touch pl_gram.y."""
    keep, out = False, []
    for part in re.split(r"(?m)^(?=(?:diff |--- ))", text):
        if part.startswith(("diff ", "--- ")):
            keep = GRAMMAR in part.split("\n", 1)[0]
        if keep:
            out.append(part)
    return "".join(out)


def patched_grammar(args):
    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, os.path.dirname(GRAMMAR))
        os.makedirs(path)
        with open(os.path.join(tmp, GRAMMAR), "wb") as f:
            f.write(git(args.postgres, "show", f"{TAG}:{GRAMMAR}"))
        for name in PATCHES:
            text = git(args.libpg_query, "show", f"{LIBPG_QUERY}:patches/{name}.patch").decode("utf8", "replace")
            hunks = grammar_patch(text)
            if hunks:
                subprocess.run(["patch", "-s", "-p1", "--no-backup-if-mismatch"], cwd=tmp, input=hunks.encode(), check=True)
        with open(os.path.join(tmp, GRAMMAR), encoding="utf8", errors="replace") as f:
            return f.read()


def emptied_actions(source):
    """The grammar with each action's code removed and its braces kept, and the declarations Bison
    needs for that: no value types, so no action is asked for one."""
    head, rules, *_ = re.split(r"^%%\s*$", source, flags=re.M)
    out, i, depth = [], 0, 0
    while i < len(rules):
        c = rules[i]
        if rules.startswith("/*", i):
            i = rules.find("*/", i + 2) + 2
            if not depth:
                out.append(" ")
            continue
        if not depth and c == "'":
            end = rules.find("'", i + 1) + 1
            out.append(rules[i:end])
            i = end
            continue
        if depth and c in "\"'":
            j = i + 1
            while rules[j] != c:
                j += 2 if rules[j] == "\\" else 1
            i = j + 1
            continue
        if c == "{":
            if not depth:
                out.append("{}")
            depth += 1
        elif c == "}":
            depth -= 1
        elif not depth:
            out.append(c)
        i += 1
    head = re.sub(r"%\{.*?%\}", "", head, flags=re.S)
    head = re.sub(r"/\*.*?\*/", " ", head, flags=re.S)
    # The union of value types nests braces of its own.
    at = head.find("%union")
    if at >= 0:
        open_at, depth, j = head.index("{", at), 0, head.index("{", at)
        while True:
            depth += {"{": 1, "}": -1}.get(head[j], 0)
            j += 1
            if depth == 0:
                break
        head = head[:at] + head[j:]
    head = re.sub(r"^%(parse-param|lex-param|name-prefix|locations|pure-parser|define).*$", "", head, flags=re.M)
    head = re.sub(r"%type\s*<\w+>", "%nterm", head)
    head = re.sub(r"%token\s*<\w+>", "%token", head)
    return head + "\n%%\n" + "".join(out) + "\n%%\n"


def rules_of(report):
    """Every rule Bison numbered, from its report: its left side and the symbols on its right."""
    grammar = report.split("\nGrammar\n", 1)[1].split("\n\nTerminals", 1)[0]
    rules, lhs = {}, None
    for line in grammar.split("\n"):
        m = re.match(r"\s*(\d+) (?:(\S+):|\s*\|)(.*)$", line)
        if not m:
            continue
        number, name, rhs = int(m.group(1)), m.group(2), m.group(3).split()
        lhs = name or lhs
        rules[number] = (lhs, [] if rhs in (["%empty"], ["ε"]) else rhs)
    # The tables number rules from 1, the report from 0: the tables' rule 0 is no rule.
    return [("$none", [])] + [rules[n] for n in sorted(rules)]


def keywords(args, name):
    text = git(args.postgres, "show", f"{TAG}:src/pl/plpgsql/src/{name}").decode()
    return re.findall(r'PG_KEYWORD\("(\w+)", (\w+)\)', text)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--postgres", required=True)
    parser.add_argument("--libpg-query", required=True)
    args = parser.parse_args()
    source = patched_grammar(args)
    bison = os.environ.get("BISON", "/opt/homebrew/opt/bison/bin/bison")
    with tempfile.TemporaryDirectory() as tmp:
        y, c, report = os.path.join(tmp, "pl_gram.y"), os.path.join(tmp, "pl_gram.c"), os.path.join(tmp, "pl_gram.output")
        open(y, "w", encoding="utf8").write(emptied_actions(source))
        subprocess.run([bison, "-Wno-other", "-Wno-empty-rule", "-v", "--report-file", report, "-o", c, y], check=True)
        arrays, constants, names = tables(open(c, encoding="utf8").read())
        rules = rules_of(open(report, encoding="utf8").read())
    if len(rules) != len(arrays["r1"]):
        raise SystemExit(f"{len(rules)} rules in the report, {len(arrays['r1'])} in the tables")
    for number, (lhs, rhs) in list(enumerate(rules))[1:]:
        if names[arrays["r1"][number]] != lhs or arrays["r2"][number] != len(rhs):
            raise SystemExit(f"rule {number}: the report has {lhs}/{len(rhs)}, the tables {names[arrays['r1'][number]]}/{arrays['r2'][number]}")
    symbol = {n: i for i, n in enumerate(names)}

    def rust_array(name, values, ty="i32"):
        return f"pub static {name}: [{ty}; {len(values)}] = [{', '.join(map(str, values))}];\n"

    def const_name(n):
        return {"';'": "SEMI", "'('": "LPAREN", "')'": "RPAREN", "','": "COMMA", "'['": "LBRACKET", "']'": "RBRACKET", "'.'": "DOT", "'='": "EQUALS", "'#'": "HASH", "'<'": "LESS", "'>'": "GREATER", "$end": "END"}.get(n)

    out = [f"// Generated by gen/plpgsql.py from Postgres {TAG}'s pl_gram.y. Do not edit.\n\n"]
    for k in ["pact", "pgoto", "defact", "defgoto", "table", "check", "r1", "r2"]:
        out.append(rust_array(k.upper(), arrays[k]))
    out.append(f"pub const PACT_NINF: i32 = {constants['pact_ninf']};\npub const TABLE_NINF: i32 = {constants['table_ninf']};\npub const FINAL: i32 = {constants['final']};\npub const LAST: i32 = {constants['last']};\npub const NTOKENS: i32 = {constants['ntokens']};\n\n")
    out.append("/// Each symbol's name, by its number.\npub static NAMES: [&str; %d] = [%s];\n\n" % (len(names), ", ".join('"' + n.replace("\\", "\\\\").replace('"', '\\"') + '"' for n in names)))
    out.append("/// Each rule's left side and the symbols on its right, by its number, as Bison's report lists them.\npub static RULES: [(&str, &[&str]); %d] = [%s];\n\n" % (len(rules), ", ".join('("%s", &[%s])' % (l, ", ".join('"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"' for s in r)) for l, r in rules)))
    out.append("/// The terminals, by name.\n#[allow(dead_code)]\npub mod t {\n")
    for n in names[: constants["ntokens"]]:
        name = const_name(n) or (n if re.fullmatch(r"[A-Z_][A-Z0-9_]*", n) else None)
        if name:
            out.append(f"    pub const {name}: u16 = {symbol[n]};\n")
    out.append("}\n\n")
    for kind, file in [("RESERVED", "pl_reserved_kwlist.h"), ("UNRESERVED", "pl_unreserved_kwlist.h")]:
        words = keywords(args, file)
        out.append(f"/// PL/pgSQL's {kind.lower()} keywords and their tokens, in the order of `{file}`.\npub static {kind}: [(&str, u16); {len(words)}] = [{', '.join(f'(\"{w}\", t::{tok})' for w, tok in words)}];\n")
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    open(OUT, "w").write("".join(out))
    print(f"{len(rules)} rules, {len(arrays['pact'])} states, {constants['ntokens']} terminals")


if __name__ == "__main__":
    main()

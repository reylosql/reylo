#!/usr/bin/env python3
"""Generates the MySQL parser's tables and keyword lists from TiDB's grammar.

    python3 engine/syntax/gen/mysql.py --tidb <checkout of pingcap/tidb>

Takes `pkg/parser/parser.y` and the keyword maps of `pkg/parser/misc.go` at the commit below (Apache
License 2.0), removes the Go actions, adds the MySQL syntax TiDB lacks (`mysql_grammar.py`) and
runs Bison over the result. MySQL's own grammar is GPL, so nothing here is read from it. Needs Bison 3.8.2 (BISON names it) and
writes `src/mysql/generated/`.
"""
import argparse, hashlib, json, os, re, struct, subprocess, sys, tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.dont_write_bytecode = True
import mysql_grammar

COMMIT = "bf712cf67f6f711117d8246d46586e47ad851b6b"
HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "..", "src", "mysql", "generated")


def git(repo, *args):
    return subprocess.run(["git", "-C", repo, *args], check=True, capture_output=True).stdout


def skip_quoted(text, j, quote):
    k = j + 1
    while text[k] != quote:
        k += 2 if (text[k] == "\\" and quote != "`") else 1
    return k + 1


def without_actions(source):
    """The grammar with its prologue, `%union`, type tags and `%type` lines gone and each action `{}`."""
    head, rules = source.split("\n%%\n", 2)[:2]
    head = re.sub(r"(?s)^%\{.*?\n%\}\n", "", head)
    head = re.sub(r"(?s)%union\s*\{.*?\n\}\n", "", head)
    head = re.sub(r"(?ms)^%type\b.*?(?=^%)", "", head)
    head = re.sub(r"<\w+>", "", head)
    out, i, n = [], 0, len(rules)
    while i < n:
        c = rules[i]
        if rules.startswith("/*", i):
            j = rules.index("*/", i) + 2
            out.append(" ")
            i = j
        elif rules.startswith("//", i):
            j = rules.find("\n", i)
            i = n if j < 0 else j
        elif c in "\"'":
            j = skip_quoted(rules, i, c)
            out.append(rules[i:j])
            i = j
        elif c == "{":
            depth, j = 0, i
            while True:
                d = rules[j]
                if rules.startswith("//", j):
                    j = rules.find("\n", j)
                    continue
                if rules.startswith("/*", j):
                    j = rules.index("*/", j) + 2
                    continue
                if d in "\"'`":
                    j = skip_quoted(rules, j, d)
                    continue
                if d == "{":
                    depth += 1
                elif d == "}":
                    depth -= 1
                    if depth == 0:
                        break
                j += 1
            out.append("{}")
            i = j + 1
        else:
            out.append(c)
            i += 1
    return head + "\n%%\n" + "".join(out) + "\n"


def tables(c_source):
    def array(name):
        m = re.search(r"static const [\w ]+?\b" + name + r"\[\]\s*=\s*\{(.*?)\};", c_source, re.S)
        return [int(x) for x in re.findall(r"-?\d+", m.group(1))]

    def define(name):
        return int(re.search(r"#define " + name + r"\s+\(?(-?\d+)\)?", c_source).group(1))

    names = re.search(r"yytname\[\]\s*=\s*\{(.*?)\};", c_source, re.S).group(1)
    names = [bytes(x, "utf8").decode("unicode_escape") for x in re.findall(r'"((?:[^"\\]|\\.)*)"', names)]
    return {k: array("yy" + k) for k in "pact pgoto defact defgoto table check r1 r2".split()}, {
        "pact_ninf": define("YYPACT_NINF"),
        "table_ninf": define("YYTABLE_NINF"),
        "final": define("YYFINAL"),
        "last": define("YYLAST"),
        "ntokens": define("YYNTOKENS"),
    }, names


def declared_tokens(source):
    """Each `%token` name and the string it is spelled as, where it has one."""
    head = source.split("\n%%\n", 1)[0]
    out = {}
    for block in re.findall(r"(?ms)^%token\b(.*?)(?=^%)", head):
        block = re.sub(r"(?s)/\*.*?\*/", " ", block)
        block = re.sub(r"<\w+>", " ", block)
        for name, alias in re.findall(r'(\w+)(?:\s+("(?:[^"\\]|\\.)*"))?', block):
            out[name] = json.loads(alias) if alias else None
    return out


RUST_KEYWORDS = set("as async await break const continue crate dyn else enum extern false fn for gen if impl in let loop match mod move mut pub ref return self Self static struct super trait true type unsafe use where while abstract become box do final macro override priv try typeof unsized virtual yield".split())


def rust_ident(name):
    if name not in RUST_KEYWORDS:
        return name
    return name + "_" if name in ("self", "Self", "super", "crate") else "r#" + name


def go_map(misc, name):
    body = re.search(r"var " + name + r" = map\[string\](?:int|string)\{(.*?)\n\}", misc, re.S).group(1)
    return re.findall(r'"([^"]+)":\s*"?(\w+)"?,', body)


def paren_followers(rules_text, start):
    """The symbols `(` may come right after in what the rule `start` reads, by the FOLLOW sets of
    the rules it reaches, but where a rule that takes a keyword as a name reads it: there a name
    before `(` is right."""
    names_rules = {"UnReservedKeyword", "TiDBKeyword", "NotKeywordToken", "LabelName"}
    rules = {}
    for name, body in re.findall(r"(?ms)^([A-Za-z_]\w*):(.*?)(?=^[A-Za-z_]\w*:|\Z)", rules_text):
        body = re.sub(r"\{\}|%prec \S+|/\*.*?\*/", " ", body)
        rules.setdefault(name, []).extend(re.findall(r'"(?:[^"\\]|\\.)*"|\'.\'|[A-Za-z_]\w*', a) for a in body.split("|"))
    # The rules `start` reaches; the others are statements of TiDB's own, which MySQL doesn't have.
    reached, todo = set(), [start]
    while todo:
        name = todo.pop()
        if name in reached or name not in rules:
            continue
        reached.add(name)
        todo.extend(sym for alt in rules[name] for sym in alt if sym in rules)
    nullable, first = set(), {n: set() for n in rules}
    def first_of(seq):
        out = set()
        for sym in seq:
            if sym not in rules:
                out.add(sym)
                return out, False
            out |= first[sym]
            if sym not in nullable:
                return out, False
        return out, True
    changed = True
    while changed:
        changed = False
        for name, alts in rules.items():
            for alt in alts:
                f, empty = first_of(alt)
                if not f <= first[name]:
                    first[name] |= f
                    changed = True
                if empty and name not in nullable:
                    nullable.add(name)
                    changed = True
    follow = {}
    changed = True
    while changed:
        changed = False
        for name, alts in rules.items():
            if name in names_rules or name not in reached:
                continue
            for alt in alts:
                for i, sym in enumerate(alt):
                    f, empty = first_of(alt[i + 1:])
                    if empty:
                        f = f | follow.get(name, set())
                    have = follow.setdefault(sym, set())
                    if not f <= have:
                        have |= f
                        changed = True
    return {sym for sym, f in follow.items() if sym not in rules and "'('" in f}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--tidb", required=True)
    parser.add_argument("--keep", help="a directory to keep the grammar Bison read and its report in")
    args = parser.parse_args()
    source = git(args.tidb, "show", f"{COMMIT}:pkg/parser/parser.y").decode()
    misc = git(args.tidb, "show", f"{COMMIT}:pkg/parser/misc.go").decode()
    base = without_actions(source)
    bison = os.environ.get("BISON", "/opt/homebrew/opt/bison/bin/bison")
    # A keyword TiDB reserves and MySQL doesn't is a name, unless the grammar can't read it as one
    # where it reads the keyword: each word a conflict names goes back to reserved.
    mysql_reserved = {w for w, r in (line.split() for line in open(os.path.join(HERE, "mysql-8.4-keywords.txt")) if not line.startswith("#")) if r == "1"}
    declared = declared_tokens(source)
    added = {f'"{w}"' for w, _ in mysql_grammar.KEYWORDS}
    taken = mysql_grammar.keyword_rules(mysql_grammar.apply(base))
    unreserve = sorted({json.dumps(declared[g]) for w, g in go_map(misc, "tokenMap") if declared.get(g) and w == declared[g] and w not in mysql_reserved} - taken - added)
    kept = []
    with tempfile.TemporaryDirectory() as tmp:
        y, c, out = (os.path.join(tmp, n) for n in ("parser.y", "parser.c", "parser.output"))
        while True:
            stripped = mysql_grammar.apply(base, unreserve)
            open(y, "w", encoding="utf8").write(stripped)
            run = subprocess.run([bison, "-Wno-other", "-Wno-empty-rule", "-v", "--report-file", out, "-o", c, y], capture_output=True, text=True)
            if run.returncode and "conflict" not in run.stderr:
                raise SystemExit(run.stderr)
            if "conflict" not in run.stderr:
                break
            report = open(out, encoding="utf8").read()
            states = re.split(r"\nState \d+\n", report)
            heads = [int(n) for n in re.findall(r"^State (\d+) conflicts", report, re.M)]
            numbered = dict(zip([int(n) for n in re.findall(r"\nState (\d+)\n", report)], states[1:]))
            # A word is to blame where it is the item a conflict reduces, or the token it looks ahead at.
            blamed = [m for n in heads for m in re.findall(r'(?:UnReservedKeyword|LabelName): ("[A-Z0-9_]+") •', numbered[n]) + re.findall(r'^\s+("[A-Z0-9_]+")\s+\[', numbered[n], re.M) if m in unreserve]
            # One word a round, the one most conflicts name, so a word that only shares a state with it stays a name.
            culprits = {max(set(blamed), key=blamed.count)} if blamed else set()
            if not culprits:
                raise SystemExit(run.stderr + "".join(f"\nState {n}\n{numbered[n][:1500]}" for n in heads))
            kept += sorted(culprits)
            unreserve = [u for u in unreserve if u not in culprits]
        if args.keep:
            os.makedirs(args.keep, exist_ok=True)
            open(os.path.join(args.keep, "parser.y"), "w", encoding="utf8").write(stripped)
            open(os.path.join(args.keep, "parser.output"), "w", encoding="utf8").write(open(out, encoding="utf8").read())
        arrays, constants, names = tables(open(c, encoding="utf8").read())
    print(f"{len(unreserve)} of TiDB's reserved words MySQL doesn't reserve are names; reserved still, for the grammar's sake: {' '.join(sorted(kept))}")
    order = ["pact", "pgoto", "defact", "defgoto", "table", "check", "r1", "r2"]
    os.makedirs(OUT, exist_ok=True)
    wide = {k for k in order if max(map(abs, arrays[k])) > 32767}
    blob = b"".join(struct.pack(f"<{len(arrays[k])}{'i' if k in wide else 'h'}", *arrays[k]) for k in order)

    ntokens = constants["ntokens"]
    index = {n: i for i, n in enumerate(names[:ntokens])}
    tokens = declared_tokens(source)
    tokens.update({mysql_grammar.token(w): w for w, _ in mysql_grammar.KEYWORDS})
    keyword_map = go_map(misc, "tokenMap") + [(w, mysql_grammar.token(w)) for w, _ in mysql_grammar.KEYWORDS]

    def symbol(go_name):
        alias = tokens.get(go_name, "missing")
        if alias == "missing":
            raise SystemExit(f"misc.go names {go_name}, which parser.y doesn't declare")
        spelled = json.dumps(alias) if alias is not None else go_name
        return index[spelled]

    L = [f"// Generated by engine/syntax/gen/mysql.py from TiDB {COMMIT}'s pkg/parser/parser.y and misc.go.", f"// parser.y sha256 {hashlib.sha256(source.encode()).hexdigest()}, with gen/mysql_grammar.py's additions.", ""]
    for k, v in constants.items():
        L.append(f"pub const {k.upper()}: i32 = {v};")
    offset = 0
    segments = []
    for k in order:
        width = 4 if k in wide else 2
        segments.append(f"({offset}, {len(arrays[k])}, {width})")
        offset += width * len(arrays[k])
    L.append("/// Each table's start, length and integer width in bytes in `tables.bin`, little-endian.")
    L.append("pub const SEGMENTS: [(usize, usize, usize); 8] = [" + ", ".join(segments) + "];")
    L.append("")
    L.append("/// The terminals, by the names TiDB's grammar gives them, a Rust keyword written raw.\n#[allow(non_upper_case_globals, dead_code)]\npub mod tok {")
    for go_name in sorted(tokens):
        if re.fullmatch(r"[A-Za-z_]\w*", go_name) and go_name != "error":
            L.append(f"    pub const {rust_ident(go_name)}: u16 = {symbol(go_name)};")
    L.append("}\n")
    chars = [0] * 128
    for i, n in enumerate(names[:ntokens]):
        if len(n) == 3 and n[0] == "'" and n[2] == "'" and ord(n[1]) < 128:
            chars[ord(n[1])] = i
    L.append("/// The grammar's symbol of each ASCII character token, 0 where it names none.")
    L.append("pub static CHAR_TOKENS: [u16; 128] = [" + ", ".join(map(str, chars)) + "];\n")

    def table(name, doc, pairs):
        L.append(f"/// {doc}")
        L.append(f"pub static {name}: [(&str, u16); {len(pairs)}] = [")
        L.extend(f'    ("{word}", {symbol(go_name)}),' for word, go_name in sorted(pairs))
        L.append("];\n")

    table("KEYWORDS", "Every keyword, sorted by its uppercase spelling, and its symbol.", keyword_map)
    # TiDB's own aggregates and TRANSLATE are no built-ins of MySQL's, where each is a stored function's name.
    tidb_functions = {"APPROX_COUNT_DISTINCT", "APPROX_PERCENTILE", "MAX_COUNT", "MIN_COUNT", "SUM_INT", "TRANSLATE"}
    table("FUNCTIONS", "The builtin functions that are keywords only before `(`, sorted.", [(w, g) for w, g in go_map(misc, "btFuncTokenMap") if w not in tidb_functions])
    table("WINDOW_FUNCTIONS", "The window functions' keywords, sorted.", go_map(misc, "windowFuncTokenMap"))
    hinted = re.search(r"var hintedTokens = map\[int\]struct\{\}\{(.*?)\n\}", misc, re.S).group(1)
    hinted = re.findall(r"(\w+):\s*\{\}", hinted)
    L.append(f"/// The keywords an optimizer hint comment may follow.\npub static HINTED: [u16; {len(hinted)}] = [" + ", ".join(str(symbol(h)) for h in hinted) + "];\n")
    # A keyword is reserved unless one of the rules that take a keyword as a name lists it.
    names_ok = set()
    for rule in ["UnReservedKeyword", "TiDBKeyword", "NotKeywordToken"]:
        body = re.search(r"(?ms)^" + rule + r":(.*?)^(?=[A-Za-z]\w*:)", stripped.split("\n%%\n", 1)[1]).group(1)
        names_ok |= {index[json.dumps(json.loads(s))] for s in re.findall(r'"(?:[^"\\]|\\.)*"', body)}
    # The words a name spelled like one needs quotes for: MySQL's reserved words, and those this grammar can't read as a name.
    reserved = sorted({word for word, go_name in keyword_map if symbol(go_name) not in names_ok} | mysql_reserved)
    L.append(f"/// The keywords no rule takes as a name, sorted.\npub static RESERVED: [&str; {len(reserved)}] = [" + ", ".join(f'"{w}"' for w in reserved) + "];\n")
    # A word the grammar spells as a token is one only if the scanner makes it.
    spelled = {"UNDERSCORE_CHARSET"} | {w for w, _ in keyword_map} | {w for w, _ in go_map(misc, "btFuncTokenMap")} | {w for w, _ in go_map(misc, "windowFuncTokenMap")}
    unmade = [n for n in names[:ntokens] if re.fullmatch(r'"[A-Z][A-Z0-9_]*"', n) and n[1:-1] not in spelled]
    if unmade:
        raise SystemExit("the grammar names tokens no keyword makes: " + " ".join(unmade))
    # A word TiDB keeps as a keyword and MySQL 8.4 doesn't know is a name before `(`, where no
    # expression reads the keyword with `(` after it (`ADDDATE(`, `CAST(x AS VECTOR(3))`): a statement
    # that does so elsewhere is TiDB's own (`TRACE PLAN (…)`).
    mysql_words = {line.split()[0] for line in open(os.path.join(HERE, "mysql-8.4-keywords.txt")) if not line.startswith("#")}
    before_paren = paren_followers(stripped.split("\n%%\n", 1)[1], "SimpleExpr")
    functions = {w for w, _ in go_map(misc, "btFuncTokenMap")}
    tidb_only = {w for w, g in keyword_map if w not in mysql_words and w not in functions and json.dumps(w) not in before_paren and g not in before_paren}
    words = sorted(mysql_grammar.names_before_paren())
    L.append(f"/// Keywords that are a function's name before `(`, sorted.\npub static NAMES_BEFORE_PAREN: [&str; {len(words)}] = [" + ", ".join(f'"{w}"' for w in words) + "];\n")
    # MySQL knows none of these words, so before `(`, spaced or not, each is a stored function's name.
    words = sorted(tidb_only | tidb_functions)
    L.append(f"/// TiDB's keywords MySQL doesn't have: a function's name before `(`, after any spaces, sorted.\npub static TIDB_NAMES: [&str; {len(words)}] = [" + ", ".join(f'"{w}"' for w in words) + "];\n")
    L.append(f"pub static NAMES: [&str; {len(names)}] = [")
    L.extend("    " + json.dumps(n) + "," for n in names)
    L.append("];")
    open(os.path.join(OUT, "tables.bin"), "wb").write(blob)
    open(os.path.join(OUT, "tokens.rs"), "w").write("\n".join(L) + "\n")
    print(f"{ntokens} tokens, {len(arrays['r1']) - 1} rules, {len(arrays['pact'])} states, {len(blob)} table bytes")


if __name__ == "__main__":
    main()

// A migration's text as `reylo migrate` compares it, ported from engine/migrate/src/commands.rs
// (`normalized`) and engine/migrate/src/lock.rs (`canonical`), which change together with this
// file: a Durable Object holds what it ran against the bundle (src/migrate.ts), and the build holds
// the files against the lock (src/embedded-module.ts), by the rule the deploy and the lock use, so
// that an edit to a comment, to spacing or to line endings is no edit there either. It runs inside
// a Durable Object, so it imports nothing.

/** The text with its `\r\n` line endings as `\n` and no trailing ASCII spacing. */
export function normalized(sql: string): string {
  return sql.replace(/\r\n/g, "\n").replace(/[ \t\n\f\r]+$/, "");
}

// Spacing is ASCII's alone, as Rust's `is_ascii_whitespace`: the databases read a U+3000 or a
// U+00A0 outside quotes and comments as part of a name.
const SPACING = /^[ \t\n\f\r]$/;
// A character of a dollar quote's tag, as Postgres reads one: any past ASCII.
const TAG = /^[A-Za-z0-9_]$|^[^\x00-\x7f]$/u;

/**
 * SQL without its comments, its whitespace runs as one space, as any of the three databases reads
 * it: what a lock's checksum covers. Quoted text (`'…'`, `"…"`, `` `…` ``, and Postgres's
 * `$tag$…$tag$`) and MySQL's `/*! … *\/` are kept as written, and so is everything after a string,
 * a comment, a `--` or a `#` that the databases read differently, or a comment that never ends, so
 * that text one of them runs is never dropped as a comment.
 */
export function canonical(sql: string): string {
  const b = Array.from(sql);
  let out = "";
  let space = false;
  let k = 0;
  const push = (text: string) => {
    if (space && out !== "") out += " ";
    space = false;
    out += text;
  };
  const rest = (from: number, to = b.length) => b.slice(from, to).join("");
  // SQLite reads `[…]` as a name, up to its first `]`, and Postgres as a subscript: where a quote or
  // a comment that opens inside one ends past that `]`, the rest is kept as written.
  let bracket: number | undefined;
  while (k < b.length) {
    const c = b[k]!;
    const [start, written, spaced] = [k, out.length, space];
    if (bracket !== undefined && k > bracket) bracket = undefined;
    if (c === "[" && bracket === undefined) {
      const e = b.indexOf("]", k + 1);
      bracket = e < 0 ? undefined : e;
    }
    if (c === "-" && b[k + 1] === "-") {
      // MySQL reads `--` as a comment only where a space or a control character follows it, Postgres
      // and SQLite everywhere: where the two differ, the rest is kept as written.
      const next = b[k + 2];
      if (next !== undefined && next.codePointAt(0)! > 0x20) {
        push(rest(k));
        k = b.length;
        continue;
      }
      // Postgres ends one at a lone carriage return too, MySQL and SQLite only at a line feed.
      let end = k;
      while (end < b.length && b[end] !== "\n") end++;
      if (b.slice(k, end).some((ch, j) => ch === "\r" && b[k + j + 1] !== "\n")) {
        push(rest(k));
        k = b.length;
        continue;
      }
      k = end;
      space = true;
    } else if (c === "#") {
      // MySQL reads `#` as a line comment, Postgres as an operator: the rest is kept as written.
      push(rest(k));
      k = b.length;
    } else if (c === "/" && b[k + 1] === "*") {
      // Postgres nests block comments, while MySQL and SQLite end one at its first `*/`:
      // where the two end it at different places, the rest is kept as written, and so are MySQL's
      // `/*! … */` and the rest after a comment that never ends.
      let flat: number | undefined;
      for (let j = k + 2; j < b.length - 1; j++) {
        if (b[j] === "*" && b[j + 1] === "/") {
          flat = j + 2;
          break;
        }
      }
      let depth = 0;
      let nested = k;
      while (nested < b.length) {
        if (b[nested] === "/" && b[nested + 1] === "*") {
          depth++;
          nested += 2;
        } else if (b[nested] === "*" && b[nested + 1] === "/") {
          depth--;
          nested += 2;
          if (depth === 0) break;
        } else {
          nested++;
        }
      }
      if (flat === undefined || flat !== nested) {
        push(rest(k));
        k = b.length;
      } else if (b[k + 2] === "!") {
        push(rest(k, flat));
        k = flat;
      } else {
        k = flat;
        space = true;
      }
    } else if (SPACING.test(c)) {
      space = true;
      k++;
    } else if (c === "'" || c === '"' || c === "`") {
      // MySQL reads a backslash in a string as escaping the next character, and so does Postgres in
      // an `E'…'` string: where the two readings end a string at different places, the rest is kept
      // as written.
      const standard = quotedEnd(b, k, c, false);
      const end = c === "`" || quotedEnd(b, k, c, true) === standard ? standard : b.length;
      push(rest(k, end));
      k = end;
    } else if (c === "$" && (k === 0 || !(TAG.test(b[k - 1]!) || b[k - 1] === "$"))) {
      // A dollar-quoted string: $tag$ … $tag$. A `$` inside a name, as `x$$a$`, opens none.
      let tagEnd = k + 1;
      while (tagEnd < b.length && TAG.test(b[tagEnd]!)) tagEnd++;
      if (tagEnd < b.length && b[tagEnd] === "$") {
        const tag = rest(k, tagEnd + 1);
        const body = rest(tagEnd + 1);
        const at = body.indexOf(tag);
        const end = at < 0 ? b.length : tagEnd + 1 + Array.from(body.slice(0, at)).length + Array.from(tag).length;
        push(rest(k, end));
        k = end;
      } else {
        push("$");
        k++;
      }
    } else {
      push(c);
      k++;
    }
    if (bracket !== undefined && start < bracket && k > bracket) {
      out = out.slice(0, written);
      space = spaced;
      push(rest(start));
      break;
    }
  }
  return out;
}

/** Where the string or identifier quoted by `q` at `start` ends: after its closing quote, a doubled quote being one quote of its text, and, with `backslash`, a backslash escaping the next character. */
function quotedEnd(b: string[], start: number, q: string, backslash: boolean): number {
  let k = start + 1;
  while (k < b.length) {
    if (backslash && b[k] === "\\") k += 2;
    else if (b[k] === q) {
      if (b[k + 1] === q) {
        k += 2;
        continue;
      }
      return k + 1;
    } else k++;
  }
  return b.length;
}

/** Whether two texts of a migration are the same migration, as the lock's checksum compares them. */
export const sameMigration = (a: string, b: string): boolean => canonical(normalized(a)) === canonical(normalized(b));

// The statements of SQLite's that end or open a transaction, which a migration can't hold of its own.
const CONTROL = new Set(["BEGIN", "COMMIT", "END", "ROLLBACK", "SAVEPOINT", "RELEASE"]);

/**
 * The tokens of each statement of a SQLite migration, as SQLite reads the text: a `--` comment to
 * the line's end, a `/*` one to the first `*\/`, a byte order mark as spacing, quotes doubled inside
 * their own, and a trigger as one statement, which ends at the `;` after an `end` that itself follows
 * a `;`, as SQLite completes one (`sqlite3_complete`), not after a `case`'s `end` or a column named
 * `end`. A name is uppercase, a number as written, and a quoted text or name `'` followed by its
 * content; symbols are left out.
 */
export function statementTokens(sql: string): string[][] {
  const statements: string[][] = [];
  let head: string[] = [];
  let trigger = false;
  let afterSemicolon = false;
  let afterEnd = false;
  let afterDot = false;
  const token = (text: string) => {
    head.push(text);
    afterSemicolon = afterEnd = afterDot = false;
  };
  let k = 0;
  while (k < sql.length) {
    const c = sql[k]!;
    if (SPACING.test(c) || c === "\uFEFF") {
      k++;
      continue;
    }
    if (c === "-" && sql[k + 1] === "-") {
      const end = sql.indexOf("\n", k);
      k = end < 0 ? sql.length : end + 1;
      continue;
    }
    if (c === "/" && sql[k + 1] === "*") {
      const end = sql.indexOf("*/", k + 2);
      k = end < 0 ? sql.length : end + 2;
      continue;
    }
    if (c === "'" || c === '"' || c === "`" || c === "[") {
      const close = c === "[" ? "]" : c;
      let end = k + 1;
      for (; end < sql.length; end++) {
        if (sql[end] !== close) continue;
        if (close !== "]" && sql[end + 1] === close) end++;
        else break;
      }
      token(`'${sql.slice(k + 1, end).replaceAll(close + close, close)}`);
      k = end + 1;
      continue;
    }
    // A number is one token, its dot and exponent included (`1.`, `2.5e-3`), so its dot qualifies no name.
    if (/[0-9]/.test(c) || (c === "." && /[0-9]/.test(sql[k + 1] ?? ""))) {
      // A sign belongs to a decimal number's exponent alone, and only before its digits.
      const decimal = !/^0[xX]/.test(sql.slice(k, k + 2));
      let end = k + 1;
      while (end < sql.length && (/[0-9A-Za-z_.]/.test(sql[end]!) || (decimal && /[+-]/.test(sql[end]!) && /[eE]/.test(sql[end - 1]!) && /[0-9]/.test(sql[end + 1] ?? "")))) end++;
      token(sql.slice(k, end));
      k = end;
      continue;
    }
    // A name, as SQLite reads one: any character past ASCII is part of it.
    if (/[A-Za-z_]/.test(c) || c.charCodeAt(0) >= 0x80) {
      let end = k;
      while (end < sql.length && (/[A-Za-z0-9_$]/.test(sql[end]!) || sql.charCodeAt(end) >= 0x80)) end++;
      const word = sql.slice(k, end).toUpperCase();
      // A trigger's body closes at an `end` right after one of its statements' `;`.
      const closes = trigger && afterSemicolon && !afterDot && word === "END";
      token(word);
      afterEnd = closes;
      // `create trigger` or `create temp trigger`: elsewhere, as in `create table trigger`, it's a name.
      if (word === "TRIGGER" && head[0] === "CREATE" && (head.length === 2 || (head.length === 3 && (head[1] === "TEMP" || head[1] === "TEMPORARY")))) trigger = true;
      k = end;
      continue;
    }
    if (c === ";" && (!trigger || afterEnd)) {
      if (head.length) statements.push(head);
      head = [];
      trigger = afterSemicolon = afterEnd = afterDot = false;
      k++;
      continue;
    }
    afterSemicolon = c === ";";
    afterDot = c === ".";
    afterEnd = false;
    k++;
  }
  if (head.length) statements.push(head);
  return statements;
}

/**
 * The first statement of a SQLite migration that controls the transaction (`begin`, `commit`,
 * `end`, `rollback`, `savepoint`, `release`), lowercase, as `atomicity_problem` refuses one
 * (engine/migrate/src/sql.rs), or none. Each migration runs in a transaction of its own, so one that
 * commits partway would leave what ran before its commit in place, with no row in the history to say
 * so.
 */
export function transactionControl(sql: string): string | undefined {
  return statementTokens(sql).find((t) => CONTROL.has(t[0]!))?.[0]!.toLowerCase();
}

/**
 * Whether a SQLite migration may turn foreign keys on: any `pragma foreign_keys` assignment (its
 * name quoted or not, as itself or `main.foreign_keys`) but a plain off, `0`, `off`, `no` or
 * `false`, quoted or not. The engines read more spellings as on than any list keeps up with (`2`,
 * `0x1`, `1.0`, `full`), so every other value counts.
 */
export function turnsForeignKeysOn(sql: string): boolean {
  const bare = (token: string | undefined) => token?.replace(/^'/, "").toUpperCase();
  return statementTokens(sql).some((t) => {
    const at = t[0] === "PRAGMA" ? t.findIndex((token, i) => i > 0 && i < 3 && bare(token) === "FOREIGN_KEYS") : -1;
    const value = at > 0 ? bare(t[at + 1]) : undefined;
    return value !== undefined && !["0", "OFF", "NO", "FALSE"].includes(value);
  });
}

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

// Postgres's rarer types at run time, `postgresTypes` from "reylo": the readers and writers of its
// row values, ranges and multiranges, geometric types, hstore and money. A build step's module passes them to its define function where its
// statement reads or binds one (src/loader.ts), which installs them (src/values.ts,
// `installPostgresTypes`), so that an app none of whose statements holds one bundles none of this.
import { hstoreText, ParameterError, postgresLiteral, rareDecoder, rowsDecoder, type PostgresTypes } from "./values.ts";
import { EFFECT_SQL, type AnyClient, type EffectSql, type QueryModule } from "./execute.ts";

/** The clients whose session writes a `money` in the C locale's form, which `checkMoney` has seen. */
const moneyLocales = new WeakSet<object>();

/** The text a session writes for a money of 1234.50, in its lc_monetary. */
const MONEY_CHECK = "select 1234.50::numeric::money::text as m";

/**
 * Postgres reads a `money` parameter in the session's lc_monetary, which under another locale can
 * read the same text as another amount without an error (under de_DE, `1234.56` is 123456.00):
 * `.run()` reads a money in the C locale's form and refuses another, and a parameter is refused
 * under a session that does not write 1234.50 as the C locale's `$1,234.50`. A client is checked once, before the first statement that takes
 * one, by the text its session writes for 1234.50; a client whose check fails to run is not held up.
 */
async function checkMoney(c: AnyClient, query: QueryModule<"postgres">, name: string): Promise<void> {
  // A client effect() passes (src/effect.ts) is made for one call, and checks under the client it reads through.
  const effect = (c as Partial<EffectSql>)[EFFECT_SQL];
  const key = effect?.dialect === "postgres" ? effect.key : (c as object);
  if (moneyLocales.has(key)) return;
  let written: unknown;
  try {
    const rows = await query.runtime.run(c, { dialect: "postgres", runtime: query.runtime, drivers: query.drivers, sql: MONEY_CHECK, params: [], text: MONEY_CHECK, order: [], marks: [], columns: [["m", "text"]] }, { text: MONEY_CHECK, values: [], names: [], marks: [] });
    // A client that is none of the drivers' is refused when the statement itself runs.
    if (rows === undefined) return;
    written = (rows[0] as { m?: unknown } | undefined)?.m;
  } catch {
    return;
  }
  if (written === "$1,234.50") return void moneyLocales.add(key);
  throw new ParameterError(`Parameter "${name}" is a Postgres money, which Postgres reads in the session's lc_monetary, and this session writes 1234.50 as ${JSON.stringify(written)}, not the C locale's "$1,234.50", so it could store the parameter as another amount. Set lc_monetary = 'C' on the session, the role or the database, or cast the parameter to numeric in the statement (:${name}::numeric).`);
}

export const postgresTypes: PostgresTypes = { decoder: rareDecoder, rowsDecoder, literal: postgresLiteral, hstoreText, checkMoney };

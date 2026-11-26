# Reylo example: Rolldown

A small HTTP server on `node:http` that Rolldown bundles, with `reylo/rolldown`, into one ES module, `dist/server.mjs`, which runs with plain `node`: no preload, no TypeScript and no `node_modules`. Its queries run on Postgres through postgres.js, and the bundle carries the driver, Reylo's run time and each query's analysis. It is the setup of [Rolldown](https://reylo.dev/docs/typescript#setup-vite) in the docs.

## Run it

```sh
nub run build                 # at the repository root, once: writes dist/, which this example links
cd examples/rolldown
nub install                   # or pnpm install (npm does not take link: dependencies)
createdb reylo_rolldown       # any Postgres database you can drop tables in
cp .env.example .env          # DATABASE_URL=postgres://localhost:5432/reylo_rolldown
nub run setup                 # creates users and posts: "created users and posts"
nub run typecheck             # tsc --noEmit --runExternalCode: no output, exit 0
nub run dev                   # the sources under reylo/register, restarted on every change: "listening on http://localhost:3000"
nub run build                 # the type check, then Rolldown: writes dist/server.mjs (about 213 kB) and its source map
nub run start                 # node dist/server.mjs: "listening on http://localhost:3000"
```

With `dev` or `start` running:

```text
$ curl localhost:3000/posts
[{"id":1,"title":"Hello","body":"first post","author":"ada"},{"id":2,"title":"Again","body":null,"author":"ada"}]
$ curl localhost:3000/users/1
{"id":1,"name":"ada","email":"ada@example.com","role":"admin","created_at":"2026-11-21T20:23:36.786450Z"}
$ curl -w ' %{http_code}\n' localhost:3000/users/99
{"error":"not found"} 404
$ curl -d '{"user_id":2,"title":"From curl"}' localhost:3000/posts
{"id":3,"user_id":2,"title":"From curl","body":null}
```

The bundle needs nothing beside it. Copy it anywhere and run it with the database's URL:

```sh
cp dist/server.mjs /tmp/app/ && cd /tmp/app
DATABASE_URL=postgres://localhost:5432/reylo_rolldown PORT=3000 node server.mjs
```

The `PORT` variable picks the port (3000 by default), and `DATABASE_URL` in the environment wins over `.env`. Running `nub run setup` starts the tables over.

Misspell a column in `queries/get_user.sql` and run `nub run typecheck` again: the error shows at that file's line and column, such as `queries/get_user.sql(1,12): error reylo6: column "nmae" does not exist (42703)`, and `nub run build` stops there.

## How Reylo is wired in

- In `package.json`, `reylo` is a dependency, because the bundle carries its run time (`definePostgresQuery` and the Postgres drivers' adapters, and no other database's); Rolldown and `typescript` 7.1 are dev dependencies, used by the build and the type check only, and `reylo` brings the engine that analyzes each statement for the machine. The `postgres` package is the driver the server runs the queries with; the build and the type check read the schema over the engine's own connection.
- In `rolldown.config.ts`, `plugins: [reylo()]` from `reylo/rolldown` turns each `.sql` import into a module that carries the statement, its parameters' order and types, and the row count Reylo inferred, so the bundle reads no `.sql` file and asks the database nothing before it runs a query. Rolldown compiles the TypeScript itself. The option `platform: "node"` keeps Node's built-in modules out of the bundle, and `codeSplitting: false` writes one file.
- In the `package.json` scripts, `typecheck` is `tsc --noEmit --runExternalCode`, the flag that lets `tsc` run the content mapper. The `build` script runs that type check before `rolldown -c`, because Rolldown strips types without checking them; the plugin itself fails the build on a statement the database refuses, at its line and column. The `dev` script is `node --watch --env-file-if-exists=.env --import reylo/register src/server.ts`: the preload builds each `.sql` module as Node loads it, so development needs no build, and `--watch` restarts on a change to a `.sql` file too. The `start` script is `node --env-file-if-exists=.env dist/server.mjs`, with no preload.
- In `tsconfig.json`, `contentMappers` names `reylo`, whose package lists the extensions it takes: `.sql`, which types each import from the database, and the project file, `.reylo.yaml` or `.reylo.yml`. The `include` option is `src`, `rolldown.config.ts` and `setup.ts`, so `setup.sql`, a script rather than one query, stays out of the type check.
- The `.env` file sets `DATABASE_URL`, which Reylo reads for its analysis (the type check, the editor, the plugin and the preload) and the server reads through `--env-file-if-exists`.
- The `queries/*.sql` files hold one statement per file, with `:name` parameters, imported from `src/server.ts` with a relative path. The query `get_user.sql` resolves to a row or `null`, `list_posts.sql` to an array, and `create_post.sql`, an insert with `returning`, to the row it wrote.
- In `.vscode/settings.json` and `.vscode/extensions.json`, the TypeScript 7 extension is pointed at this project's TypeScript, so the editor shows the queries' types and errors ([Editors](https://reylo.dev/docs/typescript#editors)). In Zed, `.zed/settings.json` does the same.

## In your own project

- Install from npm instead of the `link:` dependencies: `npm install reylo postgres` and `npm install -D rolldown typescript@next @types/node`.

## Notes

- The plugin is Reylo's Vite plugin, which Rolldown runs as it is: `reylo/rolldown` and `reylo/vite` are the same function.
- Reylo's run time loads each database's error codes on demand, through a dynamic import, which Rolldown writes as a chunk of its own beside the entry. With `output.file`, set `codeSplitting: false` to keep the bundle one file; with `output.dir`, leave it on.
- The bundle holds none of Reylo's analysis and no engine. That stays in the build.

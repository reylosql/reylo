# Reylo for VS Code

Syntax errors, inferred types on hover, completion from your schema and go-to-definition in `.sql` files, from [Reylo](https://reylo.dev)'s language server.

The extension runs your project's own `reylo` from `node_modules`, else the one on your `PATH`. Each file reads the database its project names, as Reylo's type check does.

| Setting | |
| --- | --- |
| `reylo.path` | The `reylo` command, in place of the one the extension finds |
| `reylo.dialect` | The dialect of files in a project that names no database |
| `reylo.allowUrlCommand` | Run a `$(command)` in a project's `direct_url` |

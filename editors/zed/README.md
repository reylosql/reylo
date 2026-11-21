# Reylo for Zed

Syntax errors, inferred types on hover, completion from your schema and go-to-definition in `.sql` files, from [Reylo](https://reylo.dev)'s language server. It needs Zed's SQL extension for the language itself.

The extension runs your project's own `reylo` from `node_modules`, else the one on your `PATH`. A path or flags of your own go in Zed's settings:

```json
{
  "lsp": {
    "reylo": { "binary": { "path": "/usr/local/bin/reylo", "arguments": ["lsp", "--dialect", "postgres"] } }
  }
}
```

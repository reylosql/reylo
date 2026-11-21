//! Zed's side of `reylo lsp`: it finds the `reylo` command and starts the language server for SQL files.

use zed_extension_api::{self as zed, settings::LspSettings, LanguageServerId, Result};

struct Reylo;

impl zed::Extension for Reylo {
    fn new() -> Self {
        Reylo
    }

    fn language_server_command(&mut self, _: &LanguageServerId, worktree: &zed::Worktree) -> Result<zed::Command> {
        let binary = LspSettings::for_worktree("reylo", worktree).ok().and_then(|s| s.binary);
        // The project's own reylo first, so the server runs the version the project types with, then one on PATH.
        let local = "node_modules/.bin/reylo";
        let installed = worktree.read_text_file(local).is_ok().then(|| format!("{}/{local}", worktree.root_path()));
        let command = binary.as_ref().and_then(|b| b.path.clone()).or(installed).or_else(|| worktree.which("reylo"));
        let command = command.ok_or("reylo isn't installed in this project or on PATH; set lsp.reylo.binary.path to it")?;
        let args = binary.as_ref().and_then(|b| b.arguments.clone()).unwrap_or_else(|| vec!["lsp".to_string()]);
        let mut env = worktree.shell_env();
        env.extend(binary.and_then(|b| b.env).unwrap_or_default());
        Ok(zed::Command { command, args, env })
    }
}

zed::register_extension!(Reylo);

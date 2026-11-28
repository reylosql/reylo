// reylo's build-time half: the module a build step writes for a `.sql` file and the content mapper,
// with the analysis, the cache in `reylo/.cache/`, the project file and the engine behind them. reylo's
// entry points load it the first time they run (src/tooling-loader.ts), so that the run time a
// project imports never reaches it.
import type { Tooling } from "./tooling-loader.ts";
import { releaseConnections } from "./analyze.ts";
import { sqlModule } from "./loader.ts";
import { mapperHandlers } from "./mapper-server.ts";
import { embeddedModule, projectMigrations } from "./embedded-module.ts";
import { migrate } from "./migrate-deploy.ts";
import { analyze } from "./analyze-api.ts";

/** The project's migrations, from the working directory, as `migrate()` deploys from it; a project with none there is refused, since the app may have started in another directory. */
async function migrations(): Promise<{ list?: { name: string; sql: string }[]; error?: string }> {
  const { list, error } = await projectMigrations(process.cwd());
  if (list === undefined && error === undefined) return { error: `no migrations directory in the project at ${process.cwd()}: start the app in the project's directory, with its migrations beside it` };
  return error !== undefined ? { error } : { list: list ?? [] };
}

export default { analyze, migrate, migrations, sqlModule, embeddedModule, releaseConnections, mapperHandlers } satisfies Tooling;

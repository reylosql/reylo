'use client';

import { Variant, VariantTabs } from 'docs-kit/components';
import { Tabs, TabsContent, TabsList, TabsTrigger } from 'fumadocs-ui/components/ui/tabs';
import { useState, type ReactNode } from 'react';
import { DIALECTS, type DialectName } from '@/lib/dialects';
import { prepaintMarks, usePrepaintReady } from '../docs-kit/src/components/prepaint.tsx';
import { Check, Chevron, DatabaseLogo, useDatabaseMenu } from './database-menu';

/*
 * One choice of dialect for the whole page: docs-kit's variant tabs with the group `dialect`, the
 * same group as Fumadocs' code tabs marked `tab-group="dialect"`, so a choice made in any of them,
 * a code-tab strip included, switches all of them and holds across reloads.
 */

/**
 * `<DialectTabs>` shows a strip Postgres | MySQL | SQLite | ClickHouse above the panel of the chosen dialect.
 * `<DialectTabs follow>` shows no strip, only the chosen dialect's panel: it follows the choice
 * made elsewhere on the page, as the TypeScript under an example's SQL does (lib/remark-mapper.ts
 * writes those).
 */
export function DialectTabs({ follow = false, dialects, children }: { follow?: boolean; dialects?: string; children: ReactNode }) {
  // `dialects` names the strip's, comma-separated, where a panel exists for fewer than all of them.
  const values = dialects ? DIALECTS.filter((d) => dialects.split(',').includes(d)) : DIALECTS;
  return (
    <VariantTabs group="dialect" values={values} follow={follow}>
      {children}
    </VariantTabs>
  );
}

/** One dialect's panel inside `<DialectTabs>`, or prose several dialects share: `value="Postgres,MySQL,SQLite"` is that panel for each of them. */
export function Dialect({ value, children }: { value: DialectName | (string & {}); children: ReactNode }) {
  return value.split(',').map((name) => (
    <Variant key={name} value={name.trim()}>
      {children}
    </Variant>
  ));
}

/**
 * The database switch (./database-menu.tsx) on the Query line of an inference card, and at the right
 * end of a code-tab strip: lib/remark-mapper.ts puts one, marked `aside`, in each example whose
 * statement differs per dialect, beside the tabs `main.ts | <name>.sql`, whose panels follow the choice.
 */
export function DialectSwitch({ dialects }: { aside?: boolean; dialects?: string }) {
  // `dialects` names the group's, comma-separated, when it shows fewer than all of them.
  const values = dialects ? DIALECTS.filter((d) => dialects.split(',').includes(d)) : DIALECTS;
  // One database is no choice: a light gray label says which, as "Postgres only".
  if (values.length === 1) {
    return (
      <span className="reylo-dialect-only">
        <DatabaseLogo name={values[0]} />
        {values[0]} only
      </span>
    );
  }
  return <DialectMenu values={values} />;
}

/**
 * The switch on Fumadocs' tabs, with the group `dialect`, as docs-kit's `<VariantSwitch>` is built,
 * so a pick in its menu switches every dialect group on the page and holds across reloads. The
 * tabs are its value, held here too (Fumadocs' tabs take a `value` and call `onValueChange` for
 * every change of the group) to name the switch for a screen reader. The root holds a panel per
 * database, the logo and the name the switch shows, so the head script shows the saved one from the
 * first paint as it shows any group's saved panel (docs-kit/src/components/prepaint.tsx); the menu's
 * entries are the group's triggers. A choice the switch doesn't offer (ClickHouse, on a card of
 * Postgres, MySQL and SQLite) shows its first database, as the card's panels do. The root keeps
 * docs-kit's `dk-variant-switch` class, by which app/global.css lets a code-tab strip that holds a
 * switch wrap rather than scroll.
 */
function DialectMenu({ values }: { values: readonly DialectName[] }) {
  const ref = usePrepaintReady<HTMLDivElement>();
  const [value, setValue] = useState<string>(values[0]);
  const { open, shut, trigger, menu } = useDatabaseMenu();
  const shown = values.find((v) => v === value) ?? values[0];
  return (
    <Tabs ref={ref} {...prepaintMarks('dialect', true)} groupId="dialect" persist value={value} onValueChange={setValue} orientation="vertical" className="dk-variant-switch reylo-db-switch" data-open={open ? '' : undefined}>
      {values.map((v) => (
        <TabsContent key={v} value={v} forceMount tabIndex={-1} aria-hidden className="reylo-db-panel">
          <span className="reylo-db-value">
            <DatabaseLogo name={v} />
            {v}
          </span>
        </TabsContent>
      ))}
      <Chevron />
      <button type="button" className="reylo-db-hit" aria-label={`Database: ${shown}`} {...trigger} />
      {menu(
        <TabsList aria-label="Database" className="reylo-db-list">
          {values.map((v) => (
            <TabsTrigger key={v} value={v} onClick={shut}>
              <DatabaseLogo name={v} />
              <span>{v}</span>
              <Check />
            </TabsTrigger>
          ))}
        </TabsList>,
      )}
    </Tabs>
  );
}

import type { DialectName } from '@/lib/dialects';
import { DATABASES, type Logo } from './host-logos';

/*
 * Each dialect's logo, which the database switch draws beside its name (./database-menu.tsx): the
 * database logos of ./host-logos.ts, and ClickHouse's, which `DATABASES` does not list, from the same
 * Simple Icons 16.38.0 (CC0).
 */

const CLICKHOUSE: Logo = {
  name: 'ClickHouse',
  viewBox: '0 0 24 24',
  paths: ['M21.333 10H24v4h-2.667ZM16 1.335h2.667v21.33H16Zm-5.333 0h2.666v21.33h-2.666ZM0 22.665V1.335h2.667v21.33zm5.333-21.33H8v21.33H5.333Z'],
};

export const DIALECT_LOGOS = Object.fromEntries([...DATABASES, CLICKHOUSE].map((logo) => [logo.name, logo])) as Record<DialectName, Logo>;

/**
 * Event-flow fixture loader.
 *
 * P2 owns the recorded fixtures (`fixtures/events/*.ndjson`) but has not landed
 * them yet, so P3 ships a **constructed** NDJSON session under
 * `apps/desktop/fixtures/events/` to prove the replay path end to end.  The
 * constructed files say so in their own header comment and in the report.
 *
 * When the real recordings appear, drop them next to these (or point
 * `PRINCESSIDE_EVENT_FIXTURES` at them — the engine side owns path resolution)
 * and the same loader, state machine and snapshot test pick them up unchanged.
 */

import { parseNdjson, type NdjsonParseResult } from '../contract/parse.js';

const bundled = import.meta.glob('../../fixtures/events/*.ndjson', {
  query: '?raw',
  import: 'default',
  eager: true,
}) as Record<string, string>;

export interface FixtureLoad extends NdjsonParseResult {
  name: string;
  source: string;
  /** true when the file is a P3-constructed demo rather than a P2 recording. */
  constructed: boolean;
}

export function listBundledFixtures(): string[] {
  return Object.keys(bundled)
    .map((p) => p.split('/').pop() ?? p)
    .sort();
}

export function loadBundledFixture(name: string): FixtureLoad {
  const key = Object.keys(bundled).find((p) => p.endsWith(`/${name}`) || p.endsWith(name));
  if (!key) throw new Error(`fixture not bundled: ${name} (have: ${listBundledFixtures().join(', ')})`);
  const text = bundled[key] ?? '';
  const parsed = parseNdjson(text, name);
  return {
    name,
    source: key,
    constructed: text.includes('CONSTRUCTED') || text.includes('P3-constructed'),
    ...parsed,
  };
}

/** Load every bundled fixture, sorted by name. */
export function loadAllBundledFixtures(): FixtureLoad[] {
  return listBundledFixtures().map(loadBundledFixture);
}

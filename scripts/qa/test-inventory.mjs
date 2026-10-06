// Static source inventory; never loads or executes test modules.
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = fileURLToPath(new URL('../../', import.meta.url));
const rows = [];
function visit(directory, extension, pattern) {
  for (const entry of readdirSync(path.join(root, directory), { withFileTypes: true })) {
    const file = `${directory}/${entry.name}`;
    if (entry.isDirectory()) { visit(file, extension, pattern); continue; }
    if (!entry.isFile() || !file.endsWith(extension)) continue;
    if (extension === '.mjs' && !file.endsWith('.test.mjs')) continue;
    const bytes = readFileSync(path.join(root, file));
    const names = [...bytes.toString('utf8').matchAll(pattern)].map(match => match[1] ?? match[2] ?? match[3]);
    if (!names.length) continue;
    rows.push({ file, sha256: createHash('sha256').update(bytes).digest('hex'), declared_test_names: names, count: names.length });
  }
}
const rust = /#\[test\]\s*(?:#\[[^\n]*\]\s*)*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)/g;
visit('core/src', '.rs', rust);
visit('core/tests', '.rs', rust);
visit('windows/src', '.rs', rust);
visit('mac/Tests', '.swift', /^\s*func\s+(test\w+)\s*\(/gm);
visit('scripts', '.mjs', /^\s*test\((?:"([^"]+)"|'([^']+)'|`([^`]+)`)/gm);
rows.sort((a, b) => a.file.localeCompare(b.file));
const byArea = {};
for (const row of rows) {
  const area = row.file.split('/')[0];
  byArea[area] = (byArea[area] ?? 0) + row.count;
}
const assertionScripts = ['dashboard/app.test.mjs'].filter(file => statSync(path.join(root, file), { throwIfNoEntry: false })?.isFile());
process.stdout.write(JSON.stringify({
  schema_version: 1,
  counting_method: 'Source test declarations; cfg exclusions & parameterized registrations may change runtime case counts. Standalone assertion scripts & installed journeys are separate.',
  total_declared: rows.reduce((sum, row) => sum + row.count, 0),
  by_area: byArea,
  source_working_tree_inventory: rows,
  standalone_assertion_scripts: assertionScripts,
  installed_journey_runners: ['scripts/qa/mac-installed-journey.mjs', 'scripts/qa/mac-storage-installed-journey.mjs']
    .filter(file => statSync(path.join(root, file), { throwIfNoEntry: false })?.isFile()),
}, null, 2) + '\n');

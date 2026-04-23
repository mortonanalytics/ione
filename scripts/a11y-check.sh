#!/usr/bin/env bash
#
# Runs axe-core via puppeteer against every route of a running IONe instance.
# Exits non-zero on any "critical" or "serious" violation.
#
# Prereqs:
#   - ione is running on http://localhost:3000 (cargo run --release)
#   - Postgres + MinIO up (docker compose up -d postgres minio)
#   - IONE_SEED_DEMO=1 was set on the ione process
#   - Node.js 20+ with @axe-core/puppeteer installed locally
#
# Usage:
#   scripts/a11y-check.sh
#   BASE_URL=http://localhost:3001 scripts/a11y-check.sh
#
# This file should be executable: chmod +x scripts/a11y-check.sh

set -euo pipefail

BASE_URL="${BASE_URL:-http://localhost:3000}"

# Check that @axe-core/puppeteer is available. Install locally if not.
if ! node -e "require.resolve('@axe-core/puppeteer')" 2>/dev/null; then
    echo "Installing @axe-core/puppeteer + puppeteer locally..."
    npm install --no-save @axe-core/puppeteer puppeteer
fi

cat > /tmp/ione-axe-runner.mjs <<'NODE'
import { AxePuppeteer } from '@axe-core/puppeteer';
import puppeteer from 'puppeteer';

const baseUrl = process.env.BASE_URL || 'http://localhost:3000';
const routes = [
  '/',           // main SPA: Chat tab
  '/#/chat',
  '/#/connectors',
  '/#/signals',
  '/#/survivors',
  '/#/approvals',
];

const browser = await puppeteer.launch({ headless: 'new' });
let totalViolations = 0;
let highViolations = 0;

for (const route of routes) {
  const page = await browser.newPage();
  await page.setViewport({ width: 1280, height: 900 });
  await page.goto(baseUrl + route, { waitUntil: 'networkidle2', timeout: 20000 });
  const results = await new AxePuppeteer(page)
    .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'])
    .analyze();
  if (results.violations.length > 0) {
    console.log(`\n### ${route}`);
    for (const v of results.violations) {
      console.log(`  [${v.impact}] ${v.id}: ${v.description}`);
      for (const n of v.nodes.slice(0, 3)) {
        console.log(`    ${n.target.join(' ')}`);
      }
      totalViolations++;
      if (v.impact === 'critical' || v.impact === 'serious') highViolations++;
    }
  } else {
    console.log(`${route}: OK`);
  }
  await page.close();
}

await browser.close();
console.log(`\nTotal violations: ${totalViolations} (serious/critical: ${highViolations})`);
process.exit(highViolations > 0 ? 1 : 0);
NODE

node /tmp/ione-axe-runner.mjs

import { test, expect } from '@playwright/test';
const ws = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa';
const other = 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb';
const dataset = 'cccccccc-cccc-cccc-cccc-cccccccccccc';
const version = 'dddddddd-dddd-dddd-dddd-dddddddddddd';
const origin = { deployment_id: other, tenant_id: other, workspace_id: other, actor_id: 'remote-user', service_account_id: 'remote-account' };
const grant = { grant_id: 'eeeeeeee-eeee-eeee-eeee-eeeeeeeeeeee', origin, expires_at: '2099-01-01T00:00:00Z', revoked_at: null as string | null };

test.beforeEach(async ({ page }) => {
  await page.route('**/api/**', route => route.fulfill({ json: { items: [] } }));
  await page.route('**/api/v1/me', route => route.fulfill({ json: { user: { email: 'default@localhost', displayName: 'Default' } } }));
  await page.route('**/api/v1/workspaces', route => route.fulfill({ json: { items: [{ id: ws, name: 'Operations', lifecycle: 'continuous' }, { id: other, name: 'Other', lifecycle: 'continuous' }] } }));
  await page.route('**/relay/destinations', route => route.fulfill({ json: { destinations: [], canManage: true, canPublish: true } }));
  await page.route('**/relay/datasets', route => route.fulfill({ json: { canDelegate: true, datasets: [{ dataset_id: dataset, version_id: version, dataset_name: 'Full report', row_count: 200, column_count: 1, requires_source_access: false, lineage: [{ connection_id: other }], expires_at: '2099-01-01T00:00:00Z' }] } }));
});

test('sharing shows a credential once, lists and revokes grants, then clears across workspaces', async ({ page }) => {
  let created = false;
  let revoked = false;
  await page.route('**/delegations', route => {
    if (route.request().method() === 'POST') {
      expect(route.request().postDataJSON()).toEqual({ origin, ttlSeconds: 3600 });
      created = true;
      return route.fulfill({ json: { grant, token: 'copy-once-secret-canary' } });
    }
    return route.fulfill({ json: { grants: created ? [{ ...grant, revoked_at: revoked ? '2026-01-01T00:00:00Z' : null }] : [] } });
  });
  await page.route(`**/delegations/${grant.grant_id}`, route => { revoked = true; return route.fulfill({ status: 204 }); });
  await page.goto('/');
  await page.locator('#tab-data').click();
  await expect(page.locator('#data-dataset-list')).toContainText('200 complete rows');
  await page.getByRole('button', { name: 'Share', exact: true }).click();
  for (const [key, value] of Object.entries(origin)) await page.locator(`#data-delegation-form [name=${key}]`).fill(value);
  await page.getByRole('button', { name: 'Create read grant' }).click();
  await expect(page.locator('#data-delegation-token')).toHaveValue('copy-once-secret-canary');
  expect(await page.evaluate(() => JSON.stringify({ ...localStorage, ...sessionStorage }))).not.toContain('copy-once-secret-canary');
  await page.getByRole('button', { name: 'Revoke', exact: true }).click();
  await expect(page.locator('#data-delegation-list')).toContainText('(revoked)');
  await expect(page.locator('#data-delegation-token')).toHaveValue('');
  await page.getByRole('button', { name: 'Create read grant' }).click();
  await expect(page.locator('#data-delegation-token')).toHaveValue('copy-once-secret-canary');
  await page.evaluate((id) => (window as any).setActiveWorkspace({ id, name: 'Other', lifecycle: 'continuous' }), other);
  await expect(page.locator('#data-delegation')).toBeHidden();
  await expect(page.locator('#data-delegation-token')).toHaveValue('');
  await expect(page.locator('#data-delegation-form [name=actor_id]')).toHaveValue('');
});

test('redistribution is explicit for new storage and late credentials cannot enter another workspace', async ({ page }) => {
  let storage: any;
  await page.route('**/relay/destinations', route => {
    if (route.request().method() === 'POST') { storage = route.request().postDataJSON(); return route.fulfill({ json: { id: dataset } }); }
    return route.fulfill({ json: { destinations: [], canManage: true, canPublish: true } });
  });
  let release: (() => void) | undefined;
  await page.route('**/delegations', async route => {
    if (route.request().method() === 'POST') {
      await new Promise<void>(resolve => { release = resolve; });
      return route.fulfill({ json: { grant, token: 'late-secret-canary' } });
    }
    return route.fulfill({ json: { grants: [] } });
  });
  await page.goto('/'); await page.locator('#tab-data').click();
  await page.locator('#data-destination-admin summary').click();
  await expect(page.locator('[name=allowRedistribution]')).not.toBeChecked();
  await page.locator('#data-destination-form [name=name]').fill('Exportable');
  await page.locator('[name=allowRedistribution]').check();
  await page.getByRole('button', { name: 'Create storage', exact: true }).click();
  await expect.poll(() => storage?.allowRedistribution).toBe(true);
  expect(storage.policy.allowRedistribution).toBeUndefined();
  await page.getByRole('button', { name: 'Share', exact: true }).click();
  for (const [key, value] of Object.entries(origin)) await page.locator(`#data-delegation-form [name=${key}]`).fill(value);
  await page.getByRole('button', { name: 'Create read grant' }).click();
  await expect.poll(() => !!release).toBe(true);
  await page.evaluate((id) => (window as any).setActiveWorkspace({ id, name: 'Other', lifecycle: 'continuous' }), other);
  release!();
  await expect(page.locator('#data-delegation-token')).toHaveValue('');
  await expect(page.locator('#data-delegation')).toBeHidden();
  await expect(page.locator('[name=allowRedistribution]')).not.toBeChecked();
});

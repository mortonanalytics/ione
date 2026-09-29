import { test, expect } from '@playwright/test';
const ws='aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa';const peer='bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb';const grant='cccccccc-cccc-cccc-cccc-cccccccccccc';const mapping='dddddddd-dddd-dddd-dddd-dddddddddddd';
const origin={deployment_id:peer,tenant_id:peer,workspace_id:ws,actor_id:'actual-actor',service_account_id:'actual-account'};
test.beforeEach(async({page})=>{
 await page.route('**/api/**',r=>r.fulfill({json:{items:[]}}));
 await page.route('**/api/v1/me',r=>r.fulfill({json:{user:{email:'default@localhost',displayName:'Default'}}}));
 await page.route('**/api/v1/workspaces',r=>r.fulfill({json:{items:[{id:ws,name:'Operations',lifecycle:'continuous'}]}}));
 await page.route('**/relay/dataset-origin',r=>r.fulfill({json:origin}));
 await page.route('**/relay/peer-datasets',r=>r.fulfill({json:{peers:[{id:peer,name:'Owner',bindingId:grant}]}}));
 await page.route('**/relay/sources',r=>r.fulfill({json:{sources:[]}}));
});
test('actual identity and inspected grant import select a normal query source without persisting credentials',async({page})=>{
 let imported=false;
 await page.route('**/relay/peer-datasets/descriptor',r=>{expect(r.request().postDataJSON()).toEqual({peerId:peer,grantId:grant,token:'opaque-secret-canary'});return r.fulfill({json:{row_count:200,columns:['amount'],version_id:grant,expires_at:'2099-01-01T00:00:00Z'}})});
 await page.route('**/relay/peer-datasets/import',r=>{expect(r.request().postDataJSON()).toEqual({peerId:peer,grantId:grant,token:'opaque-secret-canary',name:'Report',alias:'report'});imported=true;return r.fulfill({json:{id:mapping}})});
 await page.route('**/relay/sources',r=>r.fulfill({json:{sources:imported?[{mappingId:mapping,displayName:'Report',alias:'report',entities:[]}]:[]}}));
 await page.goto('/');await page.locator('#tab-data').click();await page.locator('#data-peer-dataset-admin summary').click();
 await expect(page.locator('#data-origin-identity')).toHaveValue(JSON.stringify(origin,null,2));
 const form=page.locator('#data-peer-dataset-form');await form.locator('[name=peerId]').selectOption(peer);await form.locator('[name=grantId]').fill(grant);await form.locator('[name=token]').fill('opaque-secret-canary');await form.locator('[name=name]').fill('Report');await form.locator('[name=alias]').fill('report');await form.getByRole('button',{name:'Inspect granted dataset'}).click();
 await expect(page.locator('#data-peer-dataset-status')).toContainText('200 rows, 1 columns');await expect(form.locator('[name=token]')).toHaveValue('');
 await page.locator('#data-peer-dataset-import').click();await expect(page.locator('#data-source-list input')).toBeChecked();await expect(page.locator('#data-peer-dataset-status')).toContainText('Dataset source ready');
 expect(await page.evaluate(()=>JSON.stringify({...localStorage,...sessionStorage}))).not.toContain('opaque-secret-canary');await expect(form.locator('[name=token]')).toHaveValue('');
});
test('workspace switch cancels descriptor work and clears origin and credential',async({page})=>{
 let release:(()=>void)|undefined;
 await page.route('**/relay/peer-datasets/descriptor',async r=>{await new Promise<void>(resolve=>{release=resolve});try{await r.fulfill({json:{row_count:200,columns:['amount'],version_id:grant,expires_at:'2099-01-01T00:00:00Z'}})}catch{}});
 await page.goto('/');await page.locator('#tab-data').click();await page.locator('#data-peer-dataset-admin summary').click();
 const form=page.locator('#data-peer-dataset-form');await form.locator('[name=peerId]').selectOption(peer);await form.locator('[name=grantId]').fill(grant);await form.locator('[name=token]').fill('late-secret-canary');await form.locator('[name=name]').fill('Report');await form.locator('[name=alias]').fill('report');await form.getByRole('button',{name:'Inspect granted dataset'}).click();await expect.poll(()=>!!release).toBe(true);
 await page.evaluate(id=>(window as any).setActiveWorkspace({id,name:'Other',lifecycle:'continuous'}),peer);release!();await expect(form.locator('[name=token]')).toHaveValue('');await expect(page.locator('#data-peer-dataset-import')).toBeDisabled();await expect(page.locator('#data-origin-identity')).not.toHaveValue(JSON.stringify(origin,null,2));
});

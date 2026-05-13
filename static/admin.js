const form = document.getElementById('issuer-form');
const preset = document.getElementById('preset');
const tenant = document.getElementById('tenant-id');
const issuer = document.getElementById('issuer-url');
const audience = document.getElementById('audience');
const jwks = document.getElementById('jwks-uri');
const displayName = document.getElementById('display-name');
const secret = document.getElementById('client-secret');
const list = document.getElementById('issuer-list');
const statusEl = document.getElementById('admin-status');

function applyPreset() {
  if (preset.value === 'entra' && tenant.value) {
    issuer.value = `https://login.microsoftonline.com/${tenant.value}/v2.0`;
    jwks.value = `https://login.microsoftonline.com/${tenant.value}/discovery/v2.0/keys`;
  } else if (preset.value === 'login-gov') {
    issuer.value = 'https://secure.login.gov';
    jwks.value = 'https://secure.login.gov/api/openid_connect/certs';
  }
}

preset.addEventListener('change', applyPreset);
tenant.addEventListener('input', applyPreset);

async function load() {
  const resp = await fetch('/api/v1/admin/trust-issuers');
  if (!resp.ok) {
    list.textContent = 'Admin session required.';
    return;
  }
  const rows = await resp.json();
  list.innerHTML = rows.map((row) => `
    <article class="item">
      <strong>${escapeHtml(row.displayName || row.issuerUrl)}</strong>
      <span>${escapeHtml(row.audience)}</span>
      <button data-id="${row.id}" type="button">Delete</button>
    </article>`).join('');
}

form.addEventListener('submit', async (event) => {
  event.preventDefault();
  applyPreset();
  const body = {
    idpType: 'oidc',
    issuerUrl: issuer.value,
    audience: audience.value,
    jwksUri: jwks.value,
    displayName: displayName.value || null,
    clientSecret: secret.value || null,
    maxCocLevel: 100,
    claimMapping: { email_claim: 'preferred_username', role_claim: 'roles', coc_level_claim: 'ione_coc_level' },
  };
  const resp = await fetch('/api/v1/admin/trust-issuers', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  statusEl.textContent = resp.ok ? 'Saved.' : `Failed: ${await resp.text()}`;
  await load();
});

list.addEventListener('click', async (event) => {
  const id = event.target?.dataset?.id;
  if (!id) return;
  await fetch(`/api/v1/admin/trust-issuers/${id}`, { method: 'DELETE' });
  await load();
});

function escapeHtml(value) {
  return String(value).replace(/[&<>"']/g, (ch) => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[ch]));
}

load();

const form = document.getElementById('connect-form');
const list = document.getElementById('connections');

async function load() {
  const resp = await fetch('/api/v1/broker/connections');
  if (!resp.ok) {
    list.textContent = 'Sign in to manage connections.';
    return;
  }
  const rows = await resp.json();
  list.innerHTML = rows.map((row) => `
    <article class="item">
      <strong>${row.provider}</strong>
      <span>${row.label || 'default'}</span>
      <button data-id="${row.id}" type="button">Disconnect</button>
    </article>`).join('');
}

form.addEventListener('submit', async (event) => {
  event.preventDefault();
  const resp = await fetch('/api/v1/broker/connections', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ provider: provider.value, label: label.value || '' }),
  });
  const data = await resp.json();
  if (data.authorizeUrl) window.location.href = data.authorizeUrl;
});

list.addEventListener('click', async (event) => {
  const id = event.target?.dataset?.id;
  if (!id) return;
  await fetch(`/api/v1/broker/connections/${id}`, { method: 'DELETE' });
  await load();
});

load();

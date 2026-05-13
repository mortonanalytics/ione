const issuers = document.getElementById('issuers');

async function loadIssuers() {
  const rows = await (await fetch('/auth/issuers')).json();
  if (rows.length === 1) {
    window.location.href = `/auth/login?issuer=${encodeURIComponent(rows[0].id)}`;
    return;
  }
  issuers.innerHTML = rows.map((row) => `
    <button type="button" data-id="${row.id}" data-name="${row.displayName}">${row.displayName}</button>
  `).join('');
}

issuers.addEventListener('click', (event) => {
  const name = event.target?.dataset?.name;
  const id = event.target?.dataset?.id;
  if (id) window.location.href = `/auth/login?issuer=${encodeURIComponent(id)}`;
});

loadIssuers();

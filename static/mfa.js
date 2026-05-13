const code = document.getElementById('code');
const output = document.getElementById('output');
const qr = document.getElementById('qr');
const secret = document.getElementById('secret');

document.getElementById('enroll-btn').addEventListener('click', async () => {
  const resp = await fetch('/api/v1/me/mfa/totp/enroll', { method: 'POST' });
  const data = await resp.json();
  qr.textContent = data.qrSvg || data.otpauthUri;
  secret.textContent = data.secretB32;
});

document.getElementById('confirm-btn').addEventListener('click', () => postCode('/api/v1/me/mfa/totp/confirm'));
document.getElementById('challenge-btn').addEventListener('click', () => postCode('/api/v1/me/mfa/challenge'));
document.getElementById('recovery-btn').addEventListener('click', async () => {
  const resp = await fetch('/api/v1/me/mfa/recovery-codes');
  output.textContent = await resp.text();
});

async function postCode(path) {
  const resp = await fetch(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ code: code.value }),
  });
  output.textContent = resp.ok ? 'Verified.' : await resp.text();
}

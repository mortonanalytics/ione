const form = document.getElementById('chat-form');
const promptEl = document.getElementById('prompt');
const transcript = document.getElementById('transcript');
const statusEl = document.getElementById('status');
const sendBtn = document.getElementById('send-btn');

function appendMessage(role, text) {
  const div = document.createElement('div');
  div.className = 'message ' + role;
  div.textContent = (role === 'user' ? 'You: ' : 'Model: ') + text;
  transcript.appendChild(div);
  transcript.scrollTop = transcript.scrollHeight;
}

form.addEventListener('submit', async (e) => {
  e.preventDefault();
  const prompt = promptEl.value.trim();
  if (!prompt) return;

  appendMessage('user', prompt);
  promptEl.value = '';
  sendBtn.disabled = true;
  statusEl.textContent = 'Loading…';

  try {
    const resp = await fetch('/api/v1/chat', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ prompt }),
    });

    if (!resp.ok) {
      const err = await resp.json().catch(() => ({ error: resp.statusText }));
      statusEl.textContent = 'Error: ' + (err.error || resp.statusText);
      return;
    }

    const data = await resp.json();
    appendMessage('assistant', data.reply);
    statusEl.textContent = '';
  } catch (err) {
    statusEl.textContent = 'Request failed: ' + err.message;
  } finally {
    sendBtn.disabled = false;
  }
});

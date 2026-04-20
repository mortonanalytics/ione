/* ── DOM refs ── */
const convList   = document.getElementById('conv-list');
const newChatBtn = document.getElementById('new-chat-btn');
const form       = document.getElementById('chat-form');
const promptEl   = document.getElementById('prompt');
const transcript = document.getElementById('transcript');
const statusEl   = document.getElementById('status');
const sendBtn    = document.getElementById('send-btn');

/* ── State ── */
let activeConvId = null;   // UUID string or null
let conversations = [];    // Conversation[] newest-first

/* ── API helpers ── */

async function apiFetch(path, options) {
  const resp = await fetch(path, options);
  if (!resp.ok) {
    const body = await resp.json().catch(() => ({}));
    throw new ApiError(body.error || resp.statusText, resp.status);
  }
  return resp.json();
}

class ApiError extends Error {
  constructor(message, status) {
    super(message);
    this.status = status;
  }
}

function listConversations() {
  return apiFetch('/api/v1/conversations');
}

function createConversation(title) {
  return apiFetch('/api/v1/conversations', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ title }),
  });
}

function getConversation(id) {
  return apiFetch('/api/v1/conversations/' + id);
}

function postMessage(convId, content) {
  return apiFetch('/api/v1/conversations/' + convId + '/messages', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ content }),
  });
}

/* ── Render helpers ── */

function formatDate(iso) {
  const d = new Date(iso);
  return d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

function appendMessage(role, text) {
  const div = document.createElement('div');
  div.className = 'message ' + role;
  div.textContent = (role === 'user' ? 'You: ' : 'Model: ') + text;
  transcript.appendChild(div);
  transcript.scrollTop = transcript.scrollHeight;
}

function clearTranscript() {
  transcript.innerHTML = '';
}

function setStatus(msg) {
  statusEl.textContent = msg;
}

function clearStatus() {
  statusEl.textContent = '';
}

/* ── Sidebar rendering ── */

function renderSidebar() {
  convList.innerHTML = '';
  conversations.forEach((conv) => {
    const li = buildConvItem(conv);
    convList.appendChild(li);
  });
}

function buildConvItem(conv) {
  const li = document.createElement('li');
  li.className = 'conv-item' + (conv.id === activeConvId ? ' active' : '');
  li.setAttribute('role', 'listitem');
  li.setAttribute('tabindex', '0');
  li.dataset.id = conv.id;

  const titleDiv = document.createElement('div');
  titleDiv.className = 'conv-item-title';
  titleDiv.textContent = conv.title;

  const dateDiv = document.createElement('div');
  dateDiv.className = 'conv-item-date';
  dateDiv.textContent = formatDate(conv.createdAt);

  li.appendChild(titleDiv);
  li.appendChild(dateDiv);

  li.addEventListener('click', () => loadConversation(conv.id));
  li.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      loadConversation(conv.id);
    }
  });

  return li;
}

function setActiveSidebarItem(id) {
  convList.querySelectorAll('.conv-item').forEach((el) => {
    el.classList.toggle('active', el.dataset.id === id);
  });
}

function prependConversation(conv) {
  conversations.unshift(conv);
  const li = buildConvItem(conv);
  convList.insertBefore(li, convList.firstChild);
}

function updateSidebarTitle(id, title) {
  const item = conversations.find((c) => c.id === id);
  if (item) item.title = title;
  const li = convList.querySelector('[data-id="' + id + '"]');
  if (li) {
    const titleDiv = li.querySelector('.conv-item-title');
    if (titleDiv) titleDiv.textContent = title;
  }
}

/* ── Load conversation ── */

async function loadConversation(id) {
  clearStatus();
  clearTranscript();
  activeConvId = id;
  setActiveSidebarItem(id);

  try {
    const data = await getConversation(id);
    // Response shape: { conversation, messages: Message[] }
    const msgs = data.messages || [];
    msgs.forEach((msg) => appendMessage(msg.role, msg.content));
  } catch (err) {
    setStatus('Error loading conversation: ' + err.message);
  }
}

/* ── New chat ── */

async function handleNewChat() {
  clearStatus();
  clearTranscript();
  activeConvId = null;
  setActiveSidebarItem(null);

  try {
    const conv = await createConversation('New chat');
    activeConvId = conv.id;
    prependConversation(conv);
    setActiveSidebarItem(conv.id);
  } catch (err) {
    setStatus('Error creating conversation: ' + err.message);
  }
}

newChatBtn.addEventListener('click', handleNewChat);

/* ── Send message ── */

form.addEventListener('submit', async (e) => {
  e.preventDefault();
  const content = promptEl.value.trim();
  if (!content) return;

  clearStatus();

  // Ensure we have an active conversation.
  if (!activeConvId) {
    const title = content.slice(0, 40) || 'New chat';
    try {
      const conv = await createConversation(title);
      activeConvId = conv.id;
      prependConversation(conv);
      setActiveSidebarItem(conv.id);
    } catch (err) {
      setStatus('Error creating conversation: ' + err.message);
      return;
    }
  }

  const convId = activeConvId;

  // Optimistically append user message.
  appendMessage('user', content);
  promptEl.value = '';
  sendBtn.disabled = true;
  setStatus('Loading…');

  // After first message in a new conversation, update title if still default.
  const conv = conversations.find((c) => c.id === convId);
  const isDefaultTitle = conv && conv.title === 'New chat';
  const derivedTitle = content.slice(0, 40);

  try {
    const assistantMsg = await postMessage(convId, content);
    // assistantMsg is a Message (assistant reply)
    appendMessage(assistantMsg.role, assistantMsg.content);

    if (isDefaultTitle && derivedTitle) {
      updateSidebarTitle(convId, derivedTitle);
    }

    clearStatus();
  } catch (err) {
    setStatus('Error: ' + err.message);
    // User message stays in transcript so they can retry.
  } finally {
    sendBtn.disabled = false;
  }
});

/* ── Keyboard: shift+enter inserts newline, enter submits ── */

promptEl.addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault();
    form.dispatchEvent(new Event('submit', { cancelable: true, bubbles: true }));
  }
});

/* ── Init: load conversation list ── */

(async function init() {
  try {
    const data = await listConversations();
    // Response shape: { items: Conversation[] }
    conversations = data.items || [];
    renderSidebar();
  } catch (err) {
    setStatus('Error loading conversations: ' + err.message);
  }
})();

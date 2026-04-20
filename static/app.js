/* ── DOM refs ── */
const convList             = document.getElementById('conv-list');
const newChatBtn           = document.getElementById('new-chat-btn');
const newWorkspaceBtn      = document.getElementById('new-workspace-btn');
const form                 = document.getElementById('chat-form');
const promptEl             = document.getElementById('prompt');
const transcript           = document.getElementById('transcript');
const statusEl             = document.getElementById('status');
const sendBtn              = document.getElementById('send-btn');
const workspaceTrigger     = document.getElementById('workspace-trigger');
const workspaceNameEl      = document.getElementById('workspace-name');
const workspaceDomainEl    = document.getElementById('workspace-domain');
const workspaceMenu        = document.getElementById('workspace-menu');
const workspaceClosedNotice = document.getElementById('workspace-closed-notice');
const newWorkspaceDialog   = document.getElementById('new-workspace-dialog');
const newWorkspaceForm     = document.getElementById('new-workspace-form');
const nwNameInput          = document.getElementById('nw-name');
const nwDomainInput        = document.getElementById('nw-domain');
const nwLifecycleSelect    = document.getElementById('nw-lifecycle');
const nwErrorEl            = document.getElementById('nw-error');
const nwCancelBtn          = document.getElementById('nw-cancel-btn');
const nwSubmitBtn          = document.getElementById('nw-submit-btn');

/* ── State ── */
let activeConvId   = null;   // UUID string or null
let conversations  = [];     // Conversation[] newest-first (all workspaces, filtered client-side)
let workspaces     = [];     // Workspace[]
let activeWorkspace = null;  // Workspace | null

const ACTIVE_WORKSPACE_KEY = 'ione.activeWorkspaceId';

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

function createConversation(title, workspaceId) {
  return apiFetch('/api/v1/conversations', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ title, workspaceId }),
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

function listWorkspaces() {
  return apiFetch('/api/v1/workspaces');
}

function createWorkspace(name, domain, lifecycle, parentId) {
  const body = { name, domain: domain || 'generic', lifecycle };
  if (parentId) body.parentId = parentId;
  return apiFetch('/api/v1/workspaces', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
}

function closeWorkspace(id) {
  return apiFetch('/api/v1/workspaces/' + id + '/close', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({}),
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

/* ── Workspace helpers ── */

function workspaceIsClosed(ws) {
  return ws && ws.closedAt != null;
}

function workspaceLabel(ws) {
  if (workspaceIsClosed(ws)) {
    const date = formatDate(ws.closedAt);
    return ws.name + ' (closed ' + date + ')';
  }
  return ws.name;
}

function setActiveWorkspace(ws) {
  activeWorkspace = ws;
  localStorage.setItem(ACTIVE_WORKSPACE_KEY, ws.id);

  workspaceNameEl.textContent = workspaceLabel(ws);
  workspaceDomainEl.textContent = ws.domain || '';

  const isClosed = workspaceIsClosed(ws);
  newChatBtn.hidden = isClosed;
  workspaceClosedNotice.hidden = !isClosed;

  if (workspaceNameEl.classList) {
    workspaceNameEl.classList.toggle('workspace-name--closed', isClosed);
  }

  // Reset active conversation when switching workspaces.
  activeConvId = null;
  clearTranscript();
  clearStatus();

  renderSidebar();
}

/* ── Workspace menu ── */

function buildWorkspaceMenuItem(ws) {
  const li = document.createElement('li');
  li.setAttribute('role', 'menuitem');
  li.className = 'ws-menu-item' + (workspaceIsClosed(ws) ? ' ws-menu-item--closed' : '');
  li.dataset.id = ws.id;

  const labelSpan = document.createElement('span');
  labelSpan.className = 'ws-menu-label';
  labelSpan.textContent = workspaceLabel(ws);
  li.appendChild(labelSpan);

  if (!workspaceIsClosed(ws)) {
    const closeBtn = document.createElement('button');
    closeBtn.type = 'button';
    closeBtn.className = 'ws-close-btn';
    closeBtn.title = 'Close workspace';
    closeBtn.setAttribute('aria-label', 'Close workspace ' + ws.name);
    closeBtn.textContent = '⎘';

    closeBtn.addEventListener('click', async (e) => {
      e.stopPropagation();
      if (!confirm('Close workspace "' + ws.name + '"?')) return;
      try {
        const updated = await closeWorkspace(ws.id);
        // Update in-place so the same object reference is mutated for downstream callers.
        ws.closedAt = updated.closedAt;
        // Re-render just this item.
        const newLi = buildWorkspaceMenuItem(ws);
        workspaceMenu.replaceChild(newLi, li);
        // If this was the active workspace, refresh the header.
        if (activeWorkspace && activeWorkspace.id === ws.id) {
          setActiveWorkspace(ws);
        }
      } catch (err) {
        alert('Failed to close workspace: ' + err.message);
      }
    });

    li.appendChild(closeBtn);
  }

  li.addEventListener('click', (e) => {
    if (e.target.classList.contains('ws-close-btn')) return;
    setActiveWorkspace(ws);
    closeWorkspaceMenu();
  });

  li.setAttribute('tabindex', '0');
  li.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      if (e.target === li) {
        setActiveWorkspace(ws);
        closeWorkspaceMenu();
      }
    }
    if (e.key === 'Escape') {
      closeWorkspaceMenu();
      workspaceTrigger.focus();
    }
  });

  return li;
}

function renderWorkspaceMenu() {
  workspaceMenu.innerHTML = '';
  workspaces.forEach((ws) => {
    workspaceMenu.appendChild(buildWorkspaceMenuItem(ws));
  });
}

function openWorkspaceMenu() {
  renderWorkspaceMenu();
  workspaceMenu.hidden = false;
  workspaceTrigger.setAttribute('aria-expanded', 'true');
  // Focus first item.
  const first = workspaceMenu.querySelector('[role="menuitem"]');
  if (first) first.focus();
}

function closeWorkspaceMenu() {
  workspaceMenu.hidden = true;
  workspaceTrigger.setAttribute('aria-expanded', 'false');
}

workspaceTrigger.addEventListener('click', () => {
  if (workspaceMenu.hidden) {
    openWorkspaceMenu();
  } else {
    closeWorkspaceMenu();
  }
});

workspaceTrigger.addEventListener('keydown', (e) => {
  if (e.key === 'ArrowDown' || e.key === 'Enter' || e.key === ' ') {
    e.preventDefault();
    openWorkspaceMenu();
  }
});

// Close menu on outside click.
document.addEventListener('click', (e) => {
  if (!workspaceMenu.hidden && !document.getElementById('workspace-switcher').contains(e.target)) {
    closeWorkspaceMenu();
  }
});

// Close menu on Escape.
document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && !workspaceMenu.hidden) {
    closeWorkspaceMenu();
    workspaceTrigger.focus();
  }
});

/* ── Sidebar rendering ── */

function visibleConversations() {
  if (!activeWorkspace) return conversations;
  const wsId = activeWorkspace.id;
  /*
   * TODO: remove client-side filter once GET /api/v1/conversations supports
   * ?workspaceId= server-side filtering. The server currently returns all
   * conversations for the default user regardless of workspace.
   */
  return conversations.filter((c) => c.workspaceId === wsId);
}

function renderSidebar() {
  convList.innerHTML = '';
  visibleConversations().forEach((conv) => {
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
  // Only show in sidebar if it belongs to the active workspace.
  if (!activeWorkspace || conv.workspaceId === activeWorkspace.id) {
    const li = buildConvItem(conv);
    convList.insertBefore(li, convList.firstChild);
  }
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
  if (!activeWorkspace || workspaceIsClosed(activeWorkspace)) return;

  clearStatus();
  clearTranscript();
  activeConvId = null;
  setActiveSidebarItem(null);

  try {
    const conv = await createConversation('New chat', activeWorkspace.id);
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

  if (!activeWorkspace || workspaceIsClosed(activeWorkspace)) {
    setStatus('Cannot send — workspace is closed.');
    return;
  }

  clearStatus();

  // Ensure we have an active conversation.
  if (!activeConvId) {
    const title = content.slice(0, 40) || 'New chat';
    try {
      const conv = await createConversation(title, activeWorkspace.id);
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

/* ── New workspace modal ── */

newWorkspaceBtn.addEventListener('click', () => {
  nwNameInput.value = '';
  nwDomainInput.value = '';
  nwLifecycleSelect.value = 'continuous';
  nwErrorEl.hidden = true;
  nwErrorEl.textContent = '';
  nwSubmitBtn.disabled = false;
  newWorkspaceDialog.showModal();
  nwNameInput.focus();
});

nwCancelBtn.addEventListener('click', () => {
  newWorkspaceDialog.close();
});

newWorkspaceDialog.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    newWorkspaceDialog.close();
  }
});

newWorkspaceForm.addEventListener('submit', async (e) => {
  e.preventDefault();
  const name = nwNameInput.value.trim();
  if (!name) {
    nwErrorEl.textContent = 'Name is required.';
    nwErrorEl.hidden = false;
    return;
  }
  const domain = nwDomainInput.value.trim() || 'generic';
  const lifecycle = nwLifecycleSelect.value;

  nwSubmitBtn.disabled = true;
  nwErrorEl.hidden = true;

  try {
    const ws = await createWorkspace(name, domain, lifecycle, null);
    workspaces.push(ws);
    newWorkspaceDialog.close();
    setActiveWorkspace(ws);
  } catch (err) {
    nwErrorEl.textContent = 'Error: ' + err.message;
    nwErrorEl.hidden = false;
    nwSubmitBtn.disabled = false;
  }
});

/* ── Init: load workspaces then conversations ── */

(async function init() {
  try {
    const wsData = await listWorkspaces();
    workspaces = wsData.items || [];

    // Restore last-active workspace from localStorage, fall back to first.
    const savedId = localStorage.getItem(ACTIVE_WORKSPACE_KEY);
    const restored = savedId ? workspaces.find((w) => w.id === savedId) : null;
    const initial = restored || workspaces[0] || null;

    if (initial) {
      // Call directly (not setActiveWorkspace) to avoid clearing transcript before convs load.
      activeWorkspace = initial;
      localStorage.setItem(ACTIVE_WORKSPACE_KEY, initial.id);
      workspaceNameEl.textContent = workspaceLabel(initial);
      workspaceDomainEl.textContent = initial.domain || '';
      const isClosed = workspaceIsClosed(initial);
      newChatBtn.hidden = isClosed;
      workspaceClosedNotice.hidden = !isClosed;
    } else {
      workspaceNameEl.textContent = 'No workspaces';
    }
  } catch (err) {
    setStatus('Error loading workspaces: ' + err.message);
    workspaceNameEl.textContent = 'Error';
  }

  try {
    const data = await listConversations();
    // Response shape: { items: Conversation[] }
    conversations = data.items || [];
    renderSidebar();
  } catch (err) {
    setStatus('Error loading conversations: ' + err.message);
  }
})();

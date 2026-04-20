/* ── Auth UI ── */
const userLabelEl = document.getElementById('user-label');
const loginBtn    = document.getElementById('login-btn');
const logoutBtn   = document.getElementById('logout-btn');

async function loadMe() {
  try {
    const data = await apiFetch('/api/v1/me');
    const user = data.user;
    if (user.email === 'default@localhost') {
      userLabelEl.textContent = 'Signed in as default (local)';
      loginBtn.hidden  = true;
      logoutBtn.hidden = true;
    } else {
      userLabelEl.textContent = 'Signed in as ' + (user.displayName || user.email);
      loginBtn.hidden  = true;
      logoutBtn.hidden = false;
    }
  } catch (_) {
    userLabelEl.textContent = '';
    loginBtn.hidden  = false;
    logoutBtn.hidden = true;
  }
}

if (loginBtn) {
  loginBtn.addEventListener('click', () => {
    // Redirect to login with the default Keycloak issuer. Operators can
    // customize IONE_KEYCLOAK_ISSUER or this URL as needed.
    const issuer = encodeURIComponent(
      window.IONE_KEYCLOAK_ISSUER || 'http://localhost:8080/realms/ione'
    );
    window.location.href = '/auth/login?issuer=' + issuer;
  });
}

if (logoutBtn) {
  logoutBtn.addEventListener('click', async () => {
    await fetch('/auth/logout', { method: 'POST' });
    await loadMe();
  });
}

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

function listSignals(workspaceId, source, severity) {
  let url = '/api/v1/workspaces/' + workspaceId + '/signals';
  const params = [];
  if (source) params.push('source=' + encodeURIComponent(source));
  if (severity) params.push('severity=' + encodeURIComponent(severity));
  if (params.length) url += '?' + params.join('&');
  return apiFetch(url);
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

  // If the connectors tab is active, reload connectors for the new workspace.
  if (activeTab === 'connectors') {
    loadConnectors(ws.id);
  }

  // If the signals tab is active, reload signals for the new workspace.
  if (activeTab === 'signals') {
    loadSignals();
  }

  // If the survivors tab is active, reload roles + survivors for the new workspace.
  if (activeTab === 'survivors') {
    loadRolesIntoFilter(ws.id).then(() => loadSurvivors());
  }

  // If the approvals tab is active, reload approvals for the new workspace.
  if (activeTab === 'approvals') {
    loadApprovals();
  }

  // Always update the pending badge count.
  updateApprovalsBadge(ws.id);
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

/* ── Tab state ── */

const tabChat          = document.getElementById('tab-chat');
const tabConnectors    = document.getElementById('tab-connectors');
const tabSignals       = document.getElementById('tab-signals');
const tabSurvivors     = document.getElementById('tab-survivors');
const tabApprovals     = document.getElementById('tab-approvals');
const panelChat        = document.getElementById('panel-chat');
const panelConnectors  = document.getElementById('panel-connectors');
const panelSignals     = document.getElementById('panel-signals');
const panelSurvivors   = document.getElementById('panel-survivors');
const panelApprovals   = document.getElementById('panel-approvals');

let activeTab = 'chat'; // 'chat' | 'connectors' | 'signals' | 'survivors' | 'approvals'

function switchTab(name) {
  // Stop auto-refresh when leaving a polling tab.
  if (activeTab === 'signals' && name !== 'signals') {
    stopSignalsPolling();
  }
  if (activeTab === 'survivors' && name !== 'survivors') {
    stopSurvivorsPolling();
  }
  if (activeTab === 'approvals' && name !== 'approvals') {
    stopApprovalsPolling();
  }

  activeTab = name;

  tabChat.setAttribute('aria-selected', String(name === 'chat'));
  tabChat.classList.toggle('tab--active', name === 'chat');
  panelChat.hidden = name !== 'chat';

  tabConnectors.setAttribute('aria-selected', String(name === 'connectors'));
  tabConnectors.classList.toggle('tab--active', name === 'connectors');
  panelConnectors.hidden = name !== 'connectors';

  tabSignals.setAttribute('aria-selected', String(name === 'signals'));
  tabSignals.classList.toggle('tab--active', name === 'signals');
  panelSignals.hidden = name !== 'signals';

  tabSurvivors.setAttribute('aria-selected', String(name === 'survivors'));
  tabSurvivors.classList.toggle('tab--active', name === 'survivors');
  panelSurvivors.hidden = name !== 'survivors';

  tabApprovals.setAttribute('aria-selected', String(name === 'approvals'));
  tabApprovals.classList.toggle('tab--active', name === 'approvals');
  panelApprovals.hidden = name !== 'approvals';

  if (name === 'connectors' && activeWorkspace) {
    loadConnectors(activeWorkspace.id);
  }

  if (name === 'signals' && activeWorkspace) {
    loadSignals();
    startSignalsPolling();
  }

  if (name === 'survivors' && activeWorkspace) {
    loadRolesIntoFilter(activeWorkspace.id).then(() => loadSurvivors());
    startSurvivorsPolling();
  }

  if (name === 'approvals' && activeWorkspace) {
    loadApprovals();
    startApprovalsPolling();
  }
}

tabChat.addEventListener('click', () => switchTab('chat'));
tabConnectors.addEventListener('click', () => switchTab('connectors'));
tabSignals.addEventListener('click', () => switchTab('signals'));
tabSurvivors.addEventListener('click', () => switchTab('survivors'));
tabApprovals.addEventListener('click', () => switchTab('approvals'));

tabChat.addEventListener('keydown', (e) => {
  if (e.key === 'ArrowRight') { e.preventDefault(); tabConnectors.focus(); tabConnectors.click(); }
});
tabConnectors.addEventListener('keydown', (e) => {
  if (e.key === 'ArrowLeft') { e.preventDefault(); tabChat.focus(); tabChat.click(); }
  if (e.key === 'ArrowRight') { e.preventDefault(); tabSignals.focus(); tabSignals.click(); }
});
tabSignals.addEventListener('keydown', (e) => {
  if (e.key === 'ArrowLeft') { e.preventDefault(); tabConnectors.focus(); tabConnectors.click(); }
  if (e.key === 'ArrowRight') { e.preventDefault(); tabSurvivors.focus(); tabSurvivors.click(); }
});
tabSurvivors.addEventListener('keydown', (e) => {
  if (e.key === 'ArrowLeft') { e.preventDefault(); tabSignals.focus(); tabSignals.click(); }
  if (e.key === 'ArrowRight') { e.preventDefault(); tabApprovals.focus(); tabApprovals.click(); }
});
tabApprovals.addEventListener('keydown', (e) => {
  if (e.key === 'ArrowLeft') { e.preventDefault(); tabSurvivors.focus(); tabSurvivors.click(); }
});

/* ── Connector + Stream API helpers ── */

function listConnectors(workspaceId) {
  return apiFetch('/api/v1/workspaces/' + workspaceId + '/connectors');
}

function createConnector(workspaceId, kind, name, config) {
  return apiFetch('/api/v1/workspaces/' + workspaceId + '/connectors', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ kind, name, config }),
  });
}

function listStreams(connectorId) {
  return apiFetch('/api/v1/connectors/' + connectorId + '/streams');
}

function pollStream(streamId) {
  return apiFetch('/api/v1/streams/' + streamId + '/poll', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({}),
  });
}

/* ── Connector panel state ── */

const connectorsStatusEl  = document.getElementById('connectors-status');
const connectorListEl     = document.getElementById('connector-list');
const addConnectorBtn     = document.getElementById('add-connector-btn');
const addConnectorDialog  = document.getElementById('add-connector-dialog');
const addConnectorForm    = document.getElementById('add-connector-form');
const acKindSelect        = document.getElementById('ac-kind');
const acNameInput         = document.getElementById('ac-name');
const acConfigTextarea    = document.getElementById('ac-config');
const acErrorEl           = document.getElementById('ac-error');
const acCancelBtn         = document.getElementById('ac-cancel-btn');
const acSubmitBtn         = document.getElementById('ac-submit-btn');

// connectorStreams: Map<connectorId, Stream[]>
const connectorStreams = new Map();

function setConnectorsStatus(msg, isError) {
  connectorsStatusEl.textContent = msg;
  connectorsStatusEl.style.color = isError ? 'var(--color-error)' : 'var(--color-sidebar-muted)';
}

function clearConnectorsStatus() {
  connectorsStatusEl.textContent = '';
}

/* ── Build stream sub-list ── */

function buildStreamItem(stream) {
  const li = document.createElement('li');
  li.className = 'stream-item';
  li.dataset.id = stream.id;

  const nameSpan = document.createElement('span');
  nameSpan.className = 'stream-name';
  nameSpan.textContent = stream.name;

  const pollBtn = document.createElement('button');
  pollBtn.type = 'button';
  pollBtn.className = 'poll-btn';
  pollBtn.textContent = 'Poll';
  pollBtn.setAttribute('aria-label', 'Poll stream ' + stream.name);

  const resultSpan = document.createElement('span');
  resultSpan.className = 'poll-result';
  resultSpan.setAttribute('aria-live', 'polite');

  pollBtn.addEventListener('click', async () => {
    pollBtn.disabled = true;
    resultSpan.textContent = 'Polling…';
    resultSpan.className = 'poll-result';
    try {
      const data = await pollStream(stream.id);
      resultSpan.textContent = 'Ingested ' + data.ingested + ' events';
      resultSpan.className = 'poll-result';
      // Refresh streams for parent connector.
      await refreshStreamsForConnector(stream.connectorId);
    } catch (err) {
      resultSpan.textContent = 'Error: ' + err.message;
      resultSpan.className = 'poll-result poll-result--error';
    } finally {
      pollBtn.disabled = false;
    }
  });

  li.appendChild(nameSpan);
  li.appendChild(pollBtn);
  li.appendChild(resultSpan);
  return li;
}

function buildStreamSubList(connectorId, streams) {
  if (streams.length === 0) {
    const p = document.createElement('p');
    p.className = 'streams-empty';
    p.textContent = 'No streams';
    return p;
  }

  const ul = document.createElement('ul');
  ul.className = 'stream-list';
  ul.setAttribute('role', 'list');
  streams.forEach((s) => ul.appendChild(buildStreamItem(s)));
  return ul;
}

async function refreshStreamsForConnector(connectorId) {
  const card = connectorListEl.querySelector('[data-connector-id="' + connectorId + '"]');
  if (!card) return;

  const streamsContainer = card.querySelector('.connector-streams');
  if (!streamsContainer) return;

  try {
    const data = await listStreams(connectorId);
    const streams = data.items || [];
    connectorStreams.set(connectorId, streams);
    streamsContainer.innerHTML = '';
    streamsContainer.appendChild(buildStreamSubList(connectorId, streams));
  } catch (err) {
    streamsContainer.innerHTML = '';
    const p = document.createElement('p');
    p.className = 'streams-error';
    p.textContent = 'Error loading streams: ' + err.message;
    streamsContainer.appendChild(p);
  }
}

/* ── Build connector card ── */

function buildConnectorCard(connector) {
  const li = document.createElement('li');
  li.className = 'connector-card';
  li.dataset.connectorId = connector.id;

  // Header row.
  const header = document.createElement('div');
  header.className = 'connector-card-header';

  const nameSpan = document.createElement('span');
  nameSpan.className = 'connector-name';
  nameSpan.textContent = connector.name;

  const kindBadge = document.createElement('span');
  kindBadge.className = 'badge-kind';
  kindBadge.textContent = connector.kind;

  const statusChip = document.createElement('span');
  statusChip.className = 'status-chip status-chip--' + connector.status;
  statusChip.textContent = connector.status;
  if (connector.status === 'error' && connector.lastError) {
    statusChip.title = connector.lastError;
  }

  header.appendChild(nameSpan);
  header.appendChild(kindBadge);
  header.appendChild(statusChip);
  li.appendChild(header);

  // Streams container (async-filled).
  const streamsContainer = document.createElement('div');
  streamsContainer.className = 'connector-streams';

  const loadingP = document.createElement('p');
  loadingP.className = 'stream-loading';
  loadingP.textContent = 'Loading streams…';
  streamsContainer.appendChild(loadingP);

  li.appendChild(streamsContainer);

  // Load streams.
  listStreams(connector.id).then((data) => {
    const streams = data.items || [];
    connectorStreams.set(connector.id, streams);
    streamsContainer.innerHTML = '';
    streamsContainer.appendChild(buildStreamSubList(connector.id, streams));
  }).catch((err) => {
    streamsContainer.innerHTML = '';
    const p = document.createElement('p');
    p.className = 'streams-error';
    p.textContent = 'Error: ' + err.message;
    streamsContainer.appendChild(p);
  });

  return li;
}

/* ── Load connectors for active workspace ── */

async function loadConnectors(workspaceId) {
  clearConnectorsStatus();
  connectorListEl.innerHTML = '';
  connectorStreams.clear();

  const loadingP = document.createElement('p');
  loadingP.className = 'stream-loading';
  loadingP.textContent = 'Loading connectors…';
  connectorListEl.appendChild(loadingP);

  try {
    const data = await listConnectors(workspaceId);
    const items = data.items || [];
    connectorListEl.innerHTML = '';

    if (items.length === 0) {
      const p = document.createElement('p');
      p.className = 'streams-empty';
      p.textContent = 'No connectors. Add one to start ingesting data.';
      connectorListEl.appendChild(p);
      return;
    }

    items.forEach((connector) => {
      connectorListEl.appendChild(buildConnectorCard(connector));
    });
  } catch (err) {
    connectorListEl.innerHTML = '';
    setConnectorsStatus('Error loading connectors: ' + err.message, true);
  }
}

/* ── Add connector dialog ── */

addConnectorBtn.addEventListener('click', () => {
  acKindSelect.value = 'rust_native';
  acNameInput.value = '';
  acConfigTextarea.value = '';
  acErrorEl.hidden = true;
  acErrorEl.textContent = '';
  acSubmitBtn.disabled = false;
  addConnectorDialog.showModal();
  acNameInput.focus();
});

acCancelBtn.addEventListener('click', () => {
  addConnectorDialog.close();
});

addConnectorDialog.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    addConnectorDialog.close();
  }
});

addConnectorForm.addEventListener('submit', async (e) => {
  e.preventDefault();

  const name = acNameInput.value.trim();
  const kind = acKindSelect.value;
  const configRaw = acConfigTextarea.value.trim();

  if (!name) {
    acErrorEl.textContent = 'Name is required.';
    acErrorEl.hidden = false;
    return;
  }

  let config = {};
  if (configRaw) {
    try {
      config = JSON.parse(configRaw);
    } catch (_) {
      acErrorEl.textContent = 'Config is not valid JSON.';
      acErrorEl.hidden = false;
      return;
    }
  }

  acSubmitBtn.disabled = true;
  acErrorEl.hidden = true;

  try {
    const connector = await createConnector(activeWorkspace.id, kind, name, config);
    addConnectorDialog.close();
    // Append new card without full reload.
    const emptyNotice = connectorListEl.querySelector('.streams-empty');
    if (emptyNotice) connectorListEl.removeChild(emptyNotice);
    connectorListEl.appendChild(buildConnectorCard(connector));
  } catch (err) {
    acErrorEl.textContent = 'Error: ' + err.message;
    acErrorEl.hidden = false;
    acSubmitBtn.disabled = false;
  }
});

/* ── Signals panel ── */

const signalsFilterSource   = document.getElementById('signals-filter-source');
const signalsFilterSeverity = document.getElementById('signals-filter-severity');
const signalsRefreshBtn     = document.getElementById('signals-refresh-btn');
const signalsStatusEl       = document.getElementById('signals-status');
const signalListEl          = document.getElementById('signal-list');

let signalsPollTimer = null;
const SIGNALS_POLL_MS = 15000;

function setSignalsStatus(msg, isError) {
  signalsStatusEl.textContent = msg;
  signalsStatusEl.className = isError ? 'signals-status--error' : '';
}

function clearSignalsStatus() {
  signalsStatusEl.textContent = '';
  signalsStatusEl.className = '';
}

function formatDateTime(iso) {
  const d = new Date(iso);
  return d.toLocaleString(undefined, {
    month: 'short', day: 'numeric',
    hour: 'numeric', minute: '2-digit',
  });
}

function sourceBadgeClass(source) {
  if (source === 'rule') return 'signal-source--rule';
  if (source === 'connector_event') return 'signal-source--connector-event';
  if (source === 'generator') return 'signal-source--generator';
  return '';
}

function sourceLabel(source) {
  if (source === 'connector_event') return 'connector event';
  return source;
}

function buildSignalCard(signal) {
  const li = document.createElement('li');
  li.className = 'signal-card';

  // Header row: severity chip + source badge + timestamp
  const header = document.createElement('div');
  header.className = 'signal-card-header';

  const severityChip = document.createElement('span');
  severityChip.className = 'severity-chip severity-chip--' + signal.severity;
  severityChip.textContent = signal.severity;

  const sourceBadge = document.createElement('span');
  sourceBadge.className = 'signal-source-badge ' + sourceBadgeClass(signal.source);
  sourceBadge.textContent = sourceLabel(signal.source);

  const timeSpan = document.createElement('span');
  timeSpan.className = 'signal-time';
  timeSpan.textContent = formatDateTime(signal.createdAt);

  header.appendChild(severityChip);
  header.appendChild(sourceBadge);
  header.appendChild(timeSpan);
  li.appendChild(header);

  // Title
  const titleEl = document.createElement('div');
  titleEl.className = 'signal-title';
  titleEl.textContent = signal.title;
  li.appendChild(titleEl);

  // Body
  const bodyEl = document.createElement('div');
  bodyEl.className = 'signal-body';
  bodyEl.textContent = signal.body;
  li.appendChild(bodyEl);

  // generatorModel secondary line (generator source only)
  if (signal.source === 'generator' && signal.generatorModel) {
    const modelEl = document.createElement('div');
    modelEl.className = 'signal-model';
    modelEl.textContent = signal.generatorModel;
    li.appendChild(modelEl);
  }

  // Evidence disclosure (hide if empty / null)
  const evidence = signal.evidence;
  const hasEvidence = evidence && (Array.isArray(evidence) ? evidence.length > 0 : Object.keys(evidence).length > 0);
  if (hasEvidence) {
    const details = document.createElement('details');
    details.className = 'signal-evidence';

    const summary = document.createElement('summary');
    summary.textContent = 'Evidence';
    details.appendChild(summary);

    const pre = document.createElement('pre');
    pre.className = 'signal-evidence-body';
    pre.textContent = JSON.stringify(evidence, null, 2);
    details.appendChild(pre);

    li.appendChild(details);
  }

  return li;
}

async function loadSignals() {
  if (!activeWorkspace) return;

  clearSignalsStatus();
  signalListEl.innerHTML = '';

  const loadingEl = document.createElement('p');
  loadingEl.className = 'signals-loading';
  loadingEl.textContent = 'Loading signals…';
  signalListEl.appendChild(loadingEl);

  const source = signalsFilterSource.value;
  const severity = signalsFilterSeverity.value;

  try {
    const data = await listSignals(activeWorkspace.id, source, severity);
    const items = data.items || [];
    signalListEl.innerHTML = '';

    if (items.length === 0) {
      const empty = document.createElement('p');
      empty.className = 'signals-empty';
      empty.textContent = 'No signals match the current filters.';
      signalListEl.appendChild(empty);
      return;
    }

    items.forEach((signal) => {
      signalListEl.appendChild(buildSignalCard(signal));
    });
  } catch (err) {
    signalListEl.innerHTML = '';
    setSignalsStatus('Error loading signals: ' + err.message, true);
  }
}

function startSignalsPolling() {
  stopSignalsPolling();
  signalsPollTimer = setInterval(() => {
    if (activeTab === 'signals') {
      loadSignals();
    }
  }, SIGNALS_POLL_MS);
}

function stopSignalsPolling() {
  if (signalsPollTimer !== null) {
    clearInterval(signalsPollTimer);
    signalsPollTimer = null;
  }
}

signalsRefreshBtn.addEventListener('click', loadSignals);
signalsFilterSource.addEventListener('change', loadSignals);
signalsFilterSeverity.addEventListener('change', loadSignals);

/* ── Survivors panel ── */

const survivorsFilterRole    = document.getElementById('survivors-filter-role');
const survivorsFilterVerdict = document.getElementById('survivors-filter-verdict');
const survivorsRefreshBtn    = document.getElementById('survivors-refresh-btn');
const survivorsStatusEl      = document.getElementById('survivors-status');
const survivorListEl         = document.getElementById('survivor-list');

let survivorsPollTimer = null;
const SURVIVORS_POLL_MS = 15000;

const ACTIVE_ROLE_KEY_PREFIX = 'ione.activeRoleId.';

function activeRoleStorageKey(workspaceId) {
  return ACTIVE_ROLE_KEY_PREFIX + workspaceId;
}

function listSurvivors(workspaceId, verdict) {
  let url = '/api/v1/workspaces/' + workspaceId + '/survivors';
  if (verdict) url += '?verdict=' + encodeURIComponent(verdict);
  return apiFetch(url);
}

function listFeed(workspaceId, roleId) {
  return apiFetch('/api/v1/workspaces/' + workspaceId + '/feed?roleId=' + encodeURIComponent(roleId));
}

function listRoles(workspaceId) {
  return apiFetch('/api/v1/workspaces/' + workspaceId + '/roles');
}

async function loadRolesIntoFilter(workspaceId) {
  // Reset to "All survivors" then populate from API.
  survivorsFilterRole.innerHTML = '<option value="">All survivors</option>';

  try {
    const data = await listRoles(workspaceId);
    const roles = data.items || [];
    roles.forEach((role) => {
      const opt = document.createElement('option');
      opt.value = role.id;
      opt.textContent = role.name + ' (CoC ' + role.cocLevel + ')';
      survivorsFilterRole.appendChild(opt);
    });

    // Restore persisted role selection for this workspace.
    const savedRoleId = localStorage.getItem(activeRoleStorageKey(workspaceId));
    if (savedRoleId && roles.find((r) => r.id === savedRoleId)) {
      survivorsFilterRole.value = savedRoleId;
    }
  } catch (_err) {
    // Non-fatal: role filter just stays at "All survivors".
  }
}

function setSurvivorsStatus(msg, isError) {
  survivorsStatusEl.textContent = msg;
  survivorsStatusEl.className = isError ? 'survivors-status--error' : '';
}

function clearSurvivorsStatus() {
  survivorsStatusEl.textContent = '';
  survivorsStatusEl.className = '';
}

function verdictChipClass(verdict) {
  if (verdict === 'survive') return 'verdict-chip--survive';
  if (verdict === 'reject') return 'verdict-chip--reject';
  if (verdict === 'defer') return 'verdict-chip--defer';
  return '';
}

function buildConfidenceBar(confidence) {
  const wrapper = document.createElement('div');
  wrapper.className = 'confidence-bar-wrapper';
  wrapper.title = 'Confidence: ' + (confidence * 100).toFixed(0) + '%';

  const track = document.createElement('div');
  track.className = 'confidence-bar-track';

  const fill = document.createElement('div');
  fill.className = 'confidence-bar-fill';
  fill.style.width = (confidence * 100).toFixed(1) + '%';

  track.appendChild(fill);
  wrapper.appendChild(track);

  const label = document.createElement('span');
  label.className = 'confidence-bar-label';
  label.textContent = (confidence * 100).toFixed(0) + '%';
  wrapper.appendChild(label);

  return wrapper;
}

function routingChipClass(kind) {
  if (kind === 'feed') return 'routing-chip--feed';
  if (kind === 'notification') return 'routing-chip--notification';
  if (kind === 'draft') return 'routing-chip--draft';
  if (kind === 'peer') return 'routing-chip--peer';
  return '';
}

function buildRoutingChips(routingDecisions) {
  if (!Array.isArray(routingDecisions) || routingDecisions.length === 0) return null;
  const row = document.createElement('div');
  row.className = 'routing-chips';
  routingDecisions.forEach((rd) => {
    const chip = document.createElement('span');
    chip.className = 'routing-chip ' + routingChipClass(rd.targetKind);
    chip.textContent = rd.targetKind;
    chip.title = rd.rationale || '';
    row.appendChild(chip);
  });
  return row;
}

function buildSurvivorCard(survivor) {
  const li = document.createElement('li');
  li.className = 'survivor-card';

  // Header: verdict chip + severity chip + timestamp
  const header = document.createElement('div');
  header.className = 'survivor-card-header';

  const verdictChip = document.createElement('span');
  verdictChip.className = 'verdict-chip ' + verdictChipClass(survivor.verdict);
  verdictChip.textContent = survivor.verdict;

  const severityChip = document.createElement('span');
  severityChip.className = 'severity-chip severity-chip--' + (survivor.signalSeverity || '');
  severityChip.textContent = survivor.signalSeverity || '';

  const timeSpan = document.createElement('span');
  timeSpan.className = 'signal-time';
  timeSpan.textContent = formatDateTime(survivor.createdAt);

  header.appendChild(verdictChip);
  if (survivor.signalSeverity) header.appendChild(severityChip);
  header.appendChild(timeSpan);
  li.appendChild(header);

  // Confidence bar
  li.appendChild(buildConfidenceBar(survivor.confidence));

  // Signal title (from joined projection)
  const titleEl = document.createElement('div');
  titleEl.className = 'signal-title';
  titleEl.textContent = survivor.signalTitle || '(no title)';
  li.appendChild(titleEl);

  // Signal body
  const bodyEl = document.createElement('div');
  bodyEl.className = 'signal-body';
  bodyEl.textContent = survivor.signalBody || '';
  li.appendChild(bodyEl);

  // Critic model secondary line
  const modelEl = document.createElement('div');
  modelEl.className = 'signal-model';
  modelEl.textContent = survivor.criticModel;
  li.appendChild(modelEl);

  // Rationale
  const rationaleEl = document.createElement('div');
  rationaleEl.className = 'survivor-rationale';
  rationaleEl.textContent = survivor.rationale;
  li.appendChild(rationaleEl);

  // Routing decision chips (inline, from server-side join)
  const chipsEl = buildRoutingChips(survivor.routingDecisions);
  if (chipsEl) li.appendChild(chipsEl);

  // Expandable chain-of-reasoning
  const steps = survivor.chainOfReasoning;
  if (Array.isArray(steps) && steps.length > 0) {
    const details = document.createElement('details');
    details.className = 'survivor-reasoning';

    const summary = document.createElement('summary');
    summary.textContent = 'Reasoning (' + steps.length + ' step' + (steps.length !== 1 ? 's' : '') + ')';
    details.appendChild(summary);

    const ol = document.createElement('ol');
    ol.className = 'survivor-reasoning-steps';
    steps.forEach((step) => {
      const item = document.createElement('li');
      item.textContent = typeof step === 'string' ? step : JSON.stringify(step);
      ol.appendChild(item);
    });
    details.appendChild(ol);
    li.appendChild(details);
  }

  return li;
}

async function loadSurvivors() {
  if (!activeWorkspace) return;

  clearSurvivorsStatus();
  survivorListEl.innerHTML = '';

  const loadingEl = document.createElement('p');
  loadingEl.className = 'survivors-loading';
  loadingEl.textContent = 'Loading survivors…';
  survivorListEl.appendChild(loadingEl);

  const roleId = survivorsFilterRole.value;
  const verdict = survivorsFilterVerdict.value;

  // Persist role selection for this workspace.
  if (roleId) {
    localStorage.setItem(activeRoleStorageKey(activeWorkspace.id), roleId);
  } else {
    localStorage.removeItem(activeRoleStorageKey(activeWorkspace.id));
  }

  try {
    let data;
    if (roleId) {
      // Use feed endpoint for role-scoped view.
      data = await listFeed(activeWorkspace.id, roleId);
    } else {
      data = await listSurvivors(activeWorkspace.id, verdict);
    }
    const items = data.items || [];
    survivorListEl.innerHTML = '';

    if (items.length === 0) {
      const empty = document.createElement('p');
      empty.className = 'survivors-empty';
      empty.textContent = 'No survivors match the current filter.';
      survivorListEl.appendChild(empty);
      return;
    }

    items.forEach((survivor) => {
      survivorListEl.appendChild(buildSurvivorCard(survivor));
    });
  } catch (err) {
    survivorListEl.innerHTML = '';
    setSurvivorsStatus('Error loading survivors: ' + err.message, true);
  }
}

function startSurvivorsPolling() {
  stopSurvivorsPolling();
  survivorsPollTimer = setInterval(() => {
    if (activeTab === 'survivors') {
      loadSurvivors();
    }
  }, SURVIVORS_POLL_MS);
}

function stopSurvivorsPolling() {
  if (survivorsPollTimer !== null) {
    clearInterval(survivorsPollTimer);
    survivorsPollTimer = null;
  }
}

survivorsRefreshBtn.addEventListener('click', loadSurvivors);
survivorsFilterRole.addEventListener('change', loadSurvivors);
survivorsFilterVerdict.addEventListener('change', loadSurvivors);

/* ── Approvals panel ── */

const approvalsFilterStatus = document.getElementById('approvals-filter-status');
const approvalsRefreshBtn   = document.getElementById('approvals-refresh-btn');
const approvalsStatusEl     = document.getElementById('approvals-status');
const approvalListEl        = document.getElementById('approval-list');
const approvalsBadgeEl      = document.getElementById('approvals-badge');

let approvalsPollTimer = null;
const APPROVALS_POLL_MS = 10000;

function listApprovals(workspaceId, status) {
  let url = '/api/v1/workspaces/' + workspaceId + '/approvals';
  if (status) url += '?status=' + encodeURIComponent(status);
  return apiFetch(url);
}

function decideApproval(approvalId, decision, comment) {
  return apiFetch('/api/v1/approvals/' + approvalId, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ decision, comment: comment || undefined }),
  });
}

function setApprovalsStatus(msg, isError) {
  approvalsStatusEl.textContent = msg;
  approvalsStatusEl.className = isError ? 'approvals-status--error' : '';
}

function clearApprovalsStatus() {
  approvalsStatusEl.textContent = '';
  approvalsStatusEl.className = '';
}

async function updateApprovalsBadge(workspaceId) {
  try {
    const data = await listApprovals(workspaceId, 'pending');
    const count = (data.items || []).length;
    if (count > 0) {
      approvalsBadgeEl.textContent = count;
      approvalsBadgeEl.hidden = false;
    } else {
      approvalsBadgeEl.hidden = true;
    }
  } catch (_) {
    approvalsBadgeEl.hidden = true;
  }
}

async function loadApprovals() {
  if (!activeWorkspace) return;
  clearApprovalsStatus();
  const status = approvalsFilterStatus.value;
  try {
    const data = await listApprovals(activeWorkspace.id, status);
    const items = data.items || [];
    renderApprovals(items);
    updateApprovalsBadge(activeWorkspace.id);
  } catch (err) {
    setApprovalsStatus('Error loading approvals: ' + err.message, true);
  }
}

function severityBadgeClass(severity) {
  if (severity === 'command') return 'severity-badge--command';
  if (severity === 'flagged') return 'severity-badge--flagged';
  return 'severity-badge--routine';
}

function renderApprovals(items) {
  approvalListEl.innerHTML = '';

  if (items.length === 0) {
    const empty = document.createElement('li');
    empty.className = 'approval-item approval-item--empty';
    empty.textContent = 'No approvals to show.';
    approvalListEl.appendChild(empty);
    return;
  }

  items.forEach((approval) => {
    const li = document.createElement('li');
    li.className = 'approval-item';
    li.dataset.approvalId = approval.id;

    const content = approval.artifactContent || {};
    const title = content.title || '(untitled)';
    const body = content.body || '';
    const severity = content.severity || '';
    const rationale = content.rationale || '';

    const header = document.createElement('div');
    header.className = 'approval-header';

    const kindSpan = document.createElement('span');
    kindSpan.className = 'approval-kind';
    kindSpan.textContent = approval.kind || 'notification_draft';
    header.appendChild(kindSpan);

    if (severity) {
      const sevBadge = document.createElement('span');
      sevBadge.className = 'severity-badge ' + severityBadgeClass(severity);
      sevBadge.textContent = severity;
      header.appendChild(sevBadge);
    }

    const statusSpan = document.createElement('span');
    statusSpan.className = 'approval-status approval-status--' + approval.status;
    statusSpan.textContent = approval.status;
    header.appendChild(statusSpan);

    li.appendChild(header);

    const titleEl = document.createElement('div');
    titleEl.className = 'approval-title';
    titleEl.textContent = title;
    li.appendChild(titleEl);

    if (body) {
      const bodyEl = document.createElement('div');
      bodyEl.className = 'approval-body';
      bodyEl.textContent = body;
      li.appendChild(bodyEl);
    }

    if (rationale) {
      const rationaleEl = document.createElement('div');
      rationaleEl.className = 'approval-rationale';
      rationaleEl.textContent = 'Rationale: ' + rationale;
      li.appendChild(rationaleEl);
    }

    if (approval.status === 'pending') {
      const commentArea = document.createElement('textarea');
      commentArea.className = 'approval-comment';
      commentArea.placeholder = 'Comment (optional)';
      commentArea.rows = 2;
      li.appendChild(commentArea);

      const actions = document.createElement('div');
      actions.className = 'approval-actions';

      const approveBtn = document.createElement('button');
      approveBtn.className = 'approval-btn approval-btn--approve';
      approveBtn.type = 'button';
      approveBtn.textContent = 'Approve';
      approveBtn.addEventListener('click', async () => {
        approveBtn.disabled = true;
        rejectBtn.disabled = true;
        try {
          await decideApproval(approval.id, 'approved', commentArea.value);
          setApprovalsStatus('Sent.', false);
          await loadApprovals();
        } catch (err) {
          setApprovalsStatus('Delivery failed: ' + err.message, true);
          approveBtn.disabled = false;
          rejectBtn.disabled = false;
        }
      });

      const rejectBtn = document.createElement('button');
      rejectBtn.className = 'approval-btn approval-btn--reject';
      rejectBtn.type = 'button';
      rejectBtn.textContent = 'Reject';
      rejectBtn.addEventListener('click', async () => {
        approveBtn.disabled = true;
        rejectBtn.disabled = true;
        try {
          await decideApproval(approval.id, 'rejected', commentArea.value);
          setApprovalsStatus('Rejected.', false);
          await loadApprovals();
        } catch (err) {
          setApprovalsStatus('Error: ' + err.message, true);
          approveBtn.disabled = false;
          rejectBtn.disabled = false;
        }
      });

      actions.appendChild(approveBtn);
      actions.appendChild(rejectBtn);
      li.appendChild(actions);
    } else if (approval.decidedAt) {
      const decidedEl = document.createElement('div');
      decidedEl.className = 'approval-decided-at';
      decidedEl.textContent = 'Decided: ' + new Date(approval.decidedAt).toLocaleString();
      li.appendChild(decidedEl);
      if (approval.comment) {
        const commentEl = document.createElement('div');
        commentEl.className = 'approval-comment-display';
        commentEl.textContent = 'Comment: ' + approval.comment;
        li.appendChild(commentEl);
      }
    }

    approvalListEl.appendChild(li);
  });
}

function startApprovalsPolling() {
  stopApprovalsPolling();
  approvalsPollTimer = setInterval(() => {
    if (activeTab === 'approvals') {
      loadApprovals();
    }
  }, APPROVALS_POLL_MS);
}

function stopApprovalsPolling() {
  if (approvalsPollTimer !== null) {
    clearInterval(approvalsPollTimer);
    approvalsPollTimer = null;
  }
}

approvalsRefreshBtn.addEventListener('click', loadApprovals);
approvalsFilterStatus.addEventListener('change', loadApprovals);

/* ── Init: load workspaces then conversations ── */

(async function init() {
  // Fetch current user identity and update top bar.
  await loadMe();

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
      // Seed the pending approvals badge.
      updateApprovalsBadge(initial.id);
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

/* ── Auth UI ── */
const DEMO_WORKSPACE_ID_CONST = '00000000-0000-0000-0000-000000000d30';
const DEMO_WORKSPACE_ID = DEMO_WORKSPACE_ID_CONST;

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
    let errorBody = null;
    try { errorBody = await resp.json(); } catch (_) {}
    if (errorBody?.error === 'demo_read_only') {
      showToast('The demo workspace is read-only. Switch to your workspace to make changes.');
    }
    throw new ApiError(errorBody?.message || `HTTP ${resp.status}`, resp.status, errorBody);
  }
  return resp.json();
}

class ApiError extends Error {
  constructor(message, status, body = null) {
    super(message);
    this.status = status;
    this.body = body;
  }
}

async function pollOllamaHealth() {
  try {
    const res = await fetch('/api/v1/health/ollama');
    if (!res.ok) return null;
    return await res.json();
  } catch (_err) {
    return null;
  }
}

function renderHealthDot(health) {
  const btn = document.getElementById('health-dot');
  if (!btn) return;
  btn.hidden = false;
  const ok = !!(health && health.ok);
  btn.classList.toggle('health-dot--ok', ok);
  btn.classList.toggle('health-dot--error', !ok);
  btn.dataset.state = ok ? 'ok' : 'error';
}

function renderHealthPanel(health) {
  const body = document.getElementById('health-panel-body');
  if (!body) return;
  if (!health) {
    body.innerHTML = '<p>Could not reach the health endpoint.</p>';
    return;
  }
  const missing = ((health.models && health.models.missing) || []).filter(Boolean);
  if (health.ok) {
    body.innerHTML = `<p>Ollama is reachable at <code>${escapeHtml(health.baseUrl)}</code>. All required models are pulled.</p>`;
    return;
  }
  if (missing.length > 0) {
    const items = missing.map((m) => {
      const cmd = `ollama pull ${m}`;
      return `<li><code>${escapeHtml(m)}</code> <button type="button" class="health-copy" data-cmd="${escapeHtml(cmd)}">Copy pull command</button></li>`;
    }).join('');
    body.innerHTML = `
      <p>Ollama is reachable but some models are missing. Run these in a terminal:</p>
      <ul>${items}</ul>`;
  } else {
    body.innerHTML = `
      <p>Ollama is not reachable at <code>${escapeHtml(health.baseUrl)}</code>.</p>
      <p>Run <code>ollama serve</code>, or set <code>OLLAMA_BASE_URL</code> to the correct host.</p>
      ${health.error ? `<pre>${escapeHtml(health.error)}</pre>` : ''}`;
  }
}

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;').replace(/'/g, '&#x27;');
}

const PIPELINE_STAGE_LABELS = {
  publish_started: 'Publishing',
  first_event: 'First event',
  first_signal: 'First signal',
  first_survivor: 'First survivor',
  first_decision: 'First decision',
  stall: 'Waiting',
  error: 'Error',
};

function formatRelativeTime(isoString) {
  const then = new Date(isoString);
  const deltaMs = Date.now() - then.getTime();
  const s = Math.round(deltaMs / 1000);
  if (s < 5) return 'just now';
  if (s < 60) return `${s}s ago`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h ago`;
  const d = Math.round(h / 24);
  return `${d}d ago`;
}

function renderTimelineItem(event) {
  const li = document.createElement('li');
  li.className = `pe-item pe-item--${event.stage}`;
  const label = PIPELINE_STAGE_LABELS[event.stage] || event.stage;
  const time = event.occurredAt ? formatRelativeTime(event.occurredAt) : '';
  const detailTxt = event.detail ? JSON.stringify(event.detail).slice(0, 80) : '';
  li.innerHTML = `
    <span class="pe-stage">${escapeHtml(label)}</span>
    <span class="pe-time">${escapeHtml(time)}</span>
    ${detailTxt ? `<span class="pe-detail">${escapeHtml(detailTxt)}</span>` : ''}
  `;
  return li;
}

function renderChatRemediation(health, activeWorkspace) {
  const el = document.getElementById('chat-remediation');
  const promptEl = document.getElementById('prompt');
  if (!el) return;
  const isDemo = activeWorkspace && activeWorkspace.id === '00000000-0000-0000-0000-000000000d30';
  if (isDemo) {
    el.hidden = true;
    if (promptEl) {
      promptEl.disabled = false;
      promptEl.removeAttribute('aria-describedby');
    }
    return;
  }
  if (!health || health.ok) {
    el.hidden = true;
    if (promptEl) {
      promptEl.disabled = false;
      promptEl.removeAttribute('aria-describedby');
    }
    return;
  }
  const missing = ((health.models && health.models.missing) || []).filter(Boolean);
  const modelName = missing[0] || (health.models && health.models.required && health.models.required[0]) || 'llama3.2:latest';
  const msg = missing.length
    ? `Chat needs the '${escapeHtml(modelName)}' model. Run <code>ollama pull ${escapeHtml(modelName)}</code>, then click retry.`
    : `Chat needs Ollama running. Start it with <code>ollama serve</code>, then click retry.`;
  el.innerHTML = `<p>${msg}</p><button type="button" id="health-retry">Retry</button>`;
  el.hidden = false;
  if (promptEl) {
    promptEl.disabled = true;
    promptEl.setAttribute('aria-describedby', 'chat-remediation');
  }
}

let lastHealth = null;

async function refreshHealth() {
  lastHealth = await pollOllamaHealth();
  renderHealthDot(lastHealth);
  renderChatRemediation(lastHealth, window.activeWorkspace || null);
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

async function loadWorkspaces() {
  const data = await listWorkspaces();
  workspaces = data.items || [];
  if (!workspaceMenu.hidden) renderWorkspaceMenu();
  return workspaces;
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

function appendOllamaFailureCard(errorBody, convId, lastPrompt) {
  const div = document.createElement('div');
  div.className = 'message assistant chat-failure-card';
  const pullCommand = errorBody?.pullCommand || (errorBody?.model ? `ollama pull ${errorBody.model}` : 'ollama serve');
  const message = errorBody?.message || 'Ollama is unavailable for this chat request.';
  div.innerHTML = `
    <p>${escapeHtml(message)}</p>
    <p>Run <code>${escapeHtml(pullCommand)}</code>, then retry.</p>
    <button type="button">Retry</button>`;
  const retryBtn = div.querySelector('button');
  retryBtn.addEventListener('click', async () => {
    retryBtn.disabled = true;
    sendBtn.disabled = true;
    setStatus('Loading...');
    try {
      const assistantMsg = await postMessage(convId, lastPrompt);
      div.remove();
      appendMessage(assistantMsg.role, assistantMsg.content);
      clearStatus();
      await refreshHealth();
    } catch (err) {
      const nextBody = err.body || null;
      if (nextBody?.error === 'ollama_unreachable' || nextBody?.error === 'ollama_model_missing') {
        div.remove();
        handleOllamaChatFailure(nextBody, convId, lastPrompt);
      } else {
        setStatus('Error: ' + err.message);
        retryBtn.disabled = false;
      }
    } finally {
      sendBtn.disabled = false;
    }
  });
  transcript.appendChild(div);
  transcript.scrollTop = transcript.scrollHeight;
}

function handleOllamaChatFailure(errorBody, convId, lastPrompt) {
  const model = errorBody.model || '';
  lastHealth = {
    ok: false,
    baseUrl: errorBody.baseUrl,
    models: { required: model ? [model] : [], missing: model ? [model] : [], available: [] },
    error: errorBody.message,
  };
  renderHealthDot(lastHealth);
  renderChatRemediation(lastHealth, window.activeWorkspace);
  appendOllamaFailureCard(errorBody, convId, lastPrompt);
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

function isDemoWorkspace(ws) {
  return ws && ws.id === DEMO_WORKSPACE_ID;
}

function showToast(message, { durationMs = 5000 } = {}) {
  const container = document.getElementById('toast-container');
  if (!container) return;
  const toast = document.createElement('div');
  toast.className = 'toast toast--error';
  toast.textContent = message;
  container.appendChild(toast);
  setTimeout(() => toast.remove(), durationMs);
}

function renderChatChips(ws) {
  const el = document.getElementById('chat-chips');
  if (!el) return;
  el.hidden = !isDemoWorkspace(ws);
}

function renderWorkspaceLock(ws) {
  const el = document.getElementById('workspace-lock');
  if (!el) return;
  el.hidden = !isDemoWorkspace(ws);
}

async function fetchActivation(workspaceId, track) {
  const res = await fetch(`/api/v1/activation?workspace_id=${encodeURIComponent(workspaceId)}&track=${encodeURIComponent(track)}`);
  if (!res.ok) return null;
  return await res.json();
}

async function markActivation(workspaceId, track, stepKey) {
  await fetch('/api/v1/activation/events', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ workspaceId, track, stepKey }),
  }).catch(() => {});
}

async function dismissActivation(workspaceId, track) {
  await fetch('/api/v1/activation/dismiss', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ workspaceId, track }),
  }).catch(() => {});
}

function trackForWorkspace(ws) {
  return ws && ws.id === DEMO_WORKSPACE_ID_CONST ? 'demo_walkthrough' : 'real_activation';
}

async function renderActivationTracker(ws) {
  const section = document.getElementById('activation-tracker');
  if (!section || !ws) return;
  const track = trackForWorkspace(ws);
  const state = await fetchActivation(ws.id, track);
  if (!activeWorkspace || activeWorkspace.id !== ws.id) return;
  if (!state || state.dismissed) {
    section.hidden = true;
    return;
  }
  section.hidden = false;
  section.dataset.track = track;
  document.getElementById('activation-title').textContent =
    track === 'demo_walkthrough' ? 'Demo walkthrough' : 'Get started';

  const list = document.getElementById('activation-steps');
  list.innerHTML = '';
  const items = Array.isArray(state.items) ? state.items : [];
  const allCompleted = items.length > 0 && items.every((it) => !!it.completedAt);
  items.forEach((it) => {
    const li = document.createElement('li');
    li.className = 'activation-step' + (it.completedAt ? ' activation-step--done' : '');
    li.innerHTML = `<span class="activation-check" aria-hidden="true"></span><span class="activation-label">${escapeHtml(it.label)}</span>`;
    list.appendChild(li);
  });

  const cta = document.getElementById('activation-cta');
  if (allCompleted && track === 'demo_walkthrough') {
    cta.hidden = false;
    list.hidden = true;
  } else {
    cta.hidden = true;
    list.hidden = false;
  }
}

function workspaceLabel(ws) {
  if (workspaceIsClosed(ws)) {
    const date = formatDate(ws.closedAt);
    return ws.name + ' (closed ' + date + ')';
  }
  return ws.name;
}

function setActiveWorkspace(ws) {
  if (workspaceEventSource) {
    workspaceEventSource.close();
    workspaceEventSource = null;
  }

  activeWorkspace = ws;
  window.activeWorkspace = ws;
  localStorage.setItem(ACTIVE_WORKSPACE_KEY, ws.id);

  workspaceNameEl.textContent = workspaceLabel(ws);
  workspaceDomainEl.textContent = ws.domain || '';
  renderChatChips(ws);
  renderWorkspaceLock(ws);
  renderChatRemediation(lastHealth, ws);

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
  renderActivationTracker(ws);
}

// TODO: Wire UI-owned activation marks when the UI has explicit survivor detail,
// approval detail, and audit trail open handlers. The current static UI renders
// survivor and approval lists only, and has no audit-trail view to hook.

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
    const errorBody = err.body || null;
    if (errorBody?.error === 'ollama_unreachable' || errorBody?.error === 'ollama_model_missing') {
      handleOllamaChatFailure(errorBody, convId, content);
      clearStatus();
      return;
    }
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

document.querySelectorAll('#chat-chips .chip').forEach((btn) => {
  btn.addEventListener('click', () => {
    const promptEl = document.getElementById('prompt');
    if (!promptEl) return;
    promptEl.value = btn.dataset.prompt || btn.textContent.trim();
    const form = document.getElementById('chat-form');
    form?.dispatchEvent(new Event('submit', { cancelable: true, bubbles: true }));
  });
});

// Wire activation dismiss button.
document.getElementById('activation-dismiss')?.addEventListener('click', async () => {
  const ws = window.activeWorkspace;
  if (!ws) return;
  const track = trackForWorkspace(ws);
  await dismissActivation(ws.id, track);
  document.getElementById('activation-tracker').hidden = true;
});

// Wire activation CTA.
document.getElementById('activation-cta-primary')?.addEventListener('click', async () => {
  try {
    const created = await createWorkspace('My Workspace', 'generic', 'continuous', null);
    await loadWorkspaces();
    const ws = workspaces.find((item) => item.id === created.id) || created;
    setActiveWorkspace(ws);
  } catch (_err) {
    showToast('Could not create your workspace. Try again.');
  }
});

document.getElementById('activation-cta-secondary')?.addEventListener('click', () => {
  document.getElementById('activation-cta').hidden = true;
  document.getElementById('activation-steps').hidden = false;
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
const acStepProvider      = document.getElementById('ac-step-provider');
const acStepConfigure     = document.getElementById('ac-step-configure');
const acConfigureTitle    = document.getElementById('ac-configure-title');
const acFormFields        = document.getElementById('ac-form-fields');
const acTestResultEl      = document.getElementById('ac-test-result');
const acErrorEl           = document.getElementById('ac-error');
const acBackBtn           = document.getElementById('ac-back-btn');
const acTestBtn           = document.getElementById('ac-test-btn');
const acSubmitBtn         = document.getElementById('ac-submit-btn');
const acProviderCancelBtn = document.getElementById('ac-step-provider-cancel');
const acStepProgress      = document.getElementById('ac-step-progress');
const acProgressHint      = document.getElementById('ac-progress-hint');
const acProgressDoneBtn   = document.getElementById('ac-progress-done');

// connectorStreams: Map<connectorId, Stream[]>
const connectorStreams = new Map();
let workspaceEventSource = null;
let acProgressConnectorId = null;

function setConnectorsStatus(msg, isError) {
  connectorsStatusEl.textContent = msg;
  connectorsStatusEl.style.color = isError ? 'var(--color-error)' : 'var(--color-sidebar-muted)';
}

function clearConnectorsStatus() {
  connectorsStatusEl.textContent = '';
}

async function loadConnectorTimeline(workspaceId, connectorId, listEl) {
  try {
    const res = await fetch(`/api/v1/workspaces/${workspaceId}/events?connector_id=${connectorId}&connectorId=${connectorId}&limit=10`);
    if (!res.ok) return;
    const data = await res.json();
    listEl.innerHTML = '';
    (data.items || []).forEach((ev) => listEl.appendChild(renderTimelineItem(ev)));
  } catch (_err) {
    // Timeline is supplemental; connector cards still render without it.
  }
}

function subscribeWorkspaceEvents(workspaceId) {
  if (workspaceEventSource) {
    workspaceEventSource.close();
    workspaceEventSource = null;
  }
  try {
    const url = `/api/v1/workspaces/${workspaceId}/events/stream`;
    const es = new EventSource(url, { withCredentials: true });
    es.addEventListener('pipeline_event', (msg) => {
      try {
        const ev = JSON.parse(msg.data);
        handlePipelineEvent(ev);
      } catch (_) {}
    });
    es.onerror = () => {
      const indicator = document.getElementById('sse-reconnect');
      if (indicator) indicator.hidden = false;
    };
    es.onopen = () => {
      const indicator = document.getElementById('sse-reconnect');
      if (indicator) indicator.hidden = true;
    };
    workspaceEventSource = es;
  } catch (_err) {
    // EventSource unsupported or blocked; the static timeline fetch is enough to degrade gracefully.
  }
}

function handlePipelineEvent(ev) {
  if (ev.connectorId) {
    const list = document.querySelector(`.conn-timeline[data-connector-id="${ev.connectorId}"]`);
    if (list) {
      list.insertBefore(renderTimelineItem(ev), list.firstChild);
      while (list.children.length > 10) list.removeChild(list.lastChild);
    }
  }

  if (acStepProgress && !acStepProgress.hidden && acProgressConnectorId === ev.connectorId) {
    const li = acStepProgress.querySelector(`li[data-stage="${ev.stage}"]`);
    if (li) li.classList.add('done');
    if (ev.stage === 'error' && acProgressHint) {
      acProgressHint.textContent = `Error: ${ev.detail && ev.detail.error ? ev.detail.error : 'see server logs'}`;
    }
    if (ev.stage === 'stall' && acProgressHint) {
      acProgressHint.textContent = 'Still waiting — this can take up to a minute on a cold Ollama.';
    }
  }
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
  nameSpan.textContent = isDemoWorkspace(activeWorkspace) ? `Sample — ${connector.name}` : connector.name;

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

  const timeline = document.createElement('ul');
  timeline.className = 'conn-timeline';
  timeline.dataset.connectorId = connector.id;
  timeline.setAttribute('aria-label', 'Recent pipeline events');
  li.appendChild(timeline);

  // Streams container (async-filled).
  const streamsContainer = document.createElement('div');
  streamsContainer.className = 'connector-streams';

  const loadingP = document.createElement('p');
  loadingP.className = 'stream-loading';
  loadingP.textContent = 'Loading streams…';
  streamsContainer.appendChild(loadingP);

  li.appendChild(streamsContainer);

  if (activeWorkspace) {
    loadConnectorTimeline(activeWorkspace.id, connector.id, timeline);
  }

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
  subscribeWorkspaceEvents(workspaceId);
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

const ACProvider = {
  nws: {
    title: 'NWS — weather alerts',
    fields: [
      { key: 'lat', label: 'Latitude', type: 'number', step: 'any', placeholder: '46.87', required: true, hint: 'Decimal degrees, -90 to 90.' },
      { key: 'lon', label: 'Longitude', type: 'number', step: 'any', placeholder: '-113.99', required: true, hint: 'Decimal degrees, -180 to 180.' },
      { key: 'pollIntervalSecs', label: 'Poll interval (seconds)', type: 'number', placeholder: '300', required: false },
    ],
    build: (vals) => ({ lat: Number(vals.lat), lon: Number(vals.lon), pollIntervalSecs: vals.pollIntervalSecs ? Number(vals.pollIntervalSecs) : undefined }),
  },
  firms: {
    title: 'FIRMS — fire detections',
    fields: [
      { key: 'mapKey', label: 'MAP_KEY', type: 'password', required: true, hint: 'Request a key at firms.modaps.eosdis.nasa.gov/api.' },
      { key: 'country', label: 'Country code', type: 'text', placeholder: 'USA', required: false },
    ],
    build: (vals) => ({ mapKey: vals.mapKey, country: vals.country || undefined }),
  },
  s3: {
    title: 'S3 — bucket',
    fields: [
      { key: 'endpoint', label: 'Endpoint URL', type: 'url', placeholder: 'https://s3.amazonaws.com', required: true },
      { key: 'bucket', label: 'Bucket', type: 'text', required: true },
      { key: 'prefix', label: 'Prefix', type: 'text', placeholder: 'data/', required: false },
      { key: 'accessKey', label: 'Access key ID', type: 'text', required: false },
      { key: 'secretKey', label: 'Secret access key', type: 'password', required: false },
      { key: 'region', label: 'Region', type: 'text', placeholder: 'us-east-1', required: false },
    ],
    build: (vals) => ({ endpoint: vals.endpoint, bucket: vals.bucket, prefix: vals.prefix || undefined, accessKey: vals.accessKey || undefined, secretKey: vals.secretKey || undefined, region: vals.region || undefined }),
  },
  slack: {
    title: 'Slack — webhook delivery',
    fields: [
      { key: 'webhookUrl', label: 'Webhook URL', type: 'url', required: true, placeholder: 'https://hooks.slack.com/services/...' },
    ],
    build: (vals) => ({ webhookUrl: vals.webhookUrl }),
  },
  irwin: {
    title: 'IRWIN — incident endpoint',
    fields: [
      { key: 'endpoint', label: 'Endpoint URL', type: 'url', required: true },
    ],
    build: (vals) => ({ endpoint: vals.endpoint }),
  },
  custom: {
    title: 'Custom JSON',
    fields: [
      { key: '_kind', label: 'Kind', type: 'text', required: true, placeholder: 'rust_native' },
      { key: '_name', label: 'Name', type: 'text', required: true },
      { key: '_config', label: 'Config (JSON)', type: 'textarea', placeholder: '{}' },
    ],
    build: (vals) => {
      let cfg = {};
      try { cfg = vals._config ? JSON.parse(vals._config) : {}; } catch (_e) { throw new Error('Config is not valid JSON.'); }
      return cfg;
    },
    kindOverride: (vals) => vals._kind,
    nameOverride: (vals) => vals._name,
  },
};

let acState = { kind: null, name: null, provider: null };

function openAddConnectorWizard() {
  acStepProvider.hidden = false;
  acStepConfigure.hidden = true;
  if (acStepProgress) acStepProgress.hidden = true;
  acProgressConnectorId = null;
  acState = { kind: null, name: null, provider: null };
  acErrorEl.hidden = true;
  acErrorEl.textContent = '';
  acTestResultEl.textContent = '';
  acTestResultEl.className = 'ac-test-result';
  acSubmitBtn.disabled = true;
  addConnectorDialog?.showModal?.();
}

function showAcProgress(connector) {
  acProgressConnectorId = connector.id;
  acStepConfigure.hidden = true;
  if (!acStepProgress) return;
  acStepProgress.hidden = false;
  acStepProgress.querySelectorAll('li').forEach((li) => li.classList.remove('done'));
  if (acProgressHint) acProgressHint.textContent = '';
}

function renderACFields(fields) {
  acFormFields.innerHTML = '';
  fields.forEach((f) => {
    const wrap = document.createElement('div');
    wrap.className = 'ac-field';
    const label = document.createElement('label');
    label.setAttribute('for', `ac-f-${f.key}`);
    label.textContent = f.label + (f.required ? ' *' : '');
    wrap.appendChild(label);

    let input;
    if (f.type === 'textarea') {
      input = document.createElement('textarea');
      input.rows = 4;
    } else {
      input = document.createElement('input');
      input.type = f.type || 'text';
      if (f.step) input.step = f.step;
    }
    input.id = `ac-f-${f.key}`;
    input.name = f.key;
    if (f.placeholder) input.placeholder = f.placeholder;
    if (f.required) input.required = true;
    input.addEventListener('input', () => {
      if (acState.name !== 'custom') {
        acSubmitBtn.disabled = true;
        acTestResultEl.textContent = '';
        acTestResultEl.className = 'ac-test-result';
      }
      acErrorEl.hidden = true;
    });
    wrap.appendChild(input);

    if (f.hint) {
      const hint = document.createElement('small');
      hint.className = 'ac-hint';
      hint.textContent = f.hint;
      wrap.appendChild(hint);
    }

    acFormFields.appendChild(wrap);
  });
}

function readACFields() {
  const vals = {};
  document.querySelectorAll('#ac-form-fields [name]').forEach((el) => {
    vals[el.name] = el.value;
  });
  return vals;
}

function buildACPayload() {
  const { kind, name, provider } = acState;
  if (!provider) throw new Error('Choose a provider first.');
  const vals = readACFields();
  const effectiveKind = provider.kindOverride ? provider.kindOverride(vals) : kind;
  const effectiveName = provider.nameOverride ? provider.nameOverride(vals) : name;
  const config = provider.build(vals);
  return { kind: effectiveKind, name: effectiveName, config };
}

document.querySelectorAll('#ac-step-provider .provider-tile').forEach((tile) => {
  tile.addEventListener('click', () => {
    const kind = tile.dataset.kind;
    const name = tile.dataset.name;
    const provider = ACProvider[name];
    if (!provider) return;
    acState = { kind, name, provider };
    acConfigureTitle.textContent = provider.title;
    renderACFields(provider.fields);
    acStepProvider.hidden = true;
    acStepConfigure.hidden = false;
    if (acStepProgress) acStepProgress.hidden = true;
    acSubmitBtn.disabled = name !== 'custom';
    acErrorEl.hidden = true;
    acErrorEl.textContent = '';
    acTestResultEl.textContent = '';
    acTestResultEl.className = 'ac-test-result';
    const firstField = acFormFields.querySelector('[name]');
    if (firstField) firstField.focus();
  });
});

addConnectorBtn.addEventListener('click', openAddConnectorWizard);

acBackBtn?.addEventListener('click', () => {
  acStepProvider.hidden = false;
  acStepConfigure.hidden = true;
  if (acStepProgress) acStepProgress.hidden = true;
});

acProviderCancelBtn?.addEventListener('click', () => {
  addConnectorDialog?.close?.();
});

acProgressDoneBtn?.addEventListener('click', () => {
  addConnectorDialog?.close?.();
  acProgressConnectorId = null;
});

addConnectorDialog.addEventListener('close', () => {
  addConnectorBtn.focus();
});

addConnectorDialog.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') {
    addConnectorDialog.close();
  }
});

acTestBtn?.addEventListener('click', async () => {
  if (!acStepConfigure.reportValidity()) return;
  acTestResultEl.textContent = 'Testing…';
  acTestResultEl.className = 'ac-test-result';
  acErrorEl.hidden = true;
  try {
    const payload = buildACPayload();
    const res = await fetch('/api/v1/connectors/validate', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    });
    const body = await res.json();
    if (res.ok && body.ok) {
      acTestResultEl.textContent = `✓ ${JSON.stringify(body.sample || {})}`;
      acTestResultEl.className = 'ac-test-result ac-test-result--ok';
      acSubmitBtn.disabled = false;
    } else {
      const hint = body.hint ? ` — ${body.hint}` : '';
      acTestResultEl.textContent = `✗ ${body.message || body.error || 'Validation failed'}${hint}`;
      acTestResultEl.className = 'ac-test-result ac-test-result--error';
      acSubmitBtn.disabled = true;
    }
  } catch (err) {
    acTestResultEl.textContent = `✗ ${err.message || String(err)}`;
    acTestResultEl.className = 'ac-test-result ac-test-result--error';
    acSubmitBtn.disabled = true;
  }
});

async function submitAddConnectorWizard(e) {
  e.preventDefault();
  if (!acStepConfigure.reportValidity()) return;
  acSubmitBtn.disabled = true;
  acErrorEl.hidden = true;

  try {
    const ws = window.activeWorkspace;
    if (!ws) throw new Error('No active workspace.');
    const payload = buildACPayload();
    const res = await fetch(`/api/v1/workspaces/${ws.id}/connectors`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    });
    if (!res.ok) {
      const body = await res.json().catch(() => ({}));
      const hint = body.hint ? ` — ${body.hint}` : '';
      throw new Error(`${body.message || body.error || res.statusText}${hint}`);
    }
    const body = await res.json();
    const connector = body.connector || body;
    showAcProgress(connector);
    if (typeof loadConnectors === 'function') loadConnectors(ws.id);
    (body.events || body.pipelineEvents || []).forEach((ev) => handlePipelineEvent(ev));
  } catch (err) {
    acErrorEl.textContent = err.message || String(err);
    acErrorEl.hidden = false;
    acSubmitBtn.disabled = false;
  }
}

acStepConfigure?.addEventListener('submit', submitAddConnectorWizard);

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
    await loadWorkspaces();

    // Restore last-active workspace from localStorage, fall back to first.
    const savedId = localStorage.getItem(ACTIVE_WORKSPACE_KEY);
    const restored = savedId ? workspaces.find((w) => w.id === savedId) : null;
    const initial = restored || workspaces[0] || null;

    if (initial) {
      // Call directly (not setActiveWorkspace) to avoid clearing transcript before convs load.
      activeWorkspace = initial;
      window.activeWorkspace = initial;
      localStorage.setItem(ACTIVE_WORKSPACE_KEY, initial.id);
      workspaceNameEl.textContent = workspaceLabel(initial);
      workspaceDomainEl.textContent = initial.domain || '';
      renderChatChips(initial);
      renderWorkspaceLock(initial);
      const isClosed = workspaceIsClosed(initial);
      newChatBtn.hidden = isClosed;
      workspaceClosedNotice.hidden = !isClosed;
      // Seed the pending approvals badge.
      updateApprovalsBadge(initial.id);
      renderActivationTracker(initial);
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

  await refreshHealth();
  setInterval(refreshHealth, 15000);
})();

const healthDot = document.getElementById('health-dot');
const healthPanel = document.getElementById('health-panel');
const healthPanelClose = document.getElementById('health-panel-close');
const chatRemediation = document.getElementById('chat-remediation');

if (healthDot && healthPanel) {
  healthDot.addEventListener('click', () => {
    const willShow = healthPanel.hidden;
    if (willShow) renderHealthPanel(lastHealth);
    healthPanel.hidden = !willShow;
  });
}

if (healthPanelClose && healthPanel) {
  healthPanelClose.addEventListener('click', () => {
    healthPanel.hidden = true;
  });
}

if (healthPanel) {
  healthPanel.addEventListener('click', async (e) => {
    const copyBtn = e.target.closest('.health-copy');
    if (!copyBtn) return;
    await navigator.clipboard.writeText(copyBtn.dataset.cmd || '');
  });
}

if (chatRemediation) {
  chatRemediation.addEventListener('click', (e) => {
    if (e.target && e.target.id === 'health-retry') {
      refreshHealth();
    }
  });
}

/* ── MCP endpoint copy button (Phase 11) ── */
const mcpCopyBtn = document.getElementById('mcp-copy-btn');
const mcpUrlEl   = document.getElementById('mcp-url');

function currentMcpUrl() {
  return `${window.location.protocol}//${window.location.host}/mcp`;
}

function buildCursorDeepLink(mcpUrl) {
  const config = { name: 'ione', transport: 'http', url: mcpUrl };
  const encoded = btoa(JSON.stringify(config));
  return `cursor://anysphere.cursor-deeplink/mcp/install?name=ione&config=${encoded}`;
}

function buildVsCodeDeepLink(mcpUrl) {
  const config = { name: 'ione', url: mcpUrl, type: 'http' };
  const encoded = encodeURIComponent(JSON.stringify(config));
  return `vscode:mcp/install?${encoded}`;
}

function buildRawJsonConfig(mcpUrl) {
  return JSON.stringify({
    mcpServers: { ione: { url: mcpUrl, type: 'http' } },
  }, null, 2);
}

async function loadMcpClients() {
  try {
    const res = await fetch('/api/v1/mcp/clients');
    if (!res.ok) return;
    const data = await res.json();
    const body = document.getElementById('mcp-clients-body');
    if (!body) return;
    if (!data.items || data.items.length === 0) {
      body.innerHTML = '<tr><td colspan="4" class="mcp-clients-empty">No clients connected yet.</td></tr>';
      return;
    }
    body.innerHTML = '';
    data.items.forEach((c) => {
      const tr = document.createElement('tr');
      tr.innerHTML = `
        <td>${escapeHtml(c.displayName)}</td>
        <td>${c.createdAt ? new Date(c.createdAt).toLocaleString() : '—'}</td>
        <td>${c.lastSeenAt ? new Date(c.lastSeenAt).toLocaleString() : '—'}</td>
        <td><button type="button" class="mcp-client-revoke" data-client-row-id="${escapeHtml(c.id)}">Revoke</button></td>
      `;
      body.appendChild(tr);
    });
    body.querySelectorAll('.mcp-client-revoke').forEach((btn) => {
      btn.addEventListener('click', async () => {
        const id = btn.dataset.clientRowId;
        if (!id) return;
        await fetch(`/api/v1/mcp/clients/${id}`, { method: 'DELETE' }).catch(() => {});
        loadMcpClients();
      });
    });
  } catch (_err) { /* ignore */ }
}

let mcpClientsInterval = null;

function openMcpConnectDialog() {
  const dialog = document.getElementById('mcp-connect-dialog');
  const mcpUrl = currentMcpUrl();

  const cursor = document.getElementById('mcp-tile-cursor');
  if (cursor) cursor.href = buildCursorDeepLink(mcpUrl);
  const vscode = document.getElementById('mcp-tile-vscode');
  if (vscode) vscode.href = buildVsCodeDeepLink(mcpUrl);
  const claudeDesktopUrl = document.getElementById('mcp-tile-claude-desktop-url');
  if (claudeDesktopUrl) claudeDesktopUrl.textContent = mcpUrl;
  const claudeCodeCmd = document.getElementById('mcp-tile-claude-code-cmd');
  if (claudeCodeCmd) claudeCodeCmd.textContent = `claude mcp add --transport http ione ${mcpUrl}`;
  const rawJson = document.getElementById('mcp-tile-raw-json');
  if (rawJson) rawJson.textContent = buildRawJsonConfig(mcpUrl);

  loadMcpClients();
  if (mcpClientsInterval) clearInterval(mcpClientsInterval);
  mcpClientsInterval = setInterval(loadMcpClients, 15000);
  dialog?.showModal?.();
}

function closeMcpConnectDialog() {
  const dialog = document.getElementById('mcp-connect-dialog');
  dialog?.close?.();
  if (mcpClientsInterval) { clearInterval(mcpClientsInterval); mcpClientsInterval = null; }
}

if (mcpCopyBtn && mcpUrlEl) {
  // Use window.location to derive the correct host/port at runtime.
  const mcpUrl = currentMcpUrl();
  mcpUrlEl.textContent = mcpUrl;

  mcpCopyBtn.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(mcpUrl);
      const original = mcpCopyBtn.textContent;
      mcpCopyBtn.textContent = 'Copied!';
      setTimeout(() => { mcpCopyBtn.textContent = original; }, 1500);
    } catch (_) {
      // Fallback: select the text
      const range = document.createRange();
      range.selectNode(mcpUrlEl);
      window.getSelection().removeAllRanges();
      window.getSelection().addRange(range);
    }
  });
}

document.getElementById('mcp-connect-open-btn')?.addEventListener('click', openMcpConnectDialog);
document.getElementById('mcp-connect-close')?.addEventListener('click', closeMcpConnectDialog);
document.getElementById('mcp-connect-dialog')?.addEventListener('close', () => {
  if (mcpClientsInterval) { clearInterval(mcpClientsInterval); mcpClientsInterval = null; }
});

document.addEventListener('click', (ev) => {
  const btn = ev.target.closest?.('.mcp-copy-btn');
  if (!btn || !btn.dataset.copyTarget) return;
  const target = btn.dataset.copyTarget;
  const el = document.getElementById(target);
  if (!el) return;
  const text = el.textContent || '';
  navigator.clipboard?.writeText(text);
  const orig = btn.textContent;
  btn.textContent = 'Copied';
  setTimeout(() => { btn.textContent = orig; }, 1400);
});

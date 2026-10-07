const root = document.querySelector('#app');
let auth = {}, state = null, route = 'activity', step = 1, filter = 'all', removeEmail = null;
let toastTimer, refreshing = false;
const esc = value => String(value ?? '').replace(/[&<>"']/g, char => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[char]));
const icon = (name, cls = '') => `<img class="icon ${cls}" src="/assets/icons/${name}.svg" alt="" aria-hidden="true">`;
const statuses = {queued:'Queued',preparing:'Preparing',submitting:'Sending',submitted:'Sent to printer',completed:'Completed',failed:'Failed',blocked:'Blocked',cancelled:'Cancelled',uncertain:'Check printer'};

async function api(path, method = 'GET', body) {
  const response = await fetch(`/api${path}`, {method, credentials:'same-origin', headers:{'Content-Type':'application/json','X-Paperboy-CSRF':auth.csrf || ''}, body: body === undefined ? undefined : JSON.stringify(body)});
  const result = await response.json();
  if (!response.ok) {
    if (response.status === 401 && !path.startsWith('/auth/')) { auth.authenticated = false; render(); }
    throw new Error(result.detail || 'Something went wrong. Try again.');
  }
  return result;
}
function toast(message) {
  const el = document.querySelector('#toast');
  el.textContent = message;
  el.classList.add('visible');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.classList.remove('visible'), 4500);
}
function fail(message, form) {
  const target = form?.querySelector('.error') || document.querySelector('#page-error');
  if (target) { target.textContent = message; target.focus(); }
  else toast(message);
}
function heading(title, subtitle, action = '') {
  return `<div class="page-heading"><div><h1 tabindex="-1">${title}</h1><p>${subtitle}</p></div>${action}</div><div id="page-error" class="error" role="alert" tabindex="-1"></div>`;
}
function shell(content, navigation = true) {
  const nav = navigation ? `<nav class="nav" aria-label="Main navigation">${[['activity','Activity'],['people','People'],['settings','Settings']].map(([id,label]) => `<button data-action="navigate" data-route="${id}" ${route === id ? 'aria-current="page"' : ''}>${label}</button>`).join('')}</nav>` : '<span class="top-meta">Your home printer.</span>';
  root.innerHTML = `${auth.demo ? '<div class="preview-banner">Design preview · Sample data · Printing is disabled</div>' : ''}<header class="topbar"><div class="topbar-inner"><a class="brand" href="#activity" aria-label="Paperboy home">${icon('newspaper')}Paperboy</a>${nav}</div></header>${content}`;
}
function renderAuth() {
  const initial = !auth.initialized;
  shell(`<main id="main" class="auth-page"><h1>${initial ? 'Email it. Print it.' : 'Welcome home.'}</h1><p class="intro">${initial ? 'Connect your inbox to your home printer. Give your family an easier way to print.' : 'Sign in to manage your printer and the people who can use it.'}</p><form class="auth-form" data-form="${initial ? 'owner' : 'login'}">${initial ? '<div class="field"><label for="setup-code">Setup code</label><input id="setup-code" name="setup_code" required autocomplete="off" spellcheck="false"><small>Find the code in your Paperboy Docker logs.</small></div>' : ''}<div class="field"><label for="password">${initial ? 'Choose an owner password' : 'Owner password'}</label><input id="password" name="password" type="password" minlength="10" maxlength="128" required autocomplete="${initial ? 'new-password' : 'current-password'}">${initial ? '<small>At least 10 characters. This protects your printer settings.</small>' : ''}</div><div class="error" role="alert" tabindex="-1"></div><button class="primary" type="submit">${initial ? 'Set up Paperboy' : 'Sign in'}${icon('arrow-right')}</button></form><p class="auth-footer">A small app for your home printer.</p></main>`, false);
}
function emailForm(setup = false) {
  const settings = state.settings;
  return `<form data-form="email"><div class="field"><label for="api-key">Resend API key</label><input id="api-key" name="api_key" type="password" ${settings.api_key_set ? '' : 'required'} autocomplete="off" placeholder="${settings.api_key_set ? 'Connected · Leave blank to keep your key' : 're_…'}"><small>Your key is encrypted and saved on this device.</small></div><div class="field"><label for="inbox">Printing email address</label><input id="inbox" name="inbox" type="email" value="${esc(settings.inbox)}" required autocomplete="off" placeholder="print@your-id.resend.app"><small>Use an address on your Resend receiving domain.</small></div><div class="error" role="alert" tabindex="-1"></div><div class="form-actions"><button class="primary" type="submit">${setup ? 'Connect Resend' : 'Save connection'}${setup ? icon('arrow-right') : ''}</button></div></form>`;
}
function senderForm(setup = false) {
  return `<form class="${setup ? '' : 'surface inline-form'}" data-form="sender"><div class="field-grid"><div class="field"><label for="person-name">Name</label><input id="person-name" name="name" placeholder="Alex" maxlength="80" required autocomplete="given-name"></div><div class="field"><label for="person-email">Email address</label><input id="person-email" name="email" placeholder="alex@example.com" type="email" required autocomplete="email"></div></div><div class="error" role="alert" tabindex="-1"></div><div class="form-actions">${!setup ? '<button type="button" data-action="close-add">Cancel</button>' : ''}<button class="${setup ? 'secondary' : 'primary'}" type="submit">${icon('plus')}Add person</button></div></form>`;
}
function peopleRows(setup = false) {
  return state.senders.map(person => `<div class="person-row"><div class="person"><span class="avatar" aria-hidden="true">${esc(person.name.slice(0,1).toUpperCase())}</span><div><strong>${esc(person.name)}</strong><small>${esc(person.email)}</small></div></div><div class="person-actions">${removeEmail === person.email ? `<span>Remove access?</span><button class="danger" data-action="confirm-remove" data-email="${esc(person.email)}">Remove</button><button data-action="keep-person">Keep</button>` : `<button class="${setup ? '' : 'danger'}" data-action="remove-person" data-email="${esc(person.email)}" aria-label="Remove ${esc(person.name)}">${setup ? icon('x') : 'Remove'}</button>`}</div></div>`).join('');
}
function manualPrinter() {
  return `<details><summary>Add by IP address</summary><form data-form="printer"><div class="field"><label for="printer-name">Printer name</label><input name="name" id="printer-name" value="Home printer" required maxlength="100"></div><div class="field"><label for="printer-uri">Printer address</label><input name="uri" id="printer-uri" placeholder="ipp://192.168.1.50:631/ipp/print" required spellcheck="false"><small>Find the IP address in your printer’s network settings. AirPrint and IPP printers are supported.</small></div><div class="error" role="alert" tabindex="-1"></div><div class="form-actions"><button type="submit" class="primary">Connect printer</button></div></form></details>`;
}
function renderSetup() {
  const settings = state.settings;
  const titles = ['Connect your inbox','Find your printer','Make it a family thing'];
  const descriptions = ['Paperboy checks for new files every 30 seconds. No public web address needed.','Choose the printer connected to your home network.','Only these email addresses can send files to your printer. Add yourself first.'];
  let content;
  if (step === 1) content = emailForm(true) + '<p class="help-link"><a href="https://resend.com/emails" target="_blank" rel="noreferrer">Open Resend</a> to find your API key and receiving address.</p>';
  else if (step === 2) content = `${settings.printer ? `<div class="notice">Connected to ${esc(settings.printer.name)}.</div>` : ''}<div id="discovery-results"><p class="small">Looking for printers on your network…</p></div><button data-action="discover" class="link">${icon('arrow-clockwise')}Search again</button>${manualPrinter()}<div class="form-actions"><button data-action="back-step">Back</button><button class="primary" data-action="next-step" ${settings.printer ? '' : 'disabled'}>Use this printer${icon('arrow-right')}</button></div>`;
  else content = `${senderForm(true)}<div class="people-list">${peopleRows(true)}</div><div class="form-actions"><button data-action="back-step">Back</button><button class="primary" data-action="finish" ${state.senders.length ? '' : 'disabled'}>Start Paperboy${icon('arrow-right')}</button></div>`;
  shell(`<main id="main" class="setup-page"><div class="setup-intro"><h1>A little setup.<br>Then just email.</h1><p>Your family’s files, straight to your home printer.</p></div><div class="setup-layout"><ol class="step-list" aria-label="Setup progress">${['Email','Printer','People'].map((label,index) => `<li class="${step === index + 1 ? 'current' : step > index + 1 ? 'done' : ''}" ${step === index + 1 ? 'aria-current="step"' : ''}><span class="step-number">${step > index + 1 ? '✓' : index + 1}</span>${label}</li>`).join('')}</ol><section class="surface setup-panel"><h2>${titles[step - 1]}</h2><p>${descriptions[step - 1]}</p><div id="page-error" class="error" role="alert" tabindex="-1"></div>${content}</section></div></main>`, false);
  if (step === 2) discover();
}
function personName(email) { return state.senders.find(person => person.email === email)?.name || email; }
function when(value) {
  const date = new Date(value);
  const today = new Date().toDateString() === date.toDateString();
  return today ? date.toLocaleTimeString([], {hour:'numeric',minute:'2-digit'}) : date.toLocaleDateString([], {month:'short',day:'numeric'});
}
function jobsMarkup() {
  if (filter === 'blocked') {
    if (!state.blocked.length) return `<div class="empty">${icon('shield-check')}<h3>No blocked messages</h3><p>Messages from unapproved or unverified senders will appear here.</p></div>`;
    return state.blocked.map(message => `<div class="job-row"><div class="file">${icon('shield-check','muted-icon')}<span class="file-name">${esc(message.subject || 'Untitled email')}</span></div><span class="job-sender">${esc(message.sender)}</span><span class="job-when small">${when(message.created_at)}</span><span class="job-status"><span class="status blocked">Blocked</span></span><p class="job-reason">${esc(message.reason)}</p></div>`).join('');
  }
  const jobs = state.jobs.filter(job => filter === 'all' || (filter === 'active' ? ['queued','preparing','submitting','submitted'].includes(job.status) : ['failed','uncertain','blocked'].includes(job.status)));
  if (!jobs.length) return `<div class="empty">${icon(filter === 'issues' ? 'check-circle' : 'tray')}<h3>${filter === 'all' ? 'Waiting for the first email' : filter === 'active' ? 'The queue is clear' : 'Everything looks good'}</h3><p>${filter === 'all' ? 'Send an attachment from an approved address. Your files will appear here.' : filter === 'active' ? 'New files will appear here while they’re being prepared and printed.' : 'Files that need your attention will appear here.'}</p></div>`;
  return `<div class="job-header" aria-hidden="true"><span>File</span><span>From</span><span>Received</span><span class="job-status">Status</span></div>${jobs.map(job => `<article class="job-row" aria-label="${esc(job.filename)}, ${statuses[job.status]}"><div class="file"><span class="file-glyph">${icon('file-text','muted-icon')}</span><div><span class="file-name">${esc(job.filename)}</span><small>${job.pages ? `${job.pages} ${job.pages === 1 ? 'page' : 'pages'}` : 'Attachment'}</small></div></div><span class="job-sender">${esc(personName(job.sender))}</span><time class="job-when small" datetime="${esc(job.created_at)}">${when(job.created_at)}</time><div class="job-status"><span class="status ${job.status}">${job.status === 'completed' ? '✓ ' : ''}${statuses[job.status]}</span>${job.status === 'failed' && !job.cups_id && !job.printer_queue ? `<button class="link row-action" data-action="retry" data-id="${job.id}">Try again</button>` : ['queued','preparing','submitted'].includes(job.status) ? `<button class="row-action" data-action="cancel-job" data-id="${job.id}">Cancel</button>` : ''}</div>${job.reason ? `<p class="job-reason">${esc(job.reason)}</p>` : ''}</article>`).join('')}`;
}
function printerStateMarkup() {
  const settings = state.settings;
  return `<span class="status-dot ${state.printer_status.state}" aria-hidden="true"></span>${settings.paused ? 'Printing paused' : esc(state.printer_status.message)}`;
}
function renderActivity() {
  const settings = state.settings;
  shell(`<main id="main" class="page">${heading('Activity','Email a file. Paperboy takes it from here.')}<section class="surface connection" aria-label="Printing connection"><div class="mail-address"><div><div class="eyebrow">${icon('envelope-simple','muted-icon')}Your printing address</div><div class="email-address">${esc(settings.inbox)}</div></div><button class="secondary" data-action="copy">${icon('copy')}Copy address</button></div><div class="connection-row"><div class="printer-label">${icon('printer','muted-icon')}<div><strong>${esc(settings.printer?.name || 'No printer selected')}</strong><small id="printer-state">${printerStateMarkup()}</small></div></div><button id="pause-button" data-action="pause">${icon(settings.paused ? 'play' : 'pause')}${settings.paused ? 'Resume printing' : 'Pause printing'}</button></div></section><div id="sync-error">${settings.sync_error ? `<div class="notice" role="status">${esc(settings.sync_error)}</div>` : ''}</div><div class="section-heading"><h2>Recent prints</h2><button class="link" data-action="sync">${icon('arrow-clockwise')}Check mail</button></div><section class="activity-list" aria-label="Print activity"><div class="filters" aria-label="Filter activity">${[['all','All files'],['active','In progress'],['issues','Needs attention'],['blocked','Blocked']].map(([id,label]) => `<button data-action="filter" data-filter="${id}" aria-pressed="${filter === id}">${label}</button>`).join('')}</div><div id="jobs">${jobsMarkup()}</div></section><footer class="footer"><span class="privacy">${icon('shield-check')}Only approved people can print.</span><span id="sync-time">${settings.last_sync ? `Last checked ${when(settings.last_sync)}` : 'Waiting to check your inbox'}</span></footer></main>`);
}
function renderPeople(showForm = false) {
  shell(`<main id="main" class="page">${heading('People','Choose who can print at home.',`<button class="primary" data-action="add-person">${icon('plus')}Add person</button>`)}${showForm ? senderForm() : ''}<section class="people-surface"><div class="people-list">${peopleRows()}</div>${state.senders.length ? '' : `<div class="empty">${icon('users')}<h3>No approved senders</h3><p>Add an email address to let someone send files to your printer.</p></div>`}</section><footer class="footer"><span class="privacy">${icon('shield-check')}Unapproved addresses are blocked before files are downloaded.</span><span>${state.senders.length} ${state.senders.length === 1 ? 'person' : 'people'} approved</span></footer></main>`);
}
function selectField(id, label, choices, selected) {
  return `<div class="field"><label for="${id}">${label}</label><select name="${id}" id="${id}">${choices.map(([value,text]) => `<option value="${value}" ${value === selected ? 'selected' : ''}>${text}</option>`).join('')}</select></div>`;
}
function numberField(name, label, min, max, value) {
  return `<div class="field"><label for="${name}">${label}</label><input id="${name}" name="${name}" type="number" min="${min}" max="${max}" value="${value}" required></div>`;
}
function renderSettings() {
  const s = state.settings;
  shell(`<main id="main" class="page">${heading('Settings','A few things that keep Paperboy running.')}<section class="settings-layout"><div class="settings-label"><h2>Email</h2><p>Your connection to Resend.</p></div><div class="surface settings-content"><div class="compact-row"><div><h3>${s.api_key_set ? 'Resend connected' : 'Connect Resend'}</h3><p>${esc(s.inbox)}</p></div>${icon('check-circle','muted-icon')}</div><details><summary>Edit email connection</summary>${emailForm()}</details></div></section><section class="settings-layout"><div class="settings-label"><h2>Printer</h2><p>On the same network as Paperboy.</p></div><div class="surface settings-content"><div class="compact-row"><div class="printer-label">${icon('printer','muted-icon')}<div><h3>${esc(s.printer?.name || 'Choose a printer')}</h3><p>${esc(state.printer_status.message)}</p></div></div><button class="secondary" data-action="test-print">Print test page</button></div><details><summary>Change printer</summary><div class="details-content"><div id="discovery-results"></div><button class="link" data-action="discover">${icon('arrow-clockwise')}Find printers</button>${manualPrinter()}</div></details></div></section><section class="settings-layout"><div class="settings-label"><h2>Print defaults</h2><p>Applied to every attachment.</p></div><form class="surface settings-content" data-form="preferences"><div class="preferences">${selectField('paper','Paper size',[['Letter','US Letter'],['A4','A4']],s.paper)}${selectField('color','Color',[['monochrome','Black & white'],['color','Color']],s.color)}${selectField('sides','Sides',[['one-sided','One-sided'],['two-sided-long-edge','Two-sided']],s.sides)}${numberField('retention_days','Activity history (days)',1,90,s.retention_days)}</div><details><summary>Printing limits</summary><div class="details-content preferences">${numberField('max_pages','Pages per file',1,100,s.max_pages)}${numberField('max_size_mb','File size (MB)',1,25,s.max_size_mb)}${numberField('daily_limit','Files per person, per day',1,100,s.daily_limit)}</div></details><div class="error" role="alert" tabindex="-1"></div><div class="form-actions"><button type="submit" class="primary">Save defaults</button></div></form></section><section class="settings-layout"><div class="settings-label"><h2>Files & privacy</h2></div><div class="settings-content"><h3>Converted at home.</h3><p class="supported">PDFs, Word documents, spreadsheets, presentations, text files, and common images. Password-protected files, archives, and executables cannot be printed. Export other formats to PDF first.</p><p class="supported">Resend receives your emails. Paperboy downloads approved attachments, converts them locally, then deletes its working files. Activity details are kept for ${s.retention_days} days. The print service may retain delivery records.</p></div></section><footer class="footer"><a href="https://github.com/thekozugroup/Paperboy" target="_blank" rel="noreferrer">Source code</a><button data-action="logout" class="link">${icon('sign-out')}Sign out</button></footer></main>`);
}
function render() {
  if (!auth.authenticated) return renderAuth();
  if (!state) return;
  if (!state.settings.setup_complete) return renderSetup();
  if (route === 'people') renderPeople();
  else if (route === 'settings') renderSettings();
  else renderActivity();
}
async function refresh(paint = true) { state = await api('/state'); if (paint) render(); }
async function discover() {
  const target = document.querySelector('#discovery-results');
  if (!target) return;
  target.innerHTML = '<p class="small" role="status">Looking for printers on your network…</p>';
  try {
    const result = await api('/printers/discover');
    if (!target.isConnected) return;
    target.innerHTML = result.printers.length ? result.printers.map(printer => `<button class="printer-choice" data-action="pair" data-name="${esc(printer.name)}" data-uri="${esc(printer.uri)}"><span class="printer-label">${icon('printer')}<span><strong>${esc(printer.name)}</strong><small>${esc(printer.location || 'On your network')}</small></span></span><span class="small">Connect</span></button>`).join('') : `<p class="small">${esc(result.message)}</p>`;
  } catch (error) { if (target.isConnected) target.textContent = error.message; }
}
async function copyAddress() {
  try { await navigator.clipboard.writeText(state.settings.inbox); toast('Printing address copied.'); }
  catch { toast(`Printing address: ${state.settings.inbox}`); }
}
root.addEventListener('submit', async event => {
  event.preventDefault();
  const form = event.target;
  const button = form.querySelector('button[type="submit"]');
  const original = button.innerHTML;
  button.disabled = true; button.textContent = 'Saving…';
  const data = Object.fromEntries(new FormData(form));
  const error = form.querySelector('.error'); if (error) error.textContent = '';
  try {
    switch (form.dataset.form) {
      case 'owner': auth = {...auth, ...await api('/auth/setup','POST',data), initialized:true}; await refresh(); break;
      case 'login': auth = {...auth, ...await api('/auth/login','POST',data)}; await refresh(); break;
      case 'email': button.textContent = 'Connecting…'; await api('/settings/email','PUT',data); if (!state.settings.setup_complete) step = 2; await refresh(); toast('Resend connected.'); break;
      case 'sender': await api('/senders','POST',data); await refresh(); toast(`${data.name} can now print.`); break;
      case 'printer': button.textContent = 'Connecting…'; await api('/printer','PUT',data); await refresh(); toast('Printer connected.'); break;
      case 'preferences': for (const key of ['max_pages','max_size_mb','daily_limit','retention_days']) data[key] = Number(data[key]); await api('/settings/preferences','PUT',data); await refresh(); toast('Print defaults saved.'); break;
    }
  } catch (error) { fail(error.message,form); }
  finally { if (button.isConnected) { button.disabled = false; button.innerHTML = original; } }
});
root.addEventListener('click', async event => {
  const button = event.target.closest('button[data-action]');
  if (!button || button.disabled) return;
  const action = button.dataset.action;
  const original = button.innerHTML;
  try {
    if (action === 'navigate') { route = button.dataset.route; location.hash = route; render(); document.querySelector('h1')?.focus(); return; }
    if (action === 'filter') { filter = button.dataset.filter; document.querySelectorAll('[data-action="filter"]').forEach(el => el.setAttribute('aria-pressed',String(el === button))); document.querySelector('#jobs').innerHTML = jobsMarkup(); return; }
    if (action === 'copy') return await copyAddress();
    if (action === 'discover') return await discover();
    if (action === 'back-step') { step--; render(); return; }
    if (action === 'next-step') { step++; render(); return; }
    if (action === 'add-person') { renderPeople(true); document.querySelector('#person-name').focus(); return; }
    if (action === 'close-add') { renderPeople(); return; }
    if (action === 'remove-person') { removeEmail = button.dataset.email; render(); return; }
    if (action === 'keep-person') { removeEmail = null; render(); return; }
    button.disabled = true;
    switch (action) {
      case 'pair': button.textContent = 'Connecting…'; await api('/printer','PUT',{name:button.dataset.name,uri:button.dataset.uri}); await refresh(); toast('Printer connected.'); break;
      case 'finish': await api('/setup/complete','POST'); await refresh(); toast('Paperboy is ready. Send your first attachment.'); break;
      case 'confirm-remove': await api(`/senders/${encodeURIComponent(button.dataset.email)}`,'DELETE'); removeEmail = null; await refresh(); toast('Printing access removed.'); break;
      case 'pause': await api('/printing','PUT',{paused:!state.settings.paused}); await refresh(); toast(state.settings.paused ? 'Printing paused. New files will wait in the queue.' : 'Printing resumed.'); break;
      case 'sync': button.textContent = 'Checking…'; const result = await api('/sync','POST'); await refresh(); if (result.error) fail(result.error); else toast('Inbox checked.'); break;
      case 'test-print': await api('/printer/test','POST'); route = 'activity'; location.hash = route; await refresh(); toast('Test page added to the queue.'); break;
      case 'retry': await api(`/jobs/${button.dataset.id}/retry`,'POST'); await refresh(); toast('File added back to the queue.'); break;
      case 'cancel-job': await api(`/jobs/${button.dataset.id}/cancel`,'POST'); await refresh(); toast('Cancellation requested. Pages already printed cannot be recalled.'); break;
      case 'logout': await api('/auth/logout','POST'); auth.authenticated = false; state = null; render(); break;
    }
  } catch (error) { fail(error.message); }
  finally { if (button.isConnected) { button.disabled = false; button.innerHTML = original; } }
});
window.addEventListener('hashchange', () => { const next = location.hash.slice(1); if (['activity','people','settings'].includes(next)) { route = next; render(); } });
async function init() {
  try {
    auth = await api('/auth/status');
    route = ['activity','people','settings'].includes(location.hash.slice(1)) ? location.hash.slice(1) : 'activity';
    if (auth.authenticated) await refresh(); else render();
  } catch (error) { root.innerHTML = `<main id="main" class="auth-page"><h1>Paperboy is unavailable.</h1><p class="intro">${esc(error.message)}</p><button class="primary" data-action="reconnect">Try again</button></main>`; }
}
root.addEventListener('click', event => { if (event.target.closest('[data-action="reconnect"]')) init(); });
setInterval(async () => {
  if (!auth.authenticated || !state?.settings.setup_complete || document.hidden || refreshing || route !== 'activity') return;
  refreshing = true;
  try {
    await refresh(false);
    const jobs = document.querySelector('#jobs');
    if (jobs && !jobs.contains(document.activeElement)) jobs.innerHTML = jobsMarkup();
    const status = document.querySelector('#printer-state'); if (status) status.innerHTML = printerStateMarkup();
    const sync = document.querySelector('#sync-time'); if (sync && state.settings.last_sync) sync.textContent = `Last checked ${when(state.settings.last_sync)}`;
    const error = document.querySelector('#sync-error'); if (error) error.innerHTML = state.settings.sync_error ? `<div class="notice" role="status">${esc(state.settings.sync_error)}</div>` : '';
  } catch (error) { fail(error.message); }
  finally { refreshing = false; }
}, 5000);
init();

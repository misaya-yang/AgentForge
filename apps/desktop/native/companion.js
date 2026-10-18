/* Only shipped static code runs in the capability-bearing companion window.
 * Remote strings are text nodes. There is no HTML sink, storage or token bridge. */
'use strict';
const $ = id => document.getElementById(id);
const invoke = (command, args = {}) => window.__TAURI__.core.invoke(command, args);
let busy = false;
async function run(command, args) {
  if (busy) return;
  busy = true;
  try { const value = await invoke(command, args); $('output').textContent = JSON.stringify(value, null, 2); return value; }
  catch { $('output').textContent = 'Operation failed or its outcome is unknown. Refresh status before trying again.'; }
  finally { busy = false; }
}
$('gateway-form').onsubmit = async e => { e.preventDefault(); await run('configure_gateway', {origin: $('gateway').value}); };
$('console').onclick = () => run('open_console', {offline: false});
$('offline').onclick = () => run('open_console', {offline: true});
$('status').onclick = () => run('desktop_status');
$('login').onclick = async () => { const value = await run('begin_login'); if(value){ $('challenge').textContent = `Open ${value.verification_uri} in your browser and enter ${value.user_code}. Expires in ${value.expires_in} seconds.`; $('finish').disabled = false; } };
$('finish').onclick = async () => { $('finish').disabled = true; await run('finish_login'); $('challenge').textContent = ''; };
$('identity').onclick = () => run('native_identity');
$('logout').onclick = () => run('logout');
$('pair-form').onsubmit = async e => { e.preventDefault(); await run('pair_node', {challengeId: $('challenge-id').value, userCode: $('pair-code').value}); $('pair-code').value = ''; };
$('node').onclick = () => run('node_status');
$('check-update').onclick = async () => { const value = await run('check_update'); $('install-update').disabled = !value?.available; };
$('install-update').onclick = async () => { $('install-update').disabled = true; await run('install_update'); };
run('desktop_status');

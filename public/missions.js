(function () {
  'use strict';
  var $ = function (id) { return document.getElementById(id); };
  var esc = function (s) { return String(s == null ? '' : s).replace(/[&<>"']/g, function (c) { return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]; }); };
  var names = {}, rosterSig = '', renderSig = '';
  var labels = { planning: '宰相正在拆解', running: '执行中', reporting: '宰相正在汇报', completed: '全部完成', partial: '部分完成', failed: '执行失败', queued: '等待执行', blocked: '受阻' };
  async function request(path, body) {
    var response = await fetch(path, body ? { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) } : {});
    var result = await response.json();
    if (!result.ok) throw new Error(result.error);
    return result.data;
  }
  async function refresh() {
    try {
      var values = await Promise.all([request('/api/agents'), request('/api/missions')]);
      var agents = values[0].filter(function (a) { return a.enabled && (a.real || a.communication === 'http'); });
      agents.forEach(function (a) { names[a.id] = a.name; }); names.hub = '你 / 用户';
      var sig = agents.map(function (a) { return a.id; }).join('|');
      if (sig !== rosterSig || !$('missionManager').options.length) {
        rosterSig = sig;
        $('missionManager').innerHTML = agents.map(function (a) { return '<option value="' + esc(a.id) + '">' + esc(a.name) + '</option>'; }).join('');
        $('missionWorkers').innerHTML = agents.map(function (a, i) { return '<label><input type="checkbox" name="missionWorker" value="' + esc(a.id) + '"' + (i > 0 || agents.length === 1 ? ' checked' : '') + '> ' + esc(a.name) + '</label>'; }).join('');
      }
      $('missionSubmit').disabled = !agents.length;
      if (!agents.length) $('missionError').textContent = '未发现可用执行端，请先安装本机 CLI 或接入 HTTP Harness。';
      var next = JSON.stringify(values[1]);
      if (next === renderSig) return;
      renderSig = next;
      $('missionList').innerHTML = values[1].map(function (m) {
        return '<article class="card mission-card"><div class="card-head"><div><h3>' + esc(m.objective) + '</h3><p>宰相：' + esc(names[m.managerId] || m.managerId) + ' · ' + esc(labels[m.status] || m.status) + ' · 协作轮次 ' + m.round + '</p></div></div>' +
          (m.error ? '<p class="mission-error">' + esc(m.error) + '</p>' : '') +
          '<div class="mission-board">' + m.tasks.map(function (t) { return '<section class="mission-subtask"><strong>' + esc(t.id + ' · ' + t.title) + '</strong><p>' + esc(names[t.agentId] || t.agentId) + ' · ' + esc(labels[t.status] || t.status) + ' · 调用 ' + t.turns + ' 次</p><p>依赖：' + esc(t.dependsOn.join(', ') || '无') + '</p><pre>' + esc(t.error || t.output || t.instructions) + '</pre></section>'; }).join('') + '</div>' +
          '<details><summary>分派与相互通信（' + m.events.length + ' 条）</summary>' + m.events.map(function (e) { return '<div class="mission-event"><strong>' + esc(names[e.from] || e.from) + ' → ' + esc(names[e.to] || e.to) + '</strong><pre>' + esc(e.text) + '</pre></div>'; }).join('') + '</details>' +
          (m.report ? '<section class="mission-report"><h3>宰相汇报</h3><pre>' + esc(m.report) + '</pre></section>' : '') + '</article>';
      }).join('') || '<div class="empty-state">尚未下达任务。提交目标后，这里会显示真实分工、通信和汇报。</div>';
    } catch (e) { $('missionError').textContent = '服务未连接：' + e.message; }
  }
  $('missionForm').addEventListener('submit', async function (event) {
    event.preventDefault(); $('missionSubmit').disabled = true; $('missionError').textContent = '';
    try {
      var workers = Array.from(document.querySelectorAll('[name="missionWorker"]:checked')).map(function (el) { return el.value; });
      await request('/api/missions', { objective: $('missionObjective').value, managerId: $('missionManager').value,
        workerIds: workers, permission: $('missionPermission').value, workdir: $('missionWorkdir').value || undefined });
      $('missionObjective').value = ''; await refresh();
    } catch (e) { $('missionError').textContent = e.message; }
    finally { $('missionSubmit').disabled = false; }
  });
  if (/^https?:$/.test(location.protocol)) { refresh(); setInterval(refresh, 2000); }
  else $('missionError').textContent = '请运行 npm start，并打开 http://127.0.0.1:8787。';
})();

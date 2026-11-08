'use strict';
const { app, BrowserWindow, Menu, Tray, dialog, shell, nativeImage } = require('electron');
const fs = require('node:fs');
const path = require('node:path');
const { ensureBackend } = require('./backend');

let root = path.resolve(__dirname, '..');
if (app.isPackaged) root = JSON.parse(fs.readFileSync(path.join(process.resourcesPath, 'workspace.json'), 'utf8')).root;
root = process.env.HARNESS_HUB_ROOT || root;
const port = Number(process.env.PORT || 8787);
const base = `http://127.0.0.1:${port}`;
let window, tray, quitting = false;
app.setName('Harness Hub');
app.setAppUserModelId('app.harnesshub.desktop');
if (!app.requestSingleInstanceLock()) app.quit();
else {
  app.on('second-instance', () => showWindow());
  app.whenReady().then(start).catch(error => {
    dialog.showErrorBox('Harness Hub 启动失败', error.message);
    quitting = true; app.quit();
  });
}

function showWindow() {
  if (!window) return;
  if (window.isMinimized()) window.restore();
  window.show(); window.focus();
}

async function start() {
  await ensureBackend(root, port);
  const icon = nativeImage.createFromPath(path.join(root, 'public', 'icon-192.png'));
  window = new BrowserWindow({ width: 1480, height: 960, minWidth: 760, minHeight: 580,
    title: 'Harness Hub', icon, backgroundColor: '#f4f5fa', show: false,
    webPreferences: { nodeIntegration: false, contextIsolation: true, sandbox: true } });
  Menu.setApplicationMenu(Menu.buildFromTemplate([
    { label: '工作台', submenu: [
      { label: '显示工作台', click: showWindow }, { label: '重新加载', role: 'reload' },
      { label: '打开项目目录', click: () => shell.openPath(root) }, { type: 'separator' },
      { label: '退出界面（后台服务继续运行）', click: () => { quitting = true; app.quit(); } }
    ] },
    { label: '编辑', submenu: [{ role: 'undo' }, { role: 'redo' }, { type: 'separator' }, { role: 'cut' }, { role: 'copy' }, { role: 'paste' }, { role: 'selectAll' }] },
    { label: '视图', submenu: [{ role: 'resetZoom' }, { role: 'zoomIn' }, { role: 'zoomOut' }, { role: 'togglefullscreen' }, { role: 'toggleDevTools' }] }
  ]));
  function isLocal(url) { try { return new URL(url).origin === base; } catch (_) { return false; } }
  function openExternal(url) { if (/^https?:\/\//i.test(url)) shell.openExternal(url); }
  window.webContents.on('will-navigate', (event, url) => { if (!isLocal(url)) { event.preventDefault(); openExternal(url); } });
  window.webContents.setWindowOpenHandler(({ url }) => {
    if (isLocal(url)) window.loadURL(url);
    else openExternal(url);
    return { action: 'deny' };
  });
  window.webContents.session.setPermissionRequestHandler((_contents, _permission, callback) => callback(false));
  window.once('ready-to-show', showWindow);
  window.on('page-title-updated', event => { event.preventDefault(); });
  window.on('close', event => { if (!quitting && tray) { event.preventDefault(); window.hide(); } });
  tray = new Tray(icon.resize({ width: 20, height: 20 }));
  tray.setToolTip('Harness Hub · 双击打开工作台');
  tray.setContextMenu(Menu.buildFromTemplate([
    { label: '打开 Harness Hub', click: showWindow },
    { label: '关闭窗口后，后台任务继续运行', enabled: false }, { type: 'separator' },
    { label: '退出界面', click: () => { quitting = true; app.quit(); } }
  ]));
  tray.on('double-click', showWindow);
  await window.loadURL(base + '/');
}
app.on('before-quit', () => { quitting = true; });
app.on('window-all-closed', () => app.quit());

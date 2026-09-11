import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { CSSProperties, ReactNode } from 'react';
import type { LucideIcon } from 'lucide-react';
import {
  Activity, AlertTriangle, Ban, CalendarCheck, CalendarDays, Check, ChevronDown, ChevronLeft, ChevronRight, CircleHelp, Clipboard, Cloud,
  Copy, Database, Download, Eye, EyeOff, FileJson, Flame, FolderOpen, Gauge, Gift, Globe2, KeyRound,
  Layers3, LayoutDashboard, ListFilter, LockKeyhole, LogOut, Menu, Minus, MoreHorizontal,
  Network, Pause, Pencil, Play, Plus, RefreshCw, Search, Server, Settings2,
  ShieldCheck, SlidersHorizontal, Sparkles, Square, Terminal, Trash2, Upload,
  Users, X, Zap,
} from 'lucide-react';
import type { Account, ApiKey, AppState, CheckinResponse, CheckinStatusResponse, DayStats, ModelInfo, OAuthCompleteResponse, PageId, RequestLog, ServiceConfig, ThemeMode } from './types';
import { defaultState, emptyDayStats } from './types';
import { applyTheme } from './theme';
import {
  cancelOAuth, checkinAccount, clearLogs, completeOAuth, exportAccounts, getCheckinStatus, getState, listModels, openExternal,
  refreshAccountQuota, refreshAllQuotas, resetLocalState, saveAccounts, syncModels,
  saveConfig, saveKeys, startOAuth, startService, stopService, validateToken,
} from './services';

import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';

// 应用版本号由 Vite 构建时从 package.json 注入（见 vite.config.ts），
// 保持前端展示与打包版本一致，避免多处手动维护。
const APP_VERSION = __APP_VERSION__;

type NavItem = { id: PageId; label: string; icon: LucideIcon };
type NavGroup = { id: string; label: string; icon: LucideIcon; items: NavItem[] };

type NoticeHandler = (message: string) => void;

const navGroups: NavGroup[] = [
  { id: 'workspace', label: '工作台', icon: LayoutDashboard, items: [{ id: 'overview', label: '总览', icon: Gauge }] },
  { id: 'proxy', label: '反代服务', icon: Server, items: [
    { id: 'service', label: '服务配置', icon: SlidersHorizontal },
    { id: 'keys', label: 'API Key', icon: KeyRound },
    { id: 'logs', label: '请求日志', icon: ListFilter },
  ] },
  { id: 'codebuddy', label: 'CodeBuddy', icon: Sparkles, items: [
    { id: 'accounts', label: '账号池', icon: Users },
    { id: 'models', label: '模型管理', icon: Layers3 },
  ] },
  { id: 'settings', label: '设置', icon: Settings2, items: [{ id: 'settings', label: '应用设置', icon: Settings2 }] },
];

const statusLabels = {
  available: '可用',
  needs_auth: '需要重新认证',
  cooling: '暂时冷却',
  restricted: '对话受限',
  disabled: '已禁用',
} as const;
const statusClass = { available: 'success', needs_auth: 'danger', cooling: 'warning', restricted: 'danger', disabled: 'muted' } as const;

function formatTime(value: number | null | undefined) {
  if (!value) return '—';
  return new Intl.DateTimeFormat('zh-CN', { hour: '2-digit', minute: '2-digit' }).format(value);
}
function formatDate(value: number | null | undefined) {
  if (!value) return '从未';
  return new Intl.DateTimeFormat('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }).format(value);
}
function formatNumber(value: number) { return new Intl.NumberFormat('zh-CN').format(value); }
function formatCompact(value: number) {
  const units: Array<[number, string]> = [[1e9, 'B'], [1e8, 'Y'], [1e6, 'M'], [1e4, 'W'], [1e3, 'K']];
  for (const [threshold, suffix] of units) {
    if (value >= threshold) {
      const n = value / threshold;
      return `${n >= 100 ? Math.round(n) : n >= 10 ? n.toFixed(1) : n.toFixed(2)}${suffix}`;
    }
  }
  return formatNumber(value);
}
// —— 日期工具：本地时区的"天"以 yyyy-MM-dd 字符串为 key ——
function toDateKey(d: Date) { return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`; }
function todayKey() { return toDateKey(new Date()); }
function parseDateKey(key: string) { const [y, m, d] = key.split('-').map(Number); return new Date(y || 1970, (m || 1) - 1, d || 1); }
function formatDateLabel(d: Date) { return `${d.getFullYear()}/${String(d.getMonth() + 1).padStart(2, '0')}/${String(d.getDate()).padStart(2, '0')}`; }
function maskKey(key: string) { return key.length <= 12 ? key : `${key.slice(0, 8)}••••••${key.slice(-4)}`; }

async function copyText(value: string) {
  if (navigator.clipboard) {
    await navigator.clipboard.writeText(value);
    return;
  }
  const textarea = document.createElement('textarea');
  textarea.value = value;
  document.body.appendChild(textarea);
  textarea.select();
  document.execCommand('copy');
  textarea.remove();
}

function hasTauri() {
  return Boolean((window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__);
}

function StatusPill({ children, tone = 'muted', dot = true }: { children: ReactNode; tone?: 'success' | 'warning' | 'danger' | 'muted' | 'blue'; dot?: boolean }) {
  return <span className={`status-pill ${tone}`}>{dot && <i className="status-dot" />}{children}</span>;
}
function IconButton({ label, onClick, children, danger = false, disabled = false }: { label: string; onClick?: () => void; children: ReactNode; danger?: boolean; disabled?: boolean }) {
  return <button className={`icon-button ${danger ? 'danger' : ''}`} aria-label={label} title={label} onClick={onClick} disabled={disabled}>{children}</button>;
}
function SectionHeader({ eyebrow, title, description, action }: { eyebrow?: string; title: string; description?: string; action?: ReactNode }) {
  return <div className="section-header"><div><div className="eyebrow">{eyebrow}</div><h2>{title}</h2>{description && <p>{description}</p>}</div>{action}</div>;
}
function EmptyState({ icon: Icon, title, description, action }: { icon: LucideIcon; title: string; description: string; action?: ReactNode }) {
  return <div className="empty-state"><span className="empty-icon"><Icon size={22} /></span><strong>{title}</strong><p>{description}</p>{action}</div>;
}

export function App() {
  const [page, setPage] = useState<PageId>('overview');
  const [openGroup, setOpenGroup] = useState('workspace');
  const [state, setState] = useState<AppState>(defaultState);
  const [loading, setLoading] = useState(true);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<'start' | 'stop' | 'save' | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [showAccountModal, setShowAccountModal] = useState(false);
  const [accountModalMode, setAccountModalMode] = useState<'browser' | 'token' | 'file'>('browser');
  const [showKeyModal, setShowKeyModal] = useState(false);
  const [editingKey, setEditingKey] = useState<ApiKey | null>(null);
  const [showCheckinModal, setShowCheckinModal] = useState(false);
  const [showExitMenu, setShowExitMenu] = useState(false);

  const refreshState = async () => {
    const next = await getState();
    setState(next);
    return next;
  };

  useEffect(() => {
    void refreshState().catch((reason) => setError(String(reason))).finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    if (!hasTauri()) return;
    let unlisten: (() => void) | undefined;
    void listen('coderelay-state-changed', () => { void refreshState(); }).then((cleanup) => { unlisten = cleanup; });
    return () => unlisten?.();
  }, []);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        setOpenGroup('');
        setShowExitMenu(false);
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  useEffect(() => {
    if (!notice && !error) return;
    const timer = window.setTimeout(() => { setNotice(null); setError(null); }, 4500);
    return () => window.clearTimeout(timer);
  }, [notice, error]);

  const updateState = (next: AppState) => setState(next);
  const runAction = async (action: () => Promise<AppState>, success: string, kind?: 'start' | 'stop' | 'save') => {
    if (busy) return;
    setError(null);
    setBusy(kind ?? 'save');
    try {
      updateState(await action());
      setNotice(success);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(null);
    }
  };
  const notify: NoticeHandler = (message) => { setError(null); setNotice(message); };

  const handleRefreshAccount = async (account: Account) => {
    try {
      updateState(await refreshAccountQuota(account.id));
      setError(null);
      setNotice(`已刷新账号 ${account.email}`);
    } catch (reason) {
      setNotice(null);
      setError(reason instanceof Error ? reason.message : String(reason));
      throw reason;
    }
  };
  const handleRefreshOverview = async () => {
    if (refreshing) return;
    setRefreshing(true);
    try {
      await refreshState();
      setError(null);
      setNotice('统计数据已刷新');
    } catch (reason) {
      setNotice(null);
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setRefreshing(false);
    }
  };
  const handleRefreshAll = async () => {
    try {
      const response = await refreshAllQuotas();
      updateState(response.state);
      setError(null);
      const skippedText = response.skipped ? `，跳过 ${response.skipped} 个已禁用` : '';
      setNotice(response.failed ? `已刷新 ${response.refreshed} 个账号，${response.failed} 个失败${skippedText}` : `已刷新 ${response.refreshed} 个账号${skippedText}`);
    } catch (reason) {
      setNotice(null);
      setError(reason instanceof Error ? reason.message : String(reason));
      throw reason;
    }
  };

  const selected = navGroups.flatMap((group) => group.items).find((item) => item.id === page);
  const titlePath = page === 'overview'
    ? '总览'
    : `${selected?.id === 'accounts' || selected?.id === 'models' ? 'CodeBuddy' : selected?.id === 'settings' ? '' : '反代服务'} / ${selected?.label ?? ''}`.replace(/^ \/ /, '');

  const closeWindow = async () => {
    if (!hasTauri()) {
      notify('浏览器预览无法关闭窗口');
      setShowExitMenu(false);
      return;
    }
    if (state.running) {
      setNotice('正在停止反代服务…');
      try {
        await stopService();
        await getCurrentWindow().close();
      } catch (reason) {
        setError(reason instanceof Error ? reason.message : String(reason));
      }
      return;
    }
    await getCurrentWindow().close();
  };
  // 最小化到 Windows 任务栏（标准最小化行为，任务栏仍显示窗口按钮）。
  const minimizeToTaskbar = async () => {
    setShowExitMenu(false);
    if (hasTauri()) await getCurrentWindow().minimize();
    else notify('浏览器预览无法最小化窗口');
  };
  // 隐藏到系统托盘（窗口从任务栏消失，仅托盘图标可见）。
  const hideToTray = async () => {
    setShowExitMenu(false);
    if (hasTauri()) await getCurrentWindow().hide();
    else notify('浏览器预览无法最小化到托盘');
  };

  if (loading) return <div className="loading-shell"><div className="brand-mark">CR</div><span>正在载入 CodeRelay…</span></div>;

  return <div className="app-shell">
    <aside className="sidebar" aria-label="主导航">
      <div className="brand-mark" aria-label="CodeRelay">CR</div>
      <nav className="nav-rail">
        {navGroups.map((group) => {
          const GroupIcon = group.icon;
          const active = group.items.some((item) => item.id === page);
          return <div key={group.id} className="nav-group-wrap" onMouseEnter={() => setOpenGroup(group.id)}>
            <button className={`nav-icon ${active ? 'active' : ''}`} aria-label={group.label} aria-expanded={openGroup === group.id} onFocus={() => setOpenGroup(group.id)} onClick={() => setOpenGroup(openGroup === group.id ? '' : group.id)}><GroupIcon size={19} strokeWidth={1.8} /></button>
            {openGroup === group.id && <div className="nav-popover" onMouseLeave={() => setOpenGroup('')}><div className="popover-label">{group.label}</div>{group.items.map((item) => { const ItemIcon = item.icon; return <button key={item.id} className={`nav-item ${page === item.id ? 'selected' : ''}`} onClick={() => { setPage(item.id); setOpenGroup(group.id); }}><ItemIcon size={16} /><span>{item.label}</span>{page === item.id && <Check size={14} />}</button>; })}</div>}
          </div>;
        })}
      </nav>
      <div className="sidebar-bottom"><button className="nav-icon" aria-label="帮助" onClick={() => notify('帮助文档尚未接入，当前可查看项目 README.md')}><CircleHelp size={18} /></button><div className="avatar">C</div></div>
    </aside>
    <main className="main-shell">
      <header className="titlebar" data-tauri-drag-region="true">
        <div className="titlebar-left"><Menu size={16} className="mobile-menu" /><span className="title-brand">CodeRelay</span><span className="title-separator">/</span><span className="title-current">{titlePath}</span></div>
        <div className="window-controls">
          <IconButton label="最小化" onClick={() => { void minimizeToTaskbar(); }}><Minus size={15} /></IconButton>
          <IconButton label="最大化" onClick={() => { if (hasTauri()) void getCurrentWindow().toggleMaximize(); else notify('浏览器预览无法调整桌面窗口大小'); }}><Square size={13} /></IconButton>
          <IconButton label="关闭" danger onClick={() => setShowExitMenu(true)}><X size={15} /></IconButton>
        </div>
      </header>
      <div className="content-scroll"><div className="page-container">
        {page === 'overview' && <OverviewPage state={state} onNavigate={setPage} onRefresh={() => { void handleRefreshOverview(); }} refreshing={refreshing} />}
        {page === 'service' && <ServicePage state={state} onSave={(config) => void runAction(() => saveConfig(config), '服务配置已保存；服务运行时需重新启动后生效', 'save')} notify={notify} />}
        {page === 'keys' && <KeysPage state={state} onAdd={() => { setEditingKey(null); setShowKeyModal(true); }} onEdit={(key) => { setEditingKey(key); setShowKeyModal(true); }} onSave={(keys) => void runAction(() => saveKeys(keys), 'API Key 已更新', 'save')} notify={notify} />}
        {page === 'logs' && <LogsPage state={state} onClear={() => void runAction(clearLogs, '请求日志已清理', 'save')} notify={notify} />}
        {page === 'accounts' && <AccountsPage state={state} onAdd={() => { setAccountModalMode('browser'); setShowAccountModal(true); }} onImport={() => { setAccountModalMode('file'); setShowAccountModal(true); }} onSave={(accounts) => void runAction(() => saveAccounts(accounts), '账号列表已更新', 'save')} onRefresh={handleRefreshAccount} onRefreshAll={handleRefreshAll} onCheckin={() => setShowCheckinModal(true)} notify={notify} />}
        {page === 'models' && <ModelsPage state={state} notify={notify} />}
        {page === 'settings' && <SettingsPage onReset={resetLocalState} notify={notify} />}
      </div></div>
      <footer className="statusbar"><div className="statusbar-left"><span className="secure-note"><LockKeyhole size={13} />本地数据</span><span className="divider" /><span>CodeRelay {APP_VERSION}</span></div><div className="statusbar-right"><StatusPill tone={busy === 'start' || busy === 'stop' ? 'warning' : state.running ? 'success' : 'muted'}>{busy === 'start' ? '启动中…' : busy === 'stop' ? '停止中…' : state.running ? `运行中 · ${state.actualPort ?? state.config.port}` : '已停止'}</StatusPill>{state.running ? <button className="button compact ghost" disabled={busy !== null} onClick={() => void runAction(stopService, '反代服务已停止', 'stop')}><Pause size={14} />停止服务</button> : <button className="button compact primary" disabled={busy !== null} onClick={() => void runAction(startService, '反代服务已启动', 'start')}><Play size={14} />启动服务</button>}<button className="status-chevron" aria-label="更多服务操作" onClick={() => setPage('service')}><ChevronDown size={15} /></button></div></footer>
    </main>
    {notice && <div className="toast success-toast"><Check size={16} />{notice}</div>}
    {error && <div className="toast error-toast"><AlertTriangle size={16} />{error}</div>}
    {showAccountModal && <AccountModal existingAccounts={state.accounts} initialMode={accountModalMode} onClose={() => setShowAccountModal(false)} onSave={(accounts, summary) => { setShowAccountModal(false); void runAction(() => { const ids = new Set(accounts.map((account) => account.id)); const emails = new Set(accounts.map((account) => account.email.trim().toLowerCase()).filter(Boolean)); const kept = state.accounts.filter((account) => !ids.has(account.id) && !emails.has(account.email.trim().toLowerCase())); return saveAccounts([...kept, ...accounts]); }, summary ?? `已添加 ${accounts.length} 个账号`, 'save'); }} notify={notify} />}
    {showKeyModal && <KeyModal accounts={state.accounts} existingKey={editingKey} onClose={() => { setShowKeyModal(false); setEditingKey(null); }} onSave={(key) => { setShowKeyModal(false); setEditingKey(null); if (editingKey) { void runAction(() => saveKeys(state.keys.map((item) => item.id === key.id ? key : item)), 'API Key 已更新', 'save'); } else { void runAction(() => saveKeys([...state.keys, key]), 'API Key 已创建', 'save'); } }} />}
    {showCheckinModal && <CheckinModal accounts={state.accounts} onClose={() => setShowCheckinModal(false)} />}
    {showExitMenu && <Modal title={state.running ? '反代服务正在运行' : '退出 CodeRelay'} onClose={() => setShowExitMenu(false)}><div className="modal-form exit-confirm"><p className="exit-confirm-lead">{state.running ? '关闭前需要先停止反代服务。请选择最小化到系统盘或继续退出。' : '确认退出当前应用？'}</p><div className="exit-confirm-actions"><button className="button ghost" onClick={() => setShowExitMenu(false)}><X size={14} />取消</button><button className="button ghost" onClick={() => { void hideToTray(); }}><Minus size={14} />最小化到系统盘</button><button className="button danger-button" onClick={() => { void closeWindow(); }}><LogOut size={14} />{state.running ? '停止并退出' : '退出'}</button></div></div></Modal>}
  </div>;
}

function OverviewPage({ state, onNavigate, onRefresh, refreshing }: { state: AppState; onNavigate: (page: PageId) => void; onRefresh: () => void; refreshing: boolean }) {
  const [viewDate, setViewDate] = useState<string>('today');
  const [showCalendar, setShowCalendar] = useState(false);
  const available = state.accounts.filter((account) => account.status === 'available').length;
  const attention = state.accounts.filter((account) => account.status === 'needs_auth' || account.status === 'cooling').length;
  // 每日零点自动切回"今天"并刷新统计，实现按天重置。
  useEffect(() => {
    let timer: number;
    const schedule = () => {
      const now = new Date();
      const next = new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1, 0, 0, 2);
      timer = window.setTimeout(() => { setViewDate('today'); onRefresh(); schedule(); }, next.getTime() - now.getTime());
    };
    schedule();
    return () => window.clearTimeout(timer);
  }, [onRefresh]);
  // 依据当前视图（今天 / 某天 / 累计）归一化统计数据源。
  const source = useMemo(() => {
    if (viewDate === 'today') {
      return { requestCount: state.stats.requestCount, totalTokens: state.stats.totalTokens, cacheHitTokens: state.stats.cacheHitTokens, credit: state.stats.credit, successCount: state.stats.successCount, failureCount: state.stats.failureCount, byHour: state.stats.byHour, averageLatencyMs: state.stats.averageLatencyMs };
    }
    const day = viewDate === 'lifetime' ? state.stats.lifetime : state.stats.byDay[viewDate];
    const d: DayStats = day ?? emptyDayStats();
    return { requestCount: d.requestCount, totalTokens: d.totalTokens, cacheHitTokens: d.cacheHitTokens, credit: d.credit, successCount: d.successCount, failureCount: d.failureCount, byHour: d.byHour && d.byHour.length ? d.byHour : emptyDayStats().byHour, averageLatencyMs: d.requestCount ? Math.round(d.totalLatencyMs / d.requestCount) : 0 };
  }, [viewDate, state.stats]);
  const isEmptyDay = viewDate !== 'today' && viewDate !== 'lifetime' && !state.stats.byDay[viewDate];
  const cacheRate = source.totalTokens ? Math.round((source.cacheHitTokens / source.totalTokens) * 100) : 0;
  const maxHourTotal = Math.max(1, ...source.byHour.map((item) => item.hit + item.miss));
  const viewLabel = viewDate === 'today' ? `今天 · ${formatDateLabel(new Date())}` : viewDate === 'lifetime' ? '累计 · 全部数据' : formatDateLabel(parseDateKey(viewDate));
  const chartTitle = viewDate === 'lifetime' ? '累计请求分布' : '过去 24 小时';
  const chartHint = viewDate === 'lifetime' ? '每 3 小时聚合 · 全部数据' : '每 3 小时聚合 · 共 24 小时';
  return <>
    <div className="page-intro"><div><div className="eyebrow">工作台 / 运行概览</div><h1>保持请求链路清晰</h1><p>查看 CodeBuddy 账号池、本地 OpenAI 兼容服务和最近一次运行的关键状态。</p></div><div className="intro-actions"><button className="button ghost icon-only" onClick={onRefresh} disabled={refreshing} aria-label="刷新统计" title="刷新统计"><RefreshCw size={15} className={refreshing ? 'spin' : ''} /></button><button className="button ghost" onClick={() => onNavigate('accounts')}><Users size={15} />管理账号</button><button className="button ghost" onClick={() => onNavigate('service')}><Server size={15} />服务配置</button></div></div>
    <div className="stats-view-bar">
      <div className="view-toggle" role="tablist" aria-label="统计视图"><button className={viewDate === 'today' ? 'active' : ''} onClick={() => setViewDate('today')}>今天</button><button className={viewDate === 'lifetime' ? 'active' : ''} onClick={() => setViewDate('lifetime')}>累计</button></div>
      <button className={`date-trigger ${viewDate !== 'today' && viewDate !== 'lifetime' ? 'active' : ''}`} onClick={() => setShowCalendar(true)} aria-label="选择日期"><CalendarDays size={14} /><span>{viewLabel}</span><ChevronDown size={13} /></button>
    </div>
    {isEmptyDay && <div className="inline-empty-hint"><CalendarDays size={14} />该日暂无请求数据，可选择其他日期或切换到累计视图。</div>}
    <div className="overview-grid">
      <section className={`hero-status panel ${state.running ? 'running' : ''}`}><div className="panel-topline"><span className="panel-kicker"><Server size={14} />反代服务</span><StatusPill tone={state.running ? 'success' : 'muted'}>{state.running ? '运行中' : '已停止'}</StatusPill></div><div className="hero-value">{state.running ? '服务在线' : '等待手动启动'}</div><p>{state.running ? `正在监听 ${state.config.bindHost}:${state.actualPort ?? state.config.port}` : state.lastError ?? '服务启动后将通过本地 OpenAI 兼容接口接收请求。'}</p><div className="hero-foot"><div><span>可用账号</span><strong>{available} / {state.accounts.length}</strong></div><div><span>需要关注</span><strong>{attention}</strong></div><button className="inline-link" onClick={() => onNavigate('service')}>查看服务配置 <span>→</span></button></div></section>
      <section className="metric-card panel"><span className="metric-icon blue"><Activity size={17} /></span><span className="metric-label">总请求数</span><strong>{formatCompact(source.requestCount)}</strong><span className="metric-trend"><small>{formatNumber(source.successCount)} 成功 · {formatNumber(source.failureCount)} 失败</small></span></section>
      <section className="metric-card panel"><span className="metric-icon purple"><Zap size={17} /></span><span className="metric-label">总 Token</span><strong>{formatCompact(source.totalTokens)}</strong><span className="metric-trend"><small>输入 + 输出</small></span></section>
      <section className="metric-card panel"><span className="metric-icon green"><Database size={17} /></span><span className="metric-label">缓存命中率</span><strong>{cacheRate}%</strong><span className="metric-trend"><small>{formatNumber(source.cacheHitTokens)} tokens 命中</small></span></section>
      <section className="metric-card panel"><span className="metric-icon orange"><Cloud size={17} /></span><span className="metric-label">Credit 消耗</span><strong>{source.credit.toFixed(2)}</strong><span className="metric-trend"><small>本地统计快照</small></span></section>
    </div>
    <div className="two-column-grid"><section className="panel chart-panel"><div className="panel-heading"><div><span className="panel-kicker">请求统计</span><h3>{chartTitle}</h3></div><span className="muted-text">{chartHint}</span></div><div className="chart-legend"><span><i className="legend-dot hit" />缓存命中</span><span><i className="legend-dot miss" />未命中</span><span className="chart-average">平均延迟 <strong>{source.averageLatencyMs} ms</strong></span><span className="chart-axis-hint">最高 {maxHourTotal} 次</span></div><div className="bar-chart">{source.byHour.map((item) => <div className="bar-column" key={item.label}><div className="bar-stack" title={`${item.label}:00 起 · 命中 ${item.hit} 次 · 未命中 ${item.miss} 次`}><span className="bar hit" style={{ height: `${(item.hit / maxHourTotal) * 100}%` }} /><span className="bar miss" style={{ height: `${(item.miss / maxHourTotal) * 100}%` }} /></div><span>{item.label}</span></div>)}</div></section><section className="panel schedule-panel"><div className="panel-heading"><div><span className="panel-kicker">调度状态</span><h3>账号池健康度</h3></div><button className="icon-button" aria-label="打开账号池" onClick={() => onNavigate('accounts')}><MoreHorizontal size={17} /></button></div><div className="health-score"><div className="score-ring"><strong>{state.accounts.length ? Math.round((available / state.accounts.length) * 100) : 0}</strong><span>%</span></div><div><strong>{available} 个账号可用</strong><p>调度策略：<b>{state.config.routingStrategy === 'auto' ? '自动' : state.config.routingStrategy}</b></p></div></div><div className="health-list"><div><span className="health-label"><i className="tiny-dot green-dot" />可用</span><strong>{available}</strong></div><div><span className="health-label"><i className="tiny-dot yellow-dot" />冷却中</span><strong>{state.accounts.filter((a) => a.status === 'cooling').length}</strong></div><div><span className="health-label"><i className="tiny-dot red-dot" />需处理</span><strong>{state.accounts.filter((a) => a.status === 'needs_auth').length}</strong></div></div><button className="full-link" onClick={() => onNavigate('accounts')}>打开账号池 <span>→</span></button></section></div>
    <section className="panel activity-panel"><div className="panel-heading"><div><span className="panel-kicker">最近活动</span><h3>请求日志摘要</h3></div><button className="inline-link" onClick={() => onNavigate('logs')}>查看全部 <span>→</span></button></div><div className="mini-log-list">{state.logs.length ? state.logs.slice(0, 3).map((log) => <MiniLog key={log.requestId} log={log} accounts={state.accounts} />) : <div className="empty-inline">服务运行后，最近请求会显示在这里。</div>}</div></section>
    {showCalendar && <DatePicker value={viewDate} byDay={state.stats.byDay} onSelect={(v) => { setViewDate(v); setShowCalendar(false); }} onClose={() => setShowCalendar(false)} />}
  </>;
}

// 深色日期选择弹层：高亮有数据的日期，今天特殊标记，支持"今天 / 累计"快捷切换。
function DatePicker({ value, byDay, onSelect, onClose }: { value: string; byDay: Record<string, DayStats>; onSelect: (value: string) => void; onClose: () => void }) {
  const today = todayKey();
  const anchor = value !== 'today' && value !== 'lifetime' ? parseDateKey(value) : new Date();
  const [cursor, setCursor] = useState(new Date(anchor.getFullYear(), anchor.getMonth(), 1));
  const year = cursor.getFullYear();
  const month = cursor.getMonth();
  const firstWeekday = new Date(year, month, 1).getDay();
  const daysInMonth = new Date(year, month + 1, 0).getDate();
  const shiftMonth = (delta: number) => setCursor(new Date(year, month + delta, 1));
  const cells: Array<{ key: string; day: number; dateKey?: string; hasData?: boolean; isToday?: boolean; isSelected?: boolean; disabled?: boolean }> = [];
  for (let i = 0; i < firstWeekday; i += 1) cells.push({ key: `pad-${i}`, day: 0 });
  for (let d = 1; d <= daysInMonth; d += 1) {
    const dateKey = `${year}-${String(month + 1).padStart(2, '0')}-${String(d).padStart(2, '0')}`;
    cells.push({ key: dateKey, day: d, dateKey, hasData: Boolean(byDay[dateKey] && byDay[dateKey].requestCount > 0), isToday: dateKey === today, isSelected: value === dateKey, disabled: dateKey > today });
  }
  return <div className="date-picker-overlay" onClick={onClose}>
    <div className="date-picker" onClick={(e) => e.stopPropagation()}>
      <div className="date-picker-header"><button className="cal-nav" aria-label="上个月" onClick={() => shiftMonth(-1)}><ChevronLeft size={15} /></button><strong>{year} 年 {month + 1} 月</strong><button className="cal-nav" aria-label="下个月" onClick={() => shiftMonth(1)}><ChevronRight size={15} /></button></div>
      <div className="date-picker-weekdays">{['日', '一', '二', '三', '四', '五', '六'].map((w) => <span key={w}>{w}</span>)}</div>
      <div className="date-picker-grid">{cells.map((cell) => cell.day === 0 ? <span key={cell.key} className="cal-cell padding" /> : <button key={cell.key} className={`cal-cell ${cell.hasData ? 'has-data' : ''} ${cell.isToday ? 'today' : ''} ${cell.isSelected ? 'selected' : ''}`} disabled={cell.disabled} onClick={() => cell.dateKey && onSelect(cell.dateKey === today ? 'today' : cell.dateKey)}>{cell.day}{cell.hasData && <i className="data-dot" />}</button>)}</div>
      <div className="date-picker-footer"><button className={value === 'today' ? 'active' : ''} onClick={() => onSelect('today')}>今天</button><button className={value === 'lifetime' ? 'active' : ''} onClick={() => onSelect('lifetime')}>累计</button></div>
    </div>
  </div>;
}

function MiniLog({ log, accounts }: { log: RequestLog; accounts: Account[] }) { const account = accounts.find((item) => item.id === log.accountId); return <div className="mini-log"><span className={`log-status-icon ${log.success ? 'ok' : 'fail'}`}>{log.success ? <Check size={13} /> : <X size={13} />}</span><div className="mini-log-main"><strong>{log.model || '未指定模型'}</strong><span>{log.path || '—'} · {account?.email ?? (log.accountId || '未选择账号')}</span></div><span className="mini-log-time">{formatTime(log.timestamp)}</span><span className={`code-status ${log.success ? 'ok' : 'fail'}`}>{log.status || '—'}</span></div>; }

function ServicePage({ state, onSave, notify }: { state: AppState; onSave: (config: ServiceConfig) => void; notify: NoticeHandler }) {
  const [draft, setDraft] = useState(state.config);
  useEffect(() => setDraft(state.config), [state.config]);
  const change = <K extends keyof ServiceConfig>(key: K, value: ServiceConfig[K]) => setDraft((current) => ({ ...current, [key]: value }));
  const save = () => {
    if (draft.port < 1024 || draft.port > 65535) { notify('服务端口必须在 1024 到 65535 之间'); return; }
    onSave(draft);
  };
  return <><SectionHeader eyebrow="反代服务 / 配置" title="服务配置" description="控制本地 OpenAI 兼容入口、请求处理和 CodeBuddy 账号调度。" action={<StatusPill tone={state.running ? 'success' : 'muted'}>{state.running ? `运行中 · ${state.actualPort ?? draft.port}` : '已停止'}</StatusPill>} />
    {state.lastError && <div className="inline-error"><AlertTriangle size={15} /><span>{state.lastError}</span></div>}
    <div className="service-layout"><div className="service-form-column"><div className="panel form-panel"><div className="form-section"><div className="form-section-title"><div><h3>网络</h3><p>服务默认只绑定本机，局域网访问需要显式开启。</p></div><Network size={18} /></div><div className="form-grid"><Field label="监听地址" hint="默认使用本机回环地址"><select value={draft.scope} onChange={(e) => { const scope = e.target.value as ServiceConfig['scope']; change('scope', scope); change('bindHost', scope === 'lan' ? '0.0.0.0' : '127.0.0.1'); }}><option value="localhost">localhost · 127.0.0.1</option><option value="lan">局域网 · 0.0.0.0</option></select></Field><Field label="服务端口" hint="修改后需要重新启动服务"><input type="number" min={1024} max={65535} value={draft.port} onChange={(e) => change('port', Number(e.target.value))} /></Field></div><div className="notice-box"><ShieldCheck size={17} /><div><strong>{draft.scope === 'lan' ? '局域网访问已开启' : '仅允许本机访问'}</strong><span>{draft.scope === 'lan' ? '同一网络中的设备可以连接此服务，请确认网络可信。' : '外部设备无法访问此服务，适合单机开发。'}</span></div></div></div><div className="form-divider" /><div className="form-section"><div className="form-section-title"><div><h3>请求处理</h3><p>配置超时、重试和账号选择行为。</p></div><RefreshCw size={18} /></div><div className="form-grid"><Field label="请求超时" hint="单次上游请求最长等待时间"><div className="input-with-suffix"><input type="number" min={1} value={draft.requestTimeoutMs / 1000} onChange={(e) => change('requestTimeoutMs', Number(e.target.value) * 1000)} /><span>秒</span></div></Field><Field label="失败重试次数" hint="重试会切换到其他可用账号"><select value={draft.maxRetries} onChange={(e) => change('maxRetries', Number(e.target.value))}><option value={0}>不重试</option><option value={1}>1 次</option><option value={2}>2 次</option><option value={3}>3 次</option></select></Field><Field label="账号调度策略" hint="决定请求优先使用哪个账号" wide><select value={draft.routingStrategy} onChange={(e) => change('routingStrategy', e.target.value as ServiceConfig['routingStrategy'])}><option value="auto">自动：综合健康度和额度</option><option value="random">随机轮换</option><option value="quota_high_first">剩余额度优先</option><option value="single_account">固定单账号</option><option value="custom">自定义优先级</option></select></Field></div><Toggle label="会话亲和" description="同一会话尽量使用同一个账号，减少上下文漂移。" checked={draft.sessionAffinity} onChange={(value) => change('sessionAffinity', value)} /></div><div className="form-divider" /><div className="form-section"><div className="form-section-title"><div><h3>协议兼容</h3><p>保持 OpenAI Chat Completions 请求格式。</p></div><Globe2 size={18} /></div><Toggle label="图片生成和编辑" description="将图片请求路由到支持的 CodeBuddy 模型。" checked={draft.imageGenerationMode !== 'disabled'} onChange={(value) => change('imageGenerationMode', value ? 'enabled' : 'disabled')} /><Toggle label="调试日志" description="记录更多协议细节。可能包含请求元数据，请仅在排查问题时开启。" checked={draft.debugLogs} onChange={(value) => change('debugLogs', value)} /></div></div><div className="form-actions"><span className="save-hint">保存配置不会自动重启服务。</span><button className="button primary" onClick={save}><Check size={15} />保存配置</button></div></div><div className="service-side-column"><div className="panel endpoint-panel"><div className="panel-heading"><div><span className="panel-kicker">连接信息</span><h3>本地接口</h3></div><StatusPill tone="blue">OpenAI 兼容</StatusPill></div><div className="endpoint-row"><span>Base URL</span><code>http://localhost:{draft.port}/v1</code><IconButton label="复制 Base URL" onClick={() => { void copyText(`http://localhost:${draft.port}/v1`).then(() => notify('Base URL 已复制')); }}><Copy size={15} /></IconButton></div>{draft.scope === 'lan' && <div className="endpoint-row"><span>LAN URL</span><code>http://局域网地址:{draft.port}/v1</code><IconButton label="复制局域网地址" onClick={() => notify('请将“局域网地址”替换为本机实际 IPv4 地址')}><Copy size={15} /></IconButton></div>}<div className="endpoint-rule" /><div className="endpoint-meta"><span><LockKeyhole size={14} />API Key 鉴权</span><span><Terminal size={14} />POST /v1/chat/completions</span></div></div><div className="panel side-help"><div className="help-icon"><Clipboard size={18} /></div><div><h3>接入客户端</h3><p>将 Base URL 设置为上方地址，并使用 CodeRelay API Key 作为 Bearer Token。</p><button className="inline-link" onClick={() => notify('示例：Authorization: Bearer sk-coderelay-…')} >查看接入示例 <span>→</span></button></div></div><div className="panel side-warning"><AlertTriangle size={17} /><div><strong>安全提示</strong><p>API Key 只保存在本机配置目录。日志和错误消息不会记录上游 Token。</p></div></div></div></div></>;
}

function Field({ label, hint, children, wide = false }: { label: string; hint?: string; children: ReactNode; wide?: boolean }) { return <label className={`field ${wide ? 'wide' : ''}`}><span>{label}</span>{children}{hint && <small>{hint}</small>}</label>; }
function Toggle({ label, description, checked, onChange }: { label: string; description: string; checked: boolean; onChange: (value: boolean) => void }) { return <label className="toggle-row"><span className="toggle-copy"><strong>{label}</strong><small>{description}</small></span><input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} /><span className="toggle-track"><span /></span></label>; }

function KeysPage({ state, onAdd, onEdit, onSave, notify }: { state: AppState; onAdd: () => void; onEdit: (key: ApiKey) => void; onSave: (keys: ApiKey[]) => void; notify: NoticeHandler }) {
  const [visible, setVisible] = useState<string[]>([]);
  useEffect(() => setVisible(state.keys.map((key) => key.id)), [state.keys]);
  const toggle = (id: string) => setVisible((items) => items.includes(id) ? items.filter((item) => item !== id) : [...items, id]);
  const update = (key: ApiKey, changes: Partial<ApiKey>) => onSave(state.keys.map((item) => item.id === key.id ? { ...item, ...changes } : item));
  const remove = (key: ApiKey) => { if (window.confirm(`确认删除 API Key“${key.name}”？删除后使用它的客户端将无法访问服务。`)) onSave(state.keys.filter((item) => item.id !== key.id)); };
  const creditFor = (key: ApiKey) => {
    const scopeAccounts = key.accountIds ? state.accounts.filter((account) => key.accountIds!.includes(account.id)) : state.accounts.filter((account) => account.status === 'available');
    return { sum: scopeAccounts.reduce((total, account) => total + (account.quota || 0), 0), count: scopeAccounts.length };
  };
  return <><SectionHeader eyebrow="反代服务 / 访问控制" title="API Key" description="创建客户端访问凭据，并限制它们可以使用的账号和模型范围。" action={<button className="button primary" onClick={onAdd}><Plus size={15} />创建 Key</button>} /><div className="summary-row"><Summary label="全部 Key" value={state.keys.length.toString()} /><Summary label="已启用" value={state.keys.filter((key) => key.enabled).length.toString()} tone="success" /><Summary label="账号范围" value={state.keys.filter((key) => key.accountIds).length ? '已配置' : '全部账号'} /></div><div className="panel table-panel"><div className="table-toolbar"><div className="toolbar-title"><KeyRound size={17} /><strong>客户端凭据</strong><span>{state.keys.length} 个</span></div><span className="muted-text">完整 Key 默认显示，可直接复制</span></div>{state.keys.length ? <div className="data-table key-table"><div className="table-head"><span>名称</span><span>Key</span><span>账号范围</span><span>最近使用</span><span>状态</span><span /></div>{state.keys.map((key) => <div className="table-row" key={key.id}><div className="key-name"><span className="key-avatar"><KeyRound size={14} /></span><div><strong>{key.name}</strong><small>创建于 {formatDate(key.createdAt)}</small></div></div><div className="key-value"><code>{visible.includes(key.id) ? key.key : maskKey(key.key)}</code><IconButton label={visible.includes(key.id) ? '隐藏 Key' : '显示 Key'} onClick={() => toggle(key.id)}>{visible.includes(key.id) ? <EyeOff size={15} /> : <Eye size={15} />}</IconButton><IconButton label="复制 Key" onClick={() => { void copyText(key.key).then(() => notify('API Key 已复制')); }}><Copy size={15} /></IconButton></div><span className="key-scope"><span>{key.accountIds ? `${key.accountIds.length} 个指定账号` : <span className="all-scope"><Globe2 size={13} />全部账号</span>}</span><small className={creditFor(key).sum <= 0 && creditFor(key).count > 0 ? 'key-credit zero' : 'key-credit'}>剩余 {formatNumber(creditFor(key).sum)} credit</small></span><span className="muted-text">{formatDate(key.lastUsed)}</span><label className="switch-small"><input type="checkbox" checked={key.enabled} onChange={(e) => update(key, { enabled: e.target.checked })} /><span /></label><div className="row-actions"><IconButton label="调整账号范围" onClick={() => onEdit(key)}><Pencil size={15} /></IconButton><IconButton label="删除 Key" danger onClick={() => remove(key)}><Trash2 size={15} /></IconButton></div></div>)}</div> : <EmptyState icon={KeyRound} title="还没有 API Key" description="创建一个 Key 后，客户端才能访问本地反代服务。" action={<button className="button primary" onClick={onAdd}><Plus size={15} />创建第一个 Key</button>} />}</div><div className="security-footnote"><span><ShieldCheck size={15} />Key 只用于本机反代鉴权，删除前会要求确认。</span></div></>;
}
function Summary({ label, value, tone }: { label: string; value: string; tone?: 'success' }) { return <div className="summary-card"><span>{label}</span><strong className={tone}>{value}</strong></div>; }

function LogsPage({ state, onClear, notify }: { state: AppState; onClear: () => void; notify: NoticeHandler }) {
  const [query, setQuery] = useState('');
  const [onlyErrors, setOnlyErrors] = useState(false);
  const [selected, setSelected] = useState<RequestLog | null>(null);
  const logs = state.logs.filter((log) => (!query || `${log.model} ${log.path} ${log.accountId} ${log.apiKeyId} ${log.error ?? ''}`.toLowerCase().includes(query.toLowerCase())) && (!onlyErrors || !log.success));
  const exportLogs = () => { const blob = new Blob([JSON.stringify(logs, null, 2)], { type: 'application/json' }); const url = URL.createObjectURL(blob); const anchor = document.createElement('a'); anchor.href = url; anchor.download = 'coderelay-request-logs.json'; anchor.click(); URL.revokeObjectURL(url); };
  const renderLogRow = (log: RequestLog) => {
    const account = state.accounts.find((item) => item.id === log.accountId);
    const displayAccount = account?.email ?? log.accountId ?? '—';
    return <div className="table-row" key={log.requestId} onClick={() => setSelected(log)}><span className="time-cell">{formatDate(log.timestamp)}</span><div className="model-cell"><strong>{log.model || '—'}</strong><code>{log.method} {log.path}</code></div><span className="account-cell">{displayAccount}</span><span className={`code-status ${log.success ? 'ok' : 'fail'}`}>{log.status || '—'} {log.success ? '成功' : '失败'}</span><span>{log.latencyMs} ms</span><span className="token-cell">{formatNumber(log.inputTokens + log.outputTokens)}<small>{log.cacheHit ? '缓存命中' : '未命中'}</small></span><IconButton label="查看详情" onClick={() => setSelected(log)}><ChevronDown size={15} /></IconButton></div>;
  };
  return <>
    <SectionHeader eyebrow="反代服务 / 可观测性" title="请求日志" description="查看今天的请求、账号调度、响应耗时和错误分类（每日零点自动清零）。" action={<div className="header-actions"><button className="button ghost" onClick={exportLogs}><Upload size={15} />导出当前筛选 JSON</button></div>} />
    <div className="log-summary"><div><strong>{logs.length}</strong><span>当前结果</span></div><div><strong>{logs.filter((log) => log.success).length}</strong><span>成功</span></div><div><strong>{logs.filter((log) => !log.success).length}</strong><span>失败</span></div><div><strong>{state.stats.averageLatencyMs}ms</strong><span>平均延迟</span></div></div>
    <div className="panel table-panel"><div className="table-toolbar"><div className="search-box"><Search size={15} /><input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="搜索模型、账号、Key 或路径" /></div><div className="toolbar-right"><button className={`filter-button ${onlyErrors ? 'active' : ''}`} onClick={() => setOnlyErrors(!onlyErrors)}><AlertTriangle size={14} />仅看错误</button><button className="filter-button" onClick={onClear}><Trash2 size={14} />清理日志</button></div></div>
      {logs.length ? <div className="data-table logs-table"><div className="table-head"><span>时间</span><span>模型 / 路径</span><span>账号</span><span>状态</span><span>耗时</span><span>Token</span><span /></div>{logs.map(renderLogRow)}</div> : <EmptyState icon={FileJson} title="没有匹配的请求" description="保留筛选条件，或清除筛选后查看今天的日志。" />}
    </div>
    {selected && (() => {
      const account = state.accounts.find((item) => item.id === selected.accountId);
      const showError = !selected.success && Boolean(selected.error);
      return <Modal title="请求详情" onClose={() => setSelected(null)} wide>
        <div className="detail-view">
          <div className="detail-section">
            <h4 className="detail-section-title">基本信息</h4>
            <dl className="detail-list">
              <div className="detail-row"><dt>请求 ID</dt><dd><code>{selected.requestId}</code></dd></div>
              <div className="detail-row"><dt>时间</dt><dd><code>{formatDate(selected.timestamp)}</code></dd></div>
              <div className="detail-row"><dt>状态</dt><dd><StatusPill tone={selected.success ? 'success' : 'danger'}>{selected.status || '—'} · {selected.success ? '成功' : '失败'}</StatusPill></dd></div>
            </dl>
          </div>
          <div className="detail-section">
            <h4 className="detail-section-title">请求信息</h4>
            <dl className="detail-list">
              <div className="detail-row"><dt>方法</dt><dd><code>{selected.method}</code></dd></div>
              <div className="detail-row"><dt>路径</dt><dd><code>{selected.path}</code></dd></div>
              <div className="detail-row"><dt>模型</dt><dd><code>{selected.model || '—'}</code></dd></div>
              <div className="detail-row"><dt>API Key</dt><dd className="detail-row-copy"><code>{selected.apiKeyId || '—'}</code>{selected.apiKeyId && <IconButton label="复制 API Key ID" onClick={() => { void copyText(selected.apiKeyId!).then(() => notify('API Key ID 已复制')); }}><Copy size={14} /></IconButton>}</dd></div>
              <div className="detail-row"><dt>账号</dt><dd><span>{account?.email ?? selected.accountId ?? '—'}</span></dd></div>
            </dl>
          </div>
          <div className="detail-section">
            <h4 className="detail-section-title">性能</h4>
            <dl className="detail-list">
              <div className="detail-row"><dt>耗时</dt><dd><code>{selected.latencyMs} ms</code></dd></div>
              <div className="detail-row"><dt>输入 Token</dt><dd><code>{formatNumber(selected.inputTokens)}</code></dd></div>
              <div className="detail-row"><dt>输出 Token</dt><dd><code>{formatNumber(selected.outputTokens)}</code></dd></div>
              <div className="detail-row"><dt>Credit</dt><dd><code>{selected.credit.toFixed(2)}</code></dd></div>
              <div className="detail-row"><dt>缓存命中</dt><dd>{selected.cacheHit ? <StatusPill tone="success">命中</StatusPill> : <StatusPill tone="muted">未命中</StatusPill>}</dd></div>
            </dl>
          </div>
          {showError && <div className="detail-section"><h4 className="detail-section-title">错误信息</h4><div className="inline-error"><AlertTriangle size={15} /><span>{selected.error}</span></div></div>}
        </div>
      </Modal>;
    })()}
  </>;
}

function AccountsPage({ state, onAdd, onImport, onSave, onRefresh, onRefreshAll, onCheckin, notify }: { state: AppState; onAdd: () => void; onImport: () => void; onSave: (accounts: Account[]) => void; onRefresh: (account: Account) => Promise<void>; onRefreshAll: () => Promise<void>; onCheckin: () => void; notify: NoticeHandler }) {
  const [query, setQuery] = useState('');
  const [region, setRegion] = useState<'all' | 'cn'>('all');
  const [refreshingId, setRefreshingId] = useState<string | null>(null);
  const [refreshingAll, setRefreshingAll] = useState(false);
  const accounts = state.accounts.filter((account) => (!query || account.email.toLowerCase().includes(query.toLowerCase())) && (region === 'all' || account.region === region));
  const refreshBusy = refreshingId !== null || refreshingAll;
  const runRefresh = async (account: Account) => {
    if (refreshBusy) return;
    setRefreshingId(account.id);
    try { await onRefresh(account); } catch { /* 错误提示由父级统一展示 */ } finally { setRefreshingId(null); }
  };
  const runRefreshAll = async () => {
    if (refreshBusy) return;
    setRefreshingAll(true);
    try { await onRefreshAll(); } catch { /* 错误提示由父级统一展示 */ } finally { setRefreshingAll(false); }
  };
  const remove = (account: Account) => {
    const bindings = state.keys.filter((key) => key.accountIds?.includes(account.id));
    const bindingText = bindings.length ? `\n\n绑定的 API Key：${bindings.map((key) => key.name).join('、')}。删除后这些 Key 不会自动停用，但将无法使用该账号。` : '';
    if (window.confirm(`确认删除账号“${account.email}”？${bindingText}`)) onSave(state.accounts.filter((item) => item.id !== account.id));
  };
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const allVisibleSelected = accounts.length > 0 && accounts.every((account) => selected.has(account.id));
  const toggleSelect = (id: string) => setSelected((prev) => { const next = new Set(prev); next.has(id) ? next.delete(id) : next.add(id); return next; });
  const toggleSelectAll = () => setSelected(allVisibleSelected ? new Set() : new Set(accounts.map((account) => account.id)));
  // 清洗文件名中的非法字符与空白，避免保存失败。
  const sanitizeFileName = (value: string) => value.replace(/[\\/:*?"<>|\s]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 120);
  // 单账号导出文件名：email + uid 前 8 位，去重拼接后清洗。
  const accountFileName = (account: Account) => {
    const parts = [account.email, account.uid ? account.uid.slice(0, 8) : ''].filter(Boolean);
    return `${sanitizeFileName(parts.join('-')) || 'account'}.json`;
  };
  // 批量导出文件名：`${账号数} accounts ${yyyyMMdd}`。
  const batchFileName = (count: number) => { const d = new Date(); const y = d.getFullYear(); const m = String(d.getMonth() + 1).padStart(2, '0'); const day = String(d.getDate()).padStart(2, '0'); return `${count} accounts ${y}${m}${day}.json`; };
  const doExport = async (ids: string[], fileName: string) => {
    try {
      const saved = await exportAccounts(ids, fileName);
      notify(saved ? `已导出 ${ids.length} 个账号到 ${saved}` : '已取消导出');
    } catch (reason) {
      notify(reason instanceof Error ? reason.message : String(reason));
    }
  };
  const exportOne = (account: Account) => { void doExport([account.id], accountFileName(account)); };
  const exportSelected = () => { if (selected.size) void doExport([...selected], batchFileName(selected.size)); };
  return <><SectionHeader eyebrow="CodeBuddy / 资源池" title="账号池" description="管理用于请求调度的 CodeBuddy 中国站账号，查看健康状态、额度和绑定关系。" action={<div className="header-actions"><button className="button ghost" onClick={exportSelected} disabled={selected.size === 0}><Download size={15} />{selected.size ? `导出所选 (${selected.size})` : '导出所选'}</button><button className="button ghost" onClick={() => { void runRefreshAll(); }} disabled={refreshBusy || !state.accounts.length}><RefreshCw size={15} className={refreshingAll ? 'spin' : ''} />{refreshingAll ? '刷新中…' : '全部刷新'}</button><button className="button ghost" onClick={onImport}><Upload size={15} />导入配置</button><button className="button primary" onClick={onAdd}><Plus size={15} />添加账号</button></div>} /><div className="account-overview"><div className="account-overview-main"><div className="account-count"><strong>{state.accounts.length}</strong><span>个账号</span></div><div className="account-health-bar"><span style={{ width: `${state.accounts.length ? state.accounts.filter((a) => a.status === 'available').length / state.accounts.length * 100 : 0}%` }} /></div><span className="health-caption">{state.accounts.filter((a) => a.status === 'available').length} 个可用 · {state.accounts.filter((a) => a.status !== 'available').length} 个需要关注</span></div></div><div className="panel table-panel"><div className="table-toolbar"><div className="search-box"><Search size={15} /><input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="搜索邮箱或账号名称" /></div><div className="toolbar-right"><div className="segmented"><button className={region === 'all' ? 'active' : ''} onClick={() => setRegion('all')}>全部</button><button className={region === 'cn' ? 'active' : ''} onClick={() => setRegion('cn')}>中国站</button></div><button className="button ghost icon-only-sm" onClick={onCheckin} disabled={!state.accounts.length} aria-label="每日签到" title="每日签到"><CalendarCheck size={15} /></button></div></div>{accounts.length ? <div className="data-table accounts-table"><div className="table-head"><span><input type="checkbox" className="check-box" checked={allVisibleSelected} onChange={toggleSelectAll} aria-label="全选账号" /></span><span>账号</span><span>套餐</span><span>健康状态</span><span>额度</span><span>最近使用</span><span>标签</span><span /></div>{accounts.map((account) => <div className="table-row" key={account.id}><div className="row-check"><input type="checkbox" className="check-box" checked={selected.has(account.id)} onChange={() => toggleSelect(account.id)} aria-label={`选择账号 ${account.email}`} /></div><div className="account-name"><span className="account-avatar">{account.email.slice(0, 1).toUpperCase()}</span><div><strong>{account.email}</strong><small>CodeBuddy 中国站</small></div></div><span className={`plan-badge plan-${account.plan.toLowerCase()}`}>{account.plan || '未知'}</span><StatusPill tone={statusClass[account.status]}>{statusLabels[account.status]}</StatusPill><div className="quota-cell"><div className="quota-line"><span>{formatNumber(account.quota)}</span><small>/ {formatNumber(account.quotaTotal)}</small></div><div className="quota-bar"><span style={{ width: `${Math.min(100, account.quota / Math.max(1, account.quotaTotal) * 100)}%` }} /></div></div><span className="muted-text">{formatDate(account.lastUsed)}</span><div className="tag-list">{account.tags.map((tag) => <span key={tag}>{tag}</span>)}</div><div className="row-actions"><IconButton label="导出账号" onClick={() => exportOne(account)}><Download size={15} /></IconButton><IconButton label={refreshingId === account.id ? '正在刷新额度' : '刷新额度'} onClick={() => { void runRefresh(account); }} disabled={refreshBusy}><RefreshCw size={15} className={refreshingId === account.id ? 'spin' : ''} /></IconButton><IconButton label="删除账号" danger onClick={() => remove(account)}><Trash2 size={15} /></IconButton></div></div>)}</div> : <EmptyState icon={Users} title="还没有 CodeBuddy 账号" description="添加账号后，CodeRelay 才能为请求选择上游凭据。" action={<button className="button primary" onClick={onAdd}><Plus size={15} />添加第一个账号</button>} />}</div><div className="account-footnote"><span><ShieldCheck size={15} />Token 仅在桌面端凭据文件中保存，页面不回显完整凭据。</span></div></>;
}

type CheckinUiState = 'loading' | 'available' | 'claimed' | 'inactive' | 'error';

interface CheckinAccountState {
  status: CheckinStatusResponse | null;
  uiState: CheckinUiState;
  checkingIn: boolean;
  error: string | null;
  result: CheckinResponse | null;
}

const emptyCheckinState: CheckinAccountState = { status: null, uiState: 'loading', checkingIn: false, error: null, result: null };

function resolveCheckinUiState(status: CheckinStatusResponse | null): CheckinUiState {
  if (!status) return 'inactive';
  if (status.todayCheckedIn) return 'claimed';
  if (status.active !== true) return 'inactive';
  return 'available';
}

function CheckinModal({ accounts, onClose }: { accounts: Account[]; onClose: () => void }) {
  const [states, setStates] = useState<Record<string, CheckinAccountState>>({});
  const [refreshing, setRefreshing] = useState(false);
  const [checkingAll, setCheckingAll] = useState(false);

  const fetchAll = useCallback(async () => {
    setRefreshing(true);
    const next: Record<string, CheckinAccountState> = {};
    for (const account of accounts) next[account.id] = { ...emptyCheckinState };
    setStates(next);
    await Promise.allSettled(accounts.map(async (account) => {
      try {
        const status = await getCheckinStatus(account.id);
        next[account.id] = { status, uiState: resolveCheckinUiState(status), checkingIn: false, error: null, result: null };
      } catch (reason) {
        next[account.id] = { status: null, uiState: 'error', checkingIn: false, error: reason instanceof Error ? reason.message : String(reason), result: null };
      }
    }));
    setStates({ ...next });
    setRefreshing(false);
  }, [accounts]);

  useEffect(() => { void fetchAll(); }, [fetchAll]);

  const runCheckin = async (accountId: string) => {
    setStates((prev) => ({ ...prev, [accountId]: { ...(prev[accountId] ?? emptyCheckinState), checkingIn: true, error: null } }));
    try {
      const result = await checkinAccount(accountId);
      if (result.success) {
        setStates((prev) => {
          const prevStatus = prev[accountId]?.status;
          const status: CheckinStatusResponse = prevStatus
            ? { ...prevStatus, todayCheckedIn: true, streakDays: result.streakDays ?? prevStatus.streakDays, dailyCredit: result.credit ?? prevStatus.dailyCredit }
            : { todayCheckedIn: true, active: true, streakDays: result.streakDays ?? 0, dailyCredit: result.credit ?? 0 };
          return { ...prev, [accountId]: { status, uiState: 'claimed', checkingIn: false, error: null, result } };
        });
      } else {
        const already = /已签到|already/i.test(result.message ?? '');
        setStates((prev) => {
          const prevStatus = prev[accountId]?.status;
          if (already) {
            const status: CheckinStatusResponse = prevStatus
              ? { ...prevStatus, todayCheckedIn: true }
              : { todayCheckedIn: true, active: true, streakDays: 0, dailyCredit: 0 };
            return { ...prev, [accountId]: { status, uiState: 'claimed', checkingIn: false, error: null, result } };
          }
          return { ...prev, [accountId]: { ...prev[accountId], checkingIn: false, error: result.message ?? '签到失败', result } };
        });
      }
    } catch (reason) {
      const message = reason instanceof Error ? reason.message : String(reason);
      setStates((prev) => ({ ...prev, [accountId]: { ...prev[accountId], checkingIn: false, error: message } }));
    }
  };

  const runCheckinAll = async () => {
    const availableIds = accounts.filter((account) => states[account.id]?.uiState === 'available').map((account) => account.id);
    if (!availableIds.length) return;
    setCheckingAll(true);
    await Promise.allSettled(availableIds.map((id) => runCheckin(id)));
    setCheckingAll(false);
  };

  const claimedCount = accounts.filter((account) => states[account.id]?.uiState === 'claimed').length;
  const availableCount = accounts.filter((account) => states[account.id]?.uiState === 'available').length;
  const inactiveCount = accounts.filter((account) => states[account.id]?.uiState === 'inactive').length;

  return <Modal title="每日签到" onClose={onClose} wide>
    <div className="checkin-body">
      <div className="checkin-toolbar">
        <div className="checkin-summary">
          <span className="checkin-stat success"><Check size={14} />{claimedCount} 已签到</span>
          <span className="checkin-stat muted"><CalendarCheck size={14} />{availableCount} 未签到</span>
          {inactiveCount > 0 && <span className="checkin-stat muted"><Ban size={14} />{inactiveCount} 不可用</span>}
        </div>
        <div className="checkin-actions">
          <button className="button ghost compact" onClick={() => { void fetchAll(); }} disabled={refreshing || checkingAll}><RefreshCw size={14} className={refreshing ? 'spin' : ''} />刷新状态</button>
          <button className="button primary compact" onClick={() => { void runCheckinAll(); }} disabled={checkingAll || refreshing || availableCount === 0}><Gift size={14} />一键签到</button>
        </div>
      </div>
      <div className="checkin-list">
        {accounts.length === 0 ? <div className="empty-inline">还没有账号，添加 CodeBuddy 账号后即可签到。</div> : accounts.map((account) => {
          const state = states[account.id];
          const uiState = state?.uiState ?? 'loading';
          const streak = state?.status?.streakDays ?? 0;
          const credit = state?.status?.todayCredit ?? state?.status?.dailyCredit ?? 0;
          return <div className="checkin-row" key={account.id}>
            <div className="checkin-account"><span className="account-avatar">{account.email.slice(0, 1).toUpperCase()}</span><div className="checkin-account-name"><strong>{account.email}</strong>{streak > 0 && <small><Flame size={12} />连续 {streak} 天</small>}</div></div>
            <div className="checkin-status">{uiState === 'loading' ? <StatusPill tone="muted">查询中…</StatusPill> : uiState === 'claimed' ? <StatusPill tone="success">已签到</StatusPill> : uiState === 'available' ? <StatusPill tone="muted">未签到</StatusPill> : uiState === 'inactive' ? <StatusPill tone="muted">不可用</StatusPill> : <StatusPill tone="danger">查询失败</StatusPill>}{credit > 0 && <span className="checkin-credit"><Gift size={12} />+{credit}</span>}</div>
            <div className="checkin-action">{state?.checkingIn ? <button className="button primary compact" disabled><RefreshCw size={14} className="spin" />签到中…</button> : uiState === 'available' ? <button className="button primary compact" onClick={() => { void runCheckin(account.id); }}><Gift size={14} />签到</button> : uiState === 'claimed' ? <button className="button ghost compact" disabled><Check size={14} />已领取</button> : uiState === 'error' ? <button className="button ghost compact" onClick={() => { void fetchAll(); }}>重试</button> : <button className="button ghost compact" disabled>不可用</button>}</div>
            {state?.error && <div className="checkin-row-error"><AlertTriangle size={12} />{state.error}</div>}
          </div>;
        })}
      </div>
    </div>
  </Modal>;
}

function ModelsPage({ state, notify }: { state: AppState; notify: NoticeHandler }) {
  const [models, setModels] = useState<ModelInfo[]>([]);
  const [query, setQuery] = useState('');
  const [syncing, setSyncing] = useState(false);
  const [lastSync, setLastSync] = useState<number | null>(null);
  const enabledKey = state.keys.find((key) => key.enabled)?.key;
  const sync = async () => {
    if (!state.running) { notify('请先启动反代服务，再从 CodeBuddy CN 后端同步模型'); return; }
    setSyncing(true);
    try {
      const port = state.actualPort ?? state.config.port;
      // 先让 sidecar 从 CodeBuddy CN 后端重新拉取模型并覆盖本地缓存，
      // 再读取更新后的 /v1/models 目录展示。
      await syncModels(port, enabledKey);
      const next = await listModels(port, enabledKey);
      setModels(next);
      setLastSync(Date.now());
      notify(`已从后端同步 ${next.length} 个模型`);
    } catch (reason) {
      notify(reason instanceof Error ? reason.message : String(reason));
    } finally { setSyncing(false); }
  };
  const filtered = useMemo(() => models.filter((model) => !query || model.id.toLowerCase().includes(query.toLowerCase())), [models, query]);
  return <><SectionHeader eyebrow="CodeBuddy / 能力目录" title="模型管理" description="从运行中的 CodeBuddy CN sidecar 获取模型目录和能力信息。" action={<div className="header-actions"><span className="sync-time"><RefreshCw size={13} />{lastSync ? `上次同步：${formatDate(lastSync)}` : '尚未同步'}</span><button className="button ghost" onClick={() => { void sync(); }} disabled={syncing}><RefreshCw size={15} />{syncing ? '同步中…' : '立即同步'}</button></div>} /><div className="model-notice"><Sparkles size={17} /><div><strong>模型目录来自 CodeBuddy CN 后端</strong><span>没有运行服务或有效 API Key 时，不会显示伪造的模型列表。</span></div><StatusPill tone={models.length ? 'success' : 'muted'}>{models.length ? '已同步' : '等待同步'}</StatusPill></div><div className="panel table-panel"><div className="table-toolbar"><div className="toolbar-title"><Layers3 size={17} /><strong>模型目录</strong><span>{filtered.length} 个模型</span></div><div className="search-box compact-search"><Search size={15} /><input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="搜索模型" /></div></div>{filtered.length ? <div className="data-table models-table"><div className="table-head"><span>模型</span><span>能力</span><span>可用状态</span><span>来源</span><span>别名</span><span /></div>{filtered.map((model) => { const capabilities = ['文本', ...(model.supportsImages || model.inputModalities?.includes('image') ? ['视觉'] : []), ...(model.supportsToolCall ? ['工具'] : [])]; return <div className="table-row" key={model.id}><div className="model-name"><span className="model-glyph"><Sparkles size={14} /></span><div><strong>{model.id}</strong><code>{model.ownedBy ?? 'codebuddy'}</code></div></div><div className="capability-list">{capabilities.map((capability) => <span key={capability} className={capability === '视觉' ? 'vision' : ''}>{capability}</span>)}</div><StatusPill tone="success">可用</StatusPill><span className="muted-text">CodeBuddy CN</span><button className="alias-button" onClick={() => notify('模型别名持久化命令尚未接入')}><span>未设置</span><Pencil size={13} /></button><IconButton label="模型详情" onClick={() => notify(`${model.id}：上下文 ${model.contextLength ?? '未知'}`)}><MoreHorizontal size={16} /></IconButton></div>; })}</div> : <EmptyState icon={Layers3} title="还没有模型目录" description="启动服务并点击“立即同步”，从 CodeBuddy CN 后端读取模型。" action={<button className="button primary" onClick={() => { void sync(); }} disabled={syncing}><RefreshCw size={15} />同步模型</button>} />}</div><div className="model-footnote"><span><Eye size={14} />视觉能力由在线模型目录与实测校正表决定。</span></div></>;
}

function SettingsPage({ onReset, notify }: { onReset: () => void; notify: NoticeHandler }) {
  const [tab, setTab] = useState<'general' | 'network' | 'data' | 'about'>('general');
  const [prefs, setPrefs] = useState(() => { try { return JSON.parse(localStorage.getItem('coderelay-preferences') ?? '{}') as { openOverview?: boolean; refreshAccounts?: boolean; closeBehavior?: string; retention?: string; theme?: ThemeMode }; } catch { return {}; } });
  const update = (changes: Partial<typeof prefs>) => setPrefs((current) => ({ ...current, ...changes }));
  const changeTheme = (theme: ThemeMode) => { update({ theme }); applyTheme(theme); };
  const save = () => { localStorage.setItem('coderelay-preferences', JSON.stringify(prefs)); applyTheme(prefs.theme ?? 'system'); notify('应用设置已保存'); };
  return <><SectionHeader eyebrow="应用 / 偏好" title="设置" description="调整 CodeRelay 的桌面行为、数据保留和隐私选项。" action={<button className="button primary" onClick={save}><Check size={15} />保存设置</button>} /><div className="settings-layout"><div className="settings-tabs">{([['general', '常规', Settings2], ['network', '网络', Network], ['data', '数据与隐私', Database], ['about', '关于', CircleHelp]] as Array<[typeof tab, string, LucideIcon]>).map(([id, label, Icon]) => <button key={id} className={tab === id ? 'active' : ''} onClick={() => setTab(id)}><Icon size={16} />{label}</button>)}</div><div className="panel settings-panel">{tab === 'general' && <><div className="settings-section"><h3>启动行为</h3><Toggle label="启动时打开总览" description="软件启动后默认显示总览页。" checked={prefs.openOverview ?? true} onChange={(value) => update({ openOverview: value })} /><Toggle label="启动时自动刷新账号额度" description="启动后读取最近保存的账号并刷新配额。" checked={prefs.refreshAccounts ?? false} onChange={(value) => update({ refreshAccounts: value })} /></div><div className="settings-section"><h3>外观</h3><Field label="主题模式" hint="选择后立即预览，点击“保存设置”持久化。"><div className="segmented theme-segmented"><button className={(prefs.theme ?? 'system') === 'system' ? 'active' : ''} onClick={() => changeTheme('system')}>跟随系统</button><button className={(prefs.theme ?? 'system') === 'light' ? 'active' : ''} onClick={() => changeTheme('light')}>浅色</button><button className={(prefs.theme ?? 'system') === 'dark' ? 'active' : ''} onClick={() => changeTheme('dark')}>深色</button></div></Field></div><div className="settings-section"><h3>关闭窗口</h3><Field label="服务运行时点击关闭" hint="此设置用于后续窗口关闭流程"><select value={prefs.closeBehavior ?? 'ask'} onChange={(e) => update({ closeBehavior: e.target.value })}><option value="ask">每次询问</option><option value="tray">最小化到系统托盘</option><option value="exit">停止服务后退出</option></select></Field></div></>}{tab === 'network' && <div className="settings-section"><h3>网络安全</h3><p className="settings-note"><ShieldCheck size={15} />默认监听 localhost。局域网入口需要在“服务配置”中单独开启，所有请求仍需有效 API Key。</p></div>}{tab === 'data' && <div className="settings-section"><h3>本地数据</h3><Field label="请求日志保留时间"><select value={prefs.retention ?? '7'} onChange={(e) => update({ retention: e.target.value })}><option value="7">最近 7 天</option><option value="30">最近 30 天</option></select></Field><div className="danger-zone"><div><h3>重置浏览器预览数据</h3><p>仅清理当前 Web 预览中的本地状态，不会删除桌面端凭据文件。</p></div><button className="button danger-button" onClick={onReset}><Trash2 size={15} />重置数据</button></div></div>}{tab === 'about' && <div className="about-block"><div className="about-logo">CR</div><h3>CodeRelay</h3><p>面向高级用户的 CodeBuddy CN 账号池和本地 OpenAI 兼容反代管理工具。</p><div className="about-meta"><span>版本 {APP_VERSION}</span><span>Windows 桌面端</span><span>本地优先</span></div><button className="inline-link" onClick={() => notify('第三方组件许可见项目根目录 NOTICE.md')}>查看第三方许可 <span>→</span></button></div>}</div></div></>;
}

interface ParsedAccount {
  account: Account;
  source: string;
}

interface ImportItem extends ParsedAccount {
  selected: boolean;
  duplicate: boolean;
}

type OAuthPhase = 'idle' | 'starting' | 'waiting' | 'success' | 'error';

type TokenValidation =
  | { state: 'idle' }
  | { state: 'checking' }
  | { state: 'ok'; result: OAuthCompleteResponse }
  | { state: 'error'; message: string };

function normalizeImportedAccounts(value: unknown): ParsedAccount[] {
  const root = value as { accounts?: unknown; data?: { accounts?: unknown } } | unknown[];
  const candidates = Array.isArray(root) ? root : Array.isArray(root?.accounts) ? root.accounts : Array.isArray(root?.data?.accounts) ? root.data.accounts : [root];
  return candidates.flatMap((entry, index) => {
    if (!entry || typeof entry !== 'object') return [];
    const item = entry as Record<string, unknown>;
    const accessToken = String(item.access_token ?? item.accessToken ?? item.token ?? '').trim();
    if (!accessToken) return [];
    const email = String(item.email ?? item.account_email ?? item.accountEmail ?? item.label ?? `待识别账号 ${index + 1}`).trim();
    const id = String(item.id ?? item.uid ?? `cn-${Date.now()}-${index}`).trim();
    return [{ source: email, account: { id, email, region: 'cn', plan: String(item.plan ?? item.planType ?? 'UNKNOWN'), status: 'needs_auth', quota: Number(item.quota ?? item.remainingQuota ?? 0), quotaTotal: Number(item.quotaTotal ?? 0), lastUsed: null, failures: 0, tags: [], accessToken, refreshToken: typeof item.refresh_token === 'string' ? item.refresh_token : typeof item.refreshToken === 'string' ? item.refreshToken : undefined, uid: typeof item.uid === 'string' ? item.uid : undefined, enterpriseId: typeof item.enterprise_id === 'string' ? item.enterprise_id : typeof item.enterpriseId === 'string' ? item.enterpriseId : undefined, domain: typeof item.domain === 'string' ? item.domain : undefined } }];
  });
}

function AccountModal({ existingAccounts, initialMode = 'browser', onClose, onSave, notify }: { existingAccounts: Account[]; initialMode?: 'browser' | 'token' | 'file'; onClose: () => void; onSave: (accounts: Account[], summary?: string) => void; notify: NoticeHandler }) {
  const [mode, setMode] = useState<'browser' | 'token' | 'file'>(initialMode);
  useEffect(() => setMode(initialMode), [initialMode]);
  const [email, setEmail] = useState('');
  const [token, setToken] = useState('');
  const [showToken, setShowToken] = useState(false);
  const [imported, setImported] = useState<ImportItem[]>([]);
  const [fileName, setFileName] = useState('');
  const [phase, setPhase] = useState<OAuthPhase>('idle');
  const [oauthError, setOauthError] = useState<string | null>(null);
  const [oauthResult, setOauthResult] = useState<OAuthCompleteResponse | null>(null);
  const [tokenValidation, setTokenValidation] = useState<TokenValidation>({ state: 'idle' });
  const loginRef = useRef<string | null>(null);
  const tokenLooksValid = token.trim().length >= 20;

  useEffect(() => () => { if (loginRef.current) void cancelOAuth(loginRef.current); }, []);

  const beginAuth = async () => {
    if (phase === 'starting' || phase === 'waiting') return;
    setOauthError(null);
    setOauthResult(null);
    setPhase('starting');
    try {
      const start = await startOAuth();
      loginRef.current = start.loginId;
      try {
        await openExternal(start.verificationUri);
      } catch {
        notify('无法打开系统浏览器，请手动访问授权页完成登录');
      }
      setPhase('waiting');
      const result = await completeOAuth(start.loginId);
      loginRef.current = null;
      setOauthResult(result);
      setPhase('success');
    } catch (reason) {
      loginRef.current = null;
      const message = reason instanceof Error ? reason.message : String(reason);
      if (message.includes('已取消')) {
        setPhase('idle');
      } else {
        setOauthError(message);
        setPhase('error');
      }
    }
  };

  const cancelAuth = async () => {
    const loginId = loginRef.current;
    loginRef.current = null;
    if (loginId) await cancelOAuth(loginId).catch(() => undefined);
    setPhase('idle');
  };

  const closeModal = () => {
    if (loginRef.current) void cancelOAuth(loginRef.current);
    onClose();
  };

  const runValidate = async () => {
    const value = token.trim();
    if (!value || !tokenLooksValid) {
      setTokenValidation({ state: 'error', message: 'Token 不能为空，并且长度不能过短' });
      return;
    }
    setTokenValidation({ state: 'checking' });
    try {
      const result = await validateToken(value);
      setTokenValidation({ state: 'ok', result });
    } catch (reason) {
      setTokenValidation({ state: 'error', message: reason instanceof Error ? reason.message : String(reason) });
    }
  };

  const handleFile = async (file: File | undefined) => {
    if (!file) return;
    setFileName(file.name);
    try {
      const parsed = normalizeImportedAccounts(JSON.parse(await file.text()));
      const existingIds = new Set(existingAccounts.map((account) => account.id));
      const existingEmails = new Set(existingAccounts.map((account) => account.email.trim().toLowerCase()).filter(Boolean));
      setImported(parsed.map((entry) => {
        const duplicate = existingIds.has(entry.account.id) || (entry.account.email.trim().length > 0 && existingEmails.has(entry.account.email.trim().toLowerCase()));
        return { ...entry, duplicate, selected: !duplicate };
      }));
      if (!parsed.length) notify('文件中没有识别到包含 access_token 的账号');
    } catch {
      setImported([]);
      notify('配置文件不是有效 JSON，或格式不受支持');
    }
  };

  const submit = () => {
    if (mode === 'browser') {
      if (phase === 'success' && oauthResult) {
        const result = oauthResult;
        const account: Account = {
          id: result.uid ? `cn-${result.uid}` : `cn-${Date.now()}`,
          email: result.email || '未命名账号',
          region: 'cn',
          plan: 'UNKNOWN',
          status: 'available',
          quota: 0,
          quotaTotal: 0,
          lastUsed: null,
          failures: 0,
          tags: [],
          accessToken: result.accessToken,
          refreshToken: result.refreshToken,
          uid: result.uid,
          enterpriseId: result.enterpriseId,
          domain: result.domain,
        };
        onSave([account], `已通过浏览器认证添加账号 ${account.email}`);
        return;
      }
      void beginAuth();
      return;
    }
    if (mode === 'file') {
      const selected = imported.filter((entry) => entry.selected && !entry.duplicate);
      if (!selected.length) { notify('请先选择并预览有效的账号配置文件'); return; }
      const skipped = imported.length - selected.length;
      const base = skipped ? `已导入 ${selected.length} 个账号，跳过 ${skipped} 个重复或未选择项` : `已导入 ${selected.length} 个账号`;
      onSave(selected.map((entry) => entry.account), `${base}，点击“全部刷新”恢复额度信息`);
      return;
    }
    const value = token.trim();
    if (!value || !tokenLooksValid) { notify('Token 不能为空，并且长度不能过短'); return; }
    const validated = tokenValidation.state === 'ok' ? tokenValidation.result : undefined;
    const account: Account = {
      id: validated?.uid ? `cn-${validated.uid}` : `cn-${Date.now()}`,
      email: validated?.email || email.trim() || '待识别账号',
      region: 'cn',
      plan: 'UNKNOWN',
      status: validated ? 'available' : 'needs_auth',
      quota: 0,
      quotaTotal: 0,
      lastUsed: null,
      failures: 0,
      tags: [],
      accessToken: value,
      uid: validated?.uid,
      enterpriseId: validated?.enterpriseId,
      domain: validated?.domain,
    };
    onSave([account], validated ? `已验证并添加账号 ${account.email}` : '已保存待验证账号，首次请求时由服务验证');
  };

  const selectedImportCount = imported.filter((entry) => entry.selected && !entry.duplicate).length;
  const duplicateImportCount = imported.filter((entry) => entry.duplicate).length;
  const primaryLabel = mode === 'browser'
    ? phase === 'success' ? '保存账号' : phase === 'waiting' || phase === 'starting' ? '等待认证完成…' : '发起认证'
    : mode === 'token'
      ? tokenValidation.state === 'ok' ? '保存已验证账号' : '保存账号'
      : `确认导入${selectedImportCount ? `（${selectedImportCount}）` : ''}`;
  const primaryDisabled = (mode === 'browser' && (phase === 'starting' || phase === 'waiting')) || (mode === 'file' && !selectedImportCount) || (mode === 'token' && !tokenLooksValid);

  return <Modal title="添加 CodeBuddy 账号" onClose={closeModal} wide><div className="modal-split"><div className="modal-methods"><button className={mode === 'browser' ? 'active' : ''} onClick={() => setMode('browser')}><Globe2 size={16} /><span>浏览器认证</span><small>OAuth / 网页登录</small></button><button className={mode === 'token' ? 'active' : ''} onClick={() => setMode('token')}><KeyRound size={16} /><span>手动粘贴 Token</span></button><button className={mode === 'file' ? 'active' : ''} onClick={() => setMode('file')}><FolderOpen size={16} /><span>导入配置文件</span></button></div><div className="modal-method-content">{mode === 'browser' && <div className="method-content"><span className="large-method-icon"><Globe2 size={24} /></span><h3>通过 CodeBuddy CN 完成浏览器认证</h3><p>选择下面任一方式，CodeRelay 会在系统浏览器中打开 CodeBuddy CN 官方授权页。完成登录后凭据自动回收并保存，不需要手动复制 Token。</p>{phase === 'idle' && <div className="oauth-actions"><button className="button primary" onClick={() => { void beginAuth(); }}><KeyRound size={15} />OAuth 授权</button><button className="button ghost" onClick={() => { void beginAuth(); }}><Globe2 size={15} />网页登录</button></div>}{phase === 'starting' && <div className="oauth-status"><span className="pulse-dot" /><span>正在向 CodeBuddy CN 发起认证…</span></div>}{phase === 'waiting' && <div className="oauth-status"><span className="pulse-dot" /><span>已在系统浏览器打开授权页，等待完成登录（10 分钟内有效）…</span><button className="button ghost" onClick={() => { void cancelAuth(); }}>取消认证</button></div>}{phase === 'success' && oauthResult && <div className="import-preview"><strong>认证成功，请确认账号信息</strong><span><Check size={13} />账号：{oauthResult.email || '未命名账号'}</span>{oauthResult.uid && <span><Check size={13} />UID：{oauthResult.uid}</span>}{oauthResult.enterpriseId && <span><Check size={13} />企业：{oauthResult.enterpriseId}</span>}</div>}{phase === 'error' && <div className="inline-error"><AlertTriangle size={15} /><span>{oauthError ?? '认证失败，请重试'}</span><button className="button ghost" onClick={() => { void beginAuth(); }}>重试</button></div>}</div>}{mode === 'token' && <div className="method-content"><span className="large-method-icon"><KeyRound size={24} /></span><h3>粘贴 CodeBuddy Token</h3><p>Token 只写入桌面端凭据文件。建议先点击“验证 Token”读取账号信息，再保存。</p><label className="field"><span>Token</span><div className="input-with-action"><textarea value={token} onChange={(e) => { setToken(e.target.value.trimStart()); setTokenValidation({ state: 'idle' }); }} placeholder="粘贴 Token" rows={4} style={{ WebkitTextSecurity: showToken ? 'none' : 'disc' } as CSSProperties} /><IconButton label={showToken ? '隐藏 Token' : '显示 Token'} onClick={() => setShowToken((value) => !value)}>{showToken ? <EyeOff size={15} /> : <Eye size={15} />}</IconButton></div><small>{tokenLooksValid ? '已完成基本格式检查，可执行验证。' : '请粘贴完整 Token。'}</small></label><div className="oauth-actions"><button className="button ghost" disabled={!tokenLooksValid || tokenValidation.state === 'checking'} onClick={() => { void runValidate(); }}><ShieldCheck size={15} />{tokenValidation.state === 'checking' ? '验证中…' : '验证 Token'}</button></div>{tokenValidation.state === 'ok' && <div className="import-preview"><strong>验证成功</strong><span><Check size={13} />账号：{tokenValidation.result.email || '未命名账号'}</span>{tokenValidation.result.uid && <span><Check size={13} />UID：{tokenValidation.result.uid}</span>}{tokenValidation.result.enterpriseId && <span><Check size={13} />企业：{tokenValidation.result.enterpriseId}</span>}</div>}{tokenValidation.state === 'error' && <div className="inline-error"><AlertTriangle size={15} /><span>{tokenValidation.message}</span></div>}<label className="field"><span>账号邮箱（可选）</span><input value={email} onChange={(e) => setEmail(e.target.value)} placeholder="未验证 Token 时用于列表展示" /></label></div>}{mode === 'file' && <div className="method-content"><span className="large-method-icon"><FileJson size={24} /></span><h3>导入已有账号配置</h3><p>支持包含 accounts 数组或单个账号对象的 JSON。与现有账号重复的条目会自动标记并默认跳过。</p><label className="drop-zone"><Upload size={22} /><strong>{fileName || '选择 JSON 文件'}</strong><span>不会在选择文件时自动写入</span><input type="file" accept=".json,application/json" onChange={(e) => { void handleFile(e.target.files?.[0]); }} /></label>{imported.length > 0 && <div className="import-preview"><strong>导入预览：共 {imported.length} 项，将导入 {selectedImportCount} 项{duplicateImportCount ? `，${duplicateImportCount} 项重复已跳过` : ''}</strong>{imported.map((entry, index) => <label key={entry.account.id}><input type="checkbox" disabled={entry.duplicate} checked={entry.selected && !entry.duplicate} onChange={(e) => setImported((items) => items.map((item, itemIndex) => itemIndex === index ? { ...item, selected: e.target.checked } : item))} /><span>{entry.duplicate ? `重复 · ${entry.source}` : entry.source}</span></label>)}</div>}</div>}</div></div><div className="modal-footer"><button className="button ghost" onClick={closeModal}>取消</button><button className="button primary" disabled={primaryDisabled} onClick={submit}>{primaryLabel}</button></div></Modal>;
}

function KeyModal({ accounts, existingKey, onClose, onSave }: { accounts: Account[]; existingKey?: ApiKey | null; onClose: () => void; onSave: (key: ApiKey) => void }) {
  const editing = Boolean(existingKey);
  const [name, setName] = useState(existingKey?.name ?? '');
  const [scope, setScope] = useState<'all' | 'selected'>(existingKey?.accountIds ? 'selected' : 'all');
  const [selected, setSelected] = useState<string[]>(existingKey?.accountIds?.filter((id) => accounts.some((account) => account.id === id)) ?? []);
  const submit = () => {
    if (scope === 'selected' && !selected.length) return;
    onSave({
      ...(existingKey ?? { id: `key-${Date.now()}`, key: `sk-coderelay-${crypto.randomUUID?.() ?? Math.random().toString(36).slice(2)}`, enabled: true, models: [], createdAt: Date.now(), lastUsed: null }),
      name: name.trim() || '未命名 Key',
      accountIds: scope === 'all' ? null : selected,
    });
  };
  return <Modal title={editing ? '编辑 API Key' : '创建 API Key'} onClose={onClose}><div className="modal-form"><p className="modal-lead">{editing ? '调整此 Key 的账号使用范围。Key 值保持不变，修改后立即对使用它的客户端生效。' : '为本地客户端创建新的访问凭据。完整 Key 创建后会显示在列表中，并支持直接复制。'}</p><Field label="Key 名称"><input autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="例如：个人开发" /></Field><Field label="账号使用范围"><div className="scope-options"><button className={scope === 'all' ? 'active' : ''} onClick={() => setScope('all')}><Globe2 size={15} /><span>全部可用账号</span><small>自动调度整个账号池</small></button><button className={scope === 'selected' ? 'active' : ''} onClick={() => setScope('selected')}><Users size={15} /><span>指定账号</span><small>仅使用你选择的账号</small></button></div></Field>{scope === 'selected' && (accounts.length ? <><div className="checklist-toolbar"><span>已选 {selected.length} / {accounts.length} 个账号</span><button type="button" className="inline-link" onClick={() => setSelected(accounts.map((account) => account.id))}>全选</button><button type="button" className="inline-link" onClick={() => setSelected([])}>清空</button></div><div className="account-checklist">{accounts.map((account) => <label key={account.id}><input type="checkbox" checked={selected.includes(account.id)} onChange={(e) => setSelected(e.target.checked ? [...selected, account.id] : selected.filter((id) => id !== account.id))} /><span>{account.email}</span><small>{account.plan}</small></label>)}</div></> : <p className="checklist-empty">账号池还没有账号。请先在账号池中添加账号，再回来限定 Key 的使用范围。</p>)}</div><div className="modal-footer"><button className="button ghost" onClick={onClose}>取消</button><button className="button primary" disabled={scope === 'selected' && !selected.length} onClick={submit}>{editing ? <><Check size={15} />保存修改</> : <><Plus size={15} />创建 Key</>}</button></div></Modal>;
}

function Modal({ title, onClose, children, wide = false }: { title: string; onClose: () => void; children: ReactNode; wide?: boolean }) { return <div className="modal-scrim" role="dialog" aria-modal="true" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}><div className={`modal ${wide ? 'wide' : ''}`}><div className="modal-header"><div><span className="eyebrow">CodeRelay</span><h2>{title}</h2></div><IconButton label="关闭" onClick={onClose}><X size={17} /></IconButton></div>{children}</div></div>; }

import { useEffect, useMemo, useRef, useState } from "react";
import {
  CaretLeft,
  CaretRight,
  Check,
  Copy,
  EyeSlash,
  FolderSimple,
  PushPinSimple,
  X,
} from "@phosphor-icons/react";
import { modelDisplayName } from "./modelNames.js";
import { compactTokens } from "./tokenFormat.js";
import { formatDate, formatTime, t } from "./i18n.js";
import { exportCsvFile, getProjectRules, setProjectRules } from "./usageClient";
import { AGENT_META, AGENT_ORDER } from "./agentMeta.jsx";
import { formatUsd, PERIODS } from "./viewParts.jsx";
import { feedbackText } from "./SettingsSection.jsx";

function sessionDayLabel(ms) {
  const date = new Date(ms);
  const today = new Date();
  const startOfDay = (d) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const diffDays = Math.round((startOfDay(today) - startOfDay(date)) / 86_400_000);
  if (diffDays === 0) return t("今日");
  if (diffDays === 1) return t("昨日");
  return formatDate(date, { month: "long", day: "numeric" });
}

// 项目名、路径、模型名等文本可能以 = + - @ 开头，表格软件会当公式执行：
// 非数值文本加 ' 前缀中和；数值（含负数）原样输出。
function csvEscape(value) {
  let text = String(value ?? "");
  if (typeof value === "string" && /^[=+\-@\t\r]/.test(text) && !/^[-+]?\d+(\.\d+)?$/.test(text)) {
    text = `'${text}`;
  }
  return /[",\r\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
}

// 带 BOM，Excel 才能正确识别 UTF-8。
function toCsv(header, rows) {
  return `﻿${[header, ...rows].map((row) => row.map(csvEscape).join(",")).join("\r\n")}`;
}

// 导出只含账本本就存储的统计字段，与隐私边界一致。
function buildSessionsCsv(sessions) {
  const header = ["date", "start", "end", "agent", "model", "project", "project_path", "tokens", "input_uncached", "cache_read", "cache_write", "output", "estimated_usd", "events", "session_id"];
  const rows = sessions.map((session) => [
    new Date(session.endMs).toLocaleDateString("sv-SE"),
    new Date(session.startMs).toLocaleTimeString("zh-CN", { hour12: false }),
    new Date(session.endMs).toLocaleTimeString("zh-CN", { hour12: false }),
    session.agent,
    session.model || "",
    session.projectLabel || "",
    session.project || "",
    session.tokens,
    session.inputUncached,
    session.cacheRead,
    session.cacheWrite,
    session.output,
    session.usd == null ? "" : session.usd.toFixed(4),
    session.eventCount,
    session.sessionId,
  ]);
  return toCsv(header, rows);
}

async function saveCsv(fileName, csv) {
  // 桌面端：blob 下载在 Tauri WebView 里不生效，改走后端写入下载目录。
  const savedPath = await exportCsvFile(fileName, csv);
  if (savedPath) return savedPath;
  // 浏览器演示模式退回常规下载。
  const url = URL.createObjectURL(new Blob([csv], { type: "text/csv;charset=utf-8" }));
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = fileName;
  document.body.appendChild(anchor);
  anchor.click();
  anchor.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
  return null;
}

async function exportSessionsCsv(sessions) {
  return saveCsv(
    `metrik-sessions-${new Date().toLocaleDateString("sv-SE")}.csv`,
    buildSessionsCsv(sessions),
  );
}

// 会话行与项目行的项目名只显示目录名，完整路径放 title。
function projectLabel(path) {
  const parts = String(path).split("/").filter(Boolean);
  return parts[parts.length - 1] || path;
}

// 添加项目：登记项目根、隐藏目录，列出已有规则并可移除。
function ProjectRulesCard({ rules, busy, onAddRoot, onRemoveRoot, onRemoveHidden, onClose }) {
  const [draft, setDraft] = useState("");
  const submit = () => {
    const value = draft.trim();
    if (!value) return;
    onAddRoot(value);
    setDraft("");
  };
  return (
    <section className="rules-card" aria-label={t("添加项目")}>
      <header className="rules-card-head">
        <h2>{t("添加项目")}</h2>
        <button type="button" className="rules-close" onClick={onClose} aria-label={t("收起添加项目")}>
          <X size={14} weight="bold" aria-hidden="true" />
        </button>
      </header>
      <p>
        {t("决定哪些目录算作一个项目。默认按 git 仓库根归并，家目录、下载与系统临时目录不列为项目。 账本始终记录事件发生时的原始目录，这里只改变展示时的归类，移除后即恢复。")}
      </p>
      <div className="rules-add">
        <input
          value={draft}
          placeholder={t("登记项目根目录，其下用量归并为一个项目")}
          spellCheck={false}
          onChange={(event) => setDraft(event.target.value)}
          onKeyDown={(event) => { if (event.key === "Enter") submit(); }}
          aria-label={t("项目根目录路径")}
        />
        <button type="button" className="ledger-button" disabled={!draft.trim() || busy} onClick={submit}>
          {t("登记")}
        </button>
      </div>
      {!rules ? (
        <p className="settings-muted">{t("正在读取规则…")}</p>
      ) : (
        <>
          {rules.roots.length > 0 && (
            <div className="rules-group">
              <h3>{t("项目根 · 子目录归并至此")}</h3>
              <ul>
                {rules.roots.map((path) => (
                  <li key={path}>
                    <PushPinSimple size={12} weight="fill" aria-hidden="true" />
                    <span title={path}>{path}</span>
                    <button type="button" disabled={busy} onClick={() => onRemoveRoot(path)} aria-label={t("移除项目根 {path}", { path })}>
                      <X size={12} weight="bold" aria-hidden="true" />
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}
          {rules.hidden.length > 0 && (
            <div className="rules-group">
              <h3>{t("已隐藏 · 不作为项目展示")}</h3>
              <ul>
                {rules.hidden.map((path) => (
                  <li key={path}>
                    <EyeSlash size={12} weight="regular" aria-hidden="true" />
                    <span title={path}>{path}</span>
                    <button type="button" disabled={busy} onClick={() => onRemoveHidden(path)} aria-label={t("取消隐藏 {path}", { path })}>
                      <X size={12} weight="bold" aria-hidden="true" />
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}
          {rules.roots.length === 0 && rules.hidden.length === 0 && (
            <p className="settings-muted">{t("暂无手动归类规则。可在项目行上点击图钉或眼睛图标，或在上方登记目录。")}</p>
          )}
        </>
      )}
    </section>
  );
}

const PROJECT_PREVIEW_COUNT = 8;

// 项目分类色：经 CVD/对比度校验的固定顺序（styles.css 里的 --viz-1..6），
// 按周期内 token 排名前 6 依序分配，之后统一入"其他"灰。颜色跟随项目路径，
// Agent 筛选不重新分配，避免筛选后颜色跳变。
export const PROJECT_COLOR_COUNT = 6;

function projectColorMap(projects) {
  const map = new Map();
  projects.slice(0, PROJECT_COLOR_COUNT).forEach((project, index) => {
    map.set(project.path, `var(--viz-${index + 1})`);
  });
  return map;
}

// 项目总表导出：与账本口径一致的统计字段。
function buildProjectsCsv(projects) {
  const header = ["project", "path", "agents", "model", "tokens", "input_uncached", "cache_read", "cache_write", "output", "estimated_usd", "sessions", "events", "last_used"];
  const rows = projects.map((project) => [
    project.label,
    project.path,
    project.agents.join(" "),
    project.model || "",
    project.tokens,
    project.inputUncached,
    project.cacheRead,
    project.cacheWrite,
    project.output,
    project.usd == null ? "" : project.usd.toFixed(4),
    project.sessionCount,
    project.eventCount,
    new Date(project.lastMs).toLocaleString("sv-SE"),
  ]);
  return toCsv(header, rows);
}

async function exportProjectsCsv(projects) {
  return saveCsv(
    `metrik-projects-${new Date().toLocaleDateString("sv-SE")}.csv`,
    buildProjectsCsv(projects),
  );
}

// 项目占比环形图：与报告页 Agent 占比同一视觉语言；段间留白 2.5，
// 点击分段进入对应项目详情。"其他"聚合段不可点。
function ProjectShareDonut({ projects, colorByPath, onOpen }) {
  const total = projects.reduce((sum, project) => sum + project.tokens, 0) || 1;
  const top = projects.slice(0, PROJECT_COLOR_COUNT);
  const otherTokens = projects.slice(PROJECT_COLOR_COUNT).reduce((sum, project) => sum + project.tokens, 0);
  const segment = (key, label, tokens, color, onClick) => ({
    key,
    tokens,
    color,
    className: onClick ? "project-donut-segment" : undefined,
    onClick,
    title: `${label} · ${compactTokens(tokens)} · ${((tokens / total) * 100).toFixed(1)}%`,
  });
  const segments = [
    ...top.map((project) =>
      segment(project.path, project.label, project.tokens, colorByPath.get(project.path), () => onOpen(project.path)),
    ),
    ...(otherTokens > 0 ? [segment("__other", t("其他"), otherTokens, "var(--viz-other)")] : []),
  ];
  return (
    <ShareDonut
      className="project-donut"
      ariaLabel={t("项目用量占比环形图")}
      segments={segments}
      total={total}
      centerTokens={total}
      caption="tokens"
    />
  );
}

// 占比环形图：段间留白 2.5，从 12 点方向顺时针排列。
export function ShareDonut({ className, ariaLabel, segments, total, centerTokens, caption }) {
  const radius = 74;
  const circumference = 2 * Math.PI * radius;
  let offset = 0;
  return (
    <svg className={className} viewBox="0 0 200 200" role="img" aria-label={ariaLabel}>
      {segments.map((segment) => {
        const dash = (segment.tokens / total) * circumference;
        const rendered = (
          <circle
            key={segment.key}
            cx="100"
            cy="100"
            r={radius}
            fill="none"
            stroke={segment.color}
            strokeWidth="21"
            strokeDasharray={`${Math.max(0, dash - 2.5)} ${circumference - Math.max(0, dash - 2.5)}`}
            strokeDashoffset={-offset}
            transform="rotate(-90 100 100)"
            className={segment.className}
            onClick={segment.onClick}
          >
            {segment.title && <title>{segment.title}</title>}
          </circle>
        );
        offset += dash;
        return rendered;
      })}
      {/* 两行合起来在环心居中：基线放 96/114 时墨迹只到 77.4~114（数字与
          "tokens" 都没有下伸部），视觉中心落在 95.7，整体偏高约 4px。 */}
      <text x="100" y="100" textAnchor="middle" className="donut-total">{compactTokens(centerTokens)}</text>
      <text x="100" y="118" textAnchor="middle" className="donut-caption">{caption}</text>
    </svg>
  );
}

// 用量页：项目列表 → 点击进入单个项目的会话明细（层级下钻，不再同屏堆两个列表）。
// detail 为 null 时是项目列表；{ type:"project", path } 是项目详情；
// { type:"unattributed" } 是读不到目录的会话。
export function UsageSection({ projectsState, sessionsState, period, onRulesChanged }) {
  const [agentFilter, setAgentFilter] = useState("all");
  const [modelFilter, setModelFilter] = useState("all");
  const [detail, setDetail] = useState(null);
  const [showAllProjects, setShowAllProjects] = useState(false);
  const [copiedId, setCopiedId] = useState(null);
  const [note, setNote] = useState(null);
  const [rulesOpen, setRulesOpen] = useState(false);
  const [rules, setRules] = useState(null);
  const [rulesBusy, setRulesBusy] = useState(false);
  const rulesRef = useRef(null);
  const rulesQueueRef = useRef(Promise.resolve());

  useEffect(() => {
    if (rulesOpen && rules == null) {
      getProjectRules().then((loaded) => {
        const next = loaded.loadError ? { roots: [], hidden: [] } : loaded;
        rulesRef.current ??= next;
        setRules((current) => current ?? next);
      });
    }
  }, [rulesOpen, rules]);

  const projects = projectsState?.data;
  const sessionsData = sessionsState?.data;
  const colorByPath = useMemo(
    () => projectColorMap(projects?.projects || []),
    [projects],
  );

  // 正在看的项目因规则变更或换周期消失时，退回列表。
  useEffect(() => {
    if (detail?.type !== "project" || !projects || projects.loadError) return;
    if (!projects.projects.some((project) => project.path === detail.path)) {
      setDetail(null);
    }
  }, [detail, projects]);

  const openDetail = (next) => {
    setModelFilter("all");
    setNote(null);
    setDetail(next);
  };

  const applyRules = async (next) => {
    setRulesBusy(true);
    try {
      const saved = await setProjectRules(next);
      rulesRef.current = saved;
      setRules(saved);
      onRulesChanged();
    } catch (error) {
      setNote({ text: "规则保存失败：{error}", params: { error: `${error}` } });
    } finally {
      setRulesBusy(false);
    }
  };
  const currentRules = async () => {
    if (rulesRef.current) return rulesRef.current;
    const loaded = await getProjectRules();
    return loaded.loadError ? { roots: [], hidden: [] } : loaded;
  };
  // 规则改动排队串行执行：每次都以上一次保存的结果为底，
  // 连点两下不会读到同一份旧规则、后存的覆盖先存的。
  const updateRules = (change) => {
    const run = rulesQueueRef.current.then(async () => {
      await applyRules(change(await currentRules()));
    });
    rulesQueueRef.current = run.catch(() => {});
    return run;
  };
  const pinProject = (path) => updateRules((current) => ({ ...current, roots: [...current.roots, path] }));
  const removeRoot = (path) => updateRules((current) => ({
    ...current, roots: current.roots.filter((item) => item !== path),
  }));
  const removeHidden = (path) => updateRules((current) => ({
    ...current, hidden: current.hidden.filter((item) => item !== path),
  }));
  // 登记与取消登记同一颗按钮：图钉状态可逆，按钮也不会中途从 DOM 消失。
  const togglePin = (project) => (
    project.pinned ? removeRoot(project.path) : pinProject(project.path)
  );
  // 隐藏会让这一行消失，就地留一个撤销入口；规则面板仍是长期的恢复位置。
  const hideProject = async (project) => {
    await updateRules((current) => ({ ...current, hidden: [...current.hidden, project.path] }));
    setNote({
      text: "已隐藏 {project}",
      params: { project: project.label },
      undo: () => removeHidden(project.path),
    });
  };

  const noteExport = async (task) => {
    try {
      const savedPath = await task();
      setNote(savedPath ? { text: "已导出到 {path}", params: { path: savedPath } } : { text: "已开始下载" });
    } catch (error) {
      setNote({ text: "导出失败：{error}", params: { error: `${error}` } });
    }
  };
  const copySessionId = (sessionId) => {
    navigator.clipboard?.writeText(sessionId).then(() => {
      setCopiedId(sessionId);
      setTimeout(() => setCopiedId((current) => (current === sessionId ? null : current)), 1400);
    }).catch(() => {});
  };

  const loading = !projectsState || projectsState.status === "loading"
    || !sessionsState || sessionsState.status === "loading";
  if (loading) {
    return (
      <main className="usage-section" aria-busy="true">
        <header className="settings-header">
          <h1>{t("用量")}</h1>
          <p>{t("正在读取用量明细。只读取已索引的账本，不触发新的日志扫描。")}</p>
        </header>
      </main>
    );
  }
  if (!projects || projects.loadError || !sessionsData || sessionsData.loadError) {
    return (
      <main className="usage-section">
        <header className="settings-header">
          <h1>{t("用量")}</h1>
          <p>{t("本地账本读取失败，明细暂不可用；未以演示数据替代。稍后重试。")}</p>
        </header>
      </main>
    );
  }

  const timeRange = (session) => {
    const fmt = (ms) => formatTime(ms, { hour: "2-digit", minute: "2-digit", hour12: false });
    return `${fmt(session.startMs)}–${fmt(session.endMs)}`;
  };

  const sessionGroups = (sessions) => {
    const groups = [];
    sessions.forEach((session) => {
      const label = sessionDayLabel(session.endMs);
      const group = groups[groups.length - 1];
      if (group && group.label === label) group.sessions.push(session);
      else groups.push({ label, sessions: [session] });
    });
    return groups;
  };

  const renderSessionRows = (groups) => groups.map((group) => (
    <section className="session-group" key={group.label} aria-label={group.label}>
      <h3>{group.label}</h3>
      {group.sessions.map((session) => {
        const meta = AGENT_META[session.agent];
        return (
          <article className="session-row" key={`${session.agent}-${session.sessionId}`}>
            <i className="model-dot" style={{ backgroundColor: meta?.accent || "#74767a" }} aria-hidden="true" />
            <div className="session-copy">
              <strong>
                {timeRange(session)} · {meta?.label || session.agent}
                {session.model ? ` · ${modelDisplayName(session.model)}` : ""}
              </strong>
              <small>
                {compactTokens(session.tokens)} tokens
                {session.usd != null ? ` · ≈${formatUsd(session.usd)}` : ` · ${t("未计价")}`}
                {` · ${t("{count} 条记录", { count: session.eventCount })}`}
                {` · ${t("缓存读 {percent}%", { percent: session.tokens ? Math.round((session.cacheRead / session.tokens) * 100) : 0 })}`}
              </small>
            </div>
            <button
              type="button"
              className={`session-id-chip ${copiedId === session.sessionId ? "session-id-chip--copied" : ""}`}
              onClick={() => copySessionId(session.sessionId)}
              title={t("复制会话 ID（可用于 resume 等操作）\n{id}", { id: session.sessionId })}
            >
              {copiedId === session.sessionId
                ? <Check size={12} weight="bold" aria-hidden="true" />
                : <Copy size={12} weight="light" aria-hidden="true" />}
              <span>{session.sessionId.length > 14 ? `${session.sessionId.slice(0, 12)}…` : session.sessionId}</span>
            </button>
            <em>{compactTokens(session.tokens)}</em>
          </article>
        );
      })}
    </section>
  ));

  const noteBanner = note && (
    <p className="usage-note" role="status">
      {feedbackText(note)}
      {note.undo && (
        <button
          type="button"
          disabled={rulesBusy}
          onClick={() => { setNote(null); note.undo(); }}
        >
          {t("撤销")}
        </button>
      )}
    </p>
  );

  // ── 项目详情视图 ──
  if (detail) {
    const detailMeta = detail.type === "project"
      ? projects.projects.find((project) => project.path === detail.path)
      : null;
    const scoped = sessionsData.sessions.filter((session) => (
      detail.type === "project" ? session.project === detail.path : !session.project
    ));
    const models = [...new Set(scoped.map((session) => session.model).filter(Boolean))];
    const filtered = scoped.filter((session) => modelFilter === "all" || session.model === modelFilter);
    const groups = sessionGroups(filtered);
    const title = detail.type === "project" ? (detailMeta?.label || projectLabel(detail.path)) : t("未归类会话");

    return (
      <main className="usage-section" aria-labelledby="usage-title">
        <header className="settings-header">
          <button type="button" className="detail-back" onClick={() => openDetail(null)}>
            <CaretLeft size={12} weight="bold" aria-hidden="true" />
            {t("项目列表")}
          </button>
          <h1 id="usage-title">
            {detail.type === "project" && (
              <i
                className="project-title-dot"
                style={{ backgroundColor: colorByPath.get(detail.path) || "var(--viz-other)" }}
                aria-hidden="true"
              />
            )}
            {title}
          </h1>
          <p>
            {detail.type === "project" ? (
              <>
                {detail.path}
                {detailMeta ? ` · ${detailMeta.agents.map((id) => AGENT_META[id]?.label || id).join(t("、"))}` : ""}
                {` · ${t("{count} 个会话", { count: scoped.length })}`}
                {detailMeta?.usd != null ? ` · ${t("估算 ≈{usd}", { usd: formatUsd(detailMeta.usd) })}` : ""}
              </>
            ) : (
              <>{t("这些会话的来源不带工作目录（如 Cursor），无法归入项目。 {count} 个会话。", { count: scoped.length })}</>
            )}
          </p>
        </header>

        <div className="usage-toolbar">
          <select value={modelFilter} onChange={(event) => setModelFilter(event.target.value)} aria-label={t("按模型筛选")}>
            <option value="all">{t("全部模型")}</option>
            {/* 值必须是原始 ID：筛选按它比对 session.model，展示才走点号写法。 */}
            {models.map((model) => <option key={model} value={model}>{modelDisplayName(model)}</option>)}
          </select>
          <button
            type="button"
            className="ledger-button usage-toolbar-end"
            disabled={!filtered.length}
            onClick={() => noteExport(() => exportSessionsCsv(filtered))}
          >
            {t("导出会话 CSV（{count}）", { count: filtered.length })}
          </button>
        </div>

        {noteBanner}
        {groups.length === 0 && <p className="settings-muted">{t("本周期内暂无可显示的会话。")}</p>}
        {groups.length > 0 && <div className="report-card session-board">{renderSessionRows(groups)}</div>}
      </main>
    );
  }

  // ── 项目列表视图 ──
  const filteredProjects = projects.projects.filter((project) =>
    agentFilter === "all" || project.agents.includes(agentFilter));
  const selectedAgentLabel = agentFilter === "all" ? null : AGENT_META[agentFilter]?.label || agentFilter;
  const visibleProjects = showAllProjects
    ? filteredProjects
    : filteredProjects.slice(0, PROJECT_PREVIEW_COUNT);
  const maxTokens = filteredProjects.reduce((max, project) => Math.max(max, project.tokens), 0);
  const unattributedLabel = (projects.unattributedAgents || [])
    .map((id) => AGENT_META[id]?.label || id)
    .join(t("、"));

  return (
    <main className="usage-section" aria-labelledby="usage-title">
      <header className="settings-header">
        <h1 id="usage-title">{t("用量")}</h1>
        <p>
          <strong>{t("项目")}</strong> · {selectedAgentLabel
            ? t("{period}内 {count} 个包含 {agent} 用量的项目，点击项目查看会话明细。成本为按公开 API 价格的估算，非账单。", {
              period: t(PERIODS.find((item) => item.id === period)?.label),
              count: filteredProjects.length,
              agent: selectedAgentLabel,
            })
            : t("{period}内 {projects} 个项目、{sessions} 个会话，点击项目查看会话明细。成本为按公开 API 价格的估算，非账单。", {
              period: t(PERIODS.find((item) => item.id === period)?.label),
              projects: projects.totalProjects,
              sessions: sessionsData.totalSessions,
            })}
          {sessionsData.isDemo ? ` ${t("当前为浏览器演示数据。")}` : ""}
        </p>
      </header>

      <div className="usage-toolbar">
        <label className="usage-filter">
          <span>{t("包含 Agent")}</span>
          <select
            value={agentFilter}
            onChange={(event) => setAgentFilter(event.target.value)}
            aria-label={t("按 Agent 筛选项目")}
          >
            <option value="all">{t("全部 Agent")}</option>
            {AGENT_ORDER.map((id) => <option key={id} value={id}>{AGENT_META[id].label}</option>)}
          </select>
        </label>
        <button
          type="button"
          className={`ledger-button rules-toggle ${rulesOpen ? "rules-toggle--open" : ""}`}
          aria-expanded={rulesOpen}
          onClick={() => setRulesOpen((open) => !open)}
        >
          <FolderSimple size={13} weight="bold" aria-hidden="true" />
          {t("添加项目")}
        </button>
        <button
          type="button"
          className="ledger-button"
          disabled={!filteredProjects.length}
          onClick={() => noteExport(() => exportProjectsCsv(filteredProjects))}
        >
          {t("导出 CSV（{count}）", { count: filteredProjects.length })}
        </button>
      </div>

      {noteBanner}

      {rulesOpen && (
        <ProjectRulesCard
          rules={rules}
          busy={rulesBusy}
          onAddRoot={pinProject}
          onRemoveRoot={removeRoot}
          onRemoveHidden={removeHidden}
          onClose={() => setRulesOpen(false)}
        />
      )}

      <section className="report-card project-board" aria-label={t("项目汇总")}>
        {filteredProjects.length > 0 && (
          <div className="project-overview">
            <ProjectShareDonut
              projects={filteredProjects}
              colorByPath={colorByPath}
              onOpen={(path) => openDetail({ type: "project", path })}
            />
            <div className="project-overview-figures">
              {/* token 总量在环心；这里放成本这另一维度，同一个数不出现两次。 */}
              {(() => {
                const priced = filteredProjects.filter((project) => project.usd != null);
                if (!priced.length) {
                  return (
                    <>
                      <strong>{filteredProjects.length}</strong>
                      <span>{t("个项目 · 未计价", { count: filteredProjects.length })}</span>
                    </>
                  );
                }
                return (
                  <>
                    <strong>≈{formatUsd(priced.reduce((sum, project) => sum + project.usd, 0))}</strong>
                    <span>{t("{count} 个项目 · 估算成本", { count: filteredProjects.length })}</span>
                  </>
                );
              })()}
              {agentFilter === "all" && (projects.unattributedTokens > 0 || projects.hiddenTokens > 0) && (
                <small>
                  {projects.unattributedTokens > 0 && (
                    <button type="button" onClick={() => openDetail({ type: "unattributed" })}>
                      {t("未记录目录 {tokens}", { tokens: compactTokens(projects.unattributedTokens) })}{unattributedLabel ? t("（{agents}）", { agents: unattributedLabel }) : ""}
                    </button>
                  )}
                  {projects.hiddenTokens > 0 && (
                    // 这个数把用户隐藏的与内置排除（家目录、下载、系统临时目录）
                    // 算在一起，所以不能说"已隐藏"——用户把自己的规则删干净了，
                    // 剩下的内置部分仍会让它显示，读起来像没删掉。
                    <button type="button" onClick={() => setRulesOpen(true)}>
                      {t("未计入项目 {tokens}", { tokens: compactTokens(projects.hiddenTokens) })}
                    </button>
                  )}
                </small>
              )}
            </div>
          </div>
        )}
        {visibleProjects.length === 0 && (
          <p className="settings-muted">
            {projects.totalProjects === 0
              ? t("本周期内暂无带项目归属的用量。")
              : t("当前筛选条件下暂无项目。")}
          </p>
        )}
        {visibleProjects.map((project) => (
          <article
            className="project-row"
            key={project.path}
            onClick={(event) => {
              if (event.target.closest?.(".project-actions")) return;
              openDetail({ type: "project", path: project.path });
            }}
            role="button"
            tabIndex={0}
            onKeyDown={(event) => {
              if (event.key === "Enter" || event.key === " ") {
                event.preventDefault();
                openDetail({ type: "project", path: project.path });
              }
            }}
          >
            <div className="session-copy">
              <strong title={project.path}>{project.label}</strong>
              <small>
                {project.agents.map((id) => AGENT_META[id]?.label || id).join(" · ")}
                {project.model ? ` · ${modelDisplayName(project.model)}` : ""}
                {` · ${t("{count} 个会话", { count: project.sessionCount })}`}
                {project.usd != null ? ` · ≈${formatUsd(project.usd)}` : ` · ${t("未计价")}`}
              </small>
              <span className="project-bar" aria-hidden="true">
                <i
                  style={{
                    width: `${maxTokens ? Math.max(2, (project.tokens / maxTokens) * 100) : 0}%`,
                    background: colorByPath.get(project.path),
                  }}
                />
              </span>
            </div>
            {/* 操作区整体阻断冒泡：按钮若在点击途中卸载，mouseup 会落到行上误触发进入详情。 */}
            <div
              className={`project-actions ${project.pinned ? "project-actions--pinned" : ""}`}
              onClick={(event) => event.stopPropagation()}
              onMouseDown={(event) => event.stopPropagation()}
            >
              <button
                type="button"
                className={project.pinned ? "is-active" : ""}
                disabled={rulesBusy}
                aria-pressed={project.pinned}
                title={project.pinned
                  ? t("取消登记：{path} 下的子目录恢复各自成行", { path: project.path })
                  : t("登记为项目根：{path} 下的子目录一并归并到这一行", { path: project.path })}
                aria-label={project.pinned
                  ? t("取消登记项目根 {project}", { project: project.label })
                  : t("将 {project} 登记为项目根", { project: project.label })}
                onClick={() => togglePin(project)}
              >
                <PushPinSimple size={13} weight={project.pinned ? "fill" : "regular"} aria-hidden="true" />
              </button>
              <button
                type="button"
                disabled={rulesBusy}
                title={t("隐藏 {path}：其用量不再作为项目展示，可在项目归类中恢复", { path: project.path })}
                aria-label={t("隐藏项目 {project}", { project: project.label })}
                onClick={() => hideProject(project)}
              >
                <EyeSlash size={13} weight="regular" aria-hidden="true" />
              </button>
            </div>
            <span className="project-open" aria-hidden="true">
              <CaretRight size={13} weight="bold" />
            </span>
            <em>{compactTokens(project.tokens)}</em>
          </article>
        ))}
        {filteredProjects.length > PROJECT_PREVIEW_COUNT && (
          <button type="button" className="project-expand" onClick={() => setShowAllProjects((value) => !value)}>
            {showAllProjects ? t("收起") : t("显示全部 {count} 个项目", { count: filteredProjects.length })}
          </button>
        )}
      </section>
    </main>
  );
}

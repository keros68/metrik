import { useCallback, useEffect, useRef, useState } from "react";
import { Trash } from "@phosphor-icons/react";
import { CodexCreditsCard, QuotaAlertsCard } from "./QuotaSettings.jsx";
import { LanguageCard } from "./LanguageSettings.jsx";
import { formatDateTime, formatNumber, t } from "./i18n.js";
import {
  configureQoderCookie,
  configureSync,
  getAntigravityHookStatus,
  getClaudeHookStatus,
  getClaudeOauthStatus,
  getCursorUsageStatus,
  getGrokbotQuotaStatus,
  getQoderCookieStatus,
  getResponseSpeedStatus,
  getSyncSettings,
  removeSyncDevice,
  setAntigravityHook,
  setClaudeHook,
  setClaudeOauth,
  setCursorUsage,
  setGrokbotQuota,
  setResponseSpeedCollection,
} from "./usageClient";
import {
  checkForUpdate,
  getAutostart,
  installUpdate,
  isDesktop,
  setAutostart,
  UI_SCALE_RANGE,
} from "./windowClient";
import { IS_LINUX, IS_MAC, IS_WINDOWS } from "./platform.js";
import { AGENT_META, agentIdsInDisplayOrder, AgentMark } from "./agentMeta.jsx";
import {
  GLASS_INK_OPTIONS,
  GLASS_TINT_OPTIONS,
  PINNED_HOVER_OPTIONS,
  THEME_OPTIONS,
} from "./appearanceOptions.js";

// 反馈状态里，前端自己的文字存中文原文 text（渲染时翻译，切换语言也跟着变），
// 后端或异常带回的文字存 message（已是当前语言，原样显示）。
export function feedbackText(feedback) {
  return feedback.text ? t(feedback.text, feedback.params) : feedback.message;
}

function FeedbackLine({ feedback }) {
  if (!feedback) return null;
  return (
    <p
      className={`settings-feedback settings-feedback--${feedback.tone}`}
      role={feedback.tone === "error" ? "alert" : "status"}
    >
      {feedbackText(feedback)}
    </p>
  );
}

// 设置页里「读状态 + 一键开关」的数据来源共用这一套：首次读取状态，开关后
// 用返回的新状态替换并刷新快照。文案是中文原文，渲染时经 feedbackText 翻译。
function useSourceToggle({ load, save, loadError, enabledText, disabledText, onSnapshotRefresh }) {
  const [status, setStatus] = useState(null);
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState(null);

  useEffect(() => {
    let cancelled = false;
    load()
      .then((value) => {
        if (!cancelled) setStatus(value);
      })
      .catch(() => {
        if (!cancelled) setFeedback({ tone: "error", text: loadError });
      });
    return () => {
      cancelled = true;
    };
  }, [load, loadError]);

  const toggle = async (enabled) => {
    setBusy(true);
    setFeedback(null);
    try {
      setStatus(await save(enabled));
      setFeedback({ tone: "success", text: enabled ? enabledText : disabledText });
      onSnapshotRefresh();
    } catch (error) {
      setFeedback({ tone: "error", message: `${error}` });
    } finally {
      setBusy(false);
    }
  };

  return { status, busy, feedback, toggle };
}

function formatSyncTime(ms) {
  if (!Number.isFinite(ms)) return t("尚未同步");
  const value = new Date(ms);
  if (Number.isNaN(value.getTime())) return t("尚未同步");
  return formatDateTime(value, { hour12: false });
}

// 官方配额的 statusLine 钩子：只提取额度窗口，不碰对话与凭据。Antigravity
// CLI（agy）内嵌 language server 的 csrf 只在进程内部，外部无法直连，官方口子
// 同样是 statusLine 钩子（会话 JSON 经 stdin 推给命令）。
function HookCard({
  title,
  description,
  getStatus,
  setHook,
  installedMessage,
  waitingLabel,
  onSnapshotRefresh,
  children,
}) {
  const { status, busy, feedback, toggle } = useSourceToggle({
    load: getStatus,
    save: setHook,
    loadError: "钩子状态读取失败。",
    enabledText: installedMessage,
    disabledText: "钩子已卸载，statusLine 设置已恢复。",
    onSnapshotRefresh,
  });

  return (
    <div className="settings-card">
      <h2>{title}</h2>
      <p className="settings-muted">{description}</p>
      {status?.demo ? (
        <p className="settings-muted">{t("浏览器演示模式：仅桌面应用可配置。")}</p>
      ) : status && (
        <>
          <div className="settings-directory-row">
            <button
              type="button"
              className={`ledger-button ${status.installed ? "ledger-button--secondary" : "ledger-button--primary"}`}
              disabled={busy || (!status.installed && status.conflict)}
              onClick={() => toggle(!status.installed)}
            >
              {status.installed ? t("卸载钩子") : status.replaced ? t("重新串联") : t("安装钩子")}
            </button>
          </div>
          <dl className="settings-status">
            <div>
              <dt>{t("状态")}</dt>
              <dd>
                {status.installed
                  ? [
                      t("已安装"),
                      ...(status.chained ? [t("已串联原有状态栏")] : []),
                      status.lastDataAtMs
                        ? t(status.stale ? "数据已过期 {time}" : "最近数据 {time}", { time: formatSyncTime(status.lastDataAtMs) })
                        : waitingLabel,
                    ].join(" · ")
                  : status.conflict
                    ? t("未安装 · 现有 statusLine 缺少 command 字段，无法串联")
                    : status.replaced
                      ? t("已被其他 statusLine 替换 · 可重新串联当前命令")
                    : t("未安装")}
              </dd>
            </div>
          </dl>
        </>
      )}
      <FeedbackLine feedback={feedback} />
      {children}
    </div>
  );
}

// OAuth 官方额度：读取 Claude Code 自己保存的登录凭据（显式 opt-in），
// 直接查询账户级合并额度（含网页版消耗），不依赖终端状态栏。
function ClaudeOauthBlock({ onSnapshotRefresh }) {
  const { status, busy, feedback, toggle } = useSourceToggle({
    load: getClaudeOauthStatus,
    save: setClaudeOauth,
    loadError: "官方配额来源状态读取失败。",
    enabledText: "已开启。下次刷新起直接查询官方配额（约每 5 分钟一次）；查询失败时自动回落到状态栏钩子。",
    disabledText: "已关闭。恢复为仅由状态栏钩子提供额度。",
    onSnapshotRefresh,
  });

  if (status?.demo) return null;

  return (
    <div className="settings-subsection">
      <h3>{t("官方配额直连（OAuth）")}</h3>
      <p className="settings-muted">
        {t("备选来源：用本机 Claude Code 已保存的凭据直接查询官方配额（账户级合并值，约每 5 分钟一次）， 网页版与桌面客户端的消耗同样计入。凭据只在内存中读取，不存储、不上传。 前提是最近使用过 Claude Code：凭据有效期仅数小时，且仅在 Claude Code 运行时刷新； 服务端拒绝后回落到状态栏钩子。")}
      </p>
      <p className="settings-muted">
        {t("⚠️ 条款风险须知：Anthropic 2026 年 2 月更新的消费者条款禁止在第三方工具中使用 Claude 订阅的 OAuth 凭据。目前公开的封禁与拦截集中在借订阅做推理的第三方工具，未见只读用量查询被封号的案例， 但按条款字面本功能同样属于违规范围。若不愿承担此风险，可保持关闭，使用零凭据的状态栏钩子。")}
      </p>
      {status && (
        <>
          <div className="settings-directory-row">
            <button
              type="button"
              className={`ledger-button ${status.enabled ? "ledger-button--secondary" : "ledger-button--primary"}`}
              disabled={busy || (!status.enabled && !status.credentialsPresent)}
              onClick={() => toggle(!status.enabled)}
            >
              {status.enabled ? t("关闭直连") : t("开启直连")}
            </button>
          </div>
          <dl className="settings-status">
            <div>
              <dt>{t("状态")}</dt>
              <dd>
                {!status.credentialsPresent
                  ? t("本机未找到 Claude Code 登录凭据（先在终端运行 claude login）")
                  : !status.scopeOk
                    ? t("凭据缺少 user:profile 权限，开启后可能查询失败（可运行 claude login 重新登录）")
                    : status.expired
                      ? (status.enabled
                        ? t("已开启 · 本地有效期记录已过，开启后仍由服务端确认一次")
                        : t("未开启 · 本地有效期记录已过，开启后仍由服务端确认一次"))
                      : status.enabled
                        ? t("已开启 · 凭据可用")
                        : t("未开启 · 凭据可用")}
              </dd>
            </div>
            {/* 开关一切正常、却始终没有额度数字时，唯一能解释原因的就是这一行。 */}
            {status.lastFailure && (
              <div>
                <dt>{t("最近失败")}</dt>
                <dd>
                  {status.lastFailure.message} · {formatSyncTime(status.lastFailure.atMs)}
                </dd>
              </div>
            )}
          </dl>
        </>
      )}
      <FeedbackLine feedback={feedback} />
    </div>
  );
}

function StartupCard({ autoUpdateCheck, onAutoUpdateCheck, availableUpdate }) {
  const [enabled, setEnabled] = useState(null);
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState(null);

  useEffect(() => {
    let cancelled = false;
    getAutostart()
      .then((value) => {
        if (!cancelled) setEnabled(value);
      })
      .catch(() => {
        if (!cancelled) setFeedback({ tone: "error", text: "开机启动状态读取失败。" });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const toggle = async (next) => {
    setBusy(true);
    setFeedback(null);
    try {
      setEnabled(await setAutostart(next));
    } catch (error) {
      setFeedback({ tone: "error", message: `${error}` });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="settings-card">
      <h2>{t("启动与位置")}</h2>
      <p className="settings-muted">
        {t("位置会被记住，下次启动时恢复；超出屏幕范围时自动居中。")}
      </p>
      {enabled === null ? (
        <p className="settings-muted">{t("浏览器演示模式：仅桌面应用可配置。")}</p>
      ) : (
        <div className="settings-directory-row">
          <button
            type="button"
            className={`ledger-button ${enabled ? "ledger-button--secondary" : "ledger-button--primary"}`}
            disabled={busy}
            onClick={() => toggle(!enabled)}
          >
            {enabled ? t("关闭开机启动") : t("开机时自动启动")}
          </button>
        </div>
      )}
      {feedback && (
        <p className={`settings-feedback settings-feedback--${feedback.tone}`} role="alert">
          {feedbackText(feedback)}
        </p>
      )}
      <UpdateBlock
        autoCheck={autoUpdateCheck}
        onAutoCheckChange={onAutoUpdateCheck}
        availableUpdate={availableUpdate}
      />
    </div>
  );
}

// 检查每天自动做一次（可关，关掉后回到纯手动）；下载安装永远由用户点击触发。
function UpdateBlock({ autoCheck, onAutoCheckChange, availableUpdate }) {
  const [state, setState] = useState(() =>
    availableUpdate ? { status: "available", ...availableUpdate } : { status: "idle" },
  );
  // 自动检查在小组件形态就可能发现新版；进设置页时直接呈现，不用再点一次。
  // 后来的自动检查发现更新的版本时要顶掉手上这一份：否则一直提示第一次发现
  // 的那个版本，中间发布的都要等装完它才看得见。
  useEffect(() => {
    if (!availableUpdate) return;
    setState((current) => {
      // 正在检查或下载时不换手上这一份，免得按钮和进度对不上。
      if (current.status === "checking" || current.status === "installing") return current;
      if (current.status === "available" && current.version === availableUpdate.version) {
        return current;
      }
      return { status: "available", ...availableUpdate };
    });
  }, [availableUpdate]);

  const check = async () => {
    setState({ status: "checking" });
    try {
      const found = await checkForUpdate();
      setState(found ? { status: "available", ...found } : { status: "current" });
    } catch (error) {
      setState({ status: "error", message: `${error}` });
    }
  };

  const install = async () => {
    setState((current) => ({ ...current, status: "installing", percent: null }));
    try {
      await installUpdate(state.update, (percent) =>
        setState((current) => ({ ...current, percent })));
    } catch (error) {
      setState({ status: "error", message: `${error}` });
    }
  };

  if (!isDesktop()) return null;

  return (
    <div className="settings-subsection">
      <h3>{t("更新")}</h3>
      <p className="settings-muted">
        {t("当前版本 {version}。自动检查开启后，启动时检查一次，持续运行时每天检查一次； 下载与安装需手动确认，更新包经签名校验。", { version: __APP_VERSION__ })}
      </p>
      <label className="update-autocheck">
        <input
          type="checkbox"
          checked={autoCheck}
          onChange={(event) => onAutoCheckChange(event.target.checked)}
        />
        <span>{t("自动检查更新（每天一次）")}</span>
      </label>
      <div className="settings-directory-row">
        <button
          type="button"
          className="ledger-button"
          disabled={state.status === "checking" || state.status === "installing"}
          onClick={state.status === "available" ? install : check}
        >
          {state.status === "checking"
            ? t("检查中…")
            : state.status === "installing"
              ? (state.percent == null ? t("下载中…") : t("下载中 {percent}%…", { percent: state.percent }))
              : state.status === "available"
                ? t("更新到 {version}", { version: state.version })
                : t("检查更新")}
        </button>
        {/* 有新版待装时主按钮变成"更新到 X"，没有这一个就再也没法重新检查：
            关掉自动检查的人只能先把旧版装掉，才看得到后面发布的版本。 */}
        {state.status === "available" && (
          <button type="button" className="ledger-button ledger-button--secondary" onClick={check}>
            {t("重新检查")}
          </button>
        )}
      </div>
      {state.status === "current" && (
        <p className="settings-feedback settings-feedback--success" role="status">
          {t("已是最新版本。")}
        </p>
      )}
      {state.status === "available" && state.notes && (
        <p className="settings-muted">{state.notes}</p>
      )}
      {state.status === "error" && (
        <p className="settings-feedback settings-feedback--error" role="alert">
          {state.message}
        </p>
      )}
    </div>
  );
}

const REPO_URL = "https://github.com/keros68/metrik";
const AUTHOR_EMAIL = "keros68@gmail.com";

// 桌面端交给系统浏览器/邮件客户端（opener 插件，capability 限定了这两个地址）；
// 浏览器演示模式退化为普通链接。
function AboutCard() {
  const openExternal = async (event, url) => {
    if (!isDesktop()) return;
    event.preventDefault();
    const { openUrl } = await import("@tauri-apps/plugin-opener");
    await openUrl(url).catch((error) => console.warn("open external link failed:", error));
  };
  return (
    <div className="settings-card settings-about">
      <h2>{t("关于")}</h2>
      <p className="settings-muted">Metrik {__APP_VERSION__}</p>
      <p className="settings-muted">
        {(() => {
          const [before, after] = t("作者：keros68（{email}）").split("{email}");
          return (
            <>
              {before}
              <a href={`mailto:${AUTHOR_EMAIL}`} onClick={(event) => openExternal(event, `mailto:${AUTHOR_EMAIL}`)}>
                {AUTHOR_EMAIL}
              </a>
              {after}
            </>
          );
        })()}
      </p>
      <p className="settings-muted">
        {t("项目仓库：")}
        <a href={REPO_URL} onClick={(event) => openExternal(event, REPO_URL)}>
          github.com/keros68/metrik
        </a>
      </p>
      <p className="settings-muted">{t("许可证：AGPL-3.0-or-later")}</p>
    </div>
  );
}

function SliderRow({ label, hint, min, max, step, percent, ariaLabel, onChange }) {
  return (
    <div className="settings-subsection">
      <h3>{label}</h3>
      <p className="settings-muted">{hint}</p>
      <div className="glass-slider-row">
        <input
          type="range"
          min={min}
          max={max}
          step={step}
          value={percent}
          aria-label={ariaLabel}
          onChange={(event) => onChange(Number(event.target.value) / 100)}
        />
        <em>{percent}%</em>
      </div>
    </div>
  );
}

function AppearanceCard({ theme, onThemeChange, glassAlpha, onGlassAlpha, glassTint, onGlassTint, glassInk, onGlassInk, uiScale, onUiScale, stripScale, onStripScale, pinned, onPinnedChange, pinnedHoverMode, onPinnedHoverMode, pinnedHoverOpacity, onPinnedHoverOpacity }) {
  return (
    <div className="settings-card">
      <h2>{t("外观与缩放")}</h2>
      <p className="settings-muted">
        {t("大窗口的明暗主题，「自动」跟随系统；小组件不受影响。")}
      </p>
      <div className="theme-toggle" role="group" aria-label={t("完整视图主题")}>
        {THEME_OPTIONS.map((option) => (
          <button
            key={option.id}
            type="button"
            className={theme === option.id ? "is-selected" : ""}
            aria-pressed={theme === option.id}
            onClick={() => onThemeChange(option.id)}
          >
            {t(option.label)}
          </button>
        ))}
      </div>
      {/* macOS 面板材质跟随系统 vibrancy，不提供组件外观选项；
          深/浅配色用于 Windows 与 Linux 的卡片和胶囊。 */}
      {!IS_MAC && (
        <div className="settings-subsection">
          <h3>{t("组件外观")}</h3>
          <p className="settings-muted">
            {t("深色是 HUD 玻璃；浅色是透亮白磨砂；透明会直接透出桌面与后方窗口，并叠加轻霜和边缘高光。")}
          </p>
          <div className="theme-toggle" role="group" aria-label={t("组件外观")}>
            {GLASS_TINT_OPTIONS.map((option) => (
              <button
                key={option.id}
                type="button"
                className={glassTint === option.id ? "is-selected" : ""}
                aria-pressed={glassTint === option.id}
                onClick={() => onGlassTint(option.id)}
              >
                {t(option.label)}
              </button>
            ))}
          </div>
        </div>
      )}
      {!IS_MAC && glassTint === "clear" && (
        <div className="settings-subsection">
          <h3>{t("透明档文字")}</h3>
          <p className="settings-muted">
            {t("深色字配白霜，在浅色壁纸上更清晰；白色字配一层薄暗罩，接近桌面挂件常见的风格。")}
          </p>
          <div className="theme-toggle" role="group" aria-label={t("透明档文字")}>
            {GLASS_INK_OPTIONS.map((option) => (
              <button
                key={option.id}
                type="button"
                className={glassInk === option.id ? "is-selected" : ""}
                aria-pressed={glassInk === option.id}
                onClick={() => onGlassInk(option.id)}
              >
                {t(option.label)}
              </button>
            ))}
          </div>
        </div>
      )}
      <SliderRow
        label={t("玻璃浓度")}
        hint={t("同时作用于卡片和胶囊；越低越通透，越高越厚实。")}
        min={5}
        max={96}
        step={2}
        percent={Math.round(glassAlpha * 100)}
        ariaLabel={t("玻璃浓度百分比")}
        onChange={onGlassAlpha}
      />
      {/* mac 的菜单栏面板是系统 UI 的一部分，尺寸固定不提供缩放；
          缩放只针对 Windows 的桌面小插件与胶囊条。 */}
      {!IS_MAC && (
        <SliderRow
          label={t("卡片缩放")}
          hint={t("仅调整卡片尺寸；滑杆改动下次进入时生效。")}
          min={UI_SCALE_RANGE.min * 100}
          max={UI_SCALE_RANGE.max * 100}
          step={5}
          percent={Math.round(uiScale * 100)}
          ariaLabel={t("卡片缩放百分比")}
          onChange={onUiScale}
        />
      )}
      {!IS_MAC && (
        <SliderRow
          label={t("胶囊缩放")}
          hint={t("仅调整胶囊尺寸，与卡片互不影响；下次进入时生效。")}
          min={UI_SCALE_RANGE.min * 100}
          max={UI_SCALE_RANGE.max * 100}
          step={5}
          percent={Math.round(stripScale * 100)}
          ariaLabel={t("胶囊缩放百分比")}
          onChange={onStripScale}
        />
      )}
      {IS_LINUX && (
        <div className="settings-subsection">
          <h3>{t("置顶展示")}</h3>
          <p className="settings-muted">
            {t("置顶后卡片和胶囊的全部控件、拖动与点击均不可用；只能回到此设置取消。")}
          </p>
          <div className="theme-toggle" role="group" aria-label={t("置顶展示模式")}>
            <button
              type="button"
              className={!pinned ? "is-selected" : ""}
              aria-pressed={!pinned}
              onClick={() => onPinnedChange(false)}
            >
              {t("正常交互")}
            </button>
            <button
              type="button"
              className={pinned ? "is-selected" : ""}
              aria-pressed={pinned}
              onClick={() => onPinnedChange(true)}
            >
              {t("置顶只读")}
            </button>
          </div>
        </div>
      )}
      {IS_LINUX && (
        <div className="settings-subsection">
          <h3>{t("置顶悬停行为")}</h3>
          <p className="settings-muted">
            {t("鼠标进入置顶卡片或胶囊时生效，移开后恢复。")}
          </p>
          <div className="theme-toggle" role="group" aria-label={t("置顶悬停行为")}>
            {PINNED_HOVER_OPTIONS.map((option) => (
              <button
                key={option.id}
                type="button"
                className={pinnedHoverMode === option.id ? "is-selected" : ""}
                aria-pressed={pinnedHoverMode === option.id}
                onClick={() => onPinnedHoverMode(option.id)}
              >
                {t(option.label)}
              </button>
            ))}
          </div>
        </div>
      )}
      {IS_LINUX && pinnedHoverMode === "fade" && (
        <SliderRow
          label={t("悬停不透明度")}
          hint={t("数值越低，鼠标经过时越接近完全透明。")}
          min={5}
          max={90}
          step={5}
          percent={Math.round(pinnedHoverOpacity * 100)}
          ariaLabel={t("置顶悬停不透明度百分比")}
          onChange={onPinnedHoverOpacity}
        />
      )}
    </div>
  );
}

function NativeMacWidgetCard() {
  if (!IS_MAC) return null;
  return (
    <div className="settings-card desktop-widget-setting">
      <div>
        <span className="desktop-widget-setting-kicker">macOS</span>
        <h2>{t("桌面小组件")}</h2>
        <p className="settings-muted">
          {t("在桌面空白处右键「编辑小组件」，搜索 Metrik 后添加。透明材质、圆角和摆放由 macOS 管理。")}
        </p>
      </div>
      <span className="desktop-widget-native-badge">{t("系统原生")}</span>
    </div>
  );
}

function AgentListColumn({ title, hint, agents, detected, onToggle, onMove }) {
  // 已选的按显示顺序排前面，未选的按默认顺序垫后。
  const rows = agentIdsInDisplayOrder(agents);
  // 勾选过的一律留在上面：检测只用于分组，不能把用户自己选的挪进折叠区。
  // detected 为 null 表示这次拿不到检测结果（加载中、演示数据、旧后端），
  // 此时不折叠——宁可多列几行，也不能因为不知道就把 Agent 藏起来。
  const present = detected
    ? rows.filter((agentId) => agents.includes(agentId) || detected.has(agentId))
    : rows;
  const missing = detected ? rows.filter((agentId) => !present.includes(agentId)) : [];
  const row = (agentId) => {
    const index = agents.indexOf(agentId);
    const checked = index >= 0;
    return (
      <li key={agentId}>
        <label>
          <input
            type="checkbox"
            checked={checked}
            disabled={checked && agents.length === 1}
            onChange={() => onToggle(agentId)}
          />
          <AgentMark agentId={agentId} />
          <span>{AGENT_META[agentId].label}</span>
        </label>
        {checked && (
          <button
            type="button"
            className="settings-agent-move"
            onClick={() => onMove(agentId)}
            disabled={index === 0}
            aria-label={t("将 {agent} 上移", { agent: AGENT_META[agentId].label })}
            title={t("上移")}
          >
            ↑
          </button>
        )}
      </li>
    );
  };
  return (
    <div className="settings-agent-column">
      <h3>{title}</h3>
      <p className="settings-muted">{hint}</p>
      <ul className="settings-agent-toggle">{present.map(row)}</ul>
      {missing.length > 0 && (
        <details className="settings-agent-missing">
          <summary>{t("本机未检测到（{count}）", { count: missing.length })}</summary>
          <ul className="settings-agent-toggle">{missing.map(row)}</ul>
        </details>
      )}
    </div>
  );
}

function AgentsDisplayCard({ widgetAgents, onToggleWidgetAgent, onMoveWidgetAgent, stripAgents, onToggleStripAgent, onMoveStripAgent, detectedAgents, trayBadgeEnabled, onToggleTrayBadge }) {
  return (
    <div className="settings-card">
      <h2>{t("显示的 Agent")}</h2>
      <p className="settings-muted">
        {t("勾选即展示（至少保留一个），勾选顺序即显示顺序，↑ 上移。")}
      </p>
      <div className="settings-agent-columns">
        <AgentListColumn
          title={IS_MAC ? t("菜单栏、小组件与侧栏") : t("小组件与侧栏")}
          hint={t("完整视图侧栏还会自动加入本周期内产生用量的 Agent。")}
          agents={widgetAgents}
          detected={detectedAgents}
          onToggle={onToggleWidgetAgent}
          onMove={onMoveWidgetAgent}
        />
        {!IS_MAC && (
          <AgentListColumn
            title={t("胶囊条")}
            hint={t('无配额来源的以 "--" 占位。')}
            agents={stripAgents}
            detected={detectedAgents}
            onToggle={onToggleStripAgent}
            onMove={onMoveStripAgent}
          />
        )}
      </div>
      {/* Windows 专属：托盘图标换成数字。取的正是上面列表最上方的 Agent，
          与菜单栏状态项同一套数据，只是托盘只放得下一个数字。 */}
      {IS_WINDOWS && (
        <div className="settings-subsection">
          <h3>{t("任务栏图标")}</h3>
          <p className="settings-muted">
            {t("开启后，任务栏右下角的 Metrik 图标改为显示列表最上方 Agent 的剩余百分比； 无可靠额度时显示 --，窗口隐藏后数字仍每 5 分钟更新一次。")}
          </p>
          <label className="settings-check">
            <input
              type="checkbox"
              checked={trayBadgeEnabled}
              onChange={(event) => onToggleTrayBadge(event.target.checked)}
            />
            <span>{t("图标改为显示余量数字")}</span>
          </label>
        </div>
      )}
    </div>
  );
}

// Cursor 不把逐次 token 写进本机，用量和套餐余量只能从 cursor.com 仪表盘拉取，
// 要用到 Cursor 本机保存的登录会话，所以默认关闭，由用户在这里开启。
// 账号级、默认关闭的数据来源：开启后用对应客户端本机保存的登录凭据读取。
// Cursor 不在本机记录逐次 token；Grok Bot 的账号级额度池与 SuperGrok、新版
// Cursor 共用，本地没有逐轮 usage 可读。
const ACCOUNT_SOURCES = {
  cursor: {
    title: "Cursor 用量与套餐余量",
    description: "Cursor 不在本机记录逐次 token，开启后用 Cursor 已保存的登录会话向 cursor.com 读取仪表盘上的逐次用量，以及当前账单周期的套餐余量。 会话每次现读、只在内存中使用，不存储、不同步。已过去的日子只读一次，今天和昨天每 15 分钟重读；套餐余量最多每 5 分钟读一次。 用量是账号级的，包含这个账号在所有设备上的消耗，因此不参与多设备同步。",
    load: getCursorUsageStatus,
    save: setCursorUsage,
    loadError: "Cursor 用量来源状态读取失败。",
    enabledText: "已开启。套餐余量在下次刷新时显示，近 65 天的用量会在接下来几次刷新里补齐。",
    disabledText: "已关闭。不再请求 cursor.com，已拉取的 Cursor 用量与套餐余量已从本机账本删除。",
    sessionText: (status) => !status.signedIn
      ? t("Cursor 未登录")
      : status.expired
        ? t("登录会话已过期，打开 Cursor 即可刷新")
        : t("登录会话可用"),
  },
  grokbot: {
    title: "Grok Bot 共享额度",
    description: "Grok Bot 与 SuperGrok、新版 Cursor 共用同一个账号级额度池，本地没有逐轮 token 记录，只有这个池的剩余比例与每周重置时间。开启后用 Grok Bot 本机保存的登录凭据（仅 Windows）向官方接口查询；凭据每次现解密、只在内存中使用，不存储、不刷新、不同步。额度池是账号级的，包含所有客户端的消耗。",
    load: getGrokbotQuotaStatus,
    save: setGrokbotQuota,
    loadError: "Grok Bot 额度来源状态读取失败。",
    enabledText: "已开启。共享额度池的剩余与重置时间在下次刷新时显示。",
    disabledText: "已关闭。不再读取 Grok Bot 的凭据，已拉取的额度已从本机账本删除。",
    sessionText: (status) => status.signedIn
      ? t("Grok Bot 已登录")
      : t("Grok Bot 未登录，先在 Grok Bot 里登录"),
  },
};

function AccountSourceCard({ source, onSnapshotRefresh }) {
  const config = ACCOUNT_SOURCES[source];
  const { status, busy, feedback, toggle } = useSourceToggle({ ...config, onSnapshotRefresh });

  // 没装对应客户端时不占设置页的位置；已开启的仍然显示，便于关闭。
  if (status && !status.demo && !status.installed && !status.enabled) return null;

  return (
    <div className="settings-card">
      <h2>{t(config.title)}</h2>
      <p className="settings-muted">{t(config.description)}</p>
      {status?.demo && <p className="settings-muted">{t("浏览器演示模式：仅桌面应用可配置。")}</p>}
      {status && !status.demo && (
        <>
          <div className="settings-directory-row">
            <button
              type="button"
              className={`ledger-button ${status.enabled ? "ledger-button--secondary" : "ledger-button--primary"}`}
              disabled={busy}
              onClick={() => toggle(!status.enabled)}
            >
              {status.enabled ? t("关闭", { context: "toggle" }) : t("开启")}
            </button>
          </div>
          <dl className="settings-status">
            <div>
              <dt>{t("状态")}</dt>
              <dd>
                {[status.enabled ? t("已开启") : t("未开启"), config.sessionText(status)].join(" · ")}
              </dd>
            </div>
          </dl>
        </>
      )}
      <FeedbackLine feedback={feedback} />
    </div>
  );
}

const RESPONSE_SPEED_SOURCES = [
  { id: "claude", label: "Claude Code" },
  { id: "codex", label: "Codex" },
];

function ResponseSpeedCollectionCard({ onSnapshotRefresh }) {
  const [status, setStatus] = useState(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(null);
  const [readFailed, setReadFailed] = useState(false);
  const [feedback, setFeedback] = useState(null);
  const mounted = useRef(false);
  const inFlight = useRef(false);
  const previousStatus = useRef(null);
  const refreshSnapshot = useRef(onSnapshotRefresh);
  refreshSnapshot.current = onSnapshotRefresh;

  const acceptStatus = useCallback((next, forceRefresh = false) => {
    const changed = previousStatus.current !== null
      && JSON.stringify(previousStatus.current) !== JSON.stringify(next);
    previousStatus.current = next;
    setStatus(next);
    setReadFailed(false);
    if (changed || forceRefresh) refreshSnapshot.current();
  }, []);

  const readStatus = useCallback(async () => {
    if (inFlight.current) return;
    inFlight.current = true;
    setLoading(true);
    setFeedback(null);
    try {
      const next = await getResponseSpeedStatus();
      if (mounted.current) acceptStatus(next);
    } catch {
      if (mounted.current) {
        setReadFailed(true);
        setFeedback({ tone: "error", text: "响应速度采集状态读取失败，请刷新状态后重试。" });
      }
    } finally {
      inFlight.current = false;
      if (mounted.current) setLoading(false);
    }
  }, [acceptStatus]);

  useEffect(() => {
    mounted.current = true;
    readStatus();
    const refreshVisible = () => {
      if (document.visibilityState !== "hidden") readStatus();
    };
    window.addEventListener("focus", refreshVisible);
    document.addEventListener("visibilitychange", refreshVisible);
    return () => {
      mounted.current = false;
      window.removeEventListener("focus", refreshVisible);
      document.removeEventListener("visibilitychange", refreshVisible);
    };
  }, [readStatus]);

  const toggle = async (agentId, enabled) => {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(agentId);
    setFeedback(null);
    try {
      const next = await setResponseSpeedCollection(agentId, enabled);
      if (mounted.current) {
        acceptStatus(next, true);
        setFeedback({ tone: "success", text: enabled ? "响应速度采集设置已更新。" : "已停止采集，保留已有统计。" });
      } else {
        refreshSnapshot.current();
      }
    } catch {
      // A config can change between reading and toggling. Re-read so a conflict
      // is visible immediately without retrying the write or replacing it.
      try {
        const next = await getResponseSpeedStatus();
        if (mounted.current) acceptStatus(next);
      } catch {
        if (mounted.current) setReadFailed(true);
      }
      if (mounted.current) {
        setFeedback({ tone: "error", text: "响应速度采集设置未能更新，请检查状态后重试。" });
      }
    } finally {
      inFlight.current = false;
      if (mounted.current) setBusy(null);
    }
  };

  return (
    <div className="settings-card response-speed-card" aria-busy={loading || busy !== null}>
      <h2>{t("Claude / Codex 响应速度")}</h2>
      <p className="settings-muted">
        {t("默认关闭。分别开启后，重启对应客户端或会话，才开始采集新请求；不会补算历史速度。")}
      </p>
      <p className="settings-muted response-speed-privacy">
        {t("只在本机保存模型名、输出 token 数与计时数字，不保存对话或凭据。统计的是含等待与思考的响应均速，不是纯生成或实时速度。")}
      </p>
      {status?.demo && <p className="settings-muted response-speed-privacy">{t("浏览器演示模式：仅桌面应用可配置。")}</p>}
      <div className="response-speed-sources" aria-live="polite">
        {RESPONSE_SPEED_SOURCES.map(({ id, label }) => {
          const source = status?.sources.find((entry) => entry.id === id);
          const hasSamples = Number.isInteger(source?.sampleCount) && source.sampleCount > 0;
          return (
            <section className="response-speed-source" key={id} aria-labelledby={`response-speed-${id}`}>
              <div className="response-speed-source-heading">
                <h3 id={`response-speed-${id}`}>{label}</h3>
                <label className="response-speed-switch">
                  <input
                    type="checkbox"
                    role="switch"
                    checked={source?.enabled === true}
                    disabled={!source || status.demo || loading || busy !== null || readFailed || (!source.enabled && source.conflict)}
                    aria-label={t("采集 {agent} 响应速度", { agent: label })}
                    aria-describedby={`response-speed-state-${id}`}
                    onChange={(event) => toggle(id, event.target.checked)}
                  />
                  <span>{source ? source.enabled ? t("已开启") : t("未开启") : loading ? t("读取中…") : t("未提供")}</span>
                </label>
              </div>
              <p className="settings-muted" id={`response-speed-state-${id}`}>
                {busy === id ? t("正在更新…")
                  : loading ? t("读取中…")
                    : !source || readFailed ? t("暂时无法确认当前采集状态")
                      : source.conflict ? source.enabled
                        ? t("配置已变动 · 保留现有外部遥测配置，采集已暂停。")
                        : t("已有外部遥测配置 · 保留原配置，无法开启采集。")
                        : source.enabled && !source.configured ? t("配置已变动 · 采集已暂停，请检查客户端配置。")
                          : !source.enabled ? t("未开启 · 不采集新请求")
                            : !status.running ? t("已开启 · 本机采集服务未运行，请关闭后重新开启。")
                              : hasSamples ? t("已收到样本 · 只统计有完整计时的已完成请求")
                                : t("已开启 · 等待新请求；请先重启对应客户端或会话。")}
              </p>
              {hasSamples && (
                <p className="response-speed-sample-count">
                  {t("本机已收集 {count} 个有效样本", { count: formatNumber(source.sampleCount) })}
                  {Number.isFinite(source.lastSampleAtMs) && (
                    <span>{t("最近样本 {time}", { time: formatDateTime(source.lastSampleAtMs, { hour12: false }) })}</span>
                  )}
                </p>
              )}
              {source?.error && <p className="settings-feedback settings-feedback--error" role="alert">{t(source.error)}</p>}
            </section>
          );
        })}
      </div>
      <button type="button" className="ledger-button response-speed-refresh" disabled={loading || busy !== null} onClick={readStatus}>
        {loading ? t("读取中…") : t("刷新采集状态")}
      </button>
      <FeedbackLine feedback={feedback} />
    </div>
  );
}

function QoderQuotaCard({ onSnapshotRefresh }) {
  const [status, setStatus] = useState(null);
  const [cookieInput, setCookieInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState(null);

  useEffect(() => {
    let cancelled = false;
    getQoderCookieStatus()
      .then((value) => {
        if (!cancelled) setStatus(value);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  const apply = async (cookie) => {
    setBusy(true);
    setFeedback(null);
    try {
      const next = await configureQoderCookie(cookie);
      setStatus(next);
      setCookieInput("");
      // 验证失败也已保存：如实转述后端的结果，不粉饰。messageIsError 由后端
      // 给出，前端不解析文案判断成败。
      setFeedback({
        tone: next.messageIsError ? "error" : "success",
        ...(next.message ? { message: next.message } : { text: "已更新。" }),
      });
      if (cookie && !next.messageIsError) onSnapshotRefresh();
    } catch (error) {
      setFeedback({ tone: "error", text: "操作失败：{error}", params: { error: `${error}` } });
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="settings-card">
      <h2>{t("Qoder 官方配额")}</h2>
      <p className="settings-muted">
        {t("Qoder、QoderWork 与 Qoder CLI 共用账户级 Credits；本地客户端不提供可被可靠解析的 token 用量，只能读取官网 Credits 额度，需要提供一次 登录 Cookie。Cookie 仅明文保存在本机（不入账本、不进同步导出），可清除。")}
      </p>
      <details className="settings-guide">
        <summary>{t("如何获取 Cookie")}</summary>
        <ol>
          <li>{t("浏览器登录 qoder.com.cn（国际版 qoder.com），进入「用量明细」页；")}</li>
          <li>{t("按 F12 打开开发者工具 → 网络（Network）标签，点击过滤器中的 Fetch/XHR；")}</li>
          <li>{t("右键列表中任意 qoder 域名的请求（如 big_model_credits）→ 复制 → 「复制请求标头」或「以 cURL 格式复制」，将整段粘贴到下方，系统会自动提取其中的 Cookie。")}</li>
        </ol>
      </details>
      {status?.demo ? (
        <p className="settings-muted">{t("浏览器演示模式：仅桌面应用可配置。")}</p>
      ) : (
        <>
          <div className="settings-directory-row">
            <input
              type="password"
              value={cookieInput}
              placeholder={t("粘贴 Cookie 值 / 整段请求标头 / cURL 命令")}
              spellCheck={false}
              disabled={busy}
              aria-label="Qoder Cookie"
              onChange={(event) => setCookieInput(event.target.value)}
            />
            <button
              type="button"
              className="ledger-button ledger-button--primary"
              disabled={busy || !cookieInput.trim()}
              onClick={() => apply(cookieInput.trim())}
            >
              {t("保存并验证")}
            </button>
          </div>
          {status?.source === "file" && (
            <button
              type="button"
              className="ledger-button ledger-button--secondary"
              disabled={busy}
              onClick={() => apply(null)}
            >
              {t("清除已保存的 Cookie")}
            </button>
          )}
          <FeedbackLine feedback={feedback} />
          <dl className="settings-status">
            <div>
              <dt>{t("状态")}</dt>
              <dd>
                {status == null
                  ? t("读取中…")
                  : status.configured
                    ? status.source === "env"
                      ? t("已配置（环境变量）")
                      : t("已配置（本机保存）")
                    : t("未配置 · 配额卡将显示不可用")}
              </dd>
            </div>
          </dl>
        </>
      )}
    </div>
  );
}

// 设置分三类子页：每页 2 张卡，高度相近。Agent 列表会随支持的 Agent 变多而
// 变高，与短卡混排时会把整排撑出大片空白——分页正是为了让它只影响自己那一页。
const SETTINGS_TABS = [
  {
    id: "appearance",
    label: "外观与缩放",
    title: "外观与缩放",
    blurb: "完整视图的明暗主题，卡片与胶囊的组件外观、玻璃浓度及独立缩放。",
  },
  {
    id: "display",
    label: "Agent 与提醒",
    title: "Agent 展示与额度提醒",
    blurb: "选择展示的 Agent、调整顺序及配置低额度提醒。",
  },
  {
    id: "sources",
    label: "数据来源",
    title: "数据来源与采集",
    blurb: "配置官方配额读取方式与可选的响应速度采集。官方配额、本地解析用量与估算成本始终分开呈现。",
  },
  {
    id: "sync",
    label: "同步与更新",
    title: "多设备同步与更新",
    blurb: "多台电脑指向同一个共享文件夹即可互相合并统计；导出只含事件标识、Agent、时间与 token 数，不含对话内容或凭据。",
  },
];

export function SettingsSection({ onSnapshotRefresh, widgetAgents, onToggleWidgetAgent, onMoveWidgetAgent, stripAgents, onToggleStripAgent, onMoveStripAgent, detectedAgents, trayBadgeEnabled, onToggleTrayBadge, glassAlpha, onGlassAlpha, glassTint, onGlassTint, glassInk, onGlassInk, uiScale, onUiScale, stripScale, onStripScale, pinned, onPinnedChange, pinnedHoverMode, onPinnedHoverMode, pinnedHoverOpacity, onPinnedHoverOpacity, theme, onThemeChange, autoUpdateCheck, onAutoUpdateCheck, availableUpdate }) {
  const [settings, setSettings] = useState(null);
  const [directoryInput, setDirectoryInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [feedback, setFeedback] = useState(null);
  const [removingDeviceId, setRemovingDeviceId] = useState(null);

  useEffect(() => {
    let cancelled = false;
    getSyncSettings()
      .then((value) => {
        if (cancelled) return;
        setSettings(value);
        setDirectoryInput(value.directory || "");
      })
      .catch(() => {
        if (!cancelled) setFeedback({ tone: "error", text: "同步设置读取失败，稍后重试。" });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const applySync = async (directory) => {
    setBusy(true);
    setFeedback(null);
    try {
      const next = await configureSync(directory);
      setSettings(next);
      setDirectoryInput(next.directory || "");
      setFeedback({
        tone: "success",
        text: directory ? "同步已开启，本机统计事件已导出。" : "同步已关闭，已清除合并的远端统计。",
      });
      onSnapshotRefresh();
    } catch (error) {
      setFeedback({ tone: "error", text: "未能更新同步设置：{error}", params: { error: `${error}` } });
    } finally {
      setBusy(false);
    }
  };

  const removeDevice = async (deviceId) => {
    setBusy(true);
    setFeedback(null);
    try {
      const next = await removeSyncDevice(deviceId);
      setSettings(next);
      setFeedback({ tone: "success", text: "设备已删除，已清除该设备的同步事件与导出文件。" });
      onSnapshotRefresh();
    } catch (error) {
      setFeedback({ tone: "error", text: "未能删除设备：{error}", params: { error: `${error}` } });
    } finally {
      setBusy(false);
      setRemovingDeviceId(null);
    }
  };

  const [tab, setTab] = useState("appearance");
  const activeTab = SETTINGS_TABS.find((item) => item.id === tab) || SETTINGS_TABS[0];

  return (
    <main className="settings-section" aria-labelledby="settings-title">
      <header className="settings-header">
        <h1 id="settings-title">{t("设置")}</h1>
        <p><strong>{t(activeTab.title)}</strong> · {t(activeTab.blurb)}</p>
      </header>

      <div className="settings-tabs" role="tablist" aria-label={t("设置分类")}>
        {SETTINGS_TABS.map((item) => (
          <button
            key={item.id}
            type="button"
            role="tab"
            aria-selected={item.id === activeTab.id}
            className={item.id === activeTab.id ? "is-selected" : ""}
            onClick={() => setTab(item.id)}
          >
            {t(item.label)}
          </button>
        ))}
      </div>

      {settings?.demo && activeTab.id === "sync" && (
        <p className="settings-demo-note">{t("浏览器演示模式：仅桌面应用可配置。")}</p>
      )}

      <div className="settings-grid">
        {activeTab.id === "appearance" && (
          <>
            <LanguageCard />
            <AppearanceCard
              theme={theme}
              onThemeChange={onThemeChange}
              glassAlpha={glassAlpha}
              onGlassAlpha={onGlassAlpha}
              glassTint={glassTint}
              onGlassTint={onGlassTint}
              glassInk={glassInk}
              onGlassInk={onGlassInk}
              uiScale={uiScale}
              onUiScale={onUiScale}
              stripScale={stripScale}
              onStripScale={onStripScale}
              pinned={pinned}
              onPinnedChange={onPinnedChange}
              pinnedHoverMode={pinnedHoverMode}
              onPinnedHoverMode={onPinnedHoverMode}
              pinnedHoverOpacity={pinnedHoverOpacity}
              onPinnedHoverOpacity={onPinnedHoverOpacity}
            />
            <NativeMacWidgetCard />
          </>
        )}

        {activeTab.id === "display" && (
          <>
            <AgentsDisplayCard
              widgetAgents={widgetAgents}
              onToggleWidgetAgent={onToggleWidgetAgent}
              onMoveWidgetAgent={onMoveWidgetAgent}
              stripAgents={stripAgents}
              onToggleStripAgent={onToggleStripAgent}
              onMoveStripAgent={onMoveStripAgent}
              detectedAgents={detectedAgents}
              trayBadgeEnabled={trayBadgeEnabled}
              onToggleTrayBadge={onToggleTrayBadge}
            />
            <QuotaAlertsCard onSnapshotRefresh={onSnapshotRefresh} />
          </>
        )}
        {activeTab.id === "sources" && (
          <>
            <ResponseSpeedCollectionCard onSnapshotRefresh={onSnapshotRefresh} />
            <HookCard
              title={t("Claude Code 官方配额")}
              description={t("安装一个只提取 5h/7d 剩余额度的状态栏（statusLine）钩子（不读对话内容、不碰登录凭据）。 已有自定义 statusLine 会自动串联、原样保留；卸载时恢复原状。")}
              getStatus={getClaudeHookStatus}
              setHook={setClaudeHook}
              // 进反馈状态，存中文原文，渲染时翻译。
              installedMessage={"钩子已安装。下次 Claude Code 刷新状态栏后，此处显示官方 5h/7d 剩余额度。"}
              waitingLabel={t("等待 Claude Code 下次刷新状态栏")}
              onSnapshotRefresh={onSnapshotRefresh}
            >
              <ClaudeOauthBlock onSnapshotRefresh={onSnapshotRefresh} />
            </HookCard>
            <HookCard
              title={t("Antigravity CLI 官方配额")}
              description={t("仅为 Antigravity CLI（agy）安装一个提取官方额度窗口的状态栏（statusLine）钩子 （不读对话内容、不碰登录凭据）。已有自定义 statusLine 会自动串联、原样保留； 卸载时恢复原状。IDE（language server）在跑时额度仍走实时 RPC，不依赖此钩子。")}
              getStatus={getAntigravityHookStatus}
              setHook={setAntigravityHook}
              installedMessage={"钩子已安装。下次 Antigravity CLI 刷新状态后，此处显示官方额度窗口。"}
              waitingLabel={t("等待 Antigravity CLI 下次刷新状态")}
              onSnapshotRefresh={onSnapshotRefresh}
            />
            <QoderQuotaCard onSnapshotRefresh={onSnapshotRefresh} />
            <AccountSourceCard source="cursor" onSnapshotRefresh={onSnapshotRefresh} />
            <AccountSourceCard source="grokbot" onSnapshotRefresh={onSnapshotRefresh} />
            <CodexCreditsCard />
          </>
        )}

        {activeTab.id === "sync" && (
          <>
            <div className="settings-card">
              <label htmlFor="sync-directory">{t("同步文件夹（绝对路径）")}</label>
              <div className="settings-directory-row">
                <input
                  id="sync-directory"
                  type="text"
                  value={directoryInput}
                  placeholder={t("例如 {path}", { path: "D:\\Nutstore\\metrik-sync" })}
                  spellCheck={false}
                  disabled={busy || settings?.demo}
                  onChange={(event) => setDirectoryInput(event.target.value)}
                />
                <button
                  type="button"
                  className="ledger-button ledger-button--primary"
                  disabled={busy || settings?.demo || !directoryInput.trim()}
                  onClick={() => applySync(directoryInput.trim())}
                >
                  {settings?.enabled ? t("更新目录") : t("开启同步")}
                </button>
                {settings?.enabled && (
                  <button
                    type="button"
                    className="ledger-button ledger-button--secondary"
                    disabled={busy || settings?.demo}
                    onClick={() => applySync(null)}
                  >
                    {t("关闭同步")}
                  </button>
                )}
              </div>

              <FeedbackLine feedback={feedback} />

              {/* 读取未回来时这张卡片是空的，数据一到就整块长出来，看着像界面
                  抖了一下。先占住位置并说明在读什么。 */}
              {!settings && !feedback && (
                <p className="settings-muted" role="status">{t("读取同步设置…")}</p>
              )}

              {settings && !settings.demo && (
                <dl className="settings-status">
                  <div>
                    <dt>{t("本机设备")}</dt>
                    <dd>{settings.deviceLabel} · {settings.deviceId}</dd>
                  </div>
                  <div>
                    <dt>{t("上次同步")}</dt>
                    <dd>{settings.enabled ? formatSyncTime(settings.lastExportMs) : t("同步未开启")}</dd>
                  </div>
                  {settings.lastError && (
                    <div>
                      <dt>{t("同步告警")}</dt>
                      <dd className="settings-error-text">{settings.lastError}</dd>
                    </div>
                  )}
                </dl>
              )}

              {settings?.enabled && (
                <div className="settings-subsection">
                  <h3>{t("已发现的设备")}</h3>
                  {settings.devices.length === 0 ? (
                    <p className="settings-muted">{t("暂无其他设备的导出文件。其他电脑指向同一文件夹后即会显示。")}</p>
                  ) : (
                    <ul className="settings-device-list">
                      {settings.devices.map((device) => (
                        <li key={device.id}>
                          <div className="settings-device-head">
                            <div className="settings-device-info">
                              <strong>{device.label}</strong>
                              <span>{device.id}</span>
                              <small>{t("{count} 条事件 · 导出于 {time}", { count: device.events, time: formatSyncTime(device.exportedAtMs) })}</small>
                            </div>
                            {removingDeviceId !== device.id && (
                              <button
                                type="button"
                                className="ledger-button ledger-button--secondary settings-device-remove"
                                disabled={busy || settings?.demo}
                                onClick={() => setRemovingDeviceId(device.id)}
                              >
                                <Trash size={15} weight="light" aria-hidden="true" />
                                {t("删除")}
                              </button>
                            )}
                          </div>
                          {removingDeviceId === device.id && (
                            <div className="ledger-confirmation" role="group" aria-labelledby={`device-confirm-title-${device.id}`}>
                              <strong id={`device-confirm-title-${device.id}`}>{t("删除该设备？")}</strong>
                              <p>{t("将移除该设备的同步事件与共享文件夹中的导出文件。若该设备仍在线，会在下次同步后重新出现。")}</p>
                              <div className="ledger-confirm-actions">
                                <button
                                  type="button"
                                  className="ledger-button ledger-button--secondary"
                                  disabled={busy}
                                  onClick={() => setRemovingDeviceId(null)}
                                >
                                  {t("取消")}
                                </button>
                                <button
                                  type="button"
                                  className="ledger-button ledger-button--primary"
                                  disabled={busy}
                                  onClick={() => removeDevice(device.id)}
                                >
                                  {busy ? t("正在删除…") : t("确认删除")}
                                </button>
                              </div>
                            </div>
                          )}
                        </li>
                      ))}
                    </ul>
                  )}
                </div>
              )}
            </div>
            <StartupCard
              autoUpdateCheck={autoUpdateCheck}
              onAutoUpdateCheck={onAutoUpdateCheck}
              availableUpdate={availableUpdate}
            />
            <AboutCard />
          </>
        )}
      </div>
    </main>
  );
}

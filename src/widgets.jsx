import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { flushSync } from "react-dom";
import {
  ArrowDown,
  ArrowsClockwise,
  ArrowsDownUp,
  ArrowsInLineVertical,
  ArrowsInSimple,
  ArrowsLeftRight,
  ArrowsOutSimple,
  ArrowUp,
  Check,
  CircleHalfTilt,
  CornersOut,
  DotsThree,
  FunnelSimple,
  Minus,
  Moon,
  PushPinSimple,
  ShieldCheck,
  Sun,
  X,
} from "@phosphor-icons/react";
import { glassShellAppearance, nextGlassTint } from "./glassAppearance.js";
import { compactTokens } from "./tokenFormat.js";
import { bindingWindow, formatReset, isBalanceWindow } from "./quotaWindows.js";
import { t } from "./i18n.js";
import { desyncHealRetryDelayMs, horizontalStripTargetWidth } from "./windowGeometry";
import {
  beginStripControlsExpand,
  cancelStripControlsExpand,
  closeWindow,
  collapseStripControlsExpand,
  collapseVerticalStripHover,
  expandVerticalStripHover,
  isDesktop,
  minimizeWindow,
  onScaleFactorChanged,
  reassertCompactSize,
  resizeCompactWindow,
  resizeMacosPanel,
  resizeStripWindow,
  startWindowDragging,
  toggleMaximizeWindow,
} from "./windowClient";
import { IS_LINUX, IS_MAC, IS_WINDOWS } from "./platform.js";
import { AGENT_META, AgentMark } from "./agentMeta.jsx";
import { GLASS_TINT_OPTIONS, normalizeGlassTint, THEME_OPTIONS } from "./appearanceOptions.js";
import {
  agentQuotaFor,
  compactDisplayWindows,
  compactQuotaTooltip,
  compactQuotaWindows,
  formatBalance,
  quotaEmptyCopy,
  quotaProvenance,
  quotaSeverity,
  quotaUsedPercent,
  shortWindowLabel,
  snapshotIsPartial,
  statusDotTitle,
  stripCellData,
  StripDetailCard,
  UNAVAILABLE_QUOTA,
} from "./quotaView.jsx";
import { PeriodControl, PERIODS, sourceStatusCopy } from "./viewParts.jsx";

// 胶囊条首帧尺寸估计：一格只有图标 + 百分比两层，横条约 54px 宽、竖条约
// 46px 高。
// 这些常量只用于进入 strip 的第一帧，之后的窗口尺寸由 StripBar 里的
// 内容测量观察器按真实渲染结果收敛——不同字体/DPI/缩放比例、有无更新点
// 都不会再裁掉内容（曾因常量与 CSS 脱钩裁掉竖条最后一个按钮）。
const STRIP_CELL_WIDTH = 54;
// 控件区只剩状态灯/菜单入口那一个 26px 槽，两个平台一样：横条是外壳
// padding 11 + 槽 26 + 格间距，竖条是外壳 padding 10 + 控件区 padding 4 + 槽 26。
const STRIP_CHROME_WIDTH = 40;
// 条高的下限是 26px 的控件槽，不是格子内容（图标 16 + 上下 padding = 24）。
const STRIP_BAR_HEIGHT = 28;
// 竖条宽度由 26px 控件槽 + 外壳 padding 定死下限（32px）；42 留 10px 呼吸，
// 再宽图标和百分比周围就空得发肥。
const STRIP_VERTICAL_WIDTH = 42;
const STRIP_DETAIL_WINDOW_WIDTH = 312;
const STRIP_DETAIL_CARD_MARGIN = 12;
const STRIP_DETAIL_LEAVE_DELAY = 180;
const STRIP_VCELL_HEIGHT = 46;
// 横条宽度的收缩迟滞。一格 54px，所以 6px 远低于「真的少了一个 Agent」，
// 又高于 DPI/zoom 取整带来的亚像素噪声。
const STRIP_WIDTH_SHRINK_SLACK = 6;
const STRIP_VCHROME_HEIGHT = 40;

export function stripWindowSize(orientation, count) {
  const cells = Math.max(1, count);
  if (orientation === "vertical") {
    return {
      width: STRIP_VERTICAL_WIDTH,
      height: STRIP_VCHROME_HEIGHT + STRIP_VCELL_HEIGHT * cells,
    };
  }
  return { width: STRIP_CHROME_WIDTH + STRIP_CELL_WIDTH * cells, height: STRIP_BAR_HEIGHT };
}

/// 竖条内容高度：第一格到控件区 + 外壳 padding（CSS px）。竖条的格子是
/// flex:none，高度由真实内容决定，测量值双向可信（过裁则长、过高则收）。
function measureStripVerticalContent(shell) {
  const first = shell.querySelector(".strip-cell, .strip-empty");
  const controls = shell.querySelector(".strip-controls");
  if (!first || !controls) return null;
  const style = window.getComputedStyle(shell);
  const firstRect = first.getBoundingClientRect();
  const controlsRect = controls.getBoundingClientRect();
  // 1px 余量：分数 DPI 下物理像素取整最多吃掉不到 1 个 CSS px。
  return (
    controlsRect.bottom - firstRect.top +
    parseFloat(style.paddingTop) + parseFloat(style.paddingBottom) + 1
  );
}

/// 横条目标宽度：格子按设计宽 54/格（图标 + 三位百分比是字体无关的有界
/// 内容），控件区取实测自然宽（flex:none 永不被压缩；有无更新点、macOS
/// 有无固定键都会变）。横条格子 flex:1 会拉伸填满窗口，布局测量推不出
/// "窗口过宽"，所以格数部分必须用设计宽计算，窗口才能随格数增减伸缩。
function measureStripHorizontalTarget(shell) {
  const controls = shell.querySelector(".strip-controls");
  if (!controls) return null;
  const style = window.getComputedStyle(shell);
  const cellCount = Math.max(1, shell.querySelectorAll(".strip-cell").length);
  const controlsWidth = controls.getBoundingClientRect().width;
  return horizontalStripTargetWidth({
    cellCount,
    cellWidth: STRIP_CELL_WIDTH,
    controlsWidth,
    paddingLeft: parseFloat(style.paddingLeft),
    paddingRight: parseFloat(style.paddingRight),
    gap: parseFloat(style.columnGap || style.gap) || 0,
  });
}

// 小组件窗口高度跟随 Agent 行数。内容自然高 = 各行 getBoundingClientRect
// 实测之和（行高固定 52px，见 styles.css 的 .widget-agent-list grid-auto-rows）；
// 非列表部分（标题栏/双块/页脚/间距）也是实测（shell.clientHeight - list.clientHeight）。
// 不写布局常量——写死的常量差 4px 就会逼出一条本不该出现的滚动条。
// 列表至少一行高：只选一个 Agent 时卡片随之收短，不再固定 320。
const COMPACT_LIST_MIN_HEIGHT = 52;
// 卡片窗口高度下限：非列表实测部分约 208px + 单行 52px，留少量余量取 260，
// 与 windowClient 的 WINDOW_SIZES.compact.minHeight 一致。
const COMPACT_MIN_WINDOW_HEIGHT = 260;

let windowActionQueue = Promise.resolve();
let latestWindowCorrection = 0;

export function runWindowAction(action) {
  // hide/move/zoom/resize/show 是一条事务；并发执行时旧屏幕 factor 算出的迟到
  // resize 会覆盖新屏幕上的修正。统一排队，失败后也让后续操作继续。
  windowActionQueue = windowActionQueue.then(action).catch((error) => {
    console.warn("Unable to update the desktop window.", error);
  });
  return windowActionQueue;
}

// 作废还在排队的窗口修正：模式切换等新事务开始时，旧修正不再执行。
export function invalidateWindowCorrections() {
  latestWindowCorrection += 1;
}

export function runLatestWindowCorrection(action) {
  const correction = ++latestWindowCorrection;
  return runWindowAction(() => {
    if (correction !== latestWindowCorrection) return undefined;
    return action(() => correction === latestWindowCorrection);
  });
}

/// 标题栏的主题快捷键：单击在亮/暗之间切换，右键（或长按）弹出含「自动」的
/// 三选菜单——单击不进「自动」是刻意的：一次点击只该有一个确定结果，
/// 而「自动」的结果取决于系统当前是什么。
function ThemeQuickToggle({ theme, darkTheme, onThemeChange }) {
  const [menuOpen, setMenuOpen] = useState(false);
  const wrapRef = useRef(null);

  useEffect(() => {
    if (!menuOpen) return undefined;
    const dismiss = (event) => {
      if (!wrapRef.current?.contains(event.target)) setMenuOpen(false);
    };
    const onEscape = (event) => {
      if (event.key === "Escape") setMenuOpen(false);
    };
    document.addEventListener("mousedown", dismiss);
    document.addEventListener("keydown", onEscape);
    return () => {
      document.removeEventListener("mousedown", dismiss);
      document.removeEventListener("keydown", onEscape);
    };
  }, [menuOpen]);

  const label = theme === "auto" ? t("自动（跟随系统）") : theme === "dark" ? t("暗色") : t("亮色");
  return (
    <div className="theme-quick" ref={wrapRef}>
      <button
        type="button"
        className={`window-action ${menuOpen ? "window-action--active" : ""}`}
        onClick={() => onThemeChange(darkTheme ? "light" : "dark")}
        onContextMenu={(event) => {
          event.preventDefault();
          setMenuOpen((open) => !open);
        }}
        aria-label={t("切换明暗，当前{label}", { label })}
        aria-haspopup="menu"
        aria-expanded={menuOpen}
        title={t("当前：{label}\n点击切换明暗 · 右键选择模式", { label })}
      >
        {darkTheme ? (
          <Moon size={16} weight="light" aria-hidden="true" />
        ) : (
          <Sun size={16} weight="light" aria-hidden="true" />
        )}
      </button>
      {menuOpen && (
        <div className="theme-quick-menu" role="menu">
          {THEME_OPTIONS.map((option) => (
            <button
              key={option.id}
              type="button"
              role="menuitemradio"
              aria-checked={theme === option.id}
              className={theme === option.id ? "is-selected" : ""}
              onClick={() => {
                onThemeChange(option.id);
                setMenuOpen(false);
              }}
            >
              {t(option.label)}
              {theme === option.id && <Check size={13} weight="bold" aria-hidden="true" />}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export function WindowActions({ mode, pinned, glassTint = "dark", macMinimal = false, theme, darkTheme, onThemeChange, onToggleMode, onTogglePinned, onToggleTransparent }) {
  const glassName = (id) =>
    t(GLASS_TINT_OPTIONS.find((option) => option.id === id)?.label || "深色");
  const glassCurrent = normalizeGlassTint(glassTint);
  const glassNext = nextGlassTint(glassCurrent);
  const glassLabel = t("{current} · 点击切为{next}", { current: glassName(glassCurrent), next: glassName(glassNext) });
  return (
    <div className={`window-actions window-actions--${mode}`} aria-label={t("窗口操作")}>
      {mode === "expanded" && (
        <>
          <button
            type="button"
            className="window-action"
            onClick={() => onToggleMode("compact")}
            aria-label={t("收起为桌面小组件")}
            title={t("收起为桌面小组件")}
          >
            <ArrowsInSimple size={17} weight="light" aria-hidden="true" />
          </button>
          <button
            type="button"
            className="window-action"
            onClick={() => onToggleMode("strip")}
            aria-label={t("折叠为胶囊条")}
            title={t("折叠为胶囊条")}
          >
            <ArrowsInLineVertical size={16} weight="light" aria-hidden="true" />
          </button>
          <ThemeQuickToggle theme={theme} darkTheme={darkTheme} onThemeChange={onThemeChange} />
        </>
      )}
      {mode === "compact" && !macMinimal && (
        <button
          type="button"
          className="window-action"
          onClick={() => onToggleMode("strip")}
          aria-label={t("折叠为胶囊条")}
          title={t("折叠为胶囊条")}
        >
          <ArrowsInLineVertical size={16} weight="light" aria-hidden="true" />
        </button>
      )}
      {mode === "compact" && !macMinimal && (
        <button
          type="button"
          className="window-action window-action--active"
          onClick={onToggleTransparent}
          aria-label={t("外观：{label}", { label: glassLabel })}
          title={t("外观：{label}", { label: glassLabel })}
        >
          <CircleHalfTilt size={16} weight="fill" aria-hidden="true" />
        </button>
      )}
      {!macMinimal && mode !== "expanded" && (
        <button
          type="button"
          className={`window-action ${pinned ? "window-action--active" : ""}`}
          onClick={onTogglePinned}
          aria-label={pinned ? t("取消固定，恢复拖动") : t("固定在当前位置并置顶")}
          aria-pressed={pinned}
          title={pinned ? t("取消固定，恢复拖动") : t("固定在当前位置并置顶")}
        >
          <PushPinSimple size={17} weight={pinned ? "fill" : "light"} aria-hidden="true" />
        </button>
      )}
      {!macMinimal && (
        <>
          <button
            type="button"
            className="window-action"
            onClick={() => runWindowAction(minimizeWindow)}
            aria-label={t("最小化")}
            title={t("最小化")}
          >
            <Minus size={17} weight="light" aria-hidden="true" />
          </button>
          {mode === "expanded" && (
            <button
              type="button"
              className="window-action"
              onClick={() => runWindowAction(toggleMaximizeWindow)}
              aria-label={t("最大化或还原")}
              title={t("最大化 / 还原")}
            >
              <CornersOut size={16} weight="light" aria-hidden="true" />
            </button>
          )}
          <button
            type="button"
            className="window-action window-action--close"
            onClick={() => runWindowAction(closeWindow)}
            aria-label={t("隐藏到托盘")}
            title={t("隐藏到托盘")}
          >
            <X size={17} weight="light" aria-hidden="true" />
          </button>
        </>
      )}
    </div>
  );
}

function handleGlassPointerMove(event) {
  const shell = event.currentTarget;
  const bounds = shell.getBoundingClientRect();
  if (!bounds.width || !bounds.height) return;
  const frame = shell.parentElement?.id === "root" ? shell.parentElement : shell;
  const x = Math.max(0, Math.min(100, ((event.clientX - bounds.left) / bounds.width) * 100));
  const y = Math.max(0, Math.min(100, ((event.clientY - bounds.top) / bounds.height) * 100));
  frame.style.setProperty("--glass-pointer-x", `${x}%`);
  frame.style.setProperty("--glass-pointer-y", `${y}%`);
  frame.style.setProperty("--glass-edge-opacity", "1");
}

function handleGlassPointerLeave(event) {
  const shell = event.currentTarget;
  const frame = shell.parentElement?.id === "root" ? shell.parentElement : shell;
  frame.style.setProperty("--glass-edge-opacity", "0");
}

function glassPointerProps(enabled) {
  return enabled
    ? {
        onPointerMove: handleGlassPointerMove,
        onPointerLeave: handleGlassPointerLeave,
      }
    : {};
}

export function StripBar({
  snapshot,
  agents,
  pinned,
  loading,
  glassAlpha = 0.82,
  glassMode = "css",
  glassTint = "dark",
  glassInk = "dark",
  orientation,
  onToggleOrientation,
  onTogglePinned,
  onRestore,
  onExpand,
  availableUpdate,
  onOpenUpdate,
}) {
  // 用户自选的 agent 一律占格；没有官方配额数据的显示 "--"，不伪造数字。
  const cells = agents.map((agentId) => ({
    agentId,
    cell: stripCellData(agentQuotaFor(snapshot, agentId)),
  }));
  const vertical = orientation === "vertical";
  // 透明档的真实桌面背景变化很大，控制图标加粗以稳定识别。
  const buttonWeight = glassTint === "clear" ? "bold" : "regular";
  const shellRef = useRef(null);
  const railRef = useRef(null);
  const detailCardRef = useRef(null);
  const pointerLeaveTimerRef = useRef(null);
  const hoverTransitionRef = useRef(false);
  const OrientationIcon = vertical ? ArrowsLeftRight : ArrowsDownUp;
  const detailEnabled = (IS_WINDOWS || IS_LINUX) && vertical && !(pinned && IS_LINUX);
  const [hoveredDetail, setHoveredDetail] = useState(null);
  // 控制菜单与详情卡不能共用同一次临时扩窗：菜单需要按真实条身高度扩展，
  // 固定/切形态按钮也必须落在原生窗口的有效命中区内。
  const [controlsOpen, setControlsOpen] = useState(false);
  const [detailLayout, setDetailLayout] = useState({
    side: IS_LINUX ? "left" : "right",
    railOffsetY: 0,
  });
  const detailCell = detailEnabled && !controlsOpen && hoveredDetail
    ? cells.find(({ agentId }) => agentId === hoveredDetail.agentId) ?? null
    : null;
  const detailOpen = Boolean(detailCell);
  // 原生窗口是否处于扩宽的透明画布态。它和 detailOpen 不同步：收起时详情卡
  // 先消失，但画布布局要保持到原生窗口缩回之后，否则普通布局会先画进宽画布，
  // 胶囊在屏幕上往左跳几帧（实测 47–76ms）。条身偏移也只在画布态生效。
  const [canvasOpen, setCanvasOpen] = useState(false);
  const canvasLayout = detailOpen || canvasOpen;
  // 控制按钮不再常驻：四个 26px 的槽在竖条上要吃掉 130px，比三个格子还高。
  // 改成按需就地展开——点状态灯位上浮出的 … 把它们放出来，条身随之变长，
  // 指针离开自动收回。窗口尺寸本来就跟着内容测量走，这里不用另外调窗。
  const closeStripControls = useCallback(() => {
    setHoveredDetail(null);
    // 收起时先原生还原（单批事务）再收 DOM：按钮随窗口缩短被裁掉，
    // 不会出现"按钮已消失、空玻璃长条还在"的中间帧。不能走可丢弃的
    // 代次令牌——令牌一失效 setControlsOpen 就被跳过，菜单永远收不回；
    // 还原本身丢了也无妨（fit 观察器兜底走旧路径）。
    runWindowAction(async () => {
      await collapseStripControlsExpand();
      setControlsOpen(false);
    });
  }, []);
  const toggleControls = useCallback(() => {
    latestWindowCorrection += 1;
    setHoveredDetail(null);
    if (controlsOpen) {
      closeStripControls();
      return;
    }
    // 展开前排入还原几何捕获：队列顺序保证它先于 fit 观察器的加高事务，
    // 读到的是收起态的稳定几何。
    runWindowAction(beginStripControlsExpand);
    setControlsOpen(true);
  }, [controlsOpen, closeStripControls]);
  // 一旦进入置顶只读态，立即丢弃胶囊的临时操作面板状态，只保留数据展示。
  useEffect(() => {
    if (pinned && IS_LINUX) closeStripControls();
  }, [pinned, closeStripControls]);
  useEffect(() => {
    if (!detailEnabled || controlsOpen) setHoveredDetail(null);
  }, [controlsOpen, detailEnabled]);
  useEffect(() => {
    if (!isDesktop()) return undefined;
    const stop = onScaleFactorChanged(() => setHoveredDetail(null));
    return () => { stop.then((unlisten) => unlisten?.()); };
  }, []);
  const shellAppearance = glassShellAppearance("strip", {
    glassMode,
    glassTint,
    glassInk,
    glassAlpha,
    isMac: IS_MAC,
    vertical,
  });
  useLayoutEffect(() => {
    if (!detailOpen) {
      latestWindowCorrection += 1;
      if (isDesktop() && (IS_WINDOWS || IS_LINUX)) {
        runWindowAction(async () => {
          await collapseVerticalStripHover();
          setCanvasOpen(false);
        });
      }
      return;
    }
    const rail = railRef.current;
    const card = detailCardRef.current;
    if (!rail || !card) return;
    const railRect = rail.getBoundingClientRect();
    const cardRect = card.getBoundingClientRect();
    const railHeight = Math.ceil(Math.max(railRect.height, rail.scrollHeight));
    const targetHeight = Math.max(
      railHeight,
      Math.ceil(cardRect.height + STRIP_DETAIL_CARD_MARGIN * 2),
    );
    if (!isDesktop()) {
      const shellHeight = shellRef.current?.clientHeight || window.innerHeight;
      const halfCard = cardRect.height / 2;
      const cardCenterY = Math.min(
        Math.max(hoveredDetail.anchorY, halfCard + STRIP_DETAIL_CARD_MARGIN),
        Math.max(shellHeight - halfCard - STRIP_DETAIL_CARD_MARGIN, halfCard + STRIP_DETAIL_CARD_MARGIN),
      );
      const pointerY = Math.min(
        Math.max(hoveredDetail.anchorY - cardCenterY + halfCard, 22),
        Math.max(cardRect.height - 22, 22),
      );
      setDetailLayout({
        side: IS_LINUX ? "left" : "right",
        railOffsetY: 0,
        cardCenterY,
        pointerY,
      });
      return;
    }
    runLatestWindowCorrection(async (isLatest) => {
      hoverTransitionRef.current = true;
      let layout;
      try {
        layout = await expandVerticalStripHover({
          width: STRIP_DETAIL_WINDOW_WIDTH,
          height: targetHeight,
          railWidth: STRIP_VERTICAL_WIDTH,
          railHeight,
          anchorY: hoveredDetail.anchorY,
          cardHeight: Math.ceil(cardRect.height),
        }, (predicted) => {
          // 原生窗口已换原点：同步提交条身偏移，不等下一轮调度。即使这次展开
          // 已被更新的悬停取代也要提交——布局必须和原生几何一致，否则条身停在
          // 原地扩宽阶段的左对齐布局里，直到下一轮展开才纠正（鼠标边移动边
          // 进入时实测停留 100ms 以上）。
          flushSync(() => {
            setCanvasOpen(true);
            setDetailLayout(predicted);
          });
        }, () => {
          // 原地扩宽阶段：条身贴画布左上角（即原位），详情卡近乎透明地预先绘制。
          flushSync(() => {
            setCanvasOpen(true);
            setDetailLayout((current) => ({ ...current, side: "left", railOffsetY: 0, growing: true }));
          });
        });
      } finally {
        hoverTransitionRef.current = false;
      }
      // 同上：布局跟随原生几何，与这次展开是否已过期无关。
      if (layout) {
        setCanvasOpen(true);
        setDetailLayout(layout);
      }
      if (!isLatest()) return;
      if (!layout) {
        setHoveredDetail((current) =>
          current?.agentId === hoveredDetail.agentId ? null : current,
        );
      }
    });
  }, [detailOpen, hoveredDetail?.agentId, hoveredDetail?.anchorY]);
  useEffect(() => () => {
    window.clearTimeout(pointerLeaveTimerRef.current);
    latestWindowCorrection += 1;
    cancelStripControlsExpand();
    if (IS_WINDOWS || IS_LINUX) runWindowAction(collapseVerticalStripHover);
  }, []);
  // 窗口尺寸跟随真实内容（通用方案，替代手写常量）：每次渲染后与视口变化时
  // 复核目标尺寸，差 ≥1px 才调窗；量的是 CSS px，resizeStripWindow 内部统一
  // 乘缩放系数与 DPI。任何字体/DPI/缩放/更新点组合都收敛，不再裁按钮。
  useLayoutEffect(() => {
    if (IS_MAC || !isDesktop()) return undefined;
    const shell = shellRef.current;
    if (!shell) return undefined;
    let timer = null;
    const fit = () => {
      timer = null;
      if (canvasLayout) return;
      const isVertical = shell.classList.contains("strip-shell--vertical");
      if (isVertical) {
        const targetHeight = measureStripVerticalContent(shell);
        if (!targetHeight) return;
        // 交叉轴是设计常量：竖条恒为 52 宽（方向切换后窗口可能还停在横条宽度）。
        if (
          Math.abs(targetHeight - shell.clientHeight) <= 1
          && Math.abs(shell.clientWidth - STRIP_VERTICAL_WIDTH) <= 1
        ) {
          return;
        }
        runLatestWindowCorrection(() =>
          resizeStripWindow({ width: STRIP_VERTICAL_WIDTH, height: Math.ceil(targetHeight) }),
        );
        return;
      }
      const targetWidth = measureStripHorizontalTarget(shell);
      if (!targetWidth) return;
      // 交叉轴是设计常量：横条恒为 40 高。
      // 宽度方向做不对称迟滞：变宽立即跟进（否则格子被压到内容以下，图标和
      // 百分比会叠在一起），变窄要超过一格的余量才动。测量噪声只会让目标略微
      // 变小，对称的 1px 阈值会让它和调窗形成震荡（双屏用户实拍到持续闪烁）。
      const widthDelta = targetWidth - shell.clientWidth;
      if (
        widthDelta < 1
        && widthDelta > -STRIP_WIDTH_SHRINK_SLACK
        && Math.abs(shell.clientHeight - STRIP_BAR_HEIGHT) <= 1
      ) {
        return;
      }
      runLatestWindowCorrection(() =>
        resizeStripWindow({ width: Math.ceil(targetWidth), height: STRIP_BAR_HEIGHT }),
      );
    };
    const schedule = () => {
      window.clearTimeout(timer);
      // 60ms：缓存未命中时的尺寸修正也要尽量落在同一帧动画里，不让人看见。
      timer = window.setTimeout(fit, 60);
    };
    schedule();
    // webfont 换字可能发生在 60ms 首次 fit 之后（没有触发器补测就会留下
    // 测量空窗，竖条底部按钮被裁）：字体就绪补测一次，再兜底一个 450ms
    // 的二次测量。
    let disposed = false;
    document.fonts?.ready?.then(() => {
      if (!disposed) schedule();
    });
    const lateTimer = window.setTimeout(schedule, 450);
    if (typeof ResizeObserver === "undefined") {
      return () => {
        disposed = true;
        window.clearTimeout(timer);
        window.clearTimeout(lateTimer);
      };
    }
    const observer = new ResizeObserver(schedule);
    observer.observe(shell);
    return () => {
      disposed = true;
      window.clearTimeout(timer);
      window.clearTimeout(lateTimer);
      observer.disconnect();
    };
  });
  const glassProps = glassPointerProps(shellAppearance.edgeInteractive);
  const cancelPointerLeave = () => {
    window.clearTimeout(pointerLeaveTimerRef.current);
    pointerLeaveTimerRef.current = null;
  };
  // Windows 扩宽并重定位原生窗口时，WebView 会短暂收到 pointerleave；立即收回
  // 会与仍停在胶囊上的指针形成展开/收起循环。短缓冲只跨过这次原生事务，
  // 真正离开整个透明承载区后仍会自动收回。
  const handlePointerLeave = (event) => {
    glassProps.onPointerLeave?.(event);
    cancelPointerLeave();
    const closeAfterTransition = () => {
      if (hoverTransitionRef.current) {
        pointerLeaveTimerRef.current = window.setTimeout(closeAfterTransition, STRIP_DETAIL_LEAVE_DELAY);
        return;
      }
      pointerLeaveTimerRef.current = null;
      if (shellRef.current?.matches(":hover")) return;
      setHoveredDetail(null);
      closeStripControls();
    };
    pointerLeaveTimerRef.current = window.setTimeout(closeAfterTransition, STRIP_DETAIL_LEAVE_DELAY);
  };
  const handleDragPointerDown = (event) => {
    if (
      event.button !== 0
      || pinned
      || IS_MAC
      || !isDesktop()
      || event.target.closest?.("button, a, input, select, textarea, [role='button']")
    ) {
      return;
    }
    // 原生拖动必须排在详情扩窗收回之后。直接依赖 data-tauri-drag-region 会让
    // Windows 先进入系统拖动循环，详情卡随后才收回并用旧坐标覆盖拖动结果。
    event.preventDefault();
    event.stopPropagation();
    cancelPointerLeave();
    latestWindowCorrection += 1;
    setHoveredDetail(null);
    runWindowAction(async () => {
      await collapseVerticalStripHover();
      await startWindowDragging();
    });
  };
  const dragProps = pinned || IS_MAC ? {} : { onPointerDown: handleDragPointerDown };
  const showDetail = (event, agentId) => {
    if (!detailEnabled || controlsOpen) return;
    cancelPointerLeave();
    const railBounds = railRef.current?.getBoundingClientRect();
    const cellBounds = event.currentTarget.getBoundingClientRect();
    if (!railBounds) return;
    setHoveredDetail({
      agentId,
      anchorY: cellBounds.top - railBounds.top + cellBounds.height / 2,
    });
  };
  const shellClassName = [
    shellAppearance.className,
    canvasLayout ? "strip-shell--detail-open" : "",
    canvasLayout ? `strip-shell--detail-${detailLayout.side}` : "",
    canvasLayout && detailLayout.growing ? "strip-shell--detail-growing" : "",
  ].filter(Boolean).join(" ");
  return (
    <main
      ref={shellRef}
      className={shellClassName}
      data-glass-surface={shellAppearance.trueAlpha ? "true-alpha" : undefined}
      data-pinned={pinned && IS_LINUX ? "true" : undefined}
      inert={pinned && IS_LINUX ? true : undefined}
      {...glassProps}
      onPointerEnter={cancelPointerLeave}
      onPointerLeave={handlePointerLeave}
      style={{
        ...shellAppearance.style,
        "--strip-rail-offset-y": `${canvasOpen ? detailLayout.railOffsetY : 0}px`,
        "--strip-detail-card-center-y": `${detailLayout.cardCenterY ?? ((hoveredDetail?.anchorY || 0) + detailLayout.railOffsetY)}px`,
        "--strip-detail-pointer-y": Number.isFinite(detailLayout.pointerY)
          ? `${detailLayout.pointerY}px`
          : "50%",
        ...(pinned ? { cursor: "default" } : {}),
      }}
    >
      <h1 className="sr-only">{t("Metrik 官方配额胶囊条")}</h1>
      <div ref={railRef} className="strip-rail" {...dragProps}>
      {cells.length ? (
        cells.map(({ agentId, cell }) => {
          const meta = AGENT_META[agentId];
          if (!cell) {
            return (
              <div
                key={agentId}
                className="strip-cell strip-cell--unavailable"
                aria-label={t("{agent}：官方配额不可用", { agent: meta.label })}
                onPointerEnter={(event) => showDetail(event, agentId)}
              >
                <img
                  className={`strip-cell-icon ${meta.iconClass || ""}`}
                  src={meta.iconSrc}
                  alt={meta.label}
                  draggable={false}
                />
                <span className="strip-cell-body">
                  <em>--</em>
                </span>
              </div>
            );
          }
          const view = cell.tightest.view;
          // 余额窗口：一格显示货币金额而不是百分比，aria 同步用货币文案。
          const balance = isBalanceWindow(cell.tightest);
          const severity = quotaSeverity(view, cell.tightest.key);
          return (
            <div
              key={agentId}
              className={`strip-cell ${severity ? `strip-cell--${severity}` : ""}`}
              aria-label={balance
                ? t("{agent}：余额 {amount} · 官方余额", { agent: meta.label, amount: formatBalance(cell.tightest.key, view.remainingPercent) })
                : Number.isFinite(view.resetsInMinutes)
                  ? t("{agent}：剩余 {percent}%，{time}后重置", { agent: meta.label, percent: Math.round(view.remainingPercent), time: formatReset(view.resetsInMinutes) })
                  : t("{agent}：剩余 {percent}%", { agent: meta.label, percent: Math.round(view.remainingPercent) })}
              onPointerEnter={(event) => showDetail(event, agentId)}
            >
              <img
                className={`strip-cell-icon ${meta.iconClass || ""}`}
                src={meta.iconSrc}
                alt={meta.label}
                draggable={false}
              />
              <span className="strip-cell-body">
                <em className={balance ? "strip-balance" : undefined}>
                  {balance ? formatBalance(cell.tightest.key, view.remainingPercent, true) : `${Math.round(view.remainingPercent)}%`}
                </em>
              </span>
            </div>
          );
        })
      ) : (
        <span className="strip-empty">
          {t("配额不可用")}
        </span>
      )}
      <div className={`strip-controls ${controlsOpen && !(pinned && IS_LINUX) ? "strip-controls--open" : ""}`}>
        {availableUpdate && (
          <span className="strip-control-slot">
            <button
              type="button"
              className="update-dot"
              onClick={onOpenUpdate}
              aria-label={t("有新版本 {version}，打开设置更新", { version: availableUpdate.version })}
              title={t("有新版本 {version}，点击更新", { version: availableUpdate.version })}
            />
          </span>
        )}
        {/* 状态灯与展开入口共用一个 26px 槽：… 绝对定位压在灯上，
            悬停或展开时互换，收起态因此一个像素都不多占。 */}
        <span
          className="strip-control-slot strip-control-slot--menu"
          title={statusDotTitle(loading, snapshot.loadError)}
        >
          <i
            className={`status-dot ${loading ? "status-dot--loading" : ""} ${snapshot.loadError ? "status-dot--error" : ""}`}
            aria-hidden="true"
          />
          {!(pinned && IS_LINUX) && (
            <button
              type="button"
              className={`strip-button strip-button--menu ${controlsOpen ? "strip-button--active" : ""}`}
              onClick={toggleControls}
              aria-label={controlsOpen ? t("收起控制按钮") : t("展开控制按钮")}
              aria-expanded={controlsOpen}
              title={controlsOpen ? t("收起控制按钮") : t("展开控制按钮")}
            >
              <DotsThree size={16} weight={buttonWeight} aria-hidden="true" />
            </button>
          )}
        </span>
        {controlsOpen && !(pinned && IS_LINUX) && (
          <>
            {!IS_MAC && (
              <button
                type="button"
                className={`strip-button ${pinned ? "strip-button--active" : ""}`}
                onClick={onTogglePinned}
                aria-label={pinned ? t("取消固定，恢复拖动") : t("固定在当前位置并置顶")}
                aria-pressed={pinned}
                title={pinned ? t("取消固定，恢复拖动") : t("固定在当前位置并置顶")}
              >
                <PushPinSimple size={15} weight={pinned ? "fill" : buttonWeight} aria-hidden="true" />
              </button>
            )}
            <button
              type="button"
              className="strip-button"
              onClick={() => {
                // 换向后收起态的几何口径变了，预捕获的还原值作废，交给 fit 观察器。
                cancelStripControlsExpand();
                onToggleOrientation();
              }}
              aria-label={vertical ? t("切换为横条") : t("切换为竖条")}
              title={vertical ? t("切换为横条") : t("切换为竖条")}
            >
              <OrientationIcon size={15} weight={buttonWeight} aria-hidden="true" />
            </button>
            <button
              type="button"
              className="strip-button"
              onClick={onRestore}
              aria-label={t("展开为桌面小组件")}
              title={t("展开为桌面小组件")}
            >
              <ArrowsOutSimple size={15} weight={buttonWeight} aria-hidden="true" />
            </button>
            <button
              type="button"
              className="strip-button"
              onClick={onExpand}
              aria-label={t("打开完整视图")}
              title={t("完整视图")}
            >
              <CornersOut size={15} weight={buttonWeight} aria-hidden="true" />
            </button>
          </>
        )}
      </div>
      </div>
      {detailOpen && detailCell && (
        <StripDetailCard
          agentId={detailCell.agentId}
          cell={detailCell.cell}
          cardRef={detailCardRef}
        />
      )}
    </main>
  );
}

export function CompactWidget({
  snapshot,
  period,
  selectedAgent,
  visibleTokens,
  loading,
  pinned,
  glassMode = "css",
  glassTint = "dark",
  glassInk = "dark",
  onPeriodChange,
  onOpenSources,
  onTogglePinned,
  onToggleTransparent,
  onExpand,
  onRefresh,
  quotaAgent,
  onCycleQuotaAgent,
  widgetAgents,
  glassAlpha = 0.82,
  availableUpdate,
  onOpenUpdate,
}) {
  const comparisonIsFlat = Math.abs(snapshot.comparisonPercent) < 0.5;
  const comparisonIsLower = snapshot.comparisonPercent < -0.5;
  const ComparisonArrow = comparisonIsLower ? ArrowDown : ArrowUp;
  const shellRef = useRef(null);
  // 宽度失配自愈的节奏状态：lastDesyncHealRef 记本轮上次自愈时刻，
  // desyncHealAttemptRef 记本轮已自愈次数（决定下次间隔），inFlight 防止
  // reassert 内部收敛期间观察器再叠加新事务。间隔按 0/250/600/1200ms 升级、
  // 4 轮后退回 2s 兜底（desyncHealRetryDelayMs）：失配对用户可见（右列被裁、
  // 滚动条），不能让观众等满一个 2s 节流周期（切外观实拍挂满 2s+）；也不能
  // 退回每 120ms 无限重断言、持续打断 WebView。失配消失即清零回首轮；跨屏/
  // DPI 变化时同样清零，让新显示器上的自愈立即恢复资格。自愈只重断言当前
  // 尺寸，不 hide/show、不恢复记忆位置，避免旧显示器状态覆盖新 DPI。
  const lastDesyncHealRef = useRef(0);
  const desyncHealAttemptRef = useRef(0);
  const desyncHealInFlightRef = useRef(false);
  useEffect(() => {
    if (!isDesktop()) return undefined;
    const stop = onScaleFactorChanged(() => {
      lastDesyncHealRef.current = 0;
      desyncHealAttemptRef.current = 0;
    });
    return () => { stop.then((unlisten) => unlisten?.()); };
  }, []);
  // 一个观察器承担两条自愈：
  // 1) 宽度失配（zoom×物理尺寸失配，视口 < 320，右侧被裁）→ 按当前 DPI
  //    重断言尺寸（节流到 2s 一次，失配消失即止）；
  // 2) Agent 行数变化 → 窗口高度跟随内容（1-2 行回 320，更多行加高，
  //    工作区装不下的部分由列表内部滚动承担）。行数变化不改外壳尺寸，
  //    ResizeObserver 感知不到，所以每次渲染后再主动复核一次。
  useEffect(() => {
    if (!isDesktop()) return undefined;
    const shell = shellRef.current;
    if (!shell || typeof ResizeObserver === "undefined") return undefined;
    let timer = null;
    const check = () => {
      timer = null;
      const rect = shell.getBoundingClientRect();
      // 宽度失配（zoom×物理尺寸失配，视口 < 320，右侧被裁）是 Windows 单窗口
      // 变形独有的问题；macOS 面板没有 zoom，不会失配。
      const widthDesynced =
        shell.scrollWidth > shell.clientWidth + 1 || rect.width > window.innerWidth + 1;
      if (!IS_MAC && widthDesynced) {
        // 上一轮 reassert 还在内部收敛时不叠加新事务：两个循环互踩会来回
        // 改写尺寸；收敛循环自己会在新代次出现时让位。
        if (desyncHealInFlightRef.current) return;
        const now = Date.now();
        const attempt = desyncHealAttemptRef.current;
        if (now - lastDesyncHealRef.current < desyncHealRetryDelayMs(attempt)) return;
        lastDesyncHealRef.current = now;
        desyncHealAttemptRef.current = attempt + 1;
        desyncHealInFlightRef.current = true;
        runLatestWindowCorrection(async (isLatest) => {
          try {
            await reassertCompactSize(null, isLatest);
          } finally {
            desyncHealInFlightRef.current = false;
          }
        });
        return;
      }
      // 失配已消失：下一轮失配从首轮立即自愈重新开始。
      desyncHealAttemptRef.current = 0;
      const list = shell.querySelector(".widget-agent-list");
      if (!list) return;
      // 内容自然高 = 实际行高之和（getBoundingClientRect，与窗口大小无关）。
      // 绝不能用 list.scrollHeight：它不小于 clientHeight，窗口一大于内容
      // 就等于窗口高，目标高度会每轮 +4px 无限延伸（实测踩过）。
      const rowEls = list.querySelectorAll(".widget-agent");
      let natural = COMPACT_LIST_MIN_HEIGHT;
      if (rowEls.length) {
        const firstRow = rowEls[0].getBoundingClientRect();
        const lastRow = rowEls[rowEls.length - 1].getBoundingClientRect();
        natural = Math.max(COMPACT_LIST_MIN_HEIGHT, Math.ceil(lastRow.bottom - firstRow.top));
      }
      // 非列表部分实测 + 列表自然高 + 4px 余量（1fr 分配与取整的抖动）。
      // 用 scrollHeight 兜底：只有一个 Agent 时窗口会收到比内容还矮，底栏被
      // 裁在可视区外，clientHeight 就量不到它，于是"目标高度"一直等于当前
      // 高度，卡在底栏永远不显示的稳定错误态。
      const chrome = Math.max(shell.scrollHeight, shell.clientHeight) - list.clientHeight;
      const target = Math.max(COMPACT_MIN_WINDOW_HEIGHT, Math.round(chrome + natural + 4));
      if (IS_MAC) {
        // macOS 面板顶部锚定菜单栏图标：高度跟随内容（屏幕可用高 - 80 封顶），
        // 宽度恒为设计宽；resize 后面板会自己重算锚点。
        const cap = Math.max(
          COMPACT_MIN_WINDOW_HEIGHT,
          Math.floor((window.screen?.availHeight || 900) - 80),
        );
        const macTarget = Math.min(target, cap);
        if (Math.abs(macTarget - shell.clientHeight) >= 2) {
          runWindowAction(() => resizeMacosPanel({ height: macTarget }));
        }
        return;
      }
      if (Math.abs(target - shell.clientHeight) >= 2) {
        runLatestWindowCorrection(() => resizeCompactWindow({ height: target }));
      }
    };
    const schedule = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(check, 120);
    };
    const observer = new ResizeObserver(schedule);
    observer.observe(shell);
    schedule();
    return () => {
      window.clearTimeout(timer);
      observer.disconnect();
    };
  });
  // 标签必须描述快照本身的周期；切换周期的扫描期间不给旧数据贴新标签。
  const comparisonText = (lower, percent) => (snapshot.period === "today"
    ? (lower ? t("较近 7 日同时段低 {percent}%", { percent }) : t("较近 7 日同时段高 {percent}%", { percent }))
    : (lower ? t("较前一周期低 {percent}%", { percent }) : t("较前一周期高 {percent}%", { percent })));
  const flatComparisonLabel = snapshot.period === "today" ? t("与近 7 日同时段持平") : t("与前一周期持平");
  const switchingPeriod = !snapshot.pending && !snapshot.loadError && period !== snapshot.period;
  const quotaEntry = agentQuotaFor(snapshot, quotaAgent);
  const quotaWindows = compactQuotaWindows(quotaEntry);
  const quotaCurrentWindow = quotaWindows.find((window) => window.view.available);
  const quotaView = quotaCurrentWindow?.view || UNAVAILABLE_QUOTA;
  const quotaIsBalance = quotaCurrentWindow ? isBalanceWindow(quotaCurrentWindow) : false;
  const partial = snapshotIsPartial(snapshot);
  const sourceStatus = sourceStatusCopy(snapshot, loading, partial);
  const shellAppearance = glassShellAppearance("widget", {
    glassMode,
    glassTint,
    glassInk,
    glassAlpha,
    isMac: IS_MAC,
    loading,
  });

  return (
    <main
      ref={shellRef}
      className={shellAppearance.className}
      data-glass-surface={shellAppearance.trueAlpha ? "true-alpha" : undefined}
      data-pinned={pinned && IS_LINUX ? "true" : undefined}
      inert={pinned && IS_LINUX ? true : undefined}
      {...glassPointerProps(shellAppearance.edgeInteractive)}
      style={shellAppearance.style}
    >
      <h1 className="sr-only">{t("Metrik Agent 用量桌面小组件")}</h1>
      <header
        className="widget-titlebar"
        // 固定 = 置顶 + 锁定位置：去掉拖动区，窗口停在用户选定的位置。
        // macOS 面板贴着菜单栏图标，拖动无意义，也没有窗口按钮。
        {...(pinned || IS_MAC ? {} : { "data-tauri-drag-region": true })}
        style={pinned || IS_MAC ? { cursor: "default" } : undefined}
      >
        <div
          className="widget-brand"
          {...(pinned || IS_MAC ? {} : { "data-tauri-drag-region": true })}
        >
          <span>Metrik</span>
          <i
            className={`status-dot ${loading ? "status-dot--loading" : ""} ${snapshot.loadError ? "status-dot--error" : ""}`}
            title={statusDotTitle(loading, snapshot.loadError)}
            aria-hidden="true"
          />
          {availableUpdate && (
            <button
              type="button"
              className="update-dot"
              onClick={onOpenUpdate}
              aria-label={t("有新版本 {version}，打开设置更新", { version: availableUpdate.version })}
              title={t("有新版本 {version}，点击更新", { version: availableUpdate.version })}
            />
          )}
        </div>
        {!(pinned && IS_LINUX) && (
          <WindowActions
            mode="compact"
            pinned={pinned}
            glassTint={glassTint}
            macMinimal={IS_MAC}
            onToggleMode={onExpand}
            onTogglePinned={onTogglePinned}
            onToggleTransparent={onToggleTransparent}
          />
        )}
      </header>

      <div className="widget-content">
        <PeriodControl period={period} onChange={onPeriodChange} compact />

        <section className="widget-primary" aria-label={t("用量摘要")}>
          <div className="widget-metric">
            <span>
              {selectedAgent === "all" ? t("总用量") : AGENT_META[selectedAgent].label}
              {switchingPeriod ? t("（{period}）", { period: t(PERIODS.find((item) => item.id === snapshot.period)?.label) }) : ""}
            </span>
            <div aria-live="polite" aria-atomic="true">
              <strong>{snapshot.pending || snapshot.loadError ? "--" : compactTokens(visibleTokens)}</strong>
              <small>tokens</small>
            </div>
            <p className="widget-comparison">
              {switchingPeriod ? (
                <>{t("正在统计{period}数据…", { period: t(PERIODS.find((item) => item.id === period)?.label) })}</>
              ) : snapshot.pending ? (
                <>{t("正在建立本地索引")}</>
              ) : snapshot.loadError ? (
                <>{t("本地数据读取失败")}</>
              ) : selectedAgent !== "all" ? (
                <>
                  <FunnelSimple size={14} weight="light" aria-hidden="true" />
                  {t("已按 Agent 筛选")}
                </>
              ) : snapshot.comparisonAvailable ? (
                <>
                  {comparisonIsFlat ? (
                    flatComparisonLabel
                  ) : (
                    <>
                      <ComparisonArrow size={14} weight="bold" aria-hidden="true" />
                      {comparisonText(comparisonIsLower, Math.abs(snapshot.comparisonPercent).toFixed(0))}
                    </>
                  )}
                </>
              ) : (
                <>{period === "today" ? t("同时段基线建立中") : t("基线建立中")}</>
              )}
            </p>
          </div>

          <button
            className="widget-quota"
            style={{ "--quota-accent": AGENT_META[quotaAgent].accent }}
            type="button"
            onClick={onCycleQuotaAgent}
            aria-label={t("{agent} 配额，点击切换 Agent", { agent: AGENT_META[quotaAgent].label })}
            title={t("点击切换配额 Agent")}
          >
            <span>{t("{agent} 已用", { agent: AGENT_META[quotaAgent].label })}</span>
            {quotaWindows.map((window) => {
              // 余额窗口：金额没有"已用占比"，轨道画满作为中性表达。
              const balance = isBalanceWindow(window);
              const severity = quotaSeverity(window.view, window.key);
              const current = window.view.available && !window.view.resetExpired;
              return (
                <div
                  className={`widget-quota-window ${severity ? `widget-quota-window--${severity}` : ""}`}
                  key={window.key}
                >
                  <small>{shortWindowLabel(window.key)}</small>
                  <div className="widget-quota-track" aria-hidden="true">
                    <i style={{ transform: `scaleX(${current ? (balance ? 1 : quotaUsedPercent(window.view) / 100) : 0})` }} />
                  </div>
                  <em>{current
                    ? balance
                      ? formatBalance(window.key, window.view.remainingPercent)
                      : `${Math.round(quotaUsedPercent(window.view))}%`
                    : "--"}</em>
                </div>
              );
            })}
            <small>
              {quotaView.quality === "demo"
                ? quotaProvenance(quotaView)
                : quotaView.resetExpired
                  ? t("已重置，等待刷新")
                  : quotaView.available
                    ? quotaIsBalance
                      ? t("账户余额 · 不重置")
                      : t("{time}后重置", { time: formatReset(quotaView.resetsInMinutes) })
                    : quotaEmptyCopy(quotaEntry, quotaAgent, true)}
            </small>
          </button>
        </section>

        <section className="widget-agent-list" aria-label={t("各 Agent 官方剩余额度")}>
          {(() => {
            // 行集合只由用户的 Agent 选择决定（与胶囊条同一哲学：自选一律占格），
            // 没有可靠配额来源的 Agent 显示 "-- / 暂无可靠来源" 而不是消失——
            // 否则来源一抖动行数就变，窗口高度跟着内容跳来跳去（用户称之为"乱飘"）。
            return widgetAgents.filter((id) => AGENT_META[id]).map((agentId) => {
              const meta = AGENT_META[agentId];
              if (!meta) return null;
              const entry = agentQuotaFor(snapshot, agentId);
              const windows = compactDisplayWindows(entry);
              // 行内头条窗口与胶囊条同规则，见 quotaWindows.js；
              // 完整窗口明细在该行的原生 tooltip 与焦点卡中。
              const headline = bindingWindow(entry.windows) || windows[0] || null;
              const headlineView = headline?.view || null;
              const current = Boolean(headlineView && headlineView.available && !headlineView.resetExpired);
              const severity = headlineView ? quotaSeverity(headlineView, headline.key) : "";
              // 余额窗口的 remainingPercent 是金额（可大于 100），不做百分比钳制。
              const balance = headline ? isBalanceWindow(headline) : false;
              const remaining = headlineView ? Math.min(100, Math.max(0, headlineView.remainingPercent)) : 0;
              return (
                <div
                  className={`widget-agent ${severity ? `widget-agent--${severity}` : ""}`}
                  key={agentId}
                  style={{ "--quota-accent": meta.accent }}
                  title={compactQuotaTooltip(agentId, windows)}
                >
                  <i className="widget-agent-accent" style={{ backgroundColor: meta.accent }} aria-hidden="true" />
                  <AgentMark agentId={agentId} />
                  <span>
                    <strong>{meta.label}</strong>
                    {/* 窗口标签与名称同一行：5h/7d 剩余；无来源或已重置时明示状态。 */}
                    <small>
                      {current
                        ? `· ${shortWindowLabel(headline.key)}`
                        : headlineView
                          ? `· ${t("已重置，等待刷新")}`
                          : snapshot.pending
                            ? `· ${t("正在读取…")}`
                            : snapshot.loadError
                              ? `· ${t("读取失败")}`
                              : `· ${t("暂无可靠来源")}`}
                    </small>
                  </span>
                  {/* 展示剩余额度（用户关心的是还能用多少）。快照新鲜度不再
                      影响数字的写法与颜色：那一格已经写着窗口或"已重置，等待
                      刷新"，再加 ~ 前缀和灰化是重复信息，只会让人以为数字是
                      估算出来的。 */}
                  <em>{current
                    ? balance
                      ? formatBalance(headline.key, headlineView.remainingPercent)
                      : `${Math.round(remaining)}%`
                    : "--"}</em>
                </div>
              );
            });
          })()}
        </section>

        <footer className="widget-footer">
          <button
            type="button"
            className={`widget-source ${snapshot.loadError ? "widget-source--error" : ""} ${partial ? "widget-source--warning" : ""}`}
            onClick={onOpenSources}
            title={sourceStatus.detail}
          >
            <ShieldCheck size={15} weight="fill" aria-hidden="true" />
            <span>{sourceStatus.title}</span>
            <small>{sourceStatus.hint}</small>
          </button>
          <button
            type="button"
            className="widget-refresh"
            onClick={onRefresh}
            disabled={loading}
            aria-label={t("强制刷新官方配额与本地统计")}
            title={t("强制刷新官方配额与本地统计")}
          >
            <ArrowsClockwise size={13} weight="light" aria-hidden="true" />
          </button>
          <button type="button" className="widget-expand" onClick={() => onExpand("expanded")}>
            <span>{t("完整视图")}</span>
            <ArrowsOutSimple size={16} weight="light" aria-hidden="true" />
          </button>
        </footer>
      </div>
    </main>
  );
}

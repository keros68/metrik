

export const THEME_OPTIONS = [
  { id: "auto", label: "自动" },
  { id: "light", label: "亮色" },
  { id: "dark", label: "暗色" },
];

// 悬浮形态的窗口圆角，物理像素。10 收到 8：贴着壁纸看，10 的弧偏圆，靠近
// macOS 大部件而不是任务栏那一排小图标；8 更贴合胶囊的高度。
export const GLASS_RADIUS_PX = 8;

export const GLASS_TINT_OPTIONS = [
  { id: "dark", label: "深色" },
  { id: "light", label: "浅色" },
  { id: "clear", label: "透明" },
];

export function normalizeGlassTint(value) {
  return GLASS_TINT_OPTIONS.some((option) => option.id === value) ? value : "dark";
}

// 透明档的文字颜色。桌面挂件常见的两种风格：深色字配白霜（Pogget 一路），
// 或白色字直接压在壁纸上（Rainmeter 一路）。两者需要的底完全相反，所以
// 选文字颜色实际上也在选罩层，见 glassAppearance.js。
export const GLASS_INK_OPTIONS = [
  { id: "dark", label: "深色字" },
  { id: "light", label: "白色字" },
];

export const PINNED_HOVER_OPTIONS = [
  { id: "fade", label: "降低透明度" },
  { id: "hide", label: "完全隐藏" },
];
// X11 主通道不依赖透明窗口继续接收 DOM 事件，因此隐藏可以真正降到 0；
// 无全局坐标的本地回落会在 CSS 中保留 0.1% alpha 以维持命中区域。
export const PINNED_HIDDEN_OPACITY = 0;

export function normalizeGlassInk(value) {
  return GLASS_INK_OPTIONS.some((option) => option.id === value) ? value : "dark";
}

export function normalizePinnedHoverMode(value) {
  return PINNED_HOVER_OPTIONS.some((option) => option.id === value) ? value : "fade";
}

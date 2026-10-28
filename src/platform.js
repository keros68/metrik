import { isLinuxPlatform, isMacPlatform, isWindowsPlatform } from "./windowClient";

// macOS 是菜单栏应用：小插件是贴着菜单栏图标的面板（没有窗口按钮、不可拖动、
// 材质由系统 vibrancy 承担），完整视图是独立的标准窗口（原生红绿灯）。
// Windows 仍是"单窗口变形 + 自绘按钮"，两条路径不互相影响。
export const IS_MAC = isMacPlatform();
// Windows 任务栏托盘可换成余量数字徽标；其它平台没有这条路径。
export const IS_WINDOWS = isWindowsPlatform();
export const IS_LINUX = isLinuxPlatform();

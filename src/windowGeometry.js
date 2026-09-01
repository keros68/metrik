function normalizedScale(value) {
  return Number.isFinite(value) && value > 0 ? value : 1;
}

/// 只有稳定的悬浮形态才能参与位置记忆与边缘挂靠。竖条详情会临时移动、
/// 放大同一个原生窗口；那段几何不能被当成用户摆放结果。
function isStableFloatingMode(mode, transient = false) {
  if (transient) return false;
  return mode === "compact" || mode === "strip" || mode === "strip-horizontal" || mode === "strip-vertical";
}

function monitorArea(monitor) {
  if (!monitor?.position || !monitor?.size) return null;
  return {
    x: monitor.workArea?.position?.x ?? monitor.position.x,
    y: monitor.workArea?.position?.y ?? monitor.position.y,
    width: monitor.workArea?.size?.width ?? monitor.size.width,
    height: monitor.workArea?.size?.height ?? monitor.size.height,
  };
}

function overlapArea(rect, area) {
  const overlapX =
    Math.min(rect.x + rect.width, area.x + area.width) - Math.max(rect.x, area.x);
  const overlapY =
    Math.min(rect.y + rect.height, area.y + area.height) - Math.max(rect.y, area.y);
  return Math.max(0, overlapX) * Math.max(0, overlapY);
}

/// 把 CSS 设计尺寸按应用缩放与目标显示器 DPI 换成整数物理像素。
function physicalWindowSize(width, height, contentScale = 1, monitorScale = 1) {
  const scale = normalizedScale(contentScale) * normalizedScale(monitorScale);
  return {
    width: Math.round(width * scale),
    height: Math.round(height * scale),
  };
}

/// WebView2 偶尔在混合 DPI 环境保留一层额外 zoom，导致原生窗口物理尺寸正确，
/// 但 CSS 视口仍偏小。以当前“物理像素 / CSS 视口”的实测比例反算目标尺寸，
/// 不再假设系统 DPI 与 setZoom 就是完整缩放链。
function viewportCorrectedPhysicalSize({
  currentPhysicalWidth,
  currentPhysicalHeight,
  viewportWidth,
  viewportHeight,
  expectedWidth,
  expectedHeight,
}) {
  const values = [
    currentPhysicalWidth,
    currentPhysicalHeight,
    viewportWidth,
    viewportHeight,
    expectedWidth,
    expectedHeight,
  ];
  if (values.some((value) => !Number.isFinite(value) || value <= 0)) return null;

  const widthRatio = expectedWidth / viewportWidth;
  const heightRatio = expectedHeight / viewportHeight;
  // 防止隐藏/切形态的瞬时 0 尺寸把窗口放大到不可恢复；正常 DPI/zoom
  // 残差都落在 0.5–2 之间（应用自身允许的缩放范围也是 0.75–2）。
  if (
    widthRatio < 0.5 ||
    widthRatio > 2 ||
    heightRatio < 0.5 ||
    heightRatio > 2
  ) {
    return null;
  }
  return {
    width: Math.max(1, Math.round(currentPhysicalWidth * widthRatio)),
    height: Math.max(1, Math.round(currentPhysicalHeight * heightRatio)),
  };
}

function viewportCorrectedZoom({
  contentScale,
  viewportWidth,
  viewportHeight,
  expectedWidth,
  expectedHeight,
}) {
  const values = [
    contentScale,
    viewportWidth,
    viewportHeight,
    expectedWidth,
    expectedHeight,
  ];
  if (values.some((value) => !Number.isFinite(value) || value <= 0)) return null;

  const widthRatio = viewportWidth / expectedWidth;
  const heightRatio = viewportHeight / expectedHeight;
  // 真实 zoom 应在两条轴上给出近似相同的比例；差异过大说明布局/尺寸仍在变化，
  // 此时交给物理窗口兜底，不贸然改变内容缩放。
  if (
    widthRatio < 0.5 ||
    widthRatio > 2 ||
    heightRatio < 0.5 ||
    heightRatio > 2 ||
    Math.abs(widthRatio - heightRatio) > 0.08
  ) {
    return null;
  }
  // 只取长轴的比例，短轴仅用于上面的一致性校验。取平均会把短轴的取整残差
  // 放大成很大的 zoom 变更：横向胶囊条高 40px，视口差 1 物理像素就是 2.5%，
  // 平均之后 zoom 被改 1.25%；zoom 一变布局重排，测出来的目标宽度跟着变，
  // 于是「调窗 → 重排 → 再调窗」闭环，表现为胶囊条持续闪烁。长轴上同样的
  // 1px 只有 0.25%，落在噪声里。竖条不闪是因为它的宽度是常量，环路断开。
  const ratio = expectedWidth >= expectedHeight ? widthRatio : heightRatio;
  return contentScale * ratio;
}

function horizontalStripTargetWidth({
  cellCount,
  cellWidth,
  controlsWidth,
  paddingLeft,
  paddingRight,
  gap,
  roundingAllowance = 1,
}) {
  const cells = Math.max(1, Math.round(cellCount || 0));
  return (
    cells * cellWidth +
    controlsWidth +
    paddingLeft +
    paddingRight +
    cells * gap +
    roundingAllowance
  );
}

/// 竖向胶囊的悬停卡片需要临时放大透明窗口。窗口扩展方向取胶囊所在的
/// 半屏，原胶囊的屏幕坐标保持不动；纵向只移动到足够容纳卡片的位置。
function verticalStripHoverLocalLayout({ targetHeight, anchorY, cardHeight, margin = 8, pointerMargin = 22 }) {
  if ([targetHeight, anchorY, cardHeight, margin, pointerMargin].some((value) => !Number.isFinite(value))) {
    return null;
  }
  const halfCard = cardHeight / 2;
  const cardCenter = Math.min(
    Math.max(anchorY, halfCard + margin),
    Math.max(targetHeight - halfCard - margin, halfCard + margin),
  );
  const pointerY = Math.min(
    Math.max(anchorY - cardCenter + halfCard, pointerMargin),
    Math.max(cardHeight - pointerMargin, pointerMargin),
  );
  return { cardCenter, pointerY };
}

function verticalStripHoverLayout({ railPosition, railSize, workArea, targetSize, anchorY, cardHeight, margin = 8 }) {
  const values = [
    railPosition?.x,
    railPosition?.y,
    railSize?.width,
    railSize?.height,
    workArea?.x,
    workArea?.y,
    workArea?.width,
    workArea?.height,
    targetSize?.width,
    targetSize?.height,
    anchorY,
    cardHeight,
  ];
  if (values.some((value) => !Number.isFinite(value))) return null;

  const workRight = workArea.x + workArea.width;
  const workBottom = workArea.y + workArea.height;
  const railCenterX = railPosition.x + railSize.width / 2;
  const side = railCenterX < workArea.x + workArea.width / 2 ? "left" : "right";
  const local = verticalStripHoverLocalLayout({
    targetHeight: targetSize.height,
    anchorY,
    cardHeight,
    margin,
  });
  if (!local) return null;
  const { cardCenter } = local;
  const desiredX = side === "left"
    ? railPosition.x
    : railPosition.x + railSize.width - targetSize.width;
  const desiredY = railPosition.y - (cardCenter - anchorY);
  const x = Math.min(Math.max(desiredX, workArea.x), Math.max(workRight - targetSize.width, workArea.x));
  // 窗口不仅要留在工作区，也必须完整包住原胶囊。只按卡片锚点移动时，
  // 悬停下方条目会让 railOffsetY 变成负数，把胶囊上半段推出透明窗口。
  const railBottom = railPosition.y + railSize.height;
  const minY = Math.max(workArea.y, railBottom - targetSize.height);
  const maxY = Math.min(workBottom - targetSize.height, railPosition.y);
  const y = Math.min(Math.max(desiredY, minY), Math.max(maxY, minY));

  return {
    side,
    x,
    y,
    cardCenter,
    railOffsetY: railPosition.y - y,
  };
}

/// 记忆坐标是物理像素；用每台显示器自己的 DPI 推导候选窗口大小，再选与工作区
/// 重叠最多的显示器。不能先读当前窗口 DPI——窗口随后可能恢复到另一台屏幕。
function monitorForWindowPosition(
  monitors,
  position,
  logicalSize,
  contentScale = 1,
) {
  if (
    !position ||
    !Number.isFinite(position.x) ||
    !Number.isFinite(position.y) ||
    !logicalSize ||
    !Number.isFinite(logicalSize.width) ||
    !Number.isFinite(logicalSize.height)
  ) {
    return null;
  }

  let best = null;
  let bestOverlap = 0;
  (monitors || []).forEach((monitor) => {
    const area = monitorArea(monitor);
    if (!area) return;
    const physical = physicalWindowSize(
      logicalSize.width,
      logicalSize.height,
      contentScale,
      monitor.scaleFactor,
    );
    const overlap = overlapArea(
      { x: position.x, y: position.y, ...physical },
      area,
    );
    if (overlap > bestOverlap) {
      best = monitor;
      bestOverlap = overlap;
    }
  });
  return best;
}

export {
  horizontalStripTargetWidth,
  isStableFloatingMode,
  monitorForWindowPosition,
  physicalWindowSize,
  verticalStripHoverLocalLayout,
  verticalStripHoverLayout,
  viewportCorrectedPhysicalSize,
  viewportCorrectedZoom,
};

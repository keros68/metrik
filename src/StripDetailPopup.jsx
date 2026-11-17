import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { flushSync } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { GLASS_MODES, glassShellAppearance } from "./glassAppearance.js";
import { StripDetailCard } from "./quotaView.jsx";
import { physicalWindowSize, stripDetailPopupLayout } from "./windowGeometry.js";

const POPUP_CSS_WIDTH = 260;
const POPUP_BASE_CSS_HEIGHT = 360;
const POPUP_CARD_MARGIN = 10;

export function StripDetailPopup() {
  const [payload, setPayload] = useState(null);
  const shellRef = useRef(null);
  const cardRef = useRef(null);
  const appliedZoomRef = useRef(null);
  const receivedEventRef = useRef(false);

  useEffect(() => {
    let active = true;
    invoke("warm_strip_detail").catch(() => {});
    const unlistenPromise = listen("metrik://strip-detail", (event) => {
      if (!active) return;
      receivedEventRef.current = true;
      flushSync(() => {
        setPayload(event.payload || null);
      });
    });
    invoke("get_strip_detail_state")
      .then((initial) => {
        if (!active || receivedEventRef.current || !initial) return;
        flushSync(() => {
          setPayload(initial);
        });
      })
      .catch(() => {});
    return () => {
      active = false;
      unlistenPromise.then((unlisten) => unlisten?.()).catch(() => {});
    };
  }, []);

  const rawAlpha = Number.isFinite(payload?.glassAlpha) ? payload.glassAlpha : 0.82;
  const glassTint = payload?.glassTint || "dark";
  const glassInk = payload?.glassInk || "light";
  const shellGlassAlpha = Number.isFinite(payload?.shellGlassAlpha)
    ? payload.shellGlassAlpha
    : glassTint === "clear"
      ? rawAlpha
      : 0.55 + Math.min(1, Math.max(0, (rawAlpha - 0.05) / (0.96 - 0.05))) * (0.96 - 0.55);

  useLayoutEffect(() => {
    document.documentElement.style.setProperty("--shell-glass-alpha", String(shellGlassAlpha));
    return () => {
      document.documentElement.style.removeProperty("--shell-glass-alpha");
    };
  }, [shellGlassAlpha]);

  useLayoutEffect(() => {
    if (!payload || !cardRef.current || !shellRef.current) return undefined;
    let cancelled = false;
    const zoom = Number.isFinite(payload.stripScale) && payload.stripScale > 0
      ? payload.stripScale
      : 1;
    const factor = Number.isFinite(payload.scaleFactor) && payload.scaleFactor > 0
      ? payload.scaleFactor
      : 1;
    const cardCssHeight = Math.ceil(cardRef.current.getBoundingClientRect().height);
    const popupCssHeight = Math.max(POPUP_BASE_CSS_HEIGHT, cardCssHeight + POPUP_CARD_MARGIN * 2);
    const combinedScale = zoom * factor;
    const popupPhysical = physicalWindowSize(POPUP_CSS_WIDTH, popupCssHeight, zoom, factor);
    const layout = stripDetailPopupLayout({
      railPosition: payload.railPosition,
      railSize: payload.railSize,
      workArea: payload.workArea,
      popupSize: popupPhysical,
      cardHeight: Math.round(cardCssHeight * combinedScale),
      anchorY: payload.anchorY * combinedScale,
      margin: Math.round(POPUP_CARD_MARGIN * combinedScale),
      pointerMargin: Math.round(22 * combinedScale),
    });
    if (!layout) return undefined;

    const shell = shellRef.current;
    shell.dataset.side = layout.side;
    shell.classList.toggle("strip-shell--detail-left", layout.side === "left");
    shell.classList.toggle("strip-shell--detail-right", layout.side === "right");
    shell.style.setProperty("--strip-detail-card-center-y", `${layout.cardCenter / combinedScale}px`);
    shell.style.setProperty("--strip-detail-pointer-y", `${layout.pointerY / combinedScale}px`);

    let frameId = null;
    let fallbackTimer = null;
    let presented = false;
    const commitPresent = () => {
      if (cancelled || presented) return;
      presented = true;
      if (frameId !== null) window.cancelAnimationFrame(frameId);
      if (fallbackTimer !== null) window.clearTimeout(fallbackTimer);
      invoke("present_strip_detail", {
        generation: payload.generation,
        x: layout.x,
        y: layout.y,
        width: popupPhysical.width,
        height: popupPhysical.height,
      }).catch(() => {});
    };
    const schedulePresent = () => {
      if (cancelled) return;
      frameId = window.requestAnimationFrame(() => {
        if (cancelled) return;
        commitPresent();
      });
      fallbackTimer = window.setTimeout(commitPresent, 32);
    };

    if (appliedZoomRef.current !== zoom) {
      appliedZoomRef.current = zoom;
      getCurrentWebview().setZoom(zoom).catch(() => {}).finally(schedulePresent);
    } else {
      schedulePresent();
    }

    return () => {
      cancelled = true;
      if (frameId !== null) window.cancelAnimationFrame(frameId);
      if (fallbackTimer !== null) window.clearTimeout(fallbackTimer);
    };
  }, [payload]);

  const shellAppearance = glassShellAppearance("strip", {
    glassMode: GLASS_MODES.alpha,
    glassTint,
    glassInk,
    glassAlpha: shellGlassAlpha,
    isMac: false,
    vertical: true,
  });

  return (
    <main
      ref={shellRef}
      className={`${shellAppearance.className} strip-shell--detail-open strip-shell--detail-right strip-detail-popup`}
      style={shellAppearance.style}
    >
      {payload && (
        <StripDetailCard
          agentId={payload.agentId}
          cell={payload.cell}
          cardRef={cardRef}
        />
      )}
    </main>
  );
}

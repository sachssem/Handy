import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import "./LearnedToast.css";
import { commands, events } from "@/bindings";
import type { LearnedCorrectionEvent } from "@/bindings";
import i18n, { syncLanguageFromSettings } from "@/i18n";
import { getLanguageDirection } from "@/lib/utils/rtl";

// How long the toast stays fully visible before it auto-dismisses, and how long
// the exit animation runs before the OS window is ordered out. Keep EXIT_MS in
// sync with the `.lt-card.leaving` transition in LearnedToast.css.
const VISIBLE_MS = 5000;
const EXIT_MS = 200;

/**
 * The "learned X → Y" toast (fork feature: voice-control). Lives in its own
 * always-on-top window; listens for the backend `learnedCorrectionEvent`,
 * shows a compact pill with an Undo button, and auto-dismisses after 5 s. The
 * window is positioned and revealed from Rust (see correction_learning::toast);
 * this component owns only the content and its lifecycle.
 */
const LearnedToast: React.FC = () => {
  const { t } = useTranslation();
  const [content, setContent] = useState<LearnedCorrectionEvent | null>(null);
  const [isVisible, setIsVisible] = useState(false);
  const direction = getLanguageDirection(i18n.language);

  // Refs, because the event listener is registered once and would otherwise
  // close over stale state / handlers.
  const visibleRef = useRef(false);
  const hideTimer = useRef<number | null>(null);
  const exitTimer = useRef<number | null>(null);

  const setVisible = (value: boolean) => {
    visibleRef.current = value;
    setIsVisible(value);
  };

  const clearTimers = () => {
    if (hideTimer.current !== null) {
      clearTimeout(hideTimer.current);
      hideTimer.current = null;
    }
    if (exitTimer.current !== null) {
      clearTimeout(exitTimer.current);
      exitTimer.current = null;
    }
  };

  // Play the exit animation, then order the OS window out so it stops
  // intercepting clicks in the bottom-center region.
  const dismiss = () => {
    clearTimers();
    setVisible(false);
    exitTimer.current = window.setTimeout(() => {
      void commands.hideLearnedToast();
    }, EXIT_MS);
  };

  useEffect(() => {
    let unlisten: (() => void) | undefined;

    // Breadcrumb into the Rust log — this window has no visible dev console,
    // so the display chain is only debuggable through these stages.
    void commands.toastStage("mount");

    const showCorrection = async (payload: LearnedCorrectionEvent) => {
      void commands.toastStage(
        `show: trial=${payload.trial} extra=${payload.extra}`,
      );
      await syncLanguageFromSettings();
      clearTimers();
      setContent(payload);
      // Visible synchronously, in the same JS turn as the content: WebKit
      // suspends this panel webview (rAF *and* timers) shortly after creation
      // while it deems the window occluded, so any deferred visibility flip
      // freezes until something reactivates the app — the toast then appeared
      // minutes late. The entrance transition is sacrificed; the first paint
      // already shows the card.
      void commands.toastStage("visible-sync");
      setVisible(true);
      hideTimer.current = window.setTimeout(dismiss, VISIBLE_MS);
    };

    events.learnedCorrectionEvent
      .listen((event) => showCorrection(event.payload))
      .then((fn) => {
        unlisten = fn;
      });

    // The window is normally created at app startup, but a correction can still
    // land before this listener is ready (safety-net lazy creation, or a commit
    // racing the mount). Pick up anything the backend stashed for that case.
    void commands.takePendingLearnedToast().then((pending) => {
      void commands.toastStage(`pending: ${pending ? "some" : "none"}`);
      if (pending) {
        void showCorrection(pending);
      }
    });

    return () => {
      unlisten?.();
      clearTimers();
    };
  }, []);

  const handleUndo = async () => {
    if (content) {
      try {
        const result = await commands.removeLearnedCorrection(content.id);
        if (result.status === "error") {
          console.error("Failed to undo learned correction:", result.error);
        }
      } catch (error) {
        console.error("Failed to undo learned correction:", error);
      }
    }
    dismiss();
  };

  if (!content) return null;

  return (
    <div
      dir={direction}
      className="lt-stage"
      // Single source of truth for the exit duration: the CSS `.leaving`
      // transition reads this, so it always matches EXIT_MS.
      style={{ "--lt-exit-ms": `${EXIT_MS}ms` } as React.CSSProperties}
    >
      <div className={`lt-card ${isVisible ? "show" : "leaving"}`}>
        <span className="lt-badge" aria-hidden="true">
          <svg viewBox="0 0 16 16">
            <path
              d="M3.5 8.5 L6.5 11.5 L12.5 5"
              stroke="currentColor"
              strokeWidth="1.8"
              strokeLinecap="round"
              strokeLinejoin="round"
              fill="none"
            />
          </svg>
        </span>
        <span className="lt-title">
          {t(content.trial ? "learnedToast.trialTitle" : "learnedToast.title", {
            misheard: content.misheard,
            intended: content.intended,
          })}
        </span>
        {content.extra > 0 && (
          // Several corrections settled together; the toast shows the first pair
          // and summarises the rest as "+N more".
          <span className="lt-more">
            {t("learnedToast.more", { count: content.extra })}
          </span>
        )}
        {/* Dry-run soak: nothing was persisted, so there is nothing to undo —
            the title's "Dry-Run:" prefix already says so, no extra note. */}
        {!content.trial && (
          <button className="lt-undo" onClick={handleUndo}>
            {t("learnedToast.undo")}
          </button>
        )}
      </div>
    </div>
  );
};

export default LearnedToast;

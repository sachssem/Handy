import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { listen } from "@tauri-apps/api/event";
import "./LearnedToast.css";
import { formatKeyHint } from "./keyHint";
import { commands, events } from "@/bindings";
import type { LearnedCorrectionEvent } from "@/bindings";
import i18n, { syncLanguageFromSettings } from "@/i18n";
import { getLanguageDirection } from "@/lib/utils/rtl";

// How long the toast stays fully visible before it auto-dismisses (a suggestion
// asks for a decision, so it lingers longer), and how long the exit animation
// runs before the OS window is ordered out. EXIT_MS drives the `.lt-card.leaving`
// transition in LearnedToast.css via `--lt-exit-ms`.
const VISIBLE_MS = 5000;
const SUGGESTION_VISIBLE_MS = 8000;
const EXIT_MS = 200;

// Emitted by correction_learning::toast after a keyboard shortcut already
// acted on the toast; the payload is the toast's first-pair id.
const SHORTCUT_DISMISS_EVENT = "learned-toast-shortcut-dismiss";

interface KeyHints {
  accept: string;
  dismiss: string;
}

/**
 * The learned-correction toast (fork feature: voice-control). Lives in its own
 * always-on-top window and listens for the backend `learnedCorrectionEvent`:
 * a new suggestion shows "Suggestion: X → Y" with Accept / Never, a pair that
 * became active shows "Learned: X → Y" with Undo. A group mixing both offers
 * all three, each acting only on its own pairs. Auto-dismisses either way.
 * The window is positioned and revealed from Rust (see
 * correction_learning::toast); this component owns only the content and its
 * lifecycle. The buttons show their keyboard shortcuts (registered by Rust
 * only while the toast is visible); a shortcut acts in Rust and then asks this
 * component to play the exit animation.
 */
const LearnedToast: React.FC = () => {
  const { t } = useTranslation();
  const [content, setContent] = useState<LearnedCorrectionEvent | null>(null);
  const [isVisible, setIsVisible] = useState(false);
  const [hints, setHints] = useState<KeyHints | null>(null);
  const direction = getLanguageDirection(i18n.language);

  // Refs, because the event listener is registered once and would otherwise
  // close over stale state / handlers.
  const visibleRef = useRef(false);
  const contentRef = useRef<LearnedCorrectionEvent | null>(null);
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
    // Bound to this toast so a late hide can't close a newer one.
    const id = contentRef.current?.id ?? null;
    exitTimer.current = window.setTimeout(() => {
      void commands.hideLearnedToast(id);
    }, EXIT_MS);
  };

  useEffect(() => {
    // Listeners register asynchronously; under StrictMode the effect is torn
    // down before they resolve, so a late one is released right away.
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    let unlistenShortcut: (() => void) | undefined;

    // Breadcrumb into the Rust log — this window has no visible dev console,
    // so the display chain is only debuggable through these stages.
    void commands.toastStage("mount");

    // Read on every show, so a shortcut changed in settings is reflected.
    const loadHints = async () => {
      try {
        const result = await commands.getAppSettings();
        if (result.status === "ok") {
          setHints({
            accept: formatKeyHint(
              result.data.learned_toast_accept_shortcut ?? "",
            ),
            dismiss: formatKeyHint(
              result.data.learned_toast_dismiss_shortcut ?? "",
            ),
          });
        }
      } catch (error) {
        console.error("Failed to load toast shortcuts:", error);
      }
    };

    const showCorrection = async (payload: LearnedCorrectionEvent) => {
      // First, before any await: a previous toast's pending exit timer would
      // otherwise order the window out right after this one appears.
      clearTimers();
      void commands.toastStage(
        `show: status=${payload.status} extra=${payload.extra}`,
      );
      // Hints and language before the card appears, so its first paint already
      // carries the key hints in the right language.
      await Promise.all([loadHints(), syncLanguageFromSettings()]);
      contentRef.current = payload;
      setContent(payload);
      // Visible synchronously, in the same JS turn as the content: WebKit
      // suspends this panel webview (rAF *and* timers) shortly after creation
      // while it deems the window occluded, so any deferred visibility flip
      // freezes until something reactivates the app — the toast then appeared
      // minutes late. The entrance transition is sacrificed; the first paint
      // already shows the card.
      void commands.toastStage("visible-sync");
      setVisible(true);
      hideTimer.current = window.setTimeout(
        dismiss,
        payload.status === "suggested" ? SUGGESTION_VISIBLE_MS : VISIBLE_MS,
      );
    };

    events.learnedCorrectionEvent
      .listen((event) => showCorrection(event.payload))
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      });

    // A keyboard shortcut already accepted / rejected in Rust: just leave.
    // Scoped to the toast it acted on, so it never closes a newer one.
    void listen<string>(SHORTCUT_DISMISS_EVENT, (event) => {
      if (visibleRef.current && contentRef.current?.id === event.payload) {
        dismiss();
      }
    }).then((fn) => {
      if (cancelled) fn();
      else unlistenShortcut = fn;
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
      cancelled = true;
      unlisten?.();
      unlistenShortcut?.();
      clearTimers();
    };
  }, []);

  // Undo (the promoted pairs) and Never (the suggestions) reject their own
  // pairs only: removed and blocked from being learned again.
  const reject = async (ids: string[]) => {
    if (ids.length > 0) {
      try {
        const result = await commands.rejectLearnedCorrections(ids);
        if (result.status === "error") {
          console.error("Failed to reject learned correction:", result.error);
        }
      } catch (error) {
        console.error("Failed to reject learned correction:", error);
      }
    }
    dismiss();
  };

  const handleAccept = async () => {
    if (content) {
      try {
        const results = await Promise.all(
          content.suggested_ids.map((id) =>
            commands.acceptLearnedCorrection(id),
          ),
        );
        for (const result of results) {
          if (result.status === "error") {
            console.error("Failed to accept suggestion:", result.error);
          }
        }
      } catch (error) {
        console.error("Failed to accept suggestion:", error);
      }
    }
    dismiss();
  };

  if (!content) return null;

  const isSuggestion = content.status === "suggested";
  const hasSuggestions = content.suggested_ids.length > 0;
  const hasLearned = content.active_ids.length > 0;

  // Renders a button's shortcut hint; nothing while the hints are loading.
  const keyHint = (hint: string | undefined) =>
    hint ? (
      <kbd className="lt-kbd" aria-hidden="true">
        {hint}
      </kbd>
    ) : null;

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
          {isSuggestion ? (
            // Sparkle: a proposal, not yet applied.
            <svg viewBox="0 0 16 16">
              <path
                d="M8 2.5 L9.2 6.8 L13.5 8 L9.2 9.2 L8 13.5 L6.8 9.2 L2.5 8 L6.8 6.8 Z"
                fill="currentColor"
              />
            </svg>
          ) : (
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
          )}
        </span>
        <span className="lt-title">
          {t(
            isSuggestion
              ? "learnedToast.suggestionTitle"
              : "learnedToast.title",
            {
              misheard: content.misheard,
              intended: content.intended,
            },
          )}
        </span>
        {content.extra > 0 && (
          // Several corrections settled together; the toast shows the first pair
          // and summarises the rest as "+N more".
          <span className="lt-more">
            {t("learnedToast.more", { count: content.extra })}
          </span>
        )}
        {hasSuggestions && (
          <>
            <button className="lt-action lt-primary" onClick={handleAccept}>
              {t("learnedToast.accept")}
              {keyHint(hints?.accept)}
            </button>
            <button
              className="lt-action"
              onClick={() => reject(content.suggested_ids)}
            >
              {t("learnedToast.dismiss")}
              {keyHint(hints?.dismiss)}
            </button>
          </>
        )}
        {hasLearned && (
          // The dismiss shortcut is Never when there are suggestions, Undo
          // otherwise (mirrors correction_learning::toast_shortcuts::action_for),
          // so Undo shows its hint only without suggestions.
          <button
            className={`lt-action${hasSuggestions ? "" : " lt-primary"}`}
            onClick={() => reject(content.active_ids)}
          >
            {t("learnedToast.undo")}
            {!hasSuggestions && keyHint(hints?.dismiss)}
          </button>
        )}
      </div>
    </div>
  );
};

export default LearnedToast;

import { listen } from "@tauri-apps/api/event";
import React, { useEffect, useLayoutEffect, useRef, useState } from "react";
import { flushSync } from "react-dom";
import { useTranslation } from "react-i18next";
import "./RecordingOverlay.css";
import { commands, events } from "@/bindings";
import type {
  Notice,
  StreamPhase,
  StreamPhaseEvent,
  StreamTextEvent,
  StreamWorkKind,
} from "@/bindings";
import { NoticeBadgeIcon } from "@/components/icons/NoticeBadgeIcon";
import i18n, { syncLanguageFromSettings } from "@/i18n";
import { noticeDisplay } from "@/lib/notices";
import type { ModelStateEvent } from "@/lib/types/events";
import { getLanguageDirection } from "@/lib/utils/rtl";

type OverlayState =
  | "recording"
  | "streaming"
  | "transcribing"
  | "processing"
  | "notice"
  | "live_notice";

const isNoticeState = (state: OverlayState) =>
  state === "notice" || state === "live_notice";

// fork(voice-control): model-specific auto-stop deadline and warning window.
type RecordingLimitPayload = {
  deadline_epoch_ms: number;
  limit_ms: number;
  warning_ms: number;
};

type RecordingLimitState = {
  deadlineMs: number;
  limitMs: number;
  warningMs: number;
};

// Number of reactive bars in the waveform (the simple, smoothed style shared by
// every overlay form). Mic levels arrive as 16 FFT buckets; we take the first N.
// fork(voice-control): 13 bars for the compact 160px capsule (upstream 9).
const WAVE_BARS = 13;

// fork(voice-control): a long transcription's progress, sent when a chunk's
// decode starts: the decoded share, the share once that chunk is done, and how
// long the chunk probably takes.
type ProgressPayload = { from: number; to: number; eta_ms: number };
type Meter = { from: number; to: number; etaMs: number; at: number };
// fork(voice-control): bars of the small wave that stands for an earlier
// dictation still being transcribed while a new recording runs.
const PRIOR_BARS = 4;

// fork(voice-control): overlay latency breadcrumbs (debug log level + the
// dictation journal's `overlay` events). The overlay webview has no visible
// console, so it reports through the journal's journal_overlay_stage command.
// Epoch ms lets the lines be lined up with the backend's `overlay '<state>': …
// epoch_ms=` show line and the `TranscribeAction::start … epoch_ms=` press line
// (the log's own timestamps only carry seconds).
const overlayStage = (stage: string) => {
  commands.journalOverlayStage(`overlay: ${stage}`).catch(() => {});
};

// fork(voice-control): language + placement are read on mount and refreshed in
// the background whenever the overlay appears — never awaited before the first
// paint (two settings IPCs used to delay the pill by up to seconds).
const readOverlayPosition = async (): Promise<"top" | "bottom" | null> => {
  try {
    const settings = await commands.getAppSettings();
    if (settings.status === "ok") {
      return settings.data.overlay_position === "top" ? "top" : "bottom";
    }
  } catch {
    // Keep the previous/default placement if settings can't be read.
  }
  return null;
};

// Only call out a model load in the Live preview once it has run this long.
// Warm loads finish in well under this (~0.2s on Apple Silicon, ~1.5s on a
// Windows CPU backend), so the common case never flashes a loading notice while
// the user is speaking; cold loads can take 40s+.
const SLOW_MODEL_LOAD_MS = 2000;

// Matches the delay in `hide_recording_overlay`, so a notice can fade out.
const OVERLAY_FADE_MS = 300;

// 13px padding on each side, the 16px badge and its 7px gap.
const NOTICE_PILL_CHROME = 49;

const RecordingOverlay: React.FC = () => {
  const { t } = useTranslation();
  const [isVisible, setIsVisible] = useState(false);
  const [state, setState] = useState<OverlayState>("recording");
  // `Stream::play()` returning does not mean hardware callbacks are flowing.
  // Stay visually in an arming state until the backend processes the first
  // actual microphone sample chunk.
  const [captureReady, setCaptureReady] = useState(false);
  // Recording starts while the model loads in the background. Say so, otherwise
  // a cold start looks like an empty Live preview that ignores speech, then a
  // hung "Transcribing..." spinner. Once recording stops, any in-flight load is
  // the wait, so the working label reflects it immediately; the Live notice
  // waits for the load to be slow so fast loads don't flash it mid-speech.
  const [modelLoading, setModelLoading] = useState(false);
  const [modelLoadSlow, setModelLoadSlow] = useState(false);
  const modelLoadTimerRef = useRef<ReturnType<typeof setTimeout>>();
  // Latched per Live session: once the loading notice has opened the panel, keep
  // it open after the load so it doesn't collapse and reopen when text arrives.
  const [loadNoticeShown, setLoadNoticeShown] = useState(false);
  const [levels, setLevels] = useState<number[]>(Array(WAVE_BARS).fill(0));
  const [streamText, setStreamText] = useState<StreamTextEvent>({
    committed: "",
    tentative: "",
  });
  const [phase, setPhase] = useState<StreamPhase>("listening");
  const [workKind, setWorkKind] = useState<StreamWorkKind>("transcribing");
  // fork(voice-control): countdown and fallback state for the active recording.
  const [recordingLimit, setRecordingLimit] =
    useState<RecordingLimitState | null>(null);
  // fork(voice-control): true while the language-allowlist guard re-runs the
  // audio with the configured fallback model, so the work label says why the
  // transcription takes a second pass.
  const [fallbackActive, setFallbackActive] = useState(false);
  // fork(voice-control): the chunk a long transcription is decoding; null
  // while there is no progress to show. `litBars` is how many of the
  // WAVE_BARS bars it lights.
  const [meter, setMeter] = useState<Meter | null>(null);
  const [litBars, setLitBars] = useState(WAVE_BARS);
  // fork(voice-control): dictations that are stopped and not yet pasted. While
  // one is open, a new recording shows it as a small wave beside its bars.
  const [backlog, setBacklog] = useState(0);
  const backlogRef = useRef(0);
  const [nowMs, setNowMs] = useState(() => Date.now());
  // Bumped on each new streaming session so the Live card remounts fresh (replays
  // the pop-in, and never animates in from the previous panel's open size).
  const [session, setSession] = useState(0);
  // Overlay placement (top vs bottom of the screen). The Live panel grows downward
  // from a top overlay (oldest line under the pill) and upward from a bottom one.
  const [position, setPosition] = useState<"top" | "bottom">("bottom");
  // True once live text overflows the cap. A top overlay fades its top edge only
  // while overflowing, so the resting first line stays crisp flush under the pill.
  const [overflowing, setOverflowing] = useState(false);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [noticeWidth, setNoticeWidth] = useState<number>();
  const noticeProbeRef = useRef<HTMLSpanElement>(null);
  const fadeTimerRef = useRef<ReturnType<typeof setTimeout>>();

  // fork(voice-control): bumped on each hidden -> visible transition so the
  // compact capsule remounts and replays its (80 ms) appear animation; a state
  // change while already visible (recording -> transcribing) must not replay it.
  const [showSeq, setShowSeq] = useState(0);
  const visibleRef = useRef(false);

  const smoothedLevelsRef = useRef<number[]>(Array(16).fill(0));
  // Live-text scroll-back: the text region "sticks" to the newest line while the
  // user is at the bottom; if they scroll up to read history, auto-follow pauses
  // until they scroll back down.
  const capRef = useRef<HTMLDivElement>(null);
  const pinnedRef = useRef(true);
  const direction = getLanguageDirection(i18n.language);

  useEffect(() => {
    // fork(voice-control): settings sync off the show path (see
    // readOverlayPosition). The Live panel flows downward from a top overlay
    // and upward from a bottom one, so the placement flips the layout.
    const syncOverlaySettings = () => {
      syncLanguageFromSettings();
      readOverlayPosition().then((p) => {
        if (p) setPosition(p);
      });
    };
    syncOverlaySettings();

    const setupEventListeners = async () => {
      const unlistenNotice = await listen<Notice>("overlay-notice", (event) => {
        clearTimeout(fadeTimerRef.current);
        setNotice(event.payload);
      });

      const unlistenShow = await listen("show-overlay", (event) => {
        const overlayState = event.payload as OverlayState;
        const handlerAt = performance.now();
        const handlerEpoch = Date.now();
        const appearing = !visibleRef.current;
        visibleRef.current = true;
        // fork(voice-control): everything visible is committed synchronously —
        // no awaits before the paint. flushSync renders now instead of on the
        // scheduler's next task.
        flushSync(() => {
          clearTimeout(fadeTimerRef.current);
          if (!isNoticeState(overlayState)) setNotice(null);
          if (overlayState === "recording" || overlayState === "streaming") {
            setCaptureReady(false);
            smoothedLevelsRef.current = Array(16).fill(0);
            setLevels(Array(WAVE_BARS).fill(0));
            setStreamText({ committed: "", tentative: "" });
            // fork(voice-control): clear the previous session's recording limit.
            setRecordingLimit(null);
            setFallbackActive(false);
            setNowMs(Date.now());
          }
          // fork(voice-control): the progress belongs to the transcription
          // alone; recording and post-processing show the plain bars. A
          // recording over a running transcription keeps that one's progress.
          if (overlayState !== "transcribing" && backlogRef.current === 0) {
            setMeter(null);
          }
          setState(overlayState);
          if (overlayState === "streaming") {
            setPhase("listening");
            setWorkKind("transcribing");
            setLoadNoticeShown(false);
            setSession((s) => s + 1); // remount the card fresh for this session
          }
          if (appearing) setShowSeq((n) => n + 1);
          setIsVisible(true);
        });
        // fork(voice-control): latency breadcrumbs — handler entry, then the
        // first frame (rAF fires before the paint; the task queued from it runs
        // after the frame has been produced).
        overlayStage(
          `show '${overlayState}' handler epoch_ms=${handlerEpoch} render=${(performance.now() - handlerAt).toFixed(1)}ms`,
        );
        requestAnimationFrame(() => {
          setTimeout(() => {
            overlayStage(
              `show '${overlayState}' first-frame epoch_ms=${Date.now()} +${(performance.now() - handlerAt).toFixed(1)}ms after handler`,
            );
          }, 0);
        });
        if (appearing) syncOverlaySettings();
      });

      const unlistenHide = await listen("hide-overlay", () => {
        visibleRef.current = false;
        setIsVisible(false);
        setCaptureReady(false);
        // A notice keeps rendering while it fades. Dropping the transcript here
        // stops a later notice from showing an old session's text.
        clearTimeout(fadeTimerRef.current);
        fadeTimerRef.current = setTimeout(() => {
          setNotice(null);
          setStreamText({ committed: "", tentative: "" });
        }, OVERLAY_FADE_MS);
        // fork(voice-control): clear the limit and fallback at the end of a session.
        setRecordingLimit(null);
        setFallbackActive(false);
        setMeter(null);
      });

      const unlistenReady = await listen("recording-ready", () => {
        setCaptureReady(true);
      });

      // fork(voice-control): show the model-specific recording limit warning.
      const unlistenRecordingLimit = await listen<RecordingLimitPayload>(
        "recording-limit",
        (event) => {
          const payload = event.payload;
          setRecordingLimit({
            deadlineMs: payload.deadline_epoch_ms,
            limitMs: payload.limit_ms,
            warningMs: payload.warning_ms,
          });
          setNowMs(Date.now());
        },
      );

      // fork(voice-control): the allowlist guard announces its fallback-model
      // pass so the extra latency is visibly explained.
      const unlistenFallback = await listen("fallback-transcription", () => {
        setFallbackActive(true);
      });

      // fork(voice-control): a long transcription reports how far it is.
      const unlistenProgress = await listen<ProgressPayload>(
        "transcription-progress",
        (event) =>
          setMeter({
            from: event.payload.from,
            to: event.payload.to,
            etaMs: event.payload.eta_ms,
            at: performance.now(),
          }),
      );

      // fork(voice-control): how many dictations are still being finished.
      const unlistenBacklog = await listen<number>(
        "dictation-backlog",
        (event) => {
          backlogRef.current = event.payload;
          setBacklog(event.payload);
        },
      );

      const unlistenLevel = await listen<number[]>("mic-level", (event) => {
        const newLevels = event.payload as number[];
        // Exponential smoothing across the 16 buckets, then take the first N
        // bars for the shared waveform.
        const smoothed = smoothedLevelsRef.current.map((prev, i) => {
          const target = newLevels[i] || 0;
          return prev * 0.7 + target * 0.3;
        });
        smoothedLevelsRef.current = smoothed;
        setLevels(smoothed.slice(0, WAVE_BARS));
      });

      const unlistenStream = await events.streamTextEvent.listen((event) => {
        setStreamText(event.payload);
      });

      const unlistenPhase = await events.streamPhaseEvent.listen((event) => {
        const payload: StreamPhaseEvent = event.payload;
        setPhase(payload.phase);
        if (payload.kind) setWorkKind(payload.kind);
      });

      // The backend ends every `loading_started` with exactly one of completed
      // or failed, so only those end a load. Other events (e.g. `unloaded` from
      // the idle watcher) aren't ordered against an in-flight load and must not
      // clear it.
      const unlistenModel = await listen<ModelStateEvent>(
        "model-state-changed",
        (event) => {
          const type = event.payload.event_type;
          if (type === "loading_started") {
            clearTimeout(modelLoadTimerRef.current);
            setModelLoading(true);
            setModelLoadSlow(false);
            modelLoadTimerRef.current = setTimeout(
              () => setModelLoadSlow(true),
              SLOW_MODEL_LOAD_MS,
            );
          } else if (
            type === "loading_completed" ||
            type === "loading_failed"
          ) {
            clearTimeout(modelLoadTimerRef.current);
            setModelLoading(false);
            setModelLoadSlow(false);
          }
        },
      );

      return () => {
        unlistenNotice();
        unlistenShow();
        unlistenHide();
        unlistenReady();
        unlistenRecordingLimit();
        unlistenFallback();
        unlistenProgress();
        unlistenBacklog();
        unlistenLevel();
        unlistenStream();
        unlistenPhase();
        unlistenModel();
        clearTimeout(modelLoadTimerRef.current);
        clearTimeout(fadeTimerRef.current);
      };
    };

    setupEventListeners();
  }, []);

  // fork(voice-control): update only while a model-specific limit is active. The indicator itself is
  // still hidden until the final warning window.
  useEffect(() => {
    if (!recordingLimit || !isVisible) return;
    const id = setInterval(() => setNowMs(Date.now()), 250);
    return () => clearInterval(id);
  }, [recordingLimit, isVisible]);

  // fork(voice-control): the meter runs through the chunk in work over its
  // estimated time and then holds at the chunk's end until the next one starts.
  useEffect(() => {
    if (!meter) {
      setLitBars(WAVE_BARS);
      return;
    }
    const tick = () => {
      const elapsed = performance.now() - meter.at;
      const done = meter.etaMs > 0 ? Math.min(1, elapsed / meter.etaMs) : 1;
      const share = meter.from + (meter.to - meter.from) * done;
      // The first bar is lit from the start, so there is always motion.
      setLitBars(
        Math.min(WAVE_BARS, Math.max(1, Math.ceil(share * WAVE_BARS))),
      );
    };
    tick();
    const id = setInterval(tick, 80);
    return () => clearInterval(id);
  }, [meter]);

  // fork(voice-control): the earlier dictation is pasted while the new
  // recording still runs; its progress ends with it.
  useEffect(() => {
    if (backlog === 0 && state === "recording") setMeter(null);
  }, [backlog, state]);

  // Stick to the bottom as text streams in — but only while pinned, so a user who
  // has scrolled up to read history isn't yanked back down by the next chunk.
  useLayoutEffect(() => {
    const el = capRef.current;
    if (!el) return;
    // Fade the top edge only once text actually overflows the cap.
    setOverflowing(el.scrollHeight > el.clientHeight + 1);
    if (pinnedRef.current) el.scrollTop = el.scrollHeight;
  }, [streamText]);

  // `session` re-runs this when a new Live session starts mid-load (e.g. a
  // retry during a cold load), since the reset doesn't change the other deps.
  useEffect(() => {
    if (modelLoadSlow && state === "streaming") setLoadNoticeShown(true);
  }, [modelLoadSlow, state, session]);

  // Each fresh streaming session starts pinned to the bottom, fade cleared.
  useEffect(() => {
    pinnedRef.current = true;
    setOverflowing(false);
  }, [session]);

  const shownNotice = notice && isNoticeState(state) ? notice : null;
  const noticeTitle = shownNotice
    ? t(noticeDisplay(shownNotice.code).title)
    : "";

  // CSS can't animate to an intrinsic width, so a notice pill gets a measured one.
  useLayoutEffect(() => {
    const probe = noticeProbeRef.current;
    if (!probe) return;
    setNoticeWidth(
      Math.ceil(probe.getBoundingClientRect().width) + NOTICE_PILL_CHROME,
    );
  }, [noticeTitle, isVisible]);

  // Re-pin when the user is within ~a line of the bottom; unpin otherwise.
  const handleStreamScroll = () => {
    const el = capRef.current;
    if (!el) return;
    pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight <= 16;
  };

  const transcribingLabel = modelLoading
    ? t("overlay.loadingModel")
    : t("overlay.transcribing");

  // ---- Shared building blocks (one visual language for every overlay form) ----
  // fork(voice-control): final-window countdown ring in both overlay forms.
  const remainingLimitMs = recordingLimit
    ? Math.max(0, recordingLimit.deadlineMs - nowMs)
    : null;
  const limitWarningVisible =
    recordingLimit !== null &&
    remainingLimitMs !== null &&
    remainingLimitMs <= recordingLimit.warningMs;
  const limitWarningProgress =
    recordingLimit && remainingLimitMs !== null
      ? Math.max(0, Math.min(1, remainingLimitMs / recordingLimit.warningMs))
      : 0;
  const remainingLimitSeconds =
    remainingLimitMs === null
      ? 0
      : Math.max(0, Math.ceil(remainingLimitMs / 1000));
  const limitRingRadius = 10;
  const limitRingCircumference = 2 * Math.PI * limitRingRadius;

  const limitWarningIndicator = limitWarningVisible ? (
    <span
      className="slimit"
      aria-label={t("overlay.recordingLimitWarning", {
        seconds: remainingLimitSeconds,
      })}
    >
      <svg className="slimit-ring" viewBox="0 0 24 24" aria-hidden="true">
        <circle className="slimit-track" cx="12" cy="12" r={limitRingRadius} />
        <circle
          className="slimit-progress"
          cx="12"
          cy="12"
          r={limitRingRadius}
          strokeDasharray={limitRingCircumference}
          strokeDashoffset={limitRingCircumference * (1 - limitWarningProgress)}
        />
      </svg>
      <span className="slimit-value">{remainingLimitSeconds}</span>
    </span>
  ) : null;

  // fork(voice-control): each bar is two round caps + a body (`<b>`), moved
  // by transform only so the caps stay round at every level; the bar's level
  // (--l, 0..1) drives them, the min/max height in px come in as unitless vars.
  // `settled` = transcribing/processing: the bars glide low and a wave travels
  // across them (staggered per bar via --i); --c (distance from the center bar)
  // lets the outer bars trail the center slightly while recording.
  // With a `meter` (a long transcription) the bars double as its progress: the
  // wave only travels across the share of bars that is decoded or in work, the
  // rest wait pale.
  // `bars` below WAVE_BARS draws the small wave of an earlier dictation
  // (always settled); its lit share is scaled from `litBars`.
  const waveformFor = (
    settled: boolean,
    minPx: number,
    maxPx: number,
    bars = WAVE_BARS,
  ) => {
    const lit = Math.max(1, Math.ceil((litBars / WAVE_BARS) * bars));
    return (
      <div
        className={`swave ${settled ? "settled" : captureReady ? "ready" : "arming"} ${
          settled && meter !== null ? "metered" : ""
        }`}
        style={
          {
            "--bmin": minPx,
            "--bmax": maxPx,
            "--cmax": (bars - 1) / 2,
          } as React.CSSProperties
        }
      >
        {levels.slice(0, bars).map((v, i) => (
          <i
            key={i}
            className={settled && i >= lit ? "pending" : undefined}
            style={
              {
                "--l": Math.min(1, Math.pow(v, 0.7)).toFixed(3),
                "--i": i,
                "--c": Math.abs(i - (bars - 1) / 2),
              } as React.CSSProperties
            }
          >
            <b />
          </i>
        ))}
      </div>
    );
  };
  const waveform = waveformFor(false, 3, 18);

  const cancelBtn = (
    // fork(voice-control): expose the existing translated Cancel action to AT.
    <button
      className="sx"
      aria-label={t("tray.cancel")}
      onClick={() => commands.cancelOperation()}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true">
        <path
          d="M4 4 L12 12 M12 4 L4 12"
          stroke="currentColor"
          strokeWidth="1.6"
          strokeLinecap="round"
        />
      </svg>
    </button>
  );

  // (empty) | waveform (center) | warning + cancel (right) — same structure for
  // pill & panel, so the Live morph is a pure width change.
  // fork(voice-control): no separate recording dot — the bars carry the state.
  const listeningRow = (showCancel: boolean) => (
    <div className="sbase">
      <div className="sbase-l" />
      {waveform}
      <div className="sbase-r">
        {limitWarningIndicator}
        {showCancel && cancelBtn}
      </div>
    </div>
  );

  // spinner (left) | label (center) | cancel (right) — same 3-zone grid as the
  // listening row, so the label is centered.
  const workingRow = (label: string, showCancel: boolean) => (
    <div className="sbase">
      <div className="sbase-l">
        <span className="sspinner" />
      </div>
      <span className="swork-label">{label}</span>
      <div className="sbase-r">{showCancel && cancelBtn}</div>
    </div>
  );

  const noticeRow = (
    <div className="sbase snotice">
      <NoticeBadgeIcon className="nglyph" />
      <span className="nlabel">{noticeTitle}</span>
    </div>
  );
  const noticeProbe = shownNotice && (
    <span ref={noticeProbeRef} className="nlabel nprobe" aria-hidden="true">
      {noticeTitle}
    </span>
  );
  const noticeClass = shownNotice ? `cnotice level-${shownNotice.level}` : "";
  const dismissNotice = shownNotice
    ? () => commands.dismissOverlayNotice()
    : undefined;

  // ---- Live overlay: a pill that sculpts open into a panel ----
  const liveNotice = state === "live_notice" ? shownNotice : null;
  if (state === "streaming" || liveNotice) {
    if (!isVisible && !shownNotice) return null;
    const hasText =
      streamText.committed.length > 0 || streamText.tentative.length > 0;
    const working = !liveNotice && phase === "working";
    // A notice keeps the transcript; with none, the panel explains the notice.
    const noticeText =
      liveNotice && !hasText
        ? (noticeDisplay(liveNotice.code).explanation?.(t) ?? liveNotice.detail)
        : null;
    // While listening, a slow model load opens the panel with a notice where the
    // text would be, so speech that isn't transcribing yet doesn't look ignored.
    const loadingNotice = !liveNotice && modelLoadSlow && !working && !hasText;
    // Keep the panel open whenever there's text — even while finalizing — so the
    // transcript stays put under a working spinner instead of collapsing and
    // squishing the text mid-stream. Only fall back to the small working pill
    // when there was no text to preserve.
    const open = liveNotice
      ? hasText || !!noticeText
      : hasText || loadingNotice || (loadNoticeShown && !working);
    const collapsed = working && !hasText;

    return (
      <div dir={direction} className={`ov-stage ${position}`}>
        {noticeProbe}
        <div
          key={session}
          className={`scard ${open ? "open" : ""} ${collapsed ? "working" : ""} ${
            isVisible ? "" : "leaving"
          } ${noticeClass}`}
          style={liveNotice && !open ? { width: noticeWidth } : undefined}
          onClick={dismissNotice}
        >
          <div className="stext">
            <div className="stext-clip">
              <div
                className={`stext-cap ${overflowing ? "overflowing" : ""}`}
                ref={capRef}
                onScroll={handleStreamScroll}
              >
                <p>
                  {loadingNotice && (
                    <span className="sloading">
                      {t("overlay.loadingModel")}
                    </span>
                  )}
                  {noticeText && <span className="ndetail">{noticeText}</span>}
                  <span className="committed">
                    {streamText.committed ? streamText.committed + " " : ""}
                  </span>
                  <span className="tentative">{streamText.tentative}</span>
                  {/* Drop the blinking caret once finalizing — it's no longer
                      capturing, and a static spinner conveys the work. */}
                  {!working && !loadingNotice && !liveNotice && (
                    <span className="scaret" />
                  )}
                </p>
              </div>
            </div>
          </div>
          {liveNotice
            ? noticeRow
            : working
              ? workingRow(
                  // fork(voice-control): explain the allowlist's fallback pass.
                  fallbackActive
                    ? t("overlay.fallbackModel")
                    : workKind === "polishing"
                      ? t("overlay.processing")
                      : transcribingLabel,
                  true,
                )
              : listeningRow(true)}
        </div>
      </div>
    );
  }

  // ---- Minimal overlay ----
  // fork(voice-control): one compact capsule with a single footprint for every
  // state. Recording = live bars; transcribing / processing = the same bars
  // settled into a shimmer; the fallback-model pass is the only state with
  // text, alongside upstream model-loading feedback. Countdown and cancel sit in absolute side
  // slots so the center never shifts.
  // It stays mounted while hidden to fade out (.leaving); key={showSeq}
  // remounts it on each new appearance. Before the first show there is no capsule.
  if (showSeq === 0) return null;
  const working = state === "transcribing" || state === "processing";
  // fork(voice-control): fallback label takes priority; retain upstream's model-loading label.
  const workLabel = fallbackActive
    ? t("overlay.fallbackModel")
    : state === "processing"
      ? t("overlay.processing")
      : transcribingLabel;

  // Upstream notices retain their badge, measured title width and dismissal.
  if (shownNotice) {
    return (
      <div
        dir={direction}
        className={`ov-stage ${position} ov-fade ${isVisible ? "show" : ""}`}
      >
        {noticeProbe}
        <div
          className={`scard compact ${noticeClass}`}
          style={{ width: noticeWidth }}
          onClick={dismissNotice}
        >
          {noticeRow}
        </div>
      </div>
    );
  }

  return (
    <div dir={direction} className={`ov-stage compact ${position}`}>
      <div
        key={showSeq}
        // fork(voice-control): `working` adds the capsule sheen (CSS).
        className={`spill ${working ? "working" : ""} ${isVisible ? "" : "leaving"}`}
        role="status"
        // fork(voice-control): name the live status in recording states too.
        aria-label={working ? workLabel : t("overlay.recording")}
      >
        {!working && limitWarningIndicator}
        {/* fork(voice-control): an earlier dictation is still transcribed
            while this recording runs. Its small wave takes the start slot,
            which the countdown claims in its final seconds. */}
        {!working && backlog > 0 && !limitWarningVisible && (
          <span className="sprior" aria-hidden="true">
            {waveformFor(true, 3, 12, PRIOR_BARS)}
          </span>
        )}
        {/* fork(voice-control): text explains fallback latency; retain model-loading feedback. */}
        {working && (fallbackActive || modelLoading) ? (
          <span className="spill-label">{workLabel}</span>
        ) : (
          waveformFor(working, 4, 18)
        )}
        {cancelBtn}
      </div>
    </div>
  );
};

export default RecordingOverlay;

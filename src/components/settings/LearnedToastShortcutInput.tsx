import React, { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { commands } from "@/bindings";
import type { LearnedToastShortcut } from "@/bindings";
import { useSettings } from "../../hooks/useSettings";
import { useOsType } from "../../hooks/useOsType";
import {
  formatKeyCombination,
  getKeyName,
  normalizeKey,
} from "../../lib/utils/keyboard";
import { ResetButton } from "../ui/ResetButton";
import { SettingContainer } from "../ui/SettingContainer";

const KEY = "settings.advanced.learnedCorrections.toastShortcuts";

const MODIFIER_KEYS = [
  "ctrl",
  "control",
  "shift",
  "alt",
  "option",
  "meta",
  "command",
  "cmd",
  "super",
  "win",
  "windows",
  "fn",
  "capslock",
];

// Conflict markers naming the other toast shortcut, which is not a `bindings`
// entry (ids from correction_learning::toast_shortcuts::Slot::binding_id).
const TOAST_SLOT_TITLES: Record<string, string> = {
  learned_toast_accept: `${KEY}.accept.title`,
  learned_toast_accept_keypad: `${KEY}.accept.title`,
  learned_toast_undo: `${KEY}.dismiss.title`,
  // The toast's fixed dismiss combo (ctrl+escape), reserved for both.
  learned_toast_dismiss: "learnedToast.dismiss",
};

interface LearnedToastShortcutInputProps {
  action: LearnedToastShortcut;
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

/**
 * fork(voice-control): recorder for a learned-toast shortcut. These are not
 * `bindings` entries (they are registered only while the toast is visible, see
 * correction_learning::toast_shortcuts), so the binding-bound ShortcutInput
 * can't be reused; this is a compact recorder that captures the combo in the
 * settings window (modifiers + one key, Escape cancels) and saves it through
 * `changeLearnedToastShortcutSetting`, which validates it for both keyboard
 * backends and refuses conflicts with other Handy shortcuts.
 */
export const LearnedToastShortcutInput: React.FC<
  LearnedToastShortcutInputProps
> = ({ action, descriptionMode = "tooltip", grouped = false }) => {
  const { t, i18n } = useTranslation();
  const { getSetting, refreshSettings } = useSettings();
  const osType = useOsType();
  const [isRecording, setIsRecording] = useState(false);
  const [preview, setPreview] = useState("");
  const [isSaving, setIsSaving] = useState(false);
  const recorderRef = useRef<HTMLButtonElement | null>(null);

  const value =
    getSetting(
      action === "accept"
        ? "learned_toast_accept_shortcut"
        : "learned_toast_dismiss_shortcut",
    ) ?? "";

  const errorMessage = useCallback(
    (error: string) => {
      if (error === "needs-modifier") return t(`${KEY}.errors.needsModifier`);
      if (error === "invalid") return t(`${KEY}.errors.invalid`);
      if (error.startsWith("conflict:")) {
        const id = error.slice("conflict:".length);
        const nameKey =
          TOAST_SLOT_TITLES[id] ??
          `settings.general.shortcut.bindings.${id}.name`;
        return i18n.exists(nameKey)
          ? t(`${KEY}.errors.conflictWith`, { name: t(nameKey) })
          : t(`${KEY}.errors.conflict`);
      }
      return t(`${KEY}.errors.save`, { error });
    },
    [t, i18n],
  );

  // `null` resets to the default.
  const save = useCallback(
    async (binding: string | null) => {
      setIsSaving(true);
      try {
        const result = await commands.changeLearnedToastShortcutSetting(
          action,
          binding,
        );
        if (result.status === "error") {
          toast.error(errorMessage(result.error));
        }
        await refreshSettings();
      } catch (error) {
        toast.error(errorMessage(String(error)));
      } finally {
        setIsSaving(false);
      }
    },
    [action, errorMessage, refreshSettings],
  );

  useEffect(() => {
    if (!isRecording) return;

    // Like the other recorders: no existing shortcut may fire (or swallow the
    // keys) mid-capture.
    void commands.suspendAllBindings().catch(console.error);

    const stop = () => {
      setIsRecording(false);
      setPreview("");
    };

    const handleKeyDown = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.repeat) return;

      const modifiers = [
        e.ctrlKey ? "ctrl" : null,
        e.altKey ? (osType === "macos" ? "option" : "alt") : null,
        e.shiftKey ? "shift" : null,
        e.metaKey ? (osType === "macos" ? "command" : "super") : null,
      ].filter((m): m is string => m !== null);
      const key = normalizeKey(getKeyName(e, osType));

      if (MODIFIER_KEYS.includes(key)) {
        setPreview(modifiers.join("+"));
        return;
      }
      stop();
      if (modifiers.length === 0 && e.key === "Escape") return;
      // A bare key is refused by the backend with a localized hint.
      void save([...modifiers, key].join("+"));
    };

    // The recorder button keeps focus while capturing (keyboard and screen
    // reader users stay on it); it is the same element as the idle button.
    recorderRef.current?.focus();

    const handleClickOutside = (e: MouseEvent) => {
      if (
        recorderRef.current &&
        !recorderRef.current.contains(e.target as Node)
      ) {
        stop();
      }
    };

    window.addEventListener("keydown", handleKeyDown, true);
    window.addEventListener("click", handleClickOutside);
    return () => {
      window.removeEventListener("keydown", handleKeyDown, true);
      window.removeEventListener("click", handleClickOutside);
      void commands.resumeAllBindings().catch(console.error);
    };
  }, [isRecording, osType, save]);

  return (
    <SettingContainer
      title={t(`${KEY}.${action}.title`)}
      description={t(`${KEY}.${action}.description`)}
      descriptionMode={descriptionMode}
      grouped={grouped}
      layout="horizontal"
    >
      <div className="flex items-center space-x-1">
        {/* One button for both states, so focus stays put when recording
            starts and ends; the live region announces the "press keys"
            prompt and the saved combination. */}
        <button
          ref={recorderRef}
          type="button"
          className={`px-2 py-1 text-sm font-semibold border rounded-md ${
            isRecording
              ? "border-logo-primary bg-logo-primary/30"
              : "bg-mid-gray/10 border-mid-gray/80 hover:bg-logo-primary/10 hover:border-logo-primary cursor-pointer aria-disabled:opacity-50"
          }`}
          onClick={() => {
            if (!isSaving) setIsRecording(true);
          }}
          // Not `disabled`: that would drop focus right after a capture.
          aria-disabled={isSaving}
        >
          <span aria-live="polite">
            {isRecording
              ? preview
                ? formatKeyCombination(preview, osType)
                : t("settings.general.shortcut.pressKeys")
              : formatKeyCombination(value, osType)}
          </span>
        </button>
        <ResetButton
          onClick={() => void save(null)}
          disabled={isSaving || isRecording}
        />
      </div>
    </SettingContainer>
  );
};

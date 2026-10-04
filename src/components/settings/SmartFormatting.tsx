import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { commands } from "@/bindings";
import type { AppStyleCategories, Result, Snippet } from "@/bindings";
import { useSettings } from "../../hooks/useSettings";
import { SettingsGroup } from "../ui/SettingsGroup";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { Disclosure } from "../ui/Disclosure";
import { Input } from "../ui/Input";
import { Textarea } from "../ui/Textarea";
import { Button } from "../ui/Button";
import { RemoveIcon } from "../icons";

const CATEGORY_KEYS = ["chat", "terminal", "code", "match_context"] as const;
type CategoryKey = (typeof CATEGORY_KEYS)[number];

const CATEGORY_I18N: Record<CategoryKey, string> = {
  chat: "chat",
  terminal: "terminal",
  code: "code",
  match_context: "matchContext",
};

const isOn = (categories: AppStyleCategories, key: CategoryKey): boolean =>
  categories[key] ?? true;

// Match Rust's char::is_alphanumeric and str::to_lowercase normalization.
const triggerWords = (text: string): string[] =>
  text
    .split(/[^\p{Alphabetic}\p{Number}]+/u)
    .filter(Boolean)
    .map((word) => word.toLowerCase());

/**
 * Fork (voice-control): self-correction LLM pass, per-app styles and
 * snippets — the output stages around the deterministic text rules.
 */
export const SmartFormatting: React.FC = () => {
  const { t } = useTranslation();
  const { getSetting, updateSetting, isUpdating, refreshSettings } =
    useSettings();

  const selfCorrection = getSetting("self_correction_llm_enabled") ?? true;
  const appStyles = getSetting("app_styles_enabled") ?? true;
  const categories: AppStyleCategories =
    getSetting("app_styles_categories") ?? {};
  const snippets = getSetting("snippets") ?? [];

  const [trigger, setTrigger] = useState("");
  const [expansion, setExpansion] = useState("");
  const [editingId, setEditingId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const prefix = "settings.advanced.smartFormatting";
  const activeCategories = CATEGORY_KEYS.filter((key) =>
    isOn(categories, key),
  ).length;

  const toggleCategory = (key: CategoryKey, enabled: boolean) => {
    updateSetting("app_styles_categories", { ...categories, [key]: enabled });
  };

  // Runs a granular snippet command; the backend's settings-changed event
  // refreshes the store too, the explicit refresh keeps the UI snappy.
  const runSnippetCommand = async (
    command: () => Promise<Result<unknown, string>>,
  ): Promise<boolean> => {
    setBusy(true);
    try {
      const result = await command();
      if (result.status === "error") {
        console.error("Snippet command failed:", result.error);
        toast.error(t(`${prefix}.snippets.errors.commandFailed`));
        return false;
      }
      await refreshSettings();
      return true;
    } catch (error) {
      console.error("Snippet command failed:", error);
      toast.error(t(`${prefix}.snippets.errors.commandFailed`));
      return false;
    } finally {
      setBusy(false);
    }
  };

  const resetForm = () => {
    setTrigger("");
    setExpansion("");
    setEditingId(null);
  };

  const handleSubmit = async () => {
    const words = triggerWords(trigger.trim());
    if (words.length === 0) {
      toast.error(t(`${prefix}.snippets.errors.triggerRequired`));
      return;
    }
    if (!expansion.trim()) {
      toast.error(t(`${prefix}.snippets.errors.expansionRequired`));
      return;
    }
    if (
      snippets.some((snippet) => {
        if (snippet.id === editingId) return false;
        const existingWords = triggerWords(snippet.trigger);
        return (
          existingWords.length === words.length &&
          existingWords.every((word, index) => word === words[index])
        );
      })
    ) {
      toast.error(
        t(`${prefix}.snippets.errors.duplicateTrigger`, {
          trigger: trigger.trim(),
        }),
      );
      return;
    }
    const existing = snippets.find((s) => s.id === editingId);
    const ok = existing
      ? await runSnippetCommand(() =>
          commands.updateSnippet({ ...existing, trigger, expansion }),
        )
      : await runSnippetCommand(() => commands.addSnippet(trigger, expansion));
    if (ok) resetForm();
  };

  const handleEdit = (snippet: Snippet) => {
    setEditingId(snippet.id);
    setTrigger(snippet.trigger);
    setExpansion(snippet.expansion);
  };

  const handleToggle = (snippet: Snippet, enabled: boolean) =>
    runSnippetCommand(() => commands.updateSnippet({ ...snippet, enabled }));

  const handleRemove = async (snippet: Snippet) => {
    const ok = await runSnippetCommand(() =>
      commands.removeSnippet(snippet.id),
    );
    if (ok && editingId === snippet.id) resetForm();
  };

  return (
    <SettingsGroup title={t("settings.advanced.groups.smartFormatting")}>
      <ToggleSwitch
        checked={selfCorrection}
        onChange={(checked) =>
          updateSetting("self_correction_llm_enabled", checked)
        }
        isUpdating={isUpdating("self_correction_llm_enabled")}
        label={t(`${prefix}.selfCorrection.title`)}
        description={t(`${prefix}.selfCorrection.description`)}
        descriptionMode="tooltip"
        grouped
      />

      <ToggleSwitch
        checked={appStyles}
        onChange={(checked) => updateSetting("app_styles_enabled", checked)}
        isUpdating={isUpdating("app_styles_enabled")}
        label={t(`${prefix}.appStyles.title`)}
        description={t(`${prefix}.appStyles.description`)}
        descriptionMode="tooltip"
        grouped
      />

      {appStyles && (
        <Disclosure
          title={t(`${prefix}.appStyles.categories.title`)}
          summary={t(`${prefix}.appStyles.categories.summary`, {
            active: activeCategories,
            total: CATEGORY_KEYS.length,
          })}
        >
          <div className="px-4 space-y-2">
            {CATEGORY_KEYS.map((key) => (
              <label
                key={key}
                className="flex items-start gap-2 text-sm cursor-pointer"
              >
                <input
                  type="checkbox"
                  className="mt-1"
                  checked={isOn(categories, key)}
                  disabled={isUpdating("app_styles_categories")}
                  onChange={(e) => toggleCategory(key, e.target.checked)}
                />
                <span>
                  <span className="font-medium">
                    {t(
                      `${prefix}.appStyles.categories.${CATEGORY_I18N[key]}.label`,
                    )}
                  </span>
                  <span className="block text-xs text-mid-gray">
                    {t(
                      `${prefix}.appStyles.categories.${CATEGORY_I18N[key]}.hint`,
                    )}
                  </span>
                </span>
              </label>
            ))}
          </div>
        </Disclosure>
      )}

      <Disclosure
        title={t(`${prefix}.snippets.title`)}
        summary={snippets.length}
      >
        <div className="px-4 space-y-2">
          <p className="text-xs text-mid-gray">
            {t(`${prefix}.snippets.description`)}
          </p>
          <div className="flex flex-col gap-2">
            <Input
              type="text"
              value={trigger}
              onChange={(e) => setTrigger(e.target.value)}
              placeholder={t(`${prefix}.snippets.triggerPlaceholder`)}
              aria-label={t(`${prefix}.snippets.triggerPlaceholder`)}
              variant="compact"
              disabled={busy}
            />
            <Textarea
              value={expansion}
              onChange={(e) => setExpansion(e.target.value)}
              placeholder={t(`${prefix}.snippets.expansionPlaceholder`)}
              aria-label={t(`${prefix}.snippets.expansionPlaceholder`)}
              variant="compact"
              disabled={busy}
            />
            <div className="flex gap-2">
              <Button
                onClick={handleSubmit}
                disabled={busy}
                variant="primary"
                size="md"
              >
                {editingId
                  ? t(`${prefix}.snippets.save`)
                  : t(`${prefix}.snippets.add`)}
              </Button>
              {editingId && (
                <Button onClick={resetForm} variant="secondary" size="md">
                  {t(`${prefix}.snippets.cancel`)}
                </Button>
              )}
            </div>
          </div>
          {snippets.length > 0 && (
            <div className="flex flex-col gap-1">
              {snippets.map((snippet) => (
                <div
                  key={snippet.id}
                  className="flex items-center justify-between gap-2 text-sm"
                >
                  <label className="flex items-center gap-2 min-w-0 cursor-pointer">
                    <input
                      type="checkbox"
                      checked={snippet.enabled ?? true}
                      disabled={busy}
                      onChange={(e) => handleToggle(snippet, e.target.checked)}
                      aria-label={t(`${prefix}.snippets.enable`, {
                        trigger: snippet.trigger,
                      })}
                    />
                    <span className="truncate">
                      {t(`${prefix}.snippets.mapping`, {
                        trigger: snippet.trigger,
                        expansion: snippet.expansion.split("\n")[0],
                      })}
                    </span>
                  </label>
                  <div className="flex items-center gap-1 shrink-0">
                    <Button
                      onClick={() => handleEdit(snippet)}
                      disabled={busy}
                      variant="secondary"
                      size="sm"
                    >
                      {t(`${prefix}.snippets.edit`)}
                    </Button>
                    <Button
                      onClick={() => handleRemove(snippet)}
                      disabled={busy}
                      variant="danger-ghost"
                      size="sm"
                      aria-label={t(`${prefix}.snippets.remove`, {
                        trigger: snippet.trigger,
                      })}
                    >
                      <RemoveIcon />
                    </Button>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      </Disclosure>
    </SettingsGroup>
  );
};

import React, {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { type } from "@tauri-apps/plugin-os";
import { useSettings } from "../../hooks/useSettings";
import { commands, events } from "@/bindings";
import type {
  Aggressiveness,
  LearnedCorrection,
  LearnedCorrections as LearnedCorrectionsData,
  Result,
} from "@/bindings";
import { Input } from "../ui/Input";
import { Button } from "../ui/Button";
import { ToggleSwitch } from "../ui/ToggleSwitch";
import { Dropdown } from "../ui/Dropdown";
import type { DropdownOption } from "../ui/Dropdown";
import { Disclosure } from "../ui/Disclosure";
import { SettingContainer } from "../ui/SettingContainer";
import { RemoveIcon } from "../icons";
import { LearnedToastShortcutInput } from "./LearnedToastShortcutInput";

interface LearnedCorrectionsProps {
  descriptionMode?: "inline" | "tooltip";
  grouped?: boolean;
}

const AGGRESSIVENESS_LEVELS: Aggressiveness[] = [
  "conservative",
  "balanced",
  "aggressive",
];

// Learning-window bounds, mirroring MIN_WINDOW_SECS / MAX_WINDOW_SECS in
// correction_learning/session.rs — the persisted value is clamped here so the
// input always shows the value the session will actually use.
const WINDOW_MIN_SECS = 10;
const WINDOW_MAX_SECS = 300;

// Above this many dictionary entries a filter input appears.
const FILTER_THRESHOLD = 10;

const KEY = "settings.advanced.learnedCorrections";

// Backend error strings with a dedicated translation (correction_learning::commands).
const KNOWN_ERRORS = new Map([
  ["misheard and intended text must not be empty", "emptyPair"],
]);

const badgeClass =
  "shrink-0 text-[10px] uppercase tracking-wide px-1.5 py-0.5 rounded bg-mid-gray/15 text-mid-gray";

/**
 * fork(voice-control): review UI for learned corrections. The backend store
 * (correction_learning) is the source of truth — the list is fetched through
 * `getLearnedCorrections` and refetched on `learned-corrections-changed`, so
 * pairs learned in the background show up without a reload.
 *
 * Sections: the "learn from my corrections" switch, suggestions awaiting
 * review, the active dictionary, blocked pairs, and the learning options incl.
 * the toast's keyboard shortcuts (the switch and options are macOS only — the
 * post-paste watcher exists there alone; the dictionary itself applies on
 * every platform).
 */
export const LearnedCorrections: React.FC<LearnedCorrectionsProps> = React.memo(
  ({ descriptionMode = "tooltip", grouped = false }) => {
    const { t } = useTranslation();
    const { getSetting, updateSetting, isUpdating } = useSettings();

    const isMacOS = type() === "macos";

    const enabled = getSetting("learn_corrections_enabled") || false;
    const learnFromEdits = getSetting("learn_from_edits_enabled") ?? true;
    const aggressiveness =
      getSetting("learn_corrections_aggressiveness") || "conservative";
    const windowSecs = getSetting("learn_corrections_window_secs") ?? 45;

    const [data, setData] = useState<LearnedCorrectionsData | null>(null);
    const [newMisheard, setNewMisheard] = useState("");
    const [newIntended, setNewIntended] = useState("");
    const [filter, setFilter] = useState("");
    const [isMutating, setIsMutating] = useState(false);
    // The add inputs are disabled while a mutation runs, which drops focus;
    // after a successful add, focus returns to the misheard field once they
    // are enabled again so the next pair can be typed right away.
    const addRowRef = useRef<HTMLDivElement>(null);
    const refocusAddRef = useRef(false);
    // Draft for the window input: typing "45" passes through "4", which must
    // not be clamped (and saved) before the user is done.
    const [windowDraft, setWindowDraft] = useState(String(windowSecs));

    useEffect(() => {
      setWindowDraft(String(windowSecs));
    }, [windowSecs]);

    const refresh = useCallback(async () => {
      try {
        setData(await commands.getLearnedCorrections());
      } catch (error) {
        console.error("Failed to load learned corrections:", error);
      }
    }, []);

    useEffect(() => {
      void refresh();
      let unlisten: (() => void) | undefined;
      let cancelled = false;
      events.learnedCorrectionsChanged
        .listen(() => void refresh())
        .then((fn) => {
          if (cancelled) fn();
          else unlisten = fn;
        });
      return () => {
        cancelled = true;
        unlisten?.();
      };
    }, [refresh]);

    const corrections = data?.corrections ?? [];
    const blocked = data?.blocked ?? [];

    // Most-seen suggestions first: those are the likeliest genuine fixes.
    const suggestions = useMemo(
      () =>
        corrections
          .filter((c) => c.status === "suggested")
          .sort((a, b) => b.count - a.count || b.last_seen - a.last_seen),
      [corrections],
    );

    const dictionary = useMemo(
      () =>
        corrections
          .filter((c) => c.status === "active")
          .sort((a, b) =>
            a.misheard.localeCompare(b.misheard, undefined, {
              sensitivity: "base",
            }),
          ),
      [corrections],
    );

    const visibleDictionary = useMemo(() => {
      const query = filter.trim().toLowerCase();
      if (!query) return dictionary;
      return dictionary.filter(
        (c) =>
          c.misheard.toLowerCase().includes(query) ||
          c.intended.toLowerCase().includes(query),
      );
    }, [dictionary, filter]);

    // Every mutation goes through a granular backend command and then refetches
    // — writing a whole list back from this (possibly stale) copy would drop
    // pairs learned in the background between load and edit.
    const mutate = async (action: () => Promise<Result<unknown, string>>) => {
      setIsMutating(true);
      try {
        const result = await action();
        if (result.status === "error") {
          // Backend errors are English diagnostics, not UI copy: show a
          // translated message and keep the raw text for debugging.
          console.warn("Learned correction command failed:", result.error);
          const known = KNOWN_ERRORS.get(result.error);
          toast.error(t(`${KEY}.errors.${known ?? "generic"}`));
        }
        await refresh();
        return result.status === "ok";
      } finally {
        setIsMutating(false);
      }
    };

    const handleAdd = async () => {
      const misheard = newMisheard.trim();
      const intended = newIntended.trim();
      if (!misheard || !intended) {
        return;
      }
      // The backend upserts on the misheard key, so re-adding a word replaces
      // its previous target (and lifts a block on the pair).
      const ok = await mutate(() =>
        commands.addLearnedCorrection(misheard, intended),
      );
      if (ok) {
        setNewMisheard("");
        setNewIntended("");
        refocusAddRef.current = true;
      }
    };

    useEffect(() => {
      if (!isMutating && refocusAddRef.current) {
        refocusAddRef.current = false;
        addRowRef.current?.querySelector("input")?.focus();
      }
    }, [isMutating]);

    const handleAddKeyDown = (e: React.KeyboardEvent) => {
      if (e.key === "Enter") {
        e.preventDefault();
        void handleAdd();
      }
    };

    const commitWindow = () => {
      const value = parseInt(windowDraft, 10);
      if (isNaN(value)) {
        setWindowDraft(String(windowSecs));
        return;
      }
      const clamped = Math.min(
        WINDOW_MAX_SECS,
        Math.max(WINDOW_MIN_SECS, value),
      );
      setWindowDraft(String(clamped));
      if (clamped !== windowSecs) {
        updateSetting("learn_corrections_window_secs", clamped);
      }
    };

    const aggressivenessOptions: DropdownOption[] = AGGRESSIVENESS_LEVELS.map(
      (level) => ({
        value: level,
        label: t(`${KEY}.aggressiveness.${level}.label`),
        description: t(`${KEY}.aggressiveness.${level}.description`),
      }),
    );

    const mapping = (c: { misheard: string; intended: string }) =>
      t(`${KEY}.mapping`, { misheard: c.misheard, intended: c.intended });

    const renderDictionaryRow = (correction: LearnedCorrection) => (
      <div
        key={correction.id}
        className="flex items-center justify-between gap-2 px-2 py-1 text-sm hover:bg-mid-gray/5"
      >
        <label className="flex items-center gap-2 min-w-0 flex-1 cursor-pointer">
          <input
            type="checkbox"
            checked={correction.enabled}
            disabled={isMutating}
            onChange={(e) =>
              void mutate(() =>
                commands.setLearnedCorrectionEnabled(
                  correction.id,
                  e.target.checked,
                ),
              )
            }
            aria-label={t(`${KEY}.enable`, { misheard: correction.misheard })}
          />
          <span
            className={`truncate ${correction.enabled ? "" : "opacity-50 line-through"}`}
          >
            {mapping(correction)}
          </span>
        </label>
        <div className="flex items-center gap-2 shrink-0">
          {correction.count > 1 && (
            <span className="text-xs text-mid-gray">
              {t(`${KEY}.count`, { count: correction.count })}
            </span>
          )}
          {correction.lang && (
            <span
              className={badgeClass}
              title={t(`${KEY}.langBadge`, { lang: correction.lang })}
            >
              {correction.lang}
            </span>
          )}
          <span className={badgeClass}>
            {t(`${KEY}.source.${correction.source}`)}
          </span>
          <Button
            onClick={() =>
              void mutate(() => commands.removeLearnedCorrection(correction.id))
            }
            disabled={isMutating}
            variant="danger-ghost"
            size="sm"
            aria-label={t(`${KEY}.remove`, { misheard: correction.misheard })}
          >
            <RemoveIcon />
          </Button>
        </div>
      </div>
    );

    return (
      <>
        <ToggleSwitch
          checked={enabled}
          onChange={(checked) =>
            updateSetting("learn_corrections_enabled", checked)
          }
          isUpdating={isUpdating("learn_corrections_enabled")}
          label={t(`${KEY}.title`)}
          description={t(`${KEY}.description`)}
          descriptionMode={descriptionMode}
          grouped={grouped}
        />

        {enabled && (
          <>
            {isMacOS && (
              <ToggleSwitch
                checked={learnFromEdits}
                onChange={(checked) =>
                  updateSetting("learn_from_edits_enabled", checked)
                }
                isUpdating={isUpdating("learn_from_edits_enabled")}
                label={t(`${KEY}.learnFromEdits.title`)}
                description={t(`${KEY}.learnFromEdits.description`)}
                descriptionMode={descriptionMode}
                grouped={grouped}
              />
            )}

            {!isMacOS && (
              <p className="px-4 pb-2 text-xs text-mid-gray">
                {t(`${KEY}.platformHint`)}
              </p>
            )}

            {suggestions.length > 0 && (
              <div className="px-4 py-2 space-y-2">
                <div className="flex items-center gap-2">
                  <span className="text-sm font-medium">
                    {t(`${KEY}.suggestions.title`)}
                  </span>
                  <span className="text-xs font-semibold px-1.5 py-0.5 rounded-full bg-logo-primary/20 text-logo-primary">
                    {suggestions.length}
                  </span>
                </div>
                <p className="text-xs text-mid-gray">
                  {t(`${KEY}.suggestions.description`)}
                </p>
                <div className="flex flex-col gap-1 max-h-64 overflow-y-auto">
                  {suggestions.map((suggestion) => (
                    <div
                      key={suggestion.id}
                      className="flex items-center justify-between gap-2 px-2 py-1.5 text-sm rounded-md bg-logo-primary/5 border border-logo-primary/20"
                    >
                      <span className="truncate min-w-0 flex-1">
                        {mapping(suggestion)}
                      </span>
                      <span className="shrink-0 text-xs text-mid-gray">
                        {t(`${KEY}.count`, { count: suggestion.count })}
                      </span>
                      <div className="flex items-center gap-1 shrink-0">
                        <Button
                          onClick={() =>
                            void mutate(() =>
                              commands.acceptLearnedCorrection(suggestion.id),
                            )
                          }
                          disabled={isMutating}
                          variant="primary-soft"
                          size="sm"
                        >
                          {t(`${KEY}.suggestions.accept`)}
                        </Button>
                        <Button
                          onClick={() =>
                            void mutate(() =>
                              commands.rejectLearnedCorrections([
                                suggestion.id,
                              ]),
                            )
                          }
                          disabled={isMutating}
                          variant="ghost"
                          size="sm"
                        >
                          {t(`${KEY}.suggestions.dismiss`)}
                        </Button>
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            )}

            <Disclosure
              title={t(`${KEY}.dictionary.title`)}
              summary={dictionary.length}
              defaultOpen
            >
              <div className="px-4 space-y-2">
                <div
                  ref={addRowRef}
                  className="flex flex-wrap items-center gap-2"
                >
                  <Input
                    type="text"
                    className="max-w-40"
                    value={newMisheard}
                    onChange={(e) => setNewMisheard(e.target.value)}
                    onKeyDown={handleAddKeyDown}
                    placeholder={t(`${KEY}.add.misheardPlaceholder`)}
                    variant="compact"
                    disabled={isMutating}
                  />
                  <Input
                    type="text"
                    className="max-w-40"
                    value={newIntended}
                    onChange={(e) => setNewIntended(e.target.value)}
                    onKeyDown={handleAddKeyDown}
                    placeholder={t(`${KEY}.add.intendedPlaceholder`)}
                    variant="compact"
                    disabled={isMutating}
                  />
                  <Button
                    onClick={() => void handleAdd()}
                    disabled={
                      !newMisheard.trim() || !newIntended.trim() || isMutating
                    }
                    variant="primary"
                    size="md"
                  >
                    {t(`${KEY}.add.button`)}
                  </Button>
                </div>

                {dictionary.length === 0 ? (
                  <p className="text-xs text-mid-gray">{t(`${KEY}.empty`)}</p>
                ) : (
                  <>
                    {dictionary.length > FILTER_THRESHOLD && (
                      <Input
                        type="text"
                        className="w-full"
                        value={filter}
                        onChange={(e) => setFilter(e.target.value)}
                        placeholder={t(`${KEY}.dictionary.filterPlaceholder`)}
                        aria-label={t(`${KEY}.dictionary.filterPlaceholder`)}
                        variant="compact"
                      />
                    )}
                    <div className="max-h-64 overflow-y-auto border border-mid-gray/20 rounded">
                      {visibleDictionary.length === 0 ? (
                        <div className="px-2 py-2 text-sm text-mid-gray text-center">
                          {t(`${KEY}.dictionary.noMatches`)}
                        </div>
                      ) : (
                        visibleDictionary.map(renderDictionaryRow)
                      )}
                    </div>
                  </>
                )}
              </div>
            </Disclosure>

            {blocked.length > 0 && (
              <Disclosure
                title={t(`${KEY}.blocked.title`)}
                summary={blocked.length}
              >
                <div className="px-4 space-y-2">
                  <p className="text-xs text-mid-gray">
                    {t(`${KEY}.blocked.description`)}
                  </p>
                  <div className="max-h-64 overflow-y-auto border border-mid-gray/20 rounded">
                    {blocked.map((entry) => (
                      <div
                        key={entry.id}
                        className="flex items-center justify-between gap-2 px-2 py-1 text-sm hover:bg-mid-gray/5"
                      >
                        <span className="truncate min-w-0 text-mid-gray">
                          {mapping(entry)}
                        </span>
                        <Button
                          onClick={() =>
                            void mutate(() =>
                              commands.unblockLearnedCorrection(entry.id),
                            )
                          }
                          disabled={isMutating}
                          variant="ghost"
                          size="sm"
                        >
                          {t(`${KEY}.blocked.unblock`)}
                        </Button>
                      </div>
                    ))}
                  </div>
                </div>
              </Disclosure>
            )}

            {isMacOS && learnFromEdits && (
              <Disclosure title={t(`${KEY}.options.title`)}>
                <SettingContainer
                  title={t(`${KEY}.aggressiveness.title`)}
                  description={t(`${KEY}.aggressiveness.description`)}
                  descriptionMode={descriptionMode}
                  grouped={grouped}
                >
                  <Dropdown
                    className="w-44"
                    menuClassName="right-0 w-72"
                    options={aggressivenessOptions}
                    selectedValue={aggressiveness}
                    onSelect={(value) =>
                      updateSetting(
                        "learn_corrections_aggressiveness",
                        value as Aggressiveness,
                      )
                    }
                    disabled={isUpdating("learn_corrections_aggressiveness")}
                  />
                </SettingContainer>
                <SettingContainer
                  title={t(`${KEY}.window.title`)}
                  description={t(`${KEY}.window.description`)}
                  descriptionMode={descriptionMode}
                  grouped={grouped}
                >
                  <div className="flex items-center gap-2">
                    <Input
                      type="number"
                      min={WINDOW_MIN_SECS}
                      max={WINDOW_MAX_SECS}
                      value={windowDraft}
                      onChange={(e) => setWindowDraft(e.target.value)}
                      onBlur={commitWindow}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") {
                          e.preventDefault();
                          commitWindow();
                        }
                      }}
                      disabled={isUpdating("learn_corrections_window_secs")}
                      className="w-20"
                      variant="compact"
                    />
                    <span className="text-sm text-text">
                      {t(`${KEY}.window.seconds`)}
                    </span>
                  </div>
                </SettingContainer>
                <LearnedToastShortcutInput
                  action="accept"
                  descriptionMode={descriptionMode}
                  grouped={grouped}
                />
                <LearnedToastShortcutInput
                  action="dismiss"
                  descriptionMode={descriptionMode}
                  grouped={grouped}
                />
              </Disclosure>
            )}
          </>
        )}
      </>
    );
  },
);

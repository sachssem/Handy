# Auto-Lernen aus Korrekturen — Machbarkeit & Implementationsplan

> Stand: 2026-07-07 · Status: **Implementiert — Phasen A–D committet** (Store + Apply, macOS-AX-Session, Toast, Aggressivität/Phonetik/Fenster) · Referenz: [REQUIREMENTS.md](../REQUIREMENTS.md) 🍴 „Auto-Lernen aus Korrekturen"

## Spike-Ergebnisse (2026-07-06, Phase 0 abgeschlossen)

Probe: `~/tmp/ax_probe/` (Wegwerf-Code, signiert mit `Voice Control / Handy Dev`, TCC-Grant als „ax_probe"). Kernmechanismus **end-to-end bewiesen**: Feld lesen → Snapshot → manuelle Korrektur → Wort-Diff lieferte exakt `CORRECTION CANDIDATE: "Munchen" → "München"`; Umlaute überleben CFString→Rust intakt; `kAXSelectedTextRangeAttribute` verfügbar.

Gemessene Abdeckung (Pin-Modus auf laufende Apps):

| App                          | Ergebnis                                                                                                             |
| ---------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| TextEdit (nativ)             | ✅ voller Zyklus inkl. Diff                                                                                          |
| iTerm (Terminal)             | ✅ Feldtext lesbar                                                                                                   |
| **T3 Code (Electron)**       | ✅ **AXTextArea mit echtem Value — Electron geht entgegen Erwartung!** (Chromium exponiert fokussierten Text via AX) |
| zen (Firefox-Fork)           | ❌ nur `AXWebArea`, len=0 — Web-Content braucht tieferen AX-Descent                                                  |
| Safari (Textarea fokussiert) | ❌ `kAXErrorNoValue` am Top-Level-Fokus — gleiche Web-Content-Lücke                                                  |
| Notes                        | ➖ unschlüssig (Sidebar fokussiert, Body nicht getestet)                                                             |
| Cursor                       | ➖ unschlüssig (kein Key-Window beim Pin-Test; dürfte sich wie T3 Code verhalten)                                    |
| Slack / Mail / Passwortfeld  | ⏳ ungetestet                                                                                                        |

**Go/No-Go-Ergebnis: volles GO** (nativ ok **und** Electron ok — besser als der geplante Baseline-Fall). Neue Erkenntnisse für die Implementierung:

1. **Systemwide-Fokuspfad funktioniert nicht aus CLI-Prozessen** (`kAXErrorCannotComplete`); zuverlässig ist: Frontmost-PID (NSWorkspace) → `AXUIElementCreateApplication(pid)` → `AXFocusedUIElement`. Im echten Handy (GUI-App mit Run-Loop) so umsetzen; ein Run-Loop-Pump ist dort unnötig.
2. **Browser sind die echte Lücke** (nicht Electron): Web-`<textarea>`/`<input>` erscheinen nicht am Top-Level-Fokus — entweder tieferer Descent in den `AXWebArea`-Teilbaum (Folgearbeit) oder Browser stumm degradieren.
3. ~50 Zeilen eigenes FFI reichen; die `axuielement`-Crate (zieht Swift-Package-Build mit) ist nicht nötig.
4. Offen für später: Secure-Field-Verhalten (Privacy-Check), Slack/Mail, Notes-Body, voller Zyklus in einer Electron-App.

## Feature

Nach dem Einfügen einer Transkription in die aktive App erkennen, wenn der Nutzer innerhalb eines Zeitfensters ein Wort manuell korrigiert, die Korrektur (gehört → gemeint) extrahieren, automatisch ins Wörterbuch übernehmen und ~5 s einen Toast zeigen („Gelernt: X → Y") mit Undo-Button. macOS primär; Windows/Linux zunächst Stub (Feature fehlt dort stillschweigend, Apply-Stufe funktioniert überall).

## Machbarkeits-Fazit

**Machbar auf macOS, als Fork-Alleinstellungsfeature tauglich — aber zwingend hinter einem AX-Spike auf dieser Maschine gegated, bevor Integrationscode entsteht.**

Belege aus der Recherche:

- **Wispr Flow macht exakt das** (forensisch nachgewiesen, `EditedTextManager v2`): nach dem Paste wird das fokussierte Feld per macOS-Accessibility-API erneut gelesen und gegen den ASR-Output gedifft; Wispr extrahiert die Korrektur dann cloud-seitig per LLM. Wir replizieren den Loop komplett lokal. Quellen: https://www.wensenwu.com/thoughts/wispr-flow-investigation , https://docs.wisprflow.ai/articles/4052411709
- **Kein OSS-Tool hat das** — bestätigte Marktlücke. Nächster Verwandter: Talon (`user.add_selection_to_vocabulary`, aber nutzerinitiiert), VoiceInk (nur manuelle Replacements).
- **Rust-Bausteine existieren**: `axuielement` (v0.9, doom-fish/axuielement-rs; AXUIElement + AXObserver) für den Feld-Read, `similar` (`TextDiff::from_unicode_words`) für den Wort-Diff, `rphonetic` (Double Metaphone + Kölner Phonetik für DE) fürs Gating.
- **Handy-seitig ist alles vorbereitet**: ein Text-Transform-Funnel (`post_process_transcription_text`, transcription.rs:1596–1619) für die Apply-Stufe, eine Paste-Callsite (actions.rs:764) für die Capture-Stufe, Accessibility-Permission via `tauri-plugin-macos-permissions` bereits erteilt, `text_rules` als Blaupause für additive Fork-Features.

**Das eine harte Risiko: AX-Lesbarkeit ist pro Ziel-App verschieden.** Electron-Apps (VS Code/Cursor, Slack; `AXManualAccessibility` defekt: electron/electron#37465), Secure Fields und teils Web-Content liefern keinen Feldinhalt. Mitigation: Spike misst reale Abdeckung zuerst; Laufzeit degradiert pro App stumm (kein AX-Text → keine Session, kein Fehler); Phase A liefert auch ohne AX eigenständigen Nutzen.

## Phase 0 — Spike (vor jeder Integration)

Wegwerf-Probe **außerhalb des Repos** (z. B. `~/tmp/ax_probe/`) oder git-ignoriertes `cargo run --example ax_probe` — nicht committen. Loop alle ~3 s: `AXUIElementCreateSystemWide` → `kAXFocusedUIElementAttribute` → `kAXValueAttribute`, Ausgabe `(Bundle-ID, Rolle, Textlänge, Preview)`.

Testmatrix (Feld fokussieren, Wort tippen, Log beobachten):

| App                          | Klasse      | Erwartung                         |
| ---------------------------- | ----------- | --------------------------------- |
| TextEdit                     | Nativ Cocoa | Muss gehen (Baseline)             |
| Apple Notes / Mail           | Nativ Cocoa | Muss gehen                        |
| Safari / Chrome `<textarea>` | Web         | Safari ok / Chrome partiell       |
| VS Code / Cursor             | Electron    | Erwartet FAIL                     |
| Slack                        | Electron    | Erwartet FAIL                     |
| Terminal / iTerm             | TTY         | Kein brauchbarer AX-Value         |
| Passwortfeld                 | Secure      | MUSS leer bleiben (Privacy-Check) |

Erfolgskriterien pro App: kompletter Feldtext, aktualisiert binnen eines Polls nach Keystroke, korrektes Unicode (Umlaute).

Go/No-Go:

- Nativ ok **und** ≥1 Electron-Editor ok → volles GO.
- Nativ ok, Electron fail (wahrscheinlichster Fall) → **GO mit Per-App-Degradierung** (geplanter Baseline-Fall).
- Auch nativ fail → NO-GO für AX; nur Phase A shippen, Crate/Entitlements re-evaluieren (Fallback: dünner objc2-FFI-Wrapper).
- Secure Field liefert Text → harter Stopp; `AXSecureTextField`-Exclusion vor jeder Speicherung.

## Architektur

Neues fork-eigenes Modul `src-tauri/src/correction_learning/`:

```
correction_learning/
  mod.rs        # API: apply_learned(text, &settings) -> String; begin_session(...); LearnedCorrection
  ax_reader.rs  # #[cfg(macos)] axuielement-Read des fokussierten Felds; sonst Stub → None
  session.rs    # Lernfenster-State-Machine: Paste-Snapshot, Key-Aktivität/Fokuswechsel/Idle-Trigger, Re-Read+Diff
  differ.rs     # Wort-Diff (similar), Delete+Insert-Merge zu Substitutionen, ALLE Gates (pur, table-testbar)
  store.rs      # Persistenz + Hot-Cache; Undo; manuelles Hinzufügen
```

Datenfluss:

```
actions.rs:764 utils::paste(final_text)
  └─ (NEU, 1 Zeile) begin_session(app, original=final_text, ctx)
       └─ session: Snapshot {original, ax_element/pid/bundle, t0}
            Trigger (Key-Aktivität abgeklungen | Fokuswechsel | Idle) innerhalb ~45 s
            └─ ax_reader: Feld erneut lesen (None → Session stumm verwerfen)
                 └─ differ: diff + Gates → Kandidat (gehört→gemeint) oder verwerfen
                      └─ store: upsert {misheard, intended, count, last_seen, source: auto, enabled}
                           └─ emit "learned-correction" {id, misheard, intended} → Toast

Nächste Transkription:
transcription.rs:1618 (nach apply_text_rules)
  └─ (NEU, 1 Zeile) apply_learned(text, &settings)   # wortgrenzen-genau, längste zuerst
```

Schmale Upstream-Berührungspunkte (Text-Rules-Muster): `actions.rs` (~:765, 1 Call), `transcription.rs:1618` (1 Call), `settings.rs` (Felder + Defaults), `shortcut/mod.rs` (Commands, Muster `change_text_rules_*` ~:730), `lib.rs` (mod + invoke_handler). Frontend: `settingsStore.ts`, `bindings.ts` (handgepflegt), neu `LearnedCorrections.tsx` + `LearnedToast.tsx`, Locale-JSONs.

**Storage-Entscheidung:** `learned_corrections: Vec<LearnedCorrection>` in `AppSettings` (Klon des bewährten `text_rules_custom`-Pfads: JSON-Persistenz, specta-Bindings, Hot-Path-Zugriff gratis). SQLite (`history.db`-Migration) nur, falls die Liste groß wird oder Analytics braucht.

## Diff-Gating (gegen Wörterbuch-Vergiftung)

Reihenfolge: Case/Punktuation normalisieren → nur **Substitutionen** (reine Inserts/Deletes = Umformulierung, nicht Verhörer) → begrenzte relative Levenshtein-Distanz → phonetische Ähnlichkeit (Double Metaphone; **Kölner Phonetik bei DE**) → Wortanzahl-Limits → Zeitfenster → Präferenz für Großgeschriebenes/OOV (Eigennamen/Jargon lernen, Allerweltswörter skippen). Vorbild: Dragon/Nuance-Patente US8280733/US11848000 (Korrektur vs. absichtlicher Edit klassifizieren). Phonetik-Gate nie hart blockierend bei eindeutig niedriger Levenshtein-Distanz.

## Apply-Strategie

- **Primär:** deterministische Post-Processing-Ersetzung im Funnel (wortgrenzen-genau, längste Phrase zuerst; Spec: VoiceInk Word Replacements). Deterministisch, halluzinationsfrei, identisch für Whisper & Parakeet.
- **Sekundär:** Top-N gelernte Begriffe zusätzlich in den Whisper-`initial_prompt` (224-Token-Budget, als natürlicher Satz — Quelle: OpenAI Whisper Prompting Guide; für Parakeet wirkungslos).
- **Explizit verschoben:** natives Parakeet-Hotword-Boosting (sherpa-onnx, PR #3077) — erfordert Wechsel des Decode-Pfads, instabil (Issue #3267).

## Toast-UI

**Eigenes kleines Fenster** (`learned_toast`), nicht neuer Phase des Recording-Overlays: das Overlay (NSPanel, `focusable(false)`, overlay.rs:322) ist an den Record-Lifecycle gekoppelt und nicht klickbar; der Toast braucht ~5 s Eigenleben + klickbaren Undo-Button (`can_become_key_window: true`). Event `learned-correction-event` `{id, misheard, intended}` (tauri-specta, Muster `HistoryUpdatePayload`), Auto-Hide 5 s, Undo → Command `remove_learned_correction(id)`. i18n: `learnedCorrection.toast`, `learnedCorrection.undo` (en+de zuerst).

**Implementiert (Abweichungen vom Plan):**

- **Feld-Events statt Key-Hook** (Risiko #5 aufgelöst): Die Session hängt einen `AXObserver` an das gepinnte Feld und wacht bei jeder Wertänderung (und beim Zerstören des Elements) auf, statt einen zweiten globalen Key-Listener neben `handy_keys` aufzuspannen — kein Input-Tap-Konflikt. Ein 2-s-Poll bleibt als Fallback (Apps ohne verlässliche AX-Notifications; kein Observer erstellbar → reines Polling wie zuvor). Damit wird auch die Korrektur-dann-sofort-Submit-Lücke geschlossen, die reines Polling nie sah. Wakeup-Bursts werden auf max. einen Read pro 100 ms zusammengefasst. Details im Modul-Doc von `session.rs`.
- **Event-Struct** heißt `LearnedCorrectionEvent`, Wire-Name `learned-correction-event` (nicht `learned-correction`).
- **i18n-Namespace**: sämtliche Strings liegen unter `settings.advanced.learnedCorrections.*` (inkl. `trialMode`, `aggressiveness`, `window`), nicht unter einem Top-Level `learnedCorrection.*`.

## Settings

- `learn_corrections_enabled: bool` (Default **false**, Opt-in)
- `learn_corrections_window_secs: u32` (Default 45)
- `learn_corrections_aggressiveness: Conservative|Balanced|Aggressive` (mappt auf Gate-Schwellen)
- `learn_corrections_log_only: bool` (Dry-Run-Soak)
- `learned_corrections: Vec<LearnedCorrection>`

Review-UI `LearnedCorrections.tsx` neben CustomWords/TextRules: Master-Switch, Fenster/Aggressivität, Tabelle (Paar, Count, auto/manual-Badge, Enable-Toggle, Delete, „+ manuell hinzufügen").

## Commit-Plan (je einzeln droppbar, `feat:`)

- **A — `feat: learned-corrections store + deterministic apply stage`** — Store, Settings, Apply-Call im Funnel, Commands, Review-UI mit manuellem Add/Delete, en+de. **Ohne AX/Session/Toast** — sofort nützliches zweites Wörterbuch (exakt statt fuzzy). Dep: `similar`.
- **B — `feat: macOS AX focused-field reader + post-paste learning session`** — ax_reader (+Stubs), session, `begin_session`-Call, Trigger-Wiring, Gates live, `log_only`-Dry-Run. Dep: `axuielement`.
- **C — `feat: learned-correction toast with undo`** — Toast-Fenster, Event, Undo-Command, i18n.
- **D — `feat: learning aggressiveness settings + docs`** — Gate-Feintuning, Phonetik (`rphonetic`), REQUIREMENTS.md-Update (🔭 → 🍴). Optional Commit E: restliche Locales (Muster 2af5ebd).

## Teststrategie

- **Unit (table-driven, differ.rs):** saubere Substitution → lernt; Insert/Delete pur → verworfen; Case/Punktuation-only → verworfen; Levenshtein zu groß → verworfen; Allerweltswort → verworfen, Eigenname → lernt; DE: `Muller`→`Müller`, Komposita, ß; Phonetik-Gate in Conservative.
- **Apply-Tests:** längste-zuerst, nur Wortgrenzen, disabled übersprungen.
- **Dry-Run-Soak:** `log_only=true` einige Tage — Pipeline läuft voll, loggt nur `would-learn: X → Y` (passt zum REQUIREMENTS-Schritt „Alltag ein paar Tage").
- **Manuelles E2E pro App der Spike-Matrix:** Phrase mit bekannt-verhörbarem Eigennamen diktieren → ein Wort fixen → Toast + Eintrag im Review-UI → erneut diktieren → Auto-Korrektur greift.
- **Edge Cases:** mehrere Pastes im Fenster (Session pro Paste, keyed auf Timestamp+PID); Select-all-Delete (Gate verwirft); Fokuswechsel mid-window (finaler Re-Read des Original-Elements, dann Session zu); IME/Dead-Keys (Debounce, erst nach Settle lesen); Secure Field beim Re-Read → Abbruch.

## Risiken & offene Punkte

1. **Electron/Web-Lücke** (Hauptrisiko) → akzeptieren, per-App stumm degradieren, dokumentieren; Phase A trägt allein. Entscheidet der Spike.
2. **`axuielement` v0.9 Reife** auf Darwin 25.3 → Spike gated; Fallback objc2-FFI oder nur Phase A.
3. **Snapshot vs. Paste-Methode** — clipboard.rs:597 hängt Trailing-Space an; `PasteMethod::Direct` tippt zeichenweise → exakt den an `utils::paste` übergebenen String snapshotten bzw. im Differ trimmen.
4. **Wörterbuch-Vergiftung** → Default off, erst `log_only`, Conservative-Default, jedes Auto-Paar 1 Klick von Undo/Delete entfernt.
5. **rdev-Listener vs. `handy_keys`-Hook** — rdev ist direkte Dep (Cargo.toml:50), aber im Code ungenutzt; ob ein zweiter globaler Listener neben dem Shortcut-Backend koexistiert, zu Beginn von Phase B verifizieren (bevorzugt an bestehende Key-Events von handy_keys anhängen statt zweiter Hook).
6. **Deutsche Phonetik** — Double Metaphone ist englisch-zentriert → Kölner Phonetik (in `rphonetic` enthalten) bei `de`, Gate nie hart blockierend.

## Zentrale Dateien

- `src-tauri/src/actions.rs` (:764 Paste-Callsite — Capture-Hook)
- `src-tauri/src/managers/transcription.rs` (:1596–1619 Funnel — Apply-Hook)
- `src-tauri/src/settings.rs` (:375 Felder / :867 Defaults)
- `src-tauri/src/shortcut/mod.rs` (:730 ff. Command-Muster)
- `src-tauri/src/overlay.rs` (NSPanel/Event-Blaupause für Toast-Fenster)

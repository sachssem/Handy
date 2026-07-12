# voice-flow — Gesammelte Anforderungen & Implementationswege

> Stand: 2026-07-06 · Basis: Handy (v0.9.0) mit Parakeet TDT 0.6B v3, läuft lokal auf M2/16GB.
> Legende: ✅ in Handy vorhanden · ⚙️ per Konfiguration/Prompt lösbar (kein Code) · 🍴 Fork-Feature (eigener Code) · 🔭 später/aufwendig

## Kern (erfüllt)

- ✅ **Komplett offline** — lokale STT (Parakeet), kein Cloud-Zwang.
  _Verifikation:_ WLAN-aus-Test; dauerhaft per LuLu (Outbound-Firewall, macOS-Bordmittel können kein Outbound-Blocking).
- ✅ **Deutsch + Englisch mit Auto-Detect** inkl. Code-Switching — Parakeet v3 (25 Sprachen, Auto-LID).
- ✅🍴 **Sprach-Allowlist für Auto-Detect** (gegen LID-Ausreißer wie Kyrillisch bei kurzen englischen Phrasen): erkannte Sprache außerhalb der Liste → einmaliger Retry mit auf die Primärsprache gepinnter Konditionierung (`language_allowlist`; UI unter der Sprachauswahl, nur bei „Auto").
- ✅ **Geringer RAM-Verbrauch** — Parakeet ~1,2 GB geladen, Handy-Kern nur zweistellige MB; mit Glättungs-LLM gesamt ~4,5–5 GB (Ziel <10 GB locker erfüllt). Modelle werden bei Leerlauf entladen.
- ✅ **Push-to-talk systemweit** (+ Toggle-Modus), Text landet in aktiver App, konfigurierbare Paste-Methode, Auto-Submit.
- ✅ **History** — SQLite, mit Audio, Roh- + geglättete Version.
- ✅ **Personal Dictionary (manuell)** — Custom Words mit Fuzzy-Korrektur (Levenshtein).
- ✅ **Basis-Interpunktion** — ASR setzt Punkt/Komma/Fragezeichen aus Sprechmelodie.
- ✅ **Füllwort-Entfernung** — sprachspezifische Listen (DE: ähm/äh …), anpassbar.

## Glättung / Textqualität

- ⚙️ **LLM-Glättung** (Sprech-/Sprachfehler bereinigen, Selbstkorrekturen auflösen, Listen formatieren):
  Ollama + Qwen3 4B Q4 (~3 GB) als Custom-Provider (`localhost:11434/v1`) in Handy, deutscher Cleanup-Prompt, Temp ~0.1. Leichtgewichtige Mac-Alternative: **Apple Intelligence on-device** (0 extra RAM, keine Windows-Portabilität). Preis: +1–2 s Latenz → A/B-testen.
- ✅🍴 **Zahlen als Ziffern** (ITN: "Fünfhundertneununddreißig" → "539"):
  Umgesetzt im Fork-Regel-Layer (`src-tauri/src/text_rules/itn.rs`), per Settings-Toggle. Benchmark: mit Qwen3-ASR + Rules 100 % Format-Genauigkeit auf Zahlen/ITN-Fällen.
- ✅🍴 **Gesprochene Satzzeichen-Befehle** ("Bindestrich" → "-", "neue Zeile" → Umbruch):
  Umgesetzt als deterministischer Text-Rules-Layer (`src-tauri/src/text_rules/`, Settings → Advanced → Text Rules). Absorbiert ASR-Prosodie-Satzzeichen, Spacing-Policies (AttachLeft/AttachRight/Glue/Standalone), Built-ins einzeln abschaltbar, eigene Regeln möglich.
- ✅🍴 **Zweisprachige Befehls-Keywords** (englische Keywords im deutschen Diktat: "dash"/"dot"/"slash" → `-` `.` `/`):
  Umgesetzt in derselben Built-in-Tabelle (DE+EN gemischt, inkl. "question mark", "open/close paren" etc.).
- ⚙️ **Technisches Diktieren** (File-Paths, camelCase/snake_case, CLI-Befehle):
  Kontextabhängig → Domäne des LLM-Cleanups; Prompt explizit auf Paths/Code-Syntax ausrichten. Ergänzend Custom Words mit Tool-/Projektnamen füttern.

## Fork-Features (bauen, wenn konkreter Bedarf bestätigt)

- ✅ **Regel-Layer in der Pipeline** — geliefert, siehe oben (Einfügepunkt wurde `post_process_transcription_text` in `transcription.rs`, nicht `actions.rs`).
- ✅🍴 **Auto-Lernen aus Korrekturen** (Wispr-Flow-Loop: manuelle Nachkorrekturen im Textfeld beobachten → Dictionary automatisch füttern) — im Fork umgesetzt: macOS-Lern-Loop (Accessibility-Read nach Paste mit Element-Pinning, Wort-Diff, Anti-Vergiftungs-Gates inkl. Phonetik), Review-UI und „Gelernt"-Toast mit Undo, Trial-Mode. Design/Details: `docs/design/auto-learn-corrections.md`.
- 🔭 **Hands-free-Modus** (VAD-getriggert statt Taste) — in Handy offen (Issue #147); Silero VAD ist als Stille-Filter schon an Bord, Trigger-Modus wäre Fork-Arbeit.
- 🔭 **Per-App-Profile** (Profil je Ziel-App: anderer Prompt/Modell/Verhalten, wie VoiceInk "Power Mode") — Handy hat nichts dergleichen; Frontmost-App-Detection + Settings-Erweiterung.
- 🔭 **Snippets** (Sprach-Trigger → gespeicherter Text) — nicht vorhanden; als Ersetzungs-Sonderfall im Regel-Layer machbar.
- 🔭 **Analytics/Insights** (Wörter, WPM, pro-App-Nutzung) — History-Daten sind da (SQLite), Auswertung/UI fehlt.

## Rahmenbedingungen

- **Basis-Strategie:** Handy-Fork (MIT, Rebranding nötig, Feature-Freeze upstream → Fork ist der vorgesehene Weg). Maintenance: GitHub-Fork, eigener Branch, `main` als Upstream-Spiegel, Updates per Rebase; eigene Änderungen additiv halten (neue Module statt Umbauten).
- **macOS-first, Windows-portabel** — Handy läuft bereits auf beiden; Apple-Intelligence-Pfad wäre die einzige Mac-only-Abhängigkeit.
- **Modularität** — STT-Engine per Katalog tauschbar (65 Modelle), LLM-Provider OpenAI-kompatibel austauschbar (lokal/Cloud), Erweiterungen als eigene Pipeline-Stufen.

## Benchmark-Suite & Modell-Entscheid (Stand 2026-07-08)

Eigene Benchmark-Suite im Fork (`bench/`, `handy-bench` Binary): 15 Fälle × 3 Varianten
(normal/schnell/noise) mit eigener Stimme, Metriken WER (vs. Gesagtem) und
Format-Genauigkeit (vs. Soll-Output), Corpus/Ergebnisse lokal (gitignoriert).
Ergebnisse des ersten Matrix-Laufs:

| Modell (Kandidat)     | WER normal                                                               | WER schnell | WER noise | E2E mit Rules |
| --------------------- | ------------------------------------------------------------------------ | ----------- | --------- | ------------- |
| **Qwen3-ASR 1.7B Q8** | **0.104**                                                                | **0.066**   | **0.219** | **0.93**      |
| Parakeet TDT v3       | 0.179                                                                    | 0.173       | 0.343     | 0.84          |
| Whisper Turbo (de)    | 0.303                                                                    | 0.227       | 0.323     | 0.88          |
| Canary 1B v2 (de)     | entgleist auf OOD-Input (Wiederholungsschleifen, ungefragte Übersetzung) |             |           | 0.81          |

→ **Standard-Modell: Qwen3-ASR 1.7B** (im Fork-Katalog, Sprache auf Auto; natives
DE/EN-Code-Switching, keine Kyrillisch-Ausrutscher mehr). RTF ~0.15 auf M2/Metal.
Text-Rules bringen auf jedem Modell +9…+20 Punkte Format-Genauigkeit.
**Denoising (DTLN, inkl. Mix-Back) gemessen und verworfen:** kein Setting schlägt
die Baseline unter Noise, ohne klare Sprache zu verschlechtern (`--denoise`-Flag
bleibt im Bench-Harness für künftige Kandidaten wie GTCRN).

**Recording-Limit-Probe (`handy-bench probe-limit`):** manche Modelle brechen ihre
Ausgabe ab langer Audiodauer ab (Qwen3-ASR ~45 s am transcribe-cpp-Token-Cap). Der
Probe verkettet 2–3 Corpus-Samples im Round-Robin zu immer längerem Audio, misst pro
Länge das Wortzahl-Verhältnis und findet per Verdopplungs- plus Feinsuche die reale
Decke. Beispiel: `handy-bench probe-limit --models qwen=...gguf --max-secs 120`. Er
gibt eine `recommended_max_recording_ms` samt einfügefertigem Match-Arm aus — jede
Neubewertung eines Modells soll ihn laufen lassen, um
`recording_limit_for_model_id` (`src-tauri/src/managers/model.rs`) zu füllen.

Gemessene Ergebnisse (Probe-Lauf 2026-07-12, auto-Language, Threshold 0.70):

| Modell          | Ergebnis                                             | Limit-Eintrag              |
| --------------- | ---------------------------------------------------- | -------------------------- |
| Parakeet TDT v3 | gesund bis 360 s (Ratio 0.91–1.00, keine Truncation) | genereller Default         |
| Qwen3-ASR 1.7B  | ~45 s (Hand-Messung; Modell z. Zt. nicht lokal)      | 45 s, per Probe bestätigen |

Modelle ohne gemessenen Eintrag bekommen einen generellen Default von **5 min**
(`DEFAULT_RECORDING_LIMIT`) — weit über jeder realen Diktatlänge, aber ein Netz
gegen stilles Abschneiden bei ungemessenen Modellen.

## Nächste Schritte (empfohlene Reihenfolge)

1. Alltag mit Qwen3-ASR + Text-Rules; Fehlschläge per `handy-bench export-history` in den Corpus übernehmen
2. Apple-Intelligence-Cleanup-Prompt im Alltag A/B-testen (siehe `llm-cleanup-prompt.md`)
3. `feat/correction-learning` reviewen und mergen (Auto-Lernen, Alleinstellungs-Feature)
4. Bei Bedarf: Hands-free-Modus, Per-App-Profile, Snippets (🔭-Liste oben)

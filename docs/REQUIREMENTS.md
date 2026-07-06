# voice-flow — Gesammelte Anforderungen & Implementationswege

> Stand: 2026-07-06 · Basis: Handy (v0.9.0) mit Parakeet TDT 0.6B v3, läuft lokal auf M2/16GB.
> Legende: ✅ in Handy vorhanden · ⚙️ per Konfiguration/Prompt lösbar (kein Code) · 🍴 Fork-Feature (eigener Code) · 🔭 später/aufwendig

## Kern (erfüllt)

- ✅ **Komplett offline** — lokale STT (Parakeet), kein Cloud-Zwang.
  _Verifikation:_ WLAN-aus-Test; dauerhaft per LuLu (Outbound-Firewall, macOS-Bordmittel können kein Outbound-Blocking).
- ✅ **Deutsch + Englisch mit Auto-Detect** inkl. Code-Switching — Parakeet v3 (25 Sprachen, Auto-LID).
- ✅ **Geringer RAM-Verbrauch** — Parakeet ~1,2 GB geladen, Handy-Kern nur zweistellige MB; mit Glättungs-LLM gesamt ~4,5–5 GB (Ziel <10 GB locker erfüllt). Modelle werden bei Leerlauf entladen.
- ✅ **Push-to-talk systemweit** (+ Toggle-Modus), Text landet in aktiver App, konfigurierbare Paste-Methode, Auto-Submit.
- ✅ **History** — SQLite, mit Audio, Roh- + geglättete Version.
- ✅ **Personal Dictionary (manuell)** — Custom Words mit Fuzzy-Korrektur (Levenshtein).
- ✅ **Basis-Interpunktion** — ASR setzt Punkt/Komma/Fragezeichen aus Sprechmelodie.
- ✅ **Füllwort-Entfernung** — sprachspezifische Listen (DE: ähm/äh …), anpassbar.

## Glättung / Textqualität

- ⚙️ **LLM-Glättung** (Sprech-/Sprachfehler bereinigen, Selbstkorrekturen auflösen, Listen formatieren):
  Ollama + Qwen3 4B Q4 (~3 GB) als Custom-Provider (`localhost:11434/v1`) in Handy, deutscher Cleanup-Prompt, Temp ~0.1. Leichtgewichtige Mac-Alternative: **Apple Intelligence on-device** (0 extra RAM, keine Windows-Portabilität). Preis: +1–2 s Latenz → A/B-testen.
- ⚙️ **Zahlen als Ziffern** (ITN: "Fünfhundertneununddreißig" → "539"):
  Erst testen, was Parakeet nativ ausgibt. Falls Wörter: Prompt-Zeile im LLM-Cleanup; deterministisch später als ITN-Regeln im Fork-Regel-Layer.
- ⚙️→🍴 **Gesprochene Satzzeichen-Befehle** ("Bindestrich" → "-", "neue Zeile" → Umbruch):
  Kurzfristig per LLM-Prompt. Sauber: deterministischer Ersetzungs-Layer im Fork (Substitutions-Map, ~0 RAM, 0 Latenz) — billigstes Fork-Feature.
- ⚙️→🍴 **Zweisprachige Befehls-Keywords** (englische Keywords im deutschen Diktat: "dash"/"dot"/"slash" → `-` `.` `/`):
  Erkennung macht das ASR (Code-Switching); Umsetzung = dieselbe Ersetzungstabelle, DE+EN gemischt gepflegt.
- ⚙️ **Technisches Diktieren** (File-Paths, camelCase/snake_case, CLI-Befehle):
  Kontextabhängig → Domäne des LLM-Cleanups; Prompt explizit auf Paths/Code-Syntax ausrichten. Ergänzend Custom Words mit Tool-/Projektnamen füttern.

## Fork-Features (bauen, wenn konkreter Bedarf bestätigt)

- 🍴 **Regel-Layer in der Pipeline** — deterministische Transform-Stufe vor/statt LLM: Befehlstabelle (DE+EN), ITN-Regeln, eigene Ersetzungen. Einfügepunkt: lineare Pipeline in `actions.rs`; wenige dutzend Zeilen Rust.
- 🔭 **Auto-Lernen aus Korrekturen** (Wispr-Flow-Loop: manuelle Nachkorrekturen im Textfeld beobachten → Dictionary automatisch füttern):
  Gibt es in Handy NICHT und in keiner OSS-Alternative. Anspruchsvoll: Zielfeld nach Paste per Accessibility-API beobachten, Edits gegen eingefügten Text diffen, Korrektur ableiten. Stärkster Kandidat für ein echtes Alleinstellungs-Feature des Forks.
- 🔭 **Hands-free-Modus** (VAD-getriggert statt Taste) — in Handy offen (Issue #147); Silero VAD ist als Stille-Filter schon an Bord, Trigger-Modus wäre Fork-Arbeit.
- 🔭 **Per-App-Profile** (Profil je Ziel-App: anderer Prompt/Modell/Verhalten, wie VoiceInk "Power Mode") — Handy hat nichts dergleichen; Frontmost-App-Detection + Settings-Erweiterung.
- 🔭 **Snippets** (Sprach-Trigger → gespeicherter Text) — nicht vorhanden; als Ersetzungs-Sonderfall im Regel-Layer machbar.
- 🔭 **Analytics/Insights** (Wörter, WPM, pro-App-Nutzung) — History-Daten sind da (SQLite), Auswertung/UI fehlt.

## Rahmenbedingungen

- **Basis-Strategie:** Handy-Fork (MIT, Rebranding nötig, Feature-Freeze upstream → Fork ist der vorgesehene Weg). Maintenance: GitHub-Fork, eigener Branch, `main` als Upstream-Spiegel, Updates per Rebase; eigene Änderungen additiv halten (neue Module statt Umbauten).
- **macOS-first, Windows-portabel** — Handy läuft bereits auf beiden; Apple-Intelligence-Pfad wäre die einzige Mac-only-Abhängigkeit.
- **Modularität** — STT-Engine per Katalog tauschbar (65 Modelle), LLM-Provider OpenAI-kompatibel austauschbar (lokal/Cloud), Erweiterungen als eigene Pipeline-Stufen.

## Nächste Schritte (empfohlene Reihenfolge)

1. Parakeet-Zahlenausgabe + Whisper-turbo-Gegenprobe testen (Stage-1-Abschluss)
2. Ollama + Qwen3 4B aufsetzen, deutschen Technik-Cleanup-Prompt bauen (deckt ⚙️-Punkte ab) — A/B mit/ohne LLM
3. Apple Intelligence als LLM-Provider gegentesten (Footprint-Minimum)
4. Alltag ein paar Tage; welche 🍴/🔭-Punkte wirklich fehlen → dann Fork (Stage 2)

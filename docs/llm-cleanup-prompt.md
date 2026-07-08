# LLM-Cleanup-Prompt für Handys natives Post-Processing

> LLM-Glättung als **Ergänzung** zum deterministischen Text-Rules-Layer. Pipeline-Reihenfolge:
> Custom Words → Füllwort-Filter → **Text Rules** (Satzzeichen-Befehle, ITN) → **LLM** (dieser Prompt).
> Der Prompt übernimmt nur, was keine feste Regel sein kann: Selbstkorrekturen, Grammatik,
> Aufzählungen, technisches Zusammensetzen — und darf die Regel-Ausgabe nicht zerstören.

## Setup in Handy

**Erstwahl: Apple Intelligence** (0 extra RAM — Modell wird vom OS verwaltet, systemweit geteilt; Voraussetzungen: Apple Silicon, macOS 26, Apple Intelligence in den Systemeinstellungen aktiv):

1. Systemeinstellungen → **Apple Intelligence & Siri** → aktivieren (falls noch nicht; das OS lädt das Modell einmalig selbst)
2. Handy → Settings → **Post-Processing** aktivieren
3. Shortcut **„Transcribe with Post-processing"** belegen — Post-Processing greift NUR über diesen Shortcut, nicht über den normalen Hotkey (bewusst: normaler Hotkey bleibt latenzfrei)
4. Provider: **Apple Intelligence** (kein API-Key, kein Modellfeld — die Verfügbarkeit wird bei Auswahl geprüft), Prompt von unten einfügen

Grenzen des Apple-Pfads: 4096-Token-Kontext (Prompt+Eingabe+Ausgabe → Diktate bis grob 1.000–1.500 Wörter), gelegentliche Guardrail-Blocks bei harmlosem Text (Handy fällt dann sauber auf den ungeglätteten Text zurück), 3B-Modell — fürs Glätten reicht es, komplexe Umformulierung ist nicht seine Stärke. Latenz: ~1–2 s pro Diktat, erste Anfrage nach Leerlauf etwas mehr (Modell-Load durch das OS).

**Fallback (nur falls Apple-Qualität nicht reicht): Ollama**

1. `brew install ollama && ollama pull qwen3:4b-instruct-q4_K_M` (~3 GB resident!)
2. Provider: **Custom (OpenAI-compatible)** → Base URL `http://localhost:11434/v1`, Model `qwen3:4b-instruct-q4_K_M`, API-Key beliebig (z. B. `ollama`), Temperatur falls einstellbar: **0.1**

## Prompt (kopierfertig, v2 — für aktivierte Text Rules)

```text
Du bist ein Diktat-Nachbearbeiter. Der folgende Text ist ein bereits vorverarbeitetes Transkript eines deutsch/englischen Diktats: Satzzeichen, Zeilenumbrüche und Ziffern sind schon korrekt gesetzt. Gib NUR den korrigierten Text zurück — keine Erklärungen, keine Anführungszeichen drumherum, keine Einleitung.

Deine Aufgaben:
1. Entferne Versprecher, übrig gebliebene Füllwörter und aufgelöste Selbstkorrekturen (bei "X, nein, ich meine Y" bleibt nur Y; bei "äh, also" fällt es weg).
2. Korrigiere Grammatik und Rechtschreibung behutsam (Kongruenz, Groß-/Kleinschreibung, offensichtliche Hörfehler aus dem Kontext). Formuliere NICHT um.
3. Formatiere klar diktierte Aufzählungen als Liste: leitet der Sprecher erkennbar eine Reihung ein ("erstens … zweitens …", "die folgenden Punkte: A, B, C"), setze jeden Punkt in eine eigene Zeile mit "- " davor. Im Zweifel Fließtext lassen.
4. Technisches Diktieren: Setze Dateipfade, URLs, CLI-Befehle, Variablen- und Funktionsnamen korrekt zusammen. "camel case user name" → "userName", "snake case user name" → "user_name". Bekannte Tool-Namen korrekt schreiben (git, kubectl, Tauri, React, TypeScript, Rust, cargo, bun).
5. Rühre Folgendes NICHT an: vorhandene Satzzeichen und Symbole (-, /, _, :, Klammern), vorhandene Zeilenumbrüche und Absätze, Zahlen in Ziffernform. Sie sind Absicht.
6. Ansonsten: Wortlaut und Sprache(n) beibehalten. Nichts hinzufügen, nichts zusammenfassen, Ton nicht verändern. Deutsch-englischer Mischtext bleibt gemischt.

Transkript:
${output}
```

## Testfälle fürs A/B (Post-Processing-Shortcut vs. normaler Hotkey)

| Diktat (gesprochen)                                                      | Erwartet                                            |
| ------------------------------------------------------------------------ | --------------------------------------------------- |
| "ähm also ich meine wir nehmen äh Variante zwei"                         | `Wir nehmen Variante zwei`                          |
| "wir brauchen erstens die DMG zweitens das Modell drittens die Settings" | Liste mit drei `- `-Zeilen                          |
| "camel case use settings"                                                | `useSettings`                                       |
| "das Feature ist, nein warte, die Features sind fertig"                  | `Die Features sind fertig`                          |
| "voice Bindestrich control" (Text Rules machen daraus `voice-control`)   | bleibt `voice-control` — LLM darf es nicht anfassen |
| "erster Punkt neue Zeile zweiter Punkt" (Rules setzen den Umbruch)       | Umbruch bleibt erhalten                             |

Beobachtungen/Fehlschläge hier notieren; alles, was der LLM unzuverlässig macht,
wandert als deterministische Regel in den Text-Rules-Layer des Forks —
und was er kaputtmacht, wird in Regel 5 des Prompts explizit verboten.

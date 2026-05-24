# AGENTS.md — DictaFlow

Guía para agentes (y humanos) que trabajen en este repo. Léela completa antes de tocar
firma de código, permisos macOS, el bundle `.app`, o el selector de Ollama.

---

## Qué es DictaFlow

App de dictado **local-first** para macOS, construida con **Tauri (Rust + WebView)**.
Flujo de uso:

1. Mantienes una hotkey global (default `Option + Space`).
2. Hablas; el audio se graba con el micrófono.
3. Se transcribe **localmente** con **Whisper** (`whisper-rs`, modelo `ggml-large-v3-turbo.bin`).
4. Se limpia el texto: primero reglas (`cleanup/rules.rs`), luego un modelo **Ollama** local
   (default `gemma3:1b`).
5. Se **pega** el resultado en el cursor activo simulando `Cmd+V` (`enigo`).

Es un **menu-bar agent**: `LSUIElement = true`, no aparece en el Dock.

### Permisos macOS que necesita
- **Micrófono** (`NSMicrophoneUsageDescription`) — para grabar.
- **Accessibility** (`kTCCServiceAccessibility`) — para pegar (`Cmd+V` sintético vía `enigo`).
  Si Accessibility no valida, **el paste falla en silencio** (no hay error visible).

---

## Layout del repo

```
dist/                    UI del WebView (HTML/JS plano, sin framework)
  settings.html          Ventana de Settings (hotkey, modelo Ollama, login, permisos, HUD)
  hud.html               Pastilla flotante que muestra estado de grabación
  about.html             About + logo Invisionnaire
src-tauri/
  src/
    main.rs              Entry point + comandos Tauri (invoke_handler)
    asr.rs               Backend Whisper (transcripción)
    audio.rs             Captura de micrófono
    paste.rs             Paste sintético (Cmd+V vía enigo, corre en main thread)
    cleanup/
      mod.rs             Hybrid cleaner (reglas + Ollama), setter de modelo
      rules.rs           Limpieza por reglas (sin LLM)
      ollama.rs          Cliente Ollama: /api/generate y /api/tags (listado de modelos)
    settings.rs          Persistencia de settings (JSON)
    login_item.rs        LaunchAgent para "abrir al login" (macOS)
    state.rs             Estado compartido de la app
  tauri.conf.json        Config Tauri/bundle. identifier = com.invisionnaire.dictaflow
```

### Comandos Tauri expuestos (`generate_handler!`)
`get_hotkey`, `set_hotkey`, `get_ollama_model`, `list_ollama_models`, `set_ollama_model`,
`get_launch_at_login`, `set_launch_at_login`, `launch_at_login_supported`, `close_settings`, `hud_edit_start`,
`hud_edit_save`, `hud_edit_cancel`, `check_accessibility`, `prompt_accessibility`,
`open_accessibility_settings`, `open_microphone_settings`, `open_input_monitoring_settings`,
`check_input_monitoring`, `get_whisper_model_status`.

### Settings persistidos
`~/Library/Application Support/DictaFlow/settings.json`
```json
{
  "input_device": null,
  "hotkey": { "code": "Space", "modifiers": ["Alt"] },
  "hud_position": null,
  "ollama_model": "gemma3:1b",
  "launch_at_login": false
}
```

---

## Verificación / build

Desde `src-tauri/`:
```bash
cargo fmt --check
cargo check
cargo clippy --bins -- -D warnings   # CI trata warnings como errores
cargo test --bins                    # ~21 tests
cargo tauri build                    # produce el .app
```
El bundle queda en `src-tauri/target/release/bundle/macos/DictaFlow.app`.

Listar modelos Ollama instalados:
```bash
curl -s http://127.0.0.1:11434/api/tags | jq -r '.models[].name'
```

---

## ⚠️ LA TRAMPA GRANDE: firma ad-hoc + TCC (permisos) en macOS

Esto causó una regresión larga. **Entiéndelo antes de tocar el `.app` o los permisos.**

### El problema de fondo
- **No hay certificado Apple Developer** instalado en esta máquina:
  `security find-identity -v -p codesigning` → `0 valid identities found`.
- Por eso el `.app` se firma **solo ad-hoc** (`Signature=adhoc`).
- macOS TCC (el sistema de permisos) graba el grant de Accessibility **pineado al `cdhash`
  exacto del binario**. El requirement guardado se ve así:
  ```
  cdhash H"896f6269c102e5fbe40b3c40ad15a8fff7c82b78"
  ```
- **Cada `cargo tauri build` o cada reemplazo del `.app` cambia el `cdhash`.** En cuanto el
  cdhash cambia, el grant existente **deja de validar**: `AXIsProcessTrusted()` devuelve
  `false` aunque la base de datos diga `auth_value=2` (concedido). Resultado: la UI muestra
  el permiso "encendido" pero **el paste falla en silencio**.

### Síntoma típico reportado por el usuario
> "Cuando intento usar el mic me manda a Accessibility a dar permisos, doy permiso, pero no funciona."

El **micrófono sí funciona** (su grant es por bundle id, más tolerante). Lo que falla es el
**paste** al final del flujo, que necesita Accessibility. Y togglear el permiso "no sirve"
porque se vuelve a grabar contra un cdhash que no coincide con el binario que corre.

### Agravante: colisiones en LaunchServices
Múltiples copias registradas con el **mismo bundle id** `com.invisionnaire.dictaflow`
(builds viejos en `/Applications`, build de `target/release`, y DMGs montados/desmontados)
confunden a macOS sobre cuál binario "es" DictaFlow. Al togglear en System Settings, puede
re-grabar el requirement apuntando al binario equivocado.

### Cómo diagnosticar (sin adivinar)

```bash
# 1) Identidad del binario que CORRE (cdhash, identifier, si Info.plist está bound)
codesign -dv --verbose=4 /Applications/DictaFlow.app 2>&1 | grep -E "Identifier|CDHash|Signature|Info.plist"

# 2) ¿Qué tiene grabado TCC? (Accessibility vive en la DB del SISTEMA, no la de usuario)
sqlite3 "/Library/Application Support/com.apple.TCC/TCC.db" \
  "SELECT service,client,client_type,auth_value FROM access WHERE client='com.invisionnaire.dictaflow';"
# auth_value 2 = concedido, 0 = denegado. (Micrófono vive en la DB de usuario:
#   ~/Library/Application Support/com.apple.TCC/TCC.db)

# 3) ¿A qué cdhash está PINEADO el grant? (la prueba definitiva)
sqlite3 "/Library/Application Support/com.apple.TCC/TCC.db" \
  "SELECT writefile('/tmp/df.bin', csreq) FROM access \
   WHERE service='kTCCServiceAccessibility' AND client='com.invisionnaire.dictaflow';"
csreq -r /tmp/df.bin -t        # imprime: cdhash H"..."
# Si ese cdhash != el CDHash del paso 1 → ESE es el bug. El grant no valida.

# 4) Copias fantasma reclamando el mismo bundle id
LSREG="/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister"
"$LSREG" -dump | awk '/^[[:space:]]*path:/{p=$0} /com\.invisionnaire\.dictaflow/{if(p){print p;p=""}}' | sort -u
```

### El fix (reversible, NO toca el binario de /Applications)

```bash
# 1) Borrar el grant muerto pineado al cdhash viejo
tccutil reset Accessibility com.invisionnaire.dictaflow

# 2) Des-registrar copias fantasma de LaunchServices (no borra archivos)
LSREG="/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister"
"$LSREG" -u "<repo>/src-tauri/target/release/bundle/macos/DictaFlow.app"   # build de dev
"$LSREG" -f "/Applications/DictaFlow.app"   # re-registra la copia instalada

# 3) Quit + relaunch (NO reemplazar el bundle, solo reabrir)
osascript -e 'tell application "DictaFlow" to quit'; sleep 1
open "/Applications/DictaFlow.app"

# 4) El usuario (manual): Settings → Permissions → "Request Accessibility" →
#    System Settings → activar DictaFlow. macOS graba un csreq nuevo con el cdhash actual.
#    Verificar con el paso 3 del diagnóstico que el cdhash guardado AHORA sí coincide.
```

Señal de éxito: el botón **"Re-check"** en Settings pasa a **"Granted ✓"** y el paste pega.

### Si reemplazas la app en /Applications tras un rebuild (flujo verificado)

`cargo tauri build` produce un bundle **linker-signed** con un `Identifier` autogenerado
(`dictaflow-<hash>`) ≠ `CFBundleIdentifier`, e `Info.plist=not bound`. Si lo copias así a
`/Applications`, el grant de Accessibility queda inconsistente y togglear no pega bien.
**Re-firma el bundle con identidad consistente antes de usarlo.** Flujo completo que funciona:

```bash
SRC="<repo>/src-tauri/target/release/bundle/macos/DictaFlow.app"
DST="/Applications/DictaFlow.app"

# 1) Backup de la app actual (cambiar el binario revoca Accessibility)
cp -R "$DST" "/Applications/DictaFlow.app.backup-$(date +%Y%m%d-%H%M%S)"

# 2) Cerrar la instancia corriendo (hay single-instance lock: dos copias se estorban)
osascript -e 'tell application "DictaFlow" to quit'; sleep 1
pkill -f "/Applications/DictaFlow.app/Contents/MacOS/dictaflow"; sleep 1

# 3) Reemplazar el bundle
rm -rf "$DST"; cp -R "$SRC" "$DST"

# 4) RE-FIRMAR con identifier = bundle id y sellando el Info.plist (clave para TCC)
codesign --force --deep -s - --identifier com.invisionnaire.dictaflow "$DST"
codesign --verify --deep --strict "$DST"   # debe decir "satisfies its Designated Requirement"
# Tras esto: Identifier=com.invisionnaire.dictaflow, "Info.plist entries=18",
#            "Sealed Resources" presente. Esa identidad consistente es lo que hace
#            que el grant de Accessibility PEGUE y se MANTENGA.

# 5) Resetear el grant viejo (pineado a un cdhash que ya no existe) + limpiar LaunchServices
tccutil reset Accessibility com.invisionnaire.dictaflow
LSREG="/System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/LaunchServices.framework/Versions/A/Support/lsregister"
"$LSREG" -u "$SRC"; "$LSREG" -f "$DST"

# 6) Abrir y (manual) Settings → Request Accessibility → activar en System Settings
open "$DST"
```

Nota: `cargo tauri build` puede fallar en el paso del `.dmg` (`bundle_dmg.sh`) por un
volumen DMG montado o permisos de Finder. **No importa** — el `.app` se genera antes; solo
necesitas el `.app`.

### Por qué la firma consistente importa (defecto de fondo)
TCC graba el grant con un `csreq`. Si el `Identifier` de firma ≠ `CFBundleIdentifier` y el
`Info.plist` no está sellado, la identidad que macOS computa para el proceso es inestable y
el grant no valida aunque la UI lo muestre encendido. Re-firmar con
`--identifier com.invisionnaire.dictaflow` + `--deep` (sella el Info.plist) deja una
identidad coherente y el grant pega. La build "rota" histórica (`.broken-…`) en realidad
tenía la firma bien; "se rompió" solo por el cambio de cdhash + dos copias corriendo a la vez.

### Arreglo de fondo (Developer ID)

La fragilidad desaparece con **Developer ID Application** + install que no pisa la firma:

```bash
./scripts/setup-developer-id.sh          # CSR + instrucciones (Account Holder, una vez)
./scripts/import-developer-id-cert.sh ~/Downloads/developerID_application.cer
./scripts/setup-notary-credentials.sh    # opcional, para notarización
./scripts/install-macos.sh               # firma con Developer ID, ya no re-firma ad-hoc
```

Con Developer ID, TCC usa `identifier "com.invisionnaire.dictaflow" and anchor apple generic ...`
y el grant **sobrevive rebuilds**. Sin Developer ID, `install-macos.sh` usa Apple Development
o ad-hoc y avisa que Accessibility puede resetearse en cada rebuild.

---

## 🚫 Reglas duras (constraints del proyecto)

1. **No reemplazar `/Applications/DictaFlow.app`** sin permiso explícito del usuario.
   Reemplazarlo cambia el cdhash y revoca Accessibility → rompe la app usable.
   Para probar builds nuevos: abrir desde `target/release/bundle/macos/DictaFlow.app`,
   nunca copiar a `/Applications` sin autorización.

2. **Confirmar que la app funciona antes de hacer push/PR.** Reemplazar el `.app` o
   recompilar puede romper Accessibility (ver sección de TCC), así que valida el paste
   localmente antes de subir cambios que toquen el bundle o los permisos.

3. **`gemma3:1b` es el modelo default integrado de DictaFlow.** Trátalo como válido SIEMPRE,
   aunque `/api/tags` no lo liste. La lista de Ollama es **sugerencia, no autoridad**:
   el selector de Settings es un `<input list=...>` libre (no un `<select>`), y el backend
   (`set_ollama_model` en `main.rs`) **no** rechaza modelos no listados. Si Ollama no
   responde, el usuario igual debe poder escribir y guardar un modelo.

4. **No matar la app en ejecución sin avisar.** Si necesitas reiniciarla, confírmalo primero.

5. **Al restaurar/reemplazar la app, conserva un backup** (p. ej.
   `cp -R /Applications/DictaFlow.app /Applications/DictaFlow.app.backup-<fecha>`) antes de
   sobrescribir, porque cambiar el binario revoca Accessibility y a veces hay que volver
   a la copia anterior.

---

## Skills útiles para este repo
- `diagnose` — para regresiones con loop de feedback claro (así se resolvió el bug de TCC).
- `macos-dev-pro` / `macos-ui-pro` — firma, LaunchAgent, LSUIElement, TCC/Accessibility,
  comportamiento de menu bar.
- `swiftui-pro` — solo si se añade UI nativa (actualmente la UI es WebView/HTML).

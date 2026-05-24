# Auditoría de seguridad — DictaFlow 1.0.0

**Fecha:** 2026-05-24 · **Commit:** `main` @ `d5b40b3` · **Método:** revisión de código/config + `cargo audit`.

**Veredicto:** la seguridad del **código** está lista para producción. El único impedimento real
para publicar es de **modelo de plataforma** (App Sandbox), no de código.

---

## 🔴 CRÍTICO — Bloqueador de Mac App Store: App Sandbox vs. el core de la app

`Entitlements.appstore.plist` activa `com.apple.security.app-sandbox` (obligatorio para MAS).
Pero el core de DictaFlow —leer el campo enfocado y **pegar en OTRAS apps**— usa:

- `AXUIElementCreateApplication(pid)` + `AXUIElementSetAttributeValue` — lee/escribe el texto de otras apps.
- `CGEvent…post_to_pid` — inyecta `⌘V` en otra app.
- `NSWorkspace.frontmostApplication` / `runningApplicationWithProcessIdentifier` — inspecciona otras apps.

El App Sandbox **bloquea el control/inyección entre procesos**, y la revisión de App Store
**rechaza** apps no-asistivas que controlan otras apps por Accessibility/CGEvent. Las
entitlements MAS actuales no incluyen ninguna excepción de automation/AX (solo `app-sandbox`,
`allow-jit`, `device.audio-input`, `network.client`) — o sea, en un build sandboxed esas
llamadas fallan o son rechazadas.

**Decisión del proyecto:** distribuir por **Developer ID Application + notarización** (path
NO-sandbox: `Entitlements.plist` + `hardenedRuntime` + tooling de firma ya presente), **no**
por el Mac App Store. La versión completa **no** se limita con el sandbox. El App Store solo
sería viable degradando a "copia al clipboard, pega tú" — que es otro producto.

---

## 🟡 MEDIO — Hardening (no bloqueante)

1. **`get_transcription_config` devuelve la API key completa al webview** por IPC. Es local y
   de bajo riesgo, pero lo ideal sería devolverla **enmascarada** (`gsk_…Ez77`) + un booleano
   `has_key`, y solo aceptar escrituras.
2. **Keychain + firma ad-hoc** (operacional, no es vuln): con builds ad-hoc la key no persiste
   entre recompilaciones (cada cdhash es "otra app" para el Keychain). Con Developer ID sí.

---

## 🟢 Lo que está bien (sólido)

- **API key en Keychain** (`secrets.rs`), **nunca se loguea**, migrada y borrada de `settings.json`.
- **Sin XSS**: `history.js` y `settings.js` escapan todo el texto transcrito/aprendido con `escapeHtml`.
- **Red solo HTTPS** (api.groq.com / api.openai.com), vía `reqwest` en Rust — no por el webview.
- **CSP restrictiva** (sin script/connect remoto) y **capabilities de Tauri mínimas**.
- **Privacy policy** (`docs/PRIVACY-POLICY.md`) completa y honesta: declara local-por-defecto,
  que el cloud manda audio directo al proveedor con la key del usuario, key en Keychain, sin
  analytics ni tracking.
- **Info.plist**: `NSMicrophoneUsageDescription` claro y honesto, `ITSAppUsesNonExemptEncryption=false`
  (export compliance correcto, solo HTTPS estándar), `LSUIElement`.
- **Hardened runtime** activado; las entitlements MAS quitan `allow-unsigned-executable-memory`.

---

## 🟢 Dependencias — `cargo audit`

- **0 vulnerabilidades.** 588 dependencias escaneadas contra 1098 advisories. Exit code 0.
- **17 warnings**, todos el mismo: `gtk-rs GTK3 bindings - no longer maintained`
  (RUSTSEC-2024-04xx) → `atk`, `gdk`, `gtk`, `gdkx11`, `gdkwayland`, etc.
- Son los bindings de **GTK / webkit2gtk = backend de webview de Linux**, gated por
  `cfg(target_os = "linux")`. En macOS Tauri usa **WKWebView** → **no se compilan en el binario
  de Mac**. Cero impacto en el target de macOS.

---

## Resumen

| Área | Estado |
|---|---|
| Secretos (API key) | 🟢 Keychain, nunca logueada, migrada de plaintext |
| XSS / webview | 🟢 Todo escapado (`escapeHtml`) |
| Red | 🟢 Solo HTTPS, vía reqwest (no webview) |
| CSP / capabilities | 🟢 Restrictivas y mínimas |
| Privacy policy + Info.plist | 🟢 Completos y honestos |
| Hardened runtime / entitlements | 🟢 Bien configurados |
| Dependencias (CVEs) | 🟢 0 vulnerabilidades |
| Hardening API key al frontend | 🟡 Devuelve key completa al webview (menor) |
| **Sandbox vs. pegar en otras apps** | 🔴 Bloqueador de App Store |

**Conclusión:** código listo para producción. Camino de publicación recomendado:
**Developer ID Application + notarización** (fuera del App Store), por el sandbox vs. pegar en otras apps.

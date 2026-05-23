# Compilar DictaFlow para Windows

Guía para compilar los instaladores de Windows (`.msi` / `.exe`) desde cero.

> **Clave:** la GPU NVIDIA solo hace falta para **correr** el build CUDA, **no para compilarlo**.
> `nvcc` compila sin GPU presente. Por eso un runner de CI sin GPU (o una VM sin
> passthrough) puede generar el binario perfectamente.

Requisitos de plataforma: **Windows 10/11 x86_64**. (No funciona en Windows ARM —
CUDA no existe para ARM.)

---

## Setup del toolchain (una sola vez)

La forma más rápida es con `winget` (incluido en Win10/11). Abre **PowerShell como
administrador**:

```powershell
# 1) Rust (toolchain MSVC) + componentes de C++ que Rust necesita
winget install --id Rustlang.Rustup -e

# 2) Visual Studio Build Tools 2022 — MSVC + Windows SDK (whisper.cpp y Rust MSVC lo requieren)
winget install --id Microsoft.VisualStudio.2022.BuildTools -e `
  --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"

# 3) CMake (whisper.cpp lo usa para su backend nativo)
winget install --id Kitware.CMake -e

# 4) CUDA Toolkit 12.x — provee nvcc + cuBLAS (para la feature `cuda`)
winget install --id Nvidia.CUDA -e

# 5) WebView2 Runtime (Tauri lo necesita; en Win11 suele venir preinstalado)
winget install --id Microsoft.EdgeWebView2Runtime -e
```

**Cierra y reabre la terminal** después de instalar (para refrescar el PATH y las
variables `CUDA_PATH`, `cargo`/`rustc`, `cl.exe`).

Luego, la Tauri CLI (se compila una vez, ~5-8 min):

```powershell
cargo install tauri-cli --version "^2.0" --locked
```

> NSIS y WiX (para empaquetar el `.exe`/`.msi`) los **descarga Tauri solo** la primera
> vez que corres `tauri build`. No hay que instalarlos a mano.

### Verificar que todo quedó

```powershell
rustc --version          # x86_64-pc-windows-msvc
nvcc --version           # CUDA 12.x
cmake --version
cargo tauri --version    # tauri-cli 2.x
```

---

## Compilar

```powershell
git clone https://github.com/Invisionnaire-LLC/DictaFlow.git
cd DictaFlow\src-tauri
cargo tauri build
```

Salida en:

```
src-tauri\target\release\bundle\msi\DictaFlow_<ver>_x64_en-US.msi
src-tauri\target\release\bundle\nsis\DictaFlow_<ver>_x64-setup.exe
```

La primera compilación tarda (whisper.cpp con CUDA compila para varias arquitecturas).
Builds siguientes son mucho más rápidas por el caché de cargo.

> El modelo de Whisper (~1.5 GB) **no** va en el instalador — se descarga solo la primera
> vez que el usuario abre la app.

---

## Build CPU (sin necesidad de GPU NVIDIA para correrlo)

El build de Windows por defecto exige CUDA (`src-tauri/Cargo.toml`):

```toml
[target.'cfg(target_os = "windows")'.dependencies]
whisper-rs = { version = "0.16", default-features = false, features = ["cuda"] }
```

Si quien lo va a **correr** no tiene GPU NVIDIA, cambia esa línea a un build CPU portátil:

```toml
[target.'cfg(target_os = "windows")'.dependencies]
whisper-rs = { version = "0.16", default-features = false }
```

Así no necesita CUDA Toolkit para compilar ni GPU para correr (transcribe más lento, en CPU).

---

## Usar una VM Windows de Proxmox como build box

Para **compilar** no hace falta passthrough de GPU — la VM solo necesita CPU + RAM + disco.

- **Recursos sugeridos:** 6+ vCPU, 16 GB RAM, 80+ GB disco (el CUDA Toolkit + artefactos
  de build ocupan bastante).
- **Acceso headless:** habilita OpenSSH Server o RDP en la VM para manejarla sin GUI.
- Instala el toolchain de arriba **una sola vez**; después cada build es solo
  `cargo tauri build`.
- **No toques las GPUs del host** ni el passthrough: compilar no las usa, así que tu stack
  de contenedores (ollama, comfyui, etc.) sigue intacto.

El passthrough de GPU a la VM **solo** sería necesario si quisieras *ejecutar/probar* el
app con aceleración CUDA dentro de esa misma VM — no para sacar el instalador.

---

## CI alternativo

El repo incluye `.github/workflows/windows-build.yml` (`workflow_dispatch`) que hace
exactamente esto en un runner Windows de GitHub (instala CUDA Toolkit, compila, sube el
`.msi`/`.exe` como artefacto). Útil si no quieres mantener una build box propia. Ojo: en
repos privados consume minutos de Actions (Windows = 2x).

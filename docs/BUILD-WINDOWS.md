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

## ⚠️ Known issue: el `.exe` CUDA falla por DLLs faltantes al arrancar

El build CUDA **enlaza dinámicamente** contra DLLs del runtime de cuBLAS que **no** vienen
con el driver de NVIDIA ni van dentro del `.exe`. En una máquina con GPU NVIDIA pero **sin
el CUDA Toolkit instalado**, el app arranca y truena buscando:

- `cublas64_12.dll`
- `cublasLt64_12.dll`

(`cudart` suele ir enlazado estático en whisper.cpp, por eso normalmente solo faltan esas
dos.) La **versión de las DLLs debe coincidir con la versión de CUDA con que se compiló**
(el workflow usa CUDA **12.5.1** → cuBLAS `12.5.3.2`).

### Fix rápido (dárselas al usuario)
Copiar las 2 DLLs **junto al `dictaflow.exe`** instalado (p. ej. `C:\Program Files\DictaFlow\`).
Se bajan oficiales del CDN de NVIDIA, sin instalar CUDA:

```bash
# nombre exacto del archive para una versión de CUDA dada:
curl -s https://developer.download.nvidia.com/compute/cuda/redist/redistrib_12.5.1.json \
  | python3 -c "import json,sys; print(json.load(sys.stdin)['libcublas']['windows-x86_64']['relative_path'])"
# descargar y extraer las 2 DLLs:
curl -L -o libcublas.zip \
  https://developer.download.nvidia.com/compute/cuda/redist/libcublas/windows-x86_64/libcublas-windows-x86_64-12.5.3.2-archive.zip
unzip -j libcublas.zip '*/bin/cublas64_12.dll' '*/bin/cublasLt64_12.dll' -d cuda-dlls
```

### Fix correcto (instalador self-contained)
Bundlear esas DLLs dentro del instalador para que el usuario solo necesite el **driver**
NVIDIA. En `src-tauri/tauri.conf.json`, añade las DLLs como recursos del bundle:

```jsonc
"bundle": {
  "resources": {
    "cuda/cublas64_12.dll": "./",
    "cuda/cublasLt64_12.dll": "./"
  }
}
```

…y en el workflow de Windows, un paso que copie las DLLs del CUDA Toolkit a `src-tauri/cuda/`
antes de `cargo tauri build`. Así quedan al lado del `.exe` dentro del paquete.

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

## CI alternativo

El repo incluye `.github/workflows/windows-build.yml` (`workflow_dispatch`) que hace
exactamente esto en un runner Windows (instala CUDA Toolkit, compila, sube el
`.msi`/`.exe` como artefacto). Útil si no quieres mantener una build box propia. Ojo: en
repos privados consume minutos de Actions (Windows = 2x).

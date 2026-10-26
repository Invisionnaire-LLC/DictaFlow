# DictaFlow on Linux

DictaFlow can run on Linux, but there are three practical differences from macOS:

1. Global hotkeys are compositor-dependent on Wayland.
2. Synthetic paste needs a Wayland-aware tool on compositors such as niri.
3. GPU acceleration uses the Linux `whisper-rs` backend selected at build time.

This file documents the known-good setup used on Arch/CachyOS with niri, an NVIDIA
GPU, and optional Groq cloud transcription.

## Build Requirements

Install the normal Rust/Tauri native dependencies plus Vulkan headers when building the
Linux GPU path:

```bash
sudo pacman -S --needed rust cmake clang pkgconf webkit2gtk-4.1 \
  libappindicator-gtk3 alsa-lib vulkan-headers glslang shaderc wl-clipboard wtype
```

`wl-copy` and `wtype` are recommended on Wayland: `wl-copy` writes the transcript to the
Wayland clipboard and `wtype` synthesizes `Ctrl+V` through the virtual-keyboard protocol.
Without them, DictaFlow may transcribe successfully but leave the result on the wrong
clipboard or fail to paste into the focused app.

From the repo:

```bash
cd src-tauri
cargo build
cargo run
```

## NVIDIA GPU Acceleration

The Linux build enables the `whisper-rs` `vulkan` feature. This avoids requiring the CUDA
Toolkit (`nvcc`) while still using NVIDIA GPUs through Vulkan.

Useful diagnostics:

```bash
vulkaninfo --summary
nvidia-smi
```

Expected startup log lines when Vulkan is working:

```text
ggml_vulkan: Found ... Vulkan devices
NVIDIA GeForce RTX ...
whisper_backend_init_gpu: using Vulkan0 backend
```

`nvidia-smi` should show `dictaflow` using roughly the model size in VRAM when local Whisper
is active. If using Groq or OpenAI cloud transcription, DictaFlow intentionally skips local
Whisper and no longer needs the GPU for transcription.

## Wayland and niri Hotkeys

Tauri's global shortcut plugin may report that a shortcut was registered while the compositor
never forwards the key event to the app. This is normal on some Wayland compositors.

For niri, use a compositor keybind that calls DictaFlow's local control socket instead of
relying on in-process global hotkeys.

Create a toggle helper:

```bash
mkdir -p ~/.local/bin
cat > ~/.local/bin/dictaflow-toggle <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

socket="${XDG_DATA_HOME:-$HOME/.local/share}/DictaFlow/control.sock"

if [[ ! -S "$socket" ]]; then
  setsid -f "$HOME/.local/bin/dictaflow" >/tmp/dictaflow-toggle-start.log 2>&1
  for _ in {1..30}; do
    [[ -S "$socket" ]] && break
    sleep 0.1
  done
fi

python3 - "$socket" <<'PY'
import socket
import sys

path = sys.argv[1]
client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
client.connect(path)
client.sendall(b"toggle")
client.shutdown(socket.SHUT_WR)
print(client.recv(256).decode("utf-8", "replace").strip())
PY
EOF
chmod +x ~/.local/bin/dictaflow-toggle
```

Then add a niri keybind, for example in `~/.config/niri/cfg/keybinds.kdl`:

```kdl
ALT+Z repeat=false hotkey-overlay-title="DictaFlow: toggle dictation" {
    spawn "/home/<user>/.local/bin/dictaflow-toggle";
}
```

Reload niri:

```bash
niri validate -c ~/.config/niri/config.kdl
niri msg action load-config-file
```

Usage becomes tap-to-toggle:

1. Press `Alt+Z` to start recording.
2. Speak.
3. Press `Alt+Z` again to stop, transcribe, and paste.

## Wayland Paste

On non-macOS platforms DictaFlow writes the transcript to the clipboard and sends a paste
shortcut. On Wayland/niri the `arboard` + `enigo` path can write to a different clipboard
or fail to synthesize `Ctrl+V`. When `WAYLAND_DISPLAY` is present, DictaFlow now tries:

```bash
wl-copy <transcript>
wtype -M ctrl -k v -m ctrl
```

If the log contains `paste via wl-copy+wtype`, the Wayland paste path was used.

If paste still does not appear in the target app, confirm that the target app is focused and
that niri allows virtual-keyboard input for `wtype` in your environment.

## Groq / OpenAI Cloud Transcription

Cloud backends do not use the local Whisper model. They require an API key and internet
access. On Linux, the API key is persisted in:

```text
~/.local/share/DictaFlow/settings.json
```

The relevant fields are:

```json
{
  "transcription_backend": "groq",
  "transcription_model": "whisper-large-v3",
  "transcription_api_key": "gsk_..."
}
```

Diagnostics:

```bash
tail -f ~/.local/share/DictaFlow/logs/app.log
```

Good Groq run:

```text
cloud transcription (groq/whisper-large-v3) ... in ...ms
pipeline ok
```

Missing key:

```text
transcription: missing API key for groq backend
```

The Settings window should save the key on Linux. If debugging a broken local setup, write the
same fields above into `settings.json`, then restart DictaFlow.

## Common Logs

These messages are expected and not fatal on many Linux desktops:

```text
jack server is not running or cannot be started
ALSA lib ... Cannot open device /dev/dsp
ollama warmup skipped ... Connection refused
```

The Ollama warning only means LLM cleanup is unavailable. DictaFlow falls back to rules-only
cleanup and dictation still works.

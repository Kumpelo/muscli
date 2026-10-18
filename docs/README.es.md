# muscli

[![Build](https://github.com/Kumpelo/muscli/actions/workflows/ci.yml/badge.svg)](https://github.com/Kumpelo/muscli/actions/workflows/ci.yml)
[![Licencia MIT](https://img.shields.io/badge/licencia-MIT-8fa8b8)](../LICENSE)

Reproductor de música local para Linux y Windows, con interfaz de terminal y
bibliotecas en carpetas, USB o SD. No requiere cuenta ni servicio de streaming.

[English](../README.md) · [Instalación](#instalación) · [Guía de reproducción](playback-and-widget.es.md)

![Biblioteca de álbumes de muscli](assets/muscli-library.png)

*muscli 0.2.0-beta.3 · Biblioteca de muestra · Paleta Steel Blue.*

## Funciones

- Álbumes, artistas, géneros, búsqueda, playlists y colas guardadas.
- Reproducción sin huecos mediante mpv o el motor de audio nativo.
- ReplayGain, ecualizador de ocho bandas y salida bit-perfect opcional.
- MIX en vivo, pausa gradual y letras sincronizadas sin conexión.
- Discord Rich Presence, MPRIS en Linux y controles multimedia en Windows.
- Temas de escritorio, interfaz en español e inglés e integración con Omarchy.

## Instalación

Beta actual: **0.2.0-beta.4**. Descarga un paquete desde
[Releases](https://github.com/Kumpelo/muscli/releases).
En Windows, ejecuta el instalador `.exe`, que incluye mpv y FFmpeg. En Linux,
instala las dependencias, extrae el archivo y coloca `muscli` en `~/.local/bin`.

| Distribución | Dependencias |
|---|---|
| Arch / Omarchy | `sudo pacman -S alsa-lib mpv ffmpeg` |
| Debian | `sudo apt install libasound2 mpv ffmpeg` |
| Ubuntu 24.04+ | `sudo apt install libasound2t64 mpv ffmpeg` |

Para compilar desde un checkout, instala Rust 1.90 o posterior y, en Linux,
los headers de desarrollo de ALSA. Después ejecuta:

```bash
cargo install --locked --root "$HOME/.local" --path .
```

La [guía de uso en inglés](usage.md#installation) contiene los comandos para
instalar el archivo descargado y configurar Omarchy. El instalador de Windows
no está firmado; SmartScreen puede pedir confirmación.

## Inicio rápido

```bash
muscli library add /ruta/a/Música
muscli library rescan
muscli
```

En Linux con Kitty, `muscli --large` abre la TUI en una ventana grande
con letra tamaño 12.

| Tecla | Acción |
|---|---|
| Flechas / `hjkl` | Navegar |
| Enter / Esc | Abrir o reproducir / volver |
| Space / `n` / `p` | Pausar o reanudar / siguiente / anterior |
| `/` / `x` | Buscar / menú de la canción |
| `a` / `s` / `r` | Añadir a la cola / shuffle / repetir |
| `+` / `-` | Volumen |
| `,` / `?` / `q` | Ajustes / ayuda / guardar y salir |

En Ajustes puedes elegir motor de audio, idioma, tema y preferencias de
reproducción. `muscli keys` muestra todos los atajos y `muscli doctor` revisa
la configuración.

## Documentación

- [Álbumes, MIX y widget](playback-and-widget.es.md): cola, pausa gradual e integración con Omarchy.
- [Guía de uso en inglés](usage.md): biblioteca, audio, letras y configuración.
- [Contribuciones](../CONTRIBUTING.md) · [Seguridad](../SECURITY.md) · [Avisos de terceros](../THIRD_PARTY_NOTICES.md)

muscli está en beta. La reproducción nativa sin huecos requiere formatos
compatibles entre pistas. Opus ya se decodifica de forma nativa; WavPack y
Monkey's Audio usan mpv como respaldo.

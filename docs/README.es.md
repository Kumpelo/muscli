# muscli

muscli es un reproductor local y rápido con una TUI estilo Spotify. Indexa SD,
pendrives y carpetas locales, mantiene las playlists cuando una unidad está
desconectada y reproduce mediante mpv o su backend de audio nativo. No usa
cuentas, streaming, telemetría ni servicios residentes.

![Vista de la biblioteca de muscli](assets/muscli-beta2.png)

## Plataformas

- Linux: MPRIS, detección de medios, Discord y configuración opcional de
  Omarchy.
- Windows 11 x64: la misma TUI, detección de unidades extraíbles, controles
  multimedia de Windows (SMTC), teclas multimedia y Discord.

El instalador de Windows incluye mpv y FFmpeg verificados. Consulta
[`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md) para ver versiones,
SHA-256, licencias y código fuente. Al principio el instalador no tendrá firma
comercial y SmartScreen puede pedir confirmación.

## Instalar en Linux

muscli necesita ALSA (`libasound.so.2`), mpv y FFmpeg. En sistemas basados en
Arch:

```bash
sudo pacman -S alsa-lib mpv ffmpeg
```

En Debian:

```bash
sudo apt install libasound2 mpv ffmpeg
```

En Ubuntu 24.04 o posterior:

```bash
sudo apt install libasound2t64 mpv ffmpeg
```

Descarga el archivo Linux desde el
[release v0.2.0-beta.2](https://github.com/Kumpelo/muscli/releases/tag/v0.2.0-beta.2)
e instala el binario para tu usuario:

```bash
tar -xzf muscli-v0.2.0-beta.2-linux-x86_64.tar.gz
install -Dm755 muscli-v0.2.0-beta.2-linux-x86_64/muscli "$HOME/.local/bin/muscli"
muscli --version
```

Comprueba que `$HOME/.local/bin` esté incluido en `PATH`.

## Omarchy

```bash
omarchy pkg add alsa-lib mpv ffmpeg
muscli setup omarchy
```

El setup sólo modifica archivos del usuario, crea respaldos con fecha, valida
Hyprland y habilita el widget multimedia oficial. `SUPER+SHIFT+ALT+M` abre la
ventana flotante y `Shift+Vol±` cambia sólo muscli. Para deshacer únicamente
los bloques intactos: `muscli setup omarchy --undo`.

## Compilar desde el código fuente

Instala los headers de desarrollo de ALSA y Rust 1.90 o posterior, clona el
repositorio y ejecuta:

```bash
cargo install --locked --root "$HOME/.local" --path .
```

## Instalar en Windows

Descarga `muscli-vX.Y.Z-windows-x86_64-setup.exe` desde Releases. Se instala
por usuario, aparece en Inicio y registra `muscli.exe` en App Paths. Windows
Terminal es la opción recomendada para mostrar portadas.

## Inicio rápido

```bash
muscli library add /ruta/a/Música
muscli library rescan
muscli doctor
muscli
```

Dentro de la aplicación usa las flechas o `hjkl` para moverte, Enter para
abrir o reproducir, Space para pausar, `/` para buscar, `,` para Ajustes, `?`
para la ayuda y `q` para guardar y salir.

## Biblioteca

```text
muscli library add RUTA
muscli library remove RUTA
muscli library list
muscli library rescan
muscli library prune
muscli library forget-positions
muscli library analyze-gain
muscli library write-gain --yes
muscli doctor
muscli playlist export NOMBRE playlist.m3u8
muscli playlist import playlist.m3u8
muscli summary --days 30
muscli devices
```

Se indexan FLAC, MP3, M4A/AAC/ALAC, Ogg, Opus, WAV, AIFF, WavPack y Monkey's
Audio. Las etiquetas y portadas se leen sin modificar el medio. Si extraes una
unidad, favoritos, playlists y colas se conservan y las canciones reaparecen
al reconectarla. Sólo `library write-gain --yes` modifica archivos de audio.

Incluye géneros, álbumes por artista, historial y continuación, playlists
inteligentes, búsqueda difusa, menú contextual, colas guardadas, ReplayGain,
Settings, ayuda de atajos y modo compacto.

## Backends de audio

mpv es el backend predeterminado. Configura `audio_backend = "native"` en
`config.toml` para decodificar y procesar el audio dentro de muscli. El backend
nativo incluye ReplayGain, ecualizador, limitador, resampling, dither, modo
bit-perfect y reproducción gapless cuando las pistas contiguas tienen el mismo
formato de stream. `muscli devices` muestra los dispositivos disponibles.

Opus, WavPack y Monkey's Audio se entregan automáticamente a mpv cuando el
backend nativo no puede decodificarlos.

## Discord

```text
muscli setup discord --large-image peter
```

muscli incluye su propio ID de aplicación de Discord, así que el usuario no
tiene que crear ni configurar una aplicación. No utiliza bot, token ni OAuth.
Publica canción, artista, álbum y progreso por IPC local. Puede elegir los assets `peter_metal`, `peter_dj` y `daft_punk`.
Nunca sube las portadas locales.

## Controles principales

| Tecla              | Acción                         |
| ------------------ | ------------------------------ |
| Flechas o `hjkl`   | Navegar                        |
| Enter / Esc        | Abrir o reproducir / volver    |
| Space, `n`, `p`    | Pausa, siguiente, anterior     |
| `/`, `x`, `f`, `a` | Buscar, menú, favorito, cola   |
| `s`, `r`, `+`, `-` | Aleatorio, repetir, volumen    |
| `m`, `,`, `?`      | Compacto, Settings, ayuda      |
| `Shift+J/K`, `d`   | Reordenar o quitar de la cola  |
| `C`, `S`, `L`      | Limpiar, guardar o cargar cola |
| `q`                | Guardar estado y salir         |

## Temas

La interfaz trae una paleta clara, y es la predeterminada tanto en Linux como
en Windows. Se cambia desde Ajustes (`,`), con las flechas izquierda y derecha
sobre la fila **Tema**, o con `theme` en `config.toml`:

| `theme`                              |                                                                             |
| ------------------------------------ | --------------------------------------------------------------------------- |
| `light`                              | fondo blanco, el predeterminado                                             |
| `dark`                               | la paleta del propio terminal                                               |
| `high-contrast`                      | negro sobre blanco, para salas iluminadas y proyectores                     |
| `nord`, `gruvbox`, `solarized-light` | paletas fijas, iguales en todas partes                                      |
| `system`                             | seguir al escritorio: el tema actual de Omarchy en Linux, claro en el resto |

Con `system` en Linux la paleta sigue a Omarchy en vivo: cambiar de tema allí
repinta muscli sin reiniciarlo. `muscli --theme dark` prueba una paleta durante
una ejecución sin tocar la configurada.

## Idioma

La interfaz sigue el idioma del sistema cuando lo reconoce, y usa inglés en
caso contrario. Se cambia desde Ajustes, con `language = "en"` o
`language = "es"` en `config.toml`, o con `muscli --lang es` para una sola
ejecución. Al cambiarlo en Ajustes se aplica al instante, sin reiniciar.

## Ajustes

`,` abre la vista de Ajustes. Izquierda y derecha ajustan un valor; Space y
Enter lo alternan o ejecutan la fila. Cubre tema e idioma, modo compacto,
portadas, paso de volumen, reproducción sin huecos, ReplayGain con su modo y
objetivo, cada banda del ecualizador, continuación e historial, autodetección
de unidades extraíbles, hilos de escaneo, el tamaño de la caché de portadas y
Discord Rich Presence, y puede lanzar un reescaneo o un análisis de ReplayGain.
Cada cambio se escribe en `config.toml` al momento; ese archivo se sigue
pudiendo editar a mano.

## Limitaciones de la beta

- El instalador de Windows no está firmado; SmartScreen puede pedir
  confirmación manual.
- El backend nativo usa mpv como fallback para Opus, WavPack y Monkey's Audio.
- El gapless nativo requiere el mismo formato de stream entre pistas; un cambio
  de sample rate abre un stream nuevo.
- Cambiar o desconectar un dispositivo de audio puede requerir reiniciar la
  reproducción.

Al reportar un problema, incluye plataforma, versión exacta, pasos para
reproducirlo y la salida de `muscli doctor`.

## Desarrollo

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release
```

El workflow de release puede crear un RC manual. Los tags `v*` crean un
prerelease borrador con artefactos Linux y Windows, SHA-256 y SBOM CycloneDX.

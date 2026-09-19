# muscli

muscli es un reproductor FLAC local, rápido y sin servicios residentes. Tiene
una TUI estilo Spotify, indexa SD, pendrives y carpetas locales, y usa mpv para
reproducción gapless sin convertir el audio.

## Plataformas

- Linux: MPRIS, detección de medios, Discord y configuración opcional de
  Omarchy.
- Windows 11 x64: la misma TUI, detección de unidades extraíbles, controles
  multimedia de Windows (SMTC), teclas multimedia y Discord.

El instalador de Windows incluye mpv y FFmpeg verificados. Consulta
[`THIRD_PARTY_NOTICES.md`](../THIRD_PARTY_NOTICES.md) para ver versiones,
SHA-256, licencias y código fuente. Al principio el instalador no tendrá firma
comercial y SmartScreen puede pedir confirmación.

## Instalar en Omarchy

```bash
omarchy pkg add mpv ffmpeg
cargo install --root "$HOME/.local" --path .
muscli setup omarchy
muscli
```

El setup sólo modifica archivos del usuario, crea respaldos con fecha, valida
Hyprland y habilita el widget multimedia oficial. `SUPER+SHIFT+ALT+M` abre la
ventana flotante y `Shift+Vol±` cambia sólo muscli. Para deshacer únicamente
los bloques intactos: `muscli setup omarchy --undo`.

## Instalar en Windows

Descarga `muscli-vX.Y.Z-windows-x86_64-setup.exe` desde Releases. Se instala
por usuario, aparece en Inicio y registra `muscli.exe` en App Paths. Windows
Terminal es la opción recomendada para mostrar portadas.

## Biblioteca

```text
muscli library add RUTA
muscli library remove RUTA
muscli library list
muscli library rescan
muscli library prune
muscli library analyze-gain
muscli doctor
```

Sólo se indexan `.flac`. Las etiquetas y portadas se leen sin modificar el
medio. Si extraes una unidad, favoritos, playlists y colas se conservan y las
canciones reaparecen al reconectarla.

Incluye géneros, álbumes por artista, historial y continuación, playlists
inteligentes, búsqueda difusa, menú contextual, colas guardadas, ReplayGain,
Settings, ayuda de atajos y modo compacto.

## Discord

```text
muscli setup discord --large-image peter
```

MusCLI incluye su propio ID de aplicación de Discord, así que el usuario no
tiene que crear ni configurar una aplicación. No utiliza bot, token ni OAuth.
Publica canción, artista, álbum y progreso por IPC local. Puede elegir los assets `peter_metal`, `peter_dj` y `daft_punk`.
Nunca sube las portadas locales.

## Controles principales

| Tecla | Acción |
|---|---|
| Flechas o `hjkl` | Navegar |
| Enter / Esc | Abrir o reproducir / volver |
| Space, `n`, `p` | Pausa, siguiente, anterior |
| `/`, `x`, `f`, `a` | Buscar, menú, favorito, cola |
| `s`, `r`, `+`, `-` | Aleatorio, repetir, volumen |
| `m`, `,`, `?` | Compacto, Settings, ayuda |
| `Shift+J/K`, `d` | Reordenar o quitar de la cola |
| `C`, `S`, `L` | Limpiar, guardar o cargar cola |
| `q` | Guardar estado y salir |

## Temas

La interfaz trae una paleta clara, y es la predeterminada tanto en Linux como
en Windows. Se cambia desde Ajustes (`,`), con las flechas izquierda y derecha
sobre la fila **Tema**, o con `theme` en `config.toml`:

| `theme` | |
|---|---|
| `light` | fondo blanco, el predeterminado |
| `dark` | la paleta del propio terminal |
| `high-contrast` | negro sobre blanco, para salas iluminadas y proyectores |
| `nord`, `gruvbox`, `solarized-light` | paletas fijas, iguales en todas partes |
| `system` | seguir al escritorio: el tema actual de Omarchy en Linux, claro en el resto |

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

## Desarrollo

```bash
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build --release
```

Los tags `v*` crean un release borrador con instaladores, SHA-256 y SBOM. El
primer prerelease previsto es `v0.2.0-beta.1`.

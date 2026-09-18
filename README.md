# muscli

**muscli** es un reproductor FLAC local, rápido y sin servicios residentes.
Indexa SD, pendrives y carpetas locales, muestra la biblioteca en una TUI y usa
mpv como motor de audio. Mientras está abierto publica MPRIS, por lo que
Omarchy muestra la canción, la portada y los controles en su barra.

## Requisitos

- Linux con una sesión D-Bus
- Rust 1.90 o posterior
- mpv
- Omarchy 4.x para la integración de escritorio

En Omarchy, mpv se instala con:

~~~bash
omarchy pkg add mpv
~~~

## Instalar

~~~bash
cargo install --root "$HOME/.local" --path .
muscli setup omarchy
muscli
~~~

Omarchy incluye `~/.local/bin` en el `PATH`. Si se omite `--root`, algunas
instalaciones de Cargo dejan el binario en `~/.cargo/bin`, que no siempre está
expuesto por la sesión.

El setup respalda `shell.json` y los archivos personales de Hyprland, instala
una entrada compacta de Kitty, habilita `omarchy.media` antes del reloj y añade:

- `SUPER+SHIFT+ALT+M`: abrir muscli compacto y flotante.
- `Shift+Vol+` / `Shift+Vol-`: cambiar sólo el volumen de muscli.

Las teclas de volumen sin Shift siguen controlando el sistema y Shift+Mute
sigue cambiando la salida. No se modifica `/usr/share/omarchy`. Para deshacer
sólo los bloques intactos registrados por muscli:

~~~bash
muscli setup omarchy --undo
~~~

## Discord Rich Presence

Discord o Vesktop pueden mostrar la canción, artista, álbum y progreso usando
su IPC local; no hace falta bot, token ni OAuth:

~~~bash
muscli setup discord APPLICATION_ID --large-image peter
~~~

`large-image` es el nombre de un asset subido en Rich Presence dentro del
Discord Developer Portal. La presencia se limpia al cerrar muscli. Las
portadas FLAC locales no se publican ni se suben a Internet.

Si también existen los assets `peter_metal` y `peter_dj`, muscli los selecciona
automáticamente según la etiqueta `GENRE` del FLAC. Metal usa `peter_metal`;
Electronic, Dance, House y estilos relacionados usan `peter_dj`. Los demás
géneros conservan el asset configurado con `--large-image`.

El artista Daft Punk tiene prioridad y usa el asset `daft_punk`.

## Biblioteca

Los volúmenes montados bajo /run/media/$USER y /media/$USER se detectan
automáticamente. También pueden guardarse carpetas:

~~~bash
muscli library add /ruta/a/Music
muscli library remove /ruta/a/Music
muscli library list
muscli library rescan
muscli library prune
muscli library analyze-gain
muscli doctor
~~~

El índice, la configuración y la caché siguen las rutas XDG. Los FLAC nunca se
modifican. Si un dispositivo se extrae, sus pistas permanecen en favoritos y
playlists como no disponibles hasta que vuelva a conectarse.

`prune` elimina pistas que ya no existen únicamente cuando su fuente está
conectada, limpia referencias de portada rotas y aplica el límite de caché.
Si el TUI está abierto, tanto `rescan` como `prune` se envían a esa instancia
para evitar competir por SQLite.

## Funciones avanzadas

- Inicio con “Seguir escuchando” e historial local.
- Navegación por géneros, artistas, álbumes/singles y canciones.
- Listas inteligentes editables con reglas ALL/ANY.
- Búsqueda difusa sin distinguir mayúsculas ni acentos.
- Menú contextual con `x` o clic derecho.
- Colas guardadas, reordenables y reutilizables.
- ReplayGain no destructivo analizado con FFmpeg EBU R128.
- Settings con `,`, ayuda completa con `?` y modo compacto con `m` o
  `muscli --compact`.

El volumen remoto sólo afecta a muscli:

~~~bash
muscli remote volume up
muscli remote volume down
muscli remote volume set 55
muscli remote mute-toggle
~~~

## Controles

| Tecla | Acción |
|---|---|
| Flechas, hjkl | Moverse por listas y por la cuadrícula de álbumes |
| Tab | Cambiar entre menú y contenido |
| Enter | Abrir artista/álbum o reproducir la canción seleccionada |
| Esc | Volver un nivel desde artista o álbum |
| Space | Reproducir/pausar |
| n / p | Siguiente/anterior |
| / | Buscar |
| x / clic derecho | Menú contextual |
| a | Añadir a la cola |
| f | Alternar favorito |
| c | Crear playlist |
| P | Añadir a playlist |
| s / r | Aleatorio/repetición |
| + / - | Volumen |
| m | Alternar modo compacto |
| , / ? | Settings / ayuda de atajos |
| Shift+J/K | Reordenar la cola |
| d / Delete | Quitar de la cola |
| C / S / L | Limpiar / guardar / cargar cola |
| q | Guardar estado y salir |

La interfaz también acepta rueda del ratón y clics en la navegación. Las
portadas se reducen a 512 px al entrar en la caché. Foot usa Sixel, Kitty y
Ghostty usan el protocolo Kitty, y otros terminales reciben bloques Unicode.
`muscli doctor` muestra el protocolo que realmente se eligió.

## Desarrollo

~~~bash
cargo check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
~~~

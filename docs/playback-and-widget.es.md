# Álbumes, MIX y widget de escritorio

## Álbumes y biblioteca

Iniciar desde Albums, el detalle de un álbum o un lanzamiento dentro de Artists
crea una sesión ALBUM explícita. Respeta el orden disco/pista y mantiene el
prefetch gapless aunque la preferencia general de gapless esté desactivada.
MIX y AutoDJ quedan suspendidos sin cambiar sus preferencias globales.

Play Next inserta la canción pedida y luego continúa la cola; toda esa sesión
conserva la protección ALBUM. Saltar, eliminar o reordenar pistas no la elimina.
Shuffle conserva su preferencia, pero no reordena una sesión de álbum. Abrir la
cola y reproducir otra de sus entradas conserva el contexto. La sesión restaurada
tras reiniciar también lo conserva. Iniciar una selección nueva desde Songs,
favoritos o playlists recupera los ajustes generales. Cargar una cola guardada
inicia una sesión genérica, conservando el estado de pausa.

MusicBrainz Release ID tiene prioridad para identificar lanzamientos. Sin él,
se usan fuente, carpeta de lanzamiento, título normalizado, año y Album Artist.
Las carpetas convencionales CD1/CD2 o Disc 1/Disc 2 se agrupan cuando concuerdan
con el número de disco. Las recopilaciones mantienen los artistas individuales;
un Album Artist explícito compartido tiene prioridad sobre Various Artists.
La inferencia sin Album Artist exige numeración inequívoca dentro de la carpeta.
Si los archivos están repartidos como `biblioteca/Artista/Lanzamiento/pista`,
pueden reunirse sin Album Artist ni Release ID cuando coinciden la fuente,
raíz de biblioteca, nombre de carpeta, título, año conocido y portada cacheada
por contenido. Los directorios deben corresponder a los artistas de las pistas,
y la numeración conjunta debe ser completa desde 1, sin huecos ni duplicados
por disco. Cualquier conflicto en esas señales conserva la separación. Esta
inferencia se reconstruye al cargar el índice existente: no exige reetiquetar
ni reescanear archivos cuyos datos ya están actualizados.

Ediciones distintas en carpetas distintas no se unen solo por título/año.

La agrupación se reconstruye al cargar la biblioteca, sin duplicar pistas ni
cambiar sus IDs. La primera actualización relee las etiquetas de las filas
antiguas incluso si el archivo no cambió; no invalida ReplayGain por sí sola.
Las migraciones de esquema son transaccionales. La base actual es versión 9;
una versión anterior del programa puede rechazarla. Mantener una copia de la
base antes de cambiar de versión permite volver atrás sin intentar degradarla.

Gapless nativo requiere formatos compatibles con la salida continua. Cambios de
frecuencia/canales o un prefetch tardío pueden requerir reapertura del dispositivo.
mpv conserva sus capacidades de gapless y no aplica el crossfade nativo. Bit-perfect
sigue evitando procesamiento de muestras. La comparación de muestras con salida
capturada no sustituye una prueba auditiva del dispositivo y archivos reales.

## Modos de MIX

En Settings, la fila MIX en vivo recorre OFF → CLASSIC → SMART → AUTO_DJ.
MIX: duración conserva una duración máxima de 0–12 segundos independientemente del
modo; activar un modo con duración cero propone seis segundos.

- **OFF:** sin solapamiento.
- **CLASSIC:** duración configurada, si el motor puede mezclar esos formatos.
- **SMART:** conserva la cola y solo solapa con evidencia suficiente. Exige BPM
  cercano con confianza alta, energía comparable y extremos sin silencios largos.
  Propone aproximadamente cuatro beats, con máximo de ocho segundos y el límite
  configurado. Esto es una heurística conservadora: no alinea beats ni demuestra
  compatibilidad armónica. Cuando faltan datos, usa gapless o el cambio normal.
- **AUTO_DJ:** aplica esa política y, cuando no quedan canciones elegidas por el
  usuario, añade una candidata compatible de la biblioteca. No reemplaza ni
  reordena las entradas existentes. Play Next conserva prioridad. No repite IDs
  que ya están en la cola de esa sesión; si no encuentra candidata, termina.
  Está suspendido en ALBUM, bit-perfect y modos repeat.

El análisis persistente mide hasta treinta segundos iniciales y dos extremos de
cuatro segundos, en un worker separado del audio. Guarda BPM/confianza, RMS,
silencios inicial/final y la huella del archivo. Su versión invalida solo la caché
MIX antigua; los fallos también se recuerdan hasta que cambie el archivo o se
solicite `muscli library analyze-mix --force`. No se estima tonalidad ni estructura
completa. No se aplican filtros DJ, pitch shifting ni time stretching en vivo.
ReplayGain se calcula con su análisis existente y se aplica por pista durante el
solapamiento y en el límite de decodificación.

## Presentación durante MIX

Inicio, progreso y final de MIX se programan contra frames consumidos por la
salida, incluyendo la latencia del DSP. La cola lógica permanece en la saliente
hasta el handover; título, artista, progreso, MPRIS y widget presentan la entrante
cuando ya es audible. El historial de la entrante continúa al adoptar la cola,
sin crear un segundo inicio por el mismo handover.

Seek y los saltos manuales completan primero la transición hacia la pista
presentada. Play/pause detiene ambos decks. Un cambio de duración solicitado
durante un solapamiento se aplica después de ese límite.

La portada anterior permanece hasta que termine la decodificación de la nueva.
Kitty y halfblocks pueden mostrar cinco pasos de mezcla, preparados en el worker
a 256 px, terminando con la portada de resolución normal. Sixel e iTerm2 hacen un
cambio estático. Esto evita animaciones de alta frecuencia y borrados completos
por cada frame. El detalle de un álbum sigue mostrando la selección del usuario.

## Widget opcional de Quickshell

Requiere una instancia de esta versión de muscli ejecutándose y Quickshell 0.3.2.
El widget no abre la TUI ni inicia otro motor. Todavía no existe un servicio de
reproducción headless: al cerrar el reproductor, el widget muestra desconexión.
La dependencia no afecta a la compilación de Rust ni al reproductor.

Desde la raíz del repositorio:

```sh
cargo build --release --locked
desktop/quickshell/launch --binary "$PWD/target/release/muscli" \
  --screen HDMI-A-1 --corner bottom-right --width 360 --margin 18
```

Cambiar `--screen` por el nombre que informa `hyprctl monitors`. Las esquinas
permitidas son top-left, top-right, bottom-left y bottom-right. Un monitor no
presente usa la primera pantalla. El launcher no instala paquetes ni modifica
Hyprland. Si falta Quickshell, muestra qué dependencia opcional se necesita.

Los controles usan MPRIS exclusivamente para muscli. La cola procede de
`muscli remote status --json`, una consulta de solo lectura al socket privado:
no abre SQLite ni crea configuración, no ejecuta comandos arbitrarios y nunca
inicia un reproductor. El estado contiene hasta diez próximas entradas; el widget
muestra cuatro. Interpola progreso localmente y consulta una vez por segundo.
La cola y sus cambios pueden tardar un segundo en reflejarse. La ausencia de
MPRIS deshabilita los controles sin afectar al motor.

Contrato y límites: [compact-widget.md](design/compact-widget.md).

## Verificación reproducible

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
cargo +1.90 check --all-targets --locked
cargo build --release --locked
```

Prueba opcional con compositor Wayland y Quickshell, sin motor de audio:

```sh
QT_QPA_PLATFORM=wayland MUSCLI_WIDGET_TEST_BUS=1 \
  dbus-run-session -- cargo test --test widget --locked -- --ignored --nocapture
```

Esta prueba usa un bus D-Bus privado, cola/portada sintéticas y el servidor local
real. Comprueba los cuatro controles, carga de portada, desconexión y reconexión.
No demuestra escucha real, ni soporte Windows, ni cierra BUG-005.

Para validar manualmente: activar CLASSIC y shuffle, iniciar un álbum continuo,
comprobar orden y uniones, insertar Play Next y reiniciar. Iniciar después Songs
para comprobar la recuperación de preferencias. Reescanear una recopilación y un
multidisco; comprobar artistas, orden y edición separada. Con SMART, probar pares
con y sin análisis; con AUTO_DJ, añadir Play Next antes de agotar la cola. En MIX,
comprobar portada, seek, pausa y saltos mientras ambas canciones son audibles.
Abrir el widget en cada monitor, operar sus controles y cerrar/reabrir muscli.
Mantener abierto BUG-005 hasta disponer de evidencia adicional del dispositivo.

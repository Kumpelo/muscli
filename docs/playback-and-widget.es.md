# Álbumes, MIX y widget de Omarchy

[Volver al README](README.es.md)

## Actualizaciones

En **Ajustes → Actualizaciones** puedes elegir **Desactivadas**, **Avisar
(desarrollo)** o **Automáticas**. Las compilaciones locales avisan por defecto;
las versiones publicadas usan el modo automático. El modo elegido se aplica al
próximo inicio. Cambiarlo cancela una actualización que siga preparándose.

La búsqueda se hace en segundo plano, como máximo una vez al día. En Linux, una
actualización automática queda instalada para el próximo inicio sin detener la
música. En Windows, el instalador queda preparado y se ejecuta al cerrar muscli.
También puedes usar **Buscar e instalar actualización** desde Ajustes o ejecutar:

```bash
muscli update --check  # consultar sin instalar
muscli update          # descargar, verificar e instalar
```

Se comprueba el SHA-256 antes de instalar. En Linux se conserva el ejecutable
anterior como `muscli.previous` junto a `muscli`. Tu música, letras y configuración
se mantienen. Las instalaciones gestionadas por un gestor de paquetes deben
actualizarse con ese gestor; los ejecutables de `target` se recompilan con Cargo.
La [guía de uso](usage.md#updates) detalla las ubicaciones compatibles.

## Álbumes y cola

Al reproducir desde Álbumes o desde el detalle de un lanzamiento, muscli respeta
el orden de disco y pista. Durante esa sesión suspende MIX y shuffle, conservando
las preferencias elegidas en Ajustes.

Añadir una canción al final o con **Reproducir siguiente**, quitar pistas o
reordenarlas convierte la sesión en una cola de canciones. MIX y shuffle vuelven
a estar disponibles. Shuffle reorganiza solo las canciones por escuchar;
**Reproducir siguiente** conserva la posición solicitada.

Una cola guardada se carga como cola de canciones. Al reiniciar, una sesión de
álbum mantiene su protección si sus pistas siguen perteneciendo al mismo
lanzamiento y conservan el orden.

La biblioteca usa el identificador de lanzamiento de MusicBrainz cuando existe.
En los demás casos combina las etiquetas y la ubicación de los archivos. Las
recopilaciones conservan los artistas de cada canción, y dos lanzamientos no se
fusionan solo por compartir título. Para un álbum multidisco, los números de
disco y pista permiten mantener la secuencia.

## MIX en vivo

En Ajustes, **MIX en vivo** permite elegir:

| Modo | Comportamiento |
|---|---|
| Desactivado | Cambia de pista sin solapamiento. |
| Clásico | Solapa las pistas durante la duración configurada. |
| Inteligente | Conserva la cola y mezcla cuando el análisis de tempo, energía y extremos permite una transición. |
| AutoDJ | Usa la misma selección de transiciones y puede añadir una canción compatible al terminar la cola elegida. |

**MIX: duración** fija el máximo entre 0 y 12 segundos. MIX requiere el motor
nativo, reproducción sin huecos activada, bit-perfect desactivado y formatos
compatibles entre las pistas y la salida. Si no se cumplen esas condiciones,
las canciones pasan de una a otra mediante la reproducción habitual.

AutoDJ respeta las canciones ya elegidas y la prioridad de **Reproducir
siguiente**. Se suspende durante una sesión de álbum, con bit-perfect o con la
repetición activada. El análisis estima compatibilidad; no sincroniza beats ni
ajusta el tono de las canciones.

El análisis se guarda localmente y se reutiliza para archivos sin cambios:

```bash
muscli library analyze-mix
muscli library mix-status
```

`analyze-mix --force` reemplaza los resultados guardados. Para desactivar el
análisis automático, usa `mix_analysis_enabled = false` en `config.toml`.

Durante una mezcla, el título, la portada y el progreso muestran la canción
entrante cuando empieza a oírse. Pausar detiene ambas canciones; buscar otra
posición o saltar de pista toma como referencia la canción presentada.

## Pausa gradual

**Pausa gradual** reduce el volumen antes de detener la reproducción. El valor
inicial es 300 ms y se puede ajustar entre 0 y 2000 ms; cero desactiva el efecto.
El volumen elegido se conserva. Reanudar o cambiar de pista cancela una pausa
pendiente. En bit-perfect la pausa es inmediata.

## Widget para Omarchy

El widget requiere muscli abierto y Quickshell 0.3.2. Muestra portada, canción,
progreso y hasta cuatro próximas canciones. Sus colores siguen el tema activo
de Omarchy y sus controles actúan únicamente sobre muscli.

Desde la raíz del repositorio:

```bash
cargo build --release --locked
desktop/quickshell/launch --binary "$PWD/target/release/muscli"
```

Para elegir monitor, esquina y tamaño:

```bash
desktop/quickshell/launch --binary "$PWD/target/release/muscli" \
  --screen HDMI-A-1 --corner bottom-right --width 360 --margin 18
```

Sustituye `HDMI-A-1` por el nombre que muestra `hyprctl monitors`. Las esquinas
admitidas son `top-left`, `top-right`, `bottom-left` y `bottom-right`. Si el
monitor no existe, se usa la primera pantalla.

El lanzador no instala paquetes ni cambia Hyprland. Al cerrar muscli, el widget
muestra la desconexión. Sin MPRIS, los controles quedan deshabilitados. La cola
puede tardar aproximadamente un segundo en reflejar sus cambios.

La [referencia del protocolo local](design/compact-widget.md) describe cómo el
widget consulta el estado del reproductor.

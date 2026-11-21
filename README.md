# salamanca-bus

Autobuses urbanos de Salamanca en tiempo real desde la terminal. El comando es `salbus`:
cada subcomando imprime JSON, y sin subcomando abre una interfaz de terminal con buscador de
paradas, llegadas en vivo y mapa de los autobuses. Sin cuenta ni clave.

> **No oficial.** Sin relación con Salamanca de Transportes ni con el Ayuntamiento. `salbus` hace
> las mismas peticiones anónimas que la web oficial hace desde el navegador. Esa API no está
> documentada y puede cambiar sin aviso.

## Instalar

Hace falta [Rust](https://rustup.rs).

```sh
cargo install --git https://github.com/Sn4pe/salamanca-bus
salbus --help
```

Desde una copia local, `cargo install --path .`.

## Uso

```sh
salbus                          # interfaz de terminal
salbus stops gran via           # buscar paradas por nombre o número
salbus arrivals 28              # próximos autobuses en la parada 28
salbus arrivals madroños -l 7   # por nombre, si solo casa una parada; solo la línea 7
salbus watch 28                 # una línea JSON cada vez que cambian las llegadas
salbus lines                    # líneas de la red
salbus positions -l 7           # dónde están ahora los autobuses de la línea 7
```

| Comando | Qué hace |
|---|---|
| `salbus stops [TEXTO]` | Paradas que casan, sin acentos ni mayúsculas. Usa la lista incluida; `--refresh` pide la actual |
| `salbus arrivals PARADA` | Próximos autobuses: línea, destino, hora prevista, minutos, distancia y posición |
| `salbus watch PARADA` | Lo mismo en JSON Lines, cada `--interval` segundos (30 por defecto, mínimo 10) |
| `salbus lines` | Líneas con su nombre |
| `salbus positions` | Posición, rumbo y velocidad de cada autobús en servicio |

La salida va a stdout como JSON. Los errores van a stderr como `{"error": "..."}` con código de
salida 1, o 3 si la web ha limitado o bloqueado las peticiones.

```sh
salbus arrivals 28 | jq -r '.arrivals[] | "\(.minutes) min  L\(.line)  \(.destination)"'
```

### Interfaz de terminal

Todo va con teclado o con ratón: clic en una pestaña, una parada, una tarjeta, una línea de la
leyenda o un atajo del pie; la rueda mueve la selección. `Tab` cambia de pestaña. Con 116 columnas
o más cada pestaña añade un panel lateral.

- **Paradas:** escribe para buscar. `Enter`, o un segundo clic sobre la parada elegida, la guarda
  y salta a Llegadas. Al lado, dónde cae la parada en la ciudad. Con el buscador vacío, un autobús
  llega a la parada al abrir la pestaña.
- **Llegadas:** una tarjeta por parada guardada, refrescada cada 30 s. La barra se llena según se
  acerca el autobús. Al lado, un mapa con los autobuses que vienen hacia la parada elegida.
  `↑` `↓` eligen parada, `d` la quita, `r` refresca ya.
- **Mapa:** calles, río, paradas y autobuses en vivo, refrescado cada 5 s. Cada autobús es el
  número de su línea, sobre el centro de la ciudad (las paradas más alejadas quedan fuera).
  La rueda acerca o aleja donde está el ratón, y arrastrar lo mueve; con teclado, `+` `-` para el
  zoom, `w` `a` `s` `d` para moverlo y `0` para volver a encuadrar. Al acercarse aparecen todas las
  calles, la flecha con el rumbo de cada autobús y, muy cerca, el número de cada parada.
  `↑` `↓`, o un clic en la leyenda, eligen línea: el mapa encuadra su recorrido entero y cada
  autobús añade su rumbo (un punto si está parado) y deja estela.
- **Paleta de comandos** (`ctrl+p`, o clic en el atajo del pie): escribe para filtrar. Cambia de
  pestaña, actualiza, repite la llegada del autobús, cambia de tema o de idioma y sale.

La interfaz está en español o en inglés. Al arrancar sigue `LC_ALL`, `LC_MESSAGES` o `LANG`, y si
no dicen nada, los idiomas de macOS; cualquier idioma que no sea español da inglés. Tiene modo
oscuro y claro: al arrancar sigue la apariencia de macOS, y en otros sistemas empieza en oscuro.
Lo que elijas en la paleta se recuerda. Las paradas, destinos y nombres de línea vienen de la API
en español, y los comandos JSON hablan siempre español.

Pinta su propio fondo, con el azul de la web oficial como color de marca. Cada línea tiene su
propio color, apagado para que no deslumbre, y lleva siempre su número. Se ve mejor con color de
24 bits (`COLORTERM=truecolor`); sin él cae a 256 colores. Usa caracteres braille, que cualquier
terminal moderno trae.

Las paradas guardadas viven en `~/.config/salbus/favourites.json`, y el idioma y el tema elegidos
en `settings.json` en la misma carpeta (o bajo `$XDG_CONFIG_HOME`).

## Lo que no hace

- **Horarios y rutas entre dos paradas.** La API solo da tiempo real. Los horarios están en el
  GTFS del [Punto de Acceso Nacional](https://nap.transportes.gob.es/), que pide una clave.
- **Retrasos.** El campo `aimed` no parece un horario fiable: en muestras del 20 nov 2026 casi todos
  los autobuses salían 1–11 minutos "adelantados" frente a él. Se publica tal cual, sin calcular
  retraso.

## Cuidado con la web

Las peticiones salen de una en una, con 2 s entre ellas, y con un User-Agent que dice quién es.
Ante un 429 espera y reintenta dos veces; ante un 403 deja de pedir durante 5 minutos. Ese ritmo
vale dentro de un mismo proceso: lanzar `salbus` en bucle desde un script se lo salta.

## Desarrollo

```sh
cargo test
```

Los datos de paradas incluidos (`data/stops.json`) son la respuesta de la API el 20 nov 2026.

El mapa base (`data/basemap.json`: río Tormes, vías principales y calles, simplificados) viene de
[OpenStreetMap](https://www.openstreetmap.org/copyright), © colaboradores de OpenStreetMap, bajo
licencia ODbL. Descargado con Overpass el 21 nov 2026; `tools/basemap.py` lo regenera.

## Licencia

El código es [MIT](LICENSE). Los datos del mapa base son de OpenStreetMap, bajo ODbL (ver arriba).

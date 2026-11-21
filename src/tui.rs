//! Terminal UI: stop search, live arrivals and live bus map, by keyboard or mouse.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};
use std::{fs, io, thread};

use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Line as Segment, Points};
use ratatui::widgets::{Block, Clear, List, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use serde::{Deserialize, Serialize};

use crate::api::{self, Arrival, Client, Stop, Vehicle};

/// The official map refreshes at this pace.
const POSITIONS_EVERY: Duration = Duration::from_secs(5);
const ARRIVALS_EVERY: Duration = Duration::from_secs(30);
const ARRIVALS_SHOWN: usize = 6;
const TRAIL_POINTS: usize = 24;
/// Arrival rows keep their destination at this width; the side panel only fits beyond it.
const MAIN_WIDTH: u16 = 78;
const WIDE: u16 = MAIN_WIDTH + 38;
/// The map's line legend needs less.
const LEGEND_FROM: u16 = 100;

const WORDMARK: [&str; 3] = [
    "█▀▀▀ █▀▀█ █    █▀▀▄ █  █ █▀▀▀",
    "▀▀▀█ █▀▀█ █    █▀▀▄ █  █ ▀▀▀█",
    "▄▄▄█ █  █ █▄▄▄ █▄▄▀ █▄▄█ ▄▄▄█",
];
/// Pixel art, two pixels per cell, palette in `sprite_colour`; the bus faces the stop sign.
const BUS: [&str; 14] = [
    ".........DDDDDDDDDDDD...................",
    "..BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB..",
    ".BBBBBBBBBBBBBBBBBBBBBBBBSAASAASASASBBB.",
    ".BBBBBBBBBBBBBBBBBBBBBBBBSASASAASAASBBB.",
    ".BWWWWBWWWWWBddiddBWWWWWBWWWWWBddiddBWW.",
    ".BWWWWBWWWWWBddiddBWWWWWBWWWWWBddiddBWW.",
    ".BWWWWBWWWWWBddiddBWWWWWBWWWWWBddiddBWW.",
    ".BWWWWBWWWWWBddiddBWWWWWBWWWWWBddiddBWW.",
    ".LLLLLLLLLLLLddiddLLLLLLLLLLLLLddiddLLL.",
    ".TBBBBBBBBBBBddiddBBBBBBBBBBBBBddiddBBA.",
    ".BBBBBBBKKBBBBBBBBBBBBBBBBBBKKBBBBBBBBB.",
    ".DDDDDDKHHKDDDDDDDDDDDDDDDKHHKDDDDDDDDD.",
    ".......KHHK...............KHHK..........",
    "........KK.................KK...........",
];
const STOP_SIGN: [&str; 14] = [
    ".....", "FFFFF", "FBBBF", "FBBBF", "FFFFF", "..P..", "..P..", "..P..", "..P..", "..P..",
    "..P..", "..P..", "..P..", "..P..",
];
const BUS_WIDTH: i32 = 40;
/// The bus, a gap, the sign.
const SCENE_WIDTH: i32 = BUS_WIDTH + 2 + 5;
const DRIVE: Duration = Duration::from_millis(1800);
const DOORS_OPEN: Duration = Duration::from_millis(2300);

/// Own background, so it looks the same on any terminal theme; blues from the operator's site.
struct Theme {
    bg: Color,
    surface: Color,
    panel: Color,
    field: Color,
    fg: Color,
    muted: Color,
    primary: Color,
    accent: Color,
    success: Color,
    warning: Color,
    error: Color,
    river: Color,
    road: Color,
    road_minor: Color,
    stop: Color,
}

static TRUECOLOR: LazyLock<bool> = LazyLock::new(|| {
    std::env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit"))
});
static LIGHT: AtomicBool = AtomicBool::new(false);
static ENGLISH: AtomicBool = AtomicBool::new(false);

fn tr(es: &'static str, en: &'static str) -> &'static str {
    if ENGLISH.load(Relaxed) { en } else { es }
}

fn ago(at: Instant) -> String {
    let secs = at.elapsed().as_secs();
    if ENGLISH.load(Relaxed) {
        format!("{secs} s ago")
    } else {
        format!("hace {secs} s")
    }
}

static DARK_THEME: LazyLock<Theme> = LazyLock::new(|| Theme {
    bg: rgb(0x0c111d),
    surface: rgb(0x141b2c),
    panel: rgb(0x1e2941),
    field: rgb(0x1a2b4b),
    fg: rgb(0xe4e9f2),
    muted: rgb(0x8d97ab),
    primary: rgb(0x3f9de3), // the site's button blue, a touch softer
    accent: rgb(0x9dcff5),
    success: rgb(0x8fc48a),
    warning: rgb(0xe0b25c),
    error: rgb(0xe0675c),
    river: rgb(0x2f6ea8),
    road: rgb(0x3b4459),
    road_minor: rgb(0x252d3f),
    stop: rgb(0x65708a),
});

static LIGHT_THEME: LazyLock<Theme> = LazyLock::new(|| Theme {
    bg: rgb(0xf4f7fb),
    surface: rgb(0xe9eef6),
    panel: rgb(0xd6e0ee),
    // Not lighter: in 256 colours it would land on the same grey as `panel`.
    field: rgb(0xcfdcef),
    fg: rgb(0x1b2333),
    muted: rgb(0x5f6b80),
    primary: rgb(0x1a73c2),
    accent: rgb(0x0d4f8f),
    success: rgb(0x2e6b34),
    warning: rgb(0x8f5300),
    error: rgb(0xb3261e),
    river: rgb(0x5a9bd8),
    road: rgb(0xb3bccb),
    road_minor: rgb(0xd3dae5),
    stop: rgb(0x8a94a8),
});

fn theme() -> &'static Theme {
    if LIGHT.load(Relaxed) {
        &LIGHT_THEME
    } else {
        &DARK_THEME
    }
}

fn rgb(hex: u32) -> Color {
    colour(hex, *TRUECOLOR)
}

/// 24-bit colour where the terminal says it has it, else the nearest of the 256.
fn colour(hex: u32, truecolor: bool) -> Color {
    let [_, r, g, b] = hex.to_be_bytes();
    if truecolor {
        return Color::Rgb(r, g, b);
    }
    // The 256 palette has a 6x6x6 cube and a 24-step grey ramp: take whichever lands closer.
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let step = |v: u8| (0..6).min_by_key(|&i| v.abs_diff(STEPS[i])).unwrap();
    let (cr, cg, cb) = (step(r), step(g), step(b));
    let grey = ((u16::from(r) + u16::from(g) + u16::from(b)) / 3).saturating_sub(3) / 10;
    let grey = grey.min(23) as u8;
    let off = |to: [u8; 3]| -> u32 {
        [r, g, b]
            .iter()
            .zip(to)
            .map(|(v, t)| u32::from(v.abs_diff(t)).pow(2))
            .sum()
    };
    if off([8 + 10 * grey; 3]) < off([STEPS[cr], STEPS[cg], STEPS[cb]]) {
        Color::Indexed(232 + grey)
    } else {
        Color::Indexed((16 + 36 * cr + 6 * cg + cb) as u8)
    }
}

type Point = (f64, f64); // (lon, lat)

const LANDMARKS: [(&str, &str, f64, f64); 4] = [
    ("Plaza Mayor", "Plaza Mayor", -5.6640, 40.9650),
    ("Estación de tren", "Train station", -5.6487, 40.9722),
    ("Hospital", "Hospital", -5.6754, 40.9621),
    ("Río Tormes", "Tormes river", -5.6610, 40.9515),
];

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Stops,
    Arrivals,
    Map,
}

/// What a click on a region of the screen does.
#[derive(Clone, PartialEq)]
enum Hit {
    Tab(Tab),
    Key(KeyCode),
    Result(usize),
    Favourite(usize),
    Line(Option<String>),
    Palette,
    Command(usize),
}

#[derive(Clone, Copy)]
enum Command {
    Go(Tab),
    Refresh,
    Replay,
    Theme,
    Language,
    Quit,
}

#[derive(Default)]
struct Palette {
    query: String,
    selected: usize,
}

/// What the UI wants fetched, and what the worker has fetched.
struct Shared {
    tab: Tab,
    favourites: Vec<String>,
    refresh: bool,
    positions: Vec<Vehicle>,
    positions_at: Option<Instant>,
    /// Where each vehicle has been since the map was opened, oldest first.
    trails: HashMap<String, Vec<Point>>,
    arrivals: HashMap<String, (Instant, Vec<Arrival>)>,
    error: Option<api::Error>,
}

/// River and roads from OpenStreetMap, drawn under the stops.
#[derive(Deserialize)]
struct Basemap {
    river: Vec<Vec<Point>>,
    main: Vec<Vec<Point>>,
    minor: Vec<Vec<Point>>,
    streets: Vec<Vec<Point>>,
}

/// A moved or zoomed Map tab: `height` degrees of latitude around `centre`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct View {
    centre: Point,
    height: f64,
}

/// About 170 m to 11 km tall, centre kept over the basemap so the city can't be lost.
const MIN_HEIGHT: f64 = 0.0015;
const MAX_HEIGHT: f64 = 0.1;
const AREA_LON: [f64; 2] = [-5.745, -5.615];
const AREA_LAT: [f64; 2] = [40.925, 40.995];
/// Below these degrees of longitude per column the map adds streets, then stop numbers.
const STREETS_FROM: f64 = 0.0006;
const STOP_NUMBERS_FROM: f64 = 0.00012;

impl View {
    fn clamped(self) -> View {
        View {
            centre: (
                self.centre.0.clamp(AREA_LON[0], AREA_LON[1]),
                self.centre.1.clamp(AREA_LAT[0], AREA_LAT[1]),
            ),
            height: self.height.clamp(MIN_HEIGHT, MAX_HEIGHT),
        }
    }

    fn bounds(self, area: Rect) -> ([f64; 2], [f64; 2]) {
        let shrink = self.centre.1.to_radians().cos();
        let wanted = (f64::from(area.width.max(1)) * 2.0) / (f64::from(area.height.max(1)) * 4.0);
        let half_lon = self.height * wanted / shrink / 2.0;
        let (lon, lat) = self.centre;
        (
            [lon - half_lon, lon + half_lon],
            [lat - self.height / 2.0, lat + self.height / 2.0],
        )
    }

    /// Zoomed by `factor` (below 1 is closer) keeping `at` where it is on screen.
    fn zoomed(self, factor: f64, at: Point) -> View {
        let height = (self.height * factor).clamp(MIN_HEIGHT, MAX_HEIGHT);
        let f = height / self.height;
        View {
            centre: (
                at.0 + (self.centre.0 - at.0) * f,
                at.1 + (self.centre.1 - at.1) * f,
            ),
            height,
        }
        .clamped()
    }
}

/// The map's inner area and the bounds it was last drawn with.
#[derive(Clone, Copy)]
struct MapFrame {
    area: Rect,
    lon: [f64; 2],
    lat: [f64; 2],
}

impl MapFrame {
    fn view(self) -> View {
        View {
            centre: (
                (self.lon[0] + self.lon[1]) / 2.0,
                (self.lat[0] + self.lat[1]) / 2.0,
            ),
            height: self.lat[1] - self.lat[0],
        }
    }

    /// Degrees per column and per row.
    fn cell(self) -> (f64, f64) {
        (
            (self.lon[1] - self.lon[0]) / f64::from(self.area.width.max(1)),
            (self.lat[1] - self.lat[0]) / f64::from(self.area.height.max(1)),
        )
    }

    /// The place under the middle of a cell; rows run down while latitude runs up.
    fn point(self, at: Position) -> Point {
        let (w, h) = self.cell();
        (
            self.lon[0] + (f64::from(at.x - self.area.x) + 0.5) * w,
            self.lat[1] - (f64::from(at.y - self.area.y) + 0.5) * h,
        )
    }
}

/// What `draw_city` puts on a map, bottom layer first.
#[derive(Default)]
struct City<'a> {
    /// The map is fitted to these, unless the user has moved it.
    focus: &'a [Point],
    view: Option<View>,
    dots: &'a [Point],
    bright: &'a [Point],
    bright_colour: Color,
    trails: Vec<(&'a [Point], Color)>,
    labels: Vec<(Point, Span<'static>)>,
}

struct App {
    stops: Vec<Stop>,
    basemap: Basemap,
    shared: Arc<Mutex<Shared>>,
    query: String,
    results: ListState,
    selected: usize,
    line: Option<String>,
    /// Clickable regions of the last frame.
    hits: Vec<(Rect, Hit)>,
    mouse: Option<Position>,
    /// When the bus on the Paradas tab was first shown: its animation runs from here.
    bus_since: Option<Instant>,
    palette: Option<Palette>,
    /// The Map tab's view once moved or zoomed; `None` fits it to what is shown.
    view: Option<View>,
    map_frame: Option<MapFrame>,
    /// Where a drag on the map started, and the view then.
    drag: Option<(Position, View)>,
}

pub fn run(client: Client) -> io::Result<()> {
    let settings = load_settings();
    let english = match settings.lang.as_deref() {
        Some("en") => true,
        Some("es") => false,
        _ => !system_spanish(),
    };
    ENGLISH.store(english, Relaxed);
    let light = match settings.theme.as_deref() {
        Some("light") => true,
        Some("dark") => false,
        _ => system_light(),
    };
    LIGHT.store(light, Relaxed);
    let favourites = load_favourites();
    let shared = Arc::new(Mutex::new(Shared {
        tab: if favourites.is_empty() {
            Tab::Stops
        } else {
            Tab::Arrivals
        },
        favourites,
        refresh: false,
        positions: vec![],
        positions_at: None,
        trails: HashMap::new(),
        arrivals: HashMap::new(),
        error: None,
    }));
    // Not joined: the process ends when the UI does.
    thread::spawn({
        let shared = shared.clone();
        move || worker(&client, &shared)
    });
    let mut app = App {
        stops: api::bundled_stops(),
        basemap: serde_json::from_str(include_str!("../data/basemap.json"))
            .expect("bundled basemap.json"),
        shared,
        query: String::new(),
        results: ListState::default().with_selected(Some(0)),
        selected: 0,
        line: None,
        hits: vec![],
        mouse: None,
        bus_since: None,
        palette: None,
        view: None,
        map_frame: None,
        drag: None,
    };
    let mut terminal = ratatui::init();
    // ratatui's panic hook restores the screen but leaves mouse reporting on.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        hook(info);
    }));
    let result =
        execute!(io::stdout(), EnableMouseCapture).and_then(|()| app.event_loop(&mut terminal));
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

/// Only fetches what the open tab shows, one request at a time.
fn worker(client: &Client, shared: &Mutex<Shared>) {
    const POSITIONS: &str = "";
    let mut fetched: HashMap<String, Instant> = HashMap::new();
    loop {
        let (tab, favourites) = {
            let mut s = shared.lock().unwrap();
            if std::mem::take(&mut s.refresh) {
                fetched.clear();
            }
            (s.tab, s.favourites.clone())
        };
        fetched.retain(|key, _| key == POSITIONS || favourites.contains(key));
        let due =
            |key: &str, every: Duration| fetched.get(key).is_none_or(|at| at.elapsed() >= every);
        let job = match tab {
            Tab::Map => due(POSITIONS, POSITIONS_EVERY).then(|| POSITIONS.to_owned()),
            Tab::Arrivals => favourites.into_iter().find(|id| due(id, ARRIVALS_EVERY)),
            Tab::Stops => None,
        };
        let Some(key) = job else {
            thread::sleep(Duration::from_millis(100));
            continue;
        };
        let error = if key == POSITIONS {
            client.positions().map(|vehicles| {
                let mut s = shared.lock().unwrap();
                s.trails
                    .retain(|id, _| vehicles.iter().any(|v| v.vehicle == *id));
                for v in &vehicles {
                    let trail = s.trails.entry(v.vehicle.clone()).or_default();
                    if trail.last() != Some(&(v.lon, v.lat)) {
                        trail.push((v.lon, v.lat));
                    }
                    let extra = trail.len().saturating_sub(TRAIL_POINTS);
                    trail.drain(..extra);
                }
                s.positions = vehicles;
                s.positions_at = Some(Instant::now());
            })
        } else {
            client.arrivals(&key).map(|arrivals| {
                shared
                    .lock()
                    .unwrap()
                    .arrivals
                    .insert(key.clone(), (Instant::now(), arrivals));
            })
        }
        .err();
        fetched.insert(key, Instant::now());
        shared.lock().unwrap().error = error;
    }
}

impl App {
    fn event_loop(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;
            // Smooth only while the bus is on its way in.
            let moving = self.bus_since.is_some_and(|t| t.elapsed() < DOORS_OPEN);
            if !event::poll(Duration::from_millis(if moving { 33 } else { 200 }))? {
                continue;
            }
            let quit = match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key),
                Event::Mouse(mouse) => self.on_mouse(mouse),
                _ => false,
            };
            if quit {
                return Ok(());
            }
        }
    }

    /// Returns true to quit.
    fn on_mouse(&mut self, mouse: MouseEvent) -> bool {
        let at = Position::new(mouse.column, mouse.row);
        self.mouse = Some(at);
        let on_map = self.shared.lock().unwrap().tab == Tab::Map;
        let over_map =
            on_map && self.palette.is_none() && self.map_frame.is_some_and(|m| m.area.contains(at));
        match mouse.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp if over_map => {
                let closer = mouse.kind == MouseEventKind::ScrollUp;
                self.zoom(if closer { 0.8 } else { 1.25 }, Some(at));
                false
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                if on_map && self.palette.is_none() =>
            {
                self.cycle_line(mouse.kind == MouseEventKind::ScrollDown);
                false
            }
            MouseEventKind::ScrollDown => self.on_key(KeyCode::Down.into()),
            MouseEventKind::ScrollUp => self.on_key(KeyCode::Up.into()),
            MouseEventKind::Down(MouseButton::Left) if over_map => {
                self.drag = self.map_frame.map(|m| (at, self.view.unwrap_or(m.view())));
                false
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                // A click without movement leaves the view as it was.
                if let (Some((from, start)), Some(m)) = (self.drag, self.map_frame)
                    && at != from
                {
                    let (w, h) = m.cell();
                    let dx = f64::from(at.x) - f64::from(from.x);
                    let dy = f64::from(at.y) - f64::from(from.y);
                    let centre = (start.centre.0 - dx * w, start.centre.1 + dy * h);
                    self.view = Some(View { centre, ..start }.clamped());
                }
                false
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.drag = None;
                false
            }
            MouseEventKind::Down(MouseButton::Left) if self.palette.is_some() => {
                // Nothing under the palette takes the click: outside it, the click only closes it.
                let hit = self.hits.iter().rev().find(|(rect, _)| rect.contains(at));
                match hit.map(|(_, hit)| hit.clone()) {
                    Some(Hit::Command(i)) => {
                        self.palette = None;
                        return self
                            .palette_commands()
                            .get(i)
                            .is_some_and(|&(c, ..)| self.run(c));
                    }
                    _ => self.palette = None,
                }
                false
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = self
                    .hits
                    .iter()
                    .rev()
                    .find(|(rect, _)| rect.contains(at))
                    .map(|(_, hit)| hit.clone());
                match hit {
                    Some(Hit::Key(code)) => return self.on_key(code.into()),
                    Some(Hit::Tab(tab)) => self.shared.lock().unwrap().tab = tab,
                    // A second click on the chosen stop saves it, like Enter.
                    Some(Hit::Result(i)) if self.results.selected() == Some(i) => {
                        return self.on_key(KeyCode::Enter.into());
                    }
                    Some(Hit::Result(i)) => self.results.select(Some(i)),
                    Some(Hit::Favourite(i)) => self.selected = i,
                    Some(Hit::Line(line)) => {
                        self.line = line;
                        self.view = None;
                    }
                    Some(Hit::Palette) => self.palette = Some(Palette::default()),
                    Some(Hit::Command(_)) | None => {}
                }
                false
            }
            _ => false,
        }
    }

    /// Returns true to quit.
    fn on_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return true;
        }
        if ctrl && key.code == KeyCode::Char('p') {
            self.palette = match self.palette {
                Some(_) => None,
                None => Some(Palette::default()),
            };
            return false;
        }
        if self.palette.is_some() {
            return self.on_palette_key(key.code);
        }
        let shared = self.shared.clone();
        let mut s = shared.lock().unwrap();
        match (s.tab, key.code) {
            (Tab::Stops, KeyCode::Tab) | (Tab::Map, KeyCode::BackTab) => s.tab = Tab::Arrivals,
            (Tab::Arrivals, KeyCode::Tab) | (Tab::Stops, KeyCode::BackTab) => s.tab = Tab::Map,
            (Tab::Map, KeyCode::Tab) | (Tab::Arrivals, KeyCode::BackTab) => s.tab = Tab::Stops,

            (Tab::Stops, KeyCode::Char(c)) => {
                self.query.push(c);
                self.results.select(Some(0));
            }
            (Tab::Stops, KeyCode::Backspace) => {
                self.query.pop();
                self.results.select(Some(0));
            }
            (Tab::Stops, KeyCode::Esc) if !self.query.is_empty() => self.query.clear(),
            (Tab::Stops, KeyCode::Down) => self.results.select_next(),
            (Tab::Stops, KeyCode::Up) => self.results.select_previous(),
            (Tab::Stops, KeyCode::Enter) => {
                let found = api::search(&self.stops, &self.query);
                if let Some(stop) = self.results.selected().and_then(|i| found.get(i)) {
                    if !s.favourites.contains(&stop.id) {
                        s.favourites.push(stop.id.clone());
                        save_favourites(&s.favourites);
                    }
                    self.selected = s
                        .favourites
                        .iter()
                        .position(|id| *id == stop.id)
                        .unwrap_or(0);
                    s.tab = Tab::Arrivals;
                }
            }

            (_, KeyCode::Char('q') | KeyCode::Esc) => return true,
            (_, KeyCode::Char('r')) => s.refresh = true,

            (Tab::Arrivals, KeyCode::Down | KeyCode::Char('j')) => {
                self.selected = (self.selected + 1).min(s.favourites.len().saturating_sub(1));
            }
            (Tab::Arrivals, KeyCode::Up | KeyCode::Char('k')) => {
                self.selected = self.selected.saturating_sub(1)
            }
            (Tab::Arrivals, KeyCode::Char('d') | KeyCode::Delete)
                if self.selected < s.favourites.len() =>
            {
                let id = s.favourites.remove(self.selected);
                s.arrivals.remove(&id);
                save_favourites(&s.favourites);
                self.selected = self.selected.min(s.favourites.len().saturating_sub(1));
            }

            (Tab::Map, KeyCode::Down | KeyCode::Right | KeyCode::Up | KeyCode::Left) => {
                drop(s);
                self.cycle_line(matches!(key.code, KeyCode::Down | KeyCode::Right));
            }
            (Tab::Map, KeyCode::Char('+' | '=')) => self.zoom(0.8, None),
            (Tab::Map, KeyCode::Char('-')) => self.zoom(1.25, None),
            (Tab::Map, KeyCode::Char('0')) => self.view = None,
            (Tab::Map, KeyCode::Char(c @ ('w' | 'a' | 's' | 'd' | 'W' | 'A' | 'S' | 'D'))) => {
                if let Some(view) = self.view.or(self.map_frame.map(MapFrame::view)) {
                    // A quarter of the map per press.
                    let (step_lon, step_lat) = self.map_frame.map_or((0.0, 0.0), |m| {
                        ((m.lon[1] - m.lon[0]) / 4.0, (m.lat[1] - m.lat[0]) / 4.0)
                    });
                    let (dx, dy) = match c.to_ascii_lowercase() {
                        'a' => (-step_lon, 0.0),
                        'd' => (step_lon, 0.0),
                        'w' => (0.0, step_lat),
                        _ => (0.0, -step_lat),
                    };
                    let centre = (view.centre.0 + dx, view.centre.1 + dy);
                    self.view = Some(View { centre, ..view }.clamped());
                }
            }
            _ => {}
        }
        false
    }

    /// All lines, then each line with a bus on the road, then all again.
    fn cycle_line(&mut self, forward: bool) {
        let s = self.shared.lock().unwrap();
        let mut options: Vec<Option<String>> = vec![None];
        options.extend(
            lines_on_road(&s.positions)
                .into_iter()
                .map(|(line, _, _)| Some(line)),
        );
        let at = options.iter().position(|o| *o == self.line).unwrap_or(0);
        let step = if forward { 1 } else { options.len() - 1 };
        self.line = options[(at + step) % options.len()].clone();
        self.view = None;
    }

    /// Around the pointer when there is one over the map, else around the middle.
    fn zoom(&mut self, factor: f64, at: Option<Position>) {
        let Some(m) = self.map_frame else { return };
        let view = self.view.unwrap_or(m.view());
        let at = at.map_or(view.centre, |at| m.point(at));
        self.view = Some(view.zoomed(factor, at));
    }

    /// Returns true to quit.
    fn on_palette_key(&mut self, code: KeyCode) -> bool {
        let found = self.palette_commands();
        let Some(p) = self.palette.as_mut() else {
            return false;
        };
        match code {
            KeyCode::Esc => self.palette = None,
            KeyCode::Down => p.selected = (p.selected + 1).min(found.len().saturating_sub(1)),
            KeyCode::Up => p.selected = p.selected.saturating_sub(1),
            KeyCode::Backspace => {
                p.query.pop();
                p.selected = 0;
            }
            KeyCode::Char(c) => {
                p.query.push(c);
                p.selected = 0;
            }
            KeyCode::Enter => {
                let chosen = found.get(p.selected).map(|&(c, ..)| c);
                self.palette = None;
                return chosen.is_some_and(|c| self.run(c));
            }
            _ => {}
        }
        false
    }

    /// The commands whose name matches what is typed in the palette: (command, name, key).
    fn palette_commands(&self) -> Vec<(Command, String, &'static str)> {
        let theme_name = if LIGHT.load(Relaxed) {
            tr("Modo oscuro", "Dark mode")
        } else {
            tr("Modo claro", "Light mode")
        };
        let all = [
            (
                Command::Go(Tab::Stops),
                tr("Ir a Paradas", "Go to Stops"),
                "",
            ),
            (
                Command::Go(Tab::Arrivals),
                tr("Ir a Llegadas", "Go to Arrivals"),
                "",
            ),
            (Command::Go(Tab::Map), tr("Ir al Mapa", "Go to Map"), ""),
            (Command::Refresh, tr("Actualizar ahora", "Refresh now"), "r"),
            (
                Command::Replay,
                tr("Ver llegar el autobús", "Watch the bus pull in"),
                "",
            ),
            (Command::Theme, theme_name, ""),
            (
                Command::Language,
                tr("English (cambiar idioma)", "Español (change language)"),
                "",
            ),
            (Command::Quit, tr("Salir", "Quit"), "q"),
        ];
        let query = self
            .palette
            .as_ref()
            .map_or(String::new(), |p| api::fold(&p.query));
        all.into_iter()
            .filter(|(_, name, _)| api::fold(name).contains(&query))
            .map(|(c, name, key)| (c, name.to_owned(), key))
            .collect()
    }

    /// Returns true to quit.
    fn run(&mut self, command: Command) -> bool {
        let mut s = self.shared.lock().unwrap();
        match command {
            Command::Go(tab) => s.tab = tab,
            Command::Refresh => s.refresh = true,
            Command::Replay => {
                self.query.clear();
                self.bus_since = None;
                s.tab = Tab::Stops;
            }
            Command::Theme => {
                let light = !LIGHT.load(Relaxed);
                LIGHT.store(light, Relaxed);
                let mut settings = load_settings();
                settings.theme = Some(if light { "light" } else { "dark" }.to_owned());
                save_settings(&settings);
            }
            Command::Language => {
                let english = !ENGLISH.load(Relaxed);
                ENGLISH.store(english, Relaxed);
                let mut settings = load_settings();
                settings.lang = Some(if english { "en" } else { "es" }.to_owned());
                save_settings(&settings);
            }
            Command::Quit => return true,
        }
        false
    }

    /// A row of clickable groups of spans, laid out left to right. The one under the mouse lights up.
    fn buttons(
        &mut self,
        area: Rect,
        items: Vec<(Vec<Span<'static>>, Option<Hit>)>,
    ) -> Line<'static> {
        let mut x = area.x;
        let mut line = vec![];
        for (spans, hit) in items {
            let width = spans.iter().map(Span::width).sum::<usize>() as u16;
            let rect = Rect {
                x,
                width,
                height: 1,
                ..area
            };
            x += width;
            let hot = hit.is_some() && self.mouse.is_some_and(|at| rect.contains(at));
            line.extend(
                spans
                    .into_iter()
                    .map(|span| if hot { span.bg(theme().panel) } else { span }),
            );
            self.hits.extend(hit.map(|hit| (rect, hit)));
        }
        Line::from(line)
    }

    fn draw(&mut self, frame: &mut Frame) {
        let t = theme();
        self.hits.clear();
        frame.render_widget(
            Block::new().style(Style::new().bg(t.bg).fg(t.fg)),
            frame.area(),
        );
        let [header, _, body, _, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let padded = Margin::new(2, 0);
        let shared = self.shared.clone();
        let s = shared.lock().unwrap();

        let mut top = vec![(vec!["salbus".fg(t.primary).bold(), "   ".into()], None)];
        for (tab, name) in [
            (Tab::Stops, tr("Paradas", "Stops")),
            (Tab::Arrivals, tr("Llegadas", "Arrivals")),
            (Tab::Map, tr("Mapa", "Map")),
        ] {
            let label = format!(" {name} ");
            let label = if tab == s.tab {
                label.fg(t.bg).bg(t.primary).bold()
            } else {
                label.fg(t.muted)
            };
            // The chosen tab keeps its colour under the mouse.
            top.push((vec![label], (tab != s.tab).then_some(Hit::Tab(tab))));
            top.push((vec![" ".into()], None));
        }
        let top = self.buttons(header.inner(padded), top);
        frame.render_widget(top, header.inner(padded));
        let freshest = match s.tab {
            Tab::Stops => None,
            Tab::Arrivals => s.arrivals.values().map(|(at, _)| *at).max(),
            Tab::Map => s.positions_at,
        };
        let live = match freshest {
            _ if s.tab == Tab::Stops => {
                format!("{} {}", self.stops.len(), tr("paradas", "stops")).fg(t.muted)
            }
            Some(at) if at.elapsed() < Duration::from_secs(90) => {
                tr("● en vivo", "● live").fg(t.success)
            }
            Some(_) => tr("● datos viejos", "● stale data").fg(t.warning),
            None => tr("○ cargando…", "○ loading…").fg(t.muted),
        };
        frame.render_widget(Line::from(live).right_aligned(), header.inner(padded));

        let hints: Vec<(&str, &str, KeyCode)> = match s.tab {
            Tab::Stops => vec![
                ("↑↓", tr("elegir", "choose"), KeyCode::Down),
                ("enter", tr("guardar parada", "save stop"), KeyCode::Enter),
                ("tab", tr("llegadas", "arrivals"), KeyCode::Tab),
                ("esc", tr("salir", "quit"), KeyCode::Esc),
            ],
            Tab::Arrivals => vec![
                ("↑↓", tr("parada", "stop"), KeyCode::Down),
                ("d", tr("quitar", "remove"), KeyCode::Char('d')),
                ("r", tr("actualizar", "refresh"), KeyCode::Char('r')),
                ("tab", tr("mapa", "map"), KeyCode::Tab),
                ("q", tr("salir", "quit"), KeyCode::Char('q')),
            ],
            Tab::Map => vec![
                ("↑↓", tr("línea", "line"), KeyCode::Down),
                ("wasd", tr("mover", "move"), KeyCode::Char('d')),
                ("+−", "zoom", KeyCode::Char('+')),
                ("0", tr("centrar", "reset"), KeyCode::Char('0')),
                ("q", tr("salir", "quit"), KeyCode::Char('q')),
            ],
        };
        let mut bar: Vec<(Vec<Span>, Option<Hit>)> = hints
            .iter()
            .map(|(key, what, code)| {
                let spans = vec![
                    format!(" {key} ").fg(t.accent).bold(),
                    format!("{what}  ").fg(t.fg),
                ];
                (spans, Some(Hit::Key(*code)))
            })
            .collect();
        bar.push((
            vec![
                " ctrl+p ".fg(t.accent).bold(),
                format!("{}  ", tr("comandos", "commands")).fg(t.fg),
            ],
            Some(Hit::Palette),
        ));
        if let Some(error) = &s.error {
            let error = error.message(ENGLISH.load(Relaxed));
            bar.push((vec![format!(" {error}").fg(t.error)], None));
        }
        frame.render_widget(Block::new().style(Style::new().bg(t.surface)), footer);
        let bar = self.buttons(footer.inner(Margin::new(1, 0)), bar);
        frame.render_widget(bar, footer.inner(Margin::new(1, 0)));

        let body = body.inner(padded);
        match s.tab {
            Tab::Stops => self.draw_stops(frame, body, &s),
            Tab::Arrivals => self.draw_arrivals(frame, body, &s),
            Tab::Map => self.draw_map(frame, body, &s),
        }
        drop(s);
        if self.palette.is_some() {
            self.draw_palette(frame);
        }
    }

    /// A box over the middle of the screen: what is typed, then the matching commands.
    fn draw_palette(&mut self, frame: &mut Frame) {
        let t = theme();
        let found = self.palette_commands();
        let Some(p) = self.palette.as_mut() else {
            return;
        };
        p.selected = p.selected.min(found.len().saturating_sub(1));
        let screen = frame.area();
        let width = screen.width.saturating_sub(4).min(56);
        let height = (found.len().max(1) as u16 + 4).min(screen.height.saturating_sub(2));
        let area = Rect {
            x: screen.x + (screen.width - width) / 2,
            y: screen.y + (screen.height - height) / 3,
            width,
            height,
        };
        frame.render_widget(Clear, area);
        let block = panel(tr("Comandos", "Commands"), true).title_top(
            Line::from(tr("esc cerrar ", "esc close "))
                .fg(t.muted)
                .right_aligned(),
        );
        let inner = block.inner(area).inner(Margin::new(1, 0));
        frame.render_widget(block, area);
        let typed = format!("› {}▏", p.query);
        let input = Rect { height: 1, ..inner };
        frame.render_widget(Line::from(typed.fg(t.fg)).bg(t.field), input);
        let selected = p.selected;
        if found.is_empty() {
            let none = Rect {
                y: inner.y + 2,
                height: 1,
                ..inner
            };
            frame.render_widget(
                Line::from(tr("Ningún comando casa", "No matching command").fg(t.muted)),
                none,
            );
        }
        for (i, (_, name, key)) in found.iter().enumerate() {
            let row = Rect {
                y: inner.y + 2 + i as u16,
                height: 1,
                ..inner
            };
            if row.bottom() > inner.bottom() {
                break;
            }
            let hot = self.mouse.is_some_and(|at| row.contains(at));
            let style = match (i == selected, hot) {
                (true, _) => Style::new().fg(t.bg).bg(t.primary).bold(),
                (false, true) => Style::new().fg(t.fg).bg(t.surface),
                (false, false) => Style::new().fg(t.fg),
            };
            let room = usize::from(row.width).saturating_sub(key.chars().count() + 2);
            let text = format!(" {:<room$}{key} ", clip(name, room));
            frame.render_widget(Line::from(Span::styled(text, style)), row);
            self.hits.push((row, Hit::Command(i)));
        }
    }

    fn draw_stops(&mut self, frame: &mut Frame, area: Rect, s: &Shared) {
        let t = theme();
        let (left, side) = with_side_panel(area);
        // The wordmark and the bus give way to results as soon as there is something to look for.
        let hero = if self.query.is_empty() && left.height >= 30 {
            15
        } else {
            0
        };
        let [hero, input, _, count, _, list] = Layout::vertical([
            Constraint::Length(hero),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(left);
        if hero.height > 0 {
            let mut art: Vec<Line> = WORDMARK
                .iter()
                .map(|row| Line::from(row.fg(t.primary).bold()))
                .collect();
            art.push(Line::default());
            art.push(Line::from(
                tr(
                    "Autobuses urbanos de Salamanca, en vivo",
                    "Salamanca city buses, live",
                )
                .fg(t.muted),
            ));
            frame.render_widget(Paragraph::new(art).centered(), hero);
            let street = Rect {
                y: hero.y + 6,
                height: 8,
                ..hero
            };
            self.draw_bus(frame, street);
        }

        let width = usize::from(left.width).saturating_sub(10).min(56);
        let typed = if self.query.is_empty() {
            format!(
                " {:<width$}",
                tr("nombre, calle o número", "name, street or number")
            )
            .fg(t.muted)
        } else {
            format!(" {:<width$}", format!("{}▏", self.query)).fg(t.fg)
        };
        frame.render_widget(
            Line::from(vec![
                format!("{:<9}", tr("Parada", "Stop")).fg(t.muted),
                typed.bg(t.field),
            ]),
            input,
        );

        let found = api::search(&self.stops, &self.query);
        let help = format!(
            "{} {}",
            found.len(),
            tr(
                "paradas. Elige una con las flechas o el ratón.",
                "stops. Pick one with the arrows or the mouse.",
            )
        );
        frame.render_widget(Line::from(help.fg(t.muted)), count);

        // Badges need 4 columns per line; the busiest stops have 8 or more.
        let name_width = usize::from(left.width).saturating_sub(48).clamp(16, 30);
        let first = self.results.offset();
        let chosen = self.results.selected();
        let items = found.iter().enumerate().map(|(i, stop)| {
            let row = Rect {
                y: list.y + i.saturating_sub(first) as u16,
                height: 1,
                ..list
            };
            let hot = i >= first
                && self
                    .mouse
                    .is_some_and(|at| row.contains(at) && list.contains(at));
            // The cursor stops short of the line numbers so they keep their colours.
            let base = match (chosen == Some(i), hot) {
                (true, _) => Style::new().fg(t.bg).bg(t.primary).bold(),
                (false, true) => Style::new().bg(t.panel),
                (false, false) => Style::new(),
            };
            let star = if s.favourites.contains(&stop.id) {
                "★"
            } else {
                " "
            };
            let number = Span::styled(format!(" {star} {:>3}  ", stop.id), base);
            let mut cells = vec![
                if chosen == Some(i) {
                    number
                } else {
                    number.fg(t.accent)
                },
                Span::styled(
                    format!("{:<name_width$} ", clip(&stop.name, name_width)),
                    base,
                ),
                " ".into(),
            ];
            // Busy stops serve up to 13 lines: show what fits and count the rest.
            let mut room = usize::from(left.width).saturating_sub(name_width + 14);
            for (n, line) in stop.lines.iter().enumerate() {
                // Coloured text, not filled badges: a wall of 300 filled badges glares.
                let badge = Span::styled(
                    format!(" {line:>2} "),
                    Style::new().fg(line_colour(line)).bold(),
                );
                if badge.width() > room {
                    cells.push(format!(" +{}", stop.lines.len() - n).fg(t.muted));
                    break;
                }
                room -= badge.width();
                cells.push(badge);
            }
            Line::from(cells)
        });
        let rows = found.len();
        frame.render_stateful_widget(
            List::new(items).style(Style::new().bg(t.surface)),
            list,
            &mut self.results,
        );
        let first = self.results.offset();
        for row in 0..usize::from(list.height).min(rows.saturating_sub(first)) {
            let rect = Rect {
                y: list.y + row as u16,
                height: 1,
                ..list
            };
            self.hits.push((rect, Hit::Result(first + row)));
        }

        let Some(side) = side else { return };
        let stop = self.results.selected().and_then(|i| found.get(i));
        let everything: Vec<Point> = self.stops.iter().filter_map(point).collect();
        let title = stop.map_or(tr("Mapa", "Map").to_owned(), |stop| {
            format!("{} · {}", stop.id, stop.name)
        });
        let labels = stop
            .and_then(|stop| point(stop))
            .map(|p| (p, "◉".fg(t.accent).bold()))
            .into_iter()
            .collect();
        let city = City {
            focus: &everything,
            dots: &everything,
            labels,
            ..City::default()
        };
        self.draw_city(frame, side, panel(&title, false), city);
    }

    /// The bus pulls in from the left, stops at the sign and opens its doors, once per run.
    fn draw_bus(&mut self, frame: &mut Frame, area: Rect) {
        let since = self.bus_since.get_or_insert_with(Instant::now).elapsed();
        let parked = ((i32::from(area.width) - SCENE_WIDTH) / 2).max(0);
        let left = 1.0 - (since.as_secs_f64() / DRIVE.as_secs_f64()).min(1.0);
        // Out of sight at first, braking as it nears the sign.
        let x = parked - (f64::from(parked + BUS_WIDTH) * left.powi(3)).round() as i32;
        let (spoke, open) = (x.rem_euclid(4) as usize, since >= DOORS_OPEN);
        let paint = |pixel, col, row| sprite_colour(pixel, col, row, spoke, open);
        draw_sprite(frame, area, parked + BUS_WIDTH + 2, &STOP_SIGN, paint);
        draw_sprite(frame, area, x, &BUS, paint);
        let road = Rect {
            y: area.y + 7,
            height: 1,
            ..area
        };
        frame.render_widget(
            Line::from("▀".repeat(area.width.into()).fg(theme().road)),
            road,
        );
    }

    fn draw_arrivals(&mut self, frame: &mut Frame, area: Rect, s: &Shared) {
        let t = theme();
        if s.favourites.is_empty() {
            let (tab, enter) = ("tab".fg(t.accent).bold(), "enter".fg(t.accent).bold());
            let how = vec![
                tr("Pulsa ", "Press ").into(),
                tab,
                tr(
                    " hasta «Paradas», busca una y pulsa ",
                    " until «Stops», find one and press ",
                )
                .into(),
                enter,
                ".".into(),
            ];
            let help = vec![
                Line::default(),
                Line::from(tr(
                    "Aún no has guardado ninguna parada.",
                    "You have not saved any stop yet.",
                ))
                .bold()
                .centered(),
                Line::default(),
                Line::from(how).fg(t.muted).centered(),
            ];
            return frame.render_widget(Paragraph::new(help), area);
        }
        let (cards, side) = with_side_panel(area);
        let now = api::now_epoch();
        // A title row, the arrivals, and a blank row before the next card.
        let height = |id: &String| {
            let rows = s
                .arrivals
                .get(id)
                .map_or(1, |(_, list)| list.len().clamp(1, ARRIVALS_SHOWN));
            rows as u16 + 2
        };

        // Start at the first card that still leaves the selected one fully visible.
        let mut first = 0;
        while first < self.selected
            && s.favourites[first..=self.selected]
                .iter()
                .map(height)
                .sum::<u16>()
                > cards.height + 1
        {
            first += 1;
        }
        let mut y = cards.y;
        for (i, id) in s.favourites.iter().enumerate().skip(first) {
            let rect = Rect {
                y,
                height: (height(id) - 1).min(cards.bottom().saturating_sub(y)),
                ..cards
            };
            if rect.height < 2 {
                break;
            }
            y += rect.height + 1;
            let name = self
                .stops
                .iter()
                .find(|stop| stop.id == *id)
                .map_or("", |stop| stop.name.as_str());
            let fetched = s.arrivals.get(id);
            let age = fetched.map_or(tr("cargando…", "loading…").to_owned(), |(at, _)| {
                ago(*at)
            });
            let block = panel(&format!("★ {id} · {name}"), i == self.selected)
                .title_top(Line::from(format!("{age} ")).fg(t.muted).right_aligned());
            let width = usize::from(rect.width);
            let rows: Vec<Line> = match fetched {
                None => vec![],
                Some((_, list)) if list.is_empty() => {
                    vec![
                        tr(" sin autobuses previstos", " no buses expected")
                            .fg(t.muted)
                            .into(),
                    ]
                }
                Some((_, list)) => list
                    .iter()
                    .take(ARRIVALS_SHOWN)
                    .map(|a| arrival_row(a, now, width))
                    .collect(),
            };
            frame.render_widget(Paragraph::new(rows).block(block), rect);
            self.hits.push((rect, Hit::Favourite(i)));
        }

        let Some(side) = side else { return };
        let Some(id) = s.favourites.get(self.selected) else {
            return;
        };
        let here = self
            .stops
            .iter()
            .find(|stop| stop.id == *id)
            .and_then(point);
        // The arrivals already say where each bus is: no extra request for this map.
        let coming: Vec<(Point, &Arrival)> = s
            .arrivals
            .get(id)
            .map(|(_, list)| {
                list.iter()
                    .take(ARRIVALS_SHOWN)
                    .filter_map(|a| Some(((a.lon?, a.lat?), a)))
                    .collect()
            })
            .unwrap_or_default();
        let focus: Vec<Point> = here
            .into_iter()
            .chain(coming.iter().map(|(p, _)| *p))
            .collect();
        let everything: Vec<Point> = self.stops.iter().filter_map(point).collect();
        // Furthest first, so the next bus is drawn on top.
        let mut labels: Vec<(Point, Span)> = coming
            .iter()
            .rev()
            .map(|(p, a)| (*p, badge(&a.line)))
            .collect();
        labels.extend(here.map(|p| (p, "◉".fg(t.accent).bold())));
        let city = City {
            focus: &focus,
            dots: &everything,
            labels,
            ..City::default()
        };
        self.draw_city(
            frame,
            side,
            panel(tr("Autobuses en camino", "Buses on their way"), false),
            city,
        );
    }

    fn draw_map(&mut self, frame: &mut Frame, area: Rect, s: &Shared) {
        let t = theme();
        let on_line = |line: &str| self.line.as_deref().is_none_or(|l| l == line);
        let vehicles: Vec<&Vehicle> = s.positions.iter().filter(|v| on_line(&v.line)).collect();
        let everything: Vec<Point> = self.stops.iter().filter_map(point).collect();
        let of_line: Vec<Point> = match &self.line {
            Some(line) => self
                .stops
                .iter()
                .filter(|stop| stop.lines.contains(line))
                .filter_map(point)
                .collect(),
            None => vec![],
        };
        // Trails of every bus at once are noise: only for the line being followed.
        let core = core(&everything);
        let trails = vehicles
            .iter()
            .filter(|_| self.line.is_some())
            .filter_map(|v| Some((s.trails.get(&v.vehicle)?.as_slice(), line_colour(&v.line))))
            .collect();
        let mut labels: Vec<(Point, Span)> = self
            .stops
            .iter()
            .filter(|stop| s.favourites.contains(&stop.id))
            .filter_map(|stop| Some((point(stop)?, "★".fg(t.accent))))
            .collect();
        labels.extend(vehicles.iter().map(|v| {
            // Each bus is its line number. Following one line there is room for its heading too.
            let close = self.view.is_some_and(|v| v.height < 0.01);
            let label = match self.line {
                _ if close => {
                    Span::styled(format!(" {} {}", v.line, heading(v)), badge(&v.line).style)
                }
                Some(_) => {
                    Span::styled(format!(" {} {}", v.line, heading(v)), badge(&v.line).style)
                }
                None => Span::styled(v.line.clone(), badge(&v.line).style),
            };
            ((v.lon, v.lat), label)
        }));

        let (map, side) = if area.width >= LEGEND_FROM {
            let [map, _, side] = Layout::horizontal([
                Constraint::Min(0),
                Constraint::Length(2),
                Constraint::Length(36),
            ])
            .areas(area);
            (map, Some(side))
        } else {
            (area, None)
        };
        let age = s
            .positions_at
            .map_or(tr("cargando…", "loading…").to_owned(), ago);
        let title = match &self.line {
            Some(line) => format!(
                "{} {line} · {} {}",
                tr("Línea", "Line"),
                vehicles.len(),
                tr("autobuses", "buses")
            ),
            None => format!(
                "Salamanca · {} {}",
                vehicles.len(),
                tr("autobuses", "buses")
            ),
        };
        let key = match self.line {
            Some(_) => tr(
                " → en marcha   ● parado   ★ parada guardada",
                " → moving   ● stopped   ★ saved stop",
            ),
            None => tr(
                " número = línea del autobús   ★ parada guardada",
                " number = the bus's line   ★ saved stop",
            ),
        };
        let block = panel(&title, false)
            .title_top(Line::from(format!("{age} ")).fg(t.muted).right_aligned())
            .title_bottom(Line::from(key).fg(t.muted))
            .title_bottom(Line::from("© OpenStreetMap ").fg(t.muted).right_aligned());
        let city = City {
            focus: if of_line.is_empty() { &core } else { &of_line },
            view: self.view,
            dots: &everything,
            bright: &of_line,
            bright_colour: line_colour(self.line.as_deref().unwrap_or("")),
            trails,
            labels,
        };
        self.map_frame = Some(self.draw_city(frame, map, block, city));

        let Some(side) = side else { return };
        let block = panel(tr("Líneas en servicio", "Lines running"), false);
        let inner = block.inner(side);
        let on_road = lines_on_road(&s.positions);
        let mut entries = vec![(
            None,
            "    ".into(),
            tr("Todas las líneas", "All lines").to_owned(),
            s.positions.len(),
        )];
        entries.extend(on_road.into_iter().map(|(line, name, buses)| {
            let mark = badge(&line);
            (Some(line), mark, name, buses)
        }));
        // Keep the selected line on screen when the list is taller than the panel.
        let at = entries
            .iter()
            .position(|(line, ..)| *line == self.line)
            .unwrap_or(0);
        let scroll = (at + 1).saturating_sub(usize::from(inner.height));
        let mut rows = vec![];
        for (i, (line, mark, name, buses)) in entries.into_iter().enumerate().skip(scroll) {
            let rect = Rect {
                y: inner.y + (i - scroll) as u16,
                height: 1,
                ..inner
            };
            if rect.y >= inner.bottom() {
                break;
            }
            let base = match (
                line == self.line,
                self.mouse.is_some_and(|at| rect.contains(at)),
            ) {
                (true, _) => Style::new().fg(t.bg).bg(t.primary).bold(),
                (false, true) => Style::new().bg(t.panel),
                (false, false) => Style::new().fg(t.muted),
            };
            let name = Span::styled(format!(" {:<23}{buses:>3} ", clip(&name, 22)), base);
            rows.push(Line::from(vec![" ".into(), mark, name]));
            self.hits.push((rect, Hit::Line(line)));
        }
        frame.render_widget(Paragraph::new(rows).block(block), side);
    }

    /// City map: roads and river, stop dots, highlighted stops, trails, then labels on top.
    fn draw_city(&self, frame: &mut Frame, area: Rect, block: Block, city: City) -> MapFrame {
        let t = theme();
        let inner = block.inner(area);
        let (x_bounds, y_bounds) = match city.view {
            Some(view) => view.bounds(inner),
            None => fit(city.focus, inner),
        };
        // Labels are drawn from their left edge; nudge them so they sit on the point.
        let cell = (x_bounds[1] - x_bounds[0]) / f64::from(inner.width.max(1));
        let canvas = Canvas::default()
            .block(block)
            .background_color(t.surface)
            .marker(Marker::Braille)
            .x_bounds(x_bounds)
            .y_bounds(y_bounds)
            .paint(|ctx| {
                let streets: &[Vec<Point>] = if cell < STREETS_FROM {
                    &self.basemap.streets
                } else {
                    &[]
                };
                for (lines, colour) in [
                    (streets, t.road_minor),
                    (&self.basemap.minor[..], t.road_minor),
                    (&self.basemap.main[..], t.road),
                    (&self.basemap.river[..], t.river),
                ] {
                    for pair in lines.iter().flat_map(|line| line.windows(2)) {
                        ctx.draw(&Segment::new(
                            pair[0].0, pair[0].1, pair[1].0, pair[1].1, colour,
                        ));
                    }
                }
                ctx.layer();
                ctx.draw(&Points {
                    coords: city.dots,
                    color: t.stop,
                });
                ctx.layer();
                for (trail, colour) in &city.trails {
                    ctx.draw(&Points {
                        coords: trail,
                        color: *colour,
                    });
                }
                ctx.draw(&Points {
                    coords: city.bright,
                    color: city.bright_colour,
                });
                if cell < STOP_NUMBERS_FROM {
                    for stop in &self.stops {
                        if let Some((lon, lat)) = point(stop) {
                            ctx.print(lon + cell, lat, stop.id.clone().fg(t.muted));
                        }
                    }
                }
                // Zoomed out to the whole network the names would sit on top of each other.
                let places = if cell < 0.0015 { &LANDMARKS[..] } else { &[] };
                for &(es, en, lon, lat) in places {
                    let name = tr(es, en);
                    let shift = cell * (name.chars().count() as f64) / 2.0;
                    ctx.print(lon - shift, lat, name.fg(t.stop).italic());
                }
                for ((lon, lat), label) in &city.labels {
                    let shift = cell * (label.width().saturating_sub(1) as f64) / 2.0;
                    ctx.print(lon - shift, *lat, label.clone());
                }
            });
        frame.render_widget(canvas, area);
        MapFrame {
            area: inner,
            lon: x_bounds,
            lat: y_bounds,
        }
    }
}

/// `sprite` drawn `x` cells from `area`'s left edge, clipped to it; `None` pixels are see-through.
fn draw_sprite(
    frame: &mut Frame,
    area: Rect,
    x: i32,
    sprite: &[&str],
    colour: impl Fn(u8, usize, usize) -> Option<Color>,
) {
    for (row, pair) in sprite.chunks(2).enumerate() {
        let (top, bottom) = (pair[0].as_bytes(), pair[1].as_bytes());
        for col in 0..top.len() {
            let on_screen = i32::from(area.x) + x + col as i32;
            let (Ok(sx), sy) = (u16::try_from(on_screen), area.y + row as u16) else {
                continue;
            };
            if !area.contains(Position::new(sx, sy)) {
                continue;
            }
            let cell = &mut frame.buffer_mut()[(sx, sy)];
            match (
                colour(top[col], col, 2 * row),
                colour(bottom[col], col, 2 * row + 1),
            ) {
                (Some(top), Some(bottom)) => cell.set_char('▀').set_fg(top).set_bg(bottom),
                (Some(top), None) => cell.set_char('▀').set_fg(top),
                (None, Some(bottom)) => cell.set_char('▄').set_fg(bottom),
                (None, None) => cell,
            };
        }
    }
}

/// `spoke` is which of a hub's four pixels is lit, so the wheels turn; `open` is the doors.
/// The bus keeps its own colours in both themes; only the sign's frame follows the theme.
fn sprite_colour(pixel: u8, x: usize, y: usize, spoke: usize, open: bool) -> Option<Color> {
    let hex = match pixel {
        b'F' => return Some(theme().fg),
        b'B' => 0x3f9de3,
        b'i' if !open => 0x3f9de3,
        b'D' => 0x2f6ea8,
        b'W' => 0x9dcff5,
        b'd' if !open => 0x9dcff5,
        b'd' | b'i' | b'S' => 0x1e2941,
        b'L' => 0xe4e9f2,
        b'A' => 0xe0b25c,
        b'T' => 0xe0675c,
        b'K' | b'P' => 0x8d97ab,
        b'H' => {
            // The hub's pixels clockwise from the top left; the wheels are 19 apart.
            let (dx, dy) = ((x + 11) % 19, y - 11);
            let clockwise = if dy == 0 { dx } else { 3 - dx };
            if clockwise == spoke {
                0xe4e9f2
            } else {
                0x65708a
            }
        }
        _ => return None,
    };
    Some(rgb(hex))
}

fn arrival_row(a: &Arrival, now: i64, width: usize) -> Line<'static> {
    const BAR: usize = 14;
    const BAR_MINUTES: usize = 20;
    let t = theme();
    let minutes = a.expected_epoch.map(|e| api::minutes_until(e, now));
    let (eta, eta_colour) = match minutes {
        Some(0) => (tr("llegando", "arriving").to_owned(), t.success),
        Some(m) if m <= 2 => (format!("{m} min"), t.success),
        Some(m) if m <= 6 => (format!("{m} min"), t.warning),
        Some(m) => (format!("{m} min"), t.fg),
        None => ("?".to_owned(), t.muted),
    };
    // Fills up as the bus gets closer; empty at 20 minutes or more.
    let left = minutes.map_or(BAR_MINUTES, |m| (m as usize).min(BAR_MINUTES));
    let filled = BAR - left * BAR / BAR_MINUTES;
    let clock = a
        .expected
        .as_deref()
        .and_then(|e| e.get(11..16))
        .unwrap_or("");
    let distance = match a.distance_m {
        Some(m) if m >= 1000.0 => format!("{:.1} km", m / 1000.0),
        Some(m) => format!("{m:.0} m"),
        None => String::new(),
    };
    // Everything but the destination has a fixed width; the destination takes the rest.
    let room = width.saturating_sub(BAR + 34).max(8);
    Line::from(vec![
        " ".into(),
        badge(&a.line),
        format!(" {:<room$} ", clip(&a.destination, room)).into(),
        "━".repeat(filled).fg(line_colour(&a.line)),
        "─".repeat(BAR - filled).fg(t.panel),
        format!(" {eta:>9}").fg(eta_colour).bold(),
        format!("  {clock:<5}").fg(t.muted),
        format!("  {distance:>7} ").fg(t.muted),
    ])
}

/// (line, name, buses) for every line with a bus on the road, in line order.
fn lines_on_road(positions: &[Vehicle]) -> Vec<(String, String, usize)> {
    let mut lines: Vec<(String, String, usize)> = vec![];
    for v in positions {
        match lines.iter_mut().find(|(line, _, _)| *line == v.line) {
            Some(entry) => entry.2 += 1,
            None => lines.push((v.line.clone(), v.line_name.clone(), 1)),
        }
    }
    lines.sort_by_key(|(line, _, _)| api::numeric(line));
    lines
}

/// Arrow for a moving bus, dot for a stopped one (the API sends bearing 0 when stopped).
fn heading(v: &Vehicle) -> &'static str {
    const ARROWS: [&str; 8] = ["↑", "↗", "→", "↘", "↓", "↙", "←", "↖"];
    match (v.bearing, v.speed) {
        (Some(bearing), Some(speed)) if speed > 0.0 => {
            ARROWS[(((bearing + 22.5) / 45.0) as usize) % 8]
        }
        _ => "●",
    }
}

/// Muted hue per line: the operator's all-blue scheme is too close to tell lines apart.
fn line_colour(line: &str) -> Color {
    let hex = match line {
        "1" => 0xd9665b,
        "2" => 0xe0935a,
        "3" => 0xdcc060,
        "4" => 0xa8c95f,
        "5" => 0x63b06a,
        "6" => 0x5bbf9f,
        "7" => 0x5fc8d2,
        "8" => 0x62a3db,
        "9" => 0x6f8be0,
        "10" | "28" => 0x9a8ae0,
        "11" => 0xbb7fd4,
        "12" => 0xd97fb0,
        "13" | "26" => 0xb98a6a,
        "14" => 0xa3a36b,
        "15" => 0xc9c4ba,
        "91" => 0x7f86b8,
        "92" => 0x9a80ac,
        _ => 0x8f8a80,
    };
    if !LIGHT.load(Relaxed) {
        return rgb(hex);
    }
    // The same hues, darkened to read on a light background.
    let [_, r, g, b] = hex.to_be_bytes();
    let dark = |v: u8| u32::from(v) * 7 / 10;
    rgb(dark(r) << 16 | dark(g) << 8 | dark(b))
}

fn badge(line: &str) -> Span<'static> {
    Span::styled(
        format!(" {line:>2} "),
        Style::new().bg(line_colour(line)).fg(theme().bg).bold(),
    )
}

/// A flat surface titled on its first row, no borders.
fn panel(title: &str, active: bool) -> Block<'static> {
    let t = theme();
    let (surface, text) = if active {
        (t.panel, t.primary)
    } else {
        (t.surface, t.fg)
    };
    Block::new()
        .style(Style::new().bg(surface))
        .title(Span::from(format!(" {title} ")).fg(text).bold())
}

fn with_side_panel(area: Rect) -> (Rect, Option<Rect>) {
    if area.width < WIDE {
        return (area, None);
    }
    let main = MAIN_WIDTH.max(area.width * 58 / 100);
    let [left, _, side] = Layout::horizontal([
        Constraint::Length(main),
        Constraint::Length(2),
        Constraint::Min(0),
    ])
    .areas(area);
    (left, Some(side))
}

fn point(stop: &Stop) -> Option<Point> {
    Some((stop.lon?, stop.lat?))
}

fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    text.chars()
        .take(width.saturating_sub(1))
        .chain(['…'])
        .collect()
}

/// `points` minus the outer 5% on each axis: the whole network leaves the centre too cramped.
fn core(points: &[Point]) -> Vec<Point> {
    let range = |axis: fn(&Point) -> f64| {
        let mut values: Vec<f64> = points.iter().map(axis).collect();
        values.sort_by(f64::total_cmp);
        let cut = values.len() / 20;
        values.get(cut).copied().unwrap_or(f64::MIN)
            ..=values.iter().rev().nth(cut).copied().unwrap_or(f64::MAX)
    };
    let (lons, lats) = (range(|p| p.0), range(|p| p.1));
    points
        .iter()
        .copied()
        .filter(|p| lons.contains(&p.0) && lats.contains(&p.1))
        .collect()
}

/// Lon/lat bounds over `points`, widened so the map keeps its shape (braille dots are ~square).
fn fit(points: &[Point], area: Rect) -> ([f64; 2], [f64; 2]) {
    const MIN_SPAN: f64 = 0.006; // about 650 m: a single stop still shows its surroundings
    const CENTRE: Point = (-5.664, 40.965);
    let (west, east) = points.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
        (lo.min(p.0), hi.max(p.0))
    });
    let (south, north) = points.iter().fold((f64::MAX, f64::MIN), |(lo, hi), p| {
        (lo.min(p.1), hi.max(p.1))
    });
    let (mid_lon, mid_lat) = if points.is_empty() {
        CENTRE
    } else {
        ((west + east) / 2.0, (south + north) / 2.0)
    };
    let shrink = mid_lat.to_radians().cos(); // a degree of longitude is shorter than one of latitude
    let mut width = ((east - west) * shrink).max(MIN_SPAN) * 1.12;
    let mut height = (north - south).max(MIN_SPAN) * 1.12;
    let wanted = (f64::from(area.width.max(1)) * 2.0) / (f64::from(area.height.max(1)) * 4.0);
    if width / height < wanted {
        width = height * wanted;
    } else {
        height = width / wanted;
    }
    let half_lon = width / shrink / 2.0;
    (
        [mid_lon - half_lon, mid_lon + half_lon],
        [mid_lat - height / 2.0, mid_lat + height / 2.0],
    )
}

fn config_file(name: &str) -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("salbus").join(name))
}

fn favourites_path() -> Option<PathBuf> {
    config_file("favourites.json")
}

/// Only what the user picked; anything left out follows the system.
#[derive(Default, Serialize, Deserialize)]
struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    theme: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lang: Option<String>,
}

fn load_settings() -> Settings {
    config_file("settings.json")
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_settings(settings: &Settings) {
    let Some(path) = config_file("settings.json") else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, serde_json::to_string(settings).unwrap_or_default());
}

/// The locale variables first; if none says a language, macOS's preferred languages.
fn system_spanish() -> bool {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .find(|v| !v.is_empty() && v != "C" && v != "POSIX" && !v.starts_with("C."));
    if let Some(locale) = locale {
        return locale.starts_with("es");
    }
    cfg!(target_os = "macos")
        && std::process::Command::new("defaults")
            .args(["read", "-g", "AppleLanguages"])
            .output()
            .is_ok_and(|out| {
                let text = String::from_utf8_lossy(&out.stdout);
                text.split('"')
                    .nth(1)
                    .is_some_and(|first| first.starts_with("es"))
            })
}

/// macOS's appearance setting; dark anywhere else, or if it cannot be read.
fn system_light() -> bool {
    cfg!(target_os = "macos")
        && std::process::Command::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
            .is_ok_and(|out| !String::from_utf8_lossy(&out.stdout).contains("Dark"))
}

fn load_favourites() -> Vec<String> {
    let saved: Vec<String> = favourites_path()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    // Ids end up in a URL: keep only stop numbers.
    saved
        .into_iter()
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        .collect()
}

fn save_favourites(favourites: &[String]) {
    let Some(path) = favourites_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, serde_json::to_string(favourites).unwrap_or_default());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_keeps_the_place_under_the_pointer() {
        let area = Rect::new(10, 5, 80, 30);
        let view = View {
            centre: (-5.664, 40.965),
            height: 0.04,
        };
        let frame = |v: View| {
            let (lon, lat) = v.bounds(area);
            MapFrame { area, lon, lat }
        };
        let pointer = Position::new(30, 12);
        let before = frame(view).point(pointer);
        let after = frame(view.zoomed(0.5, before)).point(pointer);
        assert!((before.0 - after.0).abs() < 1e-5 && (before.1 - after.1).abs() < 1e-5);
        assert_eq!(view.zoomed(1e-6, before).height, MIN_HEIGHT);
    }
}

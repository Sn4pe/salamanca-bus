//! Unofficial client for the undocumented JSON API behind salamancadetransportes.com.

use std::fmt;
use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

const BASE: &str = "https://salamancadetransportes.com/api/siri";
/// Steady pace between requests. The site sits behind a per-IP limiter; this stays well under it.
const SPACING: Duration = Duration::from_secs(2);
/// Waits after a 429 before each retry; after the last one the request fails.
const BACKOFF_SECS: [u64; 2] = [8, 16];
/// A 403 is Cloudflare's wall for the whole IP. Send nothing for this long.
const BLOCK_PAUSE: Duration = Duration::from_secs(300);

#[derive(Debug)]
pub enum Error {
    Blocked,
    RateLimited,
    Http(u16),
    Network(String),
    Body(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Blocked => write!(
                f,
                "bloqueado por la web (403); sin peticiones durante 5 min"
            ),
            Error::RateLimited => write!(f, "demasiadas peticiones (429); prueba en un minuto"),
            Error::Http(code) => write!(f, "la web respondió HTTP {code}"),
            Error::Network(e) => write!(f, "fallo de red: {e}"),
            Error::Body(e) => write!(f, "respuesta inesperada de la web: {e}"),
        }
    }
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Stop {
    pub id: String,
    pub name: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub lines: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Line {
    pub id: String,
    pub name: String,
    pub directions: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Vehicle {
    pub vehicle: String,
    pub line: String,
    pub line_name: String,
    pub lat: f64,
    pub lon: f64,
    pub bearing: Option<f64>,
    pub speed: Option<f64>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Arrival {
    pub line: String,
    pub line_name: String,
    pub destination: String,
    /// Live estimate, with UTC offset.
    pub expected: Option<String>,
    /// `aimedArrival`, offset borrowed from `expected`; unreliable, so no delay is derived.
    pub aimed: Option<String>,
    pub minutes: Option<i64>,
    pub distance_m: Option<f64>,
    pub vehicle: Option<String>,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    #[serde(skip)]
    pub expected_epoch: Option<i64>,
}

struct Gate {
    next: Instant,
    blocked_until: Option<Instant>,
}

pub struct Client {
    agent: ureq::Agent,
    gate: Mutex<Gate>,
}

impl Client {
    pub fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .user_agent(concat!("salbus/", env!("CARGO_PKG_VERSION")))
            .timeout_global(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .build()
            .into();
        Client {
            agent,
            gate: Mutex::new(Gate {
                next: Instant::now(),
                blocked_until: None,
            }),
        }
    }

    // One lock held across the request: one in flight, paced. Token bucket if bursts matter.
    fn get(&self, path: &str) -> Result<Vec<Value>, Error> {
        let mut gate = self.gate.lock().unwrap();
        let mut backoffs = BACKOFF_SECS.iter();
        loop {
            if gate
                .blocked_until
                .is_some_and(|until| Instant::now() < until)
            {
                return Err(Error::Blocked);
            }
            sleep(gate.next.saturating_duration_since(Instant::now()));
            let result = self
                .agent
                .get(format!("{BASE}{path}"))
                .header("accept", "application/json")
                .call();
            gate.next = Instant::now() + SPACING;
            let mut response = result.map_err(|e| Error::Network(e.to_string()))?;
            match response.status().as_u16() {
                200 => {
                    let mut body: Value = response
                        .body_mut()
                        .read_json()
                        .map_err(|e| Error::Body(e.to_string()))?;
                    return match body["data"].take() {
                        Value::Array(items) => Ok(items),
                        _ => Err(Error::Body("falta la lista `data`".into())),
                    };
                }
                429 => match backoffs.next() {
                    Some(secs) => sleep(Duration::from_secs(*secs)),
                    None => return Err(Error::RateLimited),
                },
                403 => {
                    gate.blocked_until = Some(Instant::now() + BLOCK_PAUSE);
                    return Err(Error::Blocked);
                }
                code => return Err(Error::Http(code)),
            }
        }
    }

    pub fn stops(&self) -> Result<Vec<Stop>, Error> {
        Ok(parse_stops(&self.get("/stops")?))
    }

    pub fn lines(&self) -> Result<Vec<Line>, Error> {
        let mut lines: Vec<Line> = self
            .get("/lines")?
            .iter()
            .map(|v| Line {
                id: text(v, "lineRef"),
                name: text(v, "lineName"),
                directions: strings(&v["directions"]),
            })
            .collect();
        lines.sort_by_key(|l| numeric(&l.id));
        Ok(lines)
    }

    pub fn positions(&self) -> Result<Vec<Vehicle>, Error> {
        let mut vehicles: Vec<Vehicle> = self
            .get("/positions")?
            .iter()
            .filter_map(|v| {
                let (lat, lon) = (number(v, "latitude")?, number(v, "longitude")?);
                (lat != 0.0 && lon != 0.0).then(|| Vehicle {
                    vehicle: text(v, "vehicleId"),
                    line: text(v, "lineCode"),
                    line_name: text(v, "lineName"),
                    lat,
                    lon,
                    bearing: number(v, "bearing"),
                    speed: number(v, "speed"),
                })
            })
            .collect();
        vehicles.sort_by_key(|v| (numeric(&v.line), numeric(&v.vehicle)));
        Ok(vehicles)
    }

    pub fn arrivals(&self, stop: &str) -> Result<Vec<Arrival>, Error> {
        debug_assert!(stop.bytes().all(|b| b.is_ascii_digit()));
        Ok(parse_arrivals(
            &self.get(&format!("/arrivals?stop={stop}"))?,
            now_epoch(),
        ))
    }
}

pub fn bundled_stops() -> Vec<Stop> {
    let body: Value =
        serde_json::from_str(include_str!("../data/stops.json")).expect("bundled stops.json");
    parse_stops(
        body["data"]
            .as_array()
            .expect("bundled stops.json has data"),
    )
}

fn parse_stops(items: &[Value]) -> Vec<Stop> {
    let mut stops: Vec<Stop> = items
        .iter()
        .map(|v| {
            let mut lines = strings(&v["lineRefs"]);
            lines.sort_by_key(|l| numeric(l));
            lines.dedup();
            Stop {
                id: text(v, "stopRef"),
                name: fix_roman(&text(v, "stopName")),
                lat: number(v, "latitude"),
                lon: number(v, "longitude"),
                lines,
            }
        })
        .filter(|s| !s.id.is_empty())
        .collect();
    stops.sort_by_key(|s| numeric(&s.id));
    stops
}

fn parse_arrivals(items: &[Value], now: i64) -> Vec<Arrival> {
    let mut arrivals: Vec<Arrival> = items
        .iter()
        .map(|v| {
            let expected = Some(text(v, "expectedArrival")).filter(|s| !s.is_empty());
            let bare_aimed = Some(text(v, "aimedArrival")).filter(|s| !s.is_empty());
            let offset = expected.as_deref().and_then(offset_suffix).unwrap_or("");
            let aimed = bare_aimed.map(|a| {
                if offset_suffix(&a).is_some() {
                    a
                } else {
                    format!("{a}{offset}")
                }
            });
            let expected_epoch = expected.as_deref().and_then(epoch);
            Arrival {
                line: text(v, "lineCode"),
                line_name: text(v, "lineName"),
                destination: destination(&text(v, "directionName")),
                minutes: expected_epoch.map(|e| minutes_until(e, now)),
                distance_m: metres(&text(v, "distance")),
                vehicle: Some(text(v, "vehicleId")).filter(|s| !s.is_empty()),
                lat: number(v, "latitude").filter(|n| *n != 0.0),
                lon: number(v, "longitude").filter(|n| *n != 0.0),
                expected,
                aimed,
                expected_epoch,
            }
        })
        .collect();
    arrivals.sort_by_key(|a| a.expected_epoch.unwrap_or(i64::MAX));
    arrivals
}

pub fn minutes_until(epoch: i64, now: i64) -> i64 {
    (((epoch - now) as f64) / 60.0).round().max(0.0) as i64
}

/// Stops whose id or name contains every word of the query, accents and case ignored.
pub fn search<'a>(stops: &'a [Stop], query: &str) -> Vec<&'a Stop> {
    let query = fold(query);
    let words: Vec<&str> = query.split_whitespace().collect();
    let mut found: Vec<(&Stop, String)> = stops
        .iter()
        .map(|s| (s, haystack(&s.name)))
        .filter(|(s, name)| words.iter().all(|w| name.contains(w) || s.id == *w))
        .collect();
    found.sort_by_key(|(s, name)| (s.id != query, !name.starts_with(&query)));
    found.into_iter().map(|(s, _)| s).collect()
}

/// Folded name, then the same with abbreviations spelled out, so "avda" and "avenida" both match.
fn haystack(name: &str) -> String {
    const SPELLED: [(&str, &str); 10] = [
        ("c/mno.", "camino"),
        ("c/", "calle "),
        ("avda.", "avenida"),
        ("av.", "avenida"),
        ("pº.", "paseo"),
        ("pº", "paseo"),
        ("pza.", "plaza"),
        ("ctra.", "carretera"),
        ("gta.", "glorieta"),
        ("fren.", "frente"),
    ];
    let folded = fold(name);
    let spelled = SPELLED
        .iter()
        .fold(folded.clone(), |s, (short, full)| s.replace(short, full));
    format!("{folded} {spelled}")
}

pub fn fold(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' => 'a',
            'é' | 'è' => 'e',
            'í' | 'ï' => 'i',
            'ó' | 'ò' => 'o',
            'ú' | 'ü' => 'u',
            'ñ' => 'n',
            c => c,
        })
        .collect()
}

/// The API title-cases names, turning "Juan Pablo II" into "Juan Pablo Ii".
fn fix_roman(name: &str) -> String {
    name.split(' ')
        .map(|word| {
            let core = word.trim_end_matches(|c: char| !c.is_alphanumeric());
            let mut chars = core.chars();
            let is_numeral = core.len() >= 2
                && chars.next().is_some_and(|c| "IVX".contains(c))
                && chars.all(|c| "ivx".contains(c));
            if is_numeral {
                word.to_uppercase()
            } else {
                word.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// "Zurguén (196) > Capuchinos (317)" → "Capuchinos".
fn destination(direction: &str) -> String {
    let last = direction.rsplit('>').next().unwrap_or("").trim();
    let name = match last.rfind('(') {
        Some(open) if last.ends_with(')') => &last[..open],
        _ => last,
    };
    name.trim().to_owned()
}

/// "367 m" → 367.0, "1,2 km" → 1200.0.
fn metres(distance: &str) -> Option<f64> {
    let digits: String = distance
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
        .collect();
    let value: f64 = digits.replace(',', ".").parse().ok()?;
    Some(if distance.trim_end().ends_with("km") {
        value * 1000.0
    } else {
        value
    })
}

fn text(v: &Value, key: &str) -> String {
    match &v[key] {
        Value::String(s) => s.trim().to_owned(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn number(v: &Value, key: &str) -> Option<f64> {
    text(v, key).parse().ok()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Sort key so "10" follows "9"; non-numeric ids go last.
pub fn numeric(id: &str) -> u64 {
    id.parse().unwrap_or(u64::MAX)
}

pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// "+01:00", "-03:30" or "Z" at the end of a timestamp.
fn offset_suffix(s: &str) -> Option<&str> {
    if s.ends_with('Z') {
        return s.get(s.len() - 1..);
    }
    let tail = s.get(s.len().checked_sub(6)?..)?;
    let b = tail.as_bytes();
    (matches!(b[0], b'+' | b'-') && b[3] == b':').then_some(tail)
}

/// RFC 3339 timestamp with offset → Unix seconds.
fn epoch(s: &str) -> Option<i64> {
    let field = |range: std::ops::Range<usize>| s.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
    let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);
    let offset = match offset_suffix(s)? {
        "Z" => 0,
        o => {
            let sign = if o.starts_with('-') { -1 } else { 1 };
            sign * (o[1..3].parse::<i64>().ok()? * 3600 + o[4..6].parse::<i64>().ok()? * 60)
        }
    };
    // days_from_civil, Howard Hinnant's algorithm
    let y = if month <= 2 { year - 1 } else { year };
    let (era, yoe) = (y.div_euclid(400), y.rem_euclid(400));
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// Unix seconds → "2026-11-20T20:20:07Z".
pub fn utc_iso(epoch: i64) -> String {
    let (days, secs) = (epoch.div_euclid(86_400), epoch.rem_euclid(86_400));
    // civil_from_days, Howard Hinnant's algorithm
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (day, month) = (
        doy - (153 * mp + 2) / 5 + 1,
        if mp < 10 { mp + 3 } else { mp - 9 },
    );
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_round_trips() {
        assert_eq!(epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch("2026-11-20T21:20:07+01:00"), Some(1_795_206_007));
        assert_eq!(utc_iso(1_795_206_007), "2026-11-20T20:20:07Z");
        assert_eq!(
            epoch("2024-02-29T23:59:59-03:30").map(utc_iso).as_deref(),
            Some("2024-03-01T03:29:59Z")
        );
        assert_eq!(epoch("2026-11-20T21:25:00"), None);
    }

    #[test]
    fn parses_a_real_arrivals_response() {
        let body: Value = serde_json::from_str(include_str!("../tests/arrivals.json")).unwrap();
        let now = epoch("2026-11-20T21:17:00+01:00").unwrap();
        let arrivals = parse_arrivals(body["data"].as_array().unwrap(), now);
        assert_eq!(arrivals.len(), 3);
        let first = &arrivals[0];
        assert_eq!(first.line, "7");
        assert_eq!(first.destination, "Hospital Universitario");
        assert_eq!(first.expected.as_deref(), Some("2026-11-20T21:20:07+01:00"));
        assert_eq!(first.aimed.as_deref(), Some("2026-11-20T21:25:00+01:00"));
        assert_eq!(first.minutes, Some(3));
        assert_eq!(first.distance_m, Some(367.0));
        assert!(
            arrivals
                .windows(2)
                .all(|w| w[0].expected_epoch <= w[1].expected_epoch)
        );
    }

    #[test]
    fn bundled_stops_are_clean_and_searchable() {
        let stops = bundled_stops();
        assert!(stops.len() > 300);
        assert!(
            stops
                .iter()
                .all(|s| s.lines.windows(2).all(|w| w[0] != w[1]))
        );
        let found = search(&stops, "juan pablo");
        assert!(!found.is_empty() && found.iter().all(|s| s.name.contains("Juan Pablo II")));
        assert_eq!(search(&stops, "MADROÑOS")[0].id, "1");
        assert_eq!(search(&stops, "109")[0].id, "109");
        assert_eq!(search(&stops, "paseo madronos")[0].id, "1");
        assert!(!search(&stops, "plaza españa").is_empty());
        assert!(!search(&stops, "avenida lasalle").is_empty());
        assert_eq!(fix_roman("Guardia Civil, 3"), "Guardia Civil, 3");
        assert_eq!(metres("1,2 km"), Some(1200.0));
        assert_eq!(
            destination("Zurguén (196) > Capuchinos (317)"),
            "Capuchinos"
        );
    }
}

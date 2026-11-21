mod api;
mod tui;

use std::io::{self, IsTerminal, Write};
use std::process::exit;
use std::thread::sleep;
use std::time::Duration;

use clap::{CommandFactory, Parser, Subcommand};
use serde_json::{Value, json};

use api::{Client, Error, Stop};

#[derive(Parser)]
#[command(
    name = "salbus",
    version,
    about = "Autobuses urbanos de Salamanca en tiempo real. No oficial.\nCada comando imprime JSON; sin comando se abre la TUI."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Paradas que casan con un nombre o número (sin acentos ni mayúsculas)
    Stops {
        query: Vec<String>,
        /// Pide la lista actual a la web en vez de usar la incluida
        #[arg(long)]
        refresh: bool,
    },
    /// Próximos autobuses en una parada (número o nombre)
    Arrivals {
        stop: String,
        /// Solo esta línea
        #[arg(short, long)]
        line: Option<String>,
    },
    /// Una línea JSON cada vez que cambian las llegadas de una parada
    Watch {
        stop: String,
        #[arg(short, long)]
        line: Option<String>,
        /// Segundos entre consultas (mínimo 10)
        #[arg(short, long, default_value_t = 30)]
        interval: u64,
    },
    /// Líneas de la red
    Lines,
    /// Posición en vivo de los autobuses
    Positions {
        #[arg(short, long)]
        line: Option<String>,
    },
    /// Interfaz de terminal: paradas, llegadas y mapa
    Tui,
}

fn main() {
    let cli = Cli::parse();
    if cli.command.is_none() && !io::stdout().is_terminal() {
        // Piped or run by a script: a full-screen UI would only print escape codes.
        let _ = Cli::command().print_help();
        exit(2);
    }
    let client = Client::new();
    let result = match cli.command.unwrap_or(Command::Tui) {
        Command::Tui => return tui::run(client).unwrap_or_else(|e| fail(&e.to_string(), json!({}), 1)),
        Command::Stops { query, refresh } => stops(&client, &query.join(" "), refresh),
        Command::Arrivals { stop, line } => {
            let stop = resolve(&stop);
            arrivals(&client, &stop, line.as_deref())
        }
        Command::Watch { stop, line, interval } => watch(&client, &resolve(&stop), line.as_deref(), interval.max(10)),
        Command::Lines => client.lines().map(|lines| json!({"count": lines.len(), "lines": lines})),
        Command::Positions { line } => client.positions().map(|mut vehicles| {
            vehicles.retain(|v| line.as_ref().is_none_or(|l| &v.line == l));
            json!({"fetched_at": api::utc_iso(api::now_epoch()), "count": vehicles.len(), "vehicles": vehicles})
        }),
    };
    match result {
        Ok(value) => print_line(&serde_json::to_string_pretty(&value).unwrap()),
        Err(e) => fail(&e.to_string(), json!({}), exit_code(&e)),
    }
}

fn stops(client: &Client, query: &str, refresh: bool) -> Result<Value, Error> {
    let all = if refresh {
        client.stops()?
    } else {
        api::bundled_stops()
    };
    let found = api::search(&all, query);
    Ok(json!({"count": found.len(), "stops": found}))
}

fn arrivals(client: &Client, stop: &Stop, line: Option<&str>) -> Result<Value, Error> {
    let mut arrivals = client.arrivals(&stop.id)?;
    arrivals.retain(|a| line.is_none_or(|l| a.line == l));
    Ok(json!({
        "fetched_at": api::utc_iso(api::now_epoch()),
        "stop": stop,
        "notice": stop.name.is_empty().then_some(
            "Esa parada no está en la lista incluida; puede que no exista. Prueba `salbus stops --refresh`."
        ),
        "count": arrivals.len(),
        "arrivals": arrivals,
    }))
}

fn watch(client: &Client, stop: &Stop, line: Option<&str>, interval: u64) -> Result<Value, Error> {
    let mut last = Value::Null;
    loop {
        match arrivals(client, stop, line) {
            Ok(now) if now["arrivals"] != last => {
                print_line(&now.to_string());
                last = now["arrivals"].clone();
            }
            Ok(_) => {}
            Err(e @ Error::Blocked) => return Err(e),
            Err(e) => eprintln!("{}", json!({"error": e.to_string()})),
        }
        sleep(Duration::from_secs(interval));
    }
}

/// A stop number is taken as is; anything else must match exactly one bundled stop.
fn resolve(arg: &str) -> Stop {
    let all = api::bundled_stops();
    if !arg.is_empty() && arg.bytes().all(|b| b.is_ascii_digit()) {
        return all
            .iter()
            .find(|s| s.id == arg)
            .cloned()
            .unwrap_or_else(|| Stop {
                id: arg.to_owned(),
                name: String::new(),
                lat: None,
                lon: None,
                lines: vec![],
            });
    }
    match api::search(&all, arg)[..] {
        [one] => one.clone(),
        [] => fail(&format!("ninguna parada casa con «{arg}»"), json!({}), 1),
        ref many => fail(
            &format!("{} paradas casan con «{arg}»; usa el número", many.len()),
            json!({"stops": many.iter().take(20).collect::<Vec<_>>()}),
            1,
        ),
    }
}

/// stdout closed (`salbus watch 28 | head -1`) is a normal way to stop, not a panic.
fn print_line(text: &str) {
    if writeln!(io::stdout(), "{text}").is_err() {
        exit(0);
    }
}

fn exit_code(error: &Error) -> i32 {
    match error {
        Error::Blocked | Error::RateLimited => 3,
        _ => 1,
    }
}

fn fail(message: &str, mut extra: Value, code: i32) -> ! {
    extra["error"] = json!(message);
    eprintln!("{extra}");
    exit(code)
}

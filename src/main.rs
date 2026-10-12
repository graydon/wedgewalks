use wedgewalks::{cities, config, graph, home, output, pipeline, plan, shade, transit};

use config::Config;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Instant;

const USAGE: &str =
    "usage: wedgewalks [--config FILE] [--cities FILE] [--data DIR] [--out DIR] [gui | CITY... | all | bench CITY...]

With no city (or `gui`), opens the interactive planner. Files default to the
current directory if it has a cities.csv, else to a per-user folder (created
and filled with the defaults on first run).

Reads DIR/<city>.osm.pbf (lowercase name from the cities file) and writes
OUT/<city>/: report.txt, itinerary.html, day-N.gpx and ring.gpx for the
best ring, and {graph,rings,arcs}.geojson. `bench` times each pipeline stage.";

struct Args {
    config: PathBuf,
    cities: PathBuf,
    data: PathBuf,
    out: PathBuf,
    names: Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let home = home::dir();
    let mut a = Args {
        config: home.join("config.toml"),
        cities: home.join("cities.csv"),
        data: home.join("data"),
        out: home.join("out"),
        names: vec![],
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = || it.next().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--config" => a.config = val()?.into(),
            "--cities" => a.cities = val()?.into(),
            "--data" => a.data = val()?.into(),
            "--out" => a.out = val()?.into(),
            "-h" | "--help" => return Err(USAGE.into()),
            s if s.starts_with('-') => return Err(format!("unknown option {s}\n{USAGE}")),
            _ => a.names.push(arg),
        }
    }
    if a.config.starts_with(&home) {
        home::seed(&home, &a.config, &a.cities);
    }
    Ok(a)
}

fn run_city(city: &cities::City, cfg: &Config, data: &Path, out: &Path) -> Result<(), String> {
    let dir = out.join(city.key());
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let t0 = Instant::now();
    let log = |msg: &str| eprintln!("[{}] {msg} ({:.1}s)", city.name, t0.elapsed().as_secs_f64());
    let (built, plan) = pipeline::plan_city(city, cfg, data, &AtomicBool::new(false), &log)?;
    let text = output::write_all(&dir, &city.name, &built, &plan, cfg, 0)?;
    println!("{text}");
    log(&format!("wrote {}", dir.display()));
    Ok(())
}

/// Times each pipeline stage, for profiling.
fn bench(city: &cities::City, cfg: &Config, data: &Path) -> Result<(), String> {
    let time = |label: &str, t: Instant| {
        eprintln!("{:<10} {label:<12} {:>7.0} ms", city.name, t.elapsed().as_secs_f64() * 1e3)
    };
    let t = Instant::now();
    let loaded = pipeline::load_city(city, data, city.lat, city.lon, cfg, &|_| {})?;
    time("load", t);
    let proj = graph::Proj::new(city.lat, city.lon);
    let t = Instant::now();
    let _ = graph::Graph::build(&loaded.osm, &proj, &graph::BuildOpts::from(cfg));
    time("graph", t);
    let t = Instant::now();
    let _ = transit::Transit::build(&loaded.osm, &proj, cfg);
    time("transit", t);
    let t = Instant::now();
    let _ = shade::Shade::build(&loaded.osm, &proj, cfg.surroundings_m(), cfg.shade_near_m);
    time("shade", t);
    let t = Instant::now();
    let built = pipeline::build(&loaded, city.lat, city.lon, cfg);
    time("build(all)", t);
    for _ in 0..2 {
        let t = Instant::now();
        let raw = plan::search(&built, cfg, &AtomicBool::new(false));
        time("plan", t);
        let t = Instant::now();
        let plan = pipeline::select(&built, &raw, cfg);
        time("select", t);
        // Re-routing a plan edited by hand, as after dragging an anchor.
        if let Some(r) = plan.rings.first() {
            let m = wedgewalks::manual::Manual::from_ring(r, &built.proj, "bench".into(), cfg.travel);
            let t = Instant::now();
            let (rings, errors) = plan::manual_rings(&built, cfg, &[m]);
            time("manual", t);
            if !errors.is_empty() || rings.len() != 1 {
                eprintln!("{:<10} manual plan failed: {errors:?}", city.name);
            }
        }
    }
    Ok(())
}

fn find<'a>(all: &'a [cities::City], name: &str, file: &Path) -> Result<&'a cities::City, String> {
    all.iter().find(|c| c.name.eq_ignore_ascii_case(name)).ok_or(format!("{name}: not in {}", file.display()))
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    if args.names.is_empty() || args.names == ["gui"] {
        let opts =
            wedgewalks::gui::Options { data: args.data, cities: args.cities, config: args.config, out: args.out };
        if let Err(e) = wedgewalks::gui::run(opts) {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        return;
    }
    let run = || -> Result<(), String> {
        let cfg = Config::load(&args.config)?;
        let all = cities::load(&args.cities)?;
        if args.names.first().map(String::as_str) == Some("bench") {
            for n in &args.names[1..] {
                bench(find(&all, n, &args.cities)?, &cfg, &args.data)?;
            }
            return Ok(());
        }
        let selected: Vec<&cities::City> = if args.names.iter().any(|n| n == "all") {
            all.iter().collect()
        } else {
            args.names.iter().map(|n| find(&all, n, &args.cities)).collect::<Result<_, _>>()?
        };
        for city in selected {
            if let Err(e) = run_city(city, &cfg, &args.data, &args.out) {
                eprintln!("[{}] error: {e}", city.name);
            }
        }
        Ok(())
    };
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

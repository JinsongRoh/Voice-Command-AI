//! Connection soak probe (M0 PoC-5): pairs with a server, then sends one
//! cheap request per interval over the real Noise channel and records
//! round-trip times, lost requests and how long reconnects take.
//!
//! ```text
//! cargo run --release -p vca-client --example netprobe -- \
//!     --uri 'vca://pair?...' --data <dir> --minutes 120 --label tailscale
//! ```
//!
//! Prints one JSON summary at the end and writes `<data>/<label>-rtt.csv`
//! (`elapsed_ms,rtt_ms`) and `<data>/<label>-links.csv` (status changes).

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vca_client::{ClientHub, LinkSink, LinkState, LinkStatus};
use vca_core::{ClientMsg, Envelope, ServerMsg};
use vca_net::FileKeyStore;

struct Args {
    uri: Option<String>,
    data: PathBuf,
    minutes: f64,
    interval: Duration,
    label: String,
}

fn parse_args() -> Args {
    let mut args = Args {
        uri: None,
        data: PathBuf::from("netprobe-data"),
        minutes: 1.0,
        interval: Duration::from_millis(1000),
        label: "probe".into(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = || it.next().unwrap_or_else(|| panic!("{a} needs a value"));
        match a.as_str() {
            "--uri" => args.uri = Some(value()),
            "--data" => args.data = PathBuf::from(value()),
            "--minutes" => args.minutes = value().parse().expect("--minutes"),
            "--interval-ms" => {
                args.interval = Duration::from_millis(value().parse().expect("--interval-ms"))
            }
            "--label" => args.label = value(),
            other => panic!("unknown argument {other}"),
        }
    }
    args
}

#[derive(Default)]
struct Shared {
    pending: HashMap<u64, Instant>,
    rtts: Vec<(u128, f64)>,
    links: Vec<(u128, LinkStatus)>,
    connected: bool,
}

struct Sink {
    start: Instant,
    shared: Arc<Mutex<Shared>>,
}

impl LinkSink for Sink {
    fn status(&self, status: LinkStatus) {
        let mut s = self.shared.lock().unwrap();
        s.connected = status.state == LinkState::Connected;
        if !s.connected {
            // Requests in flight are lost with the connection.
            s.pending.clear();
        }
        s.links.push((self.start.elapsed().as_millis(), status));
    }

    fn message(&self, msgpack: Vec<u8>) {
        let Ok(env) = rmp_serde::from_slice::<Envelope<ServerMsg>>(&msgpack) else {
            return;
        };
        let mut s = self.shared.lock().unwrap();
        if let Some(sent) = s.pending.remove(&env.id) {
            let rtt = sent.elapsed().as_secs_f64() * 1000.0;
            let at = self.start.elapsed().as_millis();
            s.rtts.push((at, rtt));
        }
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx]
}

#[tokio::main]
async fn main() {
    let args = parse_args();
    std::fs::create_dir_all(&args.data).expect("data dir");
    let secrets = Arc::new(FileKeyStore::new(args.data.join("secrets")));
    let hub = ClientHub::open(args.data.join("machines"), secrets).expect("open hub");
    let machine = match (&args.uri, hub.machines().first()) {
        (Some(uri), _) => hub
            .pair(uri, &format!("netprobe-{}", args.label), "probe", None)
            .await
            .expect("pair"),
        (None, Some(m)) => m.clone(),
        (None, None) => panic!("no paired machine; pass --uri"),
    };

    let start = Instant::now();
    let shared = Arc::new(Mutex::new(Shared::default()));
    hub.connect(
        &machine.id,
        Arc::new(Sink {
            start,
            shared: shared.clone(),
        }),
    )
    .expect("connect");

    let deadline = start + Duration::from_secs_f64(args.minutes * 60.0);
    let mut ticker = tokio::time::interval(args.interval);
    let (mut sent, mut skipped) = (0u64, 0u64);
    let mut id = 0u64;
    while Instant::now() < deadline {
        ticker.tick().await;
        if !shared.lock().unwrap().connected {
            skipped += 1;
            continue;
        }
        id += 1;
        let bytes = rmp_serde::to_vec_named(&Envelope {
            id,
            body: ClientMsg::ListSessions,
        })
        .expect("encode");
        shared.lock().unwrap().pending.insert(id, Instant::now());
        if hub.send(&machine.id, &bytes).await.is_ok() {
            sent += 1;
        } else {
            shared.lock().unwrap().pending.remove(&id);
            skipped += 1;
        }
    }
    // Let the last replies arrive.
    tokio::time::sleep(Duration::from_secs(2)).await;
    hub.disconnect(&machine.id);

    let s = shared.lock().unwrap();
    let mut rtts: Vec<f64> = s.rtts.iter().map(|(_, r)| *r).collect();
    rtts.sort_by(|a, b| a.partial_cmp(b).unwrap());

    // Outages: from leaving Connected (after the first connect) to the next Connected.
    let mut outages = Vec::new();
    let mut first_connect_ms = None;
    let mut down_since: Option<u128> = None;
    for (at, st) in &s.links {
        match st.state {
            LinkState::Connected => {
                if first_connect_ms.is_none() {
                    first_connect_ms = Some(*at);
                }
                if let Some(d) = down_since.take() {
                    outages.push(serde_json::json!({
                        "at_s": d as f64 / 1000.0,
                        "reconnect_ms": at - d,
                    }));
                }
            }
            _ if first_connect_ms.is_some() && down_since.is_none() => down_since = Some(*at),
            _ => {}
        }
    }

    let write_csv = |name: &str, lines: Vec<String>| {
        let path = args.data.join(format!("{}-{name}.csv", args.label));
        let mut f = std::fs::File::create(&path).expect("csv");
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    };
    write_csv(
        "rtt",
        std::iter::once("elapsed_ms,rtt_ms".to_owned())
            .chain(s.rtts.iter().map(|(at, r)| format!("{at},{r:.3}")))
            .collect(),
    );
    write_csv(
        "links",
        std::iter::once("elapsed_ms,state,code".to_owned())
            .chain(s.links.iter().map(|(at, st)| {
                format!("{at},{:?},{}", st.state, st.code.as_deref().unwrap_or(""))
            }))
            .collect(),
    );

    let summary = serde_json::json!({
        "label": args.label,
        "endpoint": machine.endpoints,
        "minutes": args.minutes,
        "first_connect_ms": first_connect_ms,
        "requests_sent": sent,
        "replies": rtts.len(),
        "lost_in_flight": sent.saturating_sub(rtts.len() as u64),
        "ticks_while_disconnected": skipped,
        "rtt_ms": {
            "p50": percentile(&rtts, 0.50),
            "p95": percentile(&rtts, 0.95),
            "p99": percentile(&rtts, 0.99),
            "max": rtts.last().copied().unwrap_or(f64::NAN),
        },
        "outages": outages,
    });
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
}

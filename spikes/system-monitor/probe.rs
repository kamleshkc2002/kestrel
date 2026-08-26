// Kestrel Phase 0 spike: system-monitor capability probe (read-only).
// std-only; compiled with rustc, NOT a Cargo workspace member.
// Emits a structured JSON report: capability status + evidence + remediation.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
enum Json {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn render(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
            Json::Int(v) => out.push_str(&v.to_string()),
            Json::Float(v) => {
                let mut s = format!("{:.6}", v);
                while s.ends_with('0') && s.contains('.') {
                    s.pop();
                }
                if s.ends_with('.') {
                    s.pop();
                }
                out.push_str(&s);
            }
            Json::Str(v) => {
                out.push('"');
                for c in v.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        c if (c as u32) < 0x20 => {
                            out.push_str(&format!("\\u{:04x}", c as u32));
                        }
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            Json::Arr(items) => {
                out.push('[');
                for (idx, item) in items.iter().enumerate() {
                    if idx > 0 {
                        out.push(',');
                    }
                    item.render(out);
                }
                out.push(']');
            }
            Json::Obj(entries) => {
                out.push('{');
                for (idx, (k, v)) in entries.iter().enumerate() {
                    if idx > 0 {
                        out.push(',');
                    }
                    Json::Str(k.clone()).render(out);
                    out.push(':');
                    v.render(out);
                }
                out.push('}');
            }
        }
    }

    fn to_string(&self) -> String {
        let mut s = String::new();
        self.render(&mut s);
        s
    }
}

fn s(v: impl Into<String>) -> Json {
    Json::Str(v.into())
}
fn i(v: i64) -> Json {
    Json::Int(v)
}
fn f(v: f64) -> Json {
    Json::Float(v)
}
fn b(v: bool) -> Json {
    Json::Bool(v)
}
fn arr(v: Vec<Json>) -> Json {
    Json::Arr(v)
}
fn obj(v: Vec<(String, Json)>) -> Json {
    Json::Obj(v)
}

fn read_trim(path: &Path) -> Result<String, String> {
    fs::read_to_string(path)
        .map(|t| t.trim().to_string())
        .map_err(|e| format!("{}: {}", path.display(), e))
}

fn read_opt(path: &Path) -> Option<String> {
    read_trim(path).ok().filter(|t| !t.is_empty())
}

fn read_i64(path: &Path) -> Result<i64, String> {
    let t = read_trim(path)?;
    t.parse::<i64>()
        .map_err(|e| format!("{}: parse int '{}': {}", path.display(), t, e))
}

fn numeric_tail(name: &str) -> u64 {
    let digits: String = name
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.chars().rev().collect::<String>().parse().unwrap_or(0)
}

fn list_matching(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(prefix) {
                out.push(entry.path());
            }
        }
    }
    out.sort_by_key(|p| {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        (numeric_tail(&name), name)
    });
    out
}

fn canonical_device(path: &Path) -> Option<String> {
    fs::canonicalize(path.join("device"))
        .ok()
        .map(|p| p.to_string_lossy().replace("/sys/", ""))
}

fn parse_kv(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once(':') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

fn status_parts(st: &str) -> (String, Option<String>) {
    for (code, prefix) in [("Limited", "Limited { reason: "), ("Unsupported", "Unsupported { reason: ")] {
        if let Some(rest) = st.strip_prefix(prefix) {
            if let Some(inner) = rest.strip_suffix(" }") {
                return (code.to_string(), Some(inner.to_string()));
            }
        }
    }
    (st.to_string(), None)
}

fn capability(
    id: &str,
    status: &str,
    summary: &str,
    backend: &str,
    alternatives: Vec<&str>,
    remediation: Option<&str>,
    evidence: Vec<Json>,
) -> Json {
    let (status_code, reason) = status_parts(status);
    let mut entries = vec![
        ("feature_id".to_string(), s(id)),
        ("status".to_string(), s(status_code)),
        ("summary".to_string(), s(summary)),
        ("selected_backend".to_string(), s(backend)),
        (
            "alternatives_considered".to_string(),
            arr(alternatives.into_iter().map(s).collect()),
        ),
        (
            "remediation".to_string(),
            match remediation {
                Some(r) => s(r),
                None => Json::Null,
            },
        ),
        ("evidence".to_string(), arr(evidence)),
    ];
    if let Some(r) = reason {
        entries.push(("reason".to_string(), s(r)));
    }
    obj(entries)
}

struct Ctx {
    issues: Vec<Json>,
}

impl Ctx {
    fn issue(&mut self, path: &str, state: &str, detail: String, remediation: &str) {
        self.issues.push(obj(vec![
            ("path".to_string(), s(path)),
            ("state".to_string(), s(state)),
            ("detail".to_string(), s(detail)),
            ("remediation".to_string(), s(remediation)),
        ]));
    }
}

fn bench_read(path: &Path, iterations: u32) -> Json {
    let _ = fs::read_to_string(path); // warm page cache / open path
    let mut ns = Vec::with_capacity(iterations as usize);
    for _ in 0..iterations {
        let t0 = Instant::now();
        let _ = fs::read_to_string(path);
        ns.push(t0.elapsed().as_nanos() as f64);
    }
    ns.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| {
        let idx = ((ns.len() - 1) as f64 * p).round() as usize;
        ns[idx] / 1000.0
    };
    let mean = ns.iter().sum::<f64>() / ns.len() as f64 / 1000.0;
    obj(vec![
        ("path".to_string(), s(path.to_string_lossy())),
        ("iterations".to_string(), i(iterations as i64)),
        ("mean_us".to_string(), f(mean)),
        ("min_us".to_string(), f(ns[0] / 1000.0)),
        ("p50_us".to_string(), f(pct(0.50))),
        ("p90_us".to_string(), f(pct(0.90))),
        ("p99_us".to_string(), f(pct(0.99))),
        ("max_us".to_string(), f(ns[ns.len() - 1] / 1000.0)),
    ])
}

fn probe_proc_stat(ctx: &mut Ctx) -> Json {
    let path = Path::new("/proc/stat");
    match read_trim(path) {
        Ok(text) => {
            let mut agg: Vec<i64> = Vec::new();
            let mut per_cpu = 0u32;
            for line in text.lines() {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.is_empty() {
                    continue;
                }
                if fields[0] == "cpu" {
                    agg = fields[1..]
                        .iter()
                        .filter_map(|x| x.parse::<i64>().ok())
                        .collect();
                } else if fields[0].starts_with("cpu") {
                    per_cpu += 1;
                }
            }
            if agg.len() >= 4 {
                let total: i64 = agg.iter().sum();
                let idle = agg[3] + agg.get(4).copied().unwrap_or(0);
                obj(vec![
                    ("present".to_string(), b(true)),
                    ("ok".to_string(), b(true)),
                    ("per_cpu_lines".to_string(), i(per_cpu as i64)),
                    ("total_jiffies".to_string(), i(total)),
                    ("used_jiffies".to_string(), i(total - idle)),
                    ("idle_jiffies".to_string(), i(idle)),
                ])
            } else {
                ctx.issue("/proc/stat", "malformed", "no parseable aggregate cpu line".into(), "fall back to /proc/loadavg for coarse CPU pressure; log a diagnostic event");
                obj(vec![
                    ("present".to_string(), b(true)),
                    ("ok".to_string(), b(false)),
                    ("per_cpu_lines".to_string(), i(per_cpu as i64)),
                ])
            }
        }
        Err(e) => {
            ctx.issue("/proc/stat", "unreadable", e.clone(), "verify procfs is mounted; run unprivileged in a normal session");
            obj(vec![
                ("present".to_string(), b(false)),
                ("ok".to_string(), b(false)),
                ("error".to_string(), s(e)),
            ])
        }
    }
}

fn probe_proc_meminfo(ctx: &mut Ctx) -> Json {
    let path = Path::new("/proc/meminfo");
    match read_trim(path) {
        Ok(text) => {
            let map = parse_kv(&text);
            let field = |k: &str| {
                map.get(k)
                    .and_then(|v| v.split_whitespace().next())
                    .and_then(|v| v.parse::<i64>().ok())
            };
            let mem_total = field("MemTotal");
            let mem_available = field("MemAvailable");
            let ok = mem_total.is_some() && mem_available.is_some();
            if !ok {
                ctx.issue("/proc/meminfo", "malformed", "missing MemTotal/MemAvailable".into(), "log a diagnostic event and hide memory metrics");
            }
            obj(vec![
                ("present".to_string(), b(true)),
                ("ok".to_string(), b(ok)),
                ("MemTotal_kB".to_string(), mem_total.map(i).unwrap_or(Json::Null)),
                ("MemAvailable_kB".to_string(), mem_available.map(i).unwrap_or(Json::Null)),
                ("MemFree_kB".to_string(), field("MemFree").map(i).unwrap_or(Json::Null)),
                ("Buffers_kB".to_string(), field("Buffers").map(i).unwrap_or(Json::Null)),
                ("Cached_kB".to_string(), field("Cached").map(i).unwrap_or(Json::Null)),
                ("SwapTotal_kB".to_string(), field("SwapTotal").map(i).unwrap_or(Json::Null)),
                ("SwapFree_kB".to_string(), field("SwapFree").map(i).unwrap_or(Json::Null)),
            ])
        }
        Err(e) => {
            ctx.issue("/proc/meminfo", "unreadable", e.clone(), "verify procfs is mounted; run unprivileged in a normal session");
            obj(vec![("present".to_string(), b(false)), ("ok".to_string(), b(false)), ("error".to_string(), s(e))])
        }
    }
}

fn probe_proc_loadavg(ctx: &mut Ctx) -> Json {
    let path = Path::new("/proc/loadavg");
    match read_trim(path) {
        Ok(text) => {
            let parts: Vec<&str> = text.split_whitespace().collect();
            let ok = parts.len() >= 3;
            if !ok {
                ctx.issue("/proc/loadavg", "malformed", text.clone(), "hide load metrics if the format changes");
            }
            obj(vec![
                ("present".to_string(), b(true)),
                ("ok".to_string(), b(ok)),
                ("load1".to_string(), parts.get(0).and_then(|v| v.parse::<f64>().ok()).map(f).unwrap_or(Json::Null)),
                ("load5".to_string(), parts.get(1).and_then(|v| v.parse::<f64>().ok()).map(f).unwrap_or(Json::Null)),
                ("load15".to_string(), parts.get(2).and_then(|v| v.parse::<f64>().ok()).map(f).unwrap_or(Json::Null)),
                ("raw".to_string(), s(text)),
            ])
        }
        Err(e) => {
            ctx.issue("/proc/loadavg", "unreadable", e.clone(), "verify procfs is mounted");
            obj(vec![("present".to_string(), b(false)), ("ok".to_string(), b(false)), ("error".to_string(), s(e))])
        }
    }
}

fn probe_proc_uptime(ctx: &mut Ctx) -> Json {
    let path = Path::new("/proc/uptime");
    match read_trim(path) {
        Ok(text) => {
            let parts: Vec<&str> = text.split_whitespace().collect();
            let ok = parts.len() >= 2;
            if !ok {
                ctx.issue("/proc/uptime", "malformed", text.clone(), "hide uptime metric if format changes");
            }
            obj(vec![
                ("present".to_string(), b(true)),
                ("ok".to_string(), b(ok)),
                ("up_seconds".to_string(), parts.get(0).and_then(|v| v.parse::<f64>().ok()).map(f).unwrap_or(Json::Null)),
                ("idle_seconds".to_string(), parts.get(1).and_then(|v| v.parse::<f64>().ok()).map(f).unwrap_or(Json::Null)),
            ])
        }
        Err(e) => {
            ctx.issue("/proc/uptime", "unreadable", e.clone(), "verify procfs is mounted");
            obj(vec![("present".to_string(), b(false)), ("ok".to_string(), b(false)), ("error".to_string(), s(e))])
        }
    }
}

fn probe_net_dev(ctx: &mut Ctx) -> Json {
    let path = Path::new("/proc/net/dev");
    match read_trim(path) {
        Ok(text) => {
            let mut ifaces = Vec::new();
            for line in text.lines().skip(2) {
                let Some((name, rest)) = line.split_once(':') else {
                    continue;
                };
                let name = name.trim().to_string();
                let nums: Vec<i64> = rest
                    .split_whitespace()
                    .filter_map(|x| x.parse::<i64>().ok())
                    .collect();
                if nums.len() < 16 {
                    continue;
                }
                let oper = read_opt(Path::new(&format!("/sys/class/net/{}/operstate", name)));
                let carrier = read_opt(Path::new(&format!("/sys/class/net/{}/carrier", name)));
                ifaces.push(obj(vec![
                    ("iface".to_string(), s(name)),
                    ("rx_bytes".to_string(), i(nums[0])),
                    ("tx_bytes".to_string(), i(nums[8])),
                    ("rx_packets".to_string(), i(nums[1])),
                    ("tx_packets".to_string(), i(nums[9])),
                    ("rx_errors".to_string(), i(nums[2])),
                    ("tx_errors".to_string(), i(nums[10])),
                    ("rx_dropped".to_string(), i(nums[3])),
                    ("tx_dropped".to_string(), i(nums[11])),
                    ("operstate".to_string(), oper.map(s).unwrap_or(Json::Null)),
                    ("carrier".to_string(), carrier.map(s).unwrap_or(Json::Null)),
                ]));
            }
            let ok = !ifaces.is_empty();
            if !ok {
                ctx.issue("/proc/net/dev", "malformed", "no interfaces parsed".into(), "hide network metrics; check /proc/net/dev format");
            }
            obj(vec![
                ("present".to_string(), b(true)),
                ("ok".to_string(), b(ok)),
                ("interfaces".to_string(), arr(ifaces)),
            ])
        }
        Err(e) => {
            ctx.issue("/proc/net/dev", "unreadable", e.clone(), "verify procfs is mounted; network namespaces may restrict /proc/net");
            obj(vec![("present".to_string(), b(false)), ("ok".to_string(), b(false)), ("error".to_string(), s(e))])
        }
    }
}

fn probe_thermal(ctx: &mut Ctx) -> Json {
    let dir = Path::new("/sys/class/thermal");
    let zones = list_matching(dir, "thermal_zone");
    if zones.is_empty() {
        ctx.issue("/sys/class/thermal/thermal_zone*", "missing", "no thermal_zone entries".into(), "temperature is unavailable on this hardware/session");
        return obj(vec![
            ("present".to_string(), b(false)),
            ("ok".to_string(), b(false)),
            ("index_volatile".to_string(), b(true)),
            ("zones".to_string(), arr(vec![])),
        ]);
    }
    let mut out = Vec::new();
    let mut read_failures = 0;
    for z in &zones {
        let name = z.file_name().unwrap().to_string_lossy().into_owned();
        let typ = read_opt(&z.join("type"));
        let temp = read_i64(&z.join("temp")).ok();
        let mode = read_opt(&z.join("mode"));
        let policy = read_opt(&z.join("policy"));
        let device = canonical_device(z);
        if temp.is_none() {
            read_failures += 1;
            ctx.issue(
                &z.join("temp").to_string_lossy(),
                "unreadable",
                "temperature file present but not readable/parseable".into(),
                "report the zone as unknown temperature rather than 0",
            );
        }
        out.push(obj(vec![
            ("zone".to_string(), s(name)),
            ("type".to_string(), typ.clone().map(s).unwrap_or(Json::Null)),
            ("stable_id".to_string(), typ.map(s).unwrap_or(Json::Null)),
            ("temp_millidegC".to_string(), temp.map(i).unwrap_or(Json::Null)),
            ("temp_celsius".to_string(), temp.map(|v| f(v as f64 / 1000.0)).unwrap_or(Json::Null)),
            ("mode".to_string(), mode.map(s).unwrap_or(Json::Null)),
            ("policy".to_string(), policy.map(s).unwrap_or(Json::Null)),
            ("device".to_string(), device.map(s).unwrap_or(Json::Null)),
        ]));
    }
    obj(vec![
        ("present".to_string(), b(true)),
        ("ok".to_string(), b(read_failures == 0)),
        ("count".to_string(), i(out.len() as i64)),
        ("index_volatile".to_string(), b(true)),
        ("zones".to_string(), arr(out)),
    ])
}

fn probe_hwmon(ctx: &mut Ctx) -> Json {
    let dir = Path::new("/sys/class/hwmon");
    let chips = list_matching(dir, "hwmon");
    if chips.is_empty() {
        ctx.issue("/sys/class/hwmon/hwmon*", "missing", "no hwmon entries".into(), "temperature is unavailable on this hardware/session");
        return obj(vec![
            ("present".to_string(), b(false)),
            ("ok".to_string(), b(false)),
            ("index_volatile".to_string(), b(true)),
            ("chips".to_string(), arr(vec![])),
        ]);
    }
    let mut out = Vec::new();
    let mut any_fan = false;
    for h in &chips {
        let index = h.file_name().unwrap().to_string_lossy().into_owned();
        let name = read_opt(&h.join("name"));
        let device = canonical_device(h);
        let stable_identity = match (&name, &device) {
            (Some(chip_name), Some(device_path)) => {
                Some(format!("{}/{}", chip_name, device_path))
            }
            _ => {
                ctx.issue(
                    &h.to_string_lossy(),
                    "unresolved",
                    "hwmon stable identity requires both name and canonical device path".into(),
                    "do not persist this chip's sensor identity; use it only for the current probe result",
                );
                None
            }
        };
        let mut sensors = Vec::new();
        let mut fan_present = false;
        let mut pwm_present = false;
        if let Ok(rd) = fs::read_dir(h) {
            for entry in rd.flatten() {
                let fname = entry.file_name().to_string_lossy().into_owned();
                if fname.starts_with("fan") && fname.ends_with("_input") {
                    fan_present = true;
                    any_fan = true;
                }
                if fname.starts_with("pwm") {
                    pwm_present = true;
                }
                if fname.starts_with("temp") && fname.ends_with("_input") {
                    let base = fname.strip_suffix("_input").unwrap().to_string();
                    let value = read_i64(&entry.path()).ok();
                    let label = read_opt(&h.join(format!("{}_label", base)));
                    if value.is_none() {
                        ctx.issue(
                            &entry.path().to_string_lossy(),
                            "unreadable",
                            "temperature input present but not readable/parseable".into(),
                            "report the sensor as unknown rather than 0",
                        );
                    }
                    let sensor_identity = label.clone().unwrap_or(base);
                    sensors.push(obj(vec![
                        ("sensor".to_string(), s(fname)),
                        ("label".to_string(), label.clone().map(s).unwrap_or(Json::Null)),
                        (
                            "stable_key".to_string(),
                            stable_identity
                                .as_ref()
                                .map(|chip| s(format!("hwmon/{}/{}", chip, sensor_identity)))
                                .unwrap_or(Json::Null),
                        ),
                        ("temp_millidegC".to_string(), value.map(i).unwrap_or(Json::Null)),
                        ("temp_celsius".to_string(), value.map(|v| f(v as f64 / 1000.0)).unwrap_or(Json::Null)),
                    ]));
                }
            }
        }
        if sensors.is_empty() {
            ctx.issue(
                &format!("/sys/class/hwmon/{}/temp*_input", index),
                "missing",
                format!(
                    "hwmon chip '{}' exposes no temperature sensors",
                    name.clone().unwrap_or_else(|| index.clone())
                ),
                "expected for AC adapter / battery / non-thermal chips; hide this chip from the temperature list",
            );
        }
        out.push(obj(vec![
            ("index".to_string(), s(index)),
            ("name".to_string(), name.map(s).unwrap_or(Json::Null)),
            ("device".to_string(), device.map(s).unwrap_or(Json::Null)),
            (
                "stable_identity".to_string(),
                stable_identity.map(s).unwrap_or(Json::Null),
            ),
            ("temp_sensor_count".to_string(), i(sensors.len() as i64)),
            ("fan_speed_present".to_string(), b(fan_present)),
            ("pwm_present".to_string(), b(pwm_present)),
            ("sensors".to_string(), arr(sensors)),
        ]));
    }
    if !any_fan {
        ctx.issue(
            "/sys/class/hwmon/hwmon*/fan*_input",
            "missing",
            "no hwmon fan*_input speed sensors present on any chip".into(),
            "fan RPM is unavailable read-only on this machine; fan control/speed remains an opt-in, hardware-guarded feature",
        );
    }
    obj(vec![
        ("present".to_string(), b(true)),
        ("ok".to_string(), b(true)),
        ("count".to_string(), i(out.len() as i64)),
        ("index_volatile".to_string(), b(true)),
        ("chips".to_string(), arr(out)),
    ])
}

fn probe_power(ctx: &mut Ctx) -> Json {
    let dir = Path::new("/sys/class/power_supply");
    let devices = list_matching(dir, "");
    let mut out = Vec::new();
    for d in devices {
        let name = d.file_name().unwrap().to_string_lossy().into_owned();
        let typ = read_opt(&d.join("type"));
        let capacity = read_i64(&d.join("capacity")).ok();
        let status = read_opt(&d.join("status"));
        let energy_now = read_i64(&d.join("energy_now")).ok();
        let power_now = read_i64(&d.join("power_now")).ok();
        let voltage_now = read_i64(&d.join("voltage_now")).ok();
        out.push(obj(vec![
            ("name".to_string(), s(name)),
            ("type".to_string(), typ.map(s).unwrap_or(Json::Null)),
            ("capacity_percent".to_string(), capacity.map(i).unwrap_or(Json::Null)),
            ("status".to_string(), status.map(s).unwrap_or(Json::Null)),
            ("energy_now_uWh".to_string(), energy_now.map(i).unwrap_or(Json::Null)),
            ("power_now_uW".to_string(), power_now.map(i).unwrap_or(Json::Null)),
            ("voltage_now_uV".to_string(), voltage_now.map(i).unwrap_or(Json::Null)),
        ]));
    }
    let present = !out.is_empty();
    if !present {
        ctx.issue("/sys/class/power_supply", "missing", "no power_supply devices".into(), "battery/AC metrics are unavailable on this hardware/session");
    }
    obj(vec![
        ("present".to_string(), b(present)),
        ("ok".to_string(), b(present)),
        ("count".to_string(), i(out.len() as i64)),
        ("devices".to_string(), arr(out)),
    ])
}

fn jget<'a>(j: &'a Json, key: &str) -> Option<&'a Json> {
    match j {
        Json::Obj(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
        _ => None,
    }
}

fn jbool(j: &Json, key: &str) -> bool {
    match jget(j, key) {
        Some(Json::Bool(v)) => *v,
        _ => false,
    }
}

fn jint(j: &Json, key: &str) -> i64 {
    match jget(j, key) {
        Some(Json::Int(v)) => *v,
        _ => 0,
    }
}

fn jarr<'a>(j: &'a Json, key: &str) -> Vec<&'a Json> {
    match jget(j, key) {
        Some(Json::Arr(items)) => items.iter().collect(),
        _ => Vec::new(),
    }
}

fn first_temp_input(hwmon_dir: &Path) -> Option<PathBuf> {
    if let Ok(rd) = fs::read_dir(hwmon_dir) {
        for entry in rd.flatten() {
            let fname = entry.file_name().to_string_lossy().into_owned();
            if fname.starts_with("temp") && fname.ends_with("_input") {
                return Some(entry.path());
            }
        }
    }
    None
}

fn os_release_field(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == key {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

fn main() {
    let mut ctx = Ctx { issues: Vec::new() };

    // Machine facts (no network identifiers / MACs / IPs).
    let kernel = read_opt(Path::new("/proc/sys/kernel/osrelease")).unwrap_or_else(|| "unknown".into());
    let os_release = read_opt(Path::new("/etc/os-release")).unwrap_or_default();
    let pretty_name = os_release_field(&os_release, "PRETTY_NAME").unwrap_or_else(|| "unknown".into());
    let cpu_model = read_opt(Path::new("/proc/cpuinfo"))
        .map(|t| {
            t.lines()
                .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim() == "model name"))
                .map(|(_, v)| v.trim().to_string())
                .unwrap_or_else(|| "unknown".into())
        })
        .unwrap_or_else(|| "unknown".into());

    // Probes.
    let stat = probe_proc_stat(&mut ctx);
    let meminfo = probe_proc_meminfo(&mut ctx);
    let loadavg = probe_proc_loadavg(&mut ctx);
    let uptime = probe_proc_uptime(&mut ctx);
    let net_dev = probe_net_dev(&mut ctx);
    let thermal = probe_thermal(&mut ctx);
    let hwmon = probe_hwmon(&mut ctx);
    let power = probe_power(&mut ctx);

    // Sampling cost.
    let mut sampling = Vec::new();
    for p in ["/proc/stat", "/proc/meminfo", "/proc/loadavg", "/proc/uptime", "/proc/net/dev"] {
        sampling.push(bench_read(Path::new(p), 1000));
    }
    if let Some(z) = list_matching(Path::new("/sys/class/thermal"), "thermal_zone").first() {
        sampling.push(bench_read(&z.join("temp"), 1000));
    }
    for h in list_matching(Path::new("/sys/class/hwmon"), "hwmon") {
        if let Some(t) = first_temp_input(&h) {
            sampling.push(bench_read(&t, 1000));
            break;
        }
    }

    let machine = obj(vec![
        ("kernel_release".to_string(), s(kernel)),
        ("os".to_string(), s(pretty_name)),
        ("cpu_model".to_string(), s(cpu_model)),
        ("logical_cpus".to_string(), i(jint(&stat, "per_cpu_lines"))),
    ]);

    // Derived capability signals.
    let cpu_ok = jbool(&stat, "ok");
    let mem_ok = jbool(&meminfo, "ok");
    let net_ok = jbool(&net_dev, "ok");
    let thermal_zones = jarr(&thermal, "zones");
    let hwmon_chips = jarr(&hwmon, "chips");
    let mut hwmon_temp_count = 0i64;
    let mut chip_names: Vec<String> = Vec::new();
    let mut fan_present = false;
    for c in &hwmon_chips {
        hwmon_temp_count += jint(c, "temp_sensor_count");
        if let Some(Json::Str(n)) = jget(c, "name") {
            chip_names.push(n.clone());
        }
        if jbool(c, "fan_speed_present") {
            fan_present = true;
        }
    }
    let temp_ok = hwmon_temp_count > 0 || !thermal_zones.is_empty();
    let power_ok = jbool(&power, "present");

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let mut capabilities = Vec::new();
    capabilities.push(capability(
        "system-monitor.cpu",
        if cpu_ok { "Supported" } else { "Unsupported { reason: /proc/stat unavailable }" },
        "Aggregate and per-core CPU jiffy counters from /proc/stat",
        "/proc/stat",
        vec!["/proc/loadavg"],
        if cpu_ok { None } else { Some("Verify procfs is mounted and Kestrel runs unprivileged in a normal user session") },
        vec![
            obj(vec![("per_cpu_lines".to_string(), i(jint(&stat, "per_cpu_lines")))]),
            obj(vec![("total_jiffies".to_string(), i(jint(&stat, "total_jiffies")))]),
        ],
    ));
    capabilities.push(capability(
        "system-monitor.memory",
        if mem_ok { "Supported" } else { "Unsupported { reason: /proc/meminfo unavailable }" },
        "Memory and swap counters from /proc/meminfo",
        "/proc/meminfo",
        vec![],
        if mem_ok { None } else { Some("Verify procfs is mounted") },
        vec![
            obj(vec![("MemTotal_kB".to_string(), jget(&meminfo, "MemTotal_kB").cloned().unwrap_or(Json::Null))]),
            obj(vec![("MemAvailable_kB".to_string(), jget(&meminfo, "MemAvailable_kB").cloned().unwrap_or(Json::Null))]),
        ],
    ));
    capabilities.push(capability(
        "system-monitor.network",
        if net_ok { "Supported" } else { "Unsupported { reason: /proc/net/dev unavailable }" },
        "Per-interface byte/packet/error counters from /proc/net/dev",
        "/proc/net/dev",
        vec!["/sys/class/net/<iface>/statistics"],
        if net_ok { None } else { Some("Verify procfs is mounted and /proc/net is not restricted by a network namespace") },
        vec![
            obj(vec![("interfaces".to_string(), i(jarr(&net_dev, "interfaces").len() as i64))]),
        ],
    ));
    capabilities.push(capability(
        "system-monitor.temperature",
        if temp_ok { "Supported" } else { "Unsupported { reason: no readable thermal_zone or hwmon temperature sensors }" },
        "Temperature sensors from /sys/class/thermal and /sys/class/hwmon",
        "/sys/class/hwmon + /sys/class/thermal",
        vec!["/sys/class/thermal/thermal_zone*/temp", "/sys/class/hwmon/hwmon*/temp*_input"],
        if temp_ok { None } else { Some("No temperature sensors exposed by this machine's ACPI/drivers; hide the temperature panel") },
        vec![
            obj(vec![
                ("thermal_zone_count".to_string(), i(thermal_zones.len() as i64)),
                ("hwmon_chip_count".to_string(), i(hwmon_chips.len() as i64)),
                ("hwmon_temp_sensor_count".to_string(), i(hwmon_temp_count)),
                ("hwmon_chip_names".to_string(), arr(chip_names.iter().cloned().map(s).collect())),
            ]),
        ],
    ));
    capabilities.push(capability(
        "system-monitor.fan_speed",
        if fan_present { "Supported" } else { "Unsupported { reason: no hwmon fan*_input speed sensors }" },
        "Fan speed (RPM) read from hwmon fan*_input",
        "/sys/class/hwmon/hwmon*/fan*_input",
        vec![],
        if fan_present { None } else { Some("Fan control/speed is hardware-specific. cooling_device4..8 report type=Fan but expose no fan*_input speed attribute; keep fan control as a separate opt-in, hardware-guarded feature") },
        vec![obj(vec![("fan_present".to_string(), b(fan_present))])],
    ));
    capabilities.push(capability(
        "system-monitor.battery",
        if power_ok { "Supported" } else { "Unsupported { reason: no /sys/class/power_supply devices }" },
        "Read-only battery/AC presence, capacity, status, power from /sys/class/power_supply (observed adjacency, not part of the initial read-only boundary)",
        "/sys/class/power_supply",
        vec!["UPower (D-Bus)"],
        if power_ok { None } else { Some("No battery/AC devices exposed; hide battery metrics") },
        vec![obj(vec![("power_supply_count".to_string(), i(jarr(&power, "devices").len() as i64))])],
    ));

    let sources = obj(vec![
        ("proc".to_string(), obj(vec![
            ("stat".to_string(), stat),
            ("meminfo".to_string(), meminfo),
            ("loadavg".to_string(), loadavg),
            ("uptime".to_string(), uptime),
            ("net_dev".to_string(), net_dev),
        ])),
        ("sys".to_string(), obj(vec![
            ("thermal_zones".to_string(), thermal),
            ("hwmon".to_string(), hwmon),
            ("power_supply".to_string(), power),
        ])),
    ]);

    let report = obj(vec![
        ("probe".to_string(), s("system-monitor")),
        ("phase".to_string(), s("0")),
        ("read_only".to_string(), b(true)),
        ("observed_at_unix_ms".to_string(), i(now_ms)),
        ("machine".to_string(), machine),
        ("sampling_cost_us".to_string(), arr(sampling)),
        ("sources".to_string(), sources),
        ("issues".to_string(), arr(ctx.issues)),
        ("capabilities".to_string(), arr(capabilities)),
    ]);

    println!("{}", report.to_string());
}









# Phase 0 spike: system-monitor capability probe

Disposable, std-only probe for Kestrel issue #3. It is **not** a Cargo workspace
member and adds **no** dependencies. Build/run with `rustc`:

```sh
rustc --edition 2021 -O probe.rs -o /tmp/kestrel-system-monitor-probe
/tmp/kestrel-system-monitor-probe > report.json
```

The probe emits one JSON document: machine facts, `/proc` and `/sys` sources,
nanosecond sampling cost, hardware-specific missing states, and per-feature
`CapabilityReport`-shaped entries (`status`, `selected_backend`,
`alternatives_considered`, `remediation`, `evidence`).

## Machine under test

- Pop!_OS 24.04 LTS, kernel 7.0.11-76070011-generic
- 12th Gen Intel Core i9-12900HX, 24 logical CPUs (Lenovo Legion 7 16IAX7)

## Observed results

- `/proc`: `stat` (24 `cpuN` lines + aggregate), `meminfo`, `loadavg`,
  `uptime`, `net/dev` all present and world-readable (`-r--r--r--`, `root:root`).
- Thermal: 13 `thermal_zone*` (types `acpitz`, `x86_pkg_temp`, `TCPU`,
  `TCPU_PCI`, `SEN1..SEN7`, `iwlwifi_1`, `INT3400 Thermal`) all `temp`-readable.
- hwmon: 8 chips — `coretemp` (17 temp inputs with `Package id 0`/`Core N`
  labels), `nvme` (3), `acpitz`, `r8169_0_6f00:00`, `spd5118`, `iwlwifi_1`
  (1 each), and `ADP0`/`BAT0` with **no** temperature inputs.
- Missing/unreadable: no `fan*_input`/`pwm*` under any hwmon chip (fan RPM is
  not observable read-only); `ADP0` and `BAT0` expose no temp sensors.
- Identity: `hwmonN` and `thermal_zoneN` indexes are **volatile**; stable keys
  are hwmon `name` + canonical device path, and thermal-zone `type`.
- Sampling cost (1000 reads, warmed page cache): `/proc/stat` ~34 µs,
  `/proc/meminfo` ~9 µs, `/proc/loadavg` ~3 µs, `/proc/uptime` ~5 µs,
  `/proc/net/dev` ~64 µs, a sysfs `temp` read ~11 µs. A full monitor tick
  (all sources) is well under 1 ms.

## Recommended initial boundary

`system-monitor` service = read-only CPU, memory, network, temperature from
`/proc` + `/sys`. Fan speed/control is out of scope (hardware-guarded, opt-in).
Battery is observed but deferred to the `power` feature boundary.

See the issue comments / final report for the full `/proc`-/`sys` adapter
contract (`types`, `CapabilityReport` fields, remediation).

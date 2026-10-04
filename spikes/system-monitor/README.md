# Phase 0 spike: system-monitor capability probe

Disposable std-only probe for Kestrel issue #3; it is outside the Cargo
workspace and adds no dependencies.

```sh
rustc --edition 2021 -O probe.rs -o /tmp/kestrel-system-monitor-probe
/tmp/kestrel-system-monitor-probe > report.json
```

The probe emits one JSON document containing machine facts, `/proc` and `/sys`
sources, nanosecond sampling cost, hardware-specific missing states, and
per-feature `CapabilityReport` fields (`status`, `selected_backend`,
`alternatives_considered`, `remediation`, `evidence`).

## Machine under test

- Pop!_OS 24.04 LTS, kernel 7.0.11-76070011-generic.
- 12th Gen Intel Core i9-12900HX, 24 logical CPUs (Lenovo Legion 7 16IAX7).

## Observed results

- `/proc`: `stat` (24 `cpuN` lines plus aggregate), `meminfo`, `loadavg`,
  `uptime`, and `net/dev` are present and world-readable (`-r--r--r--`, `root:root`).
- Thermal: 13 `thermal_zone*` entries (`acpitz`, `x86_pkg_temp`, `TCPU`,
  `TCPU_PCI`, `SEN1..SEN7`, `iwlwifi_1`, `INT3400 Thermal`), all `temp`-readable.
- hwmon: 8 chips — `coretemp` (17 inputs with `Package id 0`/`Core N` labels),
  `nvme` (3), `acpitz`, `r8169_0_6f00:00`, `spd5118`, `iwlwifi_1` (1 each), and
  `ADP0`/`BAT0` with no temperature inputs.
- No `fan*_input`/`pwm*` exists under hwmon; fan RPM is not observable read-only.
- `hwmonN` and `thermal_zoneN` indexes are volatile. Stable keys are hwmon
  `name` plus canonical device path, and thermal-zone `type`.
- Warm-cache sampling cost over 1,000 reads: `/proc/stat` ~34 µs, `meminfo` ~9 µs,
  `loadavg` ~3 µs, `uptime` ~5 µs, `net/dev` ~64 µs, sysfs `temp` ~11 µs; a full
tick is well under 1 ms.

## Recommended initial boundary

`system-monitor` should provide read-only CPU, memory, network, and temperature
from `/proc` + `/sys`. Fan speed/control is hardware-guarded, opt-in scope;
battery is observed but deferred to the `power` feature boundary.

See the issue comments/final report for the full adapter contract (`types`,
`CapabilityReport` fields, and remediation).

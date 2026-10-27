//! LAN Cowork machine-info collection and GPU probes (F1).

use std::{
    collections::HashMap,
    ffi::OsString,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use chrono::{Local, SecondsFormat};
use serde_json::{json, Value};

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

#[derive(Clone, Copy)]
enum MachineProbe<'a> {
    GitBranch(&'a str),
    GitCommit(&'a str),
    GitStatus(&'a str),
    NvidiaSmi,
    Rocminfo,
    RocmSmi,
    SystemProfiler,
    PowershellGpu,
}

struct ProbeSpec {
    program: &'static str,
    args: Vec<String>,
    env: Vec<(&'static str, OsString)>,
    timeout: Duration,
}

impl MachineProbe<'_> {
    fn command(self) -> Option<ProbeSpec> {
        let (program, args, seconds): (&str, &[&str], u64) = match self {
            Self::GitBranch(_) => ("git", &["rev-parse", "--abbrev-ref", "HEAD"], 5),
            Self::GitCommit(_) => ("git", &["rev-parse", "--short", "HEAD"], 5),
            Self::GitStatus(_) => ("git", &["status", "--porcelain", "--untracked-files=no"], 5),
            Self::NvidiaSmi => (
                "nvidia-smi",
                &[
                    "--query-gpu=name,memory.total,memory.used,utilization.gpu",
                    "--format=csv,noheader,nounits",
                ],
                3,
            ),
            Self::Rocminfo => ("rocminfo", &[], 8),
            Self::RocmSmi => (
                "rocm-smi",
                &[
                    "--showproductname",
                    "--showmeminfo",
                    "vram",
                    "--showuse",
                    "--json",
                ],
                5,
            ),
            Self::SystemProfiler => ("system_profiler", &["SPDisplaysDataType"], 5),
            Self::PowershellGpu => (
                "powershell",
                &[
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "(Get-CimInstance Win32_VideoController).Name",
                ],
                5,
            ),
        };
        let mut spec = ProbeSpec {
            program,
            args: Vec::new(),
            env: Vec::new(),
            timeout: Duration::from_secs(seconds),
        };
        let mut env_keys = vec!["PATH"];
        #[cfg(windows)]
        env_keys.extend(["SystemRoot", "WINDIR", "COMSPEC", "PATHEXT", "TEMP", "TMP"]);
        match self {
            Self::GitBranch(path) | Self::GitCommit(path) | Self::GitStatus(path) => {
                if path.is_empty() || path.starts_with('-') || path.chars().any(char::is_control) {
                    return None;
                }
                spec.args.extend(["-C".to_owned(), path.to_owned()]);
                env_keys.push("HOME");
                #[cfg(windows)]
                env_keys.extend(["USERPROFILE", "HOMEDRIVE", "HOMEPATH"]);
                #[cfg(unix)]
                env_keys.extend(["XDG_CONFIG_HOME", "SUDO_UID", "SUDO_GID"]);
            }
            Self::PowershellGpu => {}
            _ => {
                spec.env
                    .extend([("LANG", "C".into()), ("LC_ALL", "C".into())]);
            }
        }
        for key in env_keys {
            if let Some(value) = std::env::var_os(key) {
                spec.env.push((key, value));
            }
        }
        spec.args.extend(args.iter().map(|arg| (*arg).to_owned()));
        Some(spec)
    }
}

const MAX_PROBE_STDOUT: u64 = 1024 * 1024;

fn read_probe_stdout(stdout: impl Read) -> Option<String> {
    let mut output = String::new();
    // Read one extra byte to distinguish a full response from a truncated one.
    stdout
        .take(MAX_PROBE_STDOUT + 1)
        .read_to_string(&mut output)
        .ok()?;
    (output.len() as u64 <= MAX_PROBE_STDOUT).then_some(output)
}

fn command_output(probe: MachineProbe<'_>) -> Option<String> {
    let spec = probe.command()?;
    let mut child = Command::new(spec.program)
        .env_clear()
        .args(spec.args)
        .envs(spec.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let (sender, reader) = std::sync::mpsc::channel();
    // Detach: a grandchild can keep the pipe open after the probe exits.
    std::thread::spawn(move || {
        let _ = sender.send(read_probe_stdout(stdout));
    });
    let deadline = Instant::now() + spec.timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match reader.recv_timeout(remaining) {
                    Ok(output) => return output,
                    Err(_) => break,
                }
            }
            Ok(Some(_)) => return None,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

fn git_info(repo_path: &Path) -> Value {
    let run = |probe| command_output(probe).map(|value| value.trim().to_owned());
    let path = repo_path.to_string_lossy();
    match (
        run(MachineProbe::GitBranch(&path)),
        run(MachineProbe::GitCommit(&path)),
        run(MachineProbe::GitStatus(&path)),
    ) {
        (Some(branch), Some(commit), Some(dirty)) => {
            json!({"branch": branch, "commit": commit, "dirty": !dirty.is_empty()})
        }
        _ => json!({"branch": null, "commit": null, "dirty": null}),
    }
}

fn cpu_name() -> String {
    std::env::consts::ARCH.to_owned()
}

fn cpu_physical_cores() -> u64 {
    let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") else {
        return 0;
    };
    let mut cores = std::collections::HashSet::new();
    for block in text.split("\n\n") {
        let physical = block.lines().find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| key.trim() == "physical id")
                .map(|(_, value)| value.trim())
        });
        let core = block.lines().find_map(|line| {
            line.split_once(':')
                .filter(|(key, _)| key.trim() == "core id")
                .map(|(_, value)| value.trim())
        });
        if let (Some(physical), Some(core)) = (physical, core) {
            cores.insert((physical.to_owned(), core.to_owned()));
        }
    }
    cores.len() as u64
}

fn cpu_stat() -> Option<(u64, u64)> {
    let line = std::fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .next()?
        .to_owned();
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|field| field.parse().ok())
        .collect();
    (fields.len() >= 4).then(|| {
        (
            fields.iter().sum(),
            fields[3] + fields.get(4).copied().unwrap_or(0),
        )
    })
}

fn cpu_usage_pct() -> f64 {
    let Some((total_before, idle_before)) = cpu_stat() else {
        return 0.0;
    };
    std::thread::sleep(std::time::Duration::from_millis(100));
    let Some((total_after, idle_after)) = cpu_stat() else {
        return 0.0;
    };
    let total = total_after.saturating_sub(total_before);
    if total == 0 {
        0.0
    } else {
        round1((total - idle_after.saturating_sub(idle_before)) as f64 / total as f64 * 100.0)
    }
}

fn os_info() -> Value {
    #[cfg(unix)]
    {
        let mut name = std::mem::MaybeUninit::<libc::utsname>::uninit();
        if unsafe { libc::uname(name.as_mut_ptr()) } == 0 {
            let name = unsafe { name.assume_init() };
            let read = |value: &[libc::c_char]| {
                unsafe { std::ffi::CStr::from_ptr(value.as_ptr()) }
                    .to_string_lossy()
                    .into_owned()
            };
            return json!({"system": read(&name.sysname), "release": read(&name.release), "version": read(&name.version)});
        }
    }
    let system = match std::env::consts::OS {
        "windows" => "Windows",
        other => other,
    };
    json!({"system": system, "release": "", "version": ""})
}

fn gpu(name: String, total: Option<f64>, used: Option<f64>, utilization: Option<f64>) -> Value {
    json!({"name": name, "vram_total_gb": total, "vram_used_gb": used, "utilization_pct": utilization})
}

fn gpu_nvidia_smi() -> Option<Vec<Value>> {
    let output = command_output(MachineProbe::NvidiaSmi)?;
    let gpus: Vec<_> = output
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split(',').map(str::trim).collect();
            if fields.len() < 4 {
                return None;
            }
            let total = fields[1].parse::<f64>().ok()?;
            let used = fields[2].parse::<f64>().ok()?;
            Some(gpu(
                fields[0].to_owned(),
                Some(round1(total / 1024.0)),
                Some(round1(used / 1024.0)),
                fields[3].parse().ok(),
            ))
        })
        .collect();
    (!gpus.is_empty()).then_some(gpus)
}

fn rocminfo_names() -> Vec<String> {
    command_output(MachineProbe::Rocminfo)
        .map(|output| {
            output
                .split("*******")
                .filter_map(|block| {
                    (block.contains("Device Type:             GPU")
                        || block.contains("Device Type: GPU"))
                    .then(|| {
                        block.lines().find_map(|line| {
                            line.split_once("Marketing Name:")
                                .map(|(_, value)| value.trim().to_owned())
                        })
                    })
                    .flatten()
                })
                .filter(|name| !name.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn gpu_rocm_smi() -> Option<Vec<Value>> {
    let output = command_output(MachineProbe::RocmSmi)?;
    let data = serde_json::from_str::<Value>(&output).ok()?;
    let cards = data
        .as_object()?
        .iter()
        .filter(|(key, value)| key.to_ascii_lowercase().starts_with("card") && value.is_object())
        .collect::<Vec<_>>();
    if cards.is_empty() {
        return None;
    }
    let names = rocminfo_names();
    Some(
        cards
            .into_iter()
            .enumerate()
            .map(|(index, (_, card))| {
                let get = |key| {
                    card.get(key)
                        .and_then(Value::as_str)
                        .and_then(|value| value.trim().parse::<f64>().ok())
                };
                let total = get("VRAM Total Memory (B)");
                let name = names
                    .get(index)
                    .cloned()
                    .or_else(|| {
                        ["Card series", "Card model", "Marketing Name", "Card SKU"]
                            .iter()
                            .find_map(|key| {
                                card.get(*key).and_then(Value::as_str).map(str::to_owned)
                            })
                    })
                    .unwrap_or_default();
                gpu(
                    name,
                    total.map(|value| round1(value / 1e9)),
                    total
                        .and_then(|_| get("VRAM Total Used Memory (B)"))
                        .map(|value| round1(value / 1e9)),
                    card.get("GPU use (%)")
                        .and_then(Value::as_str)
                        .and_then(|value| value.trim().parse().ok()),
                )
            })
            .collect(),
    )
}

fn gpu_macos() -> Option<Vec<Value>> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let output = command_output(MachineProbe::SystemProfiler)?;
    let mut name = None;
    let mut total = None;
    for line in output.lines().map(str::trim) {
        if name.is_none() {
            name = line
                .strip_prefix("Chipset Model:")
                .map(|value| value.trim().to_owned());
        }
        if total.is_none() && line.starts_with("VRAM") {
            let fields: Vec<_> = line
                .split_once(':')
                .map(|(_, value)| value.split_whitespace().collect())
                .unwrap_or_default();
            if fields.len() >= 2 {
                total = fields[0].replace(',', "").parse::<f64>().ok().map(|value| {
                    round1(if fields[1].to_ascii_uppercase().starts_with("MB") {
                        value / 1024.0
                    } else {
                        value
                    })
                });
            }
        }
    }
    name.map(|name| vec![gpu(name, total, None, None)])
}

fn gpu_windows_wmi() -> Vec<Value> {
    if !cfg!(target_os = "windows") {
        return Vec::new();
    }
    command_output(MachineProbe::PowershellGpu)
        .map(|output| {
            output
                .lines()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(|name| gpu(name.to_owned(), None, None, None))
                .collect()
        })
        .unwrap_or_default()
}

fn gpu_info_from(
    probes: [Option<Vec<Value>>; 3],
    windows: impl FnOnce() -> Vec<Value>,
) -> Vec<Value> {
    if let Some(gpus) = probes.into_iter().flatten().next() {
        return gpus;
    }
    let windows = windows();
    if windows.is_empty() {
        vec![gpu(String::new(), None, None, None)]
    } else {
        windows
    }
}

fn gpu_info() -> Vec<Value> {
    gpu_info_from(
        [gpu_nvidia_smi(), gpu_rocm_smi(), gpu_macos()],
        gpu_windows_wmi,
    )
}

fn meminfo() -> HashMap<String, u64> {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .map(|text| {
            text.lines()
                .filter_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    Some((
                        key.to_owned(),
                        value.split_whitespace().next()?.parse::<u64>().ok()? * 1024,
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn ram_from_meminfo(info: &HashMap<String, u64>) -> Value {
    let total = *info.get("MemTotal").unwrap_or(&0) as f64;
    let free = *info.get("MemFree").unwrap_or(&0) as f64;
    let cached = *info.get("Cached").unwrap_or(&0) as f64;
    let buffers = *info.get("Buffers").unwrap_or(&0) as f64;
    let used = (total - free - cached - buffers).max(total - free);
    let pct = if total == 0.0 {
        0.0
    } else {
        (total - *info.get("MemAvailable").unwrap_or(&0) as f64) / total * 100.0
    };
    json!({"total_gb": round1(total / 1e9), "used_gb": round1(used / 1e9), "pct": round1(pct)})
}

fn disk_pct(total: f64, free: f64, available: f64) -> f64 {
    let used = total - free;
    if used + available == 0.0 {
        0.0
    } else {
        used / (used + available) * 100.0
    }
}

#[cfg(unix)]
fn disk_info(path: &Path) -> Value {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let Ok(path_c) = CString::new(path.as_os_str().as_bytes()) else {
        return zero_disk(path);
    };
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(path_c.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return zero_disk(path);
    }
    let stat = unsafe { stat.assume_init() };
    let unit = stat.f_frsize as f64;
    let total = stat.f_blocks as f64 * unit;
    let used = total - stat.f_bfree as f64 * unit;
    let available = stat.f_bavail as f64 * unit;
    json!({"path": path.to_string_lossy(), "total_gb": round1(total / 1e9), "used_gb": round1(used / 1e9), "pct": round1(disk_pct(total, stat.f_bfree as f64 * unit, available))})
}

fn zero_disk(path: &Path) -> Value {
    json!({"path": path.to_string_lossy(), "total_gb": 0.0, "used_gb": 0.0, "pct": 0.0})
}

fn collect_for_platform(
    version: &str,
    roles: &[Value],
    repo_path: &Path,
    gpu_name: &str,
    start_time: Instant,
    linux: bool,
) -> Value {
    let mut gpus = gpu_info();
    if !gpu_name.is_empty() {
        gpus[0]["name"] = json!(gpu_name);
    }
    let ram = if linux {
        ram_from_meminfo(&meminfo())
    } else {
        json!({"total_gb": 0.0, "used_gb": 0.0, "pct": 0.0})
    };
    #[cfg(unix)]
    let disk = if linux {
        disk_info(repo_path)
    } else {
        zero_disk(repo_path)
    };
    #[cfg(not(unix))]
    let disk = zero_disk(repo_path);
    json!({"version": version, "os": os_info(), "cpu": {"name": cpu_name(), "cores_physical": if linux { cpu_physical_cores() } else { 0 }, "cores_logical": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0), "usage_pct": if linux { cpu_usage_pct() } else { 0.0}}, "ram": ram, "gpu": gpus[0], "gpus": gpus, "disk": disk, "process_uptime_sec": start_time.elapsed().as_secs(), "git": git_info(repo_path), "roles": roles, "collected_at": Local::now().to_rfc3339_opts(SecondsFormat::Micros, false)})
}

/// Collect the fixed machine-info schema used by LAN Cowork fleet APIs.
pub fn collect(
    version: &str,
    roles: &[Value],
    repo_path: &Path,
    gpu_name: &str,
    start_time: Instant,
) -> Value {
    collect_for_platform(
        version,
        roles,
        repo_path,
        gpu_name,
        start_time,
        cfg!(target_os = "linux"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probes(path: &str) -> [MachineProbe<'_>; 8] {
        [
            MachineProbe::GitBranch(path),
            MachineProbe::GitCommit(path),
            MachineProbe::GitStatus(path),
            MachineProbe::NvidiaSmi,
            MachineProbe::Rocminfo,
            MachineProbe::RocmSmi,
            MachineProbe::SystemProfiler,
            MachineProbe::PowershellGpu,
        ]
    }

    #[test]
    fn machine_probe_argv_and_timeouts_are_fixed() {
        let path = "repo with spaces; --option";
        let expected: [(&[&str], u64); 8] = [
            (&["-C", path, "rev-parse", "--abbrev-ref", "HEAD"], 5),
            (&["-C", path, "rev-parse", "--short", "HEAD"], 5),
            (
                &["-C", path, "status", "--porcelain", "--untracked-files=no"],
                5,
            ),
            (
                &[
                    "--query-gpu=name,memory.total,memory.used,utilization.gpu",
                    "--format=csv,noheader,nounits",
                ],
                3,
            ),
            (&[], 8),
            (
                &[
                    "--showproductname",
                    "--showmeminfo",
                    "vram",
                    "--showuse",
                    "--json",
                ],
                5,
            ),
            (&["SPDisplaysDataType"], 5),
            (
                &[
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "(Get-CimInstance Win32_VideoController).Name",
                ],
                5,
            ),
        ];
        for (probe, (args, seconds)) in probes(path).into_iter().zip(expected) {
            let spec = probe.command().unwrap();
            assert_eq!(spec.args, args);
            assert_eq!(spec.timeout, Duration::from_secs(seconds));
        }
    }

    #[test]
    fn machine_probe_programs_are_closed() {
        let programs: Vec<_> = probes(".")
            .into_iter()
            .map(|probe| probe.command().unwrap().program)
            .collect();
        // Eight operations share six executables; the three git templates stay distinct.
        assert_eq!(
            programs,
            [
                "git",
                "git",
                "git",
                "nvidia-smi",
                "rocminfo",
                "rocm-smi",
                "system_profiler",
                "powershell"
            ]
        );
    }

    #[test]
    fn machine_probe_env_is_closed() {
        for (index, probe) in probes(".").into_iter().enumerate() {
            let spec = probe.command().unwrap();
            let mut expected = vec!["PATH"];
            #[cfg(windows)]
            expected.extend(["SystemRoot", "WINDIR", "COMSPEC", "PATHEXT", "TEMP", "TMP"]);
            if index < 3 {
                expected.push("HOME");
                #[cfg(windows)]
                expected.extend(["USERPROFILE", "HOMEDRIVE", "HOMEPATH"]);
                #[cfg(unix)]
                expected.extend(["XDG_CONFIG_HOME", "SUDO_UID", "SUDO_GID"]);
            }
            let mut expected: std::collections::BTreeMap<_, _> = expected
                .into_iter()
                .filter_map(|key| std::env::var_os(key).map(|value| (key, value)))
                .collect();
            if (3..7).contains(&index) {
                expected.insert("LANG", "C".into());
                expected.insert("LC_ALL", "C".into());
            }
            let actual: std::collections::BTreeMap<_, _> = spec.env.into_iter().collect();
            assert_eq!(actual, expected);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // A separate test process avoids mutating the parallel test suite's environment.
            if std::env::var_os("MACHINE_PROBE_ENV_TEST").is_some() {
                for probe in probes(".") {
                    assert_eq!(command_output(probe).as_deref(), Some("closed"));
                }
                return;
            }
            let dir = tempfile::tempdir().unwrap();
            let script = "#!/bin/sh\n[ \"${LD_PRELOAD+x}${DYLD_INSERT_LIBRARIES+x}${PYTHONPATH+x}${NODE_OPTIONS+x}${GIT_DIR+x}${MACHINE_PROBE_ENV_TEST+x}\" = \"\" ] || exit 1\nprintf closed\n";
            for program in [
                "git",
                "nvidia-smi",
                "rocminfo",
                "rocm-smi",
                "system_profiler",
                "powershell",
            ] {
                let path = dir.path().join(program);
                std::fs::write(&path, script).unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "routes::lan_cowork_fleet_machine::tests::machine_probe_env_is_closed",
                    "--nocapture",
                ])
                .env("PATH", dir.path())
                .env("MACHINE_PROBE_ENV_TEST", "1")
                .env("HOME", "probe-home")
                .env("XDG_CONFIG_HOME", "probe-xdg")
                .env("SUDO_UID", "1234")
                .env("SUDO_GID", "5678")
                .env("LD_PRELOAD", "")
                .env("DYLD_INSERT_LIBRARIES", "")
                .env("PYTHONPATH", "probe-sentinel")
                .env("NODE_OPTIONS", "probe-sentinel")
                .env("GIT_DIR", "probe-sentinel")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        }
    }

    #[test]
    fn machine_probe_stdout_is_bounded() {
        let limit = 1024 * 1024;
        let data = vec![b'x'; limit + 16];
        assert_eq!(read_probe_stdout(&data[..limit]).unwrap().len(), limit);
        let mut reader = std::io::Cursor::new(&data);
        assert!(read_probe_stdout(&mut reader).is_none());
        assert_eq!(reader.position(), (limit + 1) as u64);
        assert_eq!(read_probe_stdout(&b""[..]).as_deref(), Some(""));
        assert!(read_probe_stdout(&b"\xff"[..]).is_none());
    }

    #[cfg(unix)]
    fn probe_fixture(test: &str, script: &str) -> bool {
        use std::{io::Write, os::unix::fs::PermissionsExt};

        if std::env::var_os("MACHINE_PROBE_FIXTURE_TEST").is_some() {
            return true;
        }
        let dir = tempfile::tempdir().unwrap();
        for probe in probes(".") {
            let path = dir.path().join(probe.command().unwrap().program);
            std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("routes::lan_cowork_fleet_machine::tests::{test}"),
                "--nocapture",
            ])
            .env("PATH", dir.path())
            .env("MACHINE_PROBE_FIXTURE_TEST", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"inherited\n".repeat(8).as_slice())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        false
    }

    #[cfg(unix)]
    #[test]
    fn machine_probe_stdin_is_null() {
        if probe_fixture(
            "machine_probe_stdin_is_null",
            "if read -r line; then exit 1; fi\nprintf closed",
        ) {
            for probe in probes(".") {
                assert_eq!(command_output(probe).as_deref(), Some("closed"));
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn machine_probe_deadline_bounds_inherited_stdout() {
        if probe_fixture(
            "machine_probe_deadline_bounds_inherited_stdout",
            "/bin/sleep 6 &\nexit 0",
        ) {
            let probe = MachineProbe::NvidiaSmi;
            let deadline = probe.command().unwrap().timeout;
            let start = Instant::now();
            assert!(command_output(probe).is_none());
            let elapsed = start.elapsed();
            assert!(elapsed >= deadline, "returned early: {elapsed:?}");
            assert!(
                elapsed < deadline + Duration::from_millis(500),
                "{elapsed:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn machine_probe_command_output_enforces_cap() {
        if probe_fixture(
            "machine_probe_command_output_enforces_cap",
            "/bin/dd if=/dev/zero bs=\"$2\" count=1 2>/dev/null | /usr/bin/tr '\\000' x",
        ) {
            let output = command_output(MachineProbe::GitBranch("1048576")).unwrap();
            assert_eq!(output.len(), 1024 * 1024);
            assert!(output.bytes().all(|byte| byte == b'x'));
            assert!(command_output(MachineProbe::GitBranch("1048577")).is_none());
        }
    }

    #[test]
    fn machine_probe_rejects_invalid_repo_paths_before_spawn() {
        for path in [
            "",
            "-repo",
            "--help",
            "repo\0name",
            "repo\nname",
            "repo\rname",
            "repo\tname",
            "repo\u{7f}name",
            "repo\u{85}name",
        ] {
            for probe in [
                MachineProbe::GitBranch(path),
                MachineProbe::GitCommit(path),
                MachineProbe::GitStatus(path),
            ] {
                assert!(probe.command().is_none(), "accepted {path:?}");
                assert!(command_output(probe).is_none());
            }
        }
    }

    #[test]
    fn fleet_machine_info_schema_and_roles_match_python() {
        let roles = vec![json!("chief")];
        let info = collect("4.559.0", &roles, Path::new("."), "", Instant::now());
        let keys: Vec<_> = info
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "collected_at",
                "cpu",
                "disk",
                "git",
                "gpu",
                "gpus",
                "os",
                "process_uptime_sec",
                "ram",
                "roles",
                "version"
            ]
        );
        assert_eq!(info["roles"], json!(roles));
        assert_eq!(info["cpu"]["name"], std::env::consts::ARCH);
    }

    #[test]
    fn ram_percent_is_memavailable_not_used_ratio() {
        let info = HashMap::from([
            ("MemTotal".into(), 1000),
            ("MemFree".into(), 100),
            ("Cached".into(), 100),
            ("Buffers".into(), 100),
            ("MemAvailable".into(), 400),
        ]);
        assert_eq!(ram_from_meminfo(&info)["pct"], 60.0);
    }

    #[test]
    fn disk_percent_uses_available_blocks_not_total() {
        assert_eq!(round1(disk_pct(1_000.0, 200.0, 500.0)), 61.5);
        assert_ne!(round1(disk_pct(1_000.0, 200.0, 500.0)), 80.0);
    }

    #[test]
    fn non_linux_collect_keeps_zero_resource_schema() {
        let info = collect_for_platform("v", &[], Path::new("."), "", Instant::now(), false);
        assert_eq!(
            info["ram"],
            json!({"total_gb": 0.0, "used_gb": 0.0, "pct": 0.0})
        );
        assert_eq!(
            info["disk"],
            json!({"path": ".", "total_gb": 0.0, "used_gb": 0.0, "pct": 0.0})
        );
    }

    #[test]
    fn gpu_fallback_has_fixed_shape() {
        assert_eq!(
            gpu_info_from([None, None, None], Vec::new),
            vec![gpu(String::new(), None, None, None)]
        );
    }

    #[test]
    fn collect_soft_fails_git_errors() {
        let info = collect(
            "v",
            &[],
            Path::new("/missing-fleet-repo"),
            "",
            Instant::now(),
        );
        assert_eq!(
            info["git"],
            json!({"branch": null, "commit": null, "dirty": null})
        );
        assert!(info["cpu"].is_object());
    }
}

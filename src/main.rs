const DEFAULT_HOME_SERVER_PORT: u16 = 7878;
const DEFAULT_ROOM_PORT_RANGE: (u16, u16) = (9000, 9100);

#[derive(Debug, Default)]
struct CliArgs {
    host: bool,
    app: bool,
    ip: Option<String>,
    host_ip: Option<String>,
    room_port_range: Option<(u16, u16)>,
}

fn main() -> anyhow::Result<()> {
    let args = parse_args(std::env::args().skip(1).collect())?;
    let ip_provided = args.ip.is_some();
    let host_addr = args
        .host_ip
        .clone()
        .or_else(|| args.host.then(|| args.ip.clone()).flatten())
        .unwrap_or_else(|| format!("0.0.0.0:{DEFAULT_HOME_SERVER_PORT}"));
    let room_port_range = if args.host {
        Some(args.room_port_range.unwrap_or(DEFAULT_ROOM_PORT_RANGE))
    } else {
        None
    };

    if args.host && !args.app {
        return tune::online_net::run_home_server_forever_with_ports(&host_addr, room_port_range);
    }

    if args.host && args.app {
        let _server = tune::online_net::start_home_server(&host_addr, room_port_range)?;
        let app_target = local_home_target_from_bind_addr(&host_addr);
        return tune::app::run_with_startup(tune::app::AppStartupOptions {
            default_home_server_addr: Some(app_target),
            home_server_connected: true,
        });
    }

    tune::app::run_with_startup(tune::app::AppStartupOptions {
        default_home_server_addr: args.ip,
        home_server_connected: ip_provided,
    })
}

fn local_home_target_from_bind_addr(bind_addr: &str) -> String {
    match bind_addr.parse::<std::net::SocketAddr>() {
        Ok(std::net::SocketAddr::V4(addr)) if addr.ip().is_unspecified() => {
            format!("127.0.0.1:{}", addr.port())
        }
        Ok(std::net::SocketAddr::V6(addr)) if addr.ip().is_unspecified() => {
            format!("127.0.0.1:{}", addr.port())
        }
        _ => bind_addr.to_string(),
    }
}

fn parse_args(args: Vec<String>) -> anyhow::Result<CliArgs> {
    let mut out = CliArgs::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--host" => out.host = true,
            "--app" => out.app = true,
            "--ip" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    anyhow::bail!("--ip requires server host or host:port value");
                };
                if value.trim().is_empty() {
                    anyhow::bail!("--ip cannot be empty");
                }
                out.ip = Some(normalize_home_server_addr(value.trim()));
            }
            "--host-ip" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    anyhow::bail!("--host-ip requires bind host or host:port value");
                };
                if value.trim().is_empty() {
                    anyhow::bail!("--host-ip cannot be empty");
                }
                out.host_ip = Some(normalize_home_server_addr(value.trim()));
            }
            "--room-port-range" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    anyhow::bail!("--room-port-range requires start-end value");
                };
                out.room_port_range = Some(parse_port_range(value)?);
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
        index += 1;
    }
    if out.host_ip.is_some() && !out.host {
        anyhow::bail!("--host-ip requires --host");
    }
    if out.room_port_range.is_some() && !out.host {
        anyhow::bail!("--room-port-range requires --host");
    }
    if out.host && out.host_ip.is_some() && out.ip.is_some() {
        anyhow::bail!(
            "use --host-ip for host bind address or --ip as the legacy host alias, not both"
        );
    }
    Ok(out)
}

fn print_help() {
    println!("TuneTUI");
    println!("  --host            Run home server mode");
    println!("  --app             With --host, also run TUI app");
    println!(
        "  --host-ip host[:port]  Bind address for --host (default 0.0.0.0:{})",
        DEFAULT_HOME_SERVER_PORT
    );
    println!(
        "  --ip host[:port]  Connect to a home server (default port {})",
        DEFAULT_HOME_SERVER_PORT
    );
    println!(
        "  --room-port-range start-end   Room port range for host mode (default {}-{})",
        DEFAULT_ROOM_PORT_RANGE.0, DEFAULT_ROOM_PORT_RANGE.1
    );
}

fn normalize_home_server_addr(raw: &str) -> String {
    if raw.parse::<std::net::SocketAddr>().is_ok() {
        return raw.to_string();
    }
    if let Ok(ip) = raw.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(_) => format!("{raw}:{DEFAULT_HOME_SERVER_PORT}"),
            std::net::IpAddr::V6(_) => format!("[{raw}]:{DEFAULT_HOME_SERVER_PORT}"),
        };
    }
    if raw.starts_with('[') {
        return if raw.contains("]:") {
            raw.to_string()
        } else {
            format!("{raw}:{DEFAULT_HOME_SERVER_PORT}")
        };
    }

    match raw.rsplit_once(':') {
        Some((_host, port)) if !port.is_empty() && port.chars().all(|ch| ch.is_ascii_digit()) => {
            raw.to_string()
        }
        _ => format!("{raw}:{DEFAULT_HOME_SERVER_PORT}"),
    }
}

fn parse_port_range(raw: &str) -> anyhow::Result<(u16, u16)> {
    let trimmed = raw.trim();
    let Some((start_raw, end_raw)) = trimmed.split_once('-') else {
        anyhow::bail!("port range must be start-end");
    };
    let start = start_raw
        .trim()
        .parse::<u16>()
        .map_err(|_| anyhow::anyhow!("invalid start port"))?;
    let end = end_raw
        .trim()
        .parse::<u16>()
        .map_err(|_| anyhow::anyhow!("invalid end port"))?;
    if start == 0 || end == 0 {
        anyhow::bail!("ports must be between 1 and 65535");
    }
    if start > end {
        anyhow::bail!("range start must be <= end");
    }
    Ok((start, end))
}

#[cfg(test)]
mod tests {
    use super::{
        local_home_target_from_bind_addr, normalize_home_server_addr, parse_args, parse_port_range,
    };

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn local_home_target_maps_unspecified_v4_to_loopback() {
        assert_eq!(
            local_home_target_from_bind_addr("0.0.0.0:7878"),
            "127.0.0.1:7878"
        );
    }

    #[test]
    fn local_home_target_keeps_specific_host() {
        assert_eq!(
            local_home_target_from_bind_addr("198.51.100.42:7878"),
            "198.51.100.42:7878"
        );
    }

    #[test]
    fn parse_port_range_accepts_valid_input() {
        assert_eq!(parse_port_range("9000-9100").expect("range"), (9000, 9100));
    }

    #[test]
    fn parse_port_range_rejects_invalid_input() {
        assert!(parse_port_range("9100-9000").is_err());
        assert!(parse_port_range("abc-def").is_err());
        assert!(parse_port_range("0-10").is_err());
    }

    #[test]
    fn normalize_home_server_addr_adds_default_port() {
        assert_eq!(
            normalize_home_server_addr("198.51.100.42"),
            "198.51.100.42:7878"
        );
        assert_eq!(
            normalize_home_server_addr("example.com"),
            "example.com:7878"
        );
    }

    #[test]
    fn normalize_home_server_addr_keeps_explicit_port() {
        assert_eq!(
            normalize_home_server_addr("198.51.100.42:9000"),
            "198.51.100.42:9000"
        );
    }

    #[test]
    fn parse_args_accepts_explicit_host_ip() {
        let parsed = parse_args(args(&["--host", "--host-ip", "0.0.0.0"])).expect("args");
        assert!(parsed.host);
        assert_eq!(parsed.host_ip.as_deref(), Some("0.0.0.0:7878"));
        assert_eq!(parsed.ip, None);
    }

    #[test]
    fn parse_args_keeps_host_ip_port() {
        let parsed = parse_args(args(&["--host", "--host-ip", "0.0.0.0:9000"])).expect("args");
        assert_eq!(parsed.host_ip.as_deref(), Some("0.0.0.0:9000"));
    }

    #[test]
    fn parse_args_keeps_legacy_host_ip_alias() {
        let parsed = parse_args(args(&["--host", "--ip", "0.0.0.0"])).expect("args");
        assert!(parsed.host);
        assert_eq!(parsed.ip.as_deref(), Some("0.0.0.0:7878"));
        assert_eq!(parsed.host_ip, None);
    }

    #[test]
    fn parse_args_uses_ip_as_connect_target_without_host() {
        let parsed = parse_args(args(&["--ip", "192.168.1.100"])).expect("args");
        assert!(!parsed.host);
        assert_eq!(parsed.ip.as_deref(), Some("192.168.1.100:7878"));
        assert_eq!(parsed.host_ip, None);
    }

    #[test]
    fn parse_args_rejects_ambiguous_host_ip_and_ip() {
        let err = parse_args(args(&[
            "--host",
            "--host-ip",
            "0.0.0.0",
            "--ip",
            "192.168.1.100",
        ]))
        .expect_err("ambiguous args should fail");
        assert!(err.to_string().contains("not both"));
    }

    #[test]
    fn parse_args_rejects_host_ip_without_host() {
        let err = parse_args(args(&["--host-ip", "0.0.0.0"]))
            .expect_err("host-ip without host should fail");
        assert!(err.to_string().contains("requires --host"));
    }
}

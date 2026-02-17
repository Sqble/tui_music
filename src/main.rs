#[derive(Debug, Default)]
struct CliArgs {
    host: bool,
    app: bool,
    ip: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let args = parse_args(std::env::args().skip(1).collect())?;
    let ip_provided = args.ip.is_some();
    let home_addr = args
        .ip
        .clone()
        .unwrap_or_else(|| String::from("0.0.0.0:7878"));

    if args.host && !args.app {
        return tune::online_net::run_home_server_forever(&home_addr);
    }

    if args.host && args.app {
        let _server = tune::online_net::start_home_server(&home_addr)?;
        let app_target = local_home_target_from_bind_addr(&home_addr);
        return tune::app::run_with_startup(tune::app::AppStartupOptions {
            default_home_server_addr: Some(app_target),
            home_server_from_cli: ip_provided,
        });
    }

    tune::app::run_with_startup(tune::app::AppStartupOptions {
        default_home_server_addr: args.ip,
        home_server_from_cli: ip_provided,
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
                    anyhow::bail!("--ip requires host:port value");
                };
                if value.trim().is_empty() {
                    anyhow::bail!("--ip cannot be empty");
                }
                out.ip = Some(value.trim().to_string());
            }
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
        index += 1;
    }
    Ok(out)
}

fn print_help() {
    println!("TuneTUI");
    println!("  --host            Run home server mode");
    println!("  --app             With --host, also run TUI app");
    println!("  --ip host:port    Home server bind/target address");
}

#[cfg(test)]
mod tests {
    use super::local_home_target_from_bind_addr;

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
}

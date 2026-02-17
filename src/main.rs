#[derive(Debug, Default)]
struct CliArgs {
    host: bool,
    app: bool,
    ip: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let args = parse_args(std::env::args().skip(1).collect())?;
    let home_addr = args
        .ip
        .clone()
        .unwrap_or_else(|| String::from("0.0.0.0:7878"));

    if args.host && !args.app {
        return tune::online_net::run_home_server_forever(&home_addr);
    }

    if args.host && args.app {
        let _server = tune::online_net::start_home_server(&home_addr)?;
        return tune::app::run_with_startup(tune::app::AppStartupOptions {
            default_home_server_addr: Some(home_addr),
        });
    }

    tune::app::run_with_startup(tune::app::AppStartupOptions {
        default_home_server_addr: args.ip,
    })
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

use clap::Parser;
#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:0")]
    listen: std::net::SocketAddr,
    #[arg(long)]
    host: Vec<String>,
    #[arg(long)]
    daemon_manifest: Vec<String>,
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    anyhow::ensure!(
        args.listen.ip() == std::net::Ipv4Addr::LOCALHOST,
        "fake keeper binds only 127.0.0.1"
    );
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    let address = listener.local_addr()?;
    let keeper = chimaera_link::fake::FakeKeeper::new(format!("http://{address}"));
    let mut manifests = std::collections::HashMap::new();
    for entry in args.daemon_manifest {
        let (alias, path) = entry.split_once('=').ok_or_else(|| {
            anyhow::anyhow!("expected --daemon-manifest alias=/path/to/manifest.json")
        })?;
        use tokio::io::AsyncReadExt;
        let file = tokio::fs::File::open(path).await?;
        let mut bytes = Vec::new();
        file.take(8193).read_to_end(&mut bytes).await?;
        anyhow::ensure!(bytes.len() <= 8192, "manifest exceeds 8 KiB");
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        let token = value["token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("daemon manifest has no token"))?;
        let build = value["build"].as_str().unwrap_or("development");
        manifests.insert(
            alias.to_string(),
            chimaera_link::Daemon {
                token: token.into(),
                build: build.into(),
                sessions: 0,
            },
        );
    }
    for host in args.host {
        let (alias, address) = host
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("expected --host alias=127.0.0.1:port"))?;
        keeper
            .add_target(alias, address.parse()?, manifests.remove(alias))
            .await?;
    }
    println!(
        "{}\ntoken={}",
        keeper.endpoint,
        chimaera_link::fake::STATIC_TOKEN
    );
    axum::serve(listener, keeper.router()).await?;
    Ok(())
}

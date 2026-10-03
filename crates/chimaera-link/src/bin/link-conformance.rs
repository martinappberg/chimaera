use clap::Parser;
#[derive(Parser)]
struct Args {
    #[arg(long)]
    endpoint: String,
    #[arg(long, env = "CHIMAERA_LINK_TOKEN")]
    token: String,
    #[arg(long)]
    host: Option<String>,
    #[arg(long)]
    test_hooks: bool,
    /// Also verify the account handoff extension.
    #[arg(long)]
    handoff: bool,
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut report = chimaera_link::conformance::run(
        &args.endpoint,
        &args.token,
        args.host.as_deref(),
        args.test_hooks,
    )
    .await?;
    if args.handoff {
        report.extend(
            chimaera_link::conformance::handoff(&args.endpoint, &args.token, args.test_hooks)
                .await?,
        );
    }
    for check in report {
        println!("ok: {check}");
    }
    Ok(())
}

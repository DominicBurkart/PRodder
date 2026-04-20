#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    prodder::real_main().await
}
